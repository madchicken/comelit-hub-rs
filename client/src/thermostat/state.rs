use crate::protocol::out_data_messages::{ClimaMode, ThermoSeason, ThermostatDeviceData};

#[derive(Debug, Clone, Copy, Default)]
pub struct ThermostatState {
    pub temperature: f32,
    pub target_temperature: f32,
    pub heating_cooling_state: TargetHeatingCoolingState,
    pub target_heating_cooling_state: TargetHeatingCoolingState,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
#[repr(u8)]
pub enum TargetHeatingCoolingState {
    #[default]
    Off = 0,
    Heat = 1,
    Cool = 2,
    Auto = 3,
}

impl From<u8> for TargetHeatingCoolingState {
    fn from(value: u8) -> Self {
        match value {
            0 => TargetHeatingCoolingState::Off,
            1 => TargetHeatingCoolingState::Heat,
            2 => TargetHeatingCoolingState::Cool,
            3 => TargetHeatingCoolingState::Auto,
            _ => panic!("Invalid value for TargetHeatingCoolingState"),
        }
    }
}

impl From<TargetHeatingCoolingState> for u8 {
    fn from(value: TargetHeatingCoolingState) -> Self {
        value as u8
    }
}

impl From<&ThermostatDeviceData> for ThermostatState {
    fn from(data: &ThermostatDeviceData) -> Self {
        let temperature = data
            .temperature
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap_or_default()
            / 10.0;

        let target_temperature = data
            .active_threshold
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap_or_default()
            / 10.0;

        let auto_man = data.auto_man.clone().unwrap_or_default();
        let is_off = auto_man == ClimaMode::OffAuto || auto_man == ClimaMode::OffManual;
        let is_auto = auto_man == ClimaMode::Auto;
        let is_winter = data.season.clone().unwrap_or_default() == ThermoSeason::Winter;

        let heating_cooling_state = if is_off {
            TargetHeatingCoolingState::Off
        } else if is_winter {
            TargetHeatingCoolingState::Heat
        } else if is_auto {
            TargetHeatingCoolingState::Auto
        } else {
            TargetHeatingCoolingState::Cool
        };

        Self {
            temperature,
            target_temperature,
            heating_cooling_state,
            target_heating_cooling_state: heating_cooling_state,
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
            sub_type: ObjectSubtype::ClimaTerm,
            status: None,
            description: None,
            temperature: Some("215".to_string()),
            auto_man: Some(ClimaMode::Manual),
            season: Some(ThermoSeason::Winter),
            active_threshold: Some("220".to_string()),
            humidity: None,
            humi_active_threshold: None,
            auto_man_umi: None,
        }
    }

    #[test]
    fn test_winter_manual_is_heat() {
        let data = base_data();
        let state = ThermostatState::from(&data);
        assert_eq!(state.temperature, 21.5);
        assert_eq!(state.target_temperature, 22.0);
        assert_eq!(state.heating_cooling_state, TargetHeatingCoolingState::Heat);
        assert_eq!(state.target_heating_cooling_state, TargetHeatingCoolingState::Heat);
    }

    #[test]
    fn test_summer_manual_is_cool() {
        let mut data = base_data();
        data.season = Some(ThermoSeason::Summer);
        let state = ThermostatState::from(&data);
        assert_eq!(state.heating_cooling_state, TargetHeatingCoolingState::Cool);
    }

    #[test]
    fn test_off_auto_is_off_regardless_of_season() {
        let mut data = base_data();
        data.auto_man = Some(ClimaMode::OffAuto);
        data.season = Some(ThermoSeason::Winter);
        let state = ThermostatState::from(&data);
        assert_eq!(state.heating_cooling_state, TargetHeatingCoolingState::Off);
    }

    #[test]
    fn test_summer_auto_is_auto() {
        let mut data = base_data();
        data.season = Some(ThermoSeason::Summer);
        data.auto_man = Some(ClimaMode::Auto);
        let state = ThermostatState::from(&data);
        assert_eq!(state.heating_cooling_state, TargetHeatingCoolingState::Auto);
    }

    #[test]
    fn test_winter_auto_is_still_heat() {
        // Matches existing HAP behavior: winter takes priority over the
        // auto_man==Auto check, so winter+Auto reduces to Heat, not Auto.
        let mut data = base_data();
        data.season = Some(ThermoSeason::Winter);
        data.auto_man = Some(ClimaMode::Auto);
        let state = ThermostatState::from(&data);
        assert_eq!(state.heating_cooling_state, TargetHeatingCoolingState::Heat);
    }

    #[test]
    fn test_unparseable_values_default_to_zero() {
        let mut data = base_data();
        data.temperature = Some("not-a-number".to_string());
        data.active_threshold = None;
        let state = ThermostatState::from(&data);
        assert_eq!(state.temperature, 0.0);
        assert_eq!(state.target_temperature, 0.0);
    }
}
