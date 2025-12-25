use anyhow::Result;
use comelit_hub_rs::{DeviceStatus, DoorbellDeviceData};
use futures_util::lock::Mutex;
use hap::{
    HapType,
    accessory::{AccessoryInformation, HapAccessory, doorbell::DoorbellAccessory},
    characteristic::HapCharacteristic,
    server::{IpServer, Server},
};
use serde_json::Value;
use std::sync::Arc;

use crate::accessories::ComelitAccessory;

pub(crate) struct ComelitDoorbellAccessory {
    pub(crate) id: String,
    pub(crate) accessory_pointer: Arc<Mutex<Box<dyn HapAccessory + 'static>>>,
}

impl ComelitDoorbellAccessory {
    pub(crate) async fn new(
        id: u64,
        door_data: &DoorbellDeviceData,
        server: &IpServer,
    ) -> Result<Self> {
        let device_id = door_data.id.clone();
        let name = door_data.description.clone().unwrap_or(device_id.clone());
        let mut doorbell_accessory = DoorbellAccessory::new(
            id,
            AccessoryInformation {
                name: name.clone(),
                manufacturer: "Comelit".to_string(),
                ..Default::default()
            },
        )?;

        doorbell_accessory.doorbell.brightness = None;
        doorbell_accessory.doorbell.mute = None;
        doorbell_accessory.doorbell.operating_state_response = None;
        doorbell_accessory.doorbell.volume = None;

        doorbell_accessory
            .doorbell
            .programmable_switch_event
            .set_event_notifications(Some(true));
        let accessory_pointer = server.add_accessory(doorbell_accessory).await?;
        Ok(Self {
            id: device_id,
            accessory_pointer,
        })
    }
}

impl ComelitAccessory<DoorbellDeviceData> for ComelitDoorbellAccessory {
    fn get_comelit_id(&self) -> &str {
        &self.id
    }

    async fn update(&mut self, data: &DoorbellDeviceData) -> Result<()> {
        if data.status.clone().unwrap_or_default() == DeviceStatus::On {
            let mut accessory = self.accessory_pointer.lock().await;
            let service = accessory.get_mut_service(HapType::Doorbell).unwrap();
            let characteristic = service
                .get_mut_characteristic(HapType::StatefulProgrammableSwitch)
                .unwrap();
            characteristic.set_value(Value::from(0)).await?;
        }
        Ok(())
    }
}
