// matter/src/dehumidifier.rs
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use async_trait::async_trait;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use log::info;

use rs_matter::dm::Cluster;
use rs_matter::dm::clusters::decl::on_off::{self as on_off_cluster, OffWithEffectRequest, OnWithTimedOffRequest};
use rs_matter::dm::clusters::decl::relative_humidity_measurement::{
    self as humidity_cluster, ClusterAsyncHandler as _,
};
use rs_matter::dm::{Dataver, HandlerContext, InvokeContext, ReadContext};
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

    // The remaining 3 commands are mandatory in the generated trait but are
    // never declared in CLUSTER's command list above, so the framework never
    // routes them here — these bodies exist only to satisfy the trait (same
    // pattern as `ComelitThermostatHandler`'s unsupported-schedule/preset
    // stubs in thermostat.rs).

    async fn handle_off_with_effect(
        &self,
        _ctx: impl InvokeContext,
        _request: OffWithEffectRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_on_with_recall_global_scene(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    async fn handle_on_with_timed_off(
        &self,
        _ctx: impl InvokeContext,
        _request: OnWithTimedOffRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
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

    fn matter_to_percent(hundredths: u16) -> f32 {
        hundredths as f32 / 100.0
    }

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
