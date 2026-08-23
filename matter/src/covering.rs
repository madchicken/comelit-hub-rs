// matter/src/covering.rs
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use async_trait::async_trait;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use log::info;

use rs_matter::dm::Cluster;
use rs_matter::dm::clusters::decl::window_covering::{
    self as covering_cluster, ClusterAsyncHandler, ConfigStatus, EndProductType,
    GoToLiftPercentageRequest, GoToLiftValueRequest, GoToTiltPercentageRequest,
    GoToTiltValueRequest, Mode, OperationalStatus, SafetyStatus, Type,
};
use rs_matter::dm::{Dataver, HandlerContext, InvokeContext, ReadContext, WriteContext};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::Nullable;
use rs_matter::with;

use comelit_client_rs::covering::{
    FULLY_CLOSED, FULLY_OPENED, PositionState, WindowCoveringHandle, WindowCoveringSink,
    WindowCoveringState,
};
use comelit_client_rs::{HomeDeviceData, StatusUpdate};

/// State shared between the Matter cluster handler and the Comelit worker,
/// mirroring `light::LightState`.
pub struct CoveringState {
    pub ep_id: u16,
    pub device_id: String,
    /// Comelit/HomeKit convention: 0 = fully closed, 100 = fully open.
    pub current_position: AtomicU8,
    /// Comelit/HomeKit convention: 0 = fully closed, 100 = fully open.
    pub target_position: AtomicU8,
    /// Encoded as `PositionState as u8` (0 = MovingDown, 1 = MovingUp, 2 = Stopped).
    pub position_state: AtomicU8,
    pub signal: Signal<CriticalSectionRawMutex, ()>,
    pub handle: WindowCoveringHandle,
}

impl CoveringState {
    pub fn new(
        ep_id: u16,
        device_id: String,
        initial: WindowCoveringState,
        handle: WindowCoveringHandle,
    ) -> Self {
        Self {
            ep_id,
            device_id,
            current_position: AtomicU8::new(initial.current_position),
            target_position: AtomicU8::new(initial.target_position),
            position_state: AtomicU8::new(initial.position_state as u8),
            signal: Signal::new(),
            handle,
        }
    }
}

/// Publishes worker position updates into the shared `CoveringState` and
/// wakes any pending Matter subscription poll via `Signal`.
pub struct MatterCoveringSink {
    state: Arc<CoveringState>,
}

impl MatterCoveringSink {
    pub fn new(state: Arc<CoveringState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl WindowCoveringSink for MatterCoveringSink {
    async fn update(&self, state: WindowCoveringState) {
        self.state
            .current_position
            .store(state.current_position, Ordering::Relaxed);
        self.state
            .target_position
            .store(state.target_position, Ordering::Relaxed);
        self.state
            .position_state
            .store(state.position_state as u8, Ordering::Relaxed);
        self.state.signal.signal(());
        info!(
            "MQTT → Matter ep{}: {} pos={} target={}",
            self.state.ep_id, self.state.device_id, state.current_position, state.target_position
        );
    }
}

fn decode_position_state(raw: u8) -> PositionState {
    match raw {
        0 => PositionState::MovingDown,
        1 => PositionState::MovingUp,
        _ => PositionState::Stopped,
    }
}

/// Matter expresses lift positions as "percent closed": `0` = fully open,
/// `100`/`10000` = fully closed (the position between `InstalledOpenLimitLift`
/// and `InstalledClosedLimitLift`). The Comelit/HomeKit convention used by
/// `comelit_client_rs::covering` is the opposite (`FULLY_OPENED` = 100,
/// `FULLY_CLOSED` = 0), so every lift position crossing this boundary has to
/// be inverted.
fn comelit_to_matter_percent(comelit: u8) -> u8 {
    FULLY_OPENED.saturating_sub(comelit.min(FULLY_OPENED))
}

fn comelit_to_matter_percent_100ths(comelit: u8) -> u16 {
    comelit_to_matter_percent(comelit) as u16 * 100
}

fn matter_percent_100ths_to_comelit(matter: u16) -> u8 {
    let matter_percent = (matter / 100).min(FULLY_OPENED as u16) as u8;
    FULLY_OPENED - matter_percent
}

/// `OperationalStatus` is a bitmask: bits 0-1 = Global, bits 2-3 = Lift,
/// bits 4-5 = Tilt. Each 2-bit field encodes 0=Stopped, 1=Opening, 2=Closing.
/// We only drive Lift, and mirror the same value onto Global since we don't
/// support Tilt.
fn operational_status(position_state: PositionState) -> OperationalStatus {
    let field: u8 = match position_state {
        PositionState::MovingUp => 1,   // Opening
        PositionState::MovingDown => 2, // Closing
        PositionState::Stopped => 0,
    };
    OperationalStatus::from_bits_truncate(field | (field << 2))
}

/// Implements the `WindowCovering` cluster for one bridged Comelit blind.
/// `rs-matter` has no hand-written "hooks" module for this cluster (unlike
/// `on_off`), so every method of the generated `ClusterAsyncHandler` trait is
/// implemented directly here. Tilt is not supported (Comelit blinds report
/// no tilt data), so tilt-related attributes return fixed/absent values and
/// the `Tilt` feature bit is not set in `CLUSTER`.
pub struct ComelitCoveringHandler {
    dataver: Dataver,
    state: Arc<CoveringState>,
}

impl ComelitCoveringHandler {
    pub fn new(dataver: Dataver, state: Arc<CoveringState>) -> Self {
        Self { dataver, state }
    }
}

impl ClusterAsyncHandler for ComelitCoveringHandler {
    const CLUSTER: Cluster<'static> = covering_cluster::FULL_CLUSTER
        .with_revision(5)
        .with_features(
            covering_cluster::Feature::LIFT.bits()
                | covering_cluster::Feature::POSITION_AWARE_LIFT.bits(),
        )
        .with_attrs(with!(
            required;
            covering_cluster::AttributeId::Type
                | covering_cluster::AttributeId::ConfigStatus
                | covering_cluster::AttributeId::CurrentPositionLiftPercentage
                | covering_cluster::AttributeId::CurrentPositionLiftPercent100ths
                | covering_cluster::AttributeId::TargetPositionLiftPercent100ths
                | covering_cluster::AttributeId::OperationalStatus
                | covering_cluster::AttributeId::EndProductType
                | covering_cluster::AttributeId::Mode
        ))
        .with_cmds(with!(
            covering_cluster::CommandId::UpOrOpen
                | covering_cluster::CommandId::DownOrClose
                | covering_cluster::CommandId::StopMotion
                | covering_cluster::CommandId::GoToLiftPercentage
        ));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    /// Pushes attribute changes to Matter subscriptions as soon as the worker
    /// publishes a new position (mirrors `ComelitOnOffHooks::run`). Without
    /// this, subscribers would only see updates on their max-interval sweep.
    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        loop {
            self.state.signal.wait().await;
            ctx.notify_cluster_changed(self.state.ep_id, Self::CLUSTER.id);
        }
    }

    async fn r#type(&self, _ctx: impl ReadContext) -> Result<Type, Error> {
        // `Type` has no `RollerShutter` variant; per the Matter spec's
        // Type/EndProductType mapping, `EndProductType::RollerShutter`
        // belongs to the `Type::RollerShade` family.
        Ok(Type::RollerShade)
    }

    async fn end_product_type(&self, _ctx: impl ReadContext) -> Result<EndProductType, Error> {
        Ok(EndProductType::RollerShutter)
    }

    async fn config_status(&self, _ctx: impl ReadContext) -> Result<ConfigStatus, Error> {
        Ok(ConfigStatus::OPERATIONAL | ConfigStatus::LIFT_POSITION_AWARE)
    }

    async fn current_position_lift_percentage(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<Nullable<rs_matter::im::Percent>, Error> {
        Ok(Nullable::some(comelit_to_matter_percent(
            self.state.current_position.load(Ordering::Relaxed),
        )))
    }

    async fn current_position_lift_percent_100_ths(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<Nullable<rs_matter::im::Percent100ths>, Error> {
        Ok(Nullable::some(comelit_to_matter_percent_100ths(
            self.state.current_position.load(Ordering::Relaxed),
        )))
    }

    async fn target_position_lift_percent_100_ths(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<Nullable<rs_matter::im::Percent100ths>, Error> {
        Ok(Nullable::some(comelit_to_matter_percent_100ths(
            self.state.target_position.load(Ordering::Relaxed),
        )))
    }

    async fn operational_status(&self, _ctx: impl ReadContext) -> Result<OperationalStatus, Error> {
        let raw = self.state.position_state.load(Ordering::Relaxed);
        Ok(operational_status(decode_position_state(raw)))
    }

    async fn mode(&self, _ctx: impl ReadContext) -> Result<Mode, Error> {
        Ok(Mode::empty())
    }

    async fn set_mode(&self, _ctx: impl WriteContext, _value: Mode) -> Result<(), Error> {
        Ok(())
    }

    async fn safety_status(&self, _ctx: impl ReadContext) -> Result<SafetyStatus, Error> {
        Ok(SafetyStatus::empty())
    }

    async fn physical_closed_limit_lift(&self, _ctx: impl ReadContext) -> Result<u16, Error> {
        Ok(10000)
    }

    async fn physical_closed_limit_tilt(&self, _ctx: impl ReadContext) -> Result<u16, Error> {
        Ok(0)
    }

    async fn current_position_lift(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(comelit_to_matter_percent_100ths(
            self.state.current_position.load(Ordering::Relaxed),
        )))
    }

    async fn current_position_tilt(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::none())
    }

    async fn number_of_actuations_lift(&self, _ctx: impl ReadContext) -> Result<u16, Error> {
        Ok(0)
    }

    async fn number_of_actuations_tilt(&self, _ctx: impl ReadContext) -> Result<u16, Error> {
        Ok(0)
    }

    async fn current_position_tilt_percentage(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<Nullable<rs_matter::im::Percent>, Error> {
        Ok(Nullable::none())
    }

    async fn target_position_tilt_percent_100_ths(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<Nullable<rs_matter::im::Percent100ths>, Error> {
        Ok(Nullable::none())
    }

    async fn current_position_tilt_percent_100_ths(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<Nullable<rs_matter::im::Percent100ths>, Error> {
        Ok(Nullable::none())
    }

    async fn installed_open_limit_lift(&self, _ctx: impl ReadContext) -> Result<u16, Error> {
        Ok(0)
    }

    async fn installed_closed_limit_lift(&self, _ctx: impl ReadContext) -> Result<u16, Error> {
        Ok(10000)
    }

    async fn installed_open_limit_tilt(&self, _ctx: impl ReadContext) -> Result<u16, Error> {
        Ok(0)
    }

    async fn installed_closed_limit_tilt(&self, _ctx: impl ReadContext) -> Result<u16, Error> {
        Ok(0)
    }

    async fn handle_up_or_open(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        let current = self.state.current_position.load(Ordering::Relaxed);
        self.state.handle.move_to(current, FULLY_OPENED).await;
        Ok(())
    }

    async fn handle_down_or_close(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        let current = self.state.current_position.load(Ordering::Relaxed);
        self.state.handle.move_to(current, FULLY_CLOSED).await;
        Ok(())
    }

    async fn handle_stop_motion(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        self.state.handle.stop().await;
        Ok(())
    }

    async fn handle_go_to_lift_value(
        &self,
        _ctx: impl InvokeContext,
        _request: GoToLiftValueRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::InvalidCommand.into())
    }

    async fn handle_go_to_lift_percentage(
        &self,
        _ctx: impl InvokeContext,
        request: GoToLiftPercentageRequest<'_>,
    ) -> Result<(), Error> {
        let percent_100ths = request.lift_percent_100_ths_value()?;
        let target = matter_percent_100ths_to_comelit(percent_100ths);
        let current = self.state.current_position.load(Ordering::Relaxed);
        self.state.handle.move_to(current, target).await;
        Ok(())
    }

    async fn handle_go_to_tilt_value(
        &self,
        _ctx: impl InvokeContext,
        _request: GoToTiltValueRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::InvalidCommand.into())
    }

    async fn handle_go_to_tilt_percentage(
        &self,
        _ctx: impl InvokeContext,
        _request: GoToTiltPercentageRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::InvalidCommand.into())
    }
}

/// Receives MQTT push-updates for all bridged window coverings and forwards
/// them to the matching worker via its `WindowCoveringHandle`.
pub struct MultiCoveringObserver {
    pub states: Vec<Arc<CoveringState>>,
}

#[async_trait]
impl StatusUpdate for MultiCoveringObserver {
    async fn status_update(&self, device: &HomeDeviceData) {
        if let HomeDeviceData::WindowCovering(data) = device {
            if let Some(state) = self.states.iter().find(|s| s.device_id == data.id) {
                let new_state = WindowCoveringState::from(data);
                state.handle.status_update(new_state).await;
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn lift_percent_conversion_is_inverted() {
        // Comelit 100 (fully open) -> Matter 0 (fully open).
        assert_eq!(comelit_to_matter_percent(FULLY_OPENED), 0);
        assert_eq!(comelit_to_matter_percent_100ths(FULLY_OPENED), 0);
        // Comelit 0 (fully closed) -> Matter 10000 (fully closed).
        assert_eq!(comelit_to_matter_percent(FULLY_CLOSED), 100);
        assert_eq!(comelit_to_matter_percent_100ths(FULLY_CLOSED), 10000);
        assert_eq!(comelit_to_matter_percent_100ths(30), 7000);

        assert_eq!(matter_percent_100ths_to_comelit(0), FULLY_OPENED);
        assert_eq!(matter_percent_100ths_to_comelit(10000), FULLY_CLOSED);
        assert_eq!(matter_percent_100ths_to_comelit(7000), 30);
        // Out-of-range input is clamped, never panics.
        assert_eq!(matter_percent_100ths_to_comelit(u16::MAX), FULLY_CLOSED);
    }

    #[test]
    fn operational_status_mirrors_lift_onto_global() {
        assert_eq!(operational_status(PositionState::Stopped).bits(), 0b0000);
        assert_eq!(operational_status(PositionState::MovingUp).bits(), 0b0101);
        assert_eq!(operational_status(PositionState::MovingDown).bits(), 0b1010);
    }
}
