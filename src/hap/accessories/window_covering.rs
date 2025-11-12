use std::sync::Arc;

use anyhow::{Context, Result};
use futures::FutureExt;
use hap::{
    accessory::{AccessoryInformation, window_covering::WindowCoveringAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    server::{IpServer, Server},
};
use tracing::info;

use crate::{
    hap::accessories::AccessoryPointer,
    protocol::{client::ComelitClient, out_data_messages::WindowCoveringDeviceData},
};
use crate::hap::accessories::ComelitAccessory;
use crate::protocol::out_data_messages::{DeviceStatus, HomeDeviceData};

pub(crate) struct ComelitWindowCoveringAccessory {
    accessory: AccessoryPointer,
    data: WindowCoveringDeviceData,
}

impl ComelitWindowCoveringAccessory {
    pub(crate) async fn new(
        id: u64,
        window_covering_data: WindowCoveringDeviceData,
        _client: Arc<ComelitClient>,
        server: &IpServer,
    ) -> Result<Self> {
        let device_id = window_covering_data.data.id.clone();
        let name = window_covering_data
            .data
            .description
            .clone()
            .unwrap_or(device_id.clone());
        let name = name.clone();
        let mut wc_accessory = WindowCoveringAccessory::new(
            id,
            AccessoryInformation {
                name,
                ..Default::default()
            },
        )
            .context("Cannot create lightbulb accessory")?;

        wc_accessory
            .window_covering
            .current_position
            .on_update_async(Some(|pos1, pos2| {
                async move {
                    info!("Current position for the window covering set to {pos1}, {pos2}");
                    Ok(())
                }
                    .boxed()
            }));

        Ok(Self {
            accessory: server.add_accessory(wc_accessory).await?,
            data: window_covering_data,
        })
    }
}

impl ComelitAccessory for ComelitWindowCoveringAccessory {
    fn id(&self) -> &str {
        &self.data.data.id
    }

    async fn update(&self, window_covering: &HomeDeviceData) -> Result<()> {
        if let HomeDeviceData::WindowCovering(window_covering_data) = window_covering {
            if let Some(status) = window_covering_data.open_status.as_ref() {
                let mut accessory = self.accessory.lock().await;
                let service = accessory.get_mut_service(hap::HapType::WindowCovering).unwrap();
                let position = service
                    .get_mut_characteristic(hap::HapType::CurrentPosition).unwrap();
                position.set_value(serde_json::Value::Number(if *status == DeviceStatus::On { 100.into() } else { 0.into() }))
                    .await
                    .context("Cannot update window covering position")?;
                info!(
                    "Updated window covering {} position to {:?}",
                    self.data.data.id, status
                );
            }
        }
        Ok(())
    }
}
