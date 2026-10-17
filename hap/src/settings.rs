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

/// Video doorbell through the ViP door entry system (see `crate::viper`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DoorbellVideoSettings {
    /// When enabled, rings come from the ViP registration instead of the
    /// Comelit hub MQTT messages.
    pub enabled: bool,
    /// ViP user token (from `viper-client` activation).
    pub viper_token: Option<String>,
    /// Address of the ViP gateway; discovered on the network when unset.
    pub viper_host: Option<String>,
    pub viper_port: u16,
    /// Entrance (or door) of the ViP address book shown when nobody rings.
    pub entrance: Option<String>,
    /// How long we watch a ringing call before releasing our leg.
    pub ring_timeout_secs: u64,
}

impl Default for DoorbellVideoSettings {
    fn default() -> Self {
        DoorbellVideoSettings {
            enabled: false,
            viper_token: None,
            viper_host: None,
            viper_port: 64100,
            entrance: None,
            ring_timeout_secs: 60,
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
    pub prometheus_url: Option<String>,
    pub prometheus_token: Option<String>,
    #[serde(default)]
    pub doorbell_video: DoorbellVideoSettings,
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
            prometheus_url: None,
            prometheus_token: None,
            doorbell_video: DoorbellVideoSettings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = r#"{
        "pairing_code": [1, 1, 1, 2, 2, 3, 3, 3],
        "mount_doorbells": true,
        "window_covering": { "opening_time": 30, "closing_time": 30 },
        "door": { "opening_closing_time": 10, "opened_time": 5 },
        "prometheus_url": null,
        "prometheus_token": null
    }"#;

    #[test]
    fn settings_without_doorbell_video_keep_mqtt_rings() {
        let settings: Settings = serde_json::from_str(LEGACY).unwrap();
        assert!(!settings.doorbell_video.enabled);
        assert_eq!(settings.doorbell_video.viper_port, 64100);
        assert_eq!(settings.doorbell_video.ring_timeout_secs, 60);
    }

    #[test]
    fn partial_doorbell_video_section_uses_defaults() {
        let json = LEGACY.replace(
            "\"prometheus_token\": null",
            r#""prometheus_token": null,
               "doorbell_video": { "enabled": true, "viper_token": "abc", "entrance": "CANCELLO" }"#,
        );
        let settings: Settings = serde_json::from_str(&json).unwrap();
        let video = settings.doorbell_video;
        assert!(video.enabled);
        assert_eq!(video.viper_token.as_deref(), Some("abc"));
        assert_eq!(video.entrance.as_deref(), Some("CANCELLO"));
        assert_eq!(video.viper_host, None);
        assert_eq!(video.ring_timeout_secs, 60);
    }
}
