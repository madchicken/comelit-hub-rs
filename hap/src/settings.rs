use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub pairing_code: [u8; 8],
    pub setup_id: Option<String>,
    pub mount_lights: Option<bool>,
    pub mount_window_covering: Option<bool>,
    pub mount_thermo: Option<bool>,
    pub mount_doors: Option<bool>,
    pub mount_doorbells: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            pairing_code: [1, 1, 1, 2, 2, 3, 3, 3],
            setup_id: Some(String::from("XYZK")),
            mount_lights: Some(true),
            mount_window_covering: Some(true),
            mount_thermo: Some(true),
            mount_doors: Some(true),
            mount_doorbells: Some(true),
        }
    }
}
