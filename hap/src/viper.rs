//! Doorbell rings from the ViP door entry system.
//!
//! When `doorbell_video.enabled` is set, the bridge keeps a [`ViperUnit`]
//! registered as a subunit of the apartment: when someone rings, the ViP
//! system calls it and the HomeKit doorbell rings, instead of relying on the
//! Comelit hub MQTT event. The unit is also what the HomeKit camera takes its
//! video from (`crate::doorbell_camera`), and what opens the hub doors of
//! the ViP system while it is in a call with them ([`DoorOpener`]). The
//! connection is re-established when it drops.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use viper_client::video::ctp::LogAddr;
use viper_client::video::{DoorOpening, UnitEvent, VideoParams, ViperUnit};
use viper_client::{ICONA_BRIDGE_PORT, ViperClient};

use crate::accessories::ComelitDoorbellAccessory;
use crate::doorbell_camera::SnapshotSource;
use crate::settings::DoorbellVideoSettings;

const MIN_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A session that lasted this long is considered healthy: the next
/// reconnection starts again from the minimum delay.
const HEALTHY_SESSION: Duration = Duration::from_secs(120);

/// The connected ViP unit, shared with the HomeKit camera and the doors.
pub type UnitSlot = Arc<RwLock<Option<ViperUnit>>>;

/// Time for the external unit to end the call we released, before the hub
/// sends it the door opening.
const CALL_RELEASE_WAIT: Duration = Duration::from_secs(1);

/// Opens a hub door of the ViP system on our call with its external unit.
/// While the HomeKit camera shows it (the Home app asks a snapshot every 10
/// seconds), that unit is in a call with us and ignores the door opening
/// the hub sends it.
#[derive(Clone)]
pub struct DoorOpener {
    slot: UnitSlot,
    address: LogAddr,
    output: u8,
}

impl DoorOpener {
    /// For a hub door `VIP#OD#<address>.<output>`; `None` for other doors.
    pub fn for_hub_door(slot: UnitSlot, id: &str) -> Option<DoorOpener> {
        let (address, output) = id.strip_prefix("VIP#OD#")?.split_once('.')?;
        Some(DoorOpener {
            slot,
            address: LogAddr::new(address).ok()?,
            output: output.parse().ok()?,
        })
    }

    /// Opens the door on our call with its unit. False when the hub must
    /// open it: no call (our call is released first if needed), or no ViP
    /// connection.
    pub async fn open(&self) -> bool {
        let Some(unit) = self.slot.read().await.clone() else {
            return false;
        };
        match unit.open_door(self.address, self.output).await {
            Ok(DoorOpening::OnCall) => true,
            Ok(DoorOpening::CallReleased) => {
                tokio::time::sleep(CALL_RELEASE_WAIT).await;
                false
            }
            Ok(DoorOpening::NoCall) => false,
            Err(e) => {
                warn!("Cannot open the door through the ViP unit: {e}");
                false
            }
        }
    }
}

/// Handle on the ViP doorbell service. The task is aborted when it is
/// dropped, i.e. when the bridge restarts.
pub struct ViperDoorbell {
    task: JoinHandle<()>,
}

impl Drop for ViperDoorbell {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ViperDoorbell {
    /// Starts the service; rings are forwarded to `doorbells` and the
    /// connected unit is published in `slot`.
    pub fn spawn(
        settings: DoorbellVideoSettings,
        doorbells: Vec<Arc<Mutex<ComelitDoorbellAccessory>>>,
        slot: UnitSlot,
        snapshots: Option<Arc<SnapshotSource>>,
    ) -> Result<ViperDoorbell> {
        let token = settings
            .viper_token
            .clone()
            .ok_or_else(|| anyhow!("doorbell_video.viper_token is required"))?;
        if doorbells.is_empty() {
            warn!("ViP doorbell enabled but no doorbell accessory is mounted (mount_doorbells)");
        }
        let task = tokio::spawn(run(settings, token, doorbells, slot, snapshots));
        Ok(ViperDoorbell { task })
    }
}

async fn run(
    settings: DoorbellVideoSettings,
    token: String,
    doorbells: Vec<Arc<Mutex<ComelitDoorbellAccessory>>>,
    slot: UnitSlot,
    snapshots: Option<Arc<SnapshotSource>>,
) {
    let mut backoff = MIN_BACKOFF;
    loop {
        let started = tokio::time::Instant::now();
        match session(&settings, &token, &doorbells, &slot, snapshots.as_deref()).await {
            Ok(()) => warn!("ViP connection closed"),
            Err(e) => warn!("ViP connection failed: {e:#}"),
        }
        *slot.write().await = None;
        if started.elapsed() >= HEALTHY_SESSION {
            backoff = MIN_BACKOFF;
        }
        info!("Reconnecting to the ViP system in {} s", backoff.as_secs());
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn session(
    settings: &DoorbellVideoSettings,
    token: &str,
    doorbells: &[Arc<Mutex<ComelitDoorbellAccessory>>],
    slot: &RwLock<Option<ViperUnit>>,
    snapshots: Option<&SnapshotSource>,
) -> Result<()> {
    let (host, port) = match &settings.viper_host {
        Some(host) => (host.clone(), settings.viper_port),
        None => ViperClient::scan()
            .await
            .ok_or_else(|| anyhow!("no ViP gateway found on the network"))?,
    };
    let port = if port == 0 { ICONA_BRIDGE_PORT } else { port };
    info!("Connecting to the ViP system at {host}:{port}");
    let client = ViperClient::connect(&host, port)
        .await
        .map_err(|e| anyhow!("{e}"))?;

    // UAUT and UCFG are blocking request/response exchanges
    let token = token.to_string();
    let (client, vip) = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut client = client;
        let auth = client.authorize(&token).map_err(|e| anyhow!("{e}"))?;
        if auth.response.response_code != 200 {
            return Err(anyhow!(
                "ViP authorization refused: {} {}",
                auth.response.response_code,
                auth.response.response_string
            ));
        }
        let config = client.configuration("all").map_err(|e| anyhow!("{e}"))?;
        Ok((client, config.vip))
    })
    .await
    .context("ViP authorization task")??;

    let ring_timeout = Duration::from_secs(settings.ring_timeout_secs);
    let video = VideoParams {
        bitrate_kbps: settings.video_bitrate_kbps,
        ..VideoParams::default()
    };
    let (unit, mut events, task) = client
        .into_unit(&vip, settings.entrance.as_deref(), ring_timeout, video)
        .map_err(|e| anyhow!("{e}"))?;
    for door in &vip.user_parameters.opendoor_address_book {
        info!(
            "ViP door {:?}: {} output {}",
            door.name, door.apt_address, door.output_index
        );
    }
    *slot.write().await = Some(unit);
    info!(
        "Connected to the ViP system as {}{}",
        vip.apt_address, vip.apt_subaddress
    );

    while let Some(event) = events.recv().await {
        match event {
            UnitEvent::Registered => debug!("ViP registration renewed"),
            UnitEvent::UdpLaneOpen => debug!("ViP UDP lane open"),
            // the external unit, or the floor call button of the apartment door
            UnitEvent::Ringing { caller } => {
                info!("Ring from {caller:?}");
                if let Some(snapshots) = snapshots {
                    // the notification must show who is at the door
                    snapshots.invalidate();
                }
                for doorbell in doorbells {
                    if let Err(e) = doorbell.lock().await.ring_now().await {
                        error!("Cannot ring the HomeKit doorbell: {e:#}");
                    }
                }
            }
            UnitEvent::RingEnded => debug!("Ring ended"),
            UnitEvent::MediaEnded { reason } => debug!("Doorbell video ended: {reason}"),
        }
    }
    task.await
        .context("ViP unit task")?
        .map_err(|e| anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hub_door_names_its_unit_and_output() {
        let opener = DoorOpener::for_hub_door(UnitSlot::default(), "VIP#OD#00000100.2").unwrap();
        assert_eq!(opener.address, LogAddr::new("00000100").unwrap());
        assert_eq!(opener.output, 2);
        assert!(DoorOpener::for_hub_door(UnitSlot::default(), "DOM#LT#4.1").is_none());
        assert!(DoorOpener::for_hub_door(UnitSlot::default(), "VIP#OD#00000100").is_none());
    }
}
