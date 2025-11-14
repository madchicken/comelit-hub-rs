use std::sync::Arc;

use anyhow::{Context, Result};
use futures::FutureExt;
use hap::{
    accessory::{AccessoryInformation, window_covering::WindowCoveringAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    server::{IpServer, Server},
};
use tracing::{debug, error, info};

use crate::hap::accessories::ComelitAccessory;
use crate::protocol::out_data_messages::{DeviceStatus, HomeDeviceData};
use crate::{
    hap::accessories::AccessoryPointer,
    protocol::{client::ComelitClient, out_data_messages::WindowCoveringDeviceData},
};

pub(crate) struct ComelitWindowCoveringAccessory {
    accessory: AccessoryPointer,
    data: WindowCoveringDeviceData,
}

impl ComelitWindowCoveringAccessory {
    pub(crate) async fn new(
        id: u64,
        window_covering_data: WindowCoveringDeviceData,
        client: Arc<ComelitClient>,
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

        info!(
            "Created window covering accessory: {:?}",
            window_covering_data
        );
        Self::setup_read(device_id.as_str(), client.clone(), &mut wc_accessory).await;
        Self::setup_update(device_id.as_str(), client.clone(), &mut wc_accessory).await;

        Ok(Self {
            accessory: server.add_accessory(wc_accessory).await?,
            data: window_covering_data,
        })
    }

    pub async fn setup_read(id: &str, client: Arc<ComelitClient>, accessory: &mut WindowCoveringAccessory) {
        let id = id.to_string();
        accessory
            .window_covering
            .current_position.on_read_async(Some(move || {
            info!("Window covering position read {}", id);
            let client = client.clone();
            let id = id.clone();
            async move {
                if let Ok(statuses) = client.info(id.as_str(), 1).await {
                    if let Some(first) = statuses.first() {
                        debug!("Read internal status for window covering {}: {:?}", id, first);
                        let status = first.status.as_ref().unwrap();
                        Ok(Some(if *status == DeviceStatus::On { 100 } else { 0 }))
                    } else {
                        error!("No status returned for window covering {}", id);
                        Ok(None)
                    }
                } else {
                    error!("Failed to read power state for window covering {}", id);
                    Ok(None)
                }
            }.boxed()
        }));
    }

    pub async fn setup_update(
        id: &str,
        client: Arc<ComelitClient>,
        accessory: &mut WindowCoveringAccessory,
    ) {
        let id = id.to_string();
        accessory
            .window_covering
            .current_position
            .on_update_async(Some(move |pos1, pos2| {
                let c = client.clone();
                info!("Current position for the window covering {id} set to {pos1}, {pos2}");
                let id = id.clone();
                async move {
                    c.toggle_blind_position(&id, pos1 as u8)
                        .await
                        .context("Cannot set blind position via ComelitClient")?;
                    Ok(())
                }
                .boxed()
            }));
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
                let service = accessory
                    .get_mut_service(hap::HapType::WindowCovering)
                    .unwrap();
                let position = service
                    .get_mut_characteristic(hap::HapType::CurrentPosition)
                    .unwrap();
                position
                    .set_value(serde_json::Value::Number(if *status == DeviceStatus::On {
                        100.into()
                    } else {
                        0.into()
                    }))
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
