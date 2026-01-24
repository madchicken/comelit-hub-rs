use anyhow::{Context, Result};
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
use std::cmp::{max, min};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc::{self, Sender};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::accessories::ComelitAccessory;
use crate::accessories::state::window_covering::{
    FULLY_CLOSED, FULLY_OPENED, PositionState, WindowCoveringState,
};
use comelit_hub_rs::{ComelitClient, ComelitClientTrait, WindowCoveringDeviceData};

#[derive(Clone, Copy)]
pub struct WindowCoveringConfig {
    pub closing_time: Duration,
    pub opening_time: Duration,
}

pub(crate) struct ComelitWindowCoveringAccessory {
    id: String,
    command_sender: Sender<WorkerCommand>,
    #[allow(dead_code)]
    accessory: Accessory,
}

/// Commands sent to the worker thread
#[derive(Debug)]
enum WorkerCommand {
    /// Initiate movement from HomeKit (target position changed)
    MoveTo { old_pos: u8, new_pos: u8 },

    /// Comelit update received (status changed from external source or confirmation)
    StatusUpdate { new_state: WindowCoveringState },

    /// Set the accessory pointer for updating HAP characteristics
    SetAccessory { accessory: Accessory },

    /// Shutdown the worker
    Shutdown,
}

/// Internal state machine for the worker
#[derive(Debug, Clone, Default)]
enum WorkerState {
    /// Blind is idle, not moving
    #[default]
    Idle,

    /// Movement was initiated internally (from HomeKit)
    /// We're waiting for Comelit to confirm the movement started
    WaitingForMoveConfirmation {
        target: u8,
        direction: PositionState,
    },

    /// Blind is moving (internally initiated), we're tracking position
    MovingInternal {
        target: u8,
        direction: PositionState,
        started_at: Instant,
        start_pos: u8,
    },

    /// Movement was initiated externally (physical button)
    /// We're tracking position until it stops
    MovingExternal {
        direction: PositionState,
        started_at: Instant,
        start_pos: u8,
    },

    /// We sent a stop command, waiting for confirmation
    #[allow(dead_code)]
    WaitingForStopConfirmation { current_pos: u8 },
}

struct WindowCoveringWorker<C: ComelitClientTrait> {
    id: String,
    state: Arc<TokioMutex<WindowCoveringState>>,
    client: C,
    config: WindowCoveringConfig,
    worker_state: WorkerState,
    accessory: Option<Accessory>,
}

impl<C: ComelitClientTrait + 'static> WindowCoveringWorker<C> {
    fn new(
        id: String,
        state: Arc<TokioMutex<WindowCoveringState>>,
        client: C,
        config: WindowCoveringConfig,
    ) -> Self {
        Self {
            id,
            state,
            client,
            config,
            worker_state: WorkerState::Idle,
            accessory: None,
        }
    }

    /// Main worker loop - handles commands and position updates
    async fn run(mut self, mut receiver: mpsc::Receiver<WorkerCommand>) {
        let mut position_ticker = tokio::time::interval(Duration::from_secs(1));

        loop {
            tokio::select! {
                // Handle incoming commands
                cmd = receiver.recv() => {
                    match cmd {
                        Some(WorkerCommand::MoveTo { old_pos, new_pos }) => {
                            if let Err(e) = self.handle_move_to(old_pos, new_pos).await {
                                warn!("Error handling move_to: {}", e);
                            }
                        }
                        Some(WorkerCommand::StatusUpdate { new_state }) => {
                            if let Err(e) = self.handle_status_update(new_state).await {
                                warn!("Error handling status update: {}", e);
                            }
                        }
                        Some(WorkerCommand::SetAccessory { accessory }) => {
                            self.accessory = Some(accessory);
                        }
                        Some(WorkerCommand::Shutdown) | None => {
                            info!("Worker for {} shutting down", self.id);
                            break;
                        }
                    }
                }

                // Periodically update position while moving
                _ = position_ticker.tick() => {
                    if let Err(e) = self.update_position().await {
                        warn!("Error updating position: {}", e);
                    }
                }
            }
        }
    }

    /// Handle MoveTo command from HomeKit
    async fn handle_move_to(&mut self, old_pos: u8, new_pos: u8) -> Result<()> {
        // If positions are the same, nothing to do
        if old_pos == new_pos {
            info!(
                "Target position equals current position for {}, no action",
                self.id
            );
            return Ok(());
        }

        let direction = if new_pos > old_pos {
            PositionState::MovingUp
        } else {
            PositionState::MovingDown
        };

        // If we're currently moving, stop first
        match &self.worker_state {
            WorkerState::MovingInternal { direction: dir, .. }
            | WorkerState::MovingExternal { direction: dir, .. } => {
                info!("Stopping current movement before new move for {}", self.id);
                // Send stop command
                let on = *dir == PositionState::MovingDown;
                self.client.toggle_device_status(&self.id, on).await?;

                // Wait for the blind to actually stop
                self.worker_state = WorkerState::WaitingForStopConfirmation {
                    current_pos: {
                        let state = self.state.lock().await;
                        state.current_position
                    },
                };

                // We'll handle the new move after receiving stop confirmation
                // For now, just return. The user can retry or the update will trigger new state.
                return Ok(());
            }
            WorkerState::WaitingForMoveConfirmation { .. }
            | WorkerState::WaitingForStopConfirmation { .. } => {
                info!("Already waiting for confirmation, ignoring move request");
                return Ok(());
            }
            WorkerState::Idle => {}
        }

        // Update state with target
        {
            let mut state = self.state.lock().await;
            state.target_position = new_pos;
            state.position_state = direction;
        }

        info!(
            "Initiating move for {} from {} to {} (direction: {:?})",
            self.id, old_pos, new_pos, direction
        );

        // Send toggle command to Comelit
        // For blinds: true = moving down (closing), false = moving up (opening)
        let opening = direction == PositionState::MovingUp;
        self.client.toggle_device_status(&self.id, opening).await?;

        // Enter waiting state
        self.worker_state = WorkerState::WaitingForMoveConfirmation {
            target: new_pos,
            direction,
        };

        self.update_accessory().await?;
        Ok(())
    }

    /// Handle status update from Comelit
    async fn handle_status_update(&mut self, new_state: WindowCoveringState) -> Result<()> {
        let new_position_state = new_state.position_state;

        match &self.worker_state {
            WorkerState::Idle => {
                // We weren't expecting any movement
                if new_position_state != PositionState::Stopped {
                    // External movement started (physical button)
                    let current_pos = {
                        let state = self.state.lock().await;
                        state.current_position
                    };

                    info!(
                        "External movement detected for {} ({:?})",
                        self.id, new_position_state
                    );

                    // Update state
                    {
                        let mut state = self.state.lock().await;
                        state.position_state = new_position_state;
                        state.target_position = if new_position_state == PositionState::MovingUp {
                            FULLY_OPENED
                        } else {
                            FULLY_CLOSED
                        };
                    }

                    self.worker_state = WorkerState::MovingExternal {
                        direction: new_position_state,
                        started_at: Instant::now(),
                        start_pos: current_pos,
                    };

                    self.update_accessory().await?;
                }
                // If stopped and we're idle, nothing to do
            }

            WorkerState::WaitingForMoveConfirmation { target, direction } => {
                if new_position_state == *direction {
                    // Confirmation received - movement started
                    let current_pos = {
                        let state = self.state.lock().await;
                        state.current_position
                    };

                    info!(
                        "Move confirmation received for {} (target: {})",
                        self.id, target
                    );

                    self.worker_state = WorkerState::MovingInternal {
                        target: *target,
                        direction: *direction,
                        started_at: Instant::now(),
                        start_pos: current_pos,
                    };
                } else if new_position_state == PositionState::Stopped {
                    // Unexpected stop - maybe couldn't move?
                    warn!(
                        "Received stop while waiting for move confirmation for {}",
                        self.id
                    );
                    self.worker_state = WorkerState::Idle;
                    self.finalize_position().await?;
                }
            }

            WorkerState::MovingInternal {
                target, direction, ..
            } => {
                if new_position_state == PositionState::Stopped {
                    // Movement stopped (reached target or manual stop)
                    info!("Internal movement stopped for {}", self.id);
                    let target = *target;
                    let direction = *direction;
                    self.worker_state = WorkerState::Idle;
                    self.finalize_position_with_target(target, direction)
                        .await?;
                }
                // If still moving in same direction, continue tracking
            }

            WorkerState::MovingExternal { .. } => {
                if new_position_state == PositionState::Stopped {
                    // External movement stopped
                    info!("External movement stopped for {}", self.id);
                    self.worker_state = WorkerState::Idle;
                    self.finalize_position().await?;
                }
                // If still moving, continue tracking
            }

            WorkerState::WaitingForStopConfirmation { .. } => {
                if new_position_state == PositionState::Stopped {
                    // Stop confirmed
                    info!("Stop confirmed for {}", self.id);
                    self.worker_state = WorkerState::Idle;
                    self.finalize_position().await?;
                }
            }
        }

        Ok(())
    }

    /// Update position estimate based on elapsed time
    async fn update_position(&mut self) -> Result<()> {
        let (direction, started_at, start_pos, target) = match &self.worker_state {
            WorkerState::MovingInternal {
                direction,
                started_at,
                start_pos,
                target,
            } => (*direction, *started_at, *start_pos, Some(*target)),
            WorkerState::MovingExternal {
                direction,
                started_at,
                start_pos,
            } => (*direction, *started_at, *start_pos, None),
            _ => return Ok(()), // Not moving, nothing to update
        };

        let elapsed = started_at.elapsed();
        let travel_time = if direction == PositionState::MovingUp {
            self.config.opening_time
        } else {
            self.config.closing_time
        };

        // Calculate position change based on elapsed time
        let position_change =
            (elapsed.as_secs_f32() / travel_time.as_secs_f32() * 100.0).round() as i16;

        let new_position = if direction == PositionState::MovingUp {
            min(FULLY_OPENED, (start_pos as i16 + position_change) as u8)
        } else {
            max(FULLY_CLOSED as i16, start_pos as i16 - position_change) as u8
        };

        // Check if we've reached the target (for internal movements)
        let reached_target = if let Some(target) = target {
            if direction == PositionState::MovingUp {
                new_position >= target
            } else {
                new_position <= target
            }
        } else {
            false
        };

        // Update state
        {
            let mut state = self.state.lock().await;
            state.current_position = new_position;
            debug!(
                "Position update for {}: {} (target: {:?})",
                self.id, new_position, target
            );
        }

        // If we've reached the target, stop the movement
        if reached_target && let Some(target) = target {
            info!(
                "Reached target position {} for {}, sending stop",
                target, self.id
            );
            // Send stop command
            let opening = direction == PositionState::MovingUp;
            self.client.toggle_device_status(&self.id, opening).await?;

            // Transition to waiting for stop confirmation
            self.worker_state = WorkerState::WaitingForStopConfirmation {
                current_pos: new_position,
            };
        }

        self.update_accessory().await?;
        Ok(())
    }

    /// Finalize position when movement stops
    async fn finalize_position(&mut self) -> Result<()> {
        let mut state = self.state.lock().await;
        state.position_state = PositionState::Stopped;
        state.target_position = state.current_position;

        info!(
            "Finalized position for {} at {}",
            self.id, state.current_position
        );

        state.save(&self.id).await?;
        drop(state);

        self.update_accessory().await
    }

    /// Finalize position with known target
    async fn finalize_position_with_target(
        &mut self,
        target: u8,
        direction: PositionState,
    ) -> Result<()> {
        let mut state = self.state.lock().await;

        // If we were very close to target, snap to it
        let diff = (state.current_position as i16 - target as i16).abs();
        if diff <= 5 {
            state.current_position = target;
        }

        // Ensure position is within bounds based on direction
        if direction == PositionState::MovingUp {
            state.current_position = min(state.current_position, FULLY_OPENED);
        } else {
            state.current_position = max(state.current_position, FULLY_CLOSED);
        }

        state.position_state = PositionState::Stopped;
        state.target_position = state.current_position;

        info!(
            "Finalized position for {} at {} (target was {})",
            self.id, state.current_position, target
        );

        state.save(&self.id).await?;
        drop(state);

        self.update_accessory().await
    }

    /// Update the HAP accessory characteristics
    async fn update_accessory(&self) -> Result<()> {
        if let Some(accessory) = &self.accessory {
            let state = {
                let s = self.state.lock().await;
                *s
            };

            let mut accessory = accessory.lock().await;
            let service = accessory
                .get_mut_service(HapType::WindowCovering)
                .context("WindowCovering service not found")?;

            if let Some(characteristic) = service.get_mut_characteristic(HapType::CurrentPosition) {
                characteristic
                    .update_value(Value::from(state.current_position))
                    .await?;
            }

            if let Some(characteristic) = service.get_mut_characteristic(HapType::TargetPosition) {
                characteristic
                    .update_value(Value::from(state.target_position))
                    .await?;
            }

            if let Some(characteristic) = service.get_mut_characteristic(HapType::PositionState) {
                characteristic
                    .update_value(Value::from(state.position_state as u8))
                    .await?;
            }
        }
        Ok(())
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

        // Remove optional characteristics we don't support
        wc_accessory.window_covering.current_horizontal_tilt_angle = None;
        wc_accessory.window_covering.target_horizontal_tilt_angle = None;
        wc_accessory.window_covering.obstruction_detected = None;
        wc_accessory.window_covering.hold_position = None;
        wc_accessory.window_covering.current_vertical_tilt_angle = None;
        wc_accessory.window_covering.target_vertical_tilt_angle = None;

        // Load or create initial state
        let state = WindowCoveringState::from_storage(device_id.as_str())
            .await
            .unwrap_or(WindowCoveringState::from(window_covering_data));

        state.save(device_id.as_str()).await?;

        info!(
            "Setting initial window covering position to {}",
            state.current_position
        );

        // Set initial values
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

        // Create command channel
        let (command_sender, command_receiver) = mpsc::channel::<WorkerCommand>(32);

        // Set up read callbacks
        Self::setup_read_characteristics(device_id.as_str(), &mut wc_accessory, state.clone())
            .await;

        // Set up update callback
        Self::setup_update_target_position(&mut wc_accessory, command_sender.clone()).await;

        // Spawn the worker thread
        let worker = WindowCoveringWorker::new(device_id.clone(), state.clone(), client, config);

        tokio::spawn(worker.run(command_receiver));

        // Add accessory to server
        let accessory = server.add_accessory(wc_accessory).await?;

        // Send accessory to worker
        command_sender
            .send(WorkerCommand::SetAccessory {
                accessory: accessory.clone(),
            })
            .await
            .ok();

        Ok(Self {
            id: device_id.to_string(),
            command_sender,
            accessory,
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
                    debug!("Window covering TARGET POSITION read {}", id_);
                    let state = state_.lock().await;
                    Ok(Some(state.target_position))
                }
                .boxed()
            }));
    }

    async fn setup_update_target_position(
        accessory: &mut WindowCoveringAccessory,
        command_sender: Sender<WorkerCommand>,
    ) {
        accessory
            .window_covering
            .target_position
            .on_update_async(Some(move |old_pos, new_pos| {
                let command_sender = command_sender.clone();
                async move {
                    info!(
                        "Window covering target position update: {} -> {}",
                        old_pos, new_pos
                    );

                    if let Err(e) = command_sender
                        .send(WorkerCommand::MoveTo { old_pos, new_pos })
                        .await
                    {
                        warn!("Failed to send move command: {}", e);
                    }

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

            self.command_sender
                .send(WorkerCommand::StatusUpdate { new_state })
                .await
                .ok();

            info!(
                "Sent status update for window covering {} ({:?})",
                self.id, status
            );
        }
        Ok(())
    }
}

impl Drop for ComelitWindowCoveringAccessory {
    fn drop(&mut self) {
        // Try to send shutdown command (best effort)
        let _ = self.command_sender.try_send(WorkerCommand::Shutdown);
    }
}

#[cfg(test)]
pub mod testing {
    use async_trait::async_trait;
    use comelit_hub_rs::{
        ActionType, ClimaMode, ClimaOnOff, ComelitClientError, ComelitClientTrait, HomeDeviceData,
        MacAddress, State, ThermoSeason,
    };
    use dashmap::DashMap;
    use tokio::time::sleep;

    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::RwLock;
    use tokio::task::JoinHandle;

    #[derive(Clone, Default)]
    pub struct FakeComelitClient {
        pub toggle_calls: Arc<RwLock<Vec<(String, bool)>>>,
        pub action_calls: Arc<RwLock<Vec<(String, ActionType, i32)>>>,
        pub should_fail: Arc<AtomicBool>,
    }

    #[allow(dead_code)]
    impl FakeComelitClient {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn failing() -> Self {
            Self {
                should_fail: Arc::new(AtomicBool::new(true)),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl ComelitClientTrait for FakeComelitClient {
        fn mac_address(&self) -> &MacAddress {
            static MAC: MacAddress = MacAddress::new([0, 0, 0, 0, 0, 0]);
            &MAC
        }

        async fn disconnect(&self) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn login(&self, _state: State) -> Result<JoinHandle<()>, ComelitClientError> {
            Ok(tokio::task::spawn(async {}))
        }

        async fn info<T>(
            &self,
            _device_id: &str,
            _detail_level: u8,
        ) -> Result<Vec<T>, ComelitClientError>
        where
            T: serde::de::DeserializeOwned + Send,
        {
            Ok(vec![])
        }

        async fn subscribe(&self, _device_id: &str) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn fetch_index(
            &self,
            _level: u8,
        ) -> Result<DashMap<String, HomeDeviceData>, ComelitClientError> {
            Ok(DashMap::new())
        }

        async fn fetch_external_devices(
            &self,
        ) -> Result<DashMap<String, HomeDeviceData>, ComelitClientError> {
            Ok(DashMap::new())
        }

        async fn send_action(
            &self,
            device_id: &str,
            action_type: ActionType,
            value: i32,
        ) -> Result<(), ComelitClientError> {
            self.action_calls
                .write()
                .await
                .push((device_id.to_string(), action_type, value));
            Ok(())
        }

        async fn toggle_device_status(&self, id: &str, on: bool) -> Result<(), ComelitClientError> {
            if self.should_fail.load(Ordering::Relaxed) {
                return Err(ComelitClientError::Generic("Fake error".to_string()));
            }
            self.toggle_calls.write().await.push((id.to_string(), on));
            Ok(())
        }

        async fn toggle_blind_position(
            &self,
            _id: &str,
            _position: u8,
        ) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn set_thermostat_temperature(
            &self,
            _id: &str,
            _temperature: i32,
        ) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn set_thermostat_mode(
            &self,
            _id: &str,
            _mode: ClimaMode,
        ) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn set_thermostat_season(
            &self,
            _id: &str,
            _mode: ThermoSeason,
        ) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn toggle_thermostat_status(
            &self,
            _id: &str,
            _mode: ClimaOnOff,
        ) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn set_humidity(&self, _id: &str, _humidity: i32) -> Result<(), ComelitClientError> {
            Ok(())
        }
    }

    async fn create_test_worker(
        state: WindowCoveringState,
    ) -> (
        Sender<WorkerCommand>,
        Arc<TokioMutex<WindowCoveringState>>,
        FakeComelitClient,
    ) {
        let config = WindowCoveringConfig {
            opening_time: Duration::from_secs(5),
            closing_time: Duration::from_secs(5),
        };
        let client = FakeComelitClient::new();
        let state = Arc::new(TokioMutex::new(state));
        let (sender, receiver) = mpsc::channel(32);

        let worker = WindowCoveringWorker::new(
            "test-123".to_string(),
            state.clone(),
            client.clone(),
            config,
        );

        tokio::spawn(worker.run(receiver));

        (sender, state, client)
    }

    #[tokio::test]
    async fn test_move_to_open() {
        let initial_state = WindowCoveringState {
            current_position: FULLY_CLOSED,
            target_position: FULLY_CLOSED,
            position_state: PositionState::Stopped,
        };

        let (sender, state, client) = create_test_worker(initial_state).await;

        // Send move command
        sender
            .send(WorkerCommand::MoveTo {
                old_pos: FULLY_CLOSED,
                new_pos: FULLY_OPENED,
            })
            .await
            .unwrap();

        // Wait for command to be processed
        sleep(Duration::from_millis(100)).await;

        // Verify toggle was called (opening = false means moving up)
        let calls = client.toggle_calls.read().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], ("test-123".to_string(), false)); // false = opening/moving up

        // Simulate Comelit confirmation
        sender
            .send(WorkerCommand::StatusUpdate {
                new_state: WindowCoveringState {
                    current_position: FULLY_CLOSED,
                    target_position: FULLY_OPENED,
                    position_state: PositionState::MovingUp,
                },
            })
            .await
            .unwrap();

        // Wait for position updates
        sleep(Duration::from_secs(3)).await;

        // Check position has been updating
        let current_state = state.lock().await;
        assert!(current_state.current_position > FULLY_CLOSED);
        assert!(current_state.current_position < FULLY_OPENED);
    }

    #[tokio::test]
    async fn test_external_movement() {
        let initial_state = WindowCoveringState {
            current_position: 50,
            target_position: 50,
            position_state: PositionState::Stopped,
        };

        let (sender, state, _client) = create_test_worker(initial_state).await;

        // Simulate external movement starting (physical button)
        sender
            .send(WorkerCommand::StatusUpdate {
                new_state: WindowCoveringState {
                    current_position: 50,
                    target_position: FULLY_OPENED,
                    position_state: PositionState::MovingUp,
                },
            })
            .await
            .unwrap();

        // Wait for position updates
        sleep(Duration::from_secs(2)).await;

        // Check position is updating
        {
            let current_state = state.lock().await;
            assert!(current_state.current_position > 50);
            assert_eq!(current_state.position_state, PositionState::MovingUp);
        }

        // Simulate external stop
        sender
            .send(WorkerCommand::StatusUpdate {
                new_state: WindowCoveringState {
                    current_position: 70,
                    target_position: 70,
                    position_state: PositionState::Stopped,
                },
            })
            .await
            .unwrap();

        sleep(Duration::from_millis(100)).await;

        // Check position is finalized
        let current_state = state.lock().await;
        assert_eq!(current_state.position_state, PositionState::Stopped);
        assert_eq!(
            current_state.target_position,
            current_state.current_position
        );
    }

    #[tokio::test]
    async fn test_move_to_close() {
        let initial_state = WindowCoveringState {
            current_position: FULLY_OPENED,
            target_position: FULLY_OPENED,
            position_state: PositionState::Stopped,
        };

        let (sender, state, client) = create_test_worker(initial_state).await;

        // Send move command to close
        sender
            .send(WorkerCommand::MoveTo {
                old_pos: FULLY_OPENED,
                new_pos: FULLY_CLOSED,
            })
            .await
            .unwrap();

        sleep(Duration::from_millis(100)).await;

        // Verify toggle was called (on = true means moving down)
        let calls = client.toggle_calls.read().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], ("test-123".to_string(), true)); // true = closing/moving down

        // Simulate confirmation
        sender
            .send(WorkerCommand::StatusUpdate {
                new_state: WindowCoveringState {
                    current_position: FULLY_OPENED,
                    target_position: FULLY_CLOSED,
                    position_state: PositionState::MovingDown,
                },
            })
            .await
            .unwrap();

        sleep(Duration::from_secs(3)).await;

        let current_state = state.lock().await;
        assert!(current_state.current_position < FULLY_OPENED);
        assert_eq!(current_state.position_state, PositionState::MovingDown);
    }

    #[tokio::test]
    async fn test_no_action_when_same_position() {
        let initial_state = WindowCoveringState {
            current_position: 50,
            target_position: 50,
            position_state: PositionState::Stopped,
        };

        let (sender, state, client) = create_test_worker(initial_state).await;

        // Send move to same position
        sender
            .send(WorkerCommand::MoveTo {
                old_pos: 50,
                new_pos: 50,
            })
            .await
            .unwrap();

        sleep(Duration::from_millis(100)).await;

        // No toggle should have been called
        let calls = client.toggle_calls.read().await;
        assert_eq!(calls.len(), 0);

        // State should be unchanged
        let current_state = state.lock().await;
        assert_eq!(current_state.current_position, 50);
        assert_eq!(current_state.position_state, PositionState::Stopped);
    }

    #[tokio::test]
    async fn test_reaches_target_and_stops() {
        let initial_state = WindowCoveringState {
            current_position: 95,
            target_position: 95,
            position_state: PositionState::Stopped,
        };

        let config = WindowCoveringConfig {
            opening_time: Duration::from_secs(5),
            closing_time: Duration::from_secs(5),
        };
        let client = FakeComelitClient::new();
        let state = Arc::new(TokioMutex::new(initial_state));
        let (sender, receiver) = mpsc::channel(32);

        let worker = WindowCoveringWorker::new(
            "test-123".to_string(),
            state.clone(),
            client.clone(),
            config,
        );

        tokio::spawn(worker.run(receiver));

        // Move to fully open (only 5% away)
        sender
            .send(WorkerCommand::MoveTo {
                old_pos: 95,
                new_pos: FULLY_OPENED,
            })
            .await
            .unwrap();

        sleep(Duration::from_millis(100)).await;

        // Simulate confirmation
        sender
            .send(WorkerCommand::StatusUpdate {
                new_state: WindowCoveringState {
                    current_position: 95,
                    target_position: FULLY_OPENED,
                    position_state: PositionState::MovingUp,
                },
            })
            .await
            .unwrap();

        // Wait for it to reach target (should be quick, only 5%)
        sleep(Duration::from_secs(2)).await;

        // Should have sent stop command
        let calls = client.toggle_calls.read().await;
        assert!(calls.len() >= 2); // Start + stop
    }
}
