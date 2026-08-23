use comelit_client_rs::{ClimaMode, DeviceStatus, ThermostatDeviceData};

/// Local, HAP-only humidity/dehumidifier state — out of scope for the Matter
/// bridge (Matter has no direct cluster for active dehumidifier control).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HumidityState {
    pub(crate) humidity: f32,
    pub(crate) target_humidity: f32,
    pub(crate) dehumidifier_active: bool,
    pub(crate) dehumidifier_current_state: u8, // 0=INACTIVE, 1=IDLE, 3=DEHUMIDIFYING
}

impl From<&ThermostatDeviceData> for HumidityState {
    fn from(data: &ThermostatDeviceData) -> Self {
        let humidity = data
            .humidity
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap_or_default();

        let target_humidity = data
            .humi_active_threshold
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap_or_default();

        let auto_man_umi = data.auto_man_umi.clone().unwrap_or_default();
        let dehumidifier_active = !matches!(
            auto_man_umi,
            ClimaMode::None | ClimaMode::OffAuto | ClimaMode::OffManual
        );
        let dehumidifier_current_state = if !dehumidifier_active {
            0
        } else if matches!(data.status, Some(DeviceStatus::On) | Some(DeviceStatus::Running)) {
            3
        } else {
            1
        };

        Self {
            humidity,
            target_humidity,
            dehumidifier_active,
            dehumidifier_current_state,
        }
    }
}
