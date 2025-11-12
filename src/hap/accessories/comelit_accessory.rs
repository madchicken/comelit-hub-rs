use crate::protocol::out_data_messages::{HomeDeviceData};
use anyhow::Result;

pub trait ComelitAccessory {
    fn id(&self) -> &str;

    fn update(&self, data: &HomeDeviceData) -> impl Future<Output = Result<()>>;
}