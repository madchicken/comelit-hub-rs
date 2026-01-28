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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoorSettings {
    pub opening_closing_time: u64,
    pub opened_time: u64,
}

impl Default for DoorSettings {
    fn default() -> Self {
        DoorSettings {
            opening_closing_time: 60,
            opened_time: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub pairing_code: [u8; 8],
    pub mount_lights: Option<bool>,
    pub mount_window_covering: Option<bool>,
    pub mount_thermo: Option<bool>,
    pub mount_doors: Option<bool>,
    pub mount_doorbells: Option<bool>,
    pub window_covering: WindowCoveringSettings,
    pub door: DoorSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            pairing_code: [1, 1, 1, 2, 2, 3, 3, 3],
            mount_lights: Some(true),
            mount_window_covering: Some(true),
            mount_thermo: Some(true),
            mount_doors: Some(true),
            mount_doorbells: Some(false),
            window_covering: WindowCoveringSettings::default(),
            door: DoorSettings::default(),
        }
    }
}
