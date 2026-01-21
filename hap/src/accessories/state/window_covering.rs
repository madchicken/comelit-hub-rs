use anyhow::Result;
use comelit_hub_rs::{WindowCoveringDeviceData, WindowCoveringStatus};
use hap::storage::{FileStorage, Storage};
use serde::{Deserialize, Serialize};
use tracing::info;

pub const FULLY_OPENED: u8 = 100;
pub const FULLY_CLOSED: u8 = 0;

#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
pub(crate) struct WindowCoveringState {
    pub(crate) current_position: u8,
    pub(crate) target_position: u8,
    pub(crate) position_state: PositionState,
}

impl WindowCoveringState {
    pub async fn from_storage(device_id: &str) -> Option<Self> {
        if let Ok(t) = FileStorage::current_dir().await {
            let key = &format!("{device_id}.json");
            if let Ok(bytes) = t.load_bytes(key.as_str()).await
                && let Ok(str) = String::from_utf8(bytes)
                && let Ok(stored_state) = serde_json::from_str::<WindowCoveringState>(&str)
            {
                info!("Loaded state for {device_id}: {str}");
                return Some(stored_state);
            }
        }
        None
    }

    pub async fn save(&self, device_id: &str) -> Result<()> {
        let mut t = FileStorage::current_dir().await?;
        let key = &format!("{device_id}.json");
        Ok(t.save_bytes(key, &serde_json::to_vec(self).unwrap())
            .await?)
    }

    pub fn is_moving(&self) -> bool {
        self.position_state != PositionState::Stopped
    }

    pub fn is_opening(&self) -> bool {
        self.current_position < self.target_position
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
pub(crate) enum PositionState {
    MovingDown = 0, // Going to the minimum value specified in metadata (min is 0 that is FULLY CLOSED)
    MovingUp = 1, // Going to the maximum value specified in metadata (max is 100 that is FULLY OPENED)
    Stopped = 2,  // Stopped
}

#[cfg(test)]
mod test {
    use crate::accessories::state::window_covering::{
        FULLY_OPENED, PositionState, WindowCoveringState,
    };

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
