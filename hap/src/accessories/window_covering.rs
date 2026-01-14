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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

use crate::accessories::ComelitAccessory;
use crate::accessories::state::window_covering::{PositionState, WindowCoveringState};
use comelit_hub_rs::{ComelitClient, ComelitClientError, WindowCoveringDeviceData};

pub struct WindowCoveringConfig {
    pub closing_time: Duration,
    pub opening_time: Duration,
}

pub(crate) struct ComelitWindowCoveringAccessory {
    id: String,
    state: Arc<TokioMutex<WindowCoveringState>>,
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
        let name = name.clone();
        let mut wc_accessory = WindowCoveringAccessory::new(
            id,
            AccessoryInformation {
                name,
                manufacturer: "Comelit".to_string(),
                serial_number: device_id.clone(),
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
            .set_value(Value::from(state.position_state))
            .await
            .context("Cannot set position state")?;
        wc_accessory
            .window_covering
            .target_position
            .set_value(Value::from(state.target_position))
            .await
            .context("Cannot set current target position")?;

        let state = Arc::new(TokioMutex::new(state));

        Self::setup_read_characteristics(device_id.as_str(), &mut wc_accessory, state.clone())
            .await;
        Self::setup_update_target_position(
            device_id.as_str(),
            client.clone(),
            &mut wc_accessory,
            config.closing_time,
            config.opening_time,
            state.clone(),
        )
        .await;

        server.add_accessory(wc_accessory).await?;
        Ok(Self {
            id: device_id.to_string(),
            state,
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
                    info!("Window covering POSITION STATE read {}", id_);
                    let state = state_.lock().await;
                    let WindowCoveringState {
                        moving, opening, ..
                    } = *state;
                    drop(state);
                    match (moving, opening) {
                        (true, true) => Ok(Some(PositionState::MovingUp as u8)),
                        (true, false) => Ok(Some(PositionState::MovingDown as u8)),
                        (false, true) => Ok(Some(PositionState::Stopped as u8)),
                        (false, false) => Ok(Some(PositionState::Stopped as u8)),
                    }
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
                    info!("Window covering POSITION read {}", id_);
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
                    info!("Window covering TARGET POSITION read {}", id_);
                    let state = state_.lock().await;
                    Ok(Some(state.target_position))
                }
                .boxed()
            }));
    }

    async fn setup_update_target_position(
        id: &str,
        client: ComelitClient,
        accessory: &mut WindowCoveringAccessory,
        closing_time: Duration,
        opening_time: Duration,
        state: Arc<TokioMutex<WindowCoveringState>>,
    ) {
        let id = id.to_string();
        let state = state.clone();
        let client = client.clone();
        accessory
            .window_covering
            .target_position
            .on_update_async(Some(move |_, new_pos| {
                // For blinds/shades/awnings, a value of 0 indicates a position that permits the least light and a value
                // of 100 indicates a position that allows most light.
                // This means:
                // 0   -> FULLY CLOSED
                // 100 -> FULLY OPENED

                let state = state.clone();
                let client = client.clone();
                let id = id.to_string();
                async move {
                    let WindowCoveringState { current_position: position, moving, position_state, .. } = {
                        let state = state.lock().await;
                        *state
                    };

                    if position == new_pos {
                        info!("Target position equals current position for window covering {}, no action taken", id);
                        return Ok(());
                    }

                    let id = id.clone();
                    // if the new position is greater the blind is opening (100 is fully open, 0 closed)
                    let opening = position > new_pos;
                    let delta = Duration::from_secs((if opening {
                        (opening_time.as_secs_f32() / 100f32) * (position - new_pos) as f32
                    } else {
                        (closing_time.as_secs_f32() / 100f32) * (new_pos - position) as f32
                    }) as u64);

                    let delta_pos = if opening {
                        (delta.as_secs()) / (position - new_pos) as u64
                    } else {
                        (delta.as_secs()) / (new_pos - position) as u64
                    } as u8;

                    info!("Position change for window covering {} from {} to {}", id, position, new_pos);
                    // Check if we are already moving
                    if moving {
                        info!("Window covering {} is moving, stopping it", id);
                        client.toggle_device_status(&id, position_state == PositionState::MovingDown as u8).await?; // stop the device
                        let mut state = state.lock().await;
                        state.position_state = PositionState::Stopped as u8;
                        state.target_position = new_pos;
                    } else {
                        info!("Window covering {} is not moving", id);
                    }
                    // Now move it in the new position
                    let id1 = id.clone();
                    let state1 = state.clone();
                    let moving_task = async move {
                        {
                            let mut state = state1.lock().await;
                            // state.moving = true;
                            // state.opening = opening;
                            state.position_state = if opening { PositionState::MovingUp as u8 } else { PositionState::MovingDown as u8 };
                            state.target_position = new_pos;
                        }
                        // start moving
                        info!("Start moving window covering {id1} to position {new_pos}");
                        client.toggle_device_status(&id1, !opening).await?;
                        // sleep for the required time
                        tokio::time::sleep(delta).await;
                        info!("Window covering {id1} reached the requested position {new_pos}");
                        // stop moving
                        client.toggle_device_status(&id1, opening).await?;
                        {
                            let mut state = state1.lock().await;
                            state.current_position = new_pos;
                            state.position_state = PositionState::Stopped as u8;
                            state.target_position = new_pos;
                            state.save(id1.as_str()).await
                                .map_err(|e| ComelitClientError::Generic(e.to_string()))
                        }
                    };

                    // spawn a task that waits for either the moving to finish or a cancellation
                    let state2 = state.clone();
                    let done = Arc::new(AtomicBool::new(false));
                    let done_ = done.clone();
                    let id_ = id.clone();
                    let cancel_task = async move {
                        loop {
                            {
                                let mut state = state2.lock().await;
                                if state.opening {
                                    state.current_position += delta_pos;
                                } else {
                                    state.current_position -= delta_pos;
                                }
                                info!("Window covering {id_} is now at position {}", state.current_position);
                                if done_.load(Ordering::Relaxed) {
                                    state.position_state = PositionState::Stopped as u8;
                                    break;
                                }
                            }
                            tokio::time::sleep(Duration::from_millis(1000)).await;
                        }
                    };
                    tokio::select! {
                        _ = moving_task => {
                            info!("Window covering {} position change completed", id);
                            done.store(true, Ordering::Relaxed);
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

impl ComelitAccessory<WindowCoveringDeviceData> for ComelitWindowCoveringAccessory {
    fn get_comelit_id(&self) -> &str {
        &self.id
    }

    async fn update(&mut self, window_covering_data: &WindowCoveringDeviceData) -> Result<()> {
        if let Some(status) = window_covering_data.status.as_ref() {
            info!("Window covering {} is {}", window_covering_data.id, *status);
            let new_state = WindowCoveringState::from(window_covering_data);
            {
                let mut state = self.state.lock().await;
                state.moving = new_state.moving;
                state.opening = new_state.opening;
                state.save(&window_covering_data.id).await?;
            }
            info!(
                "Updated window covering {} position to {:?}",
                self.id, status
            );
        }
        Ok(())
    }
}
