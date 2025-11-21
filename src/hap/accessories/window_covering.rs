use anyhow::{Context, Result};
use futures::FutureExt;
use hap::characteristic::HapCharacteristic;
use hap::{
    accessory::{AccessoryInformation, window_covering::WindowCoveringAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    server::{IpServer, Server},
};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;
use tracing::{info, warn};

use crate::hap::accessories::ComelitAccessory;
use crate::protocol::client::ComelitClientError;
use crate::protocol::out_data_messages::{HomeDeviceData, PowerStatus};
use crate::{
    hap::accessories::AccessoryPointer,
    protocol::{client::ComelitClient, out_data_messages::WindowCoveringDeviceData},
};

pub struct WindowCoveringConfig {
    pub closing_time: Duration,
    pub opening_time: Duration,
}

pub(crate) struct ComelitWindowCoveringAccessory {
    accessory: AccessoryPointer,
    data: WindowCoveringDeviceData,
    state: Arc<State>,
}

struct State {
    position: AtomicU8,
    target_position: AtomicU8,
    moving: AtomicBool,
    opening: AtomicBool,
}

impl State {
    pub fn new(position: u8, is_moving: bool, opening: bool) -> State {
        State {
            position: AtomicU8::new(position),
            target_position: AtomicU8::new(position),
            moving: AtomicBool::new(is_moving),
            opening: AtomicBool::new(opening),
        }
    }
}

impl ComelitWindowCoveringAccessory {
    pub(crate) async fn new(
        id: u64,
        window_covering_data: WindowCoveringDeviceData,
        client: Arc<ComelitClient>,
        server: &IpServer,
        config: WindowCoveringConfig,
    ) -> Result<Self> {
        let device_id = window_covering_data.data.id.clone();
        let name = window_covering_data
            .data
            .description
            .clone()
            .unwrap_or(device_id.clone());
        let name = name.clone();
        let mut wc_accessory = WindowCoveringAccessory::new(
            id,
            AccessoryInformation {
                name,
                ..Default::default()
            },
        )
        .context("Cannot create lightbulb accessory")?;

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

        let position = 100;
        info!("Setting initial window covering position to {}", position);
        wc_accessory
            .window_covering
            .current_position
            .set_value(Value::from(position))
            .await
            .context("Cannot set current position")?;
        wc_accessory
            .window_covering
            .position_state
            .set_value(Value::from(2))
            .await
            .context("Cannot set position state")?;
        wc_accessory
            .window_covering
            .target_position
            .set_value(Value::from(position))
            .await
            .context("Cannot set current target position")?;

        let moving = window_covering_data
            .data
            .power_status
            .clone()
            .unwrap_or_default()
            != PowerStatus::Stopped;

        let opening = window_covering_data
            .data
            .power_status
            .clone()
            .unwrap_or_default()
            == PowerStatus::Up;

        let state = Arc::new(State::new(position, moving, opening));

        Self::setup_read_position_state(device_id.as_str(), &mut wc_accessory, state.clone()).await;
        Self::setup_update_target_position(
            device_id.as_str(),
            client.clone(),
            &mut wc_accessory,
            config.closing_time,
            config.opening_time,
            state.clone(),
        )
        .await;

        Ok(Self {
            accessory: server.add_accessory(wc_accessory).await?,
            data: window_covering_data,
            state,
        })
    }

    async fn setup_read_position_state(
        id: &str,
        accessory: &mut WindowCoveringAccessory,
        state: Arc<State>,
    ) {
        let id = id.to_string();
        accessory
            .window_covering
            .position_state
            .on_read_async(Some(move || {
                info!("Window covering POSITION STATE read {}", id);
                let state = state.clone();
                async move {
                    let is_moving = state.moving.load(Ordering::Relaxed);
                    let opening = state.opening.load(Ordering::Relaxed);
                    match (is_moving, opening) {
                        (true, true) => Ok(Some(1)),
                        (true, false) => Ok(Some(0)),
                        (false, true) => Ok(Some(2)),
                        (false, false) => Ok(Some(2)),
                    }
                }
                .boxed()
            }));
    }

    async fn setup_update_target_position(
        id: &str,
        client: Arc<ComelitClient>,
        accessory: &mut WindowCoveringAccessory,
        closing_time: Duration,
        opening_time: Duration,
        state: Arc<State>,
    ) {
        let id = id.to_string();
        accessory
            .window_covering
            .target_position
            .on_update_async(Some(move |current_pos, new_pos| {
                let c = client.clone();
                info!("Current position for the window covering {id} set to {current_pos}, {new_pos}");
                let id = id.clone();
                let state = state.clone();
                let opening = current_pos > new_pos;
                state.moving.store(true, Ordering::Relaxed);
                state.opening.store(opening, Ordering::Relaxed);
                state.position.store(current_pos, Ordering::Relaxed);
                state.target_position.store(new_pos, Ordering::Relaxed);
                let delta = Duration::from_secs((if current_pos > new_pos {
                    (opening_time.as_secs_f32() / 100f32) * (current_pos - new_pos) as f32
                } else {
                    (closing_time.as_secs_f32() / 100f32) * (new_pos - current_pos) as f32
                }) as u64);

                async move {
                    if current_pos == new_pos {
                        info!("Target position equals current position for window covering {}, no action taken", id);
                        return Ok(());
                    }

                    // Check if we are already moving
                    if state.moving.load(Ordering::Relaxed) {
                        info!("Previous position change for window covering {} is still in progress, stopping it", id);
                        c.toggle_device_status(&id, true).await?; // stop the device
                        state.moving.store(false, Ordering::Relaxed); // mark as not moving
                    }
                    // Now move it in the new position
                    state.moving.store(true, Ordering::Relaxed);
                    let id1 = id.clone();
                    let state1 = state.clone();
                    let moving_task = async move {
                        // start moving
                        c.toggle_device_status(&id1, false).await?;
                        // sleep for the required time
                        tokio::time::sleep(delta).await;
                        info!("Timeout reached when setting blind position for window covering {}", id1);
                        // stop moving
                        c.toggle_device_status(&id1, true).await?;
                        state1.moving.store(false, Ordering::Relaxed);
                        Ok::<(), ComelitClientError>(())
                    };

                    // spawn a task that waits for either the moving to finish or a cancellation
                    let id2 = id.clone();
                    let state2 = state.clone();
                    let cancel_task = async move {
                        while state2.moving.load(Ordering::Relaxed) {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                        info!("Position change for window covering {} was cancelled", id2);
                    };
                    tokio::select! {
                        _ = moving_task => {
                            info!("Window covering {} position change completed", id);
                        }
                        _ = cancel_task => {
                            warn!("Window covering {} was cancelled, stopping it", id);
                        }
                    }
                    Ok(())
                }.boxed()
            }));
    }
}

impl ComelitAccessory for ComelitWindowCoveringAccessory {
    fn id(&self) -> &str {
        &self.data.data.id
    }

    async fn update(&self, window_covering: &HomeDeviceData) -> Result<()> {
        if let HomeDeviceData::WindowCovering(window_covering_data) = window_covering {
            if let Some(status) = window_covering_data.open_status.as_ref() {
                let mut accessory = self.accessory.lock().await;
                let service = accessory
                    .get_mut_service(hap::HapType::WindowCovering)
                    .unwrap();
                let position_characteristic = service
                    .get_mut_characteristic(hap::HapType::CurrentPosition)
                    .unwrap();

                position_characteristic
                    .set_value(Value::from(self.state.position.load(Ordering::Relaxed)))
                    .await?;

                let is_moving = window_covering_data
                    .data
                    .power_status
                    .clone()
                    .unwrap_or_default()
                    != PowerStatus::Stopped;

                let opening = window_covering_data
                    .data
                    .power_status
                    .clone()
                    .unwrap_or_default()
                    == PowerStatus::Up;

                let position_state_characteristic = service
                    .get_mut_characteristic(hap::HapType::PositionState)
                    .unwrap();

                let value = match (is_moving, opening) {
                    (true, true) => Value::from(1),
                    (true, false) => Value::from(0),
                    (false, true) => Value::from(2),
                    (false, false) => Value::from(2),
                };

                position_state_characteristic.set_value(value).await?;

                let target_position_characteristic = service
                    .get_mut_characteristic(hap::HapType::TargetPosition)
                    .unwrap();
                target_position_characteristic
                    .set_value(Value::from(
                        self.state.target_position.load(Ordering::Relaxed),
                    ))
                    .await?;

                info!(
                    "Updated window covering {} position to {:?}",
                    self.data.data.id, status
                );
            }
        }
        Ok(())
    }
}
