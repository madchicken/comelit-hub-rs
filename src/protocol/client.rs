use crate::protocol::credentials::get_secrets;
use crate::protocol::manager::RequestManager;
use crate::protocol::messages::{
    MqttMessage, MqttResponseMessage, RequestType, make_action_message, make_announce_message,
    make_login_message, make_ping_message, make_status_message, make_subscribe_message,
};
use crate::protocol::out_data_messages::{
    ActionType, AgentDeviceData, DeviceData, HomeDeviceData, device_data_to_home_device,
};
use crate::protocol::scanner::{ComelitHUB, Scanner};
use async_trait::async_trait;
use dashmap::DashMap;
use derive_builder::Builder;
use mac_address::get_mac_address;
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};
use tokio::time::sleep;
use tracing::{debug, error, info, trace};
use uuid::Uuid;

pub const ROOT_ID: &str = "GEN#17#13#1";

#[derive(Error, Debug)]
pub enum ComelitClientError {
    #[error("Client is not logged in")]
    InvalidState,
    #[error("Client failed to announce: {0}")]
    Login(String),
    #[error("Client request failed: {0}")]
    Generic(String),
    #[error("Client connection failed: {0}")]
    Connection(String),
    #[error("Publishing failed: {0}")]
    Publish(String),
    #[error("Reading failed: {0}")]
    ReadError(String),
    #[error("Scanning local network failed: {0}")]
    Scanner(String),
}

#[derive(Clone)]
struct Session {
    session_token: String,
    agent_id: u32,
}

pub struct ComelitClient {
    client: Arc<AsyncClient>,
    request_manager: Arc<Mutex<RequestManager>>,
    write_topic: String,
    read_topic: String,
    req_id: Arc<AtomicU32>,
    session: Arc<RwLock<Option<Session>>>,
    pub mac_address: String,
    user: String,
    password: String,
}

#[derive(Builder)]
pub struct ComelitOptions {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub mqtt_user: String,
    pub mqtt_password: String,
    pub user: Option<String>,
    pub password: Option<String>,
}

impl ComelitOptions {
    pub fn builder() -> ComelitOptionsBuilder {
        ComelitOptionsBuilder::default()
    }

    async fn get_hub_info(&self) -> Result<Option<ComelitHUB>, ComelitClientError> {
        if let Some(host) = &self.host {
            let hub = Scanner::scan_address(host)
                .await
                .map_err(|e| ComelitClientError::Scanner(e.to_string()))?;
            Ok(hub)
        } else {
            let devices = Scanner::scan()
                .await
                .map_err(|e| ComelitClientError::Scanner(e.to_string()))?;
            if devices.is_empty() {
                Err(ComelitClientError::Scanner(
                    "No Comelit HUB found".to_string(),
                ))
            } else {
                Ok(devices.iter().find(|dev| dev.model_id() == "HSrv").cloned())
            }
        }
    }
}

impl Default for ComelitOptions {
    fn default() -> Self {
        let (mqtt_user, mqtt_password) = get_secrets();
        ComelitOptions {
            host: None,
            port: Some(1883),
            mqtt_user,
            mqtt_password,
            user: Some("admin".to_string()),
            password: Some("admin".to_string()),
        }
    }
}

// hsrv-user|sf1nE9bjPc|ipc-user|irj6Glv6J0
const CLIENT_ID_PREFIX: &str = "HSrv";

fn generate_client_id() -> String {
    let uuid = Uuid::new_v4();
    format!("{CLIENT_ID_PREFIX}_{}", uuid.to_string().to_uppercase())
}

#[derive(Eq, PartialEq, Clone)]
pub enum State {
    Disconnected,
    Announced(u32),
    Logged(u32, String),
}

async fn make_id(req_id: &AtomicU32) -> u32 {
    req_id.fetch_add(1, Ordering::Relaxed)
}

#[async_trait]
pub trait StatusUpdate {
    async fn status_update(&self, device: &HomeDeviceData);
}

impl ComelitClient {
    pub async fn new(
        options: ComelitOptions,
        observer: Arc<dyn StatusUpdate + Sync + Send>,
    ) -> Result<Self, ComelitClientError> {
        let hub = options.get_hub_info().await?;
        if let Some(hub) = hub {
            let client_id = generate_client_id();
            let (write_topic, read_topic) = if let Some(_mac_address) =
                get_mac_address().map_err(|e| ComelitClientError::Generic(e.to_string()))?
            {
                let addr = hub.mac_address();
                let rx_topic = format!("{CLIENT_ID_PREFIX}/{addr}/rx/{client_id}");
                let tx_topic = format!("{CLIENT_ID_PREFIX}/{addr}/tx/{client_id}");
                (rx_topic, tx_topic)
            } else {
                panic!("Failed to get mac address");
            };
            let mut mqttoptions =
                MqttOptions::new(client_id, hub.address().unwrap(), options.port.unwrap_or(1883));
            mqttoptions.set_keep_alive(Duration::from_secs(5));
            mqttoptions.set_credentials(options.mqtt_user, options.mqtt_password);
            mqttoptions.set_max_packet_size(128 * 1024, 128 * 1024);

            let (client, event_loop) = AsyncClient::new(mqttoptions.clone(), 10);
            info!("Connected to MQTT broker at {:?}", mqttoptions);
            let request_manager = Arc::new(Mutex::new(RequestManager::new()));
            let manager_clone = Arc::clone(&request_manager);

            if let Err(e) = client
                .subscribe(read_topic.clone(), QoS::AtLeastOnce)
                .await
                .map_err(|e| ComelitClientError::Connection(e.to_string()))
            {
                return Err(ComelitClientError::Connection(format!(
                    "Failed to subscribe to topic: {e}"
                )));
            }
            info!("Subscribed to topic: {}", read_topic);
            // Start the event loop in a separate thread
            let read_topic_clone = read_topic.clone();
            let session = Arc::new(RwLock::new(None));

            tokio::spawn(async move {
                info!("Starting event loop");
                ComelitClient::run_event_loop(event_loop, manager_clone, read_topic_clone, observer)
                    .await
            });

            let req_id = Arc::new(AtomicU32::new(1));
            let client = Arc::new(client);
            Self::start_ping(
                client.clone(),
                session.clone(),
                req_id.clone(),
                write_topic.as_str(),
            );
            Ok(ComelitClient {
                client,
                request_manager,
                write_topic,
                read_topic,
                req_id,
                session,
                mac_address: hub.mac_address().to_string(),
                user: options.user.unwrap_or_default(),
                password: options.password.unwrap_or_default(),
            })
        } else {
            Err(ComelitClientError::Scanner(
                "No Comelit HUB found".to_string(),
            ))
        }
    }

    pub async fn disconnect(&self) -> Result<(), ComelitClientError> {
        self.client
            .unsubscribe(&self.read_topic)
            .await
            .map_err(|e| ComelitClientError::Generic(format!("Unsubscribe error: {e}")))?;
        info!("Unsubscribed from MQTT broker");
        self.client
            .disconnect()
            .await
            .map_err(|e| ComelitClientError::Connection(format!("Disconnect error: {e}")))?;
        info!("Disconnected from MQTT broker");
        self.session.write().await.take();
        Ok(())
    }

    fn start_ping(
        client: Arc<AsyncClient>,
        session: Arc<RwLock<Option<Session>>>,
        req_id: Arc<AtomicU32>,
        topic: &str,
    ) {
        let topic = topic.to_string();
        tokio::spawn(async move {
            info!("Starting ping task");
            let state = session.clone();
            let req_id = req_id.clone();
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.tick().await; // first tick is immediate
            loop {
                tokio::select! {
                    // Trigger periodic updates
                    _ = interval.tick() => {
                        let session = state.read().await.clone();
                        match session {
                            Some(session) => {
                                // Send ping message. We don't use the manager, so the responses will be just ignored
                                info!("Sending ping message");
                                let payload = make_ping_message(req_id.fetch_add(1, Ordering::Relaxed), session.agent_id, session.session_token.as_str());
                                client.publish(topic.as_str(), QoS::AtMostOnce, false, serde_json::to_string(&payload).unwrap()).await.map_err(|e| {
                                    ComelitClientError::Publish(format!("Serialization error: {e:?}"))
                                }).unwrap();
                            },
                            _ => {
                                // Do nothing, we are not logged in
                                debug!("Not logged in, skipping ping");
                            }
                        }
                    }
                }
                interval.tick().await;
            }
        });
    }

    async fn run_event_loop(
        mut event_loop: EventLoop,
        request_manager: Arc<Mutex<RequestManager>>,
        response_topic: String,
        observer: Arc<dyn StatusUpdate + Sync + Send>,
    ) -> () {
        loop {
            // Poll the event loop for incoming messages
            debug!("Polling event loop");
            match event_loop.poll().await {
                Ok(notification) => {
                    if let Event::Incoming(Packet::Publish(publish)) = notification {
                        if publish.topic == response_topic {
                            // Process incoming response
                            trace!(
                                "Received response: {}",
                                String::from_utf8(publish.payload.to_vec()).unwrap()
                            );
                            match serde_json::from_slice::<MqttResponseMessage>(&publish.payload) {
                                Ok(response) => {
                                    let manager = request_manager.lock().await;
                                    match response.req_type {
                                        RequestType::Status => {
                                            if response.seq_id.is_some() {
                                                if !manager.complete_request(&response).await {
                                                    info!(
                                                        "Response for unknown request: {:?}",
                                                        response
                                                    );
                                                }
                                            } else {
                                                // this is an update message from the server
                                                if let Some(obj_id) = response.obj_id {
                                                    info!("Updating object: {}", obj_id);
                                                    let vec = device_data_to_home_device(
                                                        response.out_data.first().unwrap().clone(),
                                                    );
                                                    let device = vec.first().unwrap();
                                                    info!("New data: {:?}", device);
                                                    observer.status_update(device).await;
                                                }
                                            }
                                        }
                                        RequestType::Ping => {
                                            // Ping requests are not tracked by the manager
                                            info!("Ping response received");
                                        }
                                        _ => {
                                            if !manager.complete_request(&response).await {
                                                info!(
                                                    "Response for unknown request: {:?}",
                                                    response
                                                );
                                            }
                                        }
                                    }
                                    manager.remove_pending_requests();
                                }
                                Err(e) => error!("Failed to parse response: {:?}", e),
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Connection error: {:?}", e);
                    sleep(Duration::from_millis(10)).await;
                }
            }
        }
    }

    /// Send a request and wait for the response
    /// In case of invalid token, it will try to reconnect and send the request again
    /// If the reconnection fails, it will return an error
    async fn send_request(
        &self,
        payload: MqttMessage,
    ) -> Result<MqttResponseMessage, ComelitClientError> {
        // Publish the request. Looping in case of invalid token response
        'outer: loop {
            let mut response_receiver = match self
                .client
                .publish(
                    &self.write_topic,
                    QoS::AtMostOnce,
                    false,
                    serde_json::to_string(&payload)
                        .map(|json| {
                            info!("Sending request: {json}");
                            json
                        })
                        .map_err(|e| {
                            ComelitClientError::Publish(format!("Serialization error: {e:?}",))
                        })?,
                )
                .await
            {
                Ok(_) => {
                    info!("Request sent successfully");
                    {
                        let manager = self.request_manager.lock().await;
                        let res = manager.add_request(payload.seq_id).await;
                        drop(manager);
                        res
                    }
                }
                Err(e) => {
                    break 'outer Err(ComelitClientError::Publish(format!(
                        "Failed to publish request: {e}"
                    )));
                }
            };

            debug!("Start waiting for response");
            // Wait for the response with timeout
            let timeout = Duration::from_secs(5);
            let start = Instant::now();
            // waiting loop for the response
            'inner: loop {
                if start.elapsed() > timeout {
                    error!("Request timed out: {}", payload.seq_id);
                    break 'outer Err(ComelitClientError::ReadError(format!(
                        "Request timed out: {}",
                        payload.seq_id
                    )));
                }

                if !response_receiver.is_empty() {
                    // Response is ready
                    break 'inner;
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            // Extract the response
            match response_receiver.try_recv() {
                Ok(response) => {
                    if response.req_result.unwrap() != 0 {
                        match Box::pin(self.login(State::Disconnected)).await {
                            Ok(_) => {
                                info!("Reconnected successfully. Sending request again.",);
                                continue;
                            }
                            Err(e) => {
                                return Err(ComelitClientError::Publish(format!(
                                    "Failed to publish request after receiving an error: {e}"
                                )));
                            }
                        }
                    } else {
                        trace!("Completing response: {:?}", response);
                        return Ok(response);
                    }
                }
                Err(e) => {
                    error!("Failed to receive response: {:?}", e);
                    return Err(ComelitClientError::ReadError(format!(
                        "Failed to receive response: {e}"
                    )));
                }
            }
        }
    }

    pub async fn login(&self, state: State) -> Result<(), ComelitClientError> {
        // Get a read lock
        match state {
            State::Disconnected => {
                info!("Announcing the to HUB");
                let response = self
                    .send_request(make_announce_message(make_id(&self.req_id).await, 0))
                    .await
                    .map_err(|e| ComelitClientError::Generic(e.to_string()))?;
                if response.req_result.unwrap_or_default() != 0 {
                    return Err(ComelitClientError::Login(format!(
                        "Announce failed: {}",
                        response.req_result.unwrap_or_default()
                    )));
                }
                let out = response.out_data.first().unwrap();
                let agent_data = serde_json::from_value::<AgentDeviceData>(out.clone()).unwrap();
                info!("Announce HUB description: {}", agent_data.description);
                Box::pin(self.login(State::Announced(agent_data.agent_id))).await
            }
            State::Announced(agent_id) => {
                info!("Logging into the HUB");
                let response = self
                    .send_request(make_login_message(
                        make_id(&self.req_id).await,
                        self.user.as_str(),
                        self.password.as_str(),
                        agent_id,
                    ))
                    .await
                    .map_err(|e| ComelitClientError::Generic(e.to_string()))?;
                if response.req_result.unwrap_or_default() != 0 {
                    return Err(ComelitClientError::Login(format!(
                        "Login failed: {}",
                        response.message.unwrap_or_default()
                    )));
                }
                Box::pin(self.login(State::Logged(agent_id, response.session_token.unwrap()))).await
            }
            State::Logged(agent_id, session_token) => {
                info!("Logged in");
                self.session.write().await.replace(Session {
                    session_token: session_token.clone(),
                    agent_id,
                });
                Ok(())
            }
        }
    }

    pub async fn info(
        &self,
        device_id: &str,
        detail_level: u8,
    ) -> Result<Vec<DeviceData>, ComelitClientError> {
        let session = self.get_session().await?;
        let resp = self
            .send_request(make_status_message(
                make_id(&self.req_id).await,
                session.0,
                session.1.as_str(),
                device_id,
                detail_level,
            ))
            .await
            .map_err(|e| ComelitClientError::Generic(e.to_string()))?;
        Ok(resp
            .out_data
            .iter()
            .map(|out| serde_json::from_value::<DeviceData>(out.clone()).unwrap())
            .collect::<Vec<DeviceData>>())
    }

    pub async fn subscribe(&self, device_id: &str) -> Result<(), ComelitClientError> {
        let session = self.get_session().await?;
        let _resp = self
            .send_request(make_subscribe_message(
                make_id(&self.req_id).await,
                session.0,
                session.1.as_str(),
                device_id,
            ))
            .await
            .map_err(|e| ComelitClientError::Generic(e.to_string()))?;
        Ok(())
    }

    async fn get_session(&self) -> Result<(u32, String), ComelitClientError> {
        if let Some(session) = self.session.read().await.as_ref() {
            Ok((session.agent_id, session.session_token.clone()))
        } else {
            Err(ComelitClientError::InvalidState)
        }
    }

    pub async fn fetch_index(&self) -> Result<DashMap<String, HomeDeviceData>, ComelitClientError> {
        let session = self.get_session().await?;
        let resp = self
            .send_request(make_status_message(
                make_id(&self.req_id).await,
                session.0,
                session.1.as_str(),
                ROOT_ID,
                2,
            ))
            .await
            .map_err(|e| ComelitClientError::Generic(e.to_string()))?;
        let index = DashMap::new();
        for v in resp.out_data.iter() {
            let devices = device_data_to_home_device(v.clone());
            for device in devices {
                index.insert(device.id().clone(), device);
            }
        }
        Ok(index)
    }

    pub async fn send_action(
        &self,
        device_id: &str,
        action_type: ActionType,
        value: u32,
    ) -> Result<(), ComelitClientError> {
        let session = self.get_session().await?;
        let _resp = self
            .send_request(make_action_message(
                make_id(&self.req_id).await,
                session.0,
                session.1.as_str(),
                device_id,
                action_type,
                value,
            ))
            .await
            .map_err(|e| ComelitClientError::Generic(e.to_string()))?;
        Ok(())
    }
}
