use std::sync::{Arc};

use anyhow::{Context, Result};
use futures::FutureExt;
use hap::{
    accessory::{AccessoryInformation, lightbulb::LightbulbAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    server::{IpServer, Server},
};
use hap::characteristic::HapCharacteristic;
use serde_json::Value;
use tracing::{debug, error, info};

use crate::{
    hap::accessories::AccessoryPointer,
    protocol::{
        client::ComelitClient,
        out_data_messages::{ActionType, DeviceStatus, LightDeviceData},
    },
};
use crate::hap::accessories::comelit_accessory::ComelitAccessory;
use crate::protocol::out_data_messages::HomeDeviceData;
use crate::protocol::out_data_messages::HomeDeviceData::Light;

pub(crate) struct ComelitLightbulbAccessory {
    lightbulb_accessory: AccessoryPointer,
    id: String,
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
        let mut lightbulb_accessory = LightbulbAccessory::new(
            id,
            AccessoryInformation {
                name,
                ..Default::default()
            },
        )
            .context("Cannot create lightbulb accessory")?;

        info!("Created lightbulb accessory: {:?}", light_data);
        lightbulb_accessory
            .lightbulb
            .power_state.set_value(Value::Bool(light_data.data.status.unwrap_or_default() == DeviceStatus::On))
            .await
            .context("Cannot set initial power state for lightbulb")?;

        Self::setup_update(device_id.as_str(), client.clone(), &mut lightbulb_accessory).await;
        Self::setup_read(device_id.as_str(), client.clone(), &mut lightbulb_accessory).await;

        Ok(Self {
            lightbulb_accessory: server.add_accessory(lightbulb_accessory).await?,
            id: device_id,
        })
    }

    pub async fn setup_read(id: &str, client: Arc<ComelitClient>, lightbulb_accessory: &mut LightbulbAccessory) {
        let id = id.to_string();
        lightbulb_accessory
            .lightbulb
            .power_state.on_read_async(Some(move || {
            info!("Lightbulb read {} from lightbulb", id);
            let client = client.clone();
            let id = id.clone();
            async move {
                if let Ok(statuses) = client.info(id.as_str(), 1).await {
                    if let Some(first) = statuses.first() {
                        debug!("Read internal status for lightbulb {}: {:?}", id, first);
                        let status = first.status.as_ref().unwrap();
                        Ok(Some(*status == DeviceStatus::On))
                    } else {
                        error!("No status returned for lightbulb {}", id);
                        Ok(None)
                    }
                } else {
                    error!("Failed to read power state for lightbulb {}", id);
                    Ok(None)
                }
            }
                .boxed()
        }));
    }

    pub async fn setup_update(id: &str, client: Arc<ComelitClient>, lightbulb_accessory: &mut LightbulbAccessory) {
        let id = id.to_string();
        lightbulb_accessory
            .lightbulb
            .power_state
            .on_update_async(Some(move |current_val: bool, new_val: bool| {
                info!(
                    "Lightbulb {}: power_state characteristic updated from {} to {}",
                    id, current_val, new_val
                );
                let c = client.clone();
                let id = id.clone();
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
    }
}

impl ComelitAccessory for ComelitLightbulbAccessory {

    fn id(&self) -> &str {
        self.id.as_str()
    }

    async fn update(&self, data: &HomeDeviceData) -> Result<()>{
        if let Light(light_data) = data {
            let id = self.id();
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
        Ok(())
    }
}
