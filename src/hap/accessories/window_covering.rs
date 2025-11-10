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
                manufacturer: "Comelit".into(),
                name,
                serial_number: window_covering_data.data.id.clone(),
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

    pub fn id(&self) -> &str {
        &self.data.data.id
    }

    pub async fn update(&mut self, window_covering: &WindowCoveringDeviceData) {
        self.data = window_covering.clone();
    }
}
