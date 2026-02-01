use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct BaseResponse {
    pub message: String,
    pub message_type: String,
    pub message_id: u8,
    pub response_code: u8,
    pub response_string: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct AuthResponse {
    #[serde(flatten)]
    pub response: BaseResponse,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct InfoResponse {
    pub model: String,
    pub version: String,
    pub serial_code: String,
    pub capabilities: Vec<String>,

    #[serde(flatten)]
    pub channel_details: HashMap<String, Value>,

    #[serde(flatten)]
    pub response: BaseResponse,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct ActivateUserResponse {
    pub user_token: String,

    #[serde(flatten)]
    pub response: BaseResponse,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct ViperServerResponse {
    pub local_address: String,
    pub local_tcp_port: u16,
    pub local_udp_port: u16,
    pub remote_address: String,
    pub remote_tcp_port: u16,
    pub remote_udp_port: u16,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct ViperClientResponse {
    pub description: String,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct AptConfigResponse {
    pub description: String,
    pub call_divert_busy_en: bool,
    pub call_divert_address: String,
    pub virtual_key_enabled: bool,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Switchboard {
    pub id: String,
    pub name: String,
    pub apt_address: String,
    pub emergency_calls: bool,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Entrance {
    pub id: String,
    pub name: String,
    pub apt_address: String,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Actuator {
    pub id: String,
    pub name: String,
    pub apt_address: String,
    pub module_index: u8,
    pub output_index: u8,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Opendoor {
    pub id: u8,
    pub name: String,
    pub apt_address: String,
    pub output_index: u8,
    pub secure_mode: bool,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct OpendoorAction {
    pub id: u8,
    pub action: String,
    pub apt_address: String,
    pub output_index: u8,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct UserParametersResponse {
    pub forced: bool,
    #[serde(default)]
    pub apt_address_book: Vec<HashMap<String, Value>>,
    #[serde(default)]
    pub camera_address_book: Vec<HashMap<String, Value>>,
    #[serde(default)]
    pub rtsp_camera_address_book: Vec<HashMap<String, Value>>,
    #[serde(default)]
    pub switchboard_address_book: Vec<Switchboard>,
    #[serde(default)]
    pub entrance_address_book: Vec<Entrance>,
    #[serde(default)]
    pub actuator_address_book: Vec<Actuator>,
    #[serde(default)]
    pub opendoor_address_book: Vec<Opendoor>,
    #[serde(default)]
    pub opendoor_actions: Vec<OpendoorAction>,
    #[serde(default)]
    pub additional_actuator: Vec<Actuator>,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct VipConfig {
    pub enabled: bool,
    pub apt_address: String,
    pub apt_subaddress: u16,
    pub logical_subaddress: u16,
    pub apt_config: AptConfigResponse,
    pub user_parameters: UserParametersResponse,
}

#[allow(dead_code)]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct ConfigurationResponse {
    pub viper_server: ViperServerResponse,
    pub viper_client: ViperClientResponse,
    pub vip: VipConfig,

    #[serde(flatten)]
    pub response: BaseResponse,
}
