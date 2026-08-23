// hap/src/accessories/window_covering.rs
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::FutureExt;
use hap::HapType;
use hap::characteristic::HapCharacteristic;
use hap::pointer::Accessory;
use hap::{
    accessory::{AccessoryInformation, window_covering::WindowCoveringAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    server::{IpServer, Server},
};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, info, warn};

use crate::accessories::ComelitAccessory;
use crate::web::metrics::Metrics;
use comelit_client_rs::covering::{
    WindowCoveringHandle, WindowCoveringSink, WindowCoveringState, spawn_window_covering_worker,
};
pub use comelit_client_rs::covering::WindowCoveringConfig;
use comelit_client_rs::{ComelitClient, WindowCoveringDeviceData};

pub(crate) struct ComelitWindowCoveringAccessory {
    id: String,
    handle: WindowCoveringHandle,
}

/// Writes worker position updates to the HomeKit accessory's characteristics.
struct HapWindowCoveringSink {
    id: String,
    accessory: Accessory,
}

#[async_trait]
impl WindowCoveringSink for HapWindowCoveringSink {
    async fn update(&self, state: WindowCoveringState) {
        let mut accessory = self.accessory.lock().await;
        let Some(service) = accessory.get_mut_service(HapType::WindowCovering) else {
            warn!(
                "WindowCovering service missing for window covering {}, dropping position update",
                self.id
            );
            return;
        };

        if let Some(characteristic) = service.get_mut_characteristic(HapType::CurrentPosition) {
            if let Err(e) = characteristic
                .update_value(Value::from(state.current_position))
                .await
            {
                warn!(
                    "update_value for window covering {} CurrentPosition failed: {e}",
                    self.id
                );
            }
        } else {
            warn!(
                "CurrentPosition characteristic missing for window covering {}",
                self.id
            );
        }
        if let Some(characteristic) = service.get_mut_characteristic(HapType::TargetPosition) {
            if let Err(e) = characteristic
                .update_value(Value::from(state.target_position))
                .await
            {
                warn!(
                    "update_value for window covering {} TargetPosition failed: {e}",
                    self.id
                );
            }
        } else {
            warn!(
                "TargetPosition characteristic missing for window covering {}",
                self.id
            );
        }
        if let Some(characteristic) = service.get_mut_characteristic(HapType::PositionState) {
            if let Err(e) = characteristic
                .update_value(Value::from(state.position_state as u8))
                .await
            {
                warn!(
                    "update_value for window covering {} PositionState failed: {e}",
                    self.id
                );
            }
        } else {
            warn!(
                "PositionState characteristic missing for window covering {}",
                self.id
            );
        }
    }
}

impl ComelitWindowCoveringAccessory {
    pub(crate) async fn new(
        id: u64,
        window_covering_data: &WindowCoveringDeviceData,
        client: ComelitClient,
        server: &IpServer,
        config: WindowCoveringConfig,
    ) -> Result<Self> {
        let device_id = window_covering_data.id.clone();
        let name = window_covering_data
            .description
            .clone()
            .unwrap_or(device_id.clone());

        let mut wc_accessory = WindowCoveringAccessory::new(
            id,
            AccessoryInformation {
                name: name.clone(),
                manufacturer: "Comelit".to_string(),
                serial_number: device_id.clone(),
                ..Default::default()
            },
        )
        .context("Cannot create window covering accessory")?;

        info!(
            "Created window covering accessory: {:?}",
            window_covering_data
        );

        wc_accessory.window_covering.current_horizontal_tilt_angle = None;
        wc_accessory.window_covering.target_horizontal_tilt_angle = None;
        wc_accessory.window_covering.obstruction_detected = None;
        wc_accessory.window_covering.hold_position = None;
        wc_accessory.window_covering.current_vertical_tilt_angle = None;
        wc_accessory.window_covering.target_vertical_tilt_angle = None;

        let state = WindowCoveringState::from_storage(device_id.as_str())
            .await
            .unwrap_or(WindowCoveringState::from(window_covering_data));

        state.save(device_id.as_str()).await?;

        info!(
            "Setting initial window covering position to {}",
            state.current_position
        );

        wc_accessory
            .window_covering
            .current_position
            .set_value(Value::from(state.current_position))
            .await
            .context("Cannot set current position")?;
        wc_accessory
            .window_covering
            .position_state
            .set_value(Value::from(state.position_state as u8))
            .await
            .context("Cannot set position state")?;
        wc_accessory
            .window_covering
            .target_position
            .set_value(Value::from(state.target_position))
            .await
            .context("Cannot set current target position")?;

        let shared_state = Arc::new(TokioMutex::new(state));

        Self::setup_read_characteristics(device_id.as_str(), &mut wc_accessory, shared_state.clone())
            .await;

        let handle = spawn_window_covering_worker(
            device_id.clone(),
            shared_state.clone(),
            client,
            config,
        );

        Self::setup_update_target_position(&mut wc_accessory, handle.clone()).await;

        let accessory = server.add_accessory(wc_accessory).await?;

        handle
            .set_sink(Box::new(HapWindowCoveringSink {
                id: device_id.clone(),
                accessory: accessory.clone(),
            }))
            .await;

        Ok(Self {
            id: device_id.to_string(),
            handle,
        })
    }

    async fn setup_read_characteristics(
        id: &str,
        accessory: &mut WindowCoveringAccessory,
        state: Arc<TokioMutex<WindowCoveringState>>,
    ) {
        let id_ = id.to_string();
        let state_ = state.clone();
        accessory
            .window_covering
            .position_state
            .on_read_async(Some(move || {
                let id_ = id_.clone();
                let state_ = state_.clone();
                async move {
                    Metrics::inc_hap_requests();
                    debug!("Window covering POSITION STATE read {}", id_);
                    let state = state_.lock().await;
                    Ok(Some(state.position_state as u8))
                }
                .boxed()
            }));

        let id_ = id.to_string();
        let state_ = state.clone();
        accessory
            .window_covering
            .current_position
            .on_read_async(Some(move || {
                let id_ = id_.to_string();
                let state_ = state_.clone();
                async move {
                    Metrics::inc_hap_requests();
                    debug!("Window covering POSITION read {}", id_);
                    let state = state_.lock().await;
                    Ok(Some(state.current_position))
                }
                .boxed()
            }));

        let id_ = id.to_string();
        let state_ = state.clone();
        accessory
            .window_covering
            .target_position
            .on_read_async(Some(move || {
                let id_ = id_.to_string();
                let state_ = state_.clone();
                async move {
                    Metrics::inc_hap_requests();
                    debug!("Window covering TARGET POSITION read {}", id_);
                    let state = state_.lock().await;
                    Ok(Some(state.target_position))
                }
                .boxed()
            }));
    }

    async fn setup_update_target_position(
        accessory: &mut WindowCoveringAccessory,
        handle: WindowCoveringHandle,
    ) {
        accessory
            .window_covering
            .target_position
            .on_update_async(Some(move |old_pos, new_pos| {
                let handle = handle.clone();
                async move {
                    Metrics::inc_hap_requests();
                    info!(
                        "Window covering target position update: {} -> {}",
                        old_pos, new_pos
                    );
                    handle.move_to(old_pos, new_pos).await;
                    Ok(())
                }
                .boxed()
            }));
    }
}

impl ComelitAccessory<WindowCoveringDeviceData> for ComelitWindowCoveringAccessory {
    fn get_comelit_id(&self) -> &str {
        &self.id
    }

    async fn update(&mut self, window_covering_data: &WindowCoveringDeviceData) -> Result<()> {
        if let Some(status) = window_covering_data.status.as_ref() {
            info!(
                "Window covering {} update: {}",
                window_covering_data.id, *status
            );
            let new_state = WindowCoveringState::from(window_covering_data);
            self.handle.status_update(new_state).await;
        }
        Ok(())
    }
}
