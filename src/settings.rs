use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub mount_lights: Option<bool>,
    pub mount_window_covering: Option<bool>,
    pub mount_thermo: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            mount_lights: None,
            mount_window_covering: None,
            mount_thermo: Some(true),
        }
    }
}
