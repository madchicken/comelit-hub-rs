use crate::protocol::manager::RequestManager;
use crate::protocol::messages::{MqttMessage, MqttResponseMessage, make_announce_message, make_login_message, make_status_message, make_subscribe_message, make_action_message};
use crate::protocol::out_data_messages::{device_data_to_home_device, ActionType, AgentDeviceData, DeviceData, HomeDeviceData};
use derive_builder::Builder;
use mac_address::get_mac_address;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use dashmap::DashMap;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{error, info};
use uuid::Uuid;

pub const ROOT_ID: &str = "GEN#17#13#1";

#[derive(Error, Debug)]
pub enum ComelitClientError {
    #[error("Client is not logged in")]
    InvalidStateError,
    #[error("Client failed to announce: {0}")]
    LoginError(String),
    #[error("Client request failed: {0}")]
    GenericError(String),
    #[error("Client connection failed: {0}")]
    ConnectionError(String),
    #[error("Publishing failed: {0}")]
    PublishError(String),
    #[error("Reading failed: {0}")]
    ReadError(String),
}

pub struct ComelitClient {
    client: AsyncClient,
    request_manager: Arc<Mutex<RequestManager>>,
    write_topic: String,
    read_topic: String,
    req_id: AtomicU32,
    state: State,
    user: String,
    password: String,
}

#[derive(Builder)]
pub struct ComelitOptions {
    pub host: String,
    pub port: u16,
    pub mqtt_user: String,
    pub mqtt_password: String,
    pub user: String,
    pub password: String,
}

impl ComelitOptions {
    pub fn builder() -> ComelitOptionsBuilder {
        ComelitOptionsBuilder::default()
    }
}

// hsrv-user|sf1nE9bjPc|ipc-user|irj6Glv6J0
const CLIENT_ID_PREFIX: &str = "HSrv";

fn generate_client_id() -> String {
    let uuid = Uuid::new_v4();
    format!("{CLIENT_ID_PREFIX}_{}", uuid.to_string().to_uppercase())
}

#[derive(Eq, PartialEq, Clone)]
enum State {
    Disconnected,
    Announced(u32),
    Logged(u32, String),
}

impl ComelitClient {
    pub async fn new(options: ComelitOptions) -> Result<Self, ComelitClientError> {
        let client_id = generate_client_id();
        let (write_topic, read_topic) = if let Some(_mac_address) =
            get_mac_address().map_err(|e| ComelitClientError::GenericError(e.to_string()))?
        {
            let addr = "0025291701EC";
            let rx_topic = format!("{CLIENT_ID_PREFIX}/{addr}/rx/{client_id}");
            let tx_topic = format!("{CLIENT_ID_PREFIX}/{addr}/tx/{client_id}");
            (rx_topic, tx_topic)
        } else {
            panic!("Failed to get mac address");
        };
        let mut mqttoptions = MqttOptions::new(client_id, options.host, options.port);
        mqttoptions.set_keep_alive(Duration::from_secs(5));
        mqttoptions.set_credentials(options.mqtt_user, options.mqtt_password);
        mqttoptions.set_max_packet_size(128 * 1024, 128 * 1024);

        let (client, eventloop) = AsyncClient::new(mqttoptions.clone(), 10);
        info!("Connected to MQTT broker at {:?}", mqttoptions);
        let request_manager = Arc::new(Mutex::new(RequestManager::new()));
        let manager_clone = Arc::clone(&request_manager);

        if let Err(e) = client
            .subscribe(read_topic.clone(), QoS::AtLeastOnce)
            .await
            .map_err(|e| ComelitClientError::ConnectionError(e.to_string()))
        {
            return Err(ComelitClientError::ConnectionError(format!(
                "Failed to subscribe to topic: {}",
                e
            )));
        }
        info!("Subscribed to topic: {}", read_topic);
        // Start the event loop in a separate thread
        let read_topic_clone = read_topic.clone();
        let _ = tokio::spawn(async move {
            info!("Starting event loop");
            ComelitClient::run_eventloop(eventloop, manager_clone, read_topic_clone).await
        });

        Ok(ComelitClient {
            client,
            request_manager,
            write_topic,
            read_topic,
            req_id: AtomicU32::new(1),
            state: State::Disconnected,
            user: options.user,
            password: options.password,
        })
    }

    pub async fn disconnect(self) -> Result<(), ComelitClientError> {
        self.client
            .unsubscribe(&self.read_topic)
            .await
            .map_err(|e| ComelitClientError::GenericError(format!("Unsubscribe error: {e}")))?;
        self.client
            .disconnect()
            .await
            .map_err(|e| ComelitClientError::ConnectionError(format!("Disconnect error: {e}")))?;
        Ok(())
    }

    async fn run_eventloop(
        mut eventloop: rumqttc::EventLoop,
        request_manager: Arc<Mutex<RequestManager>>,
        response_topic: String,
    ) -> () {
        loop {
            match eventloop.poll().await {
                Ok(notification) => {
                    if let Event::Incoming(Packet::Publish(publish)) = notification {
                        if publish.topic == response_topic {
                            // Process incoming response
                            info!("Received response: {}", String::from_utf8(publish.payload.to_vec()).unwrap());
                            match serde_json::from_slice::<MqttResponseMessage>(&publish.payload) {
                                Ok(response) => {
                                    let manager = request_manager.lock().await;
                                    if response.seq_id.is_some() {
                                        if !manager.complete_request(&response).await {
                                            info!("Response for unknown request: {:?}", response);
                                        }
                                    } else {
                                        // this is an update message from the server
                                        if let Some(obj_id) = response.obj_id {
                                            info!("Updating object: {}", obj_id);
                                            let vec = device_data_to_home_device(response.out_data.first().unwrap().clone());
                                            let device = vec.first().unwrap();
                                            info!("New data: {:?}", device);
                                            manager.update_index(device.id(), device);
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
                    break
                }
            }
        }
        info!("Event loop terminated");
    }

    // Send a request and wait for the response
    async fn send_request(
        &mut self,
        payload: MqttMessage,
    ) -> Result<MqttResponseMessage, ComelitClientError> {
        // Register this request before publishing
        let mut response_receiver = {
            let manager = self.request_manager.lock().await;
            manager.add_request(payload.seq_id.clone()).await
        };

        // Publish the request. Looping in case of invalid token response
        loop {
            match self
                .client
                .publish(
                    &self.write_topic,
                    QoS::AtMostOnce,
                    false,
                    serde_json::to_string(&payload).map(|json| {
                        info!("Sending request: {json}");
                        json
                    }).map_err(|e| {
                        ComelitClientError::PublishError(format!("Serialization error: {:?}", e))
                    })?,
                )
                .await
            {
                Ok(_) => {
                    info!("Request sent successfully");
                }
                Err(e) => {
                    break Err(ComelitClientError::PublishError(format!(
                        "Failed to publish request: {}",
                        e
                    )));
                }
            }

            // Wait for the response with timeout
            let timeout = Duration::from_secs(5);
            let start = Instant::now();
            // waiting loop for the response
            loop {
                if start.elapsed() > timeout {
                    return Err(ComelitClientError::ReadError(format!(
                        "Request timed out: {}",
                        payload.seq_id
                    )));
                }

                if !response_receiver.is_empty() {
                    // Response is ready
                    break;
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            // Extract the response
            match response_receiver.try_recv() {
                Ok(response) => {
                    if response.req_result.unwrap() != 0 {
                        match Box::pin(self.login()).await {
                            Ok((_, token)) => {
                                info!("Reconnected successfully with session token {token}. Sending request again.");
                                continue;
                            }
                            Err(e) => break Err(ComelitClientError::PublishError(format!(
                                "Failed to publish request after receiving an error: {}",
                                e
                            )))
                        }
                    } else {
                        break Ok(response)
                    }
                },
                Err(e) => break Err(ComelitClientError::ReadError(format!(
                    "Failed to receive response: {e}"
                ))),
            }
        }
    }

    pub async fn login(&mut self) -> Result<(u32, String), ComelitClientError> {
        let state = self.state.clone();
        match state {
            State::Disconnected => {
                let response = self
                    .send_request(make_announce_message(
                        self.req_id.fetch_add(1, Ordering::Relaxed),
                        0,
                    ))
                    .await
                    .map_err(|e| ComelitClientError::GenericError(e.to_string()))?;
                if response.req_result.unwrap_or_default() != 0 {
                    return Err(ComelitClientError::LoginError(
                        format!("Announce failed: {}", response.req_result.unwrap_or_default()).into(),
                    ));
                }
                let out = response.out_data.first().unwrap();
                let agent_data = serde_json::from_value::<AgentDeviceData>(out.clone()).unwrap();
                info!("Announce HUB description: {}", agent_data.description);
                self.state = State::Announced(agent_data.agent_id);
                Box::pin(self.login()).await
            }
            State::Announced(agent_id) => {
                let response = self
                    .send_request(make_login_message(
                        self.req_id.fetch_add(1, Ordering::Relaxed),
                        self.user.as_str(),
                        self.password.as_str(),
                        agent_id,
                    ))
                    .await
                    .map_err(|e| ComelitClientError::GenericError(e.to_string()))?;
                if response.req_result.unwrap_or_default() != 0 {
                    return Err(ComelitClientError::LoginError(
                        format!("Login failed: {}", response.req_result.unwrap_or_default()).into(),
                    ));
                }
                self.state = State::Logged(agent_id, response.session_token.unwrap());
                Box::pin(self.login()).await
            }
            State::Logged(agent_id, session_token) => {
                self.state = State::Logged(agent_id, session_token.clone());
                Ok((agent_id, session_token.clone()))
            }
        }
    }

    pub async fn info(
        &mut self,
        device_id: &str,
        detail_level: u8,
    ) -> Result<Vec<DeviceData>, ComelitClientError> {
        if !matches!(self.state, State::Logged(..)) {
            Err(ComelitClientError::InvalidStateError)
        } else {
            let (_, session_token) = self.login().await?;
            let resp = self
                .send_request(make_status_message(
                    self.req_id.fetch_add(1, Ordering::Relaxed),
                    session_token.as_str(),
                    device_id,
                    detail_level,
                ))
                .await
                .map_err(|e| ComelitClientError::GenericError(e.to_string()))?;
            Ok(resp
                .out_data
                .iter()
                .map(|out| serde_json::from_value::<DeviceData>(out.clone()).unwrap())
                .collect::<Vec<DeviceData>>())
        }
    }

    pub async fn subscribe(&mut self, device_id: &str) -> Result<(), ComelitClientError> {
        if !matches!(self.state, State::Logged(..)) {
            return Err(ComelitClientError::InvalidStateError);
        }
        let (_, session_token) = self.login().await?;
        let _resp = self
            .send_request(make_subscribe_message(
                self.req_id.fetch_add(1, Ordering::Relaxed),
                session_token.as_str(),
                device_id,
            ))
            .await
            .map_err(|e| ComelitClientError::GenericError(e.to_string()))?;
        Ok(())
    }

    pub async fn fetch_index(&mut self) -> Result<DashMap<String, HomeDeviceData>, ComelitClientError> {
        if !matches!(self.state, State::Logged(..)) {
            return Err(ComelitClientError::InvalidStateError);
        }
        let (_, session_token) = self.login().await?;
        let resp = self
            .send_request(make_status_message(
                self.req_id.fetch_add(1, Ordering::Relaxed),
                session_token.as_str(),
                ROOT_ID,
                2,
            ))
            .await
            .map_err(|e| ComelitClientError::GenericError(e.to_string()))?;
        let index = DashMap::new();
        for v in resp.out_data.iter() {
            let devices = device_data_to_home_device(v.clone());
            for device in devices {
                index.insert(device.id().clone(), device);
            }
        }
        let index2 = index.clone();
        let mut guard = self.request_manager.lock().await;
        guard.set_index(index);
        Ok(index2)
    }

    pub async fn send_action(
        &mut self,
        device_id: &str,
        action_type: ActionType,
        value: u32
    ) -> Result<(), ComelitClientError> {
        if !matches!(self.state, State::Logged(..)) {
            return Err(ComelitClientError::InvalidStateError);
        }
        let (_, session_token) = self.login().await?;
        let _resp = self
            .send_request(make_action_message(
                self.req_id.fetch_add(1, Ordering::Relaxed),
                session_token.as_str(),
                device_id,
                action_type,
                value,
            ))
            .await
            .map_err(|e| ComelitClientError::GenericError(e.to_string()))?;
        Ok(())
    }
}
