use comelit_hub_rs::{PowerStatus, WindowCoveringDeviceData};

#[derive(Clone)]
pub(crate) struct WindowCoveringState {
    pub position: u8,
    pub target_position: u8,
    pub position_state: u8,
    pub moving: bool,
    pub opening: bool,
}

impl From<&WindowCoveringDeviceData> for WindowCoveringState {
    fn from(data: &WindowCoveringDeviceData) -> Self {
        let position = 100;
        let moving = data.data.power_status.clone().unwrap_or_default() != PowerStatus::Stopped;
        let opening = data.data.power_status.clone().unwrap_or_default() == PowerStatus::On;

        WindowCoveringState {
            position,
            target_position: position,
            position_state: PositionState::Stopped as u8,
            moving,
            opening,
        }
    }
}

#[repr(u8)]
pub(crate) enum PositionState {
    MovingUp = 0,
    MovingDown = 1,
    Stopped = 2,
}
