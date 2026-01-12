use comelit_hub_rs::{WindowCoveringDeviceData, WindowCoveringStatus};
use serde::{Deserialize, Serialize};

const FULLY_OPENED: u8 = 100;
// const FULLY_CLOSED: u8 = 100;

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) struct WindowCoveringState {
    pub(crate) current_position: u8,
    pub(crate) target_position: u8,
    pub(crate) position_state: u8,
    pub(crate) moving: bool,
    pub(crate) opening: bool,
}

impl From<&WindowCoveringDeviceData> for WindowCoveringState {
    fn from(data: &WindowCoveringDeviceData) -> Self {
        // We don't know the position of the blind at the beginning, we only know if it is open
        // or closed and if it is moving
        let position = FULLY_OPENED;
        let moving = data.power_status.clone().unwrap_or_default() != WindowCoveringStatus::Stopped;
        let opening =
            data.power_status.clone().unwrap_or_default() == WindowCoveringStatus::GoingUp;

        let position_state = if moving {
            if opening {
                PositionState::MovingUp as u8
            } else {
                PositionState::MovingDown as u8
            }
        } else {
            PositionState::Stopped as u8
        };
        WindowCoveringState {
            current_position: position,
            target_position: position,
            position_state,
            moving,
            opening,
        }
    }
}

#[repr(u8)]
pub(crate) enum PositionState {
    MovingDown = 0, // Going to the minimum value specified in metadata (min is 0 that is FULLY CLOSED)
    MovingUp = 1, // Going to the maximum value specified in metadata (max is 100 that is FULLY OPENED)
    Stopped = 2,  // Stopped
}
