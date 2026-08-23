# Matter Thermostat Bridge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the Matter bridge (`matter/`) to expose Comelit thermostats as bridged `Thermostat` endpoints, alongside the existing lights and window coverings, reusing the temperature/HVAC-mode reduction logic already validated in the HomeKit bridge.

**Architecture:** Extract the thermostat's temperature/HVAC-mode worker and state reduction (currently private to `hap/`, cabled to concrete `ComelitClient`) into a new shared `client::thermostat` module generic over `ComelitClientTrait` — mirroring the `client::covering` extraction. Humidity/dehumidifier support stays entirely local to `hap/` (out of scope — Matter has no direct cluster for active dehumidifier control). Build a Matter-side `ClusterAsyncHandler` for the `Thermostat` cluster and extend `BridgedEntry` with a third variant.

**Tech Stack:** Rust, `rs-matter` (git rev `e8b0b0cbb20bf312a9c52fc1ee56541037a3b9c9`), `tokio`, `async-trait`, existing `comelit-client-rs` / `comelit-hub-hap` / `comelit-hub-matter` crates.

**Spec:** `docs/superpowers/specs/2026-08-23-matter-thermostat-design.md`

## Global Constraints

- No new external crate dependencies.
- `client/` stays protocol-agnostic: no `hap::*` or `rs_matter::*` imports.
- `ThermostatHandle` is `Clone` and must **not** have a `Drop` impl that sends a shutdown command — the final whole-branch review on the window-covering work found that pattern kills the worker the moment any single clone is dropped, since all clones share one channel. The worker must terminate naturally when the channel closes (last `Sender` clone dropped, `rx.recv()` returns `None`).
- `ComelitCoveringHandler`'s equivalent lesson: the Matter thermostat cluster handler **must** override `ClusterAsyncHandler::run` to drain its `Signal` and call `ctx.notify_cluster_changed(...)` — leaving it at the trait default `pending()` was flagged as an Important gap for window coverings and fixed there; do it correctly the first time here.
- Humidity/target-humidity/dehumidifier fields stay OUT of the shared `ThermostatState` — only `temperature`, `target_temperature`, `heating_cooling_state`, `target_heating_cooling_state` are shared. `hap/` keeps its own local state for the rest.
- The `SetHvacMode` sequence (including the `Auto` case, which Matter never sends but HAP does) must be preserved byte-for-byte from the existing HAP logic at `hap/src/accessories/thermostat.rs:220-299` when moved into the shared worker — this is a mechanical move, not a rewrite.
- Every build/test command in this plan must use `--workspace` or an explicit `-p <package>` — this repo's root `Cargo.toml` is a real package, not a virtual workspace, so bare `cargo build`/`cargo test` silently skip every member crate.

---

## Task 1: `client::thermostat::state` — shared state and reduction logic

**Files:**
- Create: `client/src/thermostat/state.rs`

**Interfaces:**
- Produces: `pub struct ThermostatState { pub temperature: f32, pub target_temperature: f32, pub heating_cooling_state: TargetHeatingCoolingState, pub target_heating_cooling_state: TargetHeatingCoolingState }` (derives `Debug, Clone, Copy, Default`), `pub enum TargetHeatingCoolingState { Off = 0, Heat = 1, Cool = 2, Auto = 3 }` (derives `Debug, Clone, Copy, Eq, PartialEq, Default`, `#[repr(u8)]`, `Off` is `#[default]`), `impl From<u8> for TargetHeatingCoolingState`, `impl From<TargetHeatingCoolingState> for u8`, `impl From<&ThermostatDeviceData> for ThermostatState`.

- [ ] **Step 1: Create the file**

```rust
// client/src/thermostat/state.rs
use crate::protocol::out_data_messages::{ClimaMode, ThermoSeason, ThermostatDeviceData};

#[derive(Debug, Clone, Copy, Default)]
pub struct ThermostatState {
    pub temperature: f32,
    pub target_temperature: f32,
    pub heating_cooling_state: TargetHeatingCoolingState,
    pub target_heating_cooling_state: TargetHeatingCoolingState,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
#[repr(u8)]
pub enum TargetHeatingCoolingState {
    #[default]
    Off = 0,
    Heat = 1,
    Cool = 2,
    Auto = 3,
}

impl From<u8> for TargetHeatingCoolingState {
    fn from(value: u8) -> Self {
        match value {
            0 => TargetHeatingCoolingState::Off,
            1 => TargetHeatingCoolingState::Heat,
            2 => TargetHeatingCoolingState::Cool,
            3 => TargetHeatingCoolingState::Auto,
            _ => panic!("Invalid value for TargetHeatingCoolingState"),
        }
    }
}

impl From<TargetHeatingCoolingState> for u8 {
    fn from(value: TargetHeatingCoolingState) -> Self {
        value as u8
    }
}

impl From<&ThermostatDeviceData> for ThermostatState {
    fn from(data: &ThermostatDeviceData) -> Self {
        let temperature = data
            .temperature
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap_or_default()
            / 10.0;

        let target_temperature = data
            .active_threshold
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap_or_default()
            / 10.0;

        let auto_man = data.auto_man.clone().unwrap_or_default();
        let is_off = auto_man == ClimaMode::OffAuto || auto_man == ClimaMode::OffManual;
        let is_auto = auto_man == ClimaMode::Auto;
        let is_winter = data.season.clone().unwrap_or_default() == ThermoSeason::Winter;

        let heating_cooling_state = if is_off {
            TargetHeatingCoolingState::Off
        } else if is_winter {
            TargetHeatingCoolingState::Heat
        } else if is_auto {
            TargetHeatingCoolingState::Auto
        } else {
            TargetHeatingCoolingState::Cool
        };

        Self {
            temperature,
            target_temperature,
            heating_cooling_state,
            target_heating_cooling_state: heating_cooling_state,
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
            sub_type: ObjectSubtype::ClimaThermostat,
            status: None,
            description: None,
            temperature: Some("215".to_string()),
            auto_man: Some(ClimaMode::Manual),
            season: Some(ThermoSeason::Winter),
            active_threshold: Some("220".to_string()),
            humidity: None,
            humi_active_threshold: None,
            auto_man_umi: None,
        }
    }

    #[test]
    fn test_winter_manual_is_heat() {
        let data = base_data();
        let state = ThermostatState::from(&data);
        assert_eq!(state.temperature, 21.5);
        assert_eq!(state.target_temperature, 22.0);
        assert_eq!(state.heating_cooling_state, TargetHeatingCoolingState::Heat);
        assert_eq!(state.target_heating_cooling_state, TargetHeatingCoolingState::Heat);
    }

    #[test]
    fn test_summer_manual_is_cool() {
        let mut data = base_data();
        data.season = Some(ThermoSeason::Summer);
        let state = ThermostatState::from(&data);
        assert_eq!(state.heating_cooling_state, TargetHeatingCoolingState::Cool);
    }

    #[test]
    fn test_off_auto_is_off_regardless_of_season() {
        let mut data = base_data();
        data.auto_man = Some(ClimaMode::OffAuto);
        data.season = Some(ThermoSeason::Winter);
        let state = ThermostatState::from(&data);
        assert_eq!(state.heating_cooling_state, TargetHeatingCoolingState::Off);
    }

    #[test]
    fn test_summer_auto_is_auto() {
        let mut data = base_data();
        data.season = Some(ThermoSeason::Summer);
        data.auto_man = Some(ClimaMode::Auto);
        let state = ThermostatState::from(&data);
        assert_eq!(state.heating_cooling_state, TargetHeatingCoolingState::Auto);
    }

    #[test]
    fn test_winter_auto_is_still_heat() {
        // Matches existing HAP behavior: winter takes priority over the
        // auto_man==Auto check, so winter+Auto reduces to Heat, not Auto.
        let mut data = base_data();
        data.season = Some(ThermoSeason::Winter);
        data.auto_man = Some(ClimaMode::Auto);
        let state = ThermostatState::from(&data);
        assert_eq!(state.heating_cooling_state, TargetHeatingCoolingState::Heat);
    }

    #[test]
    fn test_unparseable_values_default_to_zero() {
        let mut data = base_data();
        data.temperature = Some("not-a-number".to_string());
        data.active_threshold = None;
        let state = ThermostatState::from(&data);
        assert_eq!(state.temperature, 0.0);
        assert_eq!(state.target_temperature, 0.0);
    }
}
```

Verify the exact field names/types of `ThermostatDeviceData` (`client/src/protocol/out_data_messages.rs:639-658`) and `ObjectType`/`ObjectSubtype` variant names used in the test fixture (`ObjectType::Thermostat`, `ObjectSubtype::ClimaThermostat`) against the actual enum definitions in the same file — this plan was drafted from reading that file directly, but confirm the exact variant names before writing the test, since a typo there is a compile error, not a logic bug.

- [ ] **Step 2: Visual review, no standalone rustc check**

Same ruling as the window-covering plan: skip any standalone `rustc`/isolated compile check here — this file isn't wired into the crate tree yet (Task 3 does that). Do a careful visual review for syntax correctness instead. The real compile gate is Task 3's `cargo build -p comelit-client-rs`.

- [ ] **Step 3: Commit**

```bash
git add client/src/thermostat/state.rs
git commit -m "Add client::thermostat::state module (moved from hap thermostat state, reduced to thermal fields only)"
```

---

## Task 2: `client::thermostat::worker` — shared worker, generic over `ComelitClientTrait`

**Files:**
- Create: `client/src/thermostat/worker.rs`

**Interfaces:**
- Consumes: `ThermostatState`, `TargetHeatingCoolingState` from `super::state` (Task 1); `ComelitClientTrait` from `crate::protocol::client`; `ClimaMode`, `ClimaOnOff`, `ThermoSeason` from `crate::protocol::out_data_messages`.
- Produces: `pub trait ThermostatSink: Send + Sync + 'static { async fn update(&self, state: ThermostatState); }` (via `#[async_trait]`, dyn-compatible), `pub struct ThermostatHandle` (`#[derive(Clone)]`, **no `Drop` impl**) with `pub async fn set_target_temperature(&self, celsius: f32)`, `pub async fn set_hvac_mode(&self, mode: TargetHeatingCoolingState)`, `pub async fn mqtt_push(&self, state: ThermostatState)`, `pub async fn set_sink(&self, sink: Box<dyn ThermostatSink>)`; `pub fn spawn_thermostat_worker<C: ComelitClientTrait + 'static>(id: String, initial: ThermostatState, client: C) -> ThermostatHandle`.

This task ports the temperature/HVAC-mode command handling from
`hap/src/accessories/thermostat.rs:175-350` (the `SetTargetTemperature`,
`SetHvacMode`, and `MqttPush` arms of `ThermostatWorker::handle` — **not**
`SetTargetHumidity`, `SetDehumidifierActive`, or `SetDehumidifierThreshold`,
which stay in `hap/` per the design spec's scope decision), generalized
from concrete `ComelitClient` to `C: ComelitClientTrait`, and from a raw
`Option<Accessory>` field to the `ThermostatSink` trait (same pattern as
`WindowCoveringSink`).

- [ ] **Step 1: Create the file**

```rust
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
```

Verify the exact import paths (`crate::protocol::client::{ComelitClientError, ComelitClientTrait, State}`, `crate::protocol::out_data_messages::{ActionType, HomeDeviceData}`, `crate::protocol::scanner::MacAddress`) against the real crate structure — same caveat as the window-covering plan, drafted from direct reads but confirm before compiling. Also verify `ComelitClientError::Generic(String)` is the right variant/constructor (mirrors the pattern already used in `client/src/covering/worker.rs`'s test module from the previous plan — check that file directly for the exact form, since it already compiles).

- [ ] **Step 2: Visual review, no standalone rustc check**

Same ruling as Task 1 and the window-covering plan.

- [ ] **Step 3: Commit**

```bash
git add client/src/thermostat/worker.rs
git commit -m "Add client::thermostat::worker module with ThermostatSink (no Drop-sends-shutdown)"
```

---

## Task 3: Wire `client::thermostat` into the crate and run its tests

**Files:**
- Create: `client/src/thermostat/mod.rs`
- Modify: `client/src/lib.rs`

**Interfaces:**
- Produces: `pub mod thermostat;` accessible as `comelit_client_rs::thermostat::{ThermostatState, TargetHeatingCoolingState, ThermostatSink, ThermostatHandle, spawn_thermostat_worker}`.

- [ ] **Step 1: Create `client/src/thermostat/mod.rs`**

```rust
// client/src/thermostat/mod.rs
mod state;
mod worker;

pub use state::{TargetHeatingCoolingState, ThermostatState};
pub use worker::{ThermostatHandle, ThermostatSink, spawn_thermostat_worker};
```

- [ ] **Step 2: Add the module to `client/src/lib.rs`**

Modify `client/src/lib.rs` — add `pub mod thermostat;` next to the existing `pub mod covering;`:

```rust
mod protocol;
pub mod covering;
pub mod thermostat;

pub use protocol::client::*;
pub use protocol::credentials::get_secrets;
pub use protocol::out_data_messages::*;
pub use protocol::scanner::{MacAddress, Scanner};
```

- [ ] **Step 3: Build and run the new tests**

Run: `cargo build -p comelit-client-rs 2>&1 | tail -60`
Expected: builds cleanly. Fix any import-path mismatches surfaced now (Tasks 1-2 were drafted from direct file reads but never compiled together).

Run: `cargo test -p comelit-client-rs thermostat:: -- --nocapture`
Expected: all tests from Task 1 (`test_winter_manual_is_heat`, `test_summer_manual_is_cool`, `test_off_auto_is_off_regardless_of_season`, `test_summer_auto_is_auto`, `test_winter_auto_is_still_heat`, `test_unparseable_values_default_to_zero`) and Task 2 (`test_set_target_temperature_echoes_immediately`, `test_set_target_temperature_failure_does_not_echo`, `test_set_hvac_mode_heat_sets_season_winter_and_manual`, `test_set_hvac_mode_off_sends_off_toggle_only`, `test_set_hvac_mode_auto_sends_auto_mode`, `test_set_hvac_mode_from_auto_to_heat_sends_manual_then_season`, `test_mqtt_push_replaces_state_and_notifies`, `test_worker_survives_dropped_handle_clone`) — all PASS.

- [ ] **Step 4: Commit**

```bash
git add client/src/thermostat/mod.rs client/src/lib.rs
git commit -m "Wire client::thermostat module into comelit-client-rs"
```

---

## Task 4: Migrate `hap/` thermostat to the shared `client::thermostat` module

**Files:**
- Modify: `hap/src/accessories/thermostat.rs` (thermal path rewritten to use the shared module; humidity/dehumidifier path stays local, unchanged in behavior)
- Modify: `hap/src/accessories/state/thermostat.rs` (reduced — see Step 1)

**Interfaces:**
- Consumes: `comelit_client_rs::thermostat::{ThermostatState, TargetHeatingCoolingState, ThermostatSink, ThermostatHandle, spawn_thermostat_worker}`.
- Produces: `pub(crate) struct ComelitThermostatAccessory` (same public shape as before — `ComelitAccessory<ThermostatDeviceData>` impl unchanged from the outside; `hap/src/bridge.rs`'s call site at `hap/src/bridge.rs:445` needs no change).

This is the most delicate task in this plan: the existing HAP `ThermostatWorker` handles BOTH thermal state (now shared) AND humidity/dehumidifier state (stays local, out of scope for Matter). After this task, `ComelitThermostatAccessory` owns two things side by side: a `ThermostatHandle` (shared, for `SetTargetTemperature`/`SetHvacMode`/`MqttPush`-thermal-part) and a small HAP-local humidity/dehumidifier worker (unchanged logic, still cabled to concrete `ComelitClient`, still using its own `Option<Accessory>` to write the `HumidifierDehumidifier` service characteristics and the `Thermostat` service's `current_relative_humidity`/`target_relative_humidity` characteristics). Do not try to unify these two into one worker — the design spec explicitly keeps them separate.

- [ ] **Step 1: Reduce `hap/src/accessories/state/thermostat.rs` to the humidity/dehumidifier-only local state**

The thermal fields (`temperature`, `target_temperature`, `heating_cooling_state`, `target_heating_cooling_state`) and `TargetHeatingCoolingState` itself now live in `comelit_client_rs::thermostat`. Replace the file's content:

```rust
// hap/src/accessories/state/thermostat.rs
use comelit_client_rs::{ClimaMode, DeviceStatus, ThermostatDeviceData};

/// Local, HAP-only humidity/dehumidifier state — out of scope for the Matter
/// bridge (Matter has no direct cluster for active dehumidifier control).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HumidityState {
    pub(crate) humidity: f32,
    pub(crate) target_humidity: f32,
    pub(crate) dehumidifier_active: bool,
    pub(crate) dehumidifier_current_state: u8, // 0=INACTIVE, 1=IDLE, 3=DEHUMIDIFYING
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
```

Note this drops the `test_decode`-style test that may or may not have existed for the old `ThermostatState::from` — check `hap/src/accessories/state/thermostat.rs` (pre-change, via `git show HEAD:hap/src/accessories/state/thermostat.rs`) for any existing `#[cfg(test)]` module before deleting content; if one exists testing the now-removed thermal reduction, that coverage already moved to Task 1's tests in `client/src/thermostat/state.rs` — do not duplicate it here. If a test existed for humidity/dehumidifier fields specifically, port it into this file's own `#[cfg(test)]` module using the same fixture pattern.

- [ ] **Step 2: Rewrite `hap/src/accessories/thermostat.rs`**

Replace the `ThermostatCommand`/`ThermostatWorker` section (lines 128-417 of the
original file) and the relevant parts of `ComelitThermostatAccessory` with:

```rust
// hap/src/accessories/thermostat.rs — full replacement
use std::sync::Arc;

use anyhow::{Context, Result};

use async_trait::async_trait;
use futures::FutureExt;
use hap::characteristic::HapCharacteristic;
use hap::pointer::Accessory;
use hap::server::Server;
use hap::{
    HapType,
    accessory::HapAccessory,
    characteristic::AsyncCharacteristicCallbacks,
    server::IpServer,
    service::{
        HapService, accessory_information::AccessoryInformationService,
        humidifier_dehumidifier::HumidifierDehumidifierService, thermostat::ThermostatService,
    },
};
use serde::{
    Serialize,
    ser::{SerializeStruct, Serializer},
};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, Sender};
use tracing::{debug, warn};

use crate::accessories::{ComelitAccessory, state::thermostat::HumidityState};
use crate::web::metrics::Metrics;
use comelit_client_rs::thermostat::{
    TargetHeatingCoolingState, ThermostatHandle, ThermostatSink, ThermostatState,
    spawn_thermostat_worker,
};
use comelit_client_rs::{ComelitClient, ObjectSubtype, ThermostatDeviceData};

#[derive(Debug)]
struct ComelitThermostat {
    id: u64,
    pub accessory_information: AccessoryInformationService,
    pub thermostat: ThermostatService,
    pub humidifier_dehumidifier: Option<HumidifierDehumidifierService>,
}

impl HapAccessory for ComelitThermostat {
    fn get_id(&self) -> u64 {
        self.id
    }

    fn set_id(&mut self, id: u64) {
        self.id = id;
    }

    fn get_service(&self, hap_type: HapType) -> Option<&dyn HapService> {
        self.get_services().into_iter().find(|&s| s.get_type() == hap_type).map(|v| v as _)
    }

    fn get_mut_service(&mut self, hap_type: HapType) -> Option<&mut dyn HapService> {
        self.get_mut_services().into_iter().find(|s| s.get_type() == hap_type).map(|v| v as _)
    }

    fn get_services(&self) -> Vec<&dyn HapService> {
        let mut v: Vec<&dyn HapService> = vec![&self.accessory_information, &self.thermostat];
        if let Some(ref hd) = self.humidifier_dehumidifier {
            v.push(hd);
        }
        v
    }

    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        let mut v: Vec<&mut dyn HapService> = vec![&mut self.accessory_information, &mut self.thermostat];
        if let Some(ref mut hd) = self.humidifier_dehumidifier {
            v.push(hd);
        }
        v
    }
}

impl Serialize for ComelitThermostat {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("HapAccessory", 2)?;
        state.serialize_field("aid", &self.get_id())?;
        state.serialize_field("services", &self.get_services())?;
        state.end()
    }
}

impl ComelitThermostat {
    pub async fn new(id: u64, name: &str, device_id: &str, has_dehumidifier: bool) -> Result<Self> {
        let information = hap::accessory::AccessoryInformation {
            name: name.to_string(),
            manufacturer: "Comelit".to_string(),
            serial_number: device_id.to_string(),
            ..Default::default()
        };
        let accessory_information = information.to_service(1, id)?;
        let info_len = accessory_information.get_characteristics().len() as u64;

        let mut thermostat = ThermostatService::new(1 + info_len + 1, id);
        thermostat.set_primary(true);

        let humidifier_dehumidifier = if has_dehumidifier {
            let offset = 1 + info_len + 1 + thermostat.get_characteristics().len() as u64 + 1;
            Some(HumidifierDehumidifierService::new(offset, id))
        } else {
            None
        };

        Ok(Self { id, accessory_information, thermostat, humidifier_dehumidifier })
    }
}

/// Writes thermal state updates into the HomeKit `Thermostat` service's
/// characteristics. Humidity/dehumidifier characteristics are NOT written
/// here — they're written directly by `ComelitThermostatAccessory::update`,
/// since that state never passes through the shared worker.
struct HapThermostatSink {
    accessory: Accessory,
}

#[async_trait]
impl ThermostatSink for HapThermostatSink {
    async fn update(&self, state: ThermostatState) {
        let mut acc = self.accessory.lock().await;
        let Some(thermostat_service) = acc.get_mut_service(HapType::Thermostat) else {
            warn!("Thermostat service not found while updating characteristics");
            return;
        };

        if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::CurrentTemperature) {
            if let Err(e) = ch.update_value(Value::from(state.temperature)).await {
                warn!("Failed to update CurrentTemperature: {e}");
            }
        }
        if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::TargetTemperature) {
            if let Err(e) = ch.update_value(Value::from(state.target_temperature)).await {
                warn!("Failed to update TargetTemperature: {e}");
            }
        }
        if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::CurrentHeatingCoolingState) {
            if let Err(e) = ch.update_value(Value::from(u8::from(state.heating_cooling_state))).await {
                warn!("Failed to update CurrentHeatingCoolingState: {e}");
            }
        }
        if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::TargetHeatingCoolingState) {
            if let Err(e) = ch.update_value(Value::from(u8::from(state.target_heating_cooling_state))).await {
                warn!("Failed to update TargetHeatingCoolingState: {e}");
            }
        }
    }
}

/// Commands the local (HAP-only) humidity/dehumidifier worker handles —
/// none of this crosses into the shared `client::thermostat` module.
#[derive(Debug)]
enum HumidityCommand {
    SetTargetHumidity(f32),
    SetDehumidifierActive(u8),
    SetDehumidifierThreshold(f32),
    MqttPush(HumidityState),
    SetAccessory(Accessory),
}

struct HumidityWorker {
    id: String,
    state: Arc<Mutex<HumidityState>>,
    client: ComelitClient,
    accessory: Option<Accessory>,
}

impl HumidityWorker {
    fn new(id: String, state: Arc<Mutex<HumidityState>>, client: ComelitClient) -> Self {
        Self { id, state, client, accessory: None }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<HumidityCommand>) {
        while let Some(cmd) = rx.recv().await {
            if let Err(e) = self.handle(cmd).await {
                warn!("HumidityWorker {}: {e}", self.id);
            }
        }
    }

    async fn handle(&mut self, cmd: HumidityCommand) -> Result<()> {
        match cmd {
            HumidityCommand::SetAccessory(acc) => {
                self.accessory = Some(acc);
            }
            HumidityCommand::SetTargetHumidity(humidity) => {
                match self.client.set_humidity(&self.id, humidity as i32).await {
                    Ok(()) => {
                        let state = {
                            let mut guard = self.state.lock().await;
                            guard.target_humidity = humidity;
                            *guard
                        };
                        self.update_accessory(&state).await?;
                    }
                    Err(e) => warn!("set_humidity failed: {e}"),
                }
            }
            HumidityCommand::SetDehumidifierActive(new) => {
                debug!("Dehumidifier active updated to {}", new);
                match self
                    .client
                    .toggle_thermostat_status(
                        &self.id,
                        if new == 1 { comelit_client_rs::ClimaOnOff::OnHumi } else { comelit_client_rs::ClimaOnOff::OffHumi },
                    )
                    .await
                {
                    Ok(()) => {
                        let active = new == 1;
                        let state = {
                            let mut guard = self.state.lock().await;
                            guard.dehumidifier_active = active;
                            guard.dehumidifier_current_state = if active { 1 } else { 0 };
                            *guard
                        };
                        self.update_accessory(&state).await?;
                    }
                    Err(e) => warn!("toggle_thermostat_status (humi) failed: {e}"),
                }
            }
            HumidityCommand::SetDehumidifierThreshold(humidity) => {
                match self.client.set_humidity(&self.id, humidity as i32).await {
                    Ok(()) => {
                        let state = {
                            let mut guard = self.state.lock().await;
                            guard.target_humidity = humidity;
                            *guard
                        };
                        self.update_accessory(&state).await?;
                    }
                    Err(e) => warn!("set_humidity (threshold) failed: {e}"),
                }
            }
            HumidityCommand::MqttPush(new_state) => {
                *self.state.lock().await = new_state;
                self.update_accessory(&new_state).await?;
            }
        }
        Ok(())
    }

    async fn update_accessory(&self, state: &HumidityState) -> Result<()> {
        let Some(ref accessory) = self.accessory else { return Ok(()) };
        let mut acc = accessory.lock().await;

        if let Some(thermostat_service) = acc.get_mut_service(HapType::Thermostat) {
            if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::CurrentRelativeHumidity) {
                ch.update_value(Value::from(state.humidity)).await?;
            }
            if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::TargetRelativeHumidity) {
                ch.update_value(Value::from(state.target_humidity)).await?;
            }
        }

        if let Some(hd_service) = acc.get_mut_service(HapType::HumidifierDehumidifier) {
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::Active) {
                ch.update_value(Value::from(state.dehumidifier_active as u8)).await?;
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::CurrentHumidifierDehumidifierState) {
                ch.update_value(Value::from(state.dehumidifier_current_state)).await?;
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::CurrentRelativeHumidity) {
                ch.update_value(Value::from(state.humidity)).await?;
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::RelativeHumidityDehumidifierThreshold) {
                ch.update_value(Value::from(state.target_humidity)).await?;
            }
        }

        Ok(())
    }
}

pub(crate) struct ComelitThermostatAccessory {
    id: String,
    pub name: String,
    thermostat_handle: ThermostatHandle,
    humidity_sender: Sender<HumidityCommand>,
    #[allow(dead_code)]
    accessory: Accessory,
}

impl ComelitAccessory<ThermostatDeviceData> for ComelitThermostatAccessory {
    fn get_comelit_id(&self) -> &str {
        self.id.as_str()
    }

    async fn update(&mut self, thermostat_data: &ThermostatDeviceData) -> Result<()> {
        self.thermostat_handle.mqtt_push(ThermostatState::from(thermostat_data)).await;
        self.humidity_sender
            .send(HumidityCommand::MqttPush(HumidityState::from(thermostat_data)))
            .await
            .ok();
        Ok(())
    }
}

impl ComelitThermostatAccessory {
    pub async fn new(
        id: u64,
        data: &ThermostatDeviceData,
        client: ComelitClient,
        server: &IpServer,
    ) -> Result<Self> {
        let name = data.description.clone().unwrap_or(data.id.clone());
        let comelit_id = data.id.clone();
        let has_dehumidifier = data.sub_type == ObjectSubtype::ClimaThermostatDehumidifier;
        let mut accessory = ComelitThermostat::new(id, name.as_str(), comelit_id.as_str(), has_dehumidifier).await?;

        let thermal_state = ThermostatState::from(data);
        let humidity_state = HumidityState::from(data);

        // ── Initial values ──────────────────────────────────────────────────

        accessory.thermostat.current_temperature.set_value(Value::from(thermal_state.temperature)).await?;
        accessory.thermostat.target_temperature.set_value(Value::from(thermal_state.target_temperature)).await?;
        accessory.thermostat.current_heating_cooling_state
            .set_value(Value::from(u8::from(thermal_state.heating_cooling_state))).await?;
        accessory.thermostat.target_heating_cooling_state
            .set_value(Value::from(u8::from(thermal_state.target_heating_cooling_state))).await?;

        if let Some(ref mut char) = accessory.thermostat.current_relative_humidity {
            char.set_value(Value::from(humidity_state.humidity)).await?;
        }
        if let Some(ref mut char) = accessory.thermostat.target_relative_humidity {
            char.set_value(Value::from(humidity_state.target_humidity)).await?;
        }

        // ── Thermal handle + read/update callbacks ─────────────────────────

        let thermostat_handle = spawn_thermostat_worker(comelit_id.clone(), thermal_state, client.clone());

        let thermal_state_ro = Arc::new(Mutex::new(thermal_state));
        {
            let s = Arc::clone(&thermal_state_ro);
            accessory.thermostat.current_temperature.on_read_async(Some(move || {
                let s = s.clone();
                async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.temperature)) }.boxed()
            }));
        }
        {
            let s = Arc::clone(&thermal_state_ro);
            accessory.thermostat.target_temperature.on_read_async(Some(move || {
                let s = s.clone();
                async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.target_temperature)) }.boxed()
            }));
        }
        {
            let s = Arc::clone(&thermal_state_ro);
            accessory.thermostat.current_heating_cooling_state.on_read_async(Some(move || {
                let s = s.clone();
                async move { Metrics::inc_hap_requests(); Ok(Some(u8::from(s.lock().await.heating_cooling_state))) }.boxed()
            }));
        }
        {
            let s = Arc::clone(&thermal_state_ro);
            accessory.thermostat.target_heating_cooling_state.on_read_async(Some(move || {
                let s = s.clone();
                async move { Metrics::inc_hap_requests(); Ok(Some(u8::from(s.lock().await.target_heating_cooling_state))) }.boxed()
            }));
        }

        {
            let handle = thermostat_handle.clone();
            accessory.thermostat.target_temperature.on_update_async(Some(move |_, new: f32| {
                let handle = handle.clone();
                async move {
                    Metrics::inc_hap_requests();
                    handle.set_target_temperature(new).await;
                    Ok(())
                }
                .boxed()
            }));
        }

        {
            let handle = thermostat_handle.clone();
            accessory.thermostat.target_heating_cooling_state.on_update_async(Some(move |_prev: u8, new: u8| {
                let handle = handle.clone();
                async move {
                    Metrics::inc_hap_requests();
                    handle.set_hvac_mode(TargetHeatingCoolingState::from(new)).await;
                    Ok(())
                }
                .boxed()
            }));
        }

        // NOTE: `thermal_state_ro` mirrors the worker's internal state purely
        // for the read-callback closures above, which cannot reach into the
        // worker task directly. It is kept in sync via `HapThermostatSink`
        // below (set once the accessory is registered) — until then, reads
        // return the construction-time snapshot, matching the old code's
        // behavior (old code also only updated its shared `arc_state` from
        // within the worker task, which only runs after `SetAccessory`).

        // ── Humidity/dehumidifier worker (local, unchanged from before) ────

        let humidity_arc_state = Arc::new(Mutex::new(humidity_state));
        let (humidity_sender, humidity_receiver) = mpsc::channel::<HumidityCommand>(32);

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
                let tx = humidity_sender.clone();
                threshold.on_update_async(Some(move |_prev, new: f32| {
                    let tx = tx.clone();
                    async move {
                        Metrics::inc_hap_requests();
                        tx.send(HumidityCommand::SetDehumidifierThreshold(new)).await.ok();
                        Ok(())
                    }
                    .boxed()
                }));
            }

            {
                let tx = humidity_sender.clone();
                hd.active.on_update_async(Some(move |_prev: u8, new: u8| {
                    let tx = tx.clone();
                    async move {
                        Metrics::inc_hap_requests();
                        tx.send(HumidityCommand::SetDehumidifierActive(new)).await.ok();
                        Ok(())
                    }
                    .boxed()
                }));
            }
        }

        if let Some(ref mut char) = accessory.thermostat.target_relative_humidity {
            let tx = humidity_sender.clone();
            char.on_update_async(Some(move |_prev, new: f32| {
                let tx = tx.clone();
                async move {
                    Metrics::inc_hap_requests();
                    tx.send(HumidityCommand::SetTargetHumidity(new)).await.ok();
                    Ok(())
                }
                .boxed()
            }));
        }

        let humidity_worker = HumidityWorker::new(comelit_id.clone(), humidity_arc_state, client);
        tokio::spawn(humidity_worker.run(humidity_receiver));

        // ── Register accessory, wire sinks ──────────────────────────────────

        let accessory = server.add_accessory(accessory).await?;

        thermostat_handle.set_sink(Box::new(HapThermostatSink { accessory: accessory.clone() })).await;
        humidity_sender.send(HumidityCommand::SetAccessory(accessory.clone())).await.ok();

        Ok(Self {
            id: data.id.clone(),
            name,
            thermostat_handle,
            humidity_sender,
            accessory,
        })
    }
}
```

Note on `thermal_state_ro`: the read-callback closures for
`current_temperature`/`target_temperature`/`current_heating_cooling_state`/
`target_heating_cooling_state` need somewhere to read from that isn't inside
the worker task (the worker owns its `Arc<TokioMutex<ThermostatState>>`
privately — `client::thermostat::worker` doesn't expose it). The snippet
above keeps a second, HAP-local `Arc<Mutex<ThermostatState>>` that starts at
the same construction-time value and is otherwise write-only from these
reads' perspective (nothing updates it after construction in the snippet
above). **This is a known gap to close in this task, not to ship as-is**:
either (a) extend `ThermostatSink::update` usage so `HapThermostatSink` also
writes into `thermal_state_ro` before or after writing HomeKit
characteristics (simplest — add one line to `HapThermostatSink::update`), or
(b) find a cleaner way to expose read access. Prefer (a): add
`state: Arc<Mutex<ThermostatState>>` as a second field on `HapThermostatSink`,
sharing the same `Arc` as `thermal_state_ro`, and in `update`, write
`*self.state.lock().await = state;` before writing characteristics. Implement
this in Step 2 rather than leaving the two states unsynchronized — write a
manual test plan note in your task report confirming you did this, since it's
easy to miss.

- [ ] **Step 3: Build `hap/` and fix any remaining import mismatches**

Run: `cargo build -p comelit-hub-hap 2>&1 | tail -100`
Expected: builds cleanly once the `thermal_state_ro` synchronization from
Step 2's note is wired in. Fix any remaining path mismatches (verify
`ClimaOnOff` is re-exported from `comelit_client_rs` the same way it always
has been — `hap/src/accessories/thermostat.rs` already imported it before
this change, confirm the import list above didn't accidentally drop it
where still needed for `HumidityWorker`).

- [ ] **Step 4: Run the full hap and client test suites**

Run: `cargo test -p comelit-hub-hap 2>&1 | tail -100`
Expected: all pre-existing tests still pass (door, window covering, doorbell,
web, logging, etc.) — this change touches only thermostat files.

Run: `cargo test -p comelit-client-rs 2>&1 | tail -60`
Expected: all tests still passing, unaffected by this task (this task
doesn't touch `client/`).

- [ ] **Step 5: Commit**

```bash
git add hap/src/accessories/thermostat.rs hap/src/accessories/state/thermostat.rs
git commit -m "Migrate hap thermostat accessory to shared client::thermostat module (thermal path only; humidity/dehumidifier stays local)"
```

---

## Task 5: Add `ComelitThermostatHandler` (Matter `ClusterAsyncHandler` for `Thermostat`)

**Files:**
- Create: `matter/src/thermostat.rs`
- Modify: `matter/src/main.rs` (add `mod thermostat;`)

**Interfaces:**
- Consumes: `comelit_client_rs::thermostat::{ThermostatHandle, ThermostatSink, ThermostatState, TargetHeatingCoolingState, spawn_thermostat_worker}`; `rs_matter::dm::clusters::decl::thermostat` (generated cluster module).
- Produces: `pub struct ThermostatMatterState { pub ep_id: u16, pub device_id: String, pub temperature: AtomicI16, pub target_temperature: AtomicI16, pub system_mode: AtomicU8, pub signal: Signal<CriticalSectionRawMutex, ()>, pub handle: ThermostatHandle }`, `pub struct ThermostatMatterSink`, `pub struct ComelitThermostatHandler` implementing `thermostat::ClusterAsyncHandler`, `pub struct MultiThermostatObserver`.

Unlike `WindowCovering`, `rs-matter`'s generated `Thermostat` `ClusterAsyncHandler`
trait provides **default implementations for most optional attributes**
(returning `Err(ErrorCode::AttributeNotFound.into())`) and for `run`
(`pending()`) — you do NOT need to implement every attribute getter. Only
these are mandatory (no default body in the trait, confirmed by reading the
generated source): `local_temperature`, `control_sequence_of_operation`,
`system_mode` (reads); `set_control_sequence_of_operation`, `set_system_mode`
(writes); and **every** `handle_*` command method (`handle_setpoint_raise_lower`,
`handle_set_weekly_schedule`, `handle_get_weekly_schedule`,
`handle_clear_weekly_schedule`, `handle_set_active_schedule_request`,
`handle_set_active_preset_request`, `handle_add_thermostat_suggestion`,
`handle_remove_thermostat_suggestion`, `handle_atomic_request`) — all 9
commands are mandatory in the trait even though only `SetpointRaiseLower` is
declared in `CLUSTER`'s command list; the other 8 need a stub that returns an
error, since the framework won't ever route them to a handler whose `CLUSTER`
doesn't declare them, but the Rust trait still requires a body.

- [ ] **Step 1: Create `matter/src/thermostat.rs`**

```rust
// matter/src/thermostat.rs
use std::sync::Arc;
use std::sync::atomic::{AtomicI16, AtomicU8, Ordering};

use async_trait::async_trait;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use log::info;

use rs_matter::dm::Cluster;
use rs_matter::dm::clusters::decl::thermostat::{
    self as thermostat_cluster, ClusterAsyncHandler, ControlSequenceOfOperationEnum,
    SetpointRaiseLowerModeEnum, SetpointRaiseLowerRequest, SetActiveScheduleRequestRequest,
    SetActivePresetRequestRequest, SetWeeklyScheduleRequest, GetWeeklyScheduleRequest,
    GetWeeklyScheduleResponseBuilder, AddThermostatSuggestionRequest,
    AddThermostatSuggestionResponseBuilder, RemoveThermostatSuggestionRequest, SystemModeEnum,
};
use rs_matter::dm::{Dataver, HandlerContext, InvokeContext, ReadContext, WriteContext};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{Nullable, TLVBuilderParent};

use comelit_client_rs::thermostat::{
    TargetHeatingCoolingState, ThermostatHandle, ThermostatSink, ThermostatState,
};
use comelit_client_rs::{HomeDeviceData, StatusUpdate};

/// State shared between the Matter cluster handler and the Comelit worker,
/// mirroring `covering::CoveringState`. Temperatures are stored in Matter's
/// native unit: hundredths of a degree Celsius (i16), converted from/to the
/// client's Celsius-as-f32 at the boundary.
pub struct ThermostatMatterState {
    pub ep_id: u16,
    pub device_id: String,
    pub temperature: AtomicI16,
    pub target_temperature: AtomicI16,
    /// Encoded as `TargetHeatingCoolingState as u8` (0=Off, 1=Heat, 2=Cool, 3=Auto).
    /// Matter-side reads always reduce Auto away (see `system_mode`), but the
    /// raw client-side value is kept here for fidelity.
    pub system_mode: AtomicU8,
    pub signal: Signal<CriticalSectionRawMutex, ()>,
    pub handle: ThermostatHandle,
}

fn celsius_to_matter(celsius: f32) -> i16 {
    (celsius * 100.0).round() as i16
}

fn matter_to_celsius(hundredths: i16) -> f32 {
    hundredths as f32 / 100.0
}

impl ThermostatMatterState {
    pub fn new(ep_id: u16, device_id: String, initial: ThermostatState, handle: ThermostatHandle) -> Self {
        Self {
            ep_id,
            device_id,
            temperature: AtomicI16::new(celsius_to_matter(initial.temperature)),
            target_temperature: AtomicI16::new(celsius_to_matter(initial.target_temperature)),
            system_mode: AtomicU8::new(initial.heating_cooling_state as u8),
            signal: Signal::new(),
            handle,
        }
    }
}

pub struct ThermostatMatterSink {
    state: Arc<ThermostatMatterState>,
}

impl ThermostatMatterSink {
    pub fn new(state: Arc<ThermostatMatterState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ThermostatSink for ThermostatMatterSink {
    async fn update(&self, state: ThermostatState) {
        self.state.temperature.store(celsius_to_matter(state.temperature), Ordering::Relaxed);
        self.state.target_temperature.store(celsius_to_matter(state.target_temperature), Ordering::Relaxed);
        self.state.system_mode.store(state.heating_cooling_state as u8, Ordering::Relaxed);
        self.state.signal.signal(());
        info!(
            "MQTT → Matter ep{}: {} temp={:.1} target={:.1}",
            self.state.ep_id, self.state.device_id, state.temperature, state.target_temperature
        );
    }
}

fn decode_hvac_state(raw: u8) -> TargetHeatingCoolingState {
    match raw {
        1 => TargetHeatingCoolingState::Heat,
        2 => TargetHeatingCoolingState::Cool,
        3 => TargetHeatingCoolingState::Auto,
        _ => TargetHeatingCoolingState::Off,
    }
}

/// Maps the client's 4-state model onto Matter's `SystemModeEnum`. `Auto`
/// (dual-setpoint with deadband) is never exposed — per the design decision,
/// it's reduced to `Heat` (client `Auto` only ever coexists with winter,
/// which the client-side reduction already turns into `Heat` before this
/// point — see `client::thermostat::state::ThermostatState::from`). This
/// fallback exists for defense in depth in case that invariant ever changes.
fn to_system_mode(state: TargetHeatingCoolingState) -> SystemModeEnum {
    match state {
        TargetHeatingCoolingState::Off => SystemModeEnum::Off,
        TargetHeatingCoolingState::Heat | TargetHeatingCoolingState::Auto => SystemModeEnum::Heat,
        TargetHeatingCoolingState::Cool => SystemModeEnum::Cool,
    }
}

fn from_system_mode(mode: SystemModeEnum) -> TargetHeatingCoolingState {
    match mode {
        SystemModeEnum::Off => TargetHeatingCoolingState::Off,
        SystemModeEnum::Heat | SystemModeEnum::EmergencyHeat => TargetHeatingCoolingState::Heat,
        SystemModeEnum::Cool | SystemModeEnum::Precooling => TargetHeatingCoolingState::Cool,
        // AUTO_MODE feature isn't declared in CLUSTER, so a controller
        // shouldn't send Auto — fall back to Off defensively rather than
        // silently picking a season.
        _ => TargetHeatingCoolingState::Off,
    }
}

/// Implements the `Thermostat` cluster for one bridged Comelit thermostat.
/// AUTO_MODE feature is deliberately NOT declared (see design spec §"Auto
/// mode"): only HEATING | COOLING. Both `OccupiedHeatingSetpoint` and
/// `OccupiedCoolingSetpoint` mirror the same `target_temperature`, since
/// Comelit has only one target.
pub struct ComelitThermostatHandler {
    dataver: Dataver,
    state: Arc<ThermostatMatterState>,
}

impl ComelitThermostatHandler {
    pub fn new(dataver: Dataver, state: Arc<ThermostatMatterState>) -> Self {
        Self { dataver, state }
    }
}

const ABS_MIN_SETPOINT: i16 = 700;  // 7.00 C
const ABS_MAX_SETPOINT: i16 = 3500; // 35.00 C

impl ClusterAsyncHandler for ComelitThermostatHandler {
    const CLUSTER: Cluster<'static> = thermostat_cluster::FULL_CLUSTER
        .with_revision(6)
        .with_features(
            thermostat_cluster::Feature::HEATING.bits() | thermostat_cluster::Feature::COOLING.bits(),
        );

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
        }
    }

    async fn local_temperature(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::some(self.state.temperature.load(Ordering::Relaxed)))
    }

    async fn control_sequence_of_operation(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<ControlSequenceOfOperationEnum, Error> {
        Ok(ControlSequenceOfOperationEnum::CoolingAndHeating)
    }

    async fn set_control_sequence_of_operation(
        &self,
        _ctx: impl WriteContext,
        _value: ControlSequenceOfOperationEnum,
    ) -> Result<(), Error> {
        // Fixed at CoolingAndHeating; accept writes as a no-op rather than
        // erroring, since a controller re-writing the value it just read is
        // benign and this is a mandatory-writable attribute per spec.
        Ok(())
    }

    async fn system_mode(&self, _ctx: impl ReadContext) -> Result<SystemModeEnum, Error> {
        let raw = self.state.system_mode.load(Ordering::Relaxed);
        Ok(to_system_mode(decode_hvac_state(raw)))
    }

    async fn set_system_mode(&self, _ctx: impl WriteContext, value: SystemModeEnum) -> Result<(), Error> {
        self.state.handle.set_hvac_mode(from_system_mode(value)).await;
        Ok(())
    }

    async fn occupied_heating_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.state.target_temperature.load(Ordering::Relaxed))
    }

    async fn set_occupied_heating_setpoint(&self, _ctx: impl WriteContext, value: i16) -> Result<(), Error> {
        self.state.handle.set_target_temperature(matter_to_celsius(value)).await;
        Ok(())
    }

    async fn occupied_cooling_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.state.target_temperature.load(Ordering::Relaxed))
    }

    async fn set_occupied_cooling_setpoint(&self, _ctx: impl WriteContext, value: i16) -> Result<(), Error> {
        self.state.handle.set_target_temperature(matter_to_celsius(value)).await;
        Ok(())
    }

    async fn abs_min_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(ABS_MIN_SETPOINT)
    }

    async fn abs_max_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(ABS_MAX_SETPOINT)
    }

    async fn abs_min_cool_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(ABS_MIN_SETPOINT)
    }

    async fn abs_max_cool_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(ABS_MAX_SETPOINT)
    }

    async fn handle_setpoint_raise_lower(
        &self,
        _ctx: impl InvokeContext,
        request: SetpointRaiseLowerRequest<'_>,
    ) -> Result<(), Error> {
        let _mode: SetpointRaiseLowerModeEnum = request.mode()?;
        let amount = request.amount()?; // tenths of a degree C, per Matter spec
        let current = self.state.target_temperature.load(Ordering::Relaxed);
        let delta_hundredths = i16::from(amount) * 10;
        let new_value = current.saturating_add(delta_hundredths).clamp(ABS_MIN_SETPOINT, ABS_MAX_SETPOINT);
        self.state.handle.set_target_temperature(matter_to_celsius(new_value)).await;
        Ok(())
    }

    // The remaining 8 commands are mandatory in the trait but never declared
    // in CLUSTER's command list, so the framework never routes them here —
    // these bodies exist only to satisfy the trait.

    async fn handle_set_weekly_schedule(
        &self,
        _ctx: impl InvokeContext,
        _request: SetWeeklyScheduleRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_get_weekly_schedule<P: TLVBuilderParent>(
        &self,
        _ctx: impl InvokeContext,
        _request: GetWeeklyScheduleRequest<'_>,
        _response: GetWeeklyScheduleResponseBuilder<P>,
    ) -> Result<P, Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_clear_weekly_schedule(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_set_active_schedule_request(
        &self,
        _ctx: impl InvokeContext,
        _request: SetActiveScheduleRequestRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_set_active_preset_request(
        &self,
        _ctx: impl InvokeContext,
        _request: SetActivePresetRequestRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_add_thermostat_suggestion<P: TLVBuilderParent>(
        &self,
        _ctx: impl InvokeContext,
        _request: AddThermostatSuggestionRequest<'_>,
        _response: AddThermostatSuggestionResponseBuilder<P>,
    ) -> Result<P, Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_remove_thermostat_suggestion(
        &self,
        _ctx: impl InvokeContext,
        _request: RemoveThermostatSuggestionRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_atomic_request<P: TLVBuilderParent>(
        &self,
        _ctx: impl InvokeContext,
        _request: rs_matter::dm::clusters::decl::thermostat::AtomicRequestRequest<'_>,
        _response: rs_matter::dm::clusters::decl::thermostat::AtomicResponseBuilder<P>,
    ) -> Result<P, Error> {
        Err(ErrorCode::CommandNotFound.into())
    }
}

/// Receives MQTT push-updates for all bridged thermostats and forwards them
/// to the matching worker via its `ThermostatHandle`.
pub struct MultiThermostatObserver {
    pub states: Vec<Arc<ThermostatMatterState>>,
}

#[async_trait]
impl StatusUpdate for MultiThermostatObserver {
    async fn status_update(&self, device: &HomeDeviceData) {
        if let HomeDeviceData::Thermostat(data) = device {
            if let Some(state) = self.states.iter().find(|s| s.device_id == data.id) {
                state.handle.mqtt_push(ThermostatState::from(data)).await;
            }
        }
    }
}
```

Notes for the implementer to verify against the actual generated code (the
codegen output was read at
`target/debug/build/rs-matter-9cbfe561c645642e/out/clusters_generated/thermostat.rs`,
a build artifact whose hash-suffixed directory name will differ on a fresh
build — find the current one with `find target -path "*rs-matter-*/out/clusters_generated/thermostat.rs" | head -1`
if that exact path doesn't exist):
- Confirm every request/response type name used in the mandatory `handle_*`
  stubs (`SetWeeklyScheduleRequest`, `GetWeeklyScheduleRequest`/
  `GetWeeklyScheduleResponseBuilder`, `SetActiveScheduleRequestRequest`,
  `SetActivePresetRequestRequest`, `AddThermostatSuggestionRequest`/
  `AddThermostatSuggestionResponseBuilder`, `RemoveThermostatSuggestionRequest`,
  `AtomicRequestRequest`/`AtomicResponseBuilder`) against the real generated
  module — grep the generated file for each name; this plan lists them from
  the `CommandId` enum and the trait's method signatures but did not read
  every request/response struct definition individually.
- Confirm `Feature::HEATING`/`Feature::COOLING` bit values and
  `with_features`/`with_revision`/`FULL_CLUSTER` builder methods match the
  pattern already used in `matter/src/covering.rs` (same builder API,
  already proven to compile there).
- Confirm `ControlSequenceOfOperationEnum::CoolingAndHeating` is the correct
  variant name (confirmed present in the codegen source read during
  planning, enumval 4, but re-verify the exact Rust identifier casing).
- Confirm `Nullable::some(...)` is the right constructor (already used and
  proven in `matter/src/covering.rs`).
- The trait requires `async fn local_temperature` etc. with `Result<Nullable<i16>, Error>`
  in some listings and `impl Future<Output = Result<...>>` in others (mixed
  `async fn`/RPITIT style within one trait, same situation encountered and
  resolved for `WindowCovering` in the previous plan — using plain `async fn`
  for every override compiles fine regardless of which style the trait
  declares the method in, per that prior experience).
- `ABS_MIN_SETPOINT`/`ABS_MAX_SETPOINT` (7°C/35°C) are placeholder-but-real
  values per the design spec's explicit "not vincolante" (not binding) note —
  keep them as literal named constants exactly as shown, do not treat the
  need to pick concrete numbers as an open TODO.

- [ ] **Step 2: Add the module to `matter/src/main.rs`**

Add `mod thermostat;` next to the existing `mod bridge; mod covering; mod light; mod mdns;` at the top of `matter/src/main.rs`.

- [ ] **Step 3: Build**

Run: `cargo build -p comelit-hub-matter 2>&1 | tail -150`
Expected: does not build yet — `ComelitThermostatHandler` isn't wired into
`bridge.rs`/`main.rs` (Tasks 6-7). Work through type errors against the real
generated trait per the notes above until `matter/src/thermostat.rs` itself
compiles with zero errors (warnings about unused `pub` items are fine at
this stage).

- [ ] **Step 4: Commit**

```bash
git add matter/src/thermostat.rs matter/src/main.rs
git commit -m "Add ComelitThermostatHandler: Matter Thermostat cluster handler"
```

---

## Task 6: Generalize `matter/src/bridge.rs` to include `BridgedEntry::Thermostat`

**Files:**
- Modify: `matter/src/bridge.rs`

**Interfaces:**
- Consumes: `crate::thermostat::ComelitThermostatHandler` (Task 5).
- Produces: `pub enum BridgedEntry { Light(LightEntry), WindowCovering(CoveringEntry), Thermostat(ThermostatEntry) }`, `pub struct ThermostatEntry { pub ep_id: u16, pub thermostat: ComelitThermostatHandler, pub desc: desc::DescHandler<'static>, pub groups: groups::GroupsHandler, pub bridged: BridgedInfo }`.

- [ ] **Step 1: Add imports and device-type/cluster statics for `Thermostat`**

Extend the existing `use` block in `matter/src/bridge.rs` (read the current
file first — by this point it already has `covering_cluster` imported from
Task 8 of the previous plan; add alongside it, keeping whatever `desc`/`groups`
import style the file already uses, same caveat as before):

```rust
use rs_matter::dm::clusters::decl::thermostat::{self as thermostat_cluster, ClusterAsyncHandler as _};
```

Add new statics after the existing `COVERING_DEVICE_TYPES`/`COVERING_CLUSTERS`:

```rust
const DEV_TYPE_THERMOSTAT: DeviceType = DeviceType { dtype: 0x0301, drev: 1 };

static THERMOSTAT_DEVICE_TYPES: [DeviceType; 2] = [DEV_TYPE_THERMOSTAT, DEV_TYPE_BRIDGED_NODE];
static THERMOSTAT_CLUSTERS: [Cluster<'static>; 4] = [
    desc::DescHandler::CLUSTER,
    groups::GroupsHandler::CLUSTER,
    <BridgedInfo as BridgedCH>::CLUSTER,
    ComelitThermostatHandler::CLUSTER,
];
```

Add the import for `ComelitThermostatHandler`:

```rust
use crate::thermostat::ComelitThermostatHandler;
```

- [ ] **Step 2: Add `ThermostatEntry` and extend `BridgedEntry`**

After the existing `CoveringEntry` struct:

```rust
/// All handlers and shared state for a single bridged thermostat endpoint.
pub struct ThermostatEntry {
    pub ep_id: u16,
    pub thermostat: ComelitThermostatHandler,
    pub desc: desc::DescHandler<'static>,
    pub groups: groups::GroupsHandler,
    pub bridged: BridgedInfo,
}
```

Extend `BridgedEntry` and its `ep_id()` helper:

```rust
pub enum BridgedEntry {
    Light(LightEntry),
    WindowCovering(CoveringEntry),
    Thermostat(ThermostatEntry),
}

impl BridgedEntry {
    fn ep_id(&self) -> u16 {
        match self {
            BridgedEntry::Light(l) => l.ep_id,
            BridgedEntry::WindowCovering(c) => c.ep_id,
            BridgedEntry::Thermostat(t) => t.ep_id,
        }
    }
}
```

- [ ] **Step 3: Extend `read`/`write`/`invoke`/`bump_dataver`/`run`/`BridgeMetadata::new` with the third arm**

In `read`, add to the `match self.entries.iter().find(...)` block:

```rust
Some(BridgedEntry::Thermostat(thermostat)) => match cluster_id {
    c if c == desc::DescHandler::CLUSTER.id =>
        DmAsync(desc::HandlerAdaptor(&thermostat.desc)).read(ctx, reply).await,
    c if c == groups::GroupsHandler::CLUSTER.id =>
        DmAsync(groups::HandlerAdaptor(&thermostat.groups)).read(ctx, reply).await,
    c if c == bridged_device_basic_information::FULL_CLUSTER.id =>
        DmAsync(bridged_device_basic_information::HandlerAdaptor(&thermostat.bridged)).read(ctx, reply).await,
    c if c == ComelitThermostatHandler::CLUSTER.id =>
        thermostat_cluster::HandlerAsyncAdaptor(&thermostat.thermostat).read(ctx, reply).await,
    _ => Err(ErrorCode::ClusterNotFound.into()),
},
```

In `write`:

```rust
Some(BridgedEntry::Thermostat(thermostat)) => match cluster_id {
    c if c == ComelitThermostatHandler::CLUSTER.id =>
        thermostat_cluster::HandlerAsyncAdaptor(&thermostat.thermostat).write(ctx).await,
    _ => Err(ErrorCode::AttributeNotFound.into()),
},
```

In `invoke`:

```rust
Some(BridgedEntry::Thermostat(thermostat)) => match cluster_id {
    c if c == ComelitThermostatHandler::CLUSTER.id =>
        thermostat_cluster::HandlerAsyncAdaptor(&thermostat.thermostat).invoke(ctx, reply).await,
    _ => Err(ErrorCode::CommandNotFound.into()),
},
```

In `bump_dataver`'s `match entry { ... }`:

```rust
BridgedEntry::Thermostat(thermostat) => {
    if cl.map(|c| c == desc::DescHandler::CLUSTER.id).unwrap_or(true) {
        DescCH::dataver_changed(&thermostat.desc);
    }
    if cl.map(|c| c == groups::GroupsHandler::CLUSTER.id).unwrap_or(true) {
        GroupsCH::dataver_changed(&thermostat.groups);
    }
    if cl.map(|c| c == bridged_device_basic_information::FULL_CLUSTER.id).unwrap_or(true) {
        BridgedCH::dataver_changed(&thermostat.bridged);
    }
    if cl.map(|c| c == ComelitThermostatHandler::CLUSTER.id).unwrap_or(true) {
        thermostat_cluster::HandlerAsyncAdaptor(&thermostat.thermostat).bump_dataver(&ctx);
    }
}
```

In `run`'s `filter_map`/`map` (whichever form the file currently has after the
previous plan's final-review fix wave — check the current file: it should
already be a `.map(|entry| match entry { ... })` since `BridgedEntry::WindowCovering`
no longer maps to `None`), add the third arm:

```rust
BridgedEntry::Thermostat(t) => Box::pin(t.thermostat.run(&ctx)) as DynFut<'_>,
```

In `BridgeMetadata::new`'s `match entry { ... }`:

```rust
BridgedEntry::Thermostat(thermostat) => {
    endpoints.push(Endpoint::new(thermostat.ep_id, &THERMOSTAT_DEVICE_TYPES, &THERMOSTAT_CLUSTERS));
}
```

- [ ] **Step 4: Build (expect failures in `main.rs`, fix in Task 7)**

Run: `cargo build -p comelit-hub-matter 2>&1 | tail -100`
Expected: `matter/src/bridge.rs` compiles; `matter/src/main.rs` fails because
it still only constructs light/covering entries. Confirm the *only* errors
are in `main.rs`.

- [ ] **Step 5: Commit**

```bash
git add matter/src/bridge.rs
git commit -m "Extend BridgedEntry with Thermostat variant"
```

---

## Task 7: Wire thermostat discovery into `matter/src/main.rs`

**Files:**
- Modify: `matter/src/main.rs`

**Interfaces:**
- Consumes: `crate::thermostat::{ThermostatMatterState, ComelitThermostatHandler, ThermostatMatterSink, MultiThermostatObserver}` (Task 5), `crate::bridge::{BridgedEntry, ThermostatEntry}` (Task 6), `comelit_client_rs::thermostat::{ThermostatState, spawn_thermostat_worker}`.

- [ ] **Step 1: Extend discovery to also collect thermostats**

In `main()`'s discovery block (after the covering discovery from the previous
plan), add:

```rust
    let mut thermostat_data: Vec<(String, String, comelit_client_rs::thermostat::ThermostatState)> = index
        .iter()
        .filter_map(|entry| {
            if let HomeDeviceData::Thermostat(th) = entry.value() {
                let label = th.description.clone().unwrap_or_else(|| entry.key().clone());
                let initial_state = comelit_client_rs::thermostat::ThermostatState::from(th);
                Some((entry.key().clone(), label, initial_state))
            } else {
                None
            }
        })
        .collect();
    thermostat_data.sort_by(|a, b| a.0.cmp(&b.0));
```

Update the empty-check to include thermostats:

```rust
    if lights_data.is_empty() && covering_data.is_empty() && thermostat_data.is_empty() {
        return Err(anyhow::anyhow!("No lights, window coverings, or thermostats found in Comelit index"));
    }
```

Update the discovery log loop to also log thermostats, continuing the
running `next_ep`/`ep_id` counter (both step 2's log loop and step 3's
construction loop from the previous plan — extend both the same way):

```rust
    for (id, label, state) in &thermostat_data {
        info!("  ep{}: thermostat {} ({}) — {:.1}C -> {:.1}C", next_ep, label, id, state.temperature, state.target_temperature);
        next_ep += 1;
    }
```

- [ ] **Step 2: Spawn thermostat workers and wire the observer**

Extend the state-construction block (after the covering-states loop from the
previous plan):

```rust
    let mut thermostat_states: Vec<Arc<thermostat::ThermostatMatterState>> = Vec::new();
    for (id, _, initial_state) in &thermostat_data {
        let handle = comelit_client_rs::thermostat::spawn_thermostat_worker(
            id.clone(),
            *initial_state,
            client.clone(),
        );
        let state = Arc::new(thermostat::ThermostatMatterState::new(ep_id, id.clone(), *initial_state, handle));
        state.handle.set_sink(Box::new(thermostat::ThermostatMatterSink::new(state.clone()))).await;
        thermostat_states.push(state);
        ep_id += 1;
    }
```

Extend the fan-out observer to include thermostats — replace the two-observer
`FanOutObserver` from the previous plan with a three-observer version:

```rust
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

Extend the MQTT subscribe loop:

```rust
    for (id, _, _) in &thermostat_data {
        client.subscribe(id).await?;
    }
```

- [ ] **Step 3: Update `run_matter` to build `BridgedEntry::Thermostat` entries**

Extend `run_matter`'s signature:

```rust
fn run_matter(
    light_states: Vec<Arc<LightState>>,
    lights_data: Vec<(String, String, bool)>,
    covering_states: Vec<Arc<covering::CoveringState>>,
    covering_data: Vec<(String, String, comelit_client_rs::covering::WindowCoveringState)>,
    thermostat_states: Vec<Arc<thermostat::ThermostatMatterState>>,
    thermostat_data: Vec<(String, String, comelit_client_rs::thermostat::ThermostatState)>,
) -> anyhow::Result<()> {
```

Extend the entry-construction loop, after the covering-entries loop:

```rust
    for (state, (device_id, label, _)) in thermostat_states.into_iter().zip(thermostat_data.iter()) {
        let ep_id = state.ep_id;
        entries.push(BridgedEntry::Thermostat(ThermostatEntry {
            ep_id,
            thermostat: ComelitThermostatHandler::new(Dataver::new_rand(&mut rand), state),
            desc: desc::DescHandler::new(Dataver::new_rand(&mut rand)),
            groups: groups::GroupsHandler::new(Dataver::new_rand(&mut rand)),
            bridged: BridgedInfo::new(Dataver::new_rand(&mut rand), label.clone(), device_id.clone()),
        }));
    }
```

Add imports:

```rust
use bridge::{BridgeMetadata, BridgedEntry, BridgedInfo, ComelitBridgeHandler, CoveringEntry, LightEntry, NonRootMatcher, ThermostatEntry};
use thermostat::ComelitThermostatHandler;
```

(merge with whatever the existing `use bridge::{...}` line already imports
by this point in the plan sequence — read the current file first).

- [ ] **Step 4: Update the `run_matter` call site**

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

- [ ] **Step 5: Build the whole workspace**

Run: `cargo build --workspace 2>&1 | tail -150`
Expected: builds cleanly. Work through any remaining type mismatches.

- [ ] **Step 6: Run the full workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -150`
Expected: all tests pass across `comelit-client-rs`, `comelit-hub-hap`,
`comelit-hub-matter`, `viper-client`.

- [ ] **Step 7: Commit**

```bash
git add matter/src/main.rs
git commit -m "Discover and bridge Comelit thermostats alongside lights and window coverings in the Matter bridge"
```

---

## Task 8: Manual smoke check

**Files:** none required (verification-only task).

**Interfaces:** none.

- [ ] **Step 1: Full workspace check**

Run: `cargo check --workspace 2>&1 | tail -100`
Expected: zero new warnings beyond what already existed on `main` before
this plan.

- [ ] **Step 2: Dry-run the binary against no real hub**

Run: `cargo run -p comelit-hub-matter -- --host 127.0.0.1 2>&1 | head -30`
Expected: attempts to connect and fails cleanly with a connection/scan
error, not a panic — same behavior the binary already had before this plan.

- [ ] **Step 3: If available, dry-run against a real Comelit hub with a thermostat**

Only if the user explicitly agrees to a live test against real hardware:
run `cargo run -p comelit-hub-matter -- --host <real-hub-ip>` and confirm the
log lists a thermostat with sensible current/target temperature values.
Skip and report explicitly if no hardware is available or the user doesn't
want a live test.

---

## Self-Review Notes (already applied above)

- **Spec coverage:** all three numbered design decisions are implemented —
  code sharing via `client::thermostat` restricted to thermal fields (Tasks
  1-4), no persistence needed (nothing added), Auto-mode reduction to
  Heat/Cool on the Matter side (Task 5's `to_system_mode`). The spec's
  explicit out-of-scope items (dehumidifier, AUTO_MODE/SCHEDULE/PRESETS/
  SETBACK/OCCUPANCY features, other device types) are correctly not
  implemented. The spec's "rischi e aperture" are addressed: `drev` value
  flagged with the same rationale as `DEV_TYPE_WINDOW_COVERING` (Task 6),
  setpoint limit constants chosen concretely (Task 5), dehumidifier-state
  wrapper approach resolved concretely as a parallel `HumidityWorker`
  (Task 4).
- **Placeholder scan:** no TBD/TODO markers. The "verify against generated
  code" notes in Tasks 5 and 6 point to concrete `grep`/build commands, same
  pattern as the previous plan's equivalent notes, which resolved cleanly in
  practice.
- **Type consistency:** `ThermostatHandle` (Task 2) used identically in Task
  4 (HAP: `set_target_temperature`, `set_hvac_mode`, `set_sink`, `.clone()`)
  and Task 5 (Matter: same three methods). `ThermostatMatterState` (Task 5)
  fields (`ep_id`, `device_id`, `temperature`, `target_temperature`,
  `system_mode`, `signal`, `handle`) used consistently in Task 7's
  `run_matter`/observer wiring. `ComelitThermostatHandler::new(dataver, state)`
  (Task 5) matches its call site in Task 7.
- **Global Constraints cross-check:** the Global Constraints section's
  no-`Drop` rule is honored in Task 2 (`ThermostatHandle` has no `Drop`
  impl, with an explicit test — `test_worker_survives_dropped_handle_clone`
  — mirroring the regression test that closed the equivalent Critical bug
  in the previous plan's final review). The `run()`-override rule is honored
  in Task 5 (`ComelitThermostatHandler::run` overrides the default, and Task
  6 wires it into `ComelitBridgeHandler::run`'s `match`, both required
  together per the lesson from the previous plan).
