// client/src/humidity/worker.rs
use async_trait::async_trait;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::oneshot;
use std::sync::Arc;
use tracing::warn;

use crate::protocol::client::ComelitClientTrait;
use crate::protocol::out_data_messages::ClimaOnOff;

use super::state::HumidityState;

/// Final sink for humidity/dehumidifier state updates. HAP writes HomeKit
/// characteristics on both the Thermostat and HumidifierDehumidifier
/// services; Matter updates a shared atomic state + fires a `Signal` for
/// subscriptions.
#[async_trait]
pub trait HumiditySink: Send + Sync + 'static {
    async fn update(&self, state: HumidityState);
}

enum HumidityCommand {
    SetTargetHumidity(f32, oneshot::Sender<anyhow::Result<()>>),
    SetDehumidifierActive(bool, oneshot::Sender<anyhow::Result<()>>),
    MqttPush(HumidityState),
    SetSink(Box<dyn HumiditySink>),
}

struct HumidityWorker<C: ComelitClientTrait> {
    id: String,
    state: Arc<TokioMutex<HumidityState>>,
    client: C,
    sink: Option<Box<dyn HumiditySink>>,
}

impl<C: ComelitClientTrait + 'static> HumidityWorker<C> {
    fn new(id: String, state: Arc<TokioMutex<HumidityState>>, client: C) -> Self {
        Self { id, state, client, sink: None }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<HumidityCommand>) {
        while let Some(cmd) = rx.recv().await {
            if let Err(e) = self.handle(cmd).await {
                warn!("HumidityWorker {}: {e}", self.id);
            }
        }
    }

    async fn handle(&mut self, cmd: HumidityCommand) -> anyhow::Result<()> {
        match cmd {
            HumidityCommand::SetSink(sink) => {
                self.sink = Some(sink);
            }

            HumidityCommand::SetTargetHumidity(new, reply) => {
                let result = self.client.set_humidity(&self.id, new as i32).await;
                match &result {
                    Ok(()) => {
                        let state = {
                            let mut guard = self.state.lock().await;
                            guard.target_humidity = new;
                            *guard
                        };
                        self.notify_sink(state).await;
                    }
                    Err(e) => warn!("set_humidity failed: {e}"),
                }
                let _ = reply.send(result.map_err(|e| anyhow::anyhow!(e.to_string())));
            }

            HumidityCommand::SetDehumidifierActive(new, reply) => {
                let mode = if new { ClimaOnOff::OnHumi } else { ClimaOnOff::OffHumi };
                let result = self.client.toggle_thermostat_status(&self.id, mode).await;
                match &result {
                    Ok(()) => {
                        let state = {
                            let mut guard = self.state.lock().await;
                            guard.dehumidifier_active = new;
                            guard.dehumidifier_current_state = if new { 1 } else { 0 };
                            *guard
                        };
                        self.notify_sink(state).await;
                    }
                    Err(e) => warn!("toggle_thermostat_status (humi) failed: {e}"),
                }
                let _ = reply.send(result.map_err(|e| anyhow::anyhow!(e.to_string())));
            }

            HumidityCommand::MqttPush(new_state) => {
                *self.state.lock().await = new_state;
                self.notify_sink(new_state).await;
            }
        }
        Ok(())
    }

    async fn notify_sink(&self, state: HumidityState) {
        if let Some(sink) = &self.sink {
            sink.update(state).await;
        }
    }
}

/// Handle for controlling a spawned humidity worker. Dropping every clone
/// lets the worker terminate naturally (channel closes, `rx.recv()` returns
/// `None`) — no explicit shutdown command needed, and deliberately no `Drop`
/// impl here (see Global Constraints: a `Drop`-sends-shutdown pattern on a
/// `Clone` handle kills the worker the moment any single clone is dropped).
#[derive(Clone)]
pub struct HumidityHandle {
    command_sender: Sender<HumidityCommand>,
}

impl HumidityHandle {
    /// Sends the command to the worker and waits for the real outcome of the
    /// hub call (not just "the worker accepted the command") — no
    /// fire-and-forget.
    pub async fn set_target_humidity(&self, value: f32) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .command_sender
            .send(HumidityCommand::SetTargetHumidity(value, reply_tx))
            .await
            .is_err()
        {
            anyhow::bail!("humidity worker is gone");
        }
        reply_rx
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("humidity worker dropped the reply")))
    }

    pub async fn set_dehumidifier_active(&self, active: bool) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .command_sender
            .send(HumidityCommand::SetDehumidifierActive(active, reply_tx))
            .await
            .is_err()
        {
            anyhow::bail!("humidity worker is gone");
        }
        reply_rx
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("humidity worker dropped the reply")))
    }

    pub async fn mqtt_push(&self, state: HumidityState) {
        let _ = self.command_sender.send(HumidityCommand::MqttPush(state)).await;
    }

    pub async fn set_sink(&self, sink: Box<dyn HumiditySink>) {
        let _ = self.command_sender.send(HumidityCommand::SetSink(sink)).await;
    }
}

/// Spawn a worker task for one dehumidifier-equipped thermostat and return a
/// handle to it.
pub fn spawn_humidity_worker<C: ComelitClientTrait + 'static>(
    id: String,
    initial: HumidityState,
    client: C,
) -> HumidityHandle {
    let (command_sender, command_receiver) = mpsc::channel::<HumidityCommand>(32);
    let state = Arc::new(TokioMutex::new(initial));
    let worker = HumidityWorker::new(id, state, client);
    tokio::spawn(worker.run(command_receiver));
    HumidityHandle { command_sender }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::protocol::client::{ComelitClientError, State};
    use crate::protocol::out_data_messages::{ActionType, ClimaMode, HomeDeviceData, ThermoSeason};
    use crate::protocol::scanner::MacAddress;
    use dashmap::DashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::RwLock;
    use tokio::task::JoinHandle;

    #[derive(Clone, Default)]
    struct FakeComelitClient {
        humidity_calls: Arc<RwLock<Vec<(String, i32)>>>,
        toggle_calls: Arc<RwLock<Vec<(String, ClimaOnOff)>>>,
        should_fail: Arc<AtomicBool>,
    }

    #[async_trait]
    impl ComelitClientTrait for FakeComelitClient {
        fn mac_address(&self) -> &MacAddress {
            unimplemented!("not exercised by these tests")
        }
        async fn disconnect(&self) -> Result<(), ComelitClientError> {
            Ok(())
        }
        async fn login(&self, _state: State) -> Result<JoinHandle<()>, ComelitClientError> {
            unimplemented!("not exercised by these tests")
        }
        async fn info<T>(&self, _device_id: &str, _detail_level: u8) -> Result<Vec<T>, ComelitClientError>
        where
            T: serde::de::DeserializeOwned + Send,
        {
            unimplemented!("not exercised by these tests")
        }
        async fn subscribe(&self, _device_id: &str) -> Result<(), ComelitClientError> {
            Ok(())
        }
        async fn fetch_index(&self, _level: u8) -> Result<DashMap<String, HomeDeviceData>, ComelitClientError> {
            unimplemented!("not exercised by these tests")
        }
        async fn fetch_external_devices(&self) -> Result<DashMap<String, HomeDeviceData>, ComelitClientError> {
            unimplemented!("not exercised by these tests")
        }
        async fn send_action(&self, _device_id: &str, _action_type: ActionType, _value: i32) -> Result<(), ComelitClientError> {
            unimplemented!("not exercised by these tests")
        }
        async fn toggle_device_status(&self, _id: &str, _on: bool) -> Result<(), ComelitClientError> {
            unimplemented!("not exercised by these tests")
        }
        async fn toggle_blind_position(&self, _id: &str, _position: u8) -> Result<(), ComelitClientError> {
            unimplemented!("not exercised by these tests")
        }
        async fn set_thermostat_temperature(&self, _id: &str, _temperature: i32) -> Result<(), ComelitClientError> {
            unimplemented!("not exercised by these tests")
        }
        async fn set_thermostat_mode(&self, _id: &str, _mode: ClimaMode) -> Result<(), ComelitClientError> {
            unimplemented!("not exercised by these tests")
        }
        async fn set_thermostat_season(&self, _id: &str, _mode: ThermoSeason) -> Result<(), ComelitClientError> {
            unimplemented!("not exercised by these tests")
        }
        async fn toggle_thermostat_status(&self, id: &str, mode: ClimaOnOff) -> Result<(), ComelitClientError> {
            if self.should_fail.load(Ordering::Acquire) {
                return Err(ComelitClientError::Generic("boom".into()));
            }
            self.toggle_calls.write().await.push((id.to_string(), mode));
            Ok(())
        }
        async fn set_humidity(&self, id: &str, humidity: i32) -> Result<(), ComelitClientError> {
            if self.should_fail.load(Ordering::Acquire) {
                return Err(ComelitClientError::Generic("boom".into()));
            }
            self.humidity_calls.write().await.push((id.to_string(), humidity));
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct FakeSink {
        updates: Arc<RwLock<Vec<HumidityState>>>,
    }

    #[async_trait]
    impl HumiditySink for FakeSink {
        async fn update(&self, state: HumidityState) {
            self.updates.write().await.push(state);
        }
    }

    async fn create_test_worker(initial: HumidityState) -> (HumidityHandle, FakeComelitClient, FakeSink) {
        let client = FakeComelitClient::default();
        let handle = spawn_humidity_worker("DOM#TH#1".to_string(), initial, client.clone());
        let sink = FakeSink::default();
        handle.set_sink(Box::new(sink.clone())).await;
        (handle, client, sink)
    }

    #[tokio::test]
    async fn test_set_target_humidity_echoes_immediately() {
        let (handle, client, sink) = create_test_worker(HumidityState::default()).await;
        let result = handle.set_target_humidity(60.0).await;
        assert!(result.is_ok());
        assert_eq!(client.humidity_calls.read().await.as_slice(), &[("DOM#TH#1".to_string(), 60)]);
        assert_eq!(sink.updates.read().await.last().unwrap().target_humidity, 60.0);
    }

    #[tokio::test]
    async fn test_set_target_humidity_failure_does_not_echo() {
        let client = FakeComelitClient { should_fail: Arc::new(AtomicBool::new(true)), ..Default::default() };
        let handle = spawn_humidity_worker("DOM#TH#2".to_string(), HumidityState::default(), client.clone());
        let sink = FakeSink::default();
        handle.set_sink(Box::new(sink.clone())).await;

        let result = handle.set_target_humidity(60.0).await;

        assert!(result.is_err());
        assert!(sink.updates.read().await.is_empty());
        assert!(client.humidity_calls.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_set_dehumidifier_active_true_sends_on_humi() {
        let (handle, client, sink) = create_test_worker(HumidityState::default()).await;
        let result = handle.set_dehumidifier_active(true).await;
        assert!(result.is_ok());
        assert_eq!(client.toggle_calls.read().await.as_slice(), &[("DOM#TH#1".to_string(), ClimaOnOff::OnHumi)]);
        let last = *sink.updates.read().await.last().unwrap();
        assert!(last.dehumidifier_active);
        assert_eq!(last.dehumidifier_current_state, 1);
    }

    #[tokio::test]
    async fn test_set_dehumidifier_active_false_sends_off_humi() {
        let (handle, client, sink) = create_test_worker(HumidityState { dehumidifier_active: true, ..Default::default() }).await;
        let result = handle.set_dehumidifier_active(false).await;
        assert!(result.is_ok());
        assert_eq!(client.toggle_calls.read().await.as_slice(), &[("DOM#TH#1".to_string(), ClimaOnOff::OffHumi)]);
        let last = *sink.updates.read().await.last().unwrap();
        assert!(!last.dehumidifier_active);
        assert_eq!(last.dehumidifier_current_state, 0);
    }

    #[tokio::test]
    async fn test_mqtt_push_replaces_state_and_notifies() {
        let (handle, _client, sink) = create_test_worker(HumidityState::default()).await;
        let pushed = HumidityState { humidity: 42.0, target_humidity: 55.0, dehumidifier_active: true, dehumidifier_current_state: 3 };
        handle.mqtt_push(pushed).await;
        // mqtt_push doesn't wait for an ack; give the worker a beat to process.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(*sink.updates.read().await.last().unwrap(), pushed);
    }

    #[tokio::test]
    async fn test_worker_survives_dropped_handle_clone() {
        let (handle, _client, _sink) = create_test_worker(HumidityState::default()).await;
        let clone = handle.clone();
        drop(clone);
        // The original handle must still work after a clone is dropped.
        let result = handle.set_target_humidity(50.0).await;
        assert!(result.is_ok());
    }
}
