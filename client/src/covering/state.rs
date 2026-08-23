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
