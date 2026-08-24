// matter/src/thermostat.rs
use std::sync::Arc;
use std::sync::atomic::{AtomicI16, AtomicU8, Ordering};

use async_trait::async_trait;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use log::info;

use rs_matter::dm::Cluster;
use rs_matter::dm::clusters::decl::thermostat::{
    self as thermostat_cluster, AddThermostatSuggestionRequest,
    AddThermostatSuggestionResponseBuilder, AtomicRequestRequest, AtomicResponseBuilder,
    ClusterAsyncHandler, ControlSequenceOfOperationEnum, GetWeeklyScheduleRequest,
    GetWeeklyScheduleResponseBuilder, RemoveThermostatSuggestionRequest,
    SetActivePresetRequestRequest, SetActiveScheduleRequestRequest, SetWeeklyScheduleRequest,
    SetpointRaiseLowerModeEnum, SetpointRaiseLowerRequest, SystemModeEnum,
};
use rs_matter::dm::{Dataver, HandlerContext, InvokeContext, ReadContext, WriteContext};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{Nullable, TLVBuilderParent};
use rs_matter::with;

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
    pub fn new(
        ep_id: u16,
        device_id: String,
        initial: ThermostatState,
        handle: ThermostatHandle,
    ) -> Self {
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

/// Publishes worker state updates into the shared `ThermostatMatterState` and
/// wakes any pending Matter subscription poll via `Signal`.
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
        self.state
            .temperature
            .store(celsius_to_matter(state.temperature), Ordering::Relaxed);
        self.state.target_temperature.store(
            celsius_to_matter(state.target_temperature),
            Ordering::Relaxed,
        );
        self.state
            .system_mode
            .store(state.heating_cooling_state as u8, Ordering::Relaxed);
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
/// it's reduced to `Heat`. The `AUTO_MODE` feature is deliberately not
/// declared in `CLUSTER`, so advertising `SystemModeEnum::Auto` would be a
/// spec violation as well as semantically wrong (Comelit has a single
/// setpoint, not a heat/cool pair with a deadband).
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
        // silently picking a season. Same for the fan/dry/sleep modes, which
        // this cluster never advertises support for.
        _ => TargetHeatingCoolingState::Off,
    }
}

/// Implements the `Thermostat` cluster for one bridged Comelit thermostat.
/// The `AUTO_MODE` feature is deliberately NOT declared: only `HEATING |
/// COOLING`. Both `OccupiedHeatingSetpoint` and `OccupiedCoolingSetpoint`
/// mirror the same `target_temperature`, since Comelit has only one target.
pub struct ComelitThermostatHandler {
    dataver: Dataver,
    state: Arc<ThermostatMatterState>,
}

impl ComelitThermostatHandler {
    pub fn new(dataver: Dataver, state: Arc<ThermostatMatterState>) -> Self {
        Self { dataver, state }
    }
}

const ABS_MIN_SETPOINT: i16 = 700; // 7.00 C
const ABS_MAX_SETPOINT: i16 = 3500; // 35.00 C

impl ClusterAsyncHandler for ComelitThermostatHandler {
    const CLUSTER: Cluster<'static> = thermostat_cluster::FULL_CLUSTER
        .with_revision(9)
        .with_features(
            thermostat_cluster::Feature::HEATING.bits()
                | thermostat_cluster::Feature::COOLING.bits(),
        )
        // `required;` already covers LocalTemperature / ControlSequenceOfOperation
        // / SystemMode (the only non-optional attributes in FULL_CLUSTER); the
        // rest listed here are `Quality::O` in the base cluster but become
        // mandatory once HEATING / COOLING are declared, or are ones we simply
        // choose to expose.
        .with_attrs(with!(
            required;
            thermostat_cluster::AttributeId::OccupiedHeatingSetpoint
                | thermostat_cluster::AttributeId::OccupiedCoolingSetpoint
                | thermostat_cluster::AttributeId::AbsMinHeatSetpointLimit
                | thermostat_cluster::AttributeId::AbsMaxHeatSetpointLimit
                | thermostat_cluster::AttributeId::AbsMinCoolSetpointLimit
                | thermostat_cluster::AttributeId::AbsMaxCoolSetpointLimit
        ))
        // Only SetpointRaiseLower is supported; the schedule/preset/atomic
        // commands are never routed to this handler because they are not
        // declared here (their trait stubs below exist purely to satisfy Rust).
        .with_cmds(with!(thermostat_cluster::CommandId::SetpointRaiseLower));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    /// Pushes attribute changes to Matter subscriptions as soon as the worker
    /// publishes new state (mirrors `ComelitCoveringHandler::run`). Without
    /// this override the trait default is `pending()`, and subscribers would
    /// only see updates on their max-interval sweep.
    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        loop {
            self.state.signal.wait().await;
            ctx.notify_cluster_changed(self.state.ep_id, Self::CLUSTER.id);
        }
    }

    async fn local_temperature(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::some(
            self.state.temperature.load(Ordering::Relaxed),
        ))
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

    async fn set_system_mode(
        &self,
        _ctx: impl WriteContext,
        value: SystemModeEnum,
    ) -> Result<(), Error> {
        self.state
            .handle
            .set_hvac_mode(from_system_mode(value))
            .await;
        Ok(())
    }

    async fn occupied_heating_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.state.target_temperature.load(Ordering::Relaxed))
    }

    async fn set_occupied_heating_setpoint(
        &self,
        _ctx: impl WriteContext,
        value: i16,
    ) -> Result<(), Error> {
        self.state
            .handle
            .set_target_temperature(matter_to_celsius(value))
            .await;
        Ok(())
    }

    async fn occupied_cooling_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.state.target_temperature.load(Ordering::Relaxed))
    }

    async fn set_occupied_cooling_setpoint(
        &self,
        _ctx: impl WriteContext,
        value: i16,
    ) -> Result<(), Error> {
        self.state
            .handle
            .set_target_temperature(matter_to_celsius(value))
            .await;
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
        // Comelit has a single setpoint, so Heat/Cool/Both all move the same
        // value; the field is still parsed so a malformed request is rejected.
        let _mode: SetpointRaiseLowerModeEnum = request.mode()?;
        // Per the Matter spec `Amount` is a signed delta in *tenths* of a
        // degree Celsius, while our stored setpoint is in hundredths.
        let amount = request.amount()?;
        let current = self.state.target_temperature.load(Ordering::Relaxed);
        let delta_hundredths = i16::from(amount) * 10;
        let new_value = current
            .saturating_add(delta_hundredths)
            .clamp(ABS_MIN_SETPOINT, ABS_MAX_SETPOINT);
        self.state
            .handle
            .set_target_temperature(matter_to_celsius(new_value))
            .await;
        Ok(())
    }

    // The remaining 8 commands are mandatory in the generated trait but are
    // never declared in CLUSTER's command list above, so the framework never
    // routes them here — these bodies exist only to satisfy the trait.

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
        _request: AtomicRequestRequest<'_>,
        _response: AtomicResponseBuilder<P>,
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn celsius_round_trips_through_hundredths() {
        assert_eq!(celsius_to_matter(21.5), 2150);
        assert_eq!(celsius_to_matter(7.0), 700);
        assert_eq!(matter_to_celsius(2150), 21.5);
        assert_eq!(matter_to_celsius(700), 7.0);
    }

    #[test]
    fn auto_is_reduced_to_heat_and_never_exposed_as_matter_auto() {
        assert_eq!(
            to_system_mode(TargetHeatingCoolingState::Auto),
            SystemModeEnum::Heat
        );
        assert_eq!(
            to_system_mode(TargetHeatingCoolingState::Heat),
            SystemModeEnum::Heat
        );
        assert_eq!(
            to_system_mode(TargetHeatingCoolingState::Cool),
            SystemModeEnum::Cool
        );
        assert_eq!(
            to_system_mode(TargetHeatingCoolingState::Off),
            SystemModeEnum::Off
        );
    }

    #[test]
    fn unsupported_matter_modes_fall_back_to_off() {
        assert_eq!(
            from_system_mode(SystemModeEnum::Auto),
            TargetHeatingCoolingState::Off
        );
        assert_eq!(
            from_system_mode(SystemModeEnum::FanOnly),
            TargetHeatingCoolingState::Off
        );
        assert_eq!(
            from_system_mode(SystemModeEnum::EmergencyHeat),
            TargetHeatingCoolingState::Heat
        );
        assert_eq!(
            from_system_mode(SystemModeEnum::Precooling),
            TargetHeatingCoolingState::Cool
        );
    }

    #[test]
    fn hvac_state_encoding_round_trips() {
        for state in [
            TargetHeatingCoolingState::Off,
            TargetHeatingCoolingState::Heat,
            TargetHeatingCoolingState::Cool,
            TargetHeatingCoolingState::Auto,
        ] {
            assert_eq!(decode_hvac_state(state as u8), state);
        }
    }
}
