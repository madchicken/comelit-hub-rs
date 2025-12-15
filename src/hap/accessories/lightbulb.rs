use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::Result;
use futures::FutureExt;
use hap::characteristic::HapCharacteristic;
use hap::{
    accessory::{AccessoryInformation, lightbulb::LightbulbAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    server::{IpServer, Server},
};
use serde_json::Value;
use tracing::{debug, error, info};

use crate::hap::accessories::comelit_accessory::ComelitAccessory;
use crate::hap::accessories::state::light::LightState;
use crate::protocol::out_data_messages::DeviceStatus;
use crate::{
    hap::accessories::AccessoryPointer,
    protocol::{client::ComelitClient, out_data_messages::LightDeviceData},
};

pub(crate) struct ComelitLightbulbAccessory {
    lightbulb_accessory: AccessoryPointer,
    id: String,
    state: Arc<LightState>,
}

impl ComelitLightbulbAccessory {
    pub(crate) async fn new(
        id: u64,
        light_data: &LightDeviceData,
        client: ComelitClient,
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
        )?;

        lightbulb_accessory.lightbulb.brightness = None;
        lightbulb_accessory.lightbulb.color_temperature = None;
        lightbulb_accessory.lightbulb.hue = None;
        lightbulb_accessory.lightbulb.saturation = None;
        lightbulb_accessory
            .lightbulb
            .characteristic_value_active_transition_count = None;
        lightbulb_accessory
            .lightbulb
            .characteristic_value_transition_control = None;
        lightbulb_accessory
            .lightbulb
            .supported_characteristic_value_transition_configuration = None;

        let state = Arc::new(LightState::from(light_data));
        debug!(?state, "Created Lightbulb state: {light_data:#?}");
        lightbulb_accessory
            .lightbulb
            .power_state
            .set_value(Value::Bool(state.on.load(Ordering::Acquire)))
            .await?;

        Self::setup_update(device_id.as_str(), client.clone(), &mut lightbulb_accessory).await;
        Self::setup_read(device_id.as_str(), state.clone(), &mut lightbulb_accessory).await;

        Ok(Self {
            lightbulb_accessory: server.add_accessory(lightbulb_accessory).await?,
            id: device_id,
            state,
        })
    }

    pub async fn setup_read(
        id: &str,
        state: Arc<LightState>,
        lightbulb_accessory: &mut LightbulbAccessory,
    ) {
        let id = id.to_string();
        lightbulb_accessory
            .lightbulb
            .power_state
            .on_read_async(Some(move || {
                info!("Lightbulb read {} from lightbulb", id);
                let state = state.clone();
                let id = id.clone();
                async move {
                    let value = state.on.load(Ordering::Acquire);
                    info!("Lightbulb {} read: {}", id, value);
                    Ok(Some(value))
                }
                .boxed()
            }));
    }

    pub async fn setup_update(
        id: &str,
        client: ComelitClient,
        lightbulb_accessory: &mut LightbulbAccessory,
    ) {
        let id = id.to_string();
        lightbulb_accessory
            .lightbulb
            .power_state
            .on_update_async(Some(move |current_val: bool, new_val: bool| {
                let c = client.clone();
                let id = id.clone();
                async move {
                    if new_val != current_val
                        && c.toggle_device_status(id.as_str(), new_val).await.is_err()
                    {
                        error!("Failed to update power state for lightbulb {}", id);
                    } else {
                        info!(
                            "Lightbulb {}: power_state characteristic updated from {} to {}",
                            id, current_val, new_val
                        );
                    }
                    Ok(())
                }
                .boxed()
            }));
    }
}

impl ComelitAccessory<LightDeviceData> for ComelitLightbulbAccessory {
    fn get_comelit_id(&self) -> &str {
        self.id.as_str()
    }

    async fn update(&mut self, light_data: &LightDeviceData) -> Result<()> {
        let id = self.get_comelit_id();
        let is_on = light_data.data.status.clone().unwrap_or_default() == DeviceStatus::On;
        let mut accessory = self.lightbulb_accessory.lock().await;
        let service = accessory.get_mut_service(hap::HapType::Lightbulb).unwrap();
        let old_value: bool = self.state.on.load(Ordering::Acquire);
        let power_state = service
            .get_mut_characteristic(hap::HapType::PowerState)
            .unwrap();
        if old_value != is_on {
            if power_state.set_value(Value::Bool(is_on)).await.is_err() {
                error!("Failed to update power state for device {id}");
            } else {
                info!(
                    "Updated power state for device {id}: {:?}",
                    if is_on { "On" } else { "Off" }
                );
                self.state.on.store(is_on, Ordering::Release);
            }
        }
        Ok(())
    }
}
