# Matter Window Covering Bridge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the Matter bridge (`matter/`) to expose Comelit window coverings (tapparelle) as bridged `WindowCovering` endpoints, alongside the existing lights, with the same position-estimation behavior HomeKit already has.

**Architecture:** Extract the protocol-agnostic position-estimation worker out of `hap/` into a new `client::covering` module (shared by both bridges via a `WindowCoveringSink` trait), then build a Matter-side `ClusterAsyncHandler` implementation and generalize the Matter bridge's endpoint dispatch from `Vec<LightEntry>` to `Vec<BridgedEntry>` (an enum of `Light`/`WindowCovering` variants).

**Tech Stack:** Rust, `rs-matter` (git rev `e8b0b0cbb20bf312a9c52fc1ee56541037a3b9c9`), `tokio`, `async-trait`, existing `comelit-client-rs` / `comelit-hub-hap` / `comelit-hub-matter` crates in this Cargo workspace.

**Spec:** `docs/superpowers/specs/2026-08-23-matter-window-covering-design.md`

## Global Constraints

- No new external crate dependencies — everything needed (`tokio` full features, `async-trait`) is already present in `client/Cargo.toml`.
- `client/` must stay protocol-agnostic: no `hap::*` or `rs_matter::*` imports there.
- The persisted state file format/path (`./data/<device_id>.json`) must stay byte-for-byte compatible with what `hap/` writes today, so existing state files keep working after the move.
- Every existing HAP window-covering test must keep passing after the move — same assertions, only the sink injection point changes.
- Follow existing code style in each crate (the files read during planning show no `unwrap()` in library code, `Result`-returning constructors, `tracing`/`log` per-crate as already used).

---

## Task 1: Move `WindowCoveringState`/`PositionState` into `client::covering::state`

**Files:**
- Create: `client/src/covering/state.rs`
- Modify: `hap/src/accessories/state/window_covering.rs` (deleted in Task 6, left untouched here)
- Test: inline `#[cfg(test)]` module in `client/src/covering/state.rs`

**Interfaces:**
- Produces: `pub struct WindowCoveringState { pub current_position: u8, pub target_position: u8, pub position_state: PositionState }` (derives `Clone, Copy, Serialize, Deserialize, Debug`), `pub enum PositionState { MovingDown = 0, MovingUp = 1, Stopped = 2 }` (derives `Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug`), `pub const FULLY_OPENED: u8 = 100`, `pub const FULLY_CLOSED: u8 = 0`, `impl From<&WindowCoveringDeviceData> for WindowCoveringState`, `impl WindowCoveringState { pub async fn from_storage(device_id: &str) -> Option<Self>; pub async fn save(&self, device_id: &str) -> anyhow::Result<()>; }`

- [ ] **Step 1: Create the file with state + persistence (no `hap::storage` dependency — reimplemented over `tokio::fs`, same `./data/<id>.json` path convention as `hap::storage::FileStorage::current_dir()` used today)**

```rust
// client/src/covering/state.rs
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::protocol::out_data_messages::{WindowCoveringDeviceData, WindowCoveringStatus};

pub const FULLY_OPENED: u8 = 100;
pub const FULLY_CLOSED: u8 = 0;

#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
pub struct WindowCoveringState {
    pub current_position: u8,
    pub target_position: u8,
    pub position_state: PositionState,
}

impl WindowCoveringState {
    /// Same convention as the previous `hap::storage::FileStorage::current_dir()`
    /// backend: `<current_dir>/data/<device_id>.json`.
    async fn state_file_path(device_id: &str) -> std::io::Result<PathBuf> {
        let dir = std::env::current_dir()?.join("data");
        tokio::fs::create_dir_all(&dir).await?;
        Ok(dir.join(format!("{device_id}.json")))
    }

    pub async fn from_storage(device_id: &str) -> Option<Self> {
        let path = Self::state_file_path(device_id).await.ok()?;
        let bytes = tokio::fs::read(&path).await.ok()?;
        let str = String::from_utf8(bytes).ok()?;
        let stored_state = serde_json::from_str::<WindowCoveringState>(&str).ok()?;
        info!("Loaded state for {device_id}: {str}");
        Some(stored_state)
    }

    pub async fn save(&self, device_id: &str) -> Result<()> {
        let path = Self::state_file_path(device_id).await?;
        let bytes = serde_json::to_vec(self)?;
        tokio::fs::write(&path, bytes).await?;
        Ok(())
    }
}

impl From<&WindowCoveringDeviceData> for WindowCoveringState {
    fn from(data: &WindowCoveringDeviceData) -> Self {
        let moving = data.power_status.clone().unwrap_or_default() != WindowCoveringStatus::Stopped;
        let opening = data.status.clone().unwrap_or_default() == WindowCoveringStatus::GoingUp;

        let position_state = if moving {
            if opening {
                PositionState::MovingUp
            } else {
                PositionState::MovingDown
            }
        } else {
            PositionState::Stopped
        };
        let current_position = if opening { FULLY_CLOSED } else { FULLY_OPENED };
        WindowCoveringState {
            current_position,
            target_position: if moving {
                if opening { FULLY_OPENED } else { FULLY_CLOSED }
            } else {
                current_position
            },
            position_state,
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum PositionState {
    MovingDown = 0,
    MovingUp = 1,
    Stopped = 2,
}

#[cfg(test)]
mod test {
    use super::{FULLY_OPENED, PositionState, WindowCoveringState};

    #[test]
    fn test_decode() {
        let message = r###"
            {"id":"DOM#BL#20.1","type":2,"sub_type":7,"sched_status":"0","status":"0","powerst":"0","open_status":"1","ConsumptionThreshold":-1,"isShiomMis":0,"instant_power":0,"totalConsumption":-1,"isDetached":0,"scale":-1}
        "###;
        let data = serde_json::from_str(message).unwrap();
        let state = WindowCoveringState::from(&data);
        assert_eq!(state.current_position, FULLY_OPENED);
        assert_eq!(state.target_position, FULLY_OPENED);
        assert_eq!(state.position_state, PositionState::Stopped);
    }
}
```

Note: verify the exact module path for `WindowCoveringDeviceData`/`WindowCoveringStatus` re-exports — `client/src/lib.rs` currently does `pub use protocol::out_data_messages::*;`, so within the crate the path `crate::protocol::out_data_messages::{WindowCoveringDeviceData, WindowCoveringStatus}` is correct (same crate, not the public re-export).

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p comelit-client-rs covering::state::test::test_decode -- --nocapture`
Expected: cannot compile yet — `client/src/covering/` isn't wired into `lib.rs`. Skip actually running until Task 4 wires the module; for now just run `cargo build -p comelit-client-rs 2>&1 | tail -30` and confirm the only error is "file not found in module tree" / unresolved `covering` module (proves the file itself has no syntax errors once `mod.rs` in Task 4 references it — use `rustc --edition 2024 --crate-type lib client/src/covering/state.rs -o /dev/null 2>&1 | head -50` as a standalone syntax check instead).

Run: `rustc --edition 2024 --crate-type lib client/src/covering/state.rs -o /dev/null 2>&1 | head -50`
Expected: errors only about unresolved `crate::protocol::out_data_messages` (expected — this file isn't wired into the crate tree yet), no other syntax errors.

- [ ] **Step 3: Commit**

```bash
git add client/src/covering/state.rs
git commit -m "Add client::covering::state module (moved from hap window covering state)"
```

---

## Task 2: Add `client::covering::settings`

**Files:**
- Create: `client/src/covering/settings.rs`

**Interfaces:**
- Produces: `pub struct WindowCoveringSettings { pub opening_time: u64, pub closing_time: u64 }` (derives `Debug, Clone, Serialize, Deserialize`, `Default` impl with 35/35)

- [ ] **Step 1: Create the file**

```rust
// client/src/covering/settings.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowCoveringSettings {
    pub opening_time: u64,
    pub closing_time: u64,
}

impl Default for WindowCoveringSettings {
    fn default() -> Self {
        WindowCoveringSettings {
            opening_time: 35,
            closing_time: 35,
        }
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add client/src/covering/settings.rs
git commit -m "Add client::covering::settings module"
```

---

## Task 3: Move the worker state machine into `client::covering::worker`, add `WindowCoveringSink` and a `Stop` command

**Files:**
- Create: `client/src/covering/worker.rs`

**Interfaces:**
- Consumes: `WindowCoveringState`, `PositionState`, `FULLY_OPENED`, `FULLY_CLOSED` from `super::state` (Task 1); `ComelitClientTrait` from `crate::protocol::client`.
- Produces: `pub trait WindowCoveringSink: Send + Sync + 'static { async fn update(&self, state: WindowCoveringState); }` (via `#[async_trait::async_trait]`, dyn-compatible), `pub struct WindowCoveringConfig { pub closing_time: Duration, pub opening_time: Duration }` (`Clone, Copy`), `pub struct WindowCoveringHandle` with `pub async fn move_to(&self, old_pos: u8, new_pos: u8)`, `pub async fn status_update(&self, new_state: WindowCoveringState)`, `pub async fn stop(&self)`, `pub async fn set_sink(&self, sink: Box<dyn WindowCoveringSink>)`, and a `Drop` impl that sends a best-effort shutdown; `pub fn spawn_window_covering_worker<C: ComelitClientTrait + 'static>(id: String, state: Arc<TokioMutex<WindowCoveringState>>, client: C, config: WindowCoveringConfig) -> WindowCoveringHandle`.

This task both moves the worker (mechanical) and adds one new capability the shared module needs that HAP never required: an explicit `Stop` command, because HomeKit's `WindowCoveringAccessory` has `hold_position` characteristic removed (see `hap/src/accessories/window_covering.rs:564`, unchanged) so HAP never sends an explicit stop — but Matter's `WindowCovering` cluster requires handling the mandatory `StopMotion` command (Task 8). `handle_stop` reuses the exact same "stop a moving covering" logic already duplicated at the top of `handle_move_to` (`hap/src/accessories/window_covering.rs:190-209`), extracted once here.

- [ ] **Step 1: Create the file**

```rust
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
    use crate::protocol::out_data_messages::{
        ActionType, ClimaMode, ClimaOnOff, HomeDeviceData, ThermoSeason,
    };
    use crate::protocol::scanner::MacAddress;
    use crate::protocol::client::{ComelitClientError, ComelitClientTrait};

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

    async fn create_test_worker(
        state: WindowCoveringState,
    ) -> (WindowCoveringHandle, Arc<TokioMutex<WindowCoveringState>>, FakeComelitClient, FakeSink) {
        let shared_state = Arc::new(TokioMutex::new(state));
        let client = FakeComelitClient::new();
        let handle = spawn_window_covering_worker(
            "test-id".to_string(),
            shared_state.clone(),
            client.clone(),
            test_config(),
        );
        let sink = FakeSink::default();
        handle.set_sink(Box::new(sink.clone())).await;
        sleep(Duration::from_millis(20)).await;
        (handle, shared_state, client, sink)
    }

    #[tokio::test]
    async fn test_move_to_open() {
        let initial = WindowCoveringState {
            current_position: 0,
            target_position: 0,
            position_state: PositionState::Stopped,
        };
        let (handle, _state, client, _sink) = create_test_worker(initial).await;

        handle.move_to(0, 100).await;
        sleep(Duration::from_millis(50)).await;

        let toggles = client.toggle_calls.read().await;
        assert_eq!(toggles.len(), 1);
        assert_eq!(toggles[0], ("test-id".to_string(), true));
    }

    #[tokio::test]
    async fn test_move_to_close() {
        let initial = WindowCoveringState {
            current_position: 100,
            target_position: 100,
            position_state: PositionState::Stopped,
        };
        let (handle, _state, client, _sink) = create_test_worker(initial).await;

        handle.move_to(100, 0).await;
        sleep(Duration::from_millis(50)).await;

        let toggles = client.toggle_calls.read().await;
        assert_eq!(toggles.len(), 1);
        assert_eq!(toggles[0], ("test-id".to_string(), false));
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

    #[tokio::test]
    async fn test_external_movement() {
        let initial = WindowCoveringState {
            current_position: 50,
            target_position: 50,
            position_state: PositionState::Stopped,
        };
        let (handle, state, _client, sink) = create_test_worker(initial).await;

        handle
            .status_update(WindowCoveringState {
                current_position: 50,
                target_position: 50,
                position_state: PositionState::MovingUp,
            })
            .await;
        sleep(Duration::from_millis(50)).await;

        let s = state.lock().await;
        assert_eq!(s.position_state, PositionState::MovingUp);
        assert_eq!(s.target_position, FULLY_OPENED);
        drop(s);
        assert!(!sink.updates.read().await.is_empty());
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
```

Note for the implementer: `hap/src/accessories/window_covering.rs` had six tests
(`test_move_to_open`, `test_external_movement`, `test_move_to_close`,
`test_no_action_when_same_position`, `test_no_spurious_move_when_target_equals_current`,
`test_reaches_target_and_stops`). The test module above ports the first five
(rewritten against the new `WindowCoveringHandle`/`FakeSink` API — same
assertions, different call surface) and adds two new ones for the `Stop`
command. Before moving on, re-open the original file at
`hap/src/accessories/window_covering.rs:1078-1199` (git history, since Task 6
deletes it) to port `test_no_spurious_move_when_target_equals_current` across
using the same pattern as the other ported tests — it is a regression test for
a real past bug (see the comment above it in the original file) and must not
be dropped.

- [ ] **Step 2: Run tests to verify they fail to compile (module not wired yet)**

Run: `rustc --edition 2024 --crate-type lib client/src/covering/worker.rs -o /dev/null 2>&1 | head -50`
Expected: errors only about unresolved `crate::protocol::*` paths (module not wired into the crate tree yet) — no other syntax errors.

- [ ] **Step 3: Commit**

```bash
git add client/src/covering/worker.rs
git commit -m "Add client::covering::worker module with WindowCoveringSink and Stop command"
```

---

## Task 4: Wire `client::covering` into the crate and run its tests

**Files:**
- Create: `client/src/covering/mod.rs`
- Modify: `client/src/lib.rs`

**Interfaces:**
- Produces: `pub mod covering;` accessible as `comelit_client_rs::covering::{WindowCoveringState, PositionState, FULLY_OPENED, FULLY_CLOSED, WindowCoveringConfig, WindowCoveringSettings, WindowCoveringSink, WindowCoveringHandle, spawn_window_covering_worker}`.

- [ ] **Step 1: Create `client/src/covering/mod.rs`**

```rust
// client/src/covering/mod.rs
mod settings;
mod state;
mod worker;

pub use settings::WindowCoveringSettings;
pub use state::{FULLY_CLOSED, FULLY_OPENED, PositionState, WindowCoveringState};
pub use worker::{
    WindowCoveringConfig, WindowCoveringHandle, WindowCoveringSink, spawn_window_covering_worker,
};
```

- [ ] **Step 2: Add the module to `client/src/lib.rs`**

Modify `client/src/lib.rs` (currently):

```rust
mod protocol;

pub use protocol::client::*;
pub use protocol::credentials::get_secrets;
pub use protocol::out_data_messages::*;
pub use protocol::scanner::{MacAddress, Scanner};
```

to:

```rust
mod protocol;
pub mod covering;

pub use protocol::client::*;
pub use protocol::credentials::get_secrets;
pub use protocol::out_data_messages::*;
pub use protocol::scanner::{MacAddress, Scanner};
```

- [ ] **Step 3: Build and run the new tests**

Run: `cargo build -p comelit-client-rs 2>&1 | tail -60`
Expected: builds cleanly. Fix any import-path mismatches surfaced now (e.g. adjust `crate::protocol::client::{ComelitClientError, ComelitClientTrait, State}` / `crate::protocol::out_data_messages::{...}` / `crate::protocol::scanner::MacAddress` paths in `worker.rs`'s test module to whatever `sb map client/src/protocol/client.rs` and `sb map client/src/protocol/scanner.rs` show as the real paths, since this plan was drafted from outline-level reads).

Run: `cargo test -p comelit-client-rs covering:: -- --nocapture`
Expected: `test_decode`, `test_move_to_open`, `test_move_to_close`, `test_no_action_when_same_position`, `test_external_movement`, `test_reaches_target_and_stops`, `test_no_spurious_move_when_target_equals_current`, `test_stop_motion_while_moving`, `test_stop_when_idle_is_noop` — all PASS.

- [ ] **Step 4: Commit**

```bash
git add client/src/covering/mod.rs client/src/lib.rs
git commit -m "Wire client::covering module into comelit-client-rs"
```

---

## Task 5: Migrate `hap/` window covering to the shared `client::covering` module

**Files:**
- Modify: `hap/src/accessories/window_covering.rs` (rewritten as a thin adapter)
- Delete: `hap/src/accessories/state/window_covering.rs`
- Modify: `hap/src/accessories/state/mod.rs`

**Interfaces:**
- Consumes: `comelit_client_rs::covering::{WindowCoveringState, WindowCoveringConfig, WindowCoveringSink, WindowCoveringHandle, spawn_window_covering_worker}`.
- Produces: `pub(crate) struct ComelitWindowCoveringAccessory` (same public shape as before — `ComelitAccessory<WindowCoveringDeviceData>` impl, `Drop` impl now a no-op since `WindowCoveringHandle`'s own `Drop` handles shutdown), `pub(crate) struct WindowCoveringConfig` re-exported for `hap/src/bridge.rs` (unchanged call site at `hap/src/bridge.rs:399-407`).

- [ ] **Step 1: Remove the old state module**

```bash
git rm hap/src/accessories/state/window_covering.rs
```

Edit `hap/src/accessories/state/mod.rs` — remove the line `pub(crate) mod window_covering;`.

- [ ] **Step 2: Rewrite `hap/src/accessories/window_covering.rs` as an adapter**

```rust
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
use tracing::{debug, info};

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
    accessory: Accessory,
}

#[async_trait]
impl WindowCoveringSink for HapWindowCoveringSink {
    async fn update(&self, state: WindowCoveringState) {
        let mut accessory = self.accessory.lock().await;
        let Some(service) = accessory.get_mut_service(HapType::WindowCovering) else {
            return;
        };

        if let Some(characteristic) = service.get_mut_characteristic(HapType::CurrentPosition) {
            let _ = characteristic
                .update_value(Value::from(state.current_position))
                .await;
        }
        if let Some(characteristic) = service.get_mut_characteristic(HapType::TargetPosition) {
            let _ = characteristic
                .update_value(Value::from(state.target_position))
                .await;
        }
        if let Some(characteristic) = service.get_mut_characteristic(HapType::PositionState) {
            let _ = characteristic
                .update_value(Value::from(state.position_state as u8))
                .await;
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

        Self::setup_update_target_position(&mut wc_accessory, handle.clone_sender()).await;

        let accessory = server.add_accessory(wc_accessory).await?;

        handle
            .set_sink(Box::new(HapWindowCoveringSink {
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
```

This introduces two API needs not yet in `WindowCoveringHandle` from Task 3:
`Clone` (so `setup_update_target_position`'s `on_update_async` closure and
the final `self.handle = handle` can both hold a handle to the same worker —
`HapCharacteristic::on_update_async` requires a `'static` owned closure), and
`clone_sender()`/passing the handle itself into the closure. Go back to
`client/src/covering/worker.rs` (Task 3) now and:
1. Add `#[derive(Clone)]` to `WindowCoveringHandle` — this requires
   `Sender<WorkerCommand>` to already be `Clone` (it is, `tokio::sync::mpsc::Sender`
   implements `Clone`), so `#[derive(Clone)]` on `WindowCoveringHandle` works
   as-is, no `clone_sender()` method needed. Remove the `handle.clone_sender()`
   call above and replace with `handle.clone()`.
2. Re-run `cargo test -p comelit-client-rs covering::` to confirm the derive
   didn't break anything (it won't — pure additive derive).

Update the snippet above: replace
`Self::setup_update_target_position(&mut wc_accessory, handle.clone_sender()).await;`
with
`Self::setup_update_target_position(&mut wc_accessory, handle.clone()).await;`,
and the `setup_update_target_position` signature's `handle: WindowCoveringHandle`
parameter stays as-is (it now receives a cloned handle).

- [ ] **Step 3: Add the `Clone` derive to `WindowCoveringHandle`**

In `client/src/covering/worker.rs`, change:

```rust
pub struct WindowCoveringHandle {
    command_sender: Sender<WorkerCommand>,
}
```

to:

```rust
#[derive(Clone)]
pub struct WindowCoveringHandle {
    command_sender: Sender<WorkerCommand>,
}
```

Note: with `#[derive(Clone)]`, every clone's `Drop` impl will try to send
`Shutdown` when dropped — since `mpsc::Sender::send` on a channel already
closed/shutting-down is a harmless no-op (`try_send` result is discarded),
this is safe: only the last handle's drop actually matters, earlier drops
just attempt a redundant send that's ignored if the receiver already stopped.

- [ ] **Step 4: Build `hap/` and fix any remaining import mismatches**

Run: `cargo build -p comelit-hub-hap 2>&1 | tail -80`
Expected: builds cleanly after fixing any path mismatches (verify with
`sb map hap/src/accessories/mod.rs` that `WindowCoveringConfig` is still
re-exported the same way at `hap/src/accessories/mod.rs:15` — it should be,
since it's now `pub use comelit_client_rs::covering::WindowCoveringConfig;`
inside `window_covering.rs`, re-exported the same as before).

- [ ] **Step 5: Run the full hap test suite**

Run: `cargo test -p comelit-hub-hap 2>&1 | tail -100`
Expected: all tests PASS, including every window-covering test now living in
`comelit-client-rs` (they run under that crate's test binary, not `hap`'s —
confirm with `cargo test -p comelit-client-rs 2>&1 | tail -40` as well) and
every other pre-existing `hap` test (door, thermostat, doorbell, etc.)
unaffected by this change.

- [ ] **Step 6: Commit**

```bash
git add hap/src/accessories/window_covering.rs hap/src/accessories/state/mod.rs
git commit -m "Migrate hap window covering accessory to shared client::covering module"
```

---

## Task 6: Add device-type constant and IDL cross-check for `WindowCovering`

**Files:**
- Read-only investigation task, no file changes — produces the exact constant used in Task 8.

**Interfaces:**
- Produces: confirmed `dtype`/`drev` values for `DEV_TYPE_WINDOW_COVERING`, and confirmation of the `rs-matter` cluster ID for `WindowCovering`.

- [ ] **Step 1: Confirm the cluster ID and device type ID from the vendored `rs-matter` checkout**

Run:
```bash
RM=$(find ~/.cargo/git/checkouts -maxdepth 1 -iname "rs-matter-*" | head -1)/$(ls $(find ~/.cargo/git/checkouts -maxdepth 1 -iname "rs-matter-*" | head -1))
grep -n "cluster WindowCovering" "$RM/rs-matter-codegen/src/idl/parser/controller-clusters-V1.5.1.0.matter"
```
Expected: `cluster WindowCovering = 258 {` — confirms cluster ID `0x0102` (258 decimal), matching the design spec.

- [ ] **Step 2: Determine the Window Covering device type ID**

The Matter Device Library defines Window Covering device type as `0x0202`
(revision varies by Device Library version — this plan uses `drev: 1` as
the safe floor since `rs-matter`'s own `DEV_TYPE_ON_OFF_LIGHT` documents that
rev bumps are backward-conformant and rev-1 is always the mandatory-only
baseline). Grep the IDL parser directory for a device-type listing to
double check no lower rev number applies:

Run: `grep -rn "0x0202\|WindowCovering" "$RM/rs-matter-codegen/src/idl/parser/" 2>/dev/null | grep -i device`
Expected: likely no direct hit (rs-matter-codegen's parser directory holds
cluster IDL, not the device-type library XML) — if nothing is found, proceed
with `drev: 1` as documented in the spec's "Rischi e aperture" section; this
is a deliberately conservative choice (mandatory-cluster-set-only) and safe
to ship. No code change in this task — Task 8 uses the constant directly.

---

## Task 7: Add `ComelitCoveringHandler` (Matter `ClusterAsyncHandler` for `WindowCovering`)

**Files:**
- Create: `matter/src/covering.rs`
- Modify: `matter/src/main.rs` (add `mod covering;`)

**Interfaces:**
- Consumes: `comelit_client_rs::covering::{WindowCoveringHandle, WindowCoveringSink, WindowCoveringState, PositionState, FULLY_OPENED, FULLY_CLOSED, spawn_window_covering_worker, WindowCoveringConfig}`; `rs_matter::dm::clusters::decl::window_covering` (generated cluster module).
- Produces: `pub struct CoveringState { pub ep_id: u16, pub device_id: String, pub current_position: AtomicU8, pub target_position: AtomicU8, pub position_state: AtomicU8, pub signal: Signal<CriticalSectionRawMutex, ()>, pub handle: WindowCoveringHandle }`, `pub struct ComelitCoveringHandler` implementing `window_covering::ClusterAsyncHandler`, `pub struct MultiCoveringObserver { pub states: Vec<Arc<CoveringState>> }` implementing `StatusUpdate`.

- [ ] **Step 1: Create `matter/src/covering.rs`**

```rust
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
use rs_matter::dm::{Dataver, InvokeContext, ReadContext, WriteContext};
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
    pub current_position: AtomicU8,
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
        .with_revision(1)
        .with_features(covering_cluster::Feature::LIFT.bits() | covering_cluster::Feature::POSITION_AWARE_LIFT.bits())
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

    async fn r#type(&self, _ctx: impl ReadContext) -> Result<Type, Error> {
        Ok(Type::RollerShutter)
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
        Ok(Nullable::some(
            self.state.current_position.load(Ordering::Relaxed),
        ))
    }

    async fn current_position_lift_percent_100_ths(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<Nullable<rs_matter::im::Percent100ths>, Error> {
        let pct = self.state.current_position.load(Ordering::Relaxed) as u16;
        Ok(Nullable::some(pct * 100))
    }

    async fn target_position_lift_percent_100_ths(
        &self,
        _ctx: impl ReadContext,
    ) -> Result<Nullable<rs_matter::im::Percent100ths>, Error> {
        let pct = self.state.target_position.load(Ordering::Relaxed) as u16;
        Ok(Nullable::some(pct * 100))
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
        let pct = self.state.current_position.load(Ordering::Relaxed) as u16;
        Ok(Nullable::some(pct * 100))
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
        let target = (percent_100ths / 100).min(FULLY_OPENED as u16) as u8;
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
```

Notes for the implementer to verify against the actual generated code while
wiring this up (the codegen output was read at
`target/debug/build/rs-matter-eecddef0e80cd346/out/clusters_generated/window_covering.rs`,
which is a build artifact, not checked-in source — re-run `cargo build -p
comelit-hub-matter` first if that path doesn't exist locally, or find the
current one with `find target -path "*rs-matter-*/out/clusters_generated/window_covering.rs" | head -1`):
- Confirm `Nullable::some(value)` is the correct constructor (mirrors
  `Nullable::none()` already used in `matter/src/light.rs:109`) —
  `sb show` or `grep -n "impl.*Nullable" $(find ~/.cargo/git/checkouts -maxdepth 1 -iname 'rs-matter-*')/*/rs-matter/src/tlv/*.rs` if it errors.
  If the real name differs (e.g. `Nullable::new(Some(value))`), adjust every
  `Nullable::some(...)` call above accordingly — same fix everywhere.
- Confirm `covering_cluster::FULL_CLUSTER` exists (mirrors
  `on_off_cluster::FULL_CLUSTER` used in `matter/src/light.rs:71`) by grepping
  the generated file for `FULL_CLUSTER`.
- Confirm bitflags types (`ConfigStatus`, `Mode`, `SafetyStatus`,
  `OperationalStatus`) support `::empty()` and `from_bits_truncate` (standard
  `bitflags!`-crate methods — the codegen output at lines 1-75 of the
  generated file confirms these are real `bitflags!` macro invocations, so
  both methods are available).
- `#[repr(u8)]` `EndProductType`/`Type` values `RollerShutter` were confirmed
  to exist during design (see spec) — no change needed unless the enum
  variant names differ after a `rs-matter` version bump.

- [ ] **Step 2: Add the module to `matter/src/main.rs`**

Add `mod covering;` next to the existing `mod bridge; mod light; mod mdns;`
at the top of `matter/src/main.rs:1-3`.

- [ ] **Step 3: Build**

Run: `cargo build -p comelit-hub-matter 2>&1 | tail -100`
Expected: does not build yet — `ComelitCoveringHandler` isn't wired into
`bridge.rs`/`main.rs` (Tasks 8-9), and this file alone is not yet reachable
from the crate root via any consumer other than the `mod covering;`
declaration, so the compiler should report only unused-code warnings, not
errors, once Tasks 8-9 land — for now, expect and inspect any type errors
against the generated trait's real signatures (adjust per the notes above)
before proceeding. Do not move to Task 8 until this file compiles standalone
(`cargo build -p comelit-hub-matter` reports zero errors — warnings about
unused `pub` items are fine at this stage).

- [ ] **Step 4: Commit**

```bash
git add matter/src/covering.rs matter/src/main.rs
git commit -m "Add ComelitCoveringHandler: Matter WindowCovering cluster handler"
```

---

## Task 8: Generalize `matter/src/bridge.rs` to `BridgedEntry` enum dispatch

**Files:**
- Modify: `matter/src/bridge.rs`

**Interfaces:**
- Consumes: `crate::covering::ComelitCoveringHandler` (Task 7).
- Produces: `pub enum BridgedEntry { Light(LightEntry), WindowCovering(CoveringEntry) }`, `pub struct CoveringEntry { pub ep_id: u16, pub window_covering: ComelitCoveringHandler, pub desc: desc::DescHandler<'static>, pub groups: groups::GroupsHandler, pub bridged: BridgedInfo }`, `ComelitBridgeHandler` now holds `entries: Vec<BridgedEntry>` instead of `lights: Vec<LightEntry>`, `BridgeMetadata::new` now takes `&[BridgedEntry]`.

- [ ] **Step 1: Add imports and device-type/cluster statics for `WindowCovering`**

At the top of `matter/src/bridge.rs`, extend the existing `use` block:

```rust
use rs_matter::dm::clusters::app::on_off::{self, ClusterAsyncHandler as _, NoLevelControl, OnOffHooks as _};
use rs_matter::dm::clusters::decl::bridged_device_basic_information::{
    self, ClusterHandler as BridgedCH, KeepActiveRequest,
};
use rs_matter::dm::clusters::decl::desc::{self, ClusterHandler as DescCH};
use rs_matter::dm::clusters::decl::groups::{self, ClusterHandler as GroupsCH};
use rs_matter::dm::clusters::decl::window_covering::{self as covering_cluster, ClusterAsyncHandler as _};
use rs_matter::dm::devices::{DEV_TYPE_AGGREGATOR, DEV_TYPE_BRIDGED_NODE, DEV_TYPE_ON_OFF_LIGHT};
use rs_matter::dm::{
    AsyncHandler, Async as DmAsync, Cluster, Dataver, DeviceType, Endpoint, HandlerContext,
    InvokeContext, InvokeReply, MatchContext, Matcher, Metadata, Node, ReadContext, ReadReply,
    WriteContext,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{TLVBuilderParent, Utf8StrBuilder};
use rs_matter::utils::select::Coalesce;
use rs_matter::{root_endpoint, with};

use crate::covering::ComelitCoveringHandler;
use crate::light::ComelitOnOffHooks;
```

Note: `desc::{self, ClusterHandler as DescCH}` and `groups::{self, ClusterHandler as GroupsCH}`
paths above are written to match how `on_off` and `bridged_device_basic_information`
are imported (via `rs_matter::dm::clusters::decl::`); verify against the
existing file whether `desc`/`groups` were imported from
`rs_matter::dm::clusters::desc`/`groups` directly (shorter path, no `decl::`)
as the outline read earlier suggested (`use rs_matter::dm::clusters::desc::{self, ClusterHandler as DescCH};`
with no `decl::` segment) — keep whatever the existing file already does for
`desc`/`groups` unchanged, only add the new `covering_cluster` import line.

Add new statics after the existing `LIGHT_DEVICE_TYPES`/`LIGHT_CLUSTERS`:

```rust
const DEV_TYPE_WINDOW_COVERING: DeviceType = DeviceType { dtype: 0x0202, drev: 1 };

static COVERING_DEVICE_TYPES: [DeviceType; 2] = [DEV_TYPE_WINDOW_COVERING, DEV_TYPE_BRIDGED_NODE];
static COVERING_CLUSTERS: [Cluster<'static>; 4] = [
    desc::DescHandler::CLUSTER,
    groups::GroupsHandler::CLUSTER,
    <BridgedInfo as BridgedCH>::CLUSTER,
    ComelitCoveringHandler::CLUSTER,
];
```

- [ ] **Step 2: Add `CoveringEntry` and the `BridgedEntry` enum**

After the existing `LightEntry` struct:

```rust
/// All handlers and shared state for a single bridged window-covering endpoint.
pub struct CoveringEntry {
    pub ep_id: u16,
    pub window_covering: ComelitCoveringHandler,
    pub desc: desc::DescHandler<'static>,
    pub groups: groups::GroupsHandler,
    pub bridged: BridgedInfo,
}

/// One bridged endpoint: either a light or a window covering.
pub enum BridgedEntry {
    Light(LightEntry),
    WindowCovering(CoveringEntry),
}

impl BridgedEntry {
    fn ep_id(&self) -> u16 {
        match self {
            BridgedEntry::Light(l) => l.ep_id,
            BridgedEntry::WindowCovering(c) => c.ep_id,
        }
    }
}
```

- [ ] **Step 3: Change `ComelitBridgeHandler` to hold `Vec<BridgedEntry>`**

Replace:

```rust
pub struct ComelitBridgeHandler {
    agg_desc: desc::DescHandler<'static>,
    lights: Vec<LightEntry>,
}

impl ComelitBridgeHandler {
    pub fn new(agg_desc: desc::DescHandler<'static>, lights: Vec<LightEntry>) -> Self {
        Self { agg_desc, lights }
    }
```

with:

```rust
pub struct ComelitBridgeHandler {
    agg_desc: desc::DescHandler<'static>,
    entries: Vec<BridgedEntry>,
}

impl ComelitBridgeHandler {
    pub fn new(agg_desc: desc::DescHandler<'static>, entries: Vec<BridgedEntry>) -> Self {
        Self { agg_desc, entries }
    }
```

(leave `select_all` unchanged).

- [ ] **Step 4: Rewrite `read`/`write`/`invoke`/`bump_dataver`/`run` to dispatch on the enum**

Replace the whole `impl AsyncHandler for ComelitBridgeHandler` block with:

```rust
impl AsyncHandler for ComelitBridgeHandler {
    async fn read(
        &self,
        ctx: impl ReadContext,
        reply: impl ReadReply,
    ) -> Result<(), Error> {
        let ep_id = ctx.attr().endpoint_id;
        let cluster_id = ctx.attr().cluster_id;

        if ep_id == 1 {
            return DmAsync(desc::HandlerAdaptor(&self.agg_desc)).read(ctx, reply).await;
        }

        match self.entries.iter().find(|e| e.ep_id() == ep_id) {
            Some(BridgedEntry::Light(light)) => match cluster_id {
                c if c == desc::DescHandler::CLUSTER.id =>
                    DmAsync(desc::HandlerAdaptor(&light.desc)).read(ctx, reply).await,
                c if c == groups::GroupsHandler::CLUSTER.id =>
                    DmAsync(groups::HandlerAdaptor(&light.groups)).read(ctx, reply).await,
                c if c == bridged_device_basic_information::FULL_CLUSTER.id =>
                    DmAsync(bridged_device_basic_information::HandlerAdaptor(&light.bridged)).read(ctx, reply).await,
                c if c == ComelitOnOffHooks::CLUSTER.id =>
                    on_off::HandlerAsyncAdaptor(&light.on_off).read(ctx, reply).await,
                _ => Err(ErrorCode::ClusterNotFound.into()),
            },
            Some(BridgedEntry::WindowCovering(covering)) => match cluster_id {
                c if c == desc::DescHandler::CLUSTER.id =>
                    DmAsync(desc::HandlerAdaptor(&covering.desc)).read(ctx, reply).await,
                c if c == groups::GroupsHandler::CLUSTER.id =>
                    DmAsync(groups::HandlerAdaptor(&covering.groups)).read(ctx, reply).await,
                c if c == bridged_device_basic_information::FULL_CLUSTER.id =>
                    DmAsync(bridged_device_basic_information::HandlerAdaptor(&covering.bridged)).read(ctx, reply).await,
                c if c == ComelitCoveringHandler::CLUSTER.id =>
                    covering_cluster::HandlerAsyncAdaptor(&covering.window_covering).read(ctx, reply).await,
                _ => Err(ErrorCode::ClusterNotFound.into()),
            },
            None => Err(ErrorCode::EndpointNotFound.into()),
        }
    }

    async fn write(&self, ctx: impl WriteContext) -> Result<(), Error> {
        let ep_id = ctx.attr().endpoint_id;
        let cluster_id = ctx.attr().cluster_id;

        match self.entries.iter().find(|e| e.ep_id() == ep_id) {
            Some(BridgedEntry::Light(light)) => match cluster_id {
                c if c == ComelitOnOffHooks::CLUSTER.id =>
                    on_off::HandlerAsyncAdaptor(&light.on_off).write(ctx).await,
                _ => Err(ErrorCode::AttributeNotFound.into()),
            },
            Some(BridgedEntry::WindowCovering(covering)) => match cluster_id {
                c if c == ComelitCoveringHandler::CLUSTER.id =>
                    covering_cluster::HandlerAsyncAdaptor(&covering.window_covering).write(ctx).await,
                _ => Err(ErrorCode::AttributeNotFound.into()),
            },
            None => Err(ErrorCode::EndpointNotFound.into()),
        }
    }

    async fn invoke(
        &self,
        ctx: impl InvokeContext,
        reply: impl InvokeReply,
    ) -> Result<(), Error> {
        let ep_id = ctx.cmd().endpoint_id;
        let cluster_id = ctx.cmd().cluster_id;

        match self.entries.iter().find(|e| e.ep_id() == ep_id) {
            Some(BridgedEntry::Light(light)) => match cluster_id {
                c if c == ComelitOnOffHooks::CLUSTER.id =>
                    on_off::HandlerAsyncAdaptor(&light.on_off).invoke(ctx, reply).await,
                _ => Err(ErrorCode::CommandNotFound.into()),
            },
            Some(BridgedEntry::WindowCovering(covering)) => match cluster_id {
                c if c == ComelitCoveringHandler::CLUSTER.id =>
                    covering_cluster::HandlerAsyncAdaptor(&covering.window_covering).invoke(ctx, reply).await,
                _ => Err(ErrorCode::CommandNotFound.into()),
            },
            None => Err(ErrorCode::EndpointNotFound.into()),
        }
    }

    fn bump_dataver(&self, ctx: impl MatchContext) {
        let ep = ctx.endpt();
        let cl = ctx.cluster();

        if ep.map(|e| e == 1).unwrap_or(true) {
            if cl.map(|c| c == desc::DescHandler::CLUSTER.id).unwrap_or(true) {
                DescCH::dataver_changed(&self.agg_desc);
            }
        }

        for entry in &self.entries {
            if !ep.map(|e| e == entry.ep_id()).unwrap_or(true) {
                continue;
            }
            match entry {
                BridgedEntry::Light(light) => {
                    if cl.map(|c| c == desc::DescHandler::CLUSTER.id).unwrap_or(true) {
                        DescCH::dataver_changed(&light.desc);
                    }
                    if cl.map(|c| c == groups::GroupsHandler::CLUSTER.id).unwrap_or(true) {
                        GroupsCH::dataver_changed(&light.groups);
                    }
                    if cl.map(|c| c == bridged_device_basic_information::FULL_CLUSTER.id).unwrap_or(true) {
                        BridgedCH::dataver_changed(&light.bridged);
                    }
                    if cl.map(|c| c == ComelitOnOffHooks::CLUSTER.id).unwrap_or(true) {
                        on_off::HandlerAsyncAdaptor(&light.on_off).bump_dataver(&ctx);
                    }
                }
                BridgedEntry::WindowCovering(covering) => {
                    if cl.map(|c| c == desc::DescHandler::CLUSTER.id).unwrap_or(true) {
                        DescCH::dataver_changed(&covering.desc);
                    }
                    if cl.map(|c| c == groups::GroupsHandler::CLUSTER.id).unwrap_or(true) {
                        GroupsCH::dataver_changed(&covering.groups);
                    }
                    if cl.map(|c| c == bridged_device_basic_information::FULL_CLUSTER.id).unwrap_or(true) {
                        BridgedCH::dataver_changed(&covering.bridged);
                    }
                    if cl.map(|c| c == ComelitCoveringHandler::CLUSTER.id).unwrap_or(true) {
                        covering_cluster::HandlerAsyncAdaptor(&covering.window_covering).bump_dataver(&ctx);
                    }
                }
            }
        }
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        type DynFut<'a> = Pin<Box<dyn core::future::Future<Output = Result<(), Error>> + 'a>>;
        let futs: Vec<DynFut<'_>> = self
            .entries
            .iter()
            .filter_map(|entry| match entry {
                BridgedEntry::Light(light) => {
                    Some(Box::pin(light.on_off.run(&ctx)) as DynFut<'_>)
                }
                BridgedEntry::WindowCovering(_) => None,
            })
            .collect();

        if futs.is_empty() {
            core::future::pending::<Result<(), Error>>().await
        } else {
            Self::select_all(futs).await
        }
    }
}
```

Note on `run`: `ComelitCoveringHandler` does not override `ClusterAsyncHandler::run`
(Task 7 left it at the trait's default, which is `core::future::pending()` —
confirmed by reading the generated trait source, see Task 7's investigation
notes), so window coverings contribute nothing to the `run` select set here;
subscription updates for covering attributes are picked up by `rs-matter`'s
normal dataver-polling instead of an immediate push (same trade-off flagged
in the design spec). If low-latency push notification is wanted later, add a
`run` override to `ComelitCoveringHandler` that loops on `state.signal.wait()`
the same way `ComelitOnOffHooks::run` does, and include it in this `futs`
collection the same way `light.on_off.run(&ctx)` is included — that is a
follow-up, not required for this plan's scope.

- [ ] **Step 5: Update `BridgeMetadata::new`**

Replace:

```rust
impl BridgeMetadata {
    pub fn new(lights: &[LightEntry]) -> Self {
        let mut endpoints = vec![
            ROOT_EP,
            Endpoint::new(1, &AGG_DEVICE_TYPES, &AGG_CLUSTERS),
        ];
        for light in lights {
            endpoints.push(Endpoint::new(light.ep_id, &LIGHT_DEVICE_TYPES, &LIGHT_CLUSTERS));
        }
        Self { endpoints }
    }
}
```

with:

```rust
impl BridgeMetadata {
    pub fn new(entries: &[BridgedEntry]) -> Self {
        let mut endpoints = vec![
            ROOT_EP,
            Endpoint::new(1, &AGG_DEVICE_TYPES, &AGG_CLUSTERS),
        ];
        for entry in entries {
            match entry {
                BridgedEntry::Light(light) => {
                    endpoints.push(Endpoint::new(light.ep_id, &LIGHT_DEVICE_TYPES, &LIGHT_CLUSTERS));
                }
                BridgedEntry::WindowCovering(covering) => {
                    endpoints.push(Endpoint::new(covering.ep_id, &COVERING_DEVICE_TYPES, &COVERING_CLUSTERS));
                }
            }
        }
        Self { endpoints }
    }
}
```

- [ ] **Step 6: Build (expect failures in `main.rs`, fix in Task 9)**

Run: `cargo build -p comelit-hub-matter 2>&1 | tail -100`
Expected: `matter/src/bridge.rs` itself compiles; `matter/src/main.rs` now
fails to build because it still constructs `Vec<LightEntry>` directly and
calls `ComelitBridgeHandler::new`/`BridgeMetadata::new` with the old type —
this is expected and fixed in Task 9. Confirm the *only* errors are in
`main.rs` (type mismatches on `entries`/`lights` arguments), not in
`bridge.rs` itself.

- [ ] **Step 7: Commit**

```bash
git add matter/src/bridge.rs
git commit -m "Generalize ComelitBridgeHandler to dispatch over BridgedEntry (light | window covering)"
```

---

## Task 9: Wire window-covering discovery, settings loading, and construction into `matter/src/main.rs`

**Files:**
- Modify: `matter/src/main.rs`

**Interfaces:**
- Consumes: `crate::covering::{CoveringState, ComelitCoveringHandler, MatterCoveringSink, MultiCoveringObserver}` (Task 7), `crate::bridge::{BridgedEntry, CoveringEntry}` (Task 8), `comelit_client_rs::covering::{WindowCoveringSettings, WindowCoveringConfig, WindowCoveringState, spawn_window_covering_worker}`.
- Produces: updated `Args` (new `--settings` flag), updated `run_matter` accepting both lights and coverings, updated discovery step.

- [ ] **Step 1: Add the `--settings` CLI flag and a minimal settings struct**

In `matter/src/main.rs`, extend `Args`:

```rust
#[derive(Parser, Debug)]
#[command(name = "comelit-matter", about = "Comelit → Matter bridge (all lights)")]
struct Args {
    #[arg(long, env = "COMELIT_HOST")]
    host: String,

    #[arg(long, env = "COMELIT_USER", default_value = "admin")]
    user: String,

    #[arg(long, env = "COMELIT_PASSWORD", default_value = "admin")]
    password: String,

    /// Path to the same settings JSON file used by the HAP bridge (only the
    /// `window_covering` section is read; every other field is ignored).
    #[arg(long, env = "COMELIT_SETTINGS")]
    settings: Option<String>,
}
```

Add, right after the `Args` struct:

```rust
/// Reads only the `window_covering` section out of the same settings JSON
/// file the HAP bridge uses (`hap::settings::Settings`). Unknown fields
/// (pairing_code, mount_*, prometheus_*, ...) are ignored by serde.
#[derive(serde::Deserialize)]
struct MatterSettings {
    #[serde(default)]
    window_covering: comelit_client_rs::covering::WindowCoveringSettings,
}

impl Default for MatterSettings {
    fn default() -> Self {
        Self {
            window_covering: comelit_client_rs::covering::WindowCoveringSettings::default(),
        }
    }
}

fn load_settings(path: &Option<String>) -> MatterSettings {
    match path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|e| {
                error!("Failed to parse settings file {path}: {e}, using defaults");
                MatterSettings::default()
            }),
            Err(e) => {
                error!("Failed to read settings file {path}: {e}, using defaults");
                MatterSettings::default()
            }
        },
        None => MatterSettings::default(),
    }
}
```

`comelit_client_rs::covering::WindowCoveringSettings` needs `#[serde(default)]`-compatible
`Deserialize` (it already derives `Deserialize` from Task 2) — the outer
`#[serde(default)]` on the `window_covering` field requires `MatterSettings`'s
field type to implement `Default`, which `WindowCoveringSettings` already does
(Task 2's `impl Default for WindowCoveringSettings`). Also add
`use serde_json;` is unnecessary since `serde_json::from_str` is called via
full path — but confirm `serde_json` is a direct dependency of
`matter/Cargo.toml`; if not, add it:

Run: `grep -n "serde_json\|^serde " matter/Cargo.toml`
Expected: if missing, add `serde_json = "1.0"` and `serde = { version = "1", features = ["derive"] }`
to `matter/Cargo.toml`'s `[dependencies]` (both are already pinned versions
used elsewhere in the workspace — check `client/Cargo.toml`'s
`serde_json = "1.0.140"` / `serde = { version = "1.0.219", ... }` and match
those versions for consistency).

- [ ] **Step 2: Extend discovery (step 3 in `main`) to also collect window coverings**

Replace the existing discovery block:

```rust
    // ── 3. Discover all lights ────────────────────────────────────────────────

    let index = client.fetch_index(2).await?;
    let mut lights_data: Vec<(String, String, bool)> = index
        .iter()
        .filter_map(|entry| {
            if let HomeDeviceData::Light(l) = entry.value() {
                let initial_on =
                    l.status.as_ref().map(|s| s == &DeviceStatus::On).unwrap_or(false);
                let label = l.description.clone().unwrap_or_else(|| entry.key().clone());
                Some((entry.key().clone(), label, initial_on))
            } else {
                None
            }
        })
        .collect();

    // Stable order: sort by device ID
    lights_data.sort_by(|a, b| a.0.cmp(&b.0));

    if lights_data.is_empty() {
        return Err(anyhow::anyhow!("No lights found in Comelit index"));
    }

    info!("Discovered {} lights:", lights_data.len());
    for (i, (id, label, on)) in lights_data.iter().enumerate() {
        info!("  ep{}: {} ({}) — {}", i + 2, label, id, if *on { "ON" } else { "OFF" });
    }
```

with:

```rust
    // ── 3. Discover all lights and window coverings ──────────────────────────

    let settings = load_settings(&args.settings);

    let index = client.fetch_index(2).await?;
    let mut lights_data: Vec<(String, String, bool)> = index
        .iter()
        .filter_map(|entry| {
            if let HomeDeviceData::Light(l) = entry.value() {
                let initial_on =
                    l.status.as_ref().map(|s| s == &DeviceStatus::On).unwrap_or(false);
                let label = l.description.clone().unwrap_or_else(|| entry.key().clone());
                Some((entry.key().clone(), label, initial_on))
            } else {
                None
            }
        })
        .collect();
    lights_data.sort_by(|a, b| a.0.cmp(&b.0));

    let mut covering_data: Vec<(String, String, comelit_client_rs::covering::WindowCoveringState)> = index
        .iter()
        .filter_map(|entry| {
            if let HomeDeviceData::WindowCovering(wc) = entry.value() {
                let label = wc.description.clone().unwrap_or_else(|| entry.key().clone());
                let initial_state = comelit_client_rs::covering::WindowCoveringState::from(wc);
                Some((entry.key().clone(), label, initial_state))
            } else {
                None
            }
        })
        .collect();
    covering_data.sort_by(|a, b| a.0.cmp(&b.0));

    if lights_data.is_empty() && covering_data.is_empty() {
        return Err(anyhow::anyhow!("No lights or window coverings found in Comelit index"));
    }

    info!("Discovered {} lights, {} window coverings:", lights_data.len(), covering_data.len());
    let mut next_ep: u16 = 2;
    for (id, label, on) in &lights_data {
        info!("  ep{}: light {} ({}) — {}", next_ep, label, id, if *on { "ON" } else { "OFF" });
        next_ep += 1;
    }
    for (id, label, state) in &covering_data {
        info!("  ep{}: window covering {} ({}) — pos={}", next_ep, label, id, state.current_position);
        next_ep += 1;
    }
```

Note: endpoint IDs are now assigned in a fixed order (all lights first, then
all coverings) rather than interleaved by device ID as the design spec's
"ordinamento stabile per id... si intercalano naturalmente" suggested. This
is a deliberate simplification that keeps `light_states`/`lights_data`
zip-by-index logic in `run_matter` (Task 9 Step 4) unchanged and easy to
verify — endpoint numbering has no semantic meaning to Matter controllers
either way. If true interleaving is later wanted, both loops below (this one
and `run_matter`'s entry construction) need matching changes together.

- [ ] **Step 3: Wire up the covering observer and worker handles (steps 4-5 in `main`)**

Replace step 4 (`Create shared state and wire up observer`) and step 5
(`Subscribe to MQTT push for every light`):

```rust
    // ── 4. Create shared state and wire up observers ──────────────────────────

    let mut light_states: Vec<Arc<LightState>> = Vec::new();
    let mut ep_id: u16 = 2;
    for (id, _, initial_on) in &lights_data {
        let state = Arc::new(LightState::new(ep_id, id.clone(), *initial_on, cmd_tx.clone()));
        state.signal.signal(());
        light_states.push(state);
        ep_id += 1;
    }

    let covering_config = comelit_client_rs::covering::WindowCoveringConfig {
        opening_time: Duration::from_secs(settings.window_covering.opening_time),
        closing_time: Duration::from_secs(settings.window_covering.closing_time),
    };

    let mut covering_states: Vec<Arc<covering::CoveringState>> = Vec::new();
    for (id, _, initial_state) in &covering_data {
        let shared_state = Arc::new(tokio::sync::Mutex::new(*initial_state));
        let handle = comelit_client_rs::covering::spawn_window_covering_worker(
            id.clone(),
            shared_state,
            client.clone(),
            covering_config,
        );
        let state = Arc::new(covering::CoveringState::new(ep_id, id.clone(), *initial_state, handle));
        state.handle.set_sink(Box::new(covering::MatterCoveringSink::new(state.clone()))).await;
        covering_states.push(state);
        ep_id += 1;
    }

    let light_observer = Arc::new(MultiLightObserver { states: light_states.clone() });
    let covering_observer = Arc::new(covering::MultiCoveringObserver { states: covering_states.clone() });

    struct FanOutObserver {
        light: Arc<MultiLightObserver>,
        covering: Arc<covering::MultiCoveringObserver>,
    }

    #[async_trait]
    impl StatusUpdate for FanOutObserver {
        async fn status_update(&self, device: &HomeDeviceData) {
            self.light.status_update(device).await;
            self.covering.status_update(device).await;
        }
    }

    *deferred_slot.write().await = Some(Arc::new(FanOutObserver {
        light: light_observer,
        covering: covering_observer,
    }) as _);

    // ── 5. Subscribe to MQTT push for every discovered device ─────────────────

    for (id, _, _) in &lights_data {
        client.subscribe(id).await?;
    }
    for (id, _, _) in &covering_data {
        client.subscribe(id).await?;
    }
```

Add `mod covering;` was already added in Task 7 — also add
`use covering;` is unnecessary since `covering::` is used with its full
module path above (matches how `bridge::`/`light::` are already imported by
name at the top of the file — for consistency, add
`use crate::covering;` near the existing `use bridge::{...}; use light::{...};`
imports and simplify the `covering::CoveringState`/`covering::MatterCoveringSink`/
`covering::MultiCoveringObserver` references above to drop the `crate::` prefix
if `use covering::*` style matching `bridge`/`light` is preferred — either
form compiles, keep whichever matches the existing file's import style once
you're editing it directly).

- [ ] **Step 4: Update `run_matter` to build `Vec<BridgedEntry>`**

Replace `run_matter`'s signature and light-entry-construction loop:

```rust
fn run_matter(
    light_states: Vec<Arc<LightState>>,
    lights_data: Vec<(String, String, bool)>,
) -> anyhow::Result<()> {
```

with:

```rust
fn run_matter(
    light_states: Vec<Arc<LightState>>,
    lights_data: Vec<(String, String, bool)>,
    covering_states: Vec<Arc<covering::CoveringState>>,
    covering_data: Vec<(String, String, comelit_client_rs::covering::WindowCoveringState)>,
) -> anyhow::Result<()> {
```

and replace the `Build one LightEntry per light` block:

```rust
    // Build one LightEntry per light
    let mut entries: Vec<LightEntry> = Vec::new();
    for (i, (state, (device_id, label, _))) in
        light_states.into_iter().zip(lights_data.iter()).enumerate()
    {
        let ep_id = (i + 2) as u16;
        let hooks = ComelitOnOffHooks::new(state);
        entries.push(LightEntry {
            ep_id,
            on_off: on_off::OnOffHandler::new_standalone(
                Dataver::new_rand(&mut rand),
                ep_id,
                hooks,
            ),
            desc: desc::DescHandler::new(Dataver::new_rand(&mut rand)),
            groups: groups::GroupsHandler::new(Dataver::new_rand(&mut rand)),
            bridged: BridgedInfo::new(
                Dataver::new_rand(&mut rand),
                label.clone(),
                device_id.clone(),
            ),
        });
    }
```

with:

```rust
    // Build one BridgedEntry per light and per window covering.
    let mut entries: Vec<BridgedEntry> = Vec::new();
    for (state, (device_id, label, _)) in light_states.into_iter().zip(lights_data.iter()) {
        let ep_id = state.ep_id;
        let hooks = ComelitOnOffHooks::new(state);
        entries.push(BridgedEntry::Light(LightEntry {
            ep_id,
            on_off: on_off::OnOffHandler::new_standalone(
                Dataver::new_rand(&mut rand),
                ep_id,
                hooks,
            ),
            desc: desc::DescHandler::new(Dataver::new_rand(&mut rand)),
            groups: groups::GroupsHandler::new(Dataver::new_rand(&mut rand)),
            bridged: BridgedInfo::new(
                Dataver::new_rand(&mut rand),
                label.clone(),
                device_id.clone(),
            ),
        }));
    }
    for (state, (device_id, label, _)) in covering_states.into_iter().zip(covering_data.iter()) {
        let ep_id = state.ep_id;
        entries.push(BridgedEntry::WindowCovering(CoveringEntry {
            ep_id,
            window_covering: ComelitCoveringHandler::new(Dataver::new_rand(&mut rand), state),
            desc: desc::DescHandler::new(Dataver::new_rand(&mut rand)),
            groups: groups::GroupsHandler::new(Dataver::new_rand(&mut rand)),
            bridged: BridgedInfo::new(
                Dataver::new_rand(&mut rand),
                label.clone(),
                device_id.clone(),
            ),
        }));
    }
```

Note: this requires `LightState` (in `light.rs`) to expose its `ep_id` field
as `pub` — it already is (`pub ep_id: u16` at `matter/src/light.rs:28`), so
`state.ep_id` works directly without needing the enumerate-based `(i + 2)`
recomputation the original code used. This also means the `light_states`
construction loop in Step 3 above (`let mut ep_id: u16 = 2; ... ep_id += 1;`)
is the single source of truth for endpoint numbering — `run_matter` just
reads `state.ep_id` back off each already-numbered state.

Add imports at the top of `matter/src/main.rs` (alongside the existing
`use bridge::{...}; use light::{...};`):

```rust
use bridge::{BridgeMetadata, BridgedEntry, BridgedInfo, ComelitBridgeHandler, CoveringEntry, LightEntry, NonRootMatcher};
use covering::ComelitCoveringHandler;
```

- [ ] **Step 5: Update the call site that spawns `run_matter`**

Replace:

```rust
    let matter_thread = std::thread::Builder::new()
        .name("matter".into())
        .stack_size(600 * 1024)
        .spawn(move || run_matter(light_states, lights_data))?;
```

with:

```rust
    let matter_thread = std::thread::Builder::new()
        .name("matter".into())
        .stack_size(600 * 1024)
        .spawn(move || run_matter(light_states, lights_data, covering_states, covering_data))?;
```

- [ ] **Step 6: Build the whole workspace**

Run: `cargo build 2>&1 | tail -150`
Expected: builds cleanly across all crates. Work through any remaining type
mismatches — likely candidates given how this plan was drafted from
outline-level reads: exact `desc`/`groups` import paths in `bridge.rs`
(flagged in Task 8 Step 1), the exact `Nullable` constructor name (flagged
in Task 7), and whether `ComelitClient` (concrete type) satisfies
`ComelitClientTrait + 'static` as required by `spawn_window_covering_worker`
(it already does — `client/src/protocol/client.rs` shows `ComelitClient`
implementing the full `ComelitClientTrait`, and it's already used generically
this way by `hap/` after Task 5).

- [ ] **Step 7: Run the full workspace test suite**

Run: `cargo test 2>&1 | tail -150`
Expected: all tests pass across `comelit-client-rs`, `comelit-hub-hap`,
`comelit-hub-matter` (matter has no tests of its own today per the earlier
digest — this just confirms the build-and-link step doesn't break anything).

- [ ] **Step 8: Commit**

```bash
git add matter/src/main.rs matter/Cargo.toml
git commit -m "Discover and bridge Comelit window coverings alongside lights in the Matter bridge"
```

---

## Task 10: Manual smoke check (no hardware) and README note

**Files:**
- Modify: none required, but check `README.md` for any Matter section that
  needs a one-line mention (the earlier investigation found none — if still
  true, skip the doc edit).

**Interfaces:** none (verification-only task).

- [ ] **Step 1: Full workspace check**

Run: `cargo check 2>&1 | tail -100`
Expected: zero warnings introduced beyond what already existed before this
plan (the pre-existing `unused import: std::time::Duration` warning in
`matter/src/main.rs:8` noted during initial investigation should be gone now
since `Duration` is used for `covering_config` construction in Task 9).

- [ ] **Step 2: Dry-run the binary against no real hub (expect a clean connection-refused error, not a panic)**

Run: `cargo run -p comelit-hub-matter -- --host 127.0.0.1 2>&1 | head -30`
Expected: the process attempts to connect to `127.0.0.1` and fails cleanly
with a connection error (there is no Comelit hub there) — this is the same
behavior the binary already had before this plan for a bad `--host`, and
confirms the new `--settings`-optional path and discovery code don't panic
before even reaching the network call.

- [ ] **Step 3: If available, dry-run against a real Comelit hub**

If the user has a real Comelit hub reachable on the network and is willing
to test against it (this step requires the user's explicit go-ahead since it
talks to real hardware and issues real Matter pairing), run:
`cargo run -p comelit-hub-matter -- --host <real-hub-ip>` and confirm the
log output lists both lights and window coverings with their endpoint IDs,
and that the printed QR code / pairing flow still works. Skip this step
entirely if no hardware is available or the user does not want a live test —
report that explicitly rather than claiming it was verified.

---

## Self-Review Notes (for the plan author, already applied above)

- **Spec coverage:** all six numbered design decisions from the spec are
  implemented — position estimation (Tasks 1-3), code sharing via
  `client::covering` (Tasks 1-5), persistence reuse (Task 1's `tokio::fs`
  reimplementation, same path convention), single bridge with mixed
  endpoints (Tasks 8-9), settings file reuse (Task 9 Step 1), enum dispatch
  (Task 8). The spec's two "fuori ambito" items (tilt, other device types)
  are correctly not implemented. The spec's three "rischi e aperture" are
  each addressed: `drev` value picked conservatively with rationale (Task
  6), `Sink` storage strategy resolved concretely as `Box<dyn
  WindowCoveringSink>` inside the worker + `WindowCoveringHandle` wrapper
  (Task 3), `end_product_type` mapping resolved to a fixed `RollerShutter`
  (Task 7).
- **Placeholder scan:** no TBD/TODO markers remain; every step shows real
  code. The few "verify against the generated code" notes (Tasks 6, 7, 8)
  are flagged as such because this plan was drafted by reading a
  build-artifact-generated file that isn't checked into the repo and could
  shift slightly on a `rs-matter` dependency bump — they point to concrete
  `grep`/build commands to resolve, not open-ended guidance.
- **Type consistency:** `WindowCoveringHandle` (Task 3) is used identically
  in Task 5 (HAP) and Task 9 (Matter) — `move_to`, `status_update`, `stop`,
  `set_sink`, `Clone`. `CoveringState` (Task 7) fields (`ep_id`,
  `device_id`, `current_position`, `target_position`, `position_state`,
  `signal`, `handle`) are used with matching names in Task 9's `run_matter`
  and observer wiring. `ComelitCoveringHandler::new(dataver, state)` (Task
  7) matches its call site in Task 9 Step 4.
