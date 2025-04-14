use crate::protocol::manager::RequestManager;
use crate::protocol::messages::{
    MqttMessage, MqttResponseMessage, make_announce_message, make_login_message,
    make_status_message,
};
use crate::protocol::out_data_messages::{AgentDeviceData, DeviceData, OutData};
use derive_builder::Builder;
use mac_address::get_mac_address;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::Mutex;
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
}

#[derive(Builder)]
pub struct ComelitOptions {
    pub host: String,
    pub port: u16,
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
    Start,
    Announce(u32),
    Login(u32, String),
}

impl ComelitClient {
    pub async fn new(options: ComelitOptions) -> Result<Self, ComelitClientError> {
        let client_id = generate_client_id();
        let (write_topic, read_topic) = if let Some(mac_address) =
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
        mqttoptions.set_credentials(options.user, options.password);

        let (client, eventloop) = AsyncClient::new(mqttoptions.clone(), 10);
        println!("Connected to MQTT broker at {:?}", mqttoptions);
        let request_manager = Arc::new(Mutex::new(RequestManager::new()));
        let manager_clone = Arc::clone(&request_manager);

        if let Err(e) = client
            .subscribe(read_topic.clone(), QoS::AtLeastOnce)
            .await
            .map_err(|e| ComelitClientError::ConnectionError(e.to_string())) {
            return Err(ComelitClientError::ConnectionError(format!(
                "Failed to subscribe to topic: {}",
                e
            )));
        }
        println!("Subscribed to topic: {}", read_topic);
        // Start the event loop in a separate thread
        let read_topic_clone = read_topic.clone();
        let _ = tokio::spawn(async move {
            println!("Starting event loop");
            ComelitClient::run_eventloop(eventloop, manager_clone, read_topic_clone).await
        });

        Ok(ComelitClient {
            client,
            request_manager,
            write_topic,
            read_topic,
            req_id: AtomicU32::new(0),
            state: State::Start,
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
                            match serde_json::from_slice::<MqttResponseMessage>(&publish.payload) {
                                Ok(response) => {
                                    println!("Received response: {:?}", response);
                                    let mut manager = request_manager.lock().await;
                                    if !manager.complete_request(&response).await {
                                        println!("Response for unknown request: {:?}", response);
                                    }
                                }
                                Err(e) => eprintln!("Failed to parse response: {:?}", e),
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Connection error: {:?}", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    // Send a request and wait for the response
    async fn send_request(
        &mut self,
        payload: MqttMessage,
    ) -> Result<MqttResponseMessage, ComelitClientError> {
        // Register this request before publishing
        let mut response_receiver = {
            let mut manager = self.request_manager.lock().await;
            manager.add_request(payload.seq_id.clone()).await
        };

        // Publish the request
        let request_json =
            serde_json::to_string(&payload).map_err(|e| ComelitClientError::PublishError(format!("Serialization error: {:?}", e)))?;

        println!("Sending request to topic {} with payload {}", self.write_topic, request_json);
        self.client
            .publish(&self.write_topic, QoS::AtMostOnce, false, request_json)
            .await
            .map_err(|e| ComelitClientError::PublishError(format!("{}", e.to_string())))?;

        // Wait for the response with timeout
        let timeout = Duration::from_secs(5);
        let start = Instant::now();
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
            Ok(response) => Ok(response),
            Err(e) => Err(ComelitClientError::ReadError(
                format!("Failed to receive response: {e}"),
            )),
        }
    }

    pub async fn login(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<(u32, String), ComelitClientError> {
        let state = self.state.clone();
        match state {
            State::Start => {
                let response = self
                    .send_request(make_announce_message(
                        self.req_id.fetch_add(1, Ordering::Relaxed),
                        0,
                    ))
                    .await
                    .map_err(|e| ComelitClientError::GenericError(e.to_string()))?;
                if response.req_result != 0 {
                    return Err(ComelitClientError::LoginError(
                        format!("Announce failed: {}", response.req_result).into(),
                    ));
                }
                let out = response.out_data.first().unwrap();
                let agent_data = serde_json::from_value::<AgentDeviceData>(out.clone()).unwrap();
                println!("Announce HUB description: {}", agent_data.description);
                self.state = State::Announce(agent_data.agent_id);
                Box::pin(self.login(user, password)).await
            }
            State::Announce(agent_id) => {
                let response = self
                    .send_request(make_login_message(
                        self.req_id.fetch_add(1, Ordering::Relaxed),
                        user,
                        password,
                        agent_id,
                    ))
                    .await
                    .map_err(|e| ComelitClientError::GenericError(e.to_string()))?;
                if response.req_result != 0 {
                    return Err(ComelitClientError::LoginError(
                        format!("Login failed: {}", response.req_result).into(),
                    ));
                }
                self.state = State::Login(agent_id, response.session_token.unwrap());
                Box::pin(self.login(user, password)).await
            }
            State::Login(agent_id, session_token) => {
                self.state = State::Login(agent_id, session_token.clone());
                Ok((agent_id, session_token.clone()))
            }
        }
    }

    pub async fn info(
        &mut self,
        device_id: &str,
        detail_level: u8,
    ) -> Result<Option<DeviceData>, ComelitClientError> {
        if !matches!(self.state, State::Login(..)) {
            Err(ComelitClientError::InvalidStateError)
        } else {
            let session_token = match &self.state {
                State::Login(_, token) => token,
                _ => return Err(ComelitClientError::InvalidStateError),
            };
            let resp = self
                .send_request(make_status_message(
                    self.req_id.fetch_add(1, Ordering::Relaxed),
                    session_token.as_str(),
                    device_id,
                    detail_level,
                ))
                .await
                .map_err(|e| ComelitClientError::GenericError(e.to_string()))?;
            Ok(resp.out_data.first().map(|out| {
                serde_json::from_value::<DeviceData>(out.clone()).unwrap()
            }))
        }
    }
}
