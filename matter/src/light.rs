use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use log::info;
use tokio::sync::mpsc::UnboundedSender;

use rs_matter::dm::Cluster;
use rs_matter::dm::clusters::app::on_off::{EffectVariantEnum, OnOffHooks, OutOfBandMessage};
use rs_matter::dm::clusters::decl::on_off as on_off_cluster;
use rs_matter::dm::clusters::decl::on_off::Feature;
use rs_matter::error::Error;
use rs_matter::tlv::Nullable;
use rs_matter::with;

use async_trait::async_trait;
use comelit_client_rs::{DeviceStatus, HomeDeviceData, StatusUpdate};

/// Command sent from the Matter OnOffHooks to the tokio MQTT executor.
pub struct MqttCommand {
    pub device_id: String,
    pub on: bool,
}

/// State shared between the Matter server and the Comelit MQTT client.
pub struct LightState {
    pub ep_id: u16,
    pub device_id: String,
    pub on: AtomicBool,
    /// Signal fired when the MQTT client receives a state update for this light.
    pub signal: Signal<CriticalSectionRawMutex, ()>,
    /// Channel to request a toggle via MQTT (from Matter → tokio).
    pub cmd_tx: UnboundedSender<MqttCommand>,
}

impl LightState {
    pub fn new(
        ep_id: u16,
        device_id: String,
        initial_on: bool,
        cmd_tx: UnboundedSender<MqttCommand>,
    ) -> Self {
        Self {
            ep_id,
            device_id,
            on: AtomicBool::new(initial_on),
            signal: Signal::new(),
            cmd_tx,
        }
    }
}

/// Implements `OnOffHooks` for a single Comelit light, connecting the Matter
/// cluster to the MQTT client over two shared-state channels:
///  - Reads `on` from an `AtomicBool` updated by the MQTT observer.
///  - Writes to `cmd_tx` (sync) so a tokio task calls `toggle_device_status`.
///  - `run()` loops on `signal.wait()` and fires `notify(Update)` whenever the
///    MQTT observer changes the state.
pub struct ComelitOnOffHooks {
    state: Arc<LightState>,
}

impl ComelitOnOffHooks {
    pub fn new(state: Arc<LightState>) -> Self {
        Self { state }
    }
}

impl OnOffHooks for ComelitOnOffHooks {
    const CLUSTER: Cluster<'static> = on_off_cluster::FULL_CLUSTER
        .with_revision(6)
        .with_features(Feature::LIGHTING.bits())
        .with_attrs(with!(
            required;
            on_off_cluster::AttributeId::OnOff
                | on_off_cluster::AttributeId::GlobalSceneControl
                | on_off_cluster::AttributeId::OnTime
                | on_off_cluster::AttributeId::OffWaitTime
                | on_off_cluster::AttributeId::StartUpOnOff
        ))
        .with_cmds(with!(
            on_off_cluster::CommandId::Off
                | on_off_cluster::CommandId::On
                | on_off_cluster::CommandId::Toggle
                | on_off_cluster::CommandId::OffWithEffect
                | on_off_cluster::CommandId::OnWithRecallGlobalScene
                | on_off_cluster::CommandId::OnWithTimedOff
        ));

    fn on_off(&self) -> bool {
        self.state.on.load(Ordering::Relaxed)
    }

    fn set_on_off(&self, on: bool) {
        self.state.on.store(on, Ordering::Relaxed);
        let _ = self.state.cmd_tx.send(MqttCommand {
            device_id: self.state.device_id.clone(),
            on,
        });
        info!(
            "Matter → MQTT: {} {}",
            self.state.device_id,
            if on { "ON" } else { "OFF" }
        );
    }

    fn start_up_on_off(&self) -> Nullable<rs_matter::dm::clusters::app::on_off::StartUpOnOffEnum> {
        Nullable::none()
    }

    fn set_start_up_on_off(
        &self,
        _value: Nullable<rs_matter::dm::clusters::app::on_off::StartUpOnOffEnum>,
    ) -> Result<(), Error> {
        Ok(())
    }

    async fn handle_off_with_effect(&self, _effect: EffectVariantEnum) {
        self.set_on_off(false);
    }

    async fn run<F: Fn(OutOfBandMessage)>(&self, notify: F) {
        loop {
            self.state.signal.wait().await;
            notify(OutOfBandMessage::Update);
        }
    }
}

/// Receives MQTT push-updates for all bridged lights and propagates them to their `LightState`.
pub struct MultiLightObserver {
    pub states: Vec<Arc<LightState>>,
}

#[async_trait]
impl StatusUpdate for MultiLightObserver {
    async fn status_update(&self, device: &HomeDeviceData) {
        if let HomeDeviceData::Light(data) = device {
            if let Some(state) = self.states.iter().find(|s| s.device_id == data.id) {
                let is_on = data.status.as_ref().map(|s| s == &DeviceStatus::On).unwrap_or(false);
                let was_on = state.on.swap(is_on, Ordering::AcqRel);
                if was_on != is_on {
                    info!(
                        "MQTT → Matter ep{}: {} {}",
                        state.ep_id,
                        data.id,
                        if is_on { "ON" } else { "OFF" }
                    );
                    state.signal.signal(());
                }
            }
        }
    }
}

