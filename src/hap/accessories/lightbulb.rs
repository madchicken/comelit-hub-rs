use std::sync::Arc;

use anyhow::{Context, Result};
use hap::{
    accessory::{AccessoryInformation, lightbulb::LightbulbAccessory},
    characteristic::{CharacteristicCallbacks, HapCharacteristic},
};
use serde_json::Value;
use tracing::{error, info};

use crate::protocol::{
    client::ComelitClient,
    out_data_messages::{ActionType, LightDeviceData},
};

pub(crate) struct ComelitLightbulbAccessory {
    pub(crate) id: String,
    pub(crate) lightbulb_accessory: LightbulbAccessory,
    client: Arc<ComelitClient>,
    data: LightDeviceData,
}

impl ComelitLightbulbAccessory {
    pub(crate) fn new(
        id: u64,
        light_data: LightDeviceData,
        client: Arc<ComelitClient>,
    ) -> Result<Self> {
        let name = light_data
            .data
            .description
            .clone()
            .unwrap_or(light_data.data.id.clone());
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

        lightbulb_accessory.lightbulb.power_state.on_update(Some(
            move |current_val: &bool, new_val: &bool| {
                info!(
                    "Lightbulb {}: power_state characteristic updated from {} to {}",
                    lightbulb_name, current_val, new_val
                );
                Ok(())
            },
        ));
        Ok(Self {
            id: light_data.data.id.clone(),
            data: light_data,
            lightbulb_accessory,
            client,
        })
    }

    pub async fn on(&mut self) -> Result<()> {
        match self
            .client
            .send_action(&self.data.data.id, ActionType::Set, 1)
            .await
        {
            Ok(_) => {
                if self
                    .lightbulb_accessory
                    .lightbulb
                    .power_state
                    .set_value(Value::Bool(true))
                    .await
                    .is_err()
                {
                    error!("Failed to set power state to true");
                }
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn off(&mut self) -> Result<()> {
        match self
            .client
            .send_action(&self.data.data.id, ActionType::Set, 0)
            .await
        {
            Ok(_) => {
                if self
                    .lightbulb_accessory
                    .lightbulb
                    .power_state
                    .set_value(Value::Bool(false))
                    .await
                    .is_err()
                {
                    error!("Failed to set power state to false");
                }
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}
