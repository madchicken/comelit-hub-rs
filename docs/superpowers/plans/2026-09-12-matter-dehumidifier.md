# Matter Dehumidifier Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bridge each Comelit dehumidifier-equipped thermostat's on/off control and current relative humidity onto a separate Matter endpoint, and extract the existing HAP-only humidity worker into a shared `client/src/humidity/` module so both bridges use it.

**Architecture:** A new protocol-agnostic `client/src/humidity/` module (mirroring `client/src/thermostat/`) owns `HumidityState`, a `HumidityWorker`/`HumidityHandle` pair with oneshot-reply commands, and a `HumiditySink` trait. HAP migrates its existing local worker onto this shared module. Matter gets a new `matter/src/dehumidifier.rs` implementing two `ClusterAsyncHandler`s directly (`OnOff` and `RelativeHumidityMeasurement`, no setpoint-writing cluster exists in Matter 1.5.1) on a dedicated bridged endpoint, wired into `matter/src/bridge.rs`'s `BridgedEntry` enum and discovered in `matter/src/main.rs` alongside — not instead of — the parent thermostat's own endpoint.

**Tech Stack:** Rust, tokio, `rs-matter` (pinned git rev `e8b0b0cbb20bf312a9c52fc1ee56541037a3b9c9`, Matter 1.5.1 data model), `async-trait`.

**Spec:** docs/superpowers/specs/2026-09-12-matter-dehumidifier-design.md

## Global Constraints

- No fire-and-forget: every write command (`SetTargetHumidity`, `SetDehumidifierActive`) resolves to the real outcome of the hub call via an `oneshot::Sender<anyhow::Result<()>>` reply — never an immediate synthetic `Ok(())`.
- No writable humidity setpoint on the Matter side: `RelativeHumidityMeasurement` (Matter 1.5.1's only humidity cluster) is read-only. Only current humidity (read) and dehumidifier on/off (read/write) are bridged.
- The dehumidifier is bridged as its **own separate Matter endpoint**, distinct from its parent thermostat's endpoint — never `SystemMode::Dry` on the Thermostat cluster (Comelit's dehumidifier runs alongside cooling, not instead of it).
- Only thermostats where `sub_type == ObjectSubtype::ClimaThermostatDehumidifier` get a dehumidifier endpoint. Standalone `ClimaDehumidifier` devices (sub_type 17) are out of scope.
- `HumidityHandle`, mirroring `ThermostatHandle`/`WindowCoveringHandle`: `#[derive(Clone)]`, no `Drop` impl (dropping every clone lets the worker's channel close naturally — a `Drop`-sends-shutdown pattern on a `Clone` handle kills the worker the moment any single clone is dropped).
- `HumidityWorker<C: ComelitClientTrait>` is generic over `ComelitClientTrait` (defined in `client/src/protocol/client.rs`), not concrete `ComelitClient` — matching `ThermostatWorker`/`WindowCoveringWorker`, and required for unit testing with a fake client.

---

## Task 1: Shared `client/src/humidity` module

**Files:**
- Create: `client/src/humidity/state.rs`
- Create: `client/src/humidity/worker.rs`
- Create: `client/src/humidity/mod.rs`
- Modify: `client/src/lib.rs`

**Interfaces:**
- Consumes: `crate::protocol::client::ComelitClientTrait` (existing trait with `set_humidity(&self, id: &str, humidity: i32) -> Result<(), ComelitClientError>` and `toggle_thermostat_status(&self, id: &str, mode: ClimaOnOff) -> Result<(), ComelitClientError>`), `crate::protocol::out_data_messages::{ClimaMode, ClimaOnOff, DeviceStatus, ThermostatDeviceData}`.
- Produces (used by Tasks 2 and 3): `comelit_client_rs::humidity::HumidityState` (fields `humidity: f32`, `target_humidity: f32`, `dehumidifier_active: bool`, `dehumidifier_current_state: u8`, all `pub`), `comelit_client_rs::humidity::HumiditySink` trait (`async fn update(&self, state: HumidityState)`), `comelit_client_rs::humidity::HumidityHandle` (`Clone`, methods `pub async fn set_target_humidity(&self, value: f32) -> anyhow::Result<()>`, `pub async fn set_dehumidifier_active(&self, active: bool) -> anyhow::Result<()>`, `pub async fn mqtt_push(&self, state: HumidityState)`, `pub async fn set_sink(&self, sink: Box<dyn HumiditySink>)`), `comelit_client_rs::humidity::spawn_humidity_worker(id: String, initial: HumidityState, client: C) -> HumidityHandle` generic over `C: ComelitClientTrait + 'static`.

- [ ] **Step 1: Write `client/src/humidity/state.rs` with its tests**

```rust
use crate::protocol::out_data_messages::{ClimaMode, DeviceStatus, ThermostatDeviceData};

#[derive(Debug, Clone, Copy, Default)]
pub struct HumidityState {
    pub humidity: f32,
    pub target_humidity: f32,
    pub dehumidifier_active: bool,
    pub dehumidifier_current_state: u8, // 0=INACTIVE, 1=IDLE, 3=DEHUMIDIFYING
}

impl From<&ThermostatDeviceData> for HumidityState {
    fn from(data: &ThermostatDeviceData) -> Self {
        let humidity = data
            .humidity
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap_or_default();

        let target_humidity = data
            .humi_active_threshold
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap_or_default();

        let auto_man_umi = data.auto_man_umi.clone().unwrap_or_default();
        let dehumidifier_active = !matches!(
            auto_man_umi,
            ClimaMode::None | ClimaMode::OffAuto | ClimaMode::OffManual
        );
        let dehumidifier_current_state = if !dehumidifier_active {
            0
        } else if matches!(data.status, Some(DeviceStatus::On) | Some(DeviceStatus::Running)) {
            3
        } else {
            1
        };

        Self {
            humidity,
            target_humidity,
            dehumidifier_active,
            dehumidifier_current_state,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::protocol::out_data_messages::{ObjectSubtype, ObjectType};

    fn base_data() -> ThermostatDeviceData {
        ThermostatDeviceData {
            id: "DOM#TH#1".to_string(),
            r#type: ObjectType::Thermostat,
            sub_type: ObjectSubtype::ClimaThermostatDehumidifier,
            status: Some(DeviceStatus::On),
            description: None,
            temperature: None,
            auto_man: None,
            season: None,
            active_threshold: None,
            humidity: Some("55.5".to_string()),
            humi_active_threshold: Some("60".to_string()),
            auto_man_umi: Some(ClimaMode::Manual),
        }
    }

    #[test]
    fn test_active_manual_dehumidifying_is_state_3() {
        let data = base_data();
        let state = HumidityState::from(&data);
        assert_eq!(state.humidity, 55.5);
        assert_eq!(state.target_humidity, 60.0);
        assert!(state.dehumidifier_active);
        assert_eq!(state.dehumidifier_current_state, 3);
    }

    #[test]
    fn test_active_but_not_running_is_idle_state_1() {
        let mut data = base_data();
        data.status = Some(DeviceStatus::Off);
        let state = HumidityState::from(&data);
        assert!(state.dehumidifier_active);
        assert_eq!(state.dehumidifier_current_state, 1);
    }

    #[test]
    fn test_off_manual_is_inactive_state_0() {
        let mut data = base_data();
        data.auto_man_umi = Some(ClimaMode::OffManual);
        let state = HumidityState::from(&data);
        assert!(!state.dehumidifier_active);
        assert_eq!(state.dehumidifier_current_state, 0);
    }

    #[test]
    fn test_off_auto_is_inactive_state_0() {
        let mut data = base_data();
        data.auto_man_umi = Some(ClimaMode::OffAuto);
        let state = HumidityState::from(&data);
        assert!(!state.dehumidifier_active);
        assert_eq!(state.dehumidifier_current_state, 0);
    }

    #[test]
    fn test_unparseable_values_default_to_zero() {
        let mut data = base_data();
        data.humidity = Some("not-a-number".to_string());
        data.humi_active_threshold = None;
        let state = HumidityState::from(&data);
        assert_eq!(state.humidity, 0.0);
        assert_eq!(state.target_humidity, 0.0);
    }
}
```

- [ ] **Step 2: Run the new tests to verify they pass**

Run: `cargo test -p comelit-client-rs humidity::state`
Expected: 5 tests pass (the file has no `mod.rs` entry yet, so run this after Step 4's `mod.rs` is in place — if `cargo test` errors with "module not found" first, that's expected until Step 4; come back and re-run after it).

- [ ] **Step 3: Write `client/src/humidity/worker.rs` with its tests**

```rust
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
```

Note: `HumidityState` needs `PartialEq` for the `test_mqtt_push_replaces_state_and_notifies` assertion — add `PartialEq` to its `#[derive(...)]` in `state.rs` from Step 1 (go back and change `#[derive(Debug, Clone, Copy, Default)]` to `#[derive(Debug, Clone, Copy, Default, PartialEq)]`).

- [ ] **Step 4: Write `client/src/humidity/mod.rs`**

```rust
// client/src/humidity/mod.rs
mod state;
mod worker;

pub use state::HumidityState;
pub use worker::{HumidityHandle, HumiditySink, spawn_humidity_worker};
```

- [ ] **Step 5: Wire the module into `client/src/lib.rs`**

In `client/src/lib.rs`, add the new module declaration next to the existing ones:

```rust
pub mod covering;
pub mod humidity;
pub mod thermostat;
```

- [ ] **Step 6: Run all tests to verify they pass**

Run: `cargo test -p comelit-client-rs humidity`
Expected: all tests in `humidity::state::test` and `humidity::worker::test` pass (11 tests total: 5 in `state::test`, 6 in `worker::test`). Also run `cargo build -p comelit-client-rs` to confirm no warnings about the new module being unused.

- [ ] **Step 7: Commit**

```bash
git add client/src/humidity client/src/lib.rs
git commit -m "Extract shared client/src/humidity module from HAP's HumidityWorker"
```

---

## Task 2: Migrate HAP onto the shared humidity module

**Files:**
- Modify: `hap/src/accessories/thermostat.rs`
- Delete: `hap/src/accessories/state/thermostat.rs`
- Modify: `hap/src/accessories/state/mod.rs`

**Interfaces:**
- Consumes: `comelit_client_rs::humidity::{HumidityHandle, HumiditySink, HumidityState, spawn_humidity_worker}` (from Task 1).
- Produces: no new public interface — `ComelitThermostatAccessory` keeps its existing `pub(crate)` surface (`new`, `update` via `ComelitAccessory<ThermostatDeviceData>`).

- [ ] **Step 1: Delete the old local `HumidityState` and its module registration**

```bash
rm hap/src/accessories/state/thermostat.rs
```

In `hap/src/accessories/state/mod.rs`, remove the line:

```rust
pub(crate) mod thermostat;
```

leaving:

```rust
pub(crate) mod door;
pub(crate) mod light;
```

- [ ] **Step 2: Update imports in `hap/src/accessories/thermostat.rs`**

Replace the import block at the top of the file:

```rust
use crate::accessories::{ComelitAccessory, state::thermostat::HumidityState};
use crate::web::metrics::Metrics;
use comelit_client_rs::thermostat::{
    TargetHeatingCoolingState, ThermostatHandle, ThermostatSink, ThermostatState,
    spawn_thermostat_worker,
};
use comelit_client_rs::{ComelitClient, ObjectSubtype, ThermostatDeviceData};
```

with:

```rust
use crate::accessories::ComelitAccessory;
use crate::web::metrics::Metrics;
use comelit_client_rs::humidity::{HumidityHandle, HumiditySink, HumidityState, spawn_humidity_worker};
use comelit_client_rs::thermostat::{
    TargetHeatingCoolingState, ThermostatHandle, ThermostatSink, ThermostatState,
    spawn_thermostat_worker,
};
use comelit_client_rs::{ComelitClient, ObjectSubtype, ThermostatDeviceData};
```

Also remove `use tokio::sync::mpsc::{self, Sender};` if nothing else in the file still needs `mpsc`/`Sender` after Step 3 — check with `grep -n "mpsc::\|Sender<" hap/src/accessories/thermostat.rs` once Step 3 is done; if the only remaining usages are gone, delete that `use` line (the file's `Mutex` import from `tokio::sync::Mutex` stays, it's still used by `thermal_state_ro` and the new `humidity_arc_state`).

- [ ] **Step 3: Replace `HumidityCommand`/`HumidityWorker` with a `HapHumiditySink`**

Delete the entire block from the `HumidityCommand` enum through the end of `impl HumidityWorker { ... }` (currently lines 182-312: the `#[derive(Debug)] enum HumidityCommand { ... }`, `struct HumidityWorker { ... }`, and its `impl HumidityWorker { ... }` block including `new`, `run`, `handle`, `update_accessory`), and replace it with:

```rust
/// Writes humidity/dehumidifier state updates into the HomeKit `Thermostat`
/// service's humidity characteristics and the separate
/// `HumidifierDehumidifier` service.
///
/// `state` mirrors the shared worker's private humidity state so that the
/// read-callback closures registered in `ComelitThermostatAccessory::new`
/// (which cannot reach into the worker task directly) always observe the
/// latest value rather than a stale, construction-time snapshot. It shares
/// the same `Arc` as `humidity_arc_state` in `ComelitThermostatAccessory::new`.
struct HapHumiditySink {
    device_id: String,
    accessory: Accessory,
    state: Arc<Mutex<HumidityState>>,
}

#[async_trait]
impl HumiditySink for HapHumiditySink {
    async fn update(&self, state: HumidityState) {
        // Same lock-ordering constraint as `HapThermostatSink::update`:
        // release the humidity-state lock before taking the accessory lock,
        // since `hap-rs` holds the accessory lock while running the
        // `on_read_async` closures that lock this very same state mutex.
        // Keep this an explicit block.
        {
            let mut guard = self.state.lock().await;
            *guard = state;
        }

        let mut acc = self.accessory.lock().await;

        if let Some(thermostat_service) = acc.get_mut_service(HapType::Thermostat) {
            if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::CurrentRelativeHumidity) {
                if let Err(e) = ch.update_value(Value::from(state.humidity)).await {
                    warn!("update_value for thermostat {} CurrentRelativeHumidity failed: {e}", self.device_id);
                }
            }
            if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::TargetRelativeHumidity) {
                if let Err(e) = ch.update_value(Value::from(state.target_humidity)).await {
                    warn!("update_value for thermostat {} TargetRelativeHumidity failed: {e}", self.device_id);
                }
            }
        }

        if let Some(hd_service) = acc.get_mut_service(HapType::HumidifierDehumidifier) {
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::Active) {
                if let Err(e) = ch.update_value(Value::from(state.dehumidifier_active as u8)).await {
                    warn!("update_value for thermostat {} Active failed: {e}", self.device_id);
                }
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::CurrentHumidifierDehumidifierState) {
                if let Err(e) = ch.update_value(Value::from(state.dehumidifier_current_state)).await {
                    warn!("update_value for thermostat {} CurrentHumidifierDehumidifierState failed: {e}", self.device_id);
                }
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::CurrentRelativeHumidity) {
                if let Err(e) = ch.update_value(Value::from(state.humidity)).await {
                    warn!("update_value for thermostat {} humidifier CurrentRelativeHumidity failed: {e}", self.device_id);
                }
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::RelativeHumidityDehumidifierThreshold) {
                if let Err(e) = ch.update_value(Value::from(state.target_humidity)).await {
                    warn!("update_value for thermostat {} RelativeHumidityDehumidifierThreshold failed: {e}", self.device_id);
                }
            }
        }
    }
}
```

- [ ] **Step 4: Replace the `humidity_sender` field with a `HumidityHandle`**

In `pub(crate) struct ComelitThermostatAccessory`, change:

```rust
    thermostat_handle: ThermostatHandle,
    humidity_sender: Sender<HumidityCommand>,
```

to:

```rust
    thermostat_handle: ThermostatHandle,
    #[allow(dead_code)]
    humidity_handle: HumidityHandle,
```

(`#[allow(dead_code)]` matches the existing `#[allow(dead_code)] accessory: Accessory` field right below it — `humidity_handle` is kept alive on the struct so its worker task keeps running via the retained `Sender`/channel, but nothing reads the field back out after construction, same as `accessory`.)

- [ ] **Step 5: Update `ComelitAccessory::update` to push through the handle**

Change:

```rust
    async fn update(&mut self, thermostat_data: &ThermostatDeviceData) -> Result<()> {
        self.thermostat_handle.mqtt_push(ThermostatState::from(thermostat_data)).await;
        self.humidity_sender
            .send(HumidityCommand::MqttPush(HumidityState::from(thermostat_data)))
            .await
            .ok();
        Ok(())
    }
```

to:

```rust
    async fn update(&mut self, thermostat_data: &ThermostatDeviceData) -> Result<()> {
        self.thermostat_handle.mqtt_push(ThermostatState::from(thermostat_data)).await;
        self.humidity_handle.mqtt_push(HumidityState::from(thermostat_data)).await;
        Ok(())
    }
```

- [ ] **Step 6: Rewrite the humidity wiring in `ComelitThermostatAccessory::new`**

Replace the whole block from `// ── Humidity/dehumidifier worker (local, unchanged from before) ────` through the `humidity_worker.run(...)` spawn (currently lines 439-536) with:

```rust
        // ── Humidity/dehumidifier handle (shared client::humidity module) ──

        let humidity_arc_state = Arc::new(Mutex::new(humidity_state));
        let humidity_handle = spawn_humidity_worker(comelit_id.clone(), humidity_state, client);

        if let Some(ref mut hd) = accessory.humidifier_dehumidifier {
            hd.target_humidifier_dehumidifier_state.set_value(Value::from(2u8)).await?;
            hd.active.set_value(Value::from(humidity_state.dehumidifier_active as u8)).await?;
            hd.current_humidifier_dehumidifier_state.set_value(Value::from(humidity_state.dehumidifier_current_state)).await?;
            hd.current_relative_humidity.set_value(Value::from(humidity_state.humidity)).await?;

            {
                let s = Arc::clone(&humidity_arc_state);
                hd.active.on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.dehumidifier_active as u8)) }.boxed()
                }));
            }
            {
                let s = Arc::clone(&humidity_arc_state);
                hd.current_humidifier_dehumidifier_state.on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.dehumidifier_current_state)) }.boxed()
                }));
            }
            {
                let s = Arc::clone(&humidity_arc_state);
                hd.current_relative_humidity.on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.humidity)) }.boxed()
                }));
            }

            if let Some(ref mut threshold) = hd.relative_humidity_dehumidifier_threshold {
                threshold.set_value(Value::from(humidity_state.target_humidity)).await?;
                {
                    let s = Arc::clone(&humidity_arc_state);
                    threshold.on_read_async(Some(move || {
                        let s = s.clone();
                        async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.target_humidity)) }.boxed()
                    }));
                }
                let handle = humidity_handle.clone();
                threshold.on_update_async(Some(move |_prev, new: f32| {
                    let handle = handle.clone();
                    async move {
                        Metrics::inc_hap_requests();
                        handle.set_target_humidity(new).await?;
                        Ok(())
                    }
                    .boxed()
                }));
            }

            {
                let handle = humidity_handle.clone();
                hd.active.on_update_async(Some(move |_prev: u8, new: u8| {
                    let handle = handle.clone();
                    async move {
                        Metrics::inc_hap_requests();
                        handle.set_dehumidifier_active(new == 1).await?;
                        Ok(())
                    }
                    .boxed()
                }));
            }
        }

        if let Some(ref mut char) = accessory.thermostat.current_relative_humidity {
            let s = Arc::clone(&humidity_arc_state);
            char.on_read_async(Some(move || {
                let s = s.clone();
                async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.humidity)) }.boxed()
            }));
        }

        if let Some(ref mut char) = accessory.thermostat.target_relative_humidity {
            {
                let s = Arc::clone(&humidity_arc_state);
                char.on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.target_humidity)) }.boxed()
                }));
            }
            let handle = humidity_handle.clone();
            char.on_update_async(Some(move |_prev, new: f32| {
                let handle = handle.clone();
                async move {
                    Metrics::inc_hap_requests();
                    handle.set_target_humidity(new).await?;
                    Ok(())
                }
                .boxed()
            }));
        }
```

Note what changed from the old code: no `humidity_sender`/`humidity_receiver` channel is created here anymore (the channel lives inside `spawn_humidity_worker`); `client` is moved into `spawn_humidity_worker(...)` directly instead of into a separately-constructed `HumidityWorker::new(..., client)` — this means `client` must not be used again after this call in `new()` (check: the thermal handle spawn at `let thermostat_handle = spawn_thermostat_worker(comelit_id.clone(), thermal_state, client.clone());` earlier already uses `client.clone()`, so passing plain `client` here, after that line, consumes the last owned copy — correct, since nothing later in the function needs `client` again).

- [ ] **Step 7: Wire the sink after accessory registration**

Replace:

```rust
        thermostat_handle
            .set_sink(Box::new(HapThermostatSink {
                device_id: data.id.clone(),
                accessory: accessory.clone(),
                state: Arc::clone(&thermal_state_ro),
            }))
            .await;
        humidity_sender.send(HumidityCommand::SetAccessory(accessory.clone())).await.ok();

        Ok(Self {
            id: data.id.clone(),
            name,
            thermostat_handle,
            humidity_sender,
            accessory,
        })
```

with:

```rust
        thermostat_handle
            .set_sink(Box::new(HapThermostatSink {
                device_id: data.id.clone(),
                accessory: accessory.clone(),
                state: Arc::clone(&thermal_state_ro),
            }))
            .await;
        humidity_handle
            .set_sink(Box::new(HapHumiditySink {
                device_id: data.id.clone(),
                accessory: accessory.clone(),
                state: Arc::clone(&humidity_arc_state),
            }))
            .await;

        Ok(Self {
            id: data.id.clone(),
            name,
            thermostat_handle,
            humidity_handle,
            accessory,
        })
```

- [ ] **Step 8: Build and fix any remaining references**

Run: `cargo build -p comelit-hub-hap 2>&1 | head -80`
Expected: clean build. If it reports an unused `mpsc`/`Sender` import, remove that `use` line (per the note in Step 2). If it reports `humidity_state` moved before use (it's `Copy`, so this shouldn't happen, but double check the `spawn_humidity_worker(comelit_id.clone(), humidity_state, client)` call in Step 6 comes after all earlier uses of `humidity_state` — it does, since `humidity_state` is only read from, being `Copy`).

- [ ] **Step 9: Run the HAP crate's test suite**

Run: `cargo test -p comelit-hub-hap`
Expected: all existing tests pass (this task is a refactor with no new HAP-crate tests — coverage for the humidity logic itself lives in Task 1's `client` crate tests).

- [ ] **Step 10: Commit**

```bash
git add hap/src/accessories/thermostat.rs hap/src/accessories/state/mod.rs
git rm hap/src/accessories/state/thermostat.rs
git commit -m "Migrate HAP dehumidifier control onto shared client::humidity module"
```

---

## Task 3: `matter/src/dehumidifier.rs`

**Files:**
- Create: `matter/src/dehumidifier.rs`
- Modify: `matter/src/main.rs` (only to add `mod dehumidifier;` so the file compiles as part of the crate — full wiring is Task 5)

**Interfaces:**
- Consumes: `comelit_client_rs::humidity::{HumidityHandle, HumiditySink, HumidityState}` (Task 1), `comelit_client_rs::{HomeDeviceData, StatusUpdate}`, `rs_matter::dm::clusters::decl::on_off` and `rs_matter::dm::clusters::decl::relative_humidity_measurement` (both codegen'd from the `rs-matter` pin — confirmed present at `target/debug/build/rs-matter-*/out/clusters_generated/{on_off,relative_humidity_measurement}.rs`).
- Produces (used by Task 4): `pub struct DehumidifierMatterState { pub ep_id: u16, pub device_id: String, pub active: AtomicBool, pub humidity: AtomicU16, pub signal: Signal<CriticalSectionRawMutex, ()>, pub handle: HumidityHandle }` with `pub fn new(ep_id: u16, device_id: String, initial: HumidityState, handle: HumidityHandle) -> Self`; `pub struct DehumidifierMatterSink` with `pub fn new(state: Arc<DehumidifierMatterState>) -> Self`; `pub struct ComelitDehumidifierOnOffHandler` with `pub fn new(dataver: Dataver, state: Arc<DehumidifierMatterState>) -> Self` implementing `on_off::ClusterAsyncHandler`; `pub struct ComelitHumidityMeasurementHandler` with `pub fn new(dataver: Dataver, state: Arc<DehumidifierMatterState>) -> Self` implementing `relative_humidity_measurement::ClusterAsyncHandler`; `pub struct MultiDehumidifierObserver { pub states: Vec<Arc<DehumidifierMatterState>> }` implementing `StatusUpdate`.

- [ ] **Step 1: Add the module declaration to `main.rs`**

In `matter/src/main.rs`, add `mod dehumidifier;` next to the other `mod` declarations:

```rust
mod bridge;
mod covering;
mod dehumidifier;
mod light;
mod mdns;
mod thermostat;
```

- [ ] **Step 2: Write `matter/src/dehumidifier.rs`**

```rust
// matter/src/dehumidifier.rs
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use async_trait::async_trait;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use log::info;

use rs_matter::dm::Cluster;
use rs_matter::dm::clusters::decl::on_off::{self as on_off_cluster};
use rs_matter::dm::clusters::decl::relative_humidity_measurement::{
    self as humidity_cluster,
};
use rs_matter::dm::{Dataver, HandlerContext, InvokeContext, ReadContext, WriteContext};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::Nullable;
use rs_matter::with;

use comelit_client_rs::humidity::{HumidityHandle, HumiditySink, HumidityState};
use comelit_client_rs::{HomeDeviceData, StatusUpdate};

/// State shared between the two Matter cluster handlers on a dehumidifier's
/// bridged endpoint and the Comelit `HumidityHandle` worker. Humidity is
/// stored in Matter's native unit: hundredths of a percent (u16), converted
/// from/to the client's percent-as-f32 at the boundary — same convention as
/// `ThermostatMatterState`'s hundredths-of-a-degree.
pub struct DehumidifierMatterState {
    pub ep_id: u16,
    pub device_id: String,
    pub active: AtomicBool,
    pub humidity: AtomicU16,
    pub signal: Signal<CriticalSectionRawMutex, ()>,
    pub handle: HumidityHandle,
}

fn percent_to_matter(percent: f32) -> u16 {
    (percent * 100.0).round().clamp(0.0, 10000.0) as u16
}

fn matter_to_percent(hundredths: u16) -> f32 {
    hundredths as f32 / 100.0
}

impl DehumidifierMatterState {
    pub fn new(
        ep_id: u16,
        device_id: String,
        initial: HumidityState,
        handle: HumidityHandle,
    ) -> Self {
        Self {
            ep_id,
            device_id,
            active: AtomicBool::new(initial.dehumidifier_active),
            humidity: AtomicU16::new(percent_to_matter(initial.humidity)),
            signal: Signal::new(),
            handle,
        }
    }
}

/// Publishes worker state updates into the shared `DehumidifierMatterState`
/// and wakes any pending Matter subscription poll via `Signal`.
pub struct DehumidifierMatterSink {
    state: Arc<DehumidifierMatterState>,
}

impl DehumidifierMatterSink {
    pub fn new(state: Arc<DehumidifierMatterState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl HumiditySink for DehumidifierMatterSink {
    async fn update(&self, state: HumidityState) {
        self.state.active.store(state.dehumidifier_active, Ordering::Relaxed);
        self.state
            .humidity
            .store(percent_to_matter(state.humidity), Ordering::Relaxed);
        self.state.signal.signal(());
        info!(
            "MQTT → Matter ep{} dehumidifier: {} active={} humidity={:.1}",
            self.state.ep_id, self.state.device_id, state.dehumidifier_active, state.humidity
        );
    }
}

/// Implements the `OnOff` cluster for one bridged dehumidifier. No LIGHTING /
/// DEAD_FRONT_BEHAVIOR / OFF_ONLY features — this is a plain on/off switch.
///
/// Implements `ClusterAsyncHandler` directly (not the `OnOffHooks` wrapper
/// `ComelitOnOffHooks` in `light.rs` uses) because `OnOffHooks::set_on_off`
/// is synchronous and cannot report failure. `handle_on`/`handle_off` here
/// are `async` and `.await` the real hub outcome, matching
/// `ComelitThermostatHandler`/`ComelitCoveringHandler` — no fire-and-forget.
///
/// Owns the subscription-notification loop for *both* cluster handlers on
/// this endpoint (this one and `ComelitHumidityMeasurementHandler`): they
/// share one `Signal` on `DehumidifierMatterState`, and `embassy_sync::signal::Signal`
/// supports only one waiter — if both handlers' `run()` called
/// `state.signal.wait()` independently, only the most recently polled one
/// would ever wake. `ComelitHumidityMeasurementHandler::run` is therefore
/// left on the trait default (`pending()`), and only this handler is
/// included in `ComelitBridgeHandler::run`'s select set for this entry (see
/// Task 4).
pub struct ComelitDehumidifierOnOffHandler {
    dataver: Dataver,
    state: Arc<DehumidifierMatterState>,
}

impl ComelitDehumidifierOnOffHandler {
    pub fn new(dataver: Dataver, state: Arc<DehumidifierMatterState>) -> Self {
        Self { dataver, state }
    }
}

impl on_off_cluster::ClusterAsyncHandler for ComelitDehumidifierOnOffHandler {
    const CLUSTER: Cluster<'static> = on_off_cluster::FULL_CLUSTER
        .with_revision(6)
        .with_attrs(with!(required))
        .with_cmds(with!(
            on_off_cluster::CommandId::Off
                | on_off_cluster::CommandId::On
                | on_off_cluster::CommandId::Toggle
        ));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        loop {
            self.state.signal.wait().await;
            ctx.notify_cluster_changed(self.state.ep_id, Self::CLUSTER.id);
            ctx.notify_cluster_changed(self.state.ep_id, ComelitHumidityMeasurementHandler::CLUSTER.id);
        }
    }

    async fn on_off(&self, _ctx: impl ReadContext) -> Result<bool, Error> {
        Ok(self.state.active.load(Ordering::Relaxed))
    }

    async fn handle_off(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        self.state.handle.set_dehumidifier_active(false).await.map_err(|e| {
            log::warn!("set_dehumidifier_active(false) failed: {e}");
            Error::from(ErrorCode::Failure)
        })?;
        Ok(())
    }

    async fn handle_on(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        self.state.handle.set_dehumidifier_active(true).await.map_err(|e| {
            log::warn!("set_dehumidifier_active(true) failed: {e}");
            Error::from(ErrorCode::Failure)
        })?;
        Ok(())
    }

    async fn handle_toggle(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        let next = !self.state.active.load(Ordering::Relaxed);
        self.state.handle.set_dehumidifier_active(next).await.map_err(|e| {
            log::warn!("set_dehumidifier_active(toggle -> {next}) failed: {e}");
            Error::from(ErrorCode::Failure)
        })?;
        Ok(())
    }
}

/// Implements the read-only `RelativeHumidityMeasurement` cluster for one
/// bridged dehumidifier's current humidity reading.
pub struct ComelitHumidityMeasurementHandler {
    dataver: Dataver,
    state: Arc<DehumidifierMatterState>,
}

impl ComelitHumidityMeasurementHandler {
    pub fn new(dataver: Dataver, state: Arc<DehumidifierMatterState>) -> Self {
        Self { dataver, state }
    }
}

impl humidity_cluster::ClusterAsyncHandler for ComelitHumidityMeasurementHandler {
    const CLUSTER: Cluster<'static> = humidity_cluster::FULL_CLUSTER
        .with_revision(3)
        .with_attrs(with!(required))
        .with_cmds(with!());

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    // No `run` override: this handler's subscription notifications are
    // driven by `ComelitDehumidifierOnOffHandler::run`, which shares the
    // same `Signal` and notifies both cluster IDs (see that handler's doc
    // comment). The trait default (`pending()`) is correct here.

    async fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(self.state.humidity.load(Ordering::Relaxed)))
    }

    async fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(0))
    }

    async fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(10000))
    }
}

/// Receives MQTT push-updates for all bridged thermostats and forwards the
/// ones with a tracked dehumidifier endpoint to that endpoint's
/// `HumidityHandle`. A thermostat push for a device id with no matching
/// state here (a thermostat without the dehumidifier sub-type) is a no-op —
/// `iter().find()` simply finds nothing.
pub struct MultiDehumidifierObserver {
    pub states: Vec<Arc<DehumidifierMatterState>>,
}

#[async_trait]
impl StatusUpdate for MultiDehumidifierObserver {
    async fn status_update(&self, device: &HomeDeviceData) {
        if let HomeDeviceData::Thermostat(data) = device {
            if let Some(state) = self.states.iter().find(|s| s.device_id == data.id) {
                state.handle.mqtt_push(HumidityState::from(data)).await;
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn percent_round_trips_through_hundredths() {
        assert_eq!(matter_to_percent(percent_to_matter(55.5)), 55.5);
        assert_eq!(matter_to_percent(percent_to_matter(0.0)), 0.0);
        assert_eq!(matter_to_percent(percent_to_matter(100.0)), 100.0);
    }

    #[test]
    fn percent_out_of_range_clamps_instead_of_overflowing() {
        // Guards against a garbled/out-of-spec humidity reading (e.g. a
        // parse artifact producing a negative or >100 value) wrapping
        // through the `as u16` cast instead of saturating at the cluster's
        // advertised Min/Max bounds.
        assert_eq!(percent_to_matter(-5.0), 0);
        assert_eq!(percent_to_matter(150.0), 10000);
    }
}
```

- [ ] **Step 3: Run the new tests**

Run: `cargo test -p comelit-hub-matter dehumidifier`
Expected: 2 tests pass.

- [ ] **Step 4: Build the whole crate to confirm the new file compiles cleanly**

Run: `cargo build -p comelit-hub-matter 2>&1 | head -100`
Expected: clean build. Warnings about `DehumidifierMatterState`, `DehumidifierMatterSink`, `ComelitDehumidifierOnOffHandler`, `ComelitHumidityMeasurementHandler`, `MultiDehumidifierObserver` being unused are expected at this point (nothing references them until Task 4/5) — `dead_code` warnings only, no errors.

- [ ] **Step 5: Commit**

```bash
git add matter/src/dehumidifier.rs matter/src/main.rs
git commit -m "Add matter/src/dehumidifier.rs: OnOff + RelativeHumidityMeasurement cluster handlers"
```

---

## Task 4: Extend `matter/src/bridge.rs` with `BridgedEntry::Dehumidifier`

**Files:**
- Modify: `matter/src/bridge.rs`

**Interfaces:**
- Consumes: `crate::dehumidifier::{ComelitDehumidifierOnOffHandler, ComelitHumidityMeasurementHandler}` (Task 3).
- Produces (used by Task 5): `pub struct DehumidifierEntry { pub ep_id: u16, pub on_off: ComelitDehumidifierOnOffHandler, pub humidity: ComelitHumidityMeasurementHandler, pub desc: desc::DescHandler<'static>, pub groups: groups::GroupsHandler, pub bridged: BridgedInfo }`; `BridgedEntry::Dehumidifier(DehumidifierEntry)` variant.

- [ ] **Step 1: Add the dehumidifier import and device-type/cluster statics**

In the `use` block at the top of `matter/src/bridge.rs`, add:

```rust
use rs_matter::dm::clusters::decl::on_off::{self as on_off_cluster, ClusterAsyncHandler as _};
use rs_matter::dm::clusters::decl::relative_humidity_measurement::{
    self as humidity_cluster, ClusterAsyncHandler as _,
};
```

next to the existing `thermostat_cluster`/`covering_cluster` imports, and add to the `use crate::` block:

```rust
use crate::dehumidifier::{ComelitDehumidifierOnOffHandler, ComelitHumidityMeasurementHandler};
```

After the existing `THERMOSTAT_DEVICE_TYPES`/`THERMOSTAT_CLUSTERS` statics, add:

```rust
// On/Off Plug-in Unit (0x010A, revision 2) is the closest standard Matter
// device type to "a controllable on/off appliance with an associated
// sensor reading" — Matter 1.5.1 has no dedicated Dehumidifier device
// type (verified directly against the pinned rs-matter's IDL; see the
// design spec). Declaring the endpoint as a Humidity Sensor instead would
// make most controllers treat it as a read-only sensor and hide the on/off
// control, defeating the point of this endpoint.
const DEV_TYPE_ON_OFF_PLUGIN_UNIT: DeviceType = DeviceType { dtype: 0x010A, drev: 2 };

static DEHUMIDIFIER_DEVICE_TYPES: [DeviceType; 2] = [DEV_TYPE_ON_OFF_PLUGIN_UNIT, DEV_TYPE_BRIDGED_NODE];
static DEHUMIDIFIER_CLUSTERS: [Cluster<'static>; 5] = [
    desc::DescHandler::CLUSTER,
    groups::GroupsHandler::CLUSTER,
    <BridgedInfo as BridgedCH>::CLUSTER,
    ComelitDehumidifierOnOffHandler::CLUSTER,
    ComelitHumidityMeasurementHandler::CLUSTER,
];
```

- [ ] **Step 2: Add `DehumidifierEntry` after `ThermostatEntry`**

```rust
// ── DehumidifierEntry ─────────────────────────────────────────────────────────

/// All handlers and shared state for a single bridged dehumidifier endpoint.
/// This is a separate endpoint from its parent thermostat's `ThermostatEntry`
/// — see the design spec for why (Comelit's dehumidifier runs alongside
/// cooling, not instead of it, so it cannot be folded into `SystemMode`).
pub struct DehumidifierEntry {
    pub ep_id: u16,
    pub on_off: ComelitDehumidifierOnOffHandler,
    pub humidity: ComelitHumidityMeasurementHandler,
    pub desc: desc::DescHandler<'static>,
    pub groups: groups::GroupsHandler,
    pub bridged: BridgedInfo,
}
```

- [ ] **Step 3: Add the `Dehumidifier` variant to `BridgedEntry` and its `ep_id()` arm**

```rust
pub enum BridgedEntry {
    Light(LightEntry),
    WindowCovering(CoveringEntry),
    Thermostat(ThermostatEntry),
    Dehumidifier(DehumidifierEntry),
}

impl BridgedEntry {
    fn ep_id(&self) -> u16 {
        match self {
            BridgedEntry::Light(l) => l.ep_id,
            BridgedEntry::WindowCovering(c) => c.ep_id,
            BridgedEntry::Thermostat(t) => t.ep_id,
            BridgedEntry::Dehumidifier(d) => d.ep_id,
        }
    }
}
```

- [ ] **Step 4: Add the `read` dispatch arm**

Inside `impl AsyncHandler for ComelitBridgeHandler { async fn read(...) { ... match ... } }`, add after the `Some(BridgedEntry::Thermostat(thermostat)) => { ... }` arm:

```rust
            Some(BridgedEntry::Dehumidifier(dehumidifier)) => match cluster_id {
                c if c == desc::DescHandler::CLUSTER.id =>
                    DmAsync(desc::HandlerAdaptor(&dehumidifier.desc)).read(ctx, reply).await,
                c if c == groups::GroupsHandler::CLUSTER.id =>
                    DmAsync(groups::HandlerAdaptor(&dehumidifier.groups)).read(ctx, reply).await,
                c if c == bridged_device_basic_information::FULL_CLUSTER.id =>
                    DmAsync(bridged_device_basic_information::HandlerAdaptor(&dehumidifier.bridged)).read(ctx, reply).await,
                c if c == ComelitDehumidifierOnOffHandler::CLUSTER.id =>
                    on_off_cluster::HandlerAsyncAdaptor(&dehumidifier.on_off).read(ctx, reply).await,
                c if c == ComelitHumidityMeasurementHandler::CLUSTER.id =>
                    humidity_cluster::HandlerAsyncAdaptor(&dehumidifier.humidity).read(ctx, reply).await,
                _ => Err(ErrorCode::ClusterNotFound.into()),
            },
```

- [ ] **Step 5: Add the `write` dispatch arm**

Inside `async fn write(...)`, add after the `Some(BridgedEntry::Thermostat(thermostat)) => { ... }` arm:

```rust
            Some(BridgedEntry::Dehumidifier(dehumidifier)) => match cluster_id {
                c if c == ComelitDehumidifierOnOffHandler::CLUSTER.id =>
                    on_off_cluster::HandlerAsyncAdaptor(&dehumidifier.on_off).write(ctx).await,
                _ => Err(ErrorCode::AttributeNotFound.into()),
            },
```

(`RelativeHumidityMeasurement` has no writable attributes, so no branch is needed for it — any write targeting that cluster id correctly falls through to the `_ => AttributeNotFound` case.)

- [ ] **Step 6: Add the `invoke` dispatch arm**

Inside `async fn invoke(...)`, add after the `Some(BridgedEntry::Thermostat(thermostat)) => { ... }` arm:

```rust
            Some(BridgedEntry::Dehumidifier(dehumidifier)) => match cluster_id {
                c if c == ComelitDehumidifierOnOffHandler::CLUSTER.id =>
                    on_off_cluster::HandlerAsyncAdaptor(&dehumidifier.on_off).invoke(ctx, reply).await,
                _ => Err(ErrorCode::CommandNotFound.into()),
            },
```

- [ ] **Step 7: Add the `bump_dataver` arm**

Inside `fn bump_dataver(...)`, add a new match arm to the `match entry { ... }` block after `BridgedEntry::Thermostat(thermostat) => { ... }`:

```rust
                BridgedEntry::Dehumidifier(dehumidifier) => {
                    if cl.map(|c| c == desc::DescHandler::CLUSTER.id).unwrap_or(true) {
                        DescCH::dataver_changed(&dehumidifier.desc);
                    }
                    if cl.map(|c| c == groups::GroupsHandler::CLUSTER.id).unwrap_or(true) {
                        GroupsCH::dataver_changed(&dehumidifier.groups);
                    }
                    if cl.map(|c| c == bridged_device_basic_information::FULL_CLUSTER.id).unwrap_or(true) {
                        BridgedCH::dataver_changed(&dehumidifier.bridged);
                    }
                    if cl.map(|c| c == ComelitDehumidifierOnOffHandler::CLUSTER.id).unwrap_or(true) {
                        on_off_cluster::HandlerAsyncAdaptor(&dehumidifier.on_off).bump_dataver(&ctx);
                    }
                    if cl.map(|c| c == ComelitHumidityMeasurementHandler::CLUSTER.id).unwrap_or(true) {
                        humidity_cluster::HandlerAsyncAdaptor(&dehumidifier.humidity).bump_dataver(&ctx);
                    }
                }
```

- [ ] **Step 8: Add the `run` future for the on/off handler only**

Inside `async fn run(...)`, add to the `.map(|entry| match entry { ... })` closure, after `BridgedEntry::Thermostat(t) => Box::pin(t.thermostat.run(&ctx)) as DynFut<'_>,`:

```rust
                BridgedEntry::Dehumidifier(d) => Box::pin(d.on_off.run(&ctx)) as DynFut<'_>,
```

(Only `d.on_off.run` is included — per `ComelitDehumidifierOnOffHandler`'s doc comment in Task 3, it notifies both cluster IDs on the shared `Signal`; `d.humidity.run` is left on the trait's `pending()` default and is never polled, since including a permanently-pending future here would add a needless leaf to the balanced select tree.)

- [ ] **Step 9: Add the `BridgeMetadata::new` endpoint-construction arm**

Inside `impl BridgeMetadata { pub fn new(...) { ... for entry in entries { match entry { ... } } } }`, add after the `BridgedEntry::Thermostat(thermostat) => { ... }` arm:

```rust
                BridgedEntry::Dehumidifier(dehumidifier) => {
                    endpoints.push(Endpoint::new(dehumidifier.ep_id, &DEHUMIDIFIER_DEVICE_TYPES, &DEHUMIDIFIER_CLUSTERS));
                }
```

- [ ] **Step 10: Build to confirm all match arms are exhaustive and compile**

Run: `cargo build -p comelit-hub-matter 2>&1 | head -100`
Expected: clean build (adding an enum variant makes every existing `match self.entries.iter().find(...) { ... }` and `match entry { ... }` non-exhaustive until the new arm is added everywhere above — this step is where any missed arm surfaces as a compile error). `DehumidifierEntry`/`BridgedEntry::Dehumidifier` are still unconstructed anywhere (Task 5 does that), so expect `dead_code`/`never constructed` warnings only, no errors.

- [ ] **Step 11: Run the crate's existing tests**

Run: `cargo test -p comelit-hub-matter`
Expected: all existing tests still pass (this task adds no new tests of its own — it's pure plumbing, exercised end-to-end once Task 5 wires construction; `dehumidifier::test` from Task 3 keeps passing).

- [ ] **Step 12: Commit**

```bash
git add matter/src/bridge.rs
git commit -m "Add BridgedEntry::Dehumidifier and its dispatch to ComelitBridgeHandler"
```

---

## Task 5: Wire dehumidifier discovery and construction into `matter/src/main.rs`

**Files:**
- Modify: `matter/src/main.rs`

**Interfaces:**
- Consumes: everything from Tasks 1, 3, and 4.
- Produces: nothing further — this is the final integration point.

- [ ] **Step 1: Add imports**

In the `use` block at the top of `matter/src/main.rs`, extend the `comelit_client_rs` import to include `ObjectSubtype`:

```rust
use comelit_client_rs::{
    ComelitClient, ComelitObserver, ComelitOptionsBuilder, DeviceStatus, HomeDeviceData,
    ObjectSubtype, State, StatusUpdate, get_secrets,
};
```

Extend the `bridge::` import to include `DehumidifierEntry`:

```rust
use bridge::{
    BridgeMetadata, BridgedEntry, BridgedInfo, ComelitBridgeHandler, CoveringEntry,
    DehumidifierEntry, LightEntry, NonRootMatcher, ThermostatEntry,
};
```

Add:

```rust
use dehumidifier::{ComelitDehumidifierOnOffHandler, ComelitHumidityMeasurementHandler, DehumidifierMatterState, DehumidifierMatterSink, MultiDehumidifierObserver};
```

- [ ] **Step 2: Add dehumidifier discovery after the thermostat discovery block**

In `run_bridge`, after the existing block that builds and sorts `thermostat_data` (ending at `thermostat_data.sort_by(|a, b| a.0.cmp(&b.0));`), add:

```rust
    let mut dehumidifier_data: Vec<(String, String, comelit_client_rs::humidity::HumidityState)> = index
        .iter()
        .filter_map(|entry| {
            if let HomeDeviceData::Thermostat(th) = entry.value() {
                if th.sub_type == ObjectSubtype::ClimaThermostatDehumidifier {
                    let label = th.description.clone().unwrap_or_else(|| entry.key().clone());
                    let initial_state = comelit_client_rs::humidity::HumidityState::from(th);
                    return Some((entry.key().clone(), label, initial_state));
                }
            }
            None
        })
        .collect();
    dehumidifier_data.sort_by(|a, b| a.0.cmp(&b.0));
```

- [ ] **Step 3: Extend the discovery-empty check and the logging loop**

Change:

```rust
    if lights_data.is_empty() && covering_data.is_empty() && thermostat_data.is_empty() {
        return Err(anyhow::anyhow!("No lights, window coverings, or thermostats found in Comelit index"));
    }

    info!("Discovered {} lights, {} window coverings, {} thermostats:", lights_data.len(), covering_data.len(), thermostat_data.len());
```

to:

```rust
    if lights_data.is_empty() && covering_data.is_empty() && thermostat_data.is_empty() && dehumidifier_data.is_empty() {
        return Err(anyhow::anyhow!("No lights, window coverings, thermostats, or dehumidifiers found in Comelit index"));
    }

    info!(
        "Discovered {} lights, {} window coverings, {} thermostats, {} dehumidifiers:",
        lights_data.len(), covering_data.len(), thermostat_data.len(), dehumidifier_data.len()
    );
```

After the existing `for (id, label, state) in &thermostat_data { ... next_ep += 1; }` logging loop, add:

```rust
    for (id, label, state) in &dehumidifier_data {
        info!("  ep{}: dehumidifier {} ({}) — active={} humidity={:.1}", next_ep, label, id, state.dehumidifier_active, state.humidity);
        next_ep += 1;
    }
```

- [ ] **Step 4: Build `DehumidifierMatterState`s after the thermostat-states loop**

After the existing block that builds `thermostat_states` (ending at the `ep_id += 1;` inside that `for` loop), add:

```rust
    let mut dehumidifier_states: Vec<Arc<DehumidifierMatterState>> = Vec::new();
    for (id, _, initial_state) in &dehumidifier_data {
        let handle = comelit_client_rs::humidity::spawn_humidity_worker(
            id.clone(),
            *initial_state,
            client.clone(),
        );
        let state = Arc::new(DehumidifierMatterState::new(ep_id, id.clone(), *initial_state, handle));
        state.handle.set_sink(Box::new(DehumidifierMatterSink::new(state.clone()))).await;
        dehumidifier_states.push(state);
        ep_id += 1;
    }
```

- [ ] **Step 5: Extend `FanOutObserver` with the dehumidifier observer**

Change:

```rust
    let light_observer = Arc::new(MultiLightObserver { states: light_states.clone() });
    let covering_observer = Arc::new(covering::MultiCoveringObserver { states: covering_states.clone() });
    let thermostat_observer = Arc::new(thermostat::MultiThermostatObserver { states: thermostat_states.clone() });

    struct FanOutObserver {
        light: Arc<MultiLightObserver>,
        covering: Arc<covering::MultiCoveringObserver>,
        thermostat: Arc<thermostat::MultiThermostatObserver>,
    }

    #[async_trait]
    impl StatusUpdate for FanOutObserver {
        async fn status_update(&self, device: &HomeDeviceData) {
            self.light.status_update(device).await;
            self.covering.status_update(device).await;
            self.thermostat.status_update(device).await;
        }
    }

    *deferred_slot.write().await = Some(Arc::new(FanOutObserver {
        light: light_observer,
        covering: covering_observer,
        thermostat: thermostat_observer,
    }) as _);
```

to:

```rust
    let light_observer = Arc::new(MultiLightObserver { states: light_states.clone() });
    let covering_observer = Arc::new(covering::MultiCoveringObserver { states: covering_states.clone() });
    let thermostat_observer = Arc::new(thermostat::MultiThermostatObserver { states: thermostat_states.clone() });
    let dehumidifier_observer = Arc::new(MultiDehumidifierObserver { states: dehumidifier_states.clone() });

    struct FanOutObserver {
        light: Arc<MultiLightObserver>,
        covering: Arc<covering::MultiCoveringObserver>,
        thermostat: Arc<thermostat::MultiThermostatObserver>,
        dehumidifier: Arc<MultiDehumidifierObserver>,
    }

    #[async_trait]
    impl StatusUpdate for FanOutObserver {
        async fn status_update(&self, device: &HomeDeviceData) {
            self.light.status_update(device).await;
            self.covering.status_update(device).await;
            self.thermostat.status_update(device).await;
            self.dehumidifier.status_update(device).await;
        }
    }

    *deferred_slot.write().await = Some(Arc::new(FanOutObserver {
        light: light_observer,
        covering: covering_observer,
        thermostat: thermostat_observer,
        dehumidifier: dehumidifier_observer,
    }) as _);
```

- [ ] **Step 6: Do NOT add a second MQTT subscribe pass for dehumidifiers**

No change needed at the `// ── 5. Subscribe to MQTT push for every discovered device ─────────────────` block: `dehumidifier_data`'s ids are identical to entries already present in `thermostat_data` (the same physical thermostat device, observed by two Matter endpoints), and that device id is already subscribed by the existing `for (id, _, _) in &thermostat_data { client.subscribe(id).await?; }` loop. Subscribing again to the same id would be redundant. Confirm this by inspection — no code change here.

- [ ] **Step 7: Pass the new state/data through to `run_matter`**

Change the `run_matter` call:

```rust
    let matter_thread = std::thread::Builder::new()
        .name("matter".into())
        .stack_size(600 * 1024)
        .spawn(move || run_matter(
            light_states, lights_data,
            covering_states, covering_data,
            thermostat_states, thermostat_data,
        ))?;
```

to:

```rust
    let matter_thread = std::thread::Builder::new()
        .name("matter".into())
        .stack_size(600 * 1024)
        .spawn(move || run_matter(
            light_states, lights_data,
            covering_states, covering_data,
            thermostat_states, thermostat_data,
            dehumidifier_states, dehumidifier_data,
        ))?;
```

and extend `run_matter`'s signature:

```rust
fn run_matter(
    light_states: Vec<Arc<LightState>>,
    lights_data: Vec<(String, String, bool)>,
    covering_states: Vec<Arc<covering::CoveringState>>,
    covering_data: Vec<(String, String, comelit_client_rs::covering::WindowCoveringState)>,
    thermostat_states: Vec<Arc<thermostat::ThermostatMatterState>>,
    thermostat_data: Vec<(String, String, comelit_client_rs::thermostat::ThermostatState)>,
    dehumidifier_states: Vec<Arc<DehumidifierMatterState>>,
    dehumidifier_data: Vec<(String, String, comelit_client_rs::humidity::HumidityState)>,
) -> anyhow::Result<()> {
```

- [ ] **Step 8: Build `BridgedEntry::Dehumidifier` per discovered dehumidifier**

Inside `run_matter`, after the existing loop that builds `BridgedEntry::Thermostat` entries (ending at its closing `}));` and blank line before `let agg_desc = ...`), add:

```rust
    for (state, (device_id, label, _)) in dehumidifier_states.into_iter().zip(dehumidifier_data.iter()) {
        let ep_id = state.ep_id;
        entries.push(BridgedEntry::Dehumidifier(DehumidifierEntry {
            ep_id,
            on_off: ComelitDehumidifierOnOffHandler::new(Dataver::new_rand(&mut rand), state.clone()),
            humidity: ComelitHumidityMeasurementHandler::new(Dataver::new_rand(&mut rand), state),
            desc: desc::DescHandler::new(Dataver::new_rand(&mut rand)),
            groups: groups::GroupsHandler::new(Dataver::new_rand(&mut rand)),
            bridged: BridgedInfo::new(Dataver::new_rand(&mut rand), label.clone(), device_id.clone()),
        }));
    }
```

- [ ] **Step 9: Build the whole workspace**

Run: `cargo build --workspace 2>&1 | tail -100`
Expected: clean build, no errors. Any leftover `dead_code` warnings from Tasks 3/4 about `DehumidifierEntry`/`BridgedEntry::Dehumidifier`/`ComelitDehumidifierOnOffHandler`/`ComelitHumidityMeasurementHandler` never being constructed should now be gone, since this task constructs and uses all of them.

- [ ] **Step 10: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all tests across `comelit-client-rs`, `comelit-hub-hap`, and `comelit-hub-matter` pass.

- [ ] **Step 11: Manual verification against the real hub (optional but recommended)**

Run the Matter bridge binary against the real Comelit hub (same invocation pattern as previous Matter feature work — see `matter/src/main.rs`'s `Args` for `--host`/`--user`/`--password`/`--settings`), confirm in the logs that a dehumidifier-equipped thermostat now produces two log lines (`ep{N}: thermostat ...` and `ep{N+1}: dehumidifier ...`), and — if a Matter controller is available for pairing — confirm the new endpoint shows up as an on/off switch with a readable humidity value, and that toggling it actually calls through to the hub (check hub-side logs or physical dehumidifier state).

- [ ] **Step 12: Commit**

```bash
git add matter/src/main.rs
git commit -m "Wire dehumidifier discovery, worker spawn, and endpoint construction into the Matter bridge"
```

---

## Self-Review Notes

- **Spec coverage:** Task 1 covers the spec's "Shared `client/src/humidity/` module" section in full (state, worker, handle, sink trait). Task 2 covers "HAP migration". Tasks 3-5 cover "Matter: new `matter/src/dehumidifier.rs`", "`BridgedEntry` and dispatch", and "Discovery and wiring" respectively. The spec's "Non-Goals" (no writable setpoint, no `SystemMode::Dry`, no standalone `ClimaDehumidifier`, no persistence) are honored by omission — no task adds any of them. The spec's "Error Handling" section is covered by the oneshot-reply pattern in Task 1 and the `ErrorCode::Failure` mapping in Task 3.
- **Deviation from the spec's `ComelitOnOffHooks` reference:** the spec's Matter section says the on/off cluster is "ricalcato da `ComelitOnOffHooks`", but per the design-doc note this plan implements `ClusterAsyncHandler` directly instead (already called out in the spec's Matter section as a deliberate deviation, consistent with the fire-and-forget-removal work). Task 3 carries the same rationale in its doc comment.
- **Device type choice:** the spec didn't pin an exact Matter device type for the endpoint. Task 4 chooses On/Off Plug-in Unit (0x010A rev 2) over Humidity Sensor (0x0307) so controllers expose the on/off control rather than treating the endpoint as a read-only sensor — documented inline in Task 4 Step 1.
- **Type/interface consistency check:** `HumidityHandle::set_dehumidifier_active` takes `bool` throughout (Task 1's worker, Task 2's HAP callback converting `u8 == 1` to `bool` at the boundary, Task 3's Matter handler passing `bool` directly) — no `u8`/`bool` mismatch between tasks. `HumidityState` field names (`humidity`, `target_humidity`, `dehumidifier_active`, `dehumidifier_current_state`) are identical across Tasks 1-3. `DehumidifierMatterState::new`'s signature matches its two call sites in Task 5 Step 8. `ComelitDehumidifierOnOffHandler::CLUSTER`/`ComelitHumidityMeasurementHandler::CLUSTER` names match between Task 3 (definition) and Task 4 (dispatch/statics).
- **Placeholder scan:** no TBD/TODO; every step has complete code, not a description of code.
