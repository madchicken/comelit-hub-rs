//! Doorbell rings from the ViP door entry system.
//!
//! When `doorbell_video.enabled` is set, the bridge keeps a [`ViperUnit`]
//! registered as a subunit of the apartment: when someone rings, the ViP
//! system calls it and the HomeKit doorbell rings, instead of relying on the
//! Comelit hub MQTT event. The unit is also what the HomeKit camera takes its
//! video from (`crate::doorbell_camera`). The connection is re-established
//! when it drops.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use viper_client::video::{UnitEvent, ViperUnit};
use viper_client::{ICONA_BRIDGE_PORT, ViperClient};

use crate::accessories::ComelitDoorbellAccessory;
use crate::doorbell_camera::SnapshotSource;
use crate::settings::DoorbellVideoSettings;

const MIN_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A session that lasted this long is considered healthy: the next
/// reconnection starts again from the minimum delay.
const HEALTHY_SESSION: Duration = Duration::from_secs(120);

/// The connected ViP unit, shared with the HomeKit camera.
pub type UnitSlot = Arc<RwLock<Option<ViperUnit>>>;

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
    let (unit, mut events, task) = client
        .into_unit(&vip, settings.entrance.as_deref(), ring_timeout)
        .map_err(|e| anyhow!("{e}"))?;
    *slot.write().await = Some(unit);
    info!(
        "Connected to the ViP system as {}{}",
        vip.apt_address, vip.apt_subaddress
    );

    while let Some(event) = events.recv().await {
        match event {
            UnitEvent::Registered => debug!("ViP registration renewed"),
            UnitEvent::UdpLaneOpen => debug!("ViP UDP lane open"),
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
