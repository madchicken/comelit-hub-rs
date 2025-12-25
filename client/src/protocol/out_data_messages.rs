use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]
pub enum ObjectType {
    Other = 1,
    WindowCovering = 2,
    Light = 3,
    Irrigation = 4,
    Thermostat = 9,
    Outlet = 10,
    PowerSupplier = 11,
    Agent = 13,
    Zone = 1001,
    VipElement = 2000,
    Door = 2001,
    Unknown = -1,
}

impl From<i32> for ObjectType {
    fn from(value: i32) -> Self {
        match value {
            1 => Self::Other,
            2 => Self::WindowCovering,
            3 => Self::Light,
            4 => Self::Irrigation,
            9 => Self::Thermostat,
            10 => Self::Outlet,
            11 => Self::PowerSupplier,
            13 => Self::Agent,
            1001 => Self::Zone,
            2000 => Self::VipElement,
            2001 => Self::Door,
            _ => Self::Unknown, // Default case
        }
    }
}

impl From<ObjectType> for i32 {
    fn from(value: ObjectType) -> Self {
        match value {
            ObjectType::Other => 1,
            ObjectType::WindowCovering => 2,
            ObjectType::Light => 3,
            ObjectType::Irrigation => 4,
            ObjectType::Thermostat => 9,
            ObjectType::Outlet => 10,
            ObjectType::PowerSupplier => 11,
            ObjectType::Agent => 13,
            ObjectType::Zone => 1001,
            ObjectType::VipElement => 2000,
            ObjectType::Door => 2001,
            ObjectType::Unknown => -1, // Default case
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]
pub enum ObjectSubtype {
    Unknown = -1,
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
    Door = 23,
    EnhancedElectricBlind = 31,
}

impl From<i32> for ObjectSubtype {
    fn from(value: i32) -> Self {
        match value {
            -1 => Self::Unknown,
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
            23 => Self::Door,
            31 => Self::EnhancedElectricBlind,
            _ => Self::Generic, // Default case
        }
    }
}

impl From<ObjectSubtype> for i32 {
    fn from(value: ObjectSubtype) -> Self {
        match value {
            ObjectSubtype::Unknown => -1,
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
            ObjectSubtype::Door => 23,
            ObjectSubtype::EnhancedElectricBlind => 31,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(into = "u8", from = "String")]
#[repr(u8)]
pub enum WindowCoveringStatus {
    #[default]
    Stopped = 0,
    GoingUp = 1,
    GoingDown = 2,
}

impl From<WindowCoveringStatus> for u8 {
    fn from(value: WindowCoveringStatus) -> Self {
        match value {
            WindowCoveringStatus::Stopped => 0,
            WindowCoveringStatus::GoingUp => 1,
            WindowCoveringStatus::GoingDown => 2,
        }
    }
}

impl From<&str> for WindowCoveringStatus {
    fn from(value: &str) -> Self {
        match value {
            "0" => Self::Stopped,
            "1" => Self::GoingUp,
            "2" => Self::GoingDown,
            _ => Self::Stopped, // Default case
        }
    }
}

impl From<String> for WindowCoveringStatus {
    fn from(value: String) -> Self {
        WindowCoveringStatus::from(value.as_str())
    }
}

impl From<WindowCoveringStatus> for &str {
    fn from(value: WindowCoveringStatus) -> Self {
        match value {
            WindowCoveringStatus::Stopped => "0",
            WindowCoveringStatus::GoingUp => "1",
            WindowCoveringStatus::GoingDown => "2",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(into = "u8", from = "String")]
#[repr(u8)]
pub enum DeviceStatus {
    #[default]
    Off = 0,
    On = 1,
    Running = 2,
}

impl From<&str> for DeviceStatus {
    fn from(value: &str) -> Self {
        match value {
            "0" => Self::Off,
            "1" => Self::On,
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

impl From<DeviceStatus> for u8 {
    fn from(value: DeviceStatus) -> Self {
        match value {
            DeviceStatus::Off => 0,
            DeviceStatus::On => 1,
            DeviceStatus::Running => 2,
        }
    }
}

impl From<DeviceStatus> for &str {
    fn from(value: DeviceStatus) -> Self {
        match value {
            DeviceStatus::Off => "0",
            DeviceStatus::On => "1",
            DeviceStatus::Running => "2",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(into = "u8", from = "String")]
#[repr(u8)]
pub enum PowerStatus {
    #[default]
    Stopped = 0,
    Off = 1,
    On = 2,
}

impl From<&str> for PowerStatus {
    fn from(value: &str) -> Self {
        match value {
            "0" => Self::Stopped,
            "1" => Self::Off,
            "2" => Self::On,
            _ => Self::Stopped, // Default case
        }
    }
}

impl From<String> for PowerStatus {
    fn from(value: String) -> Self {
        PowerStatus::from(value.as_str())
    }
}

impl From<PowerStatus> for u8 {
    fn from(value: PowerStatus) -> Self {
        match value {
            PowerStatus::Stopped => 0,
            PowerStatus::Off => 1,
            PowerStatus::On => 2,
        }
    }
}

impl From<PowerStatus> for &str {
    fn from(value: PowerStatus) -> Self {
        match value {
            PowerStatus::Stopped => "0",
            PowerStatus::Off => "1",
            PowerStatus::On => "2",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(into = "i32", from = "String")]
#[repr(u8)]
pub enum OpenStatus {
    Closed = 0,
    #[default]
    Open = 1,
}

impl From<u8> for OpenStatus {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Closed,
            1 => Self::Open,
            _ => Self::Open, // Default case
        }
    }
}

impl From<&str> for OpenStatus {
    fn from(value: &str) -> Self {
        match value {
            "0" => Self::Closed,
            "1" => Self::Open,
            _ => Self::Open, // Default case
        }
    }
}

impl From<String> for OpenStatus {
    fn from(value: String) -> Self {
        OpenStatus::from(value.as_str())
    }
}

impl From<OpenStatus> for i32 {
    fn from(value: OpenStatus) -> Self {
        match value {
            OpenStatus::Closed => 0,
            OpenStatus::Open => 1,
        }
    }
}

impl From<OpenStatus> for &str {
    fn from(value: OpenStatus) -> Self {
        match value {
            OpenStatus::Closed => "0",
            OpenStatus::Open => "1",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(into = "i32", from = "String")]
pub enum ThermoSeason {
    Summer = 0,
    #[default]
    Winter = 1,
}

impl From<i32> for ThermoSeason {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::Summer,
            1 => Self::Winter,
            _ => Self::Summer, // Default case
        }
    }
}

impl From<ThermoSeason> for i32 {
    fn from(value: ThermoSeason) -> Self {
        match value {
            ThermoSeason::Summer => 0,
            ThermoSeason::Winter => 1,
        }
    }
}

impl From<ThermoSeason> for &str {
    fn from(value: ThermoSeason) -> Self {
        match value {
            ThermoSeason::Summer => "0",
            ThermoSeason::Winter => "1",
        }
    }
}

impl From<String> for ThermoSeason {
    fn from(value: String) -> Self {
        match value.as_str() {
            "0" => Self::Summer,
            "1" => Self::Winter,
            _ => Self::Summer, // Default case
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(into = "i32", from = "String")]
pub enum ClimaMode {
    #[default]
    None = 0,
    Auto = 1,
    Manual = 2,
    SemiAuto = 3,
    SemiMan = 4,
    OffAuto = 5,
    OffManual = 6,
}

impl From<String> for ClimaMode {
    fn from(value: String) -> Self {
        match value.as_str() {
            "0" => Self::None,
            "1" => Self::Auto,
            "2" => Self::Manual,
            "3" => Self::SemiAuto,
            "4" => Self::SemiMan,
            "5" => Self::OffAuto,
            "6" => Self::OffManual,
            _ => Self::None, // Default case
        }
    }
}

impl From<ClimaMode> for &str {
    fn from(value: ClimaMode) -> Self {
        match value {
            ClimaMode::None => "0",
            ClimaMode::Auto => "1",
            ClimaMode::Manual => "2",
            ClimaMode::SemiAuto => "3",
            ClimaMode::SemiMan => "4",
            ClimaMode::OffAuto => "5",
            ClimaMode::OffManual => "6",
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(into = "i32", from = "String")]
pub enum ClimaOnOff {
    OffThermo = 0,
    OnThermo = 1,
    OffHumi = 2,
    OnHumi = 3,
    #[default]
    Off = 4,
    On = 5,
}

impl From<String> for ClimaOnOff {
    fn from(value: String) -> Self {
        match value.as_str() {
            "0" => ClimaOnOff::OffThermo,
            "1" => ClimaOnOff::OnThermo,
            "2" => ClimaOnOff::OffHumi,
            "3" => ClimaOnOff::OnHumi,
            "4" => ClimaOnOff::Off,
            "5" => ClimaOnOff::On,
            _ => ClimaOnOff::Off, // Default case
        }
    }
}

impl From<ClimaOnOff> for &str {
    fn from(value: ClimaOnOff) -> Self {
        match value {
            ClimaOnOff::OffThermo => "0",
            ClimaOnOff::OnThermo => "1",
            ClimaOnOff::OffHumi => "2",
            ClimaOnOff::OnHumi => "3",
            ClimaOnOff::Off => "4",
            ClimaOnOff::On => "5",
        }
    }
}

impl From<i32> for ClimaOnOff {
    fn from(value: i32) -> Self {
        match value {
            0 => ClimaOnOff::OffThermo,
            1 => ClimaOnOff::OnThermo,
            2 => ClimaOnOff::OffHumi,
            3 => ClimaOnOff::OnHumi,
            4 => ClimaOnOff::Off,
            5 => ClimaOnOff::On,
            _ => ClimaOnOff::Off, // Default case
        }
    }
}

impl From<ClimaOnOff> for i32 {
    fn from(value: ClimaOnOff) -> Self {
        match value {
            ClimaOnOff::OffThermo => 0,
            ClimaOnOff::OnThermo => 1,
            ClimaOnOff::OffHumi => 2,
            ClimaOnOff::OnHumi => 3,
            ClimaOnOff::Off => 4,
            ClimaOnOff::On => 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]
pub enum ActionType {
    Set = 0,
    ClimaMode = 1,
    ClimaSetPoint = 2,
    SwitchSeason = 4,
    SwitchClimaMode = 13,
    UmiSetpoint = 19,
    SwitchUmiMode = 23,
    SetBlindPosition = 52,
}

impl From<i32> for ActionType {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::Set,
            1 => Self::ClimaMode,
            2 => Self::ClimaSetPoint,
            4 => Self::SwitchSeason,
            13 => Self::SwitchClimaMode,
            19 => Self::UmiSetpoint,
            23 => Self::SwitchUmiMode,
            52 => Self::SetBlindPosition,
            _ => Self::Set, // Default case
        }
    }
}

impl From<ActionType> for i32 {
    fn from(value: ActionType) -> Self {
        match value {
            ActionType::Set => 0,
            ActionType::ClimaMode => 1,
            ActionType::ClimaSetPoint => 2,
            ActionType::SwitchSeason => 4,
            ActionType::SwitchClimaMode => 13,
            ActionType::UmiSetpoint => 19,
            ActionType::SwitchUmiMode => 23,
            ActionType::SetBlindPosition => 52,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InnerDeviceData {
    pub id: String,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceData {
    pub id: String,
    pub r#type: ObjectType,
    pub sub_type: ObjectSubtype,
    pub status: Option<DeviceStatus>,
    #[serde(rename = "descrizione")]
    pub description: Option<String>,
    #[serde(rename = "powerst")]
    pub power_status: Option<PowerStatus>,
    #[serde(default)]
    elements: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtherDeviceData {
    #[serde(flatten)]
    data: DeviceData,
    tempo_uscita: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightDeviceData {
    pub id: String,
    pub r#type: ObjectType,
    pub sub_type: ObjectSubtype,
    pub status: Option<DeviceStatus>,
    #[serde(rename = "descrizione")]
    pub description: Option<String>,
    #[serde(rename = "powerst")]
    pub power_status: Option<PowerStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowCoveringDeviceData {
    pub id: String,
    pub r#type: ObjectType,
    pub sub_type: ObjectSubtype,
    pub status: Option<DeviceStatus>,
    #[serde(rename = "descrizione")]
    pub description: Option<String>,
    #[serde(rename = "powerst")]
    pub power_status: Option<WindowCoveringStatus>,
    // pub open_status: Option<OpenStatus>,
    // pub position: Option<String>,
    // #[serde(rename = "openTime")]
    // pub open_time: Option<String>,
    // #[serde(rename = "closeTime")]
    // pub close_time: Option<String>,
    // #[serde(rename = "preferPosition")]
    // pub prefer_position: Option<String>,
    // #[serde(rename = "enablePreferPosition")]
    // pub enable_prefer_position: Option<DeviceStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutletDeviceData {
    #[serde(flatten)]
    data: DeviceData,
    instant_power: String,
    out_power: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrrigationDeviceData {
    #[serde(flatten)]
    data: DeviceData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct ThermostatDeviceData {
    pub id: String,
    pub r#type: ObjectType,
    pub sub_type: ObjectSubtype,
    pub status: Option<DeviceStatus>,
    #[serde(rename = "descrizione")]
    pub description: Option<String>,
    #[serde(rename = "temperatura")]
    pub temperature: Option<String>,
    pub auto_man: Option<ClimaMode>,
    #[serde(rename = "est_inv")]
    pub season: Option<ThermoSeason>,
    #[serde(rename = "soglia_attiva")]
    pub active_threshold: Option<String>,
    #[serde(rename = "umidita")]
    pub humidity: Option<String>,
    #[serde(rename = "soglia_attiva_umi")]
    pub humi_active_threshold: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupplierDeviceData {
    #[serde(flatten)]
    data: DeviceData,
    label_value: Option<String>,
    label_price: Option<String>,
    prod: Option<String>,
    count_div: Option<String>,
    cost: Option<String>,
    #[serde(rename = "kCO2")]
    k_co2: Option<String>,
    compare: Option<String>,
    #[serde(rename = "groupOrder")]
    group_order: Option<String>,
    instant_power: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDeviceData {
    pub agent_id: u32,
    #[serde(rename = "descrizione")]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoorDeviceData {
    pub id: String,
    pub r#type: ObjectType,
    pub sub_type: ObjectSubtype,
    pub status: Option<DeviceStatus>,
    #[serde(rename = "descrizione")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoorbellDeviceData {
    pub id: String,
    pub r#type: ObjectType,
    pub sub_type: ObjectSubtype,
    pub status: Option<DeviceStatus>,
    #[serde(rename = "descrizione")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum HomeDeviceData {
    Agent(AgentDeviceData),
    Data(DeviceData),
    Other(OtherDeviceData),
    Light(LightDeviceData),
    WindowCovering(WindowCoveringDeviceData),
    Outlet(OutletDeviceData),
    Irrigation(IrrigationDeviceData),
    Thermostat(ThermostatDeviceData),
    Supplier(SupplierDeviceData),
    Doorbell(DoorbellDeviceData),
    Door(DoorDeviceData),
}

impl HomeDeviceData {
    pub fn id(&self) -> String {
        match self {
            HomeDeviceData::Agent(o) => o.agent_id.to_string(),
            HomeDeviceData::Data(o) => o.id.clone(),
            HomeDeviceData::Other(o) => o.data.id.clone(),
            HomeDeviceData::Light(o) => o.id.clone(),
            HomeDeviceData::WindowCovering(o) => o.id.clone(),
            HomeDeviceData::Outlet(o) => o.data.id.clone(),
            HomeDeviceData::Irrigation(o) => o.data.id.clone(),
            HomeDeviceData::Thermostat(o) => o.id.clone(),
            HomeDeviceData::Supplier(o) => o.data.id.clone(),
            HomeDeviceData::Doorbell(o) => o.id.clone(),
            HomeDeviceData::Door(o) => o.id.clone(),
        }
    }

    pub fn name(&self) -> String {
        match self {
            HomeDeviceData::Agent(o) => o.description.clone(),
            HomeDeviceData::Data(o) => o.description.clone().unwrap_or(o.id.clone()),
            HomeDeviceData::Other(o) => o.data.description.clone().unwrap_or(o.data.id.clone()),
            HomeDeviceData::Light(o) => o.description.clone().unwrap_or(o.id.clone()),
            HomeDeviceData::WindowCovering(o) => o.description.clone().unwrap_or(o.id.clone()),
            HomeDeviceData::Outlet(o) => o.data.description.clone().unwrap_or(o.data.id.clone()),
            HomeDeviceData::Irrigation(o) => {
                o.data.description.clone().unwrap_or(o.data.id.clone())
            }
            HomeDeviceData::Thermostat(o) => o.description.clone().unwrap_or(o.id.clone()),
            HomeDeviceData::Supplier(o) => o.data.description.clone().unwrap_or(o.data.id.clone()),
            HomeDeviceData::Doorbell(o) => o.description.clone().unwrap_or(o.id.clone()),
            HomeDeviceData::Door(o) => o.description.clone().unwrap_or(o.id.clone()),
        }
    }
}

pub fn device_data_to_home_device(value: Value, level: u8) -> Vec<HomeDeviceData> {
    let data = serde_json::from_value::<DeviceData>(value.clone()).unwrap();
    match data.r#type {
        ObjectType::Other => {
            let other_data = serde_json::from_value::<OtherDeviceData>(value.clone()).unwrap();
            vec![HomeDeviceData::Other(other_data)]
        }
        ObjectType::WindowCovering => {
            let blind_data =
                serde_json::from_value::<WindowCoveringDeviceData>(value.clone()).unwrap();
            vec![HomeDeviceData::WindowCovering(blind_data)]
        }
        ObjectType::Light => {
            let light_data = serde_json::from_value::<LightDeviceData>(value.clone()).unwrap();
            vec![HomeDeviceData::Light(light_data)]
        }
        ObjectType::Irrigation => {
            let irrigation_data =
                serde_json::from_value::<IrrigationDeviceData>(value.clone()).unwrap();
            vec![HomeDeviceData::Irrigation(irrigation_data)]
        }
        ObjectType::Thermostat => {
            let thermostat_data =
                serde_json::from_value::<ThermostatDeviceData>(value.clone()).unwrap();
            vec![HomeDeviceData::Thermostat(thermostat_data)]
        }
        ObjectType::Outlet => {
            let outlet_data = serde_json::from_value::<OutletDeviceData>(value.clone()).unwrap();
            vec![HomeDeviceData::Outlet(outlet_data)]
        }
        ObjectType::PowerSupplier => {
            let supplier_data =
                serde_json::from_value::<SupplierDeviceData>(value.clone()).unwrap();
            vec![HomeDeviceData::Supplier(supplier_data)]
        }
        ObjectType::Agent => {
            let agent_data = serde_json::from_value::<AgentDeviceData>(value.clone()).unwrap();
            vec![HomeDeviceData::Agent(agent_data)]
        }
        ObjectType::Zone => data
            .elements
            .iter()
            .flat_map(|v| {
                debug!(
                    "Zone {} found, reading element inside",
                    data.description.as_ref().unwrap_or(&"None".to_string()),
                );
                if level == 1 {
                    let inner = serde_json::from_value::<InnerDeviceData>(v.clone()).unwrap();
                    device_data_to_home_device(inner.data, level)
                } else {
                    device_data_to_home_device(v.clone(), level)
                }
            })
            .collect::<Vec<HomeDeviceData>>(),
        ObjectType::VipElement => {
            let other_data = serde_json::from_value::<DoorbellDeviceData>(value.clone()).unwrap();
            vec![HomeDeviceData::Doorbell(other_data)]
        }
        ObjectType::Door => {
            let door_data = serde_json::from_value::<DoorDeviceData>(value.clone()).unwrap();
            vec![HomeDeviceData::Door(door_data)]
        }
        ObjectType::Unknown => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::messages::MqttResponseMessage;

    #[test]
    fn parse_device_data() {
        let json = r#"{
            "req_type":0,
            "req_sub_type":-1,
            "seq_id":2,
            "req_result":0,
            "out_data":[{
                "id":"GEN#17#13#1",
                "type":1001,
                "sub_type":13,
                "descrizione":"root",
                "schedZoneStatus":[0,0,0],
                "elements":[{
                    "id":"VIP#APARTMENT",
                    "type":2000,
                    "sub_type":0,
                    "descrizione":"Generic vip element"
                },{
                    "id":"VIP#OD#00000100.2",
                    "type":2001,
                    "sub_type":23,
                    "descrizione":"CANCELLO"
                }]
            }],
            "count":1
        }"#;
        let result = serde_json::from_str::<MqttResponseMessage>(json);
        assert!(result.is_ok());
        let mqtt_response = result.unwrap();
        mqtt_response.out_data.iter().for_each(|out| {
            let res = serde_json::from_value::<DeviceData>(out.clone());
            assert!(res.is_ok());
            let device_data = res.unwrap();
            assert_eq!(device_data.id, "GEN#17#13#1");
        })
    }
}
