use std::error::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::protocol::out_data_messages::{OutData};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]
#[repr(i32)]
pub enum RequestType {
    #[default]
    Status = 0,
    Action = 1,
    Subscribe = 3,
    Login = 5,
    Ping = 7,
    ReadParams = 8,
    GetDatetime = 9,
    Announce = 13,
}

impl From<i32> for RequestType {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::Status,
            1 => Self::Action,
            3 => Self::Subscribe,
            5 => Self::Login,
            7 => Self::Ping,
            8 => Self::ReadParams,
            9 => Self::GetDatetime,
            13 => Self::Announce,
            _ => Self::Status, // Default case
        }
    }
}

// Implementazione per convertire da RequestSubType a i32
impl From<RequestType> for i32 {
    fn from(value: RequestType) -> Self {
        value as i32
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]
pub enum RequestSubType {
    CreateObj = 0,
    UpdateObj = 1,
    DeleteObj = 2,
    SetActionObj = 3,
    GetTempoObj = 4,
    SubscribeRt = 5,
    UnsubscribeRt = 6,
    GetConfParamGroup = 23,
    #[default]
    None = -1,
}

// Implementazione per convertire da i32 a RequestSubType
impl From<i32> for RequestSubType {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::CreateObj,
            1 => Self::UpdateObj,
            2 => Self::DeleteObj,
            3 => Self::SetActionObj,
            4 => Self::GetTempoObj,
            5 => Self::SubscribeRt,
            6 => Self::UnsubscribeRt,
            23 => Self::GetConfParamGroup,
            _ => Self::None,
        }
    }
}

// Implementazione per convertire da RequestSubType a i32
impl From<RequestSubType> for i32 {
    fn from(value: RequestSubType) -> Self {
        value as i32
    }
}

#[derive(Default, Clone, Debug, Serialize)]
pub(crate) struct MqttMessage {
    pub req_type: RequestType,
    pub seq_id: u32,
    pub req_sub_type: RequestSubType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(rename = "sessiontoken")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param_type: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obj_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obj_type: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_level: Option<u8>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub act_params: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub act_type: Option<u32>,
}

#[derive(Default, Clone, Debug, Deserialize)]
pub(crate) struct MqttResponseMessage {
    pub req_type: RequestType,
    pub seq_id: u32,
    pub req_result: u32,
    pub req_sub_type: RequestSubType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<u32>,
    pub agent_type: Option<u32>,
    #[serde(rename = "sessiontoken")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param_type: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obj_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub out_data: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params_data: Vec<Param>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Param {
    param_name: String,
    param_value: String,
}

pub (crate) struct LoginInfo {
    pub user: String,
    pub password: String,
    pub agent_id: u32,
}

pub enum MqttCommand {
    Status(String, String, u8),
    Action,
    Subscribe,
    Login(LoginInfo),
    Ping,
    ReadParams,
    GetDatetime,
    Announce(u32),
}

impl From<&MqttMessage> for Vec<u8> {
    fn from(value: &MqttMessage) -> Self {
        serde_json::to_string(value).unwrap().into_bytes()
    }
}

impl From<MqttMessage> for Vec<u8> {
    fn from(value: MqttMessage) -> Self {
        Vec::<u8>::from(&value)
    }
}

pub fn make_message(seq_id: u32, cmd: MqttCommand) -> MqttMessage {
    match cmd {
        MqttCommand::Status(session, device, depth) => make_status_message(seq_id, session.as_str(), device.as_str(), depth),
        MqttCommand::Action => make_action_message(seq_id),
        MqttCommand::Subscribe => make_subscribe_message(seq_id),
        MqttCommand::Login(info) => make_login_message(seq_id, info.user.as_str(), info.password.as_str(), info.agent_id),
        MqttCommand::Ping => make_ping_message(seq_id),
        MqttCommand::ReadParams => make_read_params_message(seq_id),
        MqttCommand::GetDatetime => make_get_datetime_message(seq_id),
        MqttCommand::Announce(agent_type) => make_announce_message(seq_id, agent_type),
    }
}

fn make_get_datetime_message(p0: u32) -> MqttMessage {
    todo!()
}

fn make_read_params_message(p0: u32) -> MqttMessage {
    todo!()
}

pub fn make_login_message(req_id: u32, user: &str, password: &str, agent_id: u32) -> MqttMessage {
    MqttMessage {
        req_type: RequestType::Login,
        seq_id: req_id,
        req_sub_type: RequestSubType::None,
        user_name: Some(user.to_string()),
        password: Some(password.to_string()),
        agent_id: Some(agent_id),
        ..MqttMessage::default()
    }
}

fn make_ping_message(p0: u32) -> MqttMessage {
    todo!()
}

fn make_subscribe_message(p0: u32) -> MqttMessage {
    todo!()
}

fn make_action_message(p0: u32) -> MqttMessage {
    todo!()
}

pub fn make_status_message(seq_id: u32, session_token: &str, device: &str, level: u8) -> MqttMessage {
    MqttMessage {
        req_type: RequestType::Status,
        seq_id,
        req_sub_type: RequestSubType::None,
        session_token: Some(session_token.to_string()),
        obj_id: Some(device.to_string()),
        detail_level: Some(level),
        ..MqttMessage::default()
    }
}

pub fn make_announce_message(seq_id: u32, agent_type: u32) -> MqttMessage {
    MqttMessage {
        req_type: RequestType::Announce,
        seq_id,
        req_sub_type: RequestSubType::None,
        agent_type: Some(agent_type),
        ..MqttMessage::default()
    }
}