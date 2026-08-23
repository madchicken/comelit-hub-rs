// client/src/covering/worker.rs
use std::cmp::{max, min};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc::{self, Sender};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::protocol::client::ComelitClientTrait;

use super::state::{FULLY_CLOSED, FULLY_OPENED, PositionState, WindowCoveringState};

#[derive(Clone, Copy)]
pub struct WindowCoveringConfig {
    pub closing_time: Duration,
    pub opening_time: Duration,
}

/// Final sink for position updates. HAP writes HomeKit characteristics;
/// Matter updates a shared atomic state + fires a `Signal` for subscriptions.
#[async_trait]
pub trait WindowCoveringSink: Send + Sync + 'static {
    async fn update(&self, state: WindowCoveringState);
}

/// Commands sent to the worker task.
enum WorkerCommand {
    /// Initiate movement toward `new_pos` (from HomeKit's target-position write,
    /// or Matter's GoToLiftPercentage/UpOrOpen/DownOrClose). `old_pos` is only
    /// used for logging, never for movement logic (the worker always reads the
    /// authoritative current position from `state`).
    MoveTo { old_pos: u8, new_pos: u8 },

    /// Comelit reported a status change (external move, or confirmation of a
    /// command we sent).
    StatusUpdate { new_state: WindowCoveringState },

    /// Stop any in-progress movement (Matter's mandatory `StopMotion` command;
    /// HAP never sends this today since it has no hold-position characteristic).
    Stop,

    /// Attach (or replace) the sink used to publish position updates.
    SetSink { sink: Box<dyn WindowCoveringSink> },

    /// Shut the worker down.
    Shutdown,
}

/// Internal state machine for the worker.
#[derive(Debug, Clone, Default)]
enum WorkerState {
    #[default]
    Idle,
    WaitingForMoveConfirmation {
        target: u8,
        direction: PositionState,
        sent_at: Instant,
    },
    MovingInternal {
        target: u8,
        direction: PositionState,
        started_at: Instant,
        start_pos: u8,
    },
    MovingExternal {
        direction: PositionState,
        started_at: Instant,
        start_pos: u8,
    },
    #[allow(dead_code)]
    WaitingForStopConfirmation { current_pos: u8 },
}

struct WindowCoveringWorker<C: ComelitClientTrait> {
    id: String,
    state: Arc<TokioMutex<WindowCoveringState>>,
    client: C,
    config: WindowCoveringConfig,
    worker_state: WorkerState,
    sink: Option<Box<dyn WindowCoveringSink>>,
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
            sink: None,
        }
    }

    async fn run(mut self, mut receiver: mpsc::Receiver<WorkerCommand>) {
        let mut position_ticker = tokio::time::interval(Duration::from_secs(1));

        loop {
            tokio::select! {
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
                        Some(WorkerCommand::Stop) => {
                            if let Err(e) = self.handle_stop().await {
                                warn!("Error handling stop: {}", e);
                            }
                        }
                        Some(WorkerCommand::SetSink { sink }) => {
                            self.sink = Some(sink);
                        }
                        Some(WorkerCommand::Shutdown) | None => {
                            info!("Worker for {} shutting down", self.id);
                            break;
                        }
                    }
                }

                _ = position_ticker.tick() => {
                    if let Err(e) = self.update_position().await {
                        warn!("Error updating position: {}", e);
                    }
                }
            }
        }
    }

    async fn handle_move_to(&mut self, old_pos: u8, new_pos: u8) -> Result<()> {
        let current_pos = {
            let state = self.state.lock().await;
            state.current_position
        };

        debug!(
            "handle_move_to for {}: old_target={}, current={}, new_target={}",
            self.id, old_pos, current_pos, new_pos
        );

        if current_pos == new_pos {
            info!(
                "Target position equals current position for {}, no action",
                self.id
            );
            return Ok(());
        }

        let direction = if new_pos > current_pos {
            PositionState::MovingUp
        } else {
            PositionState::MovingDown
        };

        match &self.worker_state {
            WorkerState::MovingInternal { direction: dir, .. }
            | WorkerState::MovingExternal { direction: dir, .. } => {
                info!("Stopping current movement before new move for {}", self.id);
                let on = *dir == PositionState::MovingDown;
                self.client.toggle_device_status(&self.id, on).await?;

                self.worker_state = WorkerState::WaitingForStopConfirmation {
                    current_pos: {
                        let state = self.state.lock().await;
                        state.current_position
                    },
                };

                return Ok(());
            }
            WorkerState::WaitingForMoveConfirmation { .. }
            | WorkerState::WaitingForStopConfirmation { .. } => {
                info!("Already waiting for confirmation, ignoring move request");
                return Ok(());
            }
            WorkerState::Idle => {}
        }

        {
            let mut state = self.state.lock().await;
            state.target_position = new_pos;
            state.position_state = direction;
        }

        info!(
            "Initiating move for {} from {} to {} (direction: {:?})",
            self.id, current_pos, new_pos, direction
        );

        let on = direction == PositionState::MovingUp;
        self.client.toggle_device_status(&self.id, on).await?;

        self.worker_state = WorkerState::WaitingForMoveConfirmation {
            target: new_pos,
            direction,
            sent_at: Instant::now(),
        };

        self.notify_sink().await;
        Ok(())
    }

    /// Stop any in-progress movement (mandatory for Matter's `StopMotion`; HAP
    /// never calls this since HomeKit has no hold-position characteristic here).
    async fn handle_stop(&mut self) -> Result<()> {
        match &self.worker_state {
            WorkerState::MovingInternal { direction, .. }
            | WorkerState::MovingExternal { direction, .. } => {
                let direction = *direction;
                info!("Stopping movement for {} on explicit Stop", self.id);
                let on = direction == PositionState::MovingDown;
                self.client.toggle_device_status(&self.id, on).await?;

                self.worker_state = WorkerState::WaitingForStopConfirmation {
                    current_pos: {
                        let state = self.state.lock().await;
                        state.current_position
                    },
                };
            }
            WorkerState::WaitingForMoveConfirmation { .. }
            | WorkerState::WaitingForStopConfirmation { .. }
            | WorkerState::Idle => {
                debug!("Stop requested for {} but nothing is moving", self.id);
            }
        }
        Ok(())
    }

    async fn handle_status_update(&mut self, new_state: WindowCoveringState) -> Result<()> {
        let new_position_state = new_state.position_state;

        match &self.worker_state {
            WorkerState::Idle => {
                if new_position_state != PositionState::Stopped {
                    let current_pos = {
                        let state = self.state.lock().await;
                        state.current_position
                    };

                    info!(
                        "External movement detected for {} ({:?})",
                        self.id, new_position_state
                    );

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

                    self.notify_sink().await;
                }
            }

            WorkerState::WaitingForMoveConfirmation {
                target,
                direction,
                sent_at,
            } => {
                if new_position_state == *direction {
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
                    if sent_at.elapsed() < Duration::from_secs(3) {
                        debug!(
                            "Ignoring early Stopped status for {} (grace period active)",
                            self.id
                        );
                    } else {
                        warn!(
                            "Received stop while waiting for move confirmation for {}",
                            self.id
                        );
                        self.worker_state = WorkerState::Idle;
                        self.finalize_position().await?;
                    }
                }
            }

            WorkerState::MovingInternal {
                target, direction, ..
            } => {
                if new_position_state == PositionState::Stopped {
                    info!("Internal movement stopped for {}", self.id);
                    let target = *target;
                    let direction = *direction;
                    self.worker_state = WorkerState::Idle;
                    self.finalize_position_with_target(target, direction)
                        .await?;
                }
            }

            WorkerState::MovingExternal { .. } => {
                if new_position_state == PositionState::Stopped {
                    info!("External movement stopped for {}", self.id);
                    self.worker_state = WorkerState::Idle;
                    self.finalize_position().await?;
                }
            }

            WorkerState::WaitingForStopConfirmation { .. } => {
                if new_position_state == PositionState::Stopped {
                    info!("Stop confirmed for {}", self.id);
                    self.worker_state = WorkerState::Idle;
                    self.finalize_position().await?;
                }
            }
        }

        Ok(())
    }

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
            _ => return Ok(()),
        };

        let elapsed = started_at.elapsed();
        let travel_time = if direction == PositionState::MovingUp {
            self.config.opening_time
        } else {
            self.config.closing_time
        };

        let position_change =
            (elapsed.as_secs_f32() / travel_time.as_secs_f32() * 100.0).round() as i16;

        let new_position = if direction == PositionState::MovingUp {
            min(FULLY_OPENED, (start_pos as i16 + position_change) as u8)
        } else {
            max(FULLY_CLOSED as i16, start_pos as i16 - position_change) as u8
        };

        let reached_target = if let Some(target) = target {
            if direction == PositionState::MovingUp {
                new_position >= target
            } else {
                new_position <= target
            }
        } else {
            false
        };

        {
            let mut state = self.state.lock().await;
            state.current_position = new_position;
            debug!(
                "Position update for {}: {} (target: {:?})",
                self.id, new_position, target
            );
        }

        if reached_target && let Some(target) = target {
            info!(
                "Reached target position {} for {}, sending stop",
                target, self.id
            );
            let opening = direction == PositionState::MovingDown;
            self.client.toggle_device_status(&self.id, opening).await?;

            self.worker_state = WorkerState::WaitingForStopConfirmation {
                current_pos: new_position,
            };
        }

        self.notify_sink().await;
        Ok(())
    }

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

        self.notify_sink().await;
        Ok(())
    }

    async fn finalize_position_with_target(
        &mut self,
        target: u8,
        direction: PositionState,
    ) -> Result<()> {
        let mut state = self.state.lock().await;

        let diff = (state.current_position as i16 - target as i16).abs();
        if diff <= 5 {
            state.current_position = target;
        }

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

        self.notify_sink().await;
        Ok(())
    }

    /// Publish the current state to whichever sink is attached (HAP characteristics,
    /// Matter shared attribute state). No-op until `SetSink` has been received.
    async fn notify_sink(&self) {
        if let Some(sink) = &self.sink {
            let state = {
                let s = self.state.lock().await;
                *s
            };
            sink.update(state).await;
        }
    }
}

/// Handle for controlling a spawned window-covering worker. Dropping it shuts
/// the worker task down (best effort).
#[derive(Clone)]
pub struct WindowCoveringHandle {
    command_sender: Sender<WorkerCommand>,
}

impl WindowCoveringHandle {
    pub async fn move_to(&self, old_pos: u8, new_pos: u8) {
        let _ = self
            .command_sender
            .send(WorkerCommand::MoveTo { old_pos, new_pos })
            .await;
    }

    pub async fn status_update(&self, new_state: WindowCoveringState) {
        let _ = self
            .command_sender
            .send(WorkerCommand::StatusUpdate { new_state })
            .await;
    }

    pub async fn stop(&self) {
        let _ = self.command_sender.send(WorkerCommand::Stop).await;
    }

    pub async fn set_sink(&self, sink: Box<dyn WindowCoveringSink>) {
        let _ = self
            .command_sender
            .send(WorkerCommand::SetSink { sink })
            .await;
    }
}

impl Drop for WindowCoveringHandle {
    fn drop(&mut self) {
        let _ = self.command_sender.try_send(WorkerCommand::Shutdown);
    }
}

/// Spawn a worker task for one window covering and return a handle to it.
/// `state` should already hold the initial position (loaded from storage or
/// derived from the latest Comelit status) before calling this.
pub fn spawn_window_covering_worker<C: ComelitClientTrait + 'static>(
    id: String,
    state: Arc<TokioMutex<WindowCoveringState>>,
    client: C,
    config: WindowCoveringConfig,
) -> WindowCoveringHandle {
    let (command_sender, command_receiver) = mpsc::channel::<WorkerCommand>(32);
    let worker = WindowCoveringWorker::new(id, state, client, config);
    tokio::spawn(worker.run(command_receiver));
    WindowCoveringHandle { command_sender }
}

#[cfg(test)]
mod test {
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use dashmap::DashMap;
    use tokio::sync::RwLock;
    use tokio::task::JoinHandle;
    use tokio::time::sleep;

    use crate::protocol::client::State;
    use crate::protocol::client::{ComelitClientError, ComelitClientTrait};
    use crate::protocol::out_data_messages::{
        ActionType, ClimaMode, ClimaOnOff, HomeDeviceData, ThermoSeason,
    };
    use crate::protocol::scanner::MacAddress;

    use super::*;

    #[derive(Clone, Default)]
    pub struct FakeComelitClient {
        pub toggle_calls: Arc<RwLock<Vec<(String, bool)>>>,
        pub action_calls: Arc<RwLock<Vec<(String, ActionType, i32)>>>,
        pub should_fail: Arc<AtomicBool>,
    }

    impl FakeComelitClient {
        pub fn new() -> Self {
            Self::default()
        }
    }

    #[async_trait]
    impl ComelitClientTrait for FakeComelitClient {
        fn mac_address(&self) -> &MacAddress {
            unimplemented!()
        }

        async fn disconnect(&self) -> Result<(), ComelitClientError> {
            Ok(())
        }

        async fn login(&self, _state: State) -> Result<JoinHandle<()>, ComelitClientError> {
            unimplemented!()
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
                return Err(ComelitClientError::Generic("fail".into()));
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

    #[derive(Clone, Default)]
    struct FakeSink {
        updates: Arc<RwLock<Vec<WindowCoveringState>>>,
    }

    #[async_trait]
    impl WindowCoveringSink for FakeSink {
        async fn update(&self, state: WindowCoveringState) {
            self.updates.write().await.push(state);
        }
    }

    fn test_config() -> WindowCoveringConfig {
        WindowCoveringConfig {
            opening_time: Duration::from_millis(500),
            closing_time: Duration::from_millis(500),
        }
    }

    /// A slower config than `test_config`, used by tests that need to observe
    /// an *in-progress* (not yet completed) position update: with the 1s
    /// position-ticker interval, a 500ms travel time reaches the target on
    /// the very first tick, leaving no window to observe a partial position.
    fn slow_test_config() -> WindowCoveringConfig {
        WindowCoveringConfig {
            opening_time: Duration::from_secs(3),
            closing_time: Duration::from_secs(3),
        }
    }

    async fn create_test_worker_with_config(
        state: WindowCoveringState,
        config: WindowCoveringConfig,
    ) -> (
        WindowCoveringHandle,
        Arc<TokioMutex<WindowCoveringState>>,
        FakeComelitClient,
        FakeSink,
    ) {
        let shared_state = Arc::new(TokioMutex::new(state));
        let client = FakeComelitClient::new();
        let handle = spawn_window_covering_worker(
            "test-id".to_string(),
            shared_state.clone(),
            client.clone(),
            config,
        );
        let sink = FakeSink::default();
        handle.set_sink(Box::new(sink.clone())).await;
        sleep(Duration::from_millis(20)).await;
        (handle, shared_state, client, sink)
    }

    async fn create_test_worker(
        state: WindowCoveringState,
    ) -> (
        WindowCoveringHandle,
        Arc<TokioMutex<WindowCoveringState>>,
        FakeComelitClient,
        FakeSink,
    ) {
        create_test_worker_with_config(state, test_config()).await
    }

    #[tokio::test]
    async fn test_move_to_open() {
        let initial = WindowCoveringState {
            current_position: FULLY_CLOSED,
            target_position: FULLY_CLOSED,
            position_state: PositionState::Stopped,
        };
        let (handle, state, client, _sink) =
            create_test_worker_with_config(initial, slow_test_config()).await;

        handle.move_to(FULLY_CLOSED, FULLY_OPENED).await;
        sleep(Duration::from_millis(100)).await;

        {
            let toggles = client.toggle_calls.read().await;
            assert_eq!(toggles.len(), 1);
            assert_eq!(toggles[0], ("test-id".to_string(), true));
        }

        // Simulate Comelit confirmation of the move.
        handle
            .status_update(WindowCoveringState {
                current_position: FULLY_CLOSED,
                target_position: FULLY_OPENED,
                position_state: PositionState::MovingUp,
            })
            .await;

        // Wait for at least one position-ticker tick (1s) while still well
        // short of the 3s travel time, so the position should be partway
        // between fully closed and fully open.
        sleep(Duration::from_millis(1600)).await;

        let current_state = state.lock().await;
        assert!(current_state.current_position > FULLY_CLOSED);
        assert!(current_state.current_position < FULLY_OPENED);
    }

    #[tokio::test]
    async fn test_move_to_close() {
        let initial = WindowCoveringState {
            current_position: FULLY_OPENED,
            target_position: FULLY_OPENED,
            position_state: PositionState::Stopped,
        };
        let (handle, state, client, _sink) =
            create_test_worker_with_config(initial, slow_test_config()).await;

        handle.move_to(FULLY_OPENED, FULLY_CLOSED).await;
        sleep(Duration::from_millis(100)).await;

        {
            let toggles = client.toggle_calls.read().await;
            assert_eq!(toggles.len(), 1);
            assert_eq!(toggles[0], ("test-id".to_string(), false));
        }

        // Simulate Comelit confirmation of the move.
        handle
            .status_update(WindowCoveringState {
                current_position: FULLY_OPENED,
                target_position: FULLY_CLOSED,
                position_state: PositionState::MovingDown,
            })
            .await;

        // Wait for at least one position-ticker tick (1s) while still well
        // short of the 3s travel time.
        sleep(Duration::from_millis(1600)).await;

        let current_state = state.lock().await;
        assert!(current_state.current_position < FULLY_OPENED);
        assert_eq!(current_state.position_state, PositionState::MovingDown);
    }

    #[tokio::test]
    async fn test_no_action_when_same_position() {
        let initial = WindowCoveringState {
            current_position: 50,
            target_position: 50,
            position_state: PositionState::Stopped,
        };
        let (handle, _state, client, _sink) = create_test_worker(initial).await;

        handle.move_to(50, 50).await;
        sleep(Duration::from_millis(50)).await;

        assert_eq!(client.toggle_calls.read().await.len(), 0);
    }

    /// Regression test: when Comelit spuriously reports GoingDown for a stopped
    /// blind, the worker enters MovingExternal and updates the sink with
    /// target = FULLY_CLOSED (0). HomeKit then writes back its desired target
    /// (matching the actual current position). The old code used old_pos (0)
    /// for the no-action check, so 0 != 20 triggered an open command. The fix
    /// uses current_pos (20 == 20) to correctly skip the command.
    #[tokio::test]
    async fn test_no_spurious_move_when_target_equals_current() {
        let initial = WindowCoveringState {
            current_position: 20,
            target_position: 20,
            position_state: PositionState::Stopped,
        };
        let (handle, state, client, _sink) = create_test_worker(initial).await;

        // Simulate: worker entered MovingExternal (spurious Comelit GoingDown report)
        // and notify_sink() published target = FULLY_CLOSED = 0.
        // HomeKit responds by writing its desired target = 20 (old target was 0).
        handle.move_to(FULLY_CLOSED, 20).await;
        sleep(Duration::from_millis(100)).await;

        // No toggle should have been called: current_pos (20) == new_pos (20)
        let calls = client.toggle_calls.read().await;
        assert_eq!(
            calls.len(),
            0,
            "Spurious command sent when target equals current position"
        );

        let current_state = state.lock().await;
        assert_eq!(current_state.current_position, 20);
        assert_eq!(current_state.position_state, PositionState::Stopped);
    }

    #[tokio::test]
    async fn test_external_movement() {
        let initial = WindowCoveringState {
            current_position: 50,
            target_position: 50,
            position_state: PositionState::Stopped,
        };
        let (handle, state, _client, sink) =
            create_test_worker_with_config(initial, slow_test_config()).await;

        // External movement starts (physical button / independent Comelit command).
        handle
            .status_update(WindowCoveringState {
                current_position: 50,
                target_position: 50,
                position_state: PositionState::MovingUp,
            })
            .await;
        sleep(Duration::from_millis(50)).await;

        {
            let s = state.lock().await;
            assert_eq!(s.position_state, PositionState::MovingUp);
            assert_eq!(s.target_position, FULLY_OPENED);
        }
        assert!(!sink.updates.read().await.is_empty());

        // Wait for at least one position-ticker tick so current_position
        // progresses above the starting position.
        sleep(Duration::from_millis(1200)).await;
        {
            let s = state.lock().await;
            assert!(s.current_position > 50);
            assert_eq!(s.position_state, PositionState::MovingUp);
        }

        // External stop: Comelit reports Stopped once the covering settles.
        handle
            .status_update(WindowCoveringState {
                current_position: 70,
                target_position: 70,
                position_state: PositionState::Stopped,
            })
            .await;
        sleep(Duration::from_millis(100)).await;

        let final_state = *state.lock().await;
        assert_eq!(final_state.position_state, PositionState::Stopped);
        assert_eq!(final_state.target_position, final_state.current_position);

        // The sink's last published update should reflect the finalized state.
        let updates = sink.updates.read().await;
        let last = updates.last().expect("expected at least one sink update");
        assert_eq!(last.position_state, PositionState::Stopped);
        assert_eq!(last.target_position, last.current_position);
    }

    #[tokio::test]
    async fn test_reaches_target_and_stops() {
        let initial = WindowCoveringState {
            current_position: 0,
            target_position: 0,
            position_state: PositionState::Stopped,
        };
        let (handle, state, client, _sink) = create_test_worker(initial).await;

        handle.move_to(0, 100).await;
        // Confirm movement started.
        handle
            .status_update(WindowCoveringState {
                current_position: 0,
                target_position: 100,
                position_state: PositionState::MovingUp,
            })
            .await;
        // Travel time is 500ms in test config; wait past it so the position
        // ticker (1s in the worker) — instead, drive it via repeated status
        // updates is not needed: the 1s ticker means this test only checks
        // that a stop toggle is eventually sent once elapsed time exceeds
        // opening_time. Poll for up to 1.5s.
        let mut stopped = false;
        for _ in 0..30 {
            sleep(Duration::from_millis(100)).await;
            if client.toggle_calls.read().await.len() == 2 {
                stopped = true;
                break;
            }
        }
        assert!(stopped, "expected a stop toggle after reaching target");
        let toggles = client.toggle_calls.read().await;
        assert_eq!(toggles[0], ("test-id".to_string(), true));
        assert_eq!(toggles[1], ("test-id".to_string(), false));
        let _ = state; // position is polled on a 1s ticker; not asserted here
    }

    #[tokio::test]
    async fn test_stop_motion_while_moving() {
        let initial = WindowCoveringState {
            current_position: 0,
            target_position: 0,
            position_state: PositionState::Stopped,
        };
        let (handle, _state, client, _sink) = create_test_worker(initial).await;

        handle.move_to(0, 100).await;
        handle
            .status_update(WindowCoveringState {
                current_position: 0,
                target_position: 100,
                position_state: PositionState::MovingUp,
            })
            .await;
        sleep(Duration::from_millis(50)).await;

        handle.stop().await;
        sleep(Duration::from_millis(50)).await;

        let toggles = client.toggle_calls.read().await;
        assert_eq!(toggles.len(), 2, "expected start toggle + stop toggle");
        assert_eq!(toggles[0], ("test-id".to_string(), true));
        assert_eq!(toggles[1], ("test-id".to_string(), false));
    }

    #[tokio::test]
    async fn test_stop_when_idle_is_noop() {
        let initial = WindowCoveringState {
            current_position: 50,
            target_position: 50,
            position_state: PositionState::Stopped,
        };
        let (handle, _state, client, _sink) = create_test_worker(initial).await;

        handle.stop().await;
        sleep(Duration::from_millis(50)).await;

        assert_eq!(client.toggle_calls.read().await.len(), 0);
    }
}
