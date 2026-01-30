use comelit_client_rs::{DeviceStatus, DoorDeviceData};

pub(crate) const FULLY_OPENED: u8 = 100;
pub(crate) const FULLY_CLOSED: u8 = 0;

#[derive(Clone, Copy, Debug)]
pub(crate) struct DoorState {
    pub(crate) current_position: u8,
    pub(crate) target_position: u8,
    pub(crate) position_state: u8,
}

impl From<&DoorDeviceData> for DoorState {
    fn from(state: &DoorDeviceData) -> Self {
        let status = state.status.as_ref().unwrap_or(&DeviceStatus::Off);
        let position_state = match status {
            DeviceStatus::Running => DoorPositionState::Opening as u8,
            DeviceStatus::Off => DoorPositionState::Stopped as u8,
            DeviceStatus::On => DoorPositionState::Opening as u8,
        };

        let current_position = match status {
            DeviceStatus::Running => FULLY_OPENED,
            DeviceStatus::Off => FULLY_CLOSED,
            DeviceStatus::On => FULLY_OPENED,
        };
        let target_position = current_position;
        DoorState {
            current_position,
            target_position,
            position_state,
        }
    }
}

#[repr(u8)]
pub(crate) enum DoorPositionState {
    Closing = 0, // Going to the minimum value specified in metadata (min is 0 that is FULLY CLOSED)
    Opening = 1, // Going to the maximum value specified in metadata (max is 100 that is FULLY OPENED)
    Stopped = 2, // Stopped
}
