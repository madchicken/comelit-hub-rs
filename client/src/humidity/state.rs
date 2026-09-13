use crate::protocol::out_data_messages::{ClimaMode, DeviceStatus, ThermostatDeviceData};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HumidityState {
    pub humidity: f32,
    pub target_humidity: f32,
    pub dehumidifier_active: bool,
    pub dehumidifier_current_state: u8, // 0=INACTIVE, 1=IDLE, 3=DEHUMIDIFYING
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

#[cfg(test)]
mod test {
    use super::*;
    use crate::protocol::out_data_messages::{ObjectSubtype, ObjectType};

    fn base_data() -> ThermostatDeviceData {
        ThermostatDeviceData {
            id: "DOM#TH#1".to_string(),
            r#type: ObjectType::Thermostat,
            sub_type: ObjectSubtype::ClimaThermostatDehumidifier,
            status: Some(DeviceStatus::On),
            description: None,
            temperature: None,
            auto_man: None,
            season: None,
            active_threshold: None,
            humidity: Some("55.5".to_string()),
            humi_active_threshold: Some("60".to_string()),
            auto_man_umi: Some(ClimaMode::Manual),
        }
    }

    #[test]
    fn test_active_manual_dehumidifying_is_state_3() {
        let data = base_data();
        let state = HumidityState::from(&data);
        assert_eq!(state.humidity, 55.5);
        assert_eq!(state.target_humidity, 60.0);
        assert!(state.dehumidifier_active);
        assert_eq!(state.dehumidifier_current_state, 3);
    }

    #[test]
    fn test_active_but_not_running_is_idle_state_1() {
        let mut data = base_data();
        data.status = Some(DeviceStatus::Off);
        let state = HumidityState::from(&data);
        assert!(state.dehumidifier_active);
        assert_eq!(state.dehumidifier_current_state, 1);
    }

    #[test]
    fn test_off_manual_is_inactive_state_0() {
        let mut data = base_data();
        data.auto_man_umi = Some(ClimaMode::OffManual);
        let state = HumidityState::from(&data);
        assert!(!state.dehumidifier_active);
        assert_eq!(state.dehumidifier_current_state, 0);
    }

    #[test]
    fn test_off_auto_is_inactive_state_0() {
        let mut data = base_data();
        data.auto_man_umi = Some(ClimaMode::OffAuto);
        let state = HumidityState::from(&data);
        assert!(!state.dehumidifier_active);
        assert_eq!(state.dehumidifier_current_state, 0);
    }

    #[test]
    fn test_unparseable_values_default_to_zero() {
        let mut data = base_data();
        data.humidity = Some("not-a-number".to_string());
        data.humi_active_threshold = None;
        let state = HumidityState::from(&data);
        assert_eq!(state.humidity, 0.0);
        assert_eq!(state.target_humidity, 0.0);
    }
}
