// client/src/thermostat/worker.rs
use async_trait::async_trait;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc::{self, Sender};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::protocol::client::ComelitClientTrait;
use crate::protocol::out_data_messages::{ClimaMode, ClimaOnOff, ThermoSeason};

use super::state::{TargetHeatingCoolingState, ThermostatState};

/// Final sink for thermostat state updates. HAP writes HomeKit characteristics
/// for the Thermostat service; Matter updates a shared atomic state + fires a
/// `Signal` for subscriptions.
#[async_trait]
pub trait ThermostatSink: Send + Sync + 'static {
    async fn update(&self, state: ThermostatState);
}

enum ThermostatCommand {
    SetTargetTemperature(f32),
    SetHvacMode(TargetHeatingCoolingState),
    MqttPush(ThermostatState),
    SetSink(Box<dyn ThermostatSink>),
}

struct ThermostatWorker<C: ComelitClientTrait> {
    id: String,
    state: Arc<TokioMutex<ThermostatState>>,
    client: C,
    sink: Option<Box<dyn ThermostatSink>>,
}

impl<C: ComelitClientTrait + 'static> ThermostatWorker<C> {
    fn new(id: String, state: Arc<TokioMutex<ThermostatState>>, client: C) -> Self {
        Self { id, state, client, sink: None }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<ThermostatCommand>) {
        while let Some(cmd) = rx.recv().await {
            if let Err(e) = self.handle(cmd).await {
                warn!("ThermostatWorker {}: {e}", self.id);
            }
        }
    }

    async fn handle(&mut self, cmd: ThermostatCommand) -> anyhow::Result<()> {
        match cmd {
            ThermostatCommand::SetSink(sink) => {
                self.sink = Some(sink);
            }

            ThermostatCommand::SetTargetTemperature(new) => {
                let temperature = (new * 10.0) as i32;
                match self.client.set_thermostat_temperature(&self.id, temperature).await {
                    Ok(()) => {
                        // Echo the value we just sent immediately: the confirmation
                        // push from the hub can take minutes (or never arrive for
                        // this specific field). Without this, a stale read makes
                        // the controller think the write failed and retry.
                        let state = {
                            let mut guard = self.state.lock().await;
                            guard.target_temperature = new;
                            *guard
                        };
                        self.notify_sink(state).await;
                    }
                    Err(e) => warn!("set_thermostat_temperature failed: {e}"),
                }
            }

            ThermostatCommand::SetHvacMode(new) => {
                let prev = self.state.lock().await.target_heating_cooling_state;
                debug!("Target heating cooling state updated from {:?} to {:?}", prev, new);

                let toggle_ok = match self
                    .client
                    .toggle_thermostat_status(
                        &self.id,
                        if new == TargetHeatingCoolingState::Off {
                            ClimaOnOff::OffThermo
                        } else {
                            ClimaOnOff::OnThermo
                        },
                    )
                    .await
                {
                    Ok(()) => true,
                    Err(e) => {
                        warn!("toggle_thermostat_status failed: {e}");
                        false
                    }
                };

                if prev == TargetHeatingCoolingState::Auto && new != TargetHeatingCoolingState::Off {
                    if let Err(e) = self.client.set_thermostat_mode(&self.id, ClimaMode::Manual).await {
                        warn!("set_thermostat_mode(Manual) failed: {e}");
                    }
                }

                match new {
                    TargetHeatingCoolingState::Auto => {
                        if let Err(e) = self.client.set_thermostat_mode(&self.id, ClimaMode::Auto).await {
                            warn!("set_thermostat_mode(Auto) failed: {e}");
                        }
                    }
                    TargetHeatingCoolingState::Cool => {
                        if let Err(e) = self.client.set_thermostat_season(&self.id, ThermoSeason::Summer).await {
                            warn!("set_thermostat_season(Summer) failed: {e}");
                        }
                    }
                    TargetHeatingCoolingState::Heat => {
                        if let Err(e) = self.client.set_thermostat_season(&self.id, ThermoSeason::Winter).await {
                            warn!("set_thermostat_season(Winter) failed: {e}");
                        }
                    }
                    TargetHeatingCoolingState::Off => {}
                }

                if toggle_ok {
                    let state = {
                        let mut guard = self.state.lock().await;
                        guard.target_heating_cooling_state = new;
                        guard.heating_cooling_state = new;
                        *guard
                    };
                    self.notify_sink(state).await;
                }
            }

            ThermostatCommand::MqttPush(new_state) => {
                *self.state.lock().await = new_state;
                self.notify_sink(new_state).await;
            }
        }
        Ok(())
    }

    async fn notify_sink(&self, state: ThermostatState) {
        if let Some(sink) = &self.sink {
            sink.update(state).await;
        }
    }
}

/// Handle for controlling a spawned thermostat worker. Dropping every clone
/// lets the worker terminate naturally (channel closes, `rx.recv()` returns
/// `None`) — no explicit shutdown command needed, and deliberately no `Drop`
/// impl here (see Global Constraints: a `Drop`-sends-shutdown pattern on a
/// `Clone` handle kills the worker the moment any single clone is dropped).
#[derive(Clone)]
pub struct ThermostatHandle {
    command_sender: Sender<ThermostatCommand>,
}

impl ThermostatHandle {
    pub async fn set_target_temperature(&self, celsius: f32) {
        let _ = self.command_sender.send(ThermostatCommand::SetTargetTemperature(celsius)).await;
    }

    pub async fn set_hvac_mode(&self, mode: TargetHeatingCoolingState) {
        let _ = self.command_sender.send(ThermostatCommand::SetHvacMode(mode)).await;
    }

    pub async fn mqtt_push(&self, state: ThermostatState) {
        let _ = self.command_sender.send(ThermostatCommand::MqttPush(state)).await;
    }

    pub async fn set_sink(&self, sink: Box<dyn ThermostatSink>) {
        let _ = self.command_sender.send(ThermostatCommand::SetSink(sink)).await;
    }
}

/// Spawn a worker task for one thermostat and return a handle to it.
pub fn spawn_thermostat_worker<C: ComelitClientTrait + 'static>(
    id: String,
    initial: ThermostatState,
    client: C,
) -> ThermostatHandle {
    let (command_sender, command_receiver) = mpsc::channel::<ThermostatCommand>(32);
    let state = Arc::new(TokioMutex::new(initial));
    let worker = ThermostatWorker::new(id, state, client);
    tokio::spawn(worker.run(command_receiver));
    ThermostatHandle { command_sender }
}

#[cfg(test)]
mod test {
    use std::sync::atomic::{AtomicBool, Ordering};

    use dashmap::DashMap;
    use tokio::sync::RwLock;
    use tokio::task::JoinHandle;
    use tokio::time::{sleep, Duration};

    use crate::protocol::client::{ComelitClientError, State};
    use crate::protocol::out_data_messages::{ActionType, HomeDeviceData};
    use crate::protocol::scanner::MacAddress;

    use super::*;

    #[derive(Clone, Default)]
    pub struct FakeComelitClient {
        pub temperature_calls: Arc<RwLock<Vec<(String, i32)>>>,
        pub toggle_calls: Arc<RwLock<Vec<(String, ClimaOnOff)>>>,
        pub mode_calls: Arc<RwLock<Vec<(String, ClimaMode)>>>,
        pub season_calls: Arc<RwLock<Vec<(String, ThermoSeason)>>>,
        pub should_fail: Arc<AtomicBool>,
    }

    #[async_trait]
    impl ComelitClientTrait for FakeComelitClient {
        fn mac_address(&self) -> &MacAddress {
            unimplemented!()
        }

        async fn disconnect(&self) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn login(&self, _state: State) -> Result<JoinHandle<()>, ComelitClientError> {
            unimplemented!()
        }

        async fn info<T>(&self, _device_id: &str, _detail_level: u8) -> Result<Vec<T>, ComelitClientError>
        where
            T: serde::de::DeserializeOwned + Send,
        {
            Ok(vec![])
        }

        async fn subscribe(&self, _device_id: &str) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn fetch_index(&self, _level: u8) -> Result<DashMap<String, HomeDeviceData>, ComelitClientError> {
            Ok(DashMap::new())
        }

        async fn fetch_external_devices(&self) -> Result<DashMap<String, HomeDeviceData>, ComelitClientError> {
            Ok(DashMap::new())
        }

        async fn send_action(&self, _device_id: &str, _action_type: ActionType, _value: i32) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn toggle_device_status(&self, _id: &str, _on: bool) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn toggle_blind_position(&self, _id: &str, _position: u8) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn set_thermostat_temperature(&self, id: &str, temperature: i32) -> Result<(), ComelitClientError> {
            if self.should_fail.load(Ordering::Relaxed) {
                return Err(ComelitClientError::Generic("fail".into()));
            }
            self.temperature_calls.write().await.push((id.to_string(), temperature));
            Ok(())
        }

        async fn set_thermostat_mode(&self, id: &str, mode: ClimaMode) -> Result<(), ComelitClientError> {
            self.mode_calls.write().await.push((id.to_string(), mode));
            Ok(())
        }

        async fn set_thermostat_season(&self, id: &str, mode: ThermoSeason) -> Result<(), ComelitClientError> {
            self.season_calls.write().await.push((id.to_string(), mode));
            Ok(())
        }

        async fn toggle_thermostat_status(&self, id: &str, mode: ClimaOnOff) -> Result<(), ComelitClientError> {
            if self.should_fail.load(Ordering::Relaxed) {
                return Err(ComelitClientError::Generic("fail".into()));
            }
            self.toggle_calls.write().await.push((id.to_string(), mode));
            Ok(())
        }

        async fn set_humidity(&self, _id: &str, _humidity: i32) -> Result<(), ComelitClientError> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct FakeSink {
        updates: Arc<RwLock<Vec<ThermostatState>>>,
    }

    #[async_trait]
    impl ThermostatSink for FakeSink {
        async fn update(&self, state: ThermostatState) {
            self.updates.write().await.push(state);
        }
    }

    async fn create_test_worker(initial: ThermostatState) -> (ThermostatHandle, FakeComelitClient, FakeSink) {
        let client = FakeComelitClient::default();
        let handle = spawn_thermostat_worker("test-id".to_string(), initial, client.clone());
        let sink = FakeSink::default();
        handle.set_sink(Box::new(sink.clone())).await;
        sleep(Duration::from_millis(20)).await;
        (handle, client, sink)
    }

    #[tokio::test]
    async fn test_set_target_temperature_echoes_immediately() {
        let (handle, client, sink) = create_test_worker(ThermostatState::default()).await;

        handle.set_target_temperature(21.5).await;
        sleep(Duration::from_millis(20)).await;

        assert_eq!(client.temperature_calls.read().await.as_slice(), &[("test-id".to_string(), 215)]);
        let updates = sink.updates.read().await;
        assert_eq!(updates.last().unwrap().target_temperature, 21.5);
    }

    #[tokio::test]
    async fn test_set_target_temperature_failure_does_not_echo() {
        let (handle, client, sink) = create_test_worker(ThermostatState::default()).await;
        client.should_fail.store(true, Ordering::Relaxed);

        handle.set_target_temperature(21.5).await;
        sleep(Duration::from_millis(20)).await;

        assert!(sink.updates.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_set_hvac_mode_heat_sets_season_winter_and_manual() {
        let (handle, client, sink) = create_test_worker(ThermostatState::default()).await;

        handle.set_hvac_mode(TargetHeatingCoolingState::Heat).await;
        sleep(Duration::from_millis(20)).await;

        assert_eq!(client.toggle_calls.read().await.as_slice(), &[("test-id".to_string(), ClimaOnOff::OnThermo)]);
        assert_eq!(client.season_calls.read().await.as_slice(), &[("test-id".to_string(), ThermoSeason::Winter)]);
        assert!(client.mode_calls.read().await.is_empty()); // Manual only sent when prev was Auto
        let updates = sink.updates.read().await;
        assert_eq!(updates.last().unwrap().heating_cooling_state, TargetHeatingCoolingState::Heat);
    }

    #[tokio::test]
    async fn test_set_hvac_mode_off_sends_off_toggle_only() {
        let (handle, client, _sink) = create_test_worker(ThermostatState::default()).await;

        handle.set_hvac_mode(TargetHeatingCoolingState::Off).await;
        sleep(Duration::from_millis(20)).await;

        assert_eq!(client.toggle_calls.read().await.as_slice(), &[("test-id".to_string(), ClimaOnOff::OffThermo)]);
        assert!(client.season_calls.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_set_hvac_mode_auto_sends_auto_mode() {
        let (handle, client, sink) = create_test_worker(ThermostatState::default()).await;

        handle.set_hvac_mode(TargetHeatingCoolingState::Auto).await;
        sleep(Duration::from_millis(20)).await;

        assert_eq!(client.mode_calls.read().await.as_slice(), &[("test-id".to_string(), ClimaMode::Auto)]);
        let updates = sink.updates.read().await;
        assert_eq!(updates.last().unwrap().heating_cooling_state, TargetHeatingCoolingState::Auto);
    }

    #[tokio::test]
    async fn test_set_hvac_mode_from_auto_to_heat_sends_manual_then_season() {
        let initial = ThermostatState {
            target_heating_cooling_state: TargetHeatingCoolingState::Auto,
            ..Default::default()
        };
        let (handle, client, _sink) = create_test_worker(initial).await;

        handle.set_hvac_mode(TargetHeatingCoolingState::Heat).await;
        sleep(Duration::from_millis(20)).await;

        assert_eq!(client.mode_calls.read().await.as_slice(), &[("test-id".to_string(), ClimaMode::Manual)]);
        assert_eq!(client.season_calls.read().await.as_slice(), &[("test-id".to_string(), ThermoSeason::Winter)]);
    }

    #[tokio::test]
    async fn test_mqtt_push_replaces_state_and_notifies() {
        let (handle, _client, sink) = create_test_worker(ThermostatState::default()).await;

        let pushed = ThermostatState {
            temperature: 19.5,
            target_temperature: 20.0,
            heating_cooling_state: TargetHeatingCoolingState::Heat,
            target_heating_cooling_state: TargetHeatingCoolingState::Heat,
        };
        handle.mqtt_push(pushed).await;
        sleep(Duration::from_millis(20)).await;

        let updates = sink.updates.read().await;
        assert_eq!(updates.last().unwrap().temperature, 19.5);
    }

    #[tokio::test]
    async fn test_worker_survives_dropped_handle_clone() {
        let (handle, client, _sink) = create_test_worker(ThermostatState::default()).await;

        let clone = handle.clone();
        drop(clone);
        sleep(Duration::from_millis(20)).await;

        handle.set_target_temperature(20.0).await;
        sleep(Duration::from_millis(20)).await;

        assert_eq!(client.temperature_calls.read().await.len(), 1);
    }
}
