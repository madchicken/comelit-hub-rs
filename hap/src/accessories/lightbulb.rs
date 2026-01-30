use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::Result;
use futures::FutureExt;
use hap::HapType;
use hap::characteristic::{CharacteristicCallbacks, HapCharacteristic};
use hap::{
    accessory::{AccessoryInformation, lightbulb::LightbulbAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    pointer::Accessory as AccessoryPointer,
    server::{IpServer, Server},
};
use serde_json::Value;
use tracing::{debug, error, info};

use crate::accessories::comelit_accessory::ComelitAccessory;
use crate::accessories::state::light::LightState;
use comelit_client_rs::{ComelitClient, DeviceStatus, LightDeviceData};

pub(crate) struct ComelitLightbulbAccessory {
    id: String,
    state: Arc<LightState>,
    accessory: AccessoryPointer,
}

impl ComelitLightbulbAccessory {
    pub(crate) async fn new(
        id: u64,
        light_data: &LightDeviceData,
        client: ComelitClient,
        server: &IpServer,
    ) -> Result<Self> {
        let device_id = light_data.id.clone();
        let name = light_data.description.clone().unwrap_or(device_id.clone());

        let mut lightbulb_accessory = LightbulbAccessory::new(
            id,
            AccessoryInformation {
                name,
                manufacturer: "Comelit".to_string(),
                serial_number: device_id.clone(),
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

        Self::setup_read(device_id.as_str(), state.clone(), &mut lightbulb_accessory).await;
        Self::setup_update(
            device_id.as_str(),
            client.clone(),
            &mut lightbulb_accessory,
            state.clone(),
        )
        .await;

        let accessory = server.add_accessory(lightbulb_accessory).await?;
        Ok(Self {
            id: device_id,
            state,
            accessory,
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
            .on_read(Some(move || {
                let value = state.on.load(Ordering::Acquire);
                debug!("Lightbulb {} read: {}", id, value);
                Ok(Some(value))
            }));
    }

    pub async fn setup_update(
        id: &str,
        client: ComelitClient,
        lightbulb_accessory: &mut LightbulbAccessory,
        state: Arc<LightState>,
    ) {
        let id = id.to_string();
        lightbulb_accessory
            .lightbulb
            .power_state
            .on_update_async(Some(move |current_val: bool, new_val: bool| {
                let c = client.clone();
                let id = id.clone();
                let current_value = state.on.load(Ordering::Acquire);
                async move {
                    if new_val != current_value {
                        if c.toggle_device_status(id.as_str(), new_val).await.is_ok() {
                            info!(
                                "Lightbulb {}: power_state characteristic updated from {} to {}",
                                id, current_val, new_val
                            );
                        } else {
                            error!("Failed to update power state for lightbulb {}", id);
                        }
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
        let is_on = light_data.status.clone().unwrap_or_default() == DeviceStatus::On;
        self.state.on.store(is_on, Ordering::Release);
        let mut accessory = self.accessory.lock().await;
        let service = accessory.get_mut_service(HapType::Lightbulb).unwrap();
        service
            .get_mut_characteristic(HapType::PowerState)
            .unwrap()
            .update_value(Value::from(is_on))
            .await?;

        info!(
            "Updated power state for device {id}: {:?}",
            if is_on { "On" } else { "Off" }
        );
        Ok(())
    }
}
