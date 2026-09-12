# Matter Dehumidifier Support — Design

## Context

The Matter bridge currently exposes lights, window coverings, and
thermostats (`matter/src/light.rs`, `covering.rs`, `thermostat.rs`,
dispatched through the `BridgedEntry` enum in `matter/src/bridge.rs`). The
HAP bridge additionally exposes dehumidifier control for Comelit
thermostats whose `sub_type` is `ObjectSubtype::ClimaThermostatDehumidifier`
(`hap/src/accessories/thermostat.rs`): a local `HumidityWorker` reads and
writes humidity target/threshold and dehumidifier on/off via
`ComelitClient::set_humidity()` / `toggle_thermostat_status(..,
ClimaOnOff::OnHumi/OffHumi)`, and reports state on both the Thermostat
service's humidity characteristics and a separate `HumidifierDehumidifier`
HAP service.

This design extends the Matter bridge with dehumidifier support, closing
part of the feature gap with the HAP bridge, and — as a prerequisite —
extracts the current HAP-only `HumidityWorker` into a shared,
protocol-agnostic module under `client/`, following the same pattern
already used for window coverings and thermostats.

## Goals

- Bridge the Comelit thermostat's dehumidifier as a Matter device: on/off
  control and current relative-humidity reporting.
- Extract `HumidityState` / `HumidityWorker` out of
  `hap/src/accessories/thermostat.rs` into a shared `client/src/humidity/`
  module, and migrate HAP onto it, matching the `covering`/`thermostat`
  precedent.
- Keep the fire-and-forget removal invariant established in the previous
  session: every write command resolves to the real outcome of the hub
  call via an `oneshot` reply, never an immediate synthetic `Ok(())`.

## Non-Goals

- Writing the humidity target or dehumidifier-on threshold from Matter.
  Verified against the actual IDL our `rs-matter` pin (`e8b0b0c`, which
  already targets the published Matter 1.5.1 data model —
  `controller-clusters-V1.5.1.0.matter`) compiles from: no cluster in it
  exposes a writable humidity setpoint for an active dehumidifier.
  `RelativeHumidityMeasurement` (cluster 0x0405) is the only humidity-related
  cluster defined, and it's read-only, built for passive sensors. The
  currently-tracked Matter 1.6 issues upstream (Thread Commissioning,
  JointFabric/JCM, NFC transport, Groupcast, Network Recovery) are all
  transport/commissioning work, not new application clusters — there's no
  near-term signal this gap closes. Only current humidity (read) and on/off
  (read/write) are bridged.
- Modeling the dehumidifier via `Thermostat`'s `SystemMode::Dry`. Comelit's
  dehumidifier runs *alongside* cooling, not instead of it (confirmed with
  the user); collapsing both into one `SystemMode` value would destroy the
  "is it cooling" signal for any thermostat that also has an active
  dehumidifier. A separate endpoint avoids this entirely.
- Standalone `ClimaDehumidifier` devices (sub_type 17, a dehumidifier with
  no attached thermostat). Out of scope for this pass — the HAP bridge does
  not expose these as a distinct entity either; revisit alongside HAP if
  ever needed.
- Persisting dehumidifier state across restarts (window coverings persist
  estimated position because Comelit's own status is coarse; dehumidifier
  on/off and humidity are always fully known from the hub's status, so
  there's nothing to estimate).

## Architecture

### Shared `client/src/humidity/` module

Mirrors `client/src/covering/` and `client/src/thermostat/`:

- **`state.rs`** — `HumidityState`, moved verbatim from
  `hap/src/accessories/state/thermostat.rs` (fields: `humidity: f32`,
  `target_humidity: f32`, `dehumidifier_active: bool`,
  `dehumidifier_current_state: u8`), plus its
  `From<&ThermostatDeviceData>` conversion. The doc comment claiming this
  is "out of scope for the Matter bridge" is removed since it no longer
  applies.
- **`worker.rs`** — `HumidityCommand`, `HumidityWorker`, `HumidityHandle`,
  `spawn_humidity_worker`, following the `ThermostatCommand`/
  `ThermostatWorker`/`ThermostatHandle`/`spawn_thermostat_worker` shape
  already in `client/src/thermostat/worker.rs`:

  ```rust
  enum HumidityCommand {
      SetTargetHumidity(f32, oneshot::Sender<anyhow::Result<()>>),
      SetDehumidifierActive(bool, oneshot::Sender<anyhow::Result<()>>),
      /// Hub pushed a status update → recompute state, notify the sink.
      MqttPush(HumidityState),
  }
  ```

  `HumidityHandle` (Clone, no `Drop`-sends-shutdown, matching the
  established pattern) exposes:

  ```rust
  impl HumidityHandle {
      pub async fn set_target_humidity(&self, value: f32) -> anyhow::Result<()>;
      pub async fn set_dehumidifier_active(&self, active: bool) -> anyhow::Result<()>;
      pub async fn mqtt_push(&self, state: HumidityState);
      pub async fn set_sink(&self, sink: Box<dyn HumiditySink>);
  }
  ```

  `set_target_humidity` calls `client.set_humidity(&id, value as i32)`;
  `set_dehumidifier_active` calls `client.toggle_thermostat_status(&id,
  ClimaOnOff::OnHumi)` or `::OffHumi`. Both send their real
  `anyhow::Result<()>` back through the `oneshot::Sender` before the
  worker updates its cached `HumidityState`, matching
  `ThermostatWorker`'s existing pattern (state is only updated on a
  confirmed success).

- **`mod.rs`** — re-exports plus the `HumiditySink` trait:

  ```rust
  #[async_trait::async_trait]
  pub trait HumiditySink: Send + Sync {
      async fn update(&self, state: HumidityState);
  }
  ```

  A worker with no sink registered yet (before `set_accessory`-equivalent
  wiring completes) simply drops the update, matching
  `WindowCoveringSink`/`ThermostatMatterSink`'s existing behavior.

### HAP migration

`hap/src/accessories/thermostat.rs` drops its local `HumidityCommand` /
`HumidityWorker` and the `humidity_sender: Sender<HumidityCommand>` field,
replacing them with `comelit_client_rs::humidity::HumidityHandle`,
constructed via `spawn_humidity_worker` alongside the existing
`spawn_thermostat_worker` call in `ComelitThermostatAccessory::new`. A new
`HapHumiditySink` (in the same file, mirroring `HapWindowCoveringSink` in
`hap/src/accessories/window_covering.rs`) implements `HumiditySink::update`
by writing the same characteristics the current `HumidityWorker::
update_accessory` writes: `CurrentRelativeHumidity`/
`TargetRelativeHumidity` on the Thermostat service, and `Active`/
`CurrentHumidifierDehumidifierState`/`CurrentRelativeHumidity`/
`RelativeHumidityDehumidifierThreshold` on the separate
`HumidifierDehumidifier` service. The two `on_update_async` callbacks for
target humidity and dehumidifier active-state switch from sending on the
local channel to calling `humidity_handle.set_target_humidity(..).await?`
/ `.set_dehumidifier_active(..).await?`, propagating the real result —
consistent with yesterday's fire-and-forget removal across the other HAP
accessories.

This module is only constructed for thermostats where `data.sub_type ==
ObjectSubtype::ClimaThermostatDehumidifier`, matching the existing
`has_dehumidifier` check at `hap/src/accessories/thermostat.rs:347`.

### Matter: new `matter/src/dehumidifier.rs`

One **separate bridged endpoint** per dehumidifier-equipped thermostat
(not reusing the thermostat's own endpoint), containing three cluster
handlers plus the standard bridged-endpoint scaffolding (`desc`, `groups`,
`bridged: BridgedInfo` — same as every other `*Entry`):

```rust
pub struct DehumidifierEntry {
    pub ep_id: u16,
    pub on_off: ComelitDehumidifierOnOffHandler,
    pub humidity: ComelitHumidityMeasurementHandler,
    pub desc: desc::DescHandler<'static>,
    pub groups: groups::GroupsHandler,
    pub bridged: BridgedInfo,
}
```

**Shared state** — `DehumidifierMatterState`, mirroring
`ThermostatMatterState`:

```rust
pub struct DehumidifierMatterState {
    pub ep_id: u16,
    pub device_id: String,
    pub active: AtomicBool,
    pub humidity: AtomicU16, // hundredths of a percent, Matter's native unit
    pub signal: Signal<CriticalSectionRawMutex, ()>,
    pub handle: HumidityHandle,
}
```

`humidity` is stored as `u16` hundredths-of-a-percent (`HumidityState.
humidity: f32` percent × 100), matching how `ThermostatMatterState` stores
temperature as `AtomicI16` hundredths-of-a-degree — same "native Matter
unit at rest, convert at read/write boundary" convention.

**`ComelitDehumidifierOnOffHandler`** implements `ClusterAsyncHandler`
for the `on_off` cluster **directly** (not via the `OnOffHooks` /
`OnOffHandler::new_standalone` wrapper `ComelitOnOffHooks` uses in
`matter/src/light.rs`). This is a deliberate deviation from the originally
discussed "pattern ricalcato da `ComelitOnOffHooks`": `OnOffHooks::
set_on_off` is synchronous and cannot report failure, whereas
`ClusterAsyncHandler::handle_on`/`handle_off` are `async fn ... ->
Result<(), Error>` and can `.await` the real hub outcome — the same
direct-`ClusterAsyncHandler` approach already used by
`ComelitThermostatHandler` and `ComelitCoveringHandler`. Since this whole
feature line exists to stop reporting synthetic success, the on/off
control for the dehumidifier gets the same treatment:

```rust
impl ClusterAsyncHandler for ComelitDehumidifierOnOffHandler {
    const CLUSTER: Cluster<'static> = on_off_cluster::FULL_CLUSTER
        .with_revision(6)
        // No LIGHTING/DEAD_FRONT_BEHAVIOR/OFF_ONLY features — plain on/off.
        .with_attrs(clusters::attrs!(on_off_cluster::AttributeId::OnOff))
        .with_cmds(clusters::cmds!(
            on_off_cluster::CommandId::Off,
            on_off_cluster::CommandId::On,
            on_off_cluster::CommandId::Toggle,
        ));

    async fn on_off(&self, _ctx: impl ReadContext) -> Result<bool, Error> {
        Ok(self.state.active.load(Ordering::Acquire))
    }

    async fn handle_on(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        self.state.handle.set_dehumidifier_active(true).await
            .map_err(|_| ErrorCode::Failure.into())?;
        Ok(())
    }

    async fn handle_off(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        self.state.handle.set_dehumidifier_active(false).await
            .map_err(|_| ErrorCode::Failure.into())?;
        Ok(())
    }

    async fn handle_toggle(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        let next = !self.state.active.load(Ordering::Acquire);
        self.state.handle.set_dehumidifier_active(next).await
            .map_err(|_| ErrorCode::Failure.into())?;
        Ok(())
    }
}
```

(`global_scene_control`, `on_time`, `off_wait_time`, `start_up_on_off` and
their setters, `handle_off_with_effect`,
`handle_on_with_recall_global_scene`, `handle_on_with_timed_off` stay on
the trait's defaults — `AttributeNotFound` / no-op — since none are
declared in `CLUSTER` and Comelit has no equivalent concept, matching how
`ComelitThermostatHandler` leaves the schedule/preset/atomic-request
methods on their defaults.)

**`ComelitHumidityMeasurementHandler`** implements `ClusterAsyncHandler`
for `relative_humidity_measurement`, read-only:

```rust
impl ClusterAsyncHandler for ComelitHumidityMeasurementHandler {
    const CLUSTER: Cluster<'static> = relative_humidity_measurement_cluster::FULL_CLUSTER
        .with_revision(3);

    async fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(self.state.humidity.load(Ordering::Acquire)))
    }
    async fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(0))
    }
    async fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(10000)) // 100.00%
    }
}
```

Both handlers hold an `Arc<DehumidifierMatterState>` and override `run()`
to push subscription updates on `state.signal.wait()`, the same override
`ComelitThermostatHandler::run` and `ComelitCoveringHandler::run` already
apply — without it, subscribers only see changes on their max-interval
sweep instead of promptly.

**`DehumidifierMatterSink`** (implements `HumiditySink`) mirrors
`ThermostatMatterSink`: on `update(HumidityState)`, stores
`state.active` from `dehumidifier_active` and `state.humidity` from
`humidity * 100.0` rounded to `u16`, then signals both cluster handlers'
subscriptions via `state.signal.signal(())`.

**`MultiDehumidifierObserver`** mirrors `MultiThermostatObserver`:
receives every `HomeDeviceData` push, and for a `Thermostat` variant whose
`id` matches one of its tracked states, forwards to that state's
`handle.mqtt_push(HumidityState::from(&data))`.

### `BridgedEntry` and dispatch

`matter/src/bridge.rs` gets a fourth variant:

```rust
pub enum BridgedEntry {
    Light(LightEntry),
    WindowCovering(CoveringEntry),
    Thermostat(ThermostatEntry),
    Dehumidifier(DehumidifierEntry),
}
```

`ep_id()`, `BridgeMetadata::new` (endpoint/cluster list construction), and
`ComelitBridgeHandler::{read, write, invoke, bump_dataver, run}` each gain
a `Dehumidifier(entry) => ...` arm dispatching to both `entry.on_off` and
`entry.humidity` by cluster ID, the same way `ThermostatEntry`'s single
`thermostat` handler is matched today extended to two handlers per entry
(matching how a `LightEntry`'s `on_off` and hypothetical additional
cluster would coexist — here concretely needed since this is the first
multi-application-cluster bridged entry type).

### Discovery and wiring (`matter/src/main.rs`)

In `run_bridge`, after the existing thermostat discovery block, add a
dehumidifier discovery pass over the same `index`:

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

Endpoint IDs continue the existing simple sequential counter (lights, then
coverings, then thermostats, then — new — dehumidifiers), each
dehumidifier getting the *next* `ep_id` after all thermostats, as its own
distinct bridged endpoint (not reusing its parent thermostat's `ep_id`).
Subscribing to MQTT push for a dehumidifier's `device_id` is **not**
duplicated: the underlying Comelit thermostat is already subscribed by
the thermostat-discovery block (`th.id` is identical for both — it's the
same physical device, just observed by two different Matter endpoints).
`MultiDehumidifierObserver` is added to the existing `FanOutObserver`
alongside `light`/`covering`/`thermostat`.

`run_matter` builds one `BridgedEntry::Dehumidifier` per discovered
dehumidifier the same way it currently builds `BridgedEntry::Thermostat`,
calling `comelit_client_rs::humidity::spawn_humidity_worker` and wiring
`DehumidifierMatterSink` via `handle.set_sink(..)` before appending to
`entries`.

## Error Handling

- `HumidityHandle::set_target_humidity` / `set_dehumidifier_active`
  propagate the real `anyhow::Result<()>` from the underlying
  `ComelitClient` call (including the retry loop already built into
  `send_action`) back through the `oneshot` reply — no fire-and-forget.
- On the Matter side, a failed hub call maps to `ErrorCode::Failure`,
  returned from `handle_on`/`handle_off`/`handle_toggle` — the controller
  sees a failed `InvokeResponse`, not a false "success" with no
  corresponding device change (matching the same principle just applied to
  HAP).
- If a `HomeDeviceData::Thermostat` push arrives for a device id with no
  matching `MultiDehumidifierObserver` state (a thermostat without the
  dehumidifier sub-type), the observer's existing per-state id match
  naturally skips it — no special-casing needed.

## Testing

- Unit tests for `client/src/humidity/worker.rs`, following the existing
  `ThermostatCommand`/`ThermostatWorker` test shape in
  `client/src/thermostat/worker.rs`: a fake `ComelitClient` (or its
  existing test double) exercising `set_target_humidity` /
  `set_dehumidifier_active` success and failure paths, asserting on the
  returned `Result` rather than sleeping and inspecting side effects.
- Unit tests for the `HumidityState::from(&ThermostatDeviceData)`
  conversion, moved with the type.
- Unit tests for `celsius_to_matter`-style unit conversion helpers
  introduced for humidity (percent ⇄ hundredths-of-a-percent), matching
  the existing `celsius_to_matter`/`matter_to_celsius` test coverage in
  `matter/src/thermostat.rs`.
- `ComelitDehumidifierOnOffHandler` / `ComelitHumidityMeasurementHandler`:
  unit tests around `on_off`, `handle_on`/`handle_off`/`handle_toggle`,
  and `measured_value`, following the existing test patterns in
  `matter/src/thermostat.rs`'s `test` module (constructing a state with a
  worker driven by a fake client, invoking the handler methods, and
  asserting both the returned `Result` and the resulting state).
- No new integration/manual-QA steps beyond what the thermostat design
  already called for (pairing to a real Matter controller and confirming
  behavior) — this reuses the same bridge machinery.
