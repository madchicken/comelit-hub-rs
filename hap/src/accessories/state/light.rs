use std::sync::atomic::AtomicBool;

use comelit_hub_rs::{DeviceStatus, LightDeviceData};

#[derive(Debug)]
pub(crate) struct LightState {
    pub(crate) on: AtomicBool,
}

impl From<&LightDeviceData> for LightState {
    fn from(data: &LightDeviceData) -> Self {
        let on = data.data.status.clone().unwrap_or_default() == DeviceStatus::On;

        Self {
            on: AtomicBool::new(on),
        }
    }
}
