use comelit_client_rs::{ClimaMode, ThermoSeason, ThermostatDeviceData};

#[derive(Debug, Clone, Default)]
pub(crate) struct ThermostatState {
    pub(crate) temperature: f32,
    pub(crate) humidity: f32,
    pub(crate) target_temperature: f32,
    pub(crate) target_humidity: f32,
    pub(crate) heating_cooling_state: TargetHeatingCoolingState,
    pub(crate) target_heating_cooling_state: TargetHeatingCoolingState,
}

impl From<&ThermostatDeviceData> for ThermostatState {
    fn from(data: &ThermostatDeviceData) -> Self {
        let temperature = data
            .temperature
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap()
            / 10.0;

        let humidity = data
            .humidity
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap();

        let target_temperature = data
            .active_threshold
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap()
            / 10.0;

        let target_humidity = data
            .humi_active_threshold
            .clone()
            .unwrap_or_default()
            .parse::<f32>()
            .unwrap();

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

        let target_heating_cooling_state = heating_cooling_state;

        Self {
            temperature,
            humidity,
            target_temperature,
            target_humidity,
            heating_cooling_state,
            target_heating_cooling_state,
        }
    }
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
