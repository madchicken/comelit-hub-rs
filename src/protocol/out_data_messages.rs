use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]
pub(crate) enum ObjectType {
    Other = 1,
    Blind = 2,
    Light = 3,
    Irrigation = 4,
    Thermostat = 9,
    Outlet = 10,
    PowerSupplier = 11,
    Zone = 1001,
}

impl From<i32> for ObjectType {
    fn from(value: i32) -> Self {
        match value {
            1 => Self::Other,
            2 => Self::Blind,
            3 => Self::Light,
            4 => Self::Irrigation,
            9 => Self::Thermostat,
            10 => Self::Outlet,
            11 => Self::PowerSupplier,
            1001 => Self::Zone,
            _ => Self::Other, // Default case
        }
    }
}

impl From<ObjectType> for i32 {
    fn from(value: ObjectType) -> Self {
        match value {
            ObjectType::Other => 1,
            ObjectType::Blind => 2,
            ObjectType::Light => 3,
            ObjectType::Irrigation => 4,
            ObjectType::Thermostat => 9,
            ObjectType::Outlet => 10,
            ObjectType::PowerSupplier => 11,
            ObjectType::Zone => 1001,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]
pub(crate) enum ObjectSubtype {
    Generic = 0,
    DigitalLight = 1,
    RgbLight = 2,
    TemporizedLight = 3,
    DimmerLight = 4,
    OtherDigit = 5,
    OtherTmp = 6,
    ElectricBlind = 7,
    ClimaTerm = 12,
    GenericZone = 13,
    Consumption = 15,
    ClimaThermostatDehumidifier = 16,
    ClimaDehumidifier = 17,
    EnhancedElectricBlind = 31,
}

impl From<i32> for ObjectSubtype {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::Generic,
            1 => Self::DigitalLight,
            2 => Self::RgbLight,
            3 => Self::TemporizedLight,
            4 => Self::DimmerLight,
            5 => Self::OtherDigit,
            6 => Self::OtherTmp,
            7 => Self::ElectricBlind,
            12 => Self::ClimaTerm,
            13 => Self::GenericZone,
            15 => Self::Consumption,
            16 => Self::ClimaThermostatDehumidifier,
            17 => Self::ClimaDehumidifier,
            31 => Self::EnhancedElectricBlind,
            _ => Self::Generic, // Default case
        }
    }
}

impl From<ObjectSubtype> for i32 {
    fn from(value: ObjectSubtype) -> Self {
        match value {
            ObjectSubtype::Generic => 0,
            ObjectSubtype::DigitalLight => 1,
            ObjectSubtype::RgbLight => 2,
            ObjectSubtype::TemporizedLight => 3,
            ObjectSubtype::DimmerLight => 4,
            ObjectSubtype::OtherDigit => 5,
            ObjectSubtype::OtherTmp => 6,
            ObjectSubtype::ElectricBlind => 7,
            ObjectSubtype::ClimaTerm => 12,
            ObjectSubtype::GenericZone => 13,
            ObjectSubtype::Consumption => 15,
            ObjectSubtype::ClimaThermostatDehumidifier => 16,
            ObjectSubtype::ClimaDehumidifier => 17,
            ObjectSubtype::EnhancedElectricBlind => 31,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(into = "i32", from = "String")]
pub(crate) enum DeviceStatus {
    #[default]
    On = 0,
    Off = 1,
    Running = 2,
}

impl From<i32> for DeviceStatus {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::On,
            1 => Self::Off,
            2 => Self::Running,
            _ => Self::Off, // Default case
        }
    }
}

impl From<&str> for DeviceStatus {
    fn from(value: &str) -> Self {
        match value {
            "0" => Self::On,
            "1" => Self::Off,
            "2" => Self::Running,
            _ => Self::Off, // Default case
        }
    }
}

impl From<String> for DeviceStatus {
    fn from(value: String) -> Self {
        DeviceStatus::from(value.as_str())
    }
}

impl From<DeviceStatus> for i32 {
    fn from(value: DeviceStatus) -> Self {
        match value {
            DeviceStatus::On => 0,
            DeviceStatus::Off => 1,
            DeviceStatus::Running => 2,
        }
    }
}

impl From<DeviceStatus> for &str {
    fn from(value: DeviceStatus) -> Self {
        match value {
            DeviceStatus::On => "0",
            DeviceStatus::Off => "1",
            DeviceStatus::Running => "2",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]
enum ThermoSeason {
    SUMMER = 0,
    WINTER = 1,
}

impl From<i32> for ThermoSeason {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::SUMMER,
            1 => Self::WINTER,
            _ => Self::SUMMER, // Default case
        }
    }
}

impl From<ThermoSeason> for i32 {
    fn from(value: ThermoSeason) -> Self {
        match value {
            ThermoSeason::SUMMER => 0,
            ThermoSeason::WINTER => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ClimaMode {
    None = 0,
    Auto = 1,
    Manual = 2,
    SemiAuto = 3,
    SemiMan = 4,
    OffAuto = 5,
    OffManual = 6,
}

impl From<i32> for ClimaMode {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::None,
            1 => Self::Auto,
            2 => Self::Manual,
            3 => Self::SemiAuto,
            4 => Self::SemiMan,
            5 => Self::OffAuto,
            6 => Self::OffManual,
            _ => Self::None, // Default case
        }
    }
}

impl From<ClimaMode> for i32 {
    fn from(value: ClimaMode) -> Self {
        match value {
            ClimaMode::None => 0,
            ClimaMode::Auto => 1,
            ClimaMode::Manual => 2,
            ClimaMode::SemiAuto => 3,
            ClimaMode::SemiMan => 4,
            ClimaMode::OffAuto => 5,
            ClimaMode::OffManual => 6,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DeviceData {
    id: String,
    r#type: ObjectType,
    sub_type: ObjectSubtype,
    sched_status: Option<DeviceStatus>,
    sched_lock: Option<String>,
    #[serde(default, rename = "schedZoneStatus")]
    sched_zone_status: Vec<u32>,
    status: DeviceStatus,
    #[serde(rename = "descrizione")]
    description: String,
    #[serde(rename = "placeOrder")]
    place_order: Option<String>,
    num_modulo: Option<String>,
    num_uscita: Option<String>,
    icon_id: Option<String>,
    #[serde(rename = "isProtected")]
    is_protected: Option<DeviceStatus>,
    #[serde(rename = "objectId")]
    object_id: Option<String>,
    #[serde(rename = "placeId")]
    place_id: Option<String>,
    #[serde(rename = "powerst")]
    power_status: DeviceStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    elements: Vec<OutData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OtherDeviceData {
    #[serde(flatten)]
    data: DeviceData,
    tempo_uscita: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LightDeviceData {
    #[serde(flatten)]
    data: DeviceData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BlindDeviceData {
    #[serde(flatten)]
    data: DeviceData,
    open_status: Option<DeviceStatus>,
    position: Option<String>,
    #[serde(rename = "openTime")]
    open_time: Option<String>,
    #[serde(rename = "closeTime")]
    close_time: Option<String>,
    #[serde(rename = "preferPosition")]
    prefer_position: Option<String>,
    #[serde(rename = "enablePreferPosition")]
    enable_prefer_position: Option<DeviceStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OutletDeviceData {
    #[serde(flatten)]
    data: DeviceData,
    instant_power: String,
    out_power: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct IrrigationDeviceData {
    #[serde(flatten)]
    data: DeviceData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ThermostatDeviceData {
    #[serde(flatten)]
    data: DeviceData,
    num_ingresso: Option<u32>,
    num_moduloIE: Option<String>,
    num_uscitaIE: Option<String>,
    num_moduloI: Option<String>,
    num_uscitaI: Option<String>,
    num_moduloE: Option<String>,
    num_uscitaE: Option<String>,
    num_moduloI_ana: Option<String>,
    num_uscitaI_ana: Option<String>,
    num_moduloE_ana: Option<String>,
    num_uscitaE_ana: Option<String>,
    num_moduloUD: Option<String>,
    num_uscitaUD: Option<String>,
    num_moduloU: Option<String>,
    num_uscitaU: Option<String>,
    num_moduloD: Option<String>,
    num_uscitaD: Option<String>,
    num_moduloU_ana: Option<String>,
    num_uscitaU_ana: Option<String>,
    num_moduloD_ana: Option<String>,
    num_uscitaD_ana: Option<String>,
    night_mode: Option<String>,
    soglia_man_inv: Option<String>,
    soglia_man_est: Option<String>,
    soglia_man_notte_inv: Option<String>,
    soglia_man_notte_est: Option<String>,
    soglia_semiauto: Option<String>,
    soglia_auto_inv: Option<String>,
    soglia_auto_est: Option<String>,
    out_enable_inv: Option<DeviceStatus>,
    out_enable_est: Option<DeviceStatus>,
    dir_enable_inv: Option<DeviceStatus>,
    dir_enable_est: Option<DeviceStatus>,
    heatAutoFanDisable: Option<DeviceStatus>,
    coolAutoFanDisable: Option<DeviceStatus>,
    heatSwingDisable: Option<DeviceStatus>,
    coolSwingDisable: Option<DeviceStatus>,
    out_type_inv: Option<String>,
    out_type_est: Option<String>,
    temp_base_inv: Option<String>,
    temp_base_est: Option<String>,
    out_enable_umi: Option<String>,
    out_enable_deumi: Option<String>,
    dir_enable_umi: Option<String>,
    dir_enable_deumi: Option<String>,
    humAutoFanDisable: Option<String>,
    dehumAutoFanDisable: Option<String>,
    humSwingDisable: Option<String>,
    dehumSwingDisable: Option<String>,
    out_type_umi: Option<String>,
    out_type_deumi: Option<String>,
    soglia_man_umi: Option<String>,
    soglia_man_deumi: Option<String>,
    soglia_man_notte_umi: Option<String>,
    soglia_man_notte_deumi: Option<String>,
    night_mode_umi: Option<String>,
    soglia_semiauto_umi: Option<String>,
    umi_base_umi: Option<String>,
    umi_base_deumi: Option<String>,
    coolLimitMax: Option<String>,
    coolLimitMin: Option<String>,
    heatLimitMax: Option<String>,
    heatLimitMin: Option<String>,
    viewOnly: Option<String>,
    temperatura: Option<String>,
    auto_man: Option<ClimaMode>,
    est_inv: Option<ThermoSeason>,
    soglia_attiva: Option<String>,
    out_value_inv: Option<String>,
    out_value_est: Option<String>,
    dir_out_inv: Option<String>,
    dir_out_est: Option<String>,
    semiauto_enabled: Option<String>,
    umidita: Option<String>,
    auto_man_umi: Option<ClimaMode>,
    deumi_umi: Option<String>,
    soglia_attiva_umi: Option<String>,
    semiauto_umi_enabled: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SupplierDeviceData {
    #[serde(flatten)]
    data: DeviceData,
    label_value: String,
    label_price: String,
    prod: String,
    count_div: String,
    cost: String,
    kCO2: String,
    compare: String,
    groupOrder: String,
    instant_power: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentDeviceData {
    pub(crate) agent_id: u32,
    #[serde(rename = "descrizione")]
    pub(crate) description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum OutData {
    Agent(AgentDeviceData),
    Data(DeviceData),
    Other(OtherDeviceData),
    Light(LightDeviceData),
    Blind(BlindDeviceData),
    Outlet(OutletDeviceData),
    Irrigation(IrrigationDeviceData),
    Thermostat(ThermostatDeviceData),
    Supplier(SupplierDeviceData),
}
