use anyhow::anyhow;
use anyhow::{Context, Result};
use futures::FutureExt;
use hap::characteristic::HapCharacteristic;
use hap::{
    accessory::{AccessoryInformation, window_covering::WindowCoveringAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    server::{IpServer, Server},
};
use serde_json::Value;
use std::cmp::{max, min};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::oneshot::{Receiver, Sender};
use tracing::{info, warn};

use crate::accessories::ComelitAccessory;
use crate::accessories::state::window_covering::{
    FULLY_CLOSED, FULLY_OPENED, PositionState, WindowCoveringState,
};
use comelit_hub_rs::{ComelitClient, WindowCoveringDeviceData};

#[derive(Clone, Copy)]
pub struct WindowCoveringConfig {
    pub closing_time: Duration,
    pub opening_time: Duration,
}

pub(crate) struct ComelitWindowCoveringAccessory {
    id: String,
    moving_observer: Arc<TokioMutex<MovingObserverTask>>,
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
            .set_value(Value::from(state.position_state as u8))
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

        let moving_observer = Arc::new(TokioMutex::new(MovingObserverTask::new(
            &device_id,
            state.clone(),
            client.clone(),
            config,
        )));
        Self::setup_update_target_position(&mut wc_accessory, moving_observer.clone()).await;

        server.add_accessory(wc_accessory).await?;
        Ok(Self {
            id: device_id.to_string(),
            moving_observer,
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
                    match (state.is_moving(), state.is_opening()) {
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
        accessory: &mut WindowCoveringAccessory,
        moving_observer: Arc<TokioMutex<MovingObserverTask>>,
    ) {
        accessory
            .window_covering
            .target_position
            .on_update_async(Some(move |old_pos, new_pos| {
                // For blinds/shades/awnings, a value of 0 indicates a position that permits the least light and a value
                // of 100 indicates a position that allows most light.
                // This means:
                // 0   -> FULLY CLOSED
                // 100 -> FULLY OPENED

                let moving_observer = moving_observer.clone();
                async move {
                    let mut moving_observer = moving_observer.lock().await;
                    match moving_observer.move_to(old_pos, new_pos).await {
                        Ok(_) => Ok(()),
                        Err(err) => Err(err.into()),
                    }
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
            info!("Window covering {} is {}", window_covering_data.id, *status);
            let new_state = WindowCoveringState::from(window_covering_data);
            let mut observer = self.moving_observer.lock().await;
            observer.update(new_state).await?;
            info!(
                "Updated window covering {} position to {:?}",
                self.id, status
            );
        }
        Ok(())
    }
}

enum MovingCommand {
    Stop,
}

enum MovingStatus {
    Moving,
    MovingExternal,
    Stopped,
}

struct MovingObserverTask {
    id: String,
    moving_sender: Option<Sender<MovingCommand>>,
    observing_sender: Option<Sender<MovingCommand>>,
    state: Arc<TokioMutex<WindowCoveringState>>,
    client: ComelitClient,
    config: WindowCoveringConfig,
    running: MovingStatus,
}

impl MovingObserverTask {
    pub fn new(
        id: &str,
        state: Arc<TokioMutex<WindowCoveringState>>,
        client: ComelitClient,
        config: WindowCoveringConfig,
    ) -> Self {
        Self {
            id: id.to_string(),
            moving_sender: None,
            observing_sender: None,
            state,
            client,
            config,
            running: MovingStatus::Stopped,
        }
    }

    async fn move_to(&mut self, old_pos: u8, new_pos: u8) -> Result<()> {
        // if the position is the same, do nothing
        if old_pos == new_pos {
            info!(
                "Target position equals current position for window covering {}, no action taken",
                self.id
            );
            return Ok(());
        }

        // Check if we are already moving
        if let Some(observer) = self.observing_sender.take() {
            info!(
                "Window covering {} was already moving, stopping it",
                self.id
            );
            if observer.send(MovingCommand::Stop).is_err() {
                return Err(anyhow!("Failed to send message to observer"));
            }
        } else {
            info!("Window covering {} is not moving", self.id);
        }
        self.running = MovingStatus::Moving; // the movement is initiated internally
        // Now move it in the new position
        let (moving_sender, moving_receiver) = tokio::sync::oneshot::channel::<MovingCommand>();
        let id = self.id.clone();
        tokio::spawn(Self::start_moving(
            id,
            old_pos,
            new_pos,
            self.state.clone(),
            self.config,
            self.client.clone(),
            moving_receiver,
        ));
        // spawn a task that waits for either the moving to finish or a cancellation
        let (observing_sender, observe_receiver) = tokio::sync::oneshot::channel::<MovingCommand>();
        tokio::spawn(Self::start_observing(
            self.id.clone(),
            self.state.clone(),
            self.config,
            self.client.clone(),
            observe_receiver,
        ));

        self.moving_sender = Some(moving_sender);
        self.observing_sender = Some(observing_sender);
        Ok(())
    }

    async fn update(&mut self, new_state: WindowCoveringState) -> Result<()> {
        let mut state = self.state.lock().await;
        match self.running {
            MovingStatus::Stopped => {
                *state = new_state;
                let (observing_sender, observe_receiver) =
                    tokio::sync::oneshot::channel::<MovingCommand>();
                self.running = MovingStatus::MovingExternal; // the movement is initiated externally
                tokio::spawn(Self::start_observing(
                    self.id.clone(),
                    self.state.clone(),
                    self.config,
                    self.client.clone(),
                    observe_receiver,
                ));
                self.observing_sender = Some(observing_sender);
                self.moving_sender = None;
            }
            MovingStatus::Moving => {
                // the window cover is moving (internally initiated)
                match new_state.position_state {
                    PositionState::Stopped => {
                        // someone stopped the movement
                        self.running = MovingStatus::Stopped;
                        if let Some(sender) = self.moving_sender.take() {
                            sender.send(MovingCommand::Stop).ok();
                        }
                        if let Some(observing_sender) = self.observing_sender.take() {
                            observing_sender.send(MovingCommand::Stop).ok();
                        }
                    }
                    PositionState::MovingUp => {}
                    PositionState::MovingDown => {}
                }
            }
            MovingStatus::MovingExternal => match new_state.position_state {
                PositionState::MovingDown => todo!(),
                PositionState::MovingUp => todo!(),
                PositionState::Stopped => {
                    self.running = MovingStatus::Stopped;
                    if let Some(sender) = self.moving_sender.take() {
                        sender.send(MovingCommand::Stop).ok();
                    }
                    if let Some(observing_sender) = self.observing_sender.take() {
                        observing_sender.send(MovingCommand::Stop).ok();
                    }
                }
            },
        }

        Ok(())
    }

    // This function is in charge of moving the window covering when the movement is initiated
    // from the HomeKit (for example, from an iPhone or from a Siri request)
    async fn start_moving(
        id: String,
        old_pos: u8,
        new_pos: u8,
        state: Arc<TokioMutex<WindowCoveringState>>,
        config: WindowCoveringConfig,
        client: ComelitClient,
        mut receiver: Receiver<MovingCommand>,
    ) -> Result<()> {
        // if the new position is greater the blind is opening (100 is fully open, 0 closed)
        let opening = old_pos > new_pos;
        let mut delta = Duration::from_millis(
            (if opening {
                (config.opening_time.as_millis() as f32 / 100f32) * (old_pos - new_pos) as f32
            } else {
                (config.closing_time.as_millis() as f32 / 100f32) * (new_pos - old_pos) as f32
            }) as u64,
        );

        info!(
            "Position change for window covering {} from {} to {}",
            id, old_pos, new_pos
        );

        {
            let mut state = state.lock().await;
            state.current_position = old_pos;
            state.target_position = new_pos;
        } // start moving
        info!("Start moving window covering {} to position {new_pos}", id);
        client.toggle_device_status(&id, !opening).await?;
        // sleep for the required time
        while delta.as_millis() > 0 {
            delta -= Duration::from_millis(1);
            tokio::time::sleep(Duration::from_millis(1)).await;
            if receiver.try_recv().is_ok() {
                // someone killed this moving process
                warn!("Window covering {} was interrupted", id);
                return Ok(());
            }
        }
        info!(
            "Window covering {} reached the requested position {new_pos}",
            id
        );
        // stop moving
        client.toggle_device_status(&id, opening).await?;
        let mut state = state.lock().await;
        state.current_position = new_pos;
        state.position_state = PositionState::Stopped;
        state.target_position = new_pos;
        // save the state on the disk so that we can resume
        state.save(&id).await
    }

    // This function should update the status of the window covering, even when the movement is initiated
    // from the outside (for example, a physical button)
    async fn start_observing(
        id: String,
        state: Arc<TokioMutex<WindowCoveringState>>,
        config: WindowCoveringConfig,
        client: ComelitClient,
        mut receiver: Receiver<MovingCommand>,
    ) -> Result<()> {
        loop {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            {
                let mut state = state.lock().await;
                if state.is_opening() {
                    let delta_pos = config.opening_time.as_secs() as f32 / 100.0;
                    let current_position =
                        (state.current_position as f32 + delta_pos).round() as u8;
                    state.current_position = min(FULLY_OPENED, current_position);
                } else {
                    let delta_pos = config.closing_time.as_secs() as f32 / 100.0;
                    let current_position =
                        (state.current_position as f32 - delta_pos).round() as u8;
                    state.current_position = max(FULLY_CLOSED, current_position);
                }
                info!(
                    "Window covering {id} is now at position {}",
                    state.current_position
                );
                if receiver.try_recv().is_ok() {
                    // we should send 1 (true) if the window is moving down or 0 (false) if it's moving up
                    let on = state.position_state == PositionState::MovingDown;
                    client.toggle_device_status(&id, on).await?; // stop the device
                    state.position_state = PositionState::Stopped;
                    break Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_move_to() {
        let mut state = WindowCoveringState {
            current_position: FULLY_OPENED,
            target_position: FULLY_CLOSED,
            position_state: PositionState::MovingDown,
        };
        let config = WindowCoveringConfig {
            opening_time: Duration::from_secs(5),
            closing_time: Duration::from_secs(5),
        };
        let client = ComelitClient::default();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let mut window_covering =
            MovingObserverTask::new("123", Arc::new(TokioMutex::new(state)), client, config);

        window_covering.move_to(0, 100).await.unwrap();
        assert_eq!(window_covering.state.lock().await.current_position, 100);

        window_covering.move_to(100, 0).await.unwrap();
        assert_eq!(window_covering.state.lock().await.current_position, 0);
    }
}
