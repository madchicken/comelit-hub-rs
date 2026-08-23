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
