use std::sync::Arc;

use anyhow::{Context, Result};
use futures::FutureExt;
use hap::{
    accessory::{AccessoryInformation, lightbulb::LightbulbAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    server::{IpServer, Server},
};
use serde_json::Value;
use tracing::{error, info};

use crate::{
    hap::accessories::AccessoryPointer,
    protocol::{
        client::ComelitClient,
        out_data_messages::{ActionType, DeviceStatus, LightDeviceData},
    },
};

pub(crate) struct ComelitLightbulbAccessory {
    lightbulb_accessory: AccessoryPointer,
    data: LightDeviceData,
}

impl ComelitLightbulbAccessory {
    pub(crate) async fn new(
        id: u64,
        light_data: LightDeviceData,
        client: Arc<ComelitClient>,
        server: &IpServer,
    ) -> Result<Self> {
        let device_id = light_data.data.id.clone();
        let name = light_data
            .data
            .description
            .clone()
            .unwrap_or(device_id.clone());
        let lightbulb_name = name.clone();
        let mut lightbulb_accessory = LightbulbAccessory::new(
            id,
            AccessoryInformation {
                manufacturer: "Comelit".into(),
                name,
                serial_number: light_data.data.id.clone(),
                ..Default::default()
            },
        )
        .context("Cannot create lightbulb accessory")?;

        let c = client.clone();
        lightbulb_accessory
            .lightbulb
            .power_state
            .on_update_async(Some(move |current_val: bool, new_val: bool| {
                info!(
                    "Lightbulb {}: power_state characteristic updated from {} to {}",
                    lightbulb_name, current_val, new_val
                );
                let c = c.clone();
                let id = device_id.clone();
                async move {
                    if c.send_action(id.as_str(), ActionType::Set, if new_val { 1 } else { 0 })
                        .await
                        .is_err()
                    {
                        error!("Failed to update power state for lightbulb {}", id);
                    }
                    Ok(())
                }
                .boxed()
            }));
        Ok(Self {
            data: light_data,
            lightbulb_accessory: server.add_accessory(lightbulb_accessory).await?,
        })
    }

    pub fn id(&self) -> &str {
        &self.data.data.id
    }

    pub async fn update(&mut self, light_data: &LightDeviceData) {
        self.data = light_data.clone();
        let id = &self.data.data.id;
        if let Some(state) = light_data.data.status.as_ref() {
            let mut accessory = self.lightbulb_accessory.lock().await;
            let service = accessory.get_mut_service(hap::HapType::Lightbulb).unwrap();
            let power_state = service
                .get_mut_characteristic(hap::HapType::PowerState)
                .unwrap();
            if power_state
                .set_value(Value::Bool(*state == DeviceStatus::On))
                .await
                .is_err()
            {
                error!("Failed to update power state for device {id}");
            } else {
                info!(
                    "Updated power state for device {id}: {:?}",
                    if *state == DeviceStatus::On {
                        "On"
                    } else {
                        "Off"
                    }
                );
            }
        }
    }
}
