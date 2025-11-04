use futures::lock::Mutex;
use serde_json::Value;
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use hap::{
    Config, MacAddress, Pin,
    accessory::{
        AccessoryCategory, AccessoryInformation, HapAccessory, bridge::BridgeAccessory,
    },
    server::{IpServer, Server},
    storage::{FileStorage, Storage},
};
use tracing::{debug, error, info};
// Use thiserror or anyhow for better error handling
use crate::{
    hap::accessories::ComelitLightbulbAccessory, protocol::out_data_messages::DeviceStatus,
};
use anyhow::{Context, Result};

use crate::{
    protocol::{
        client::{ComelitClient, ComelitClientError, ComelitOptions, State, StatusUpdate},
        credentials::get_secrets,
        out_data_messages::HomeDeviceData,
    },
};

type AccessoryPointer = Arc<Mutex<Box<dyn HapAccessory>>>;

#[derive(Default)]
struct Updater {
    accessories: DashMap<String, AccessoryPointer>,
}

#[async_trait]
impl StatusUpdate for Updater {
    async fn status_update(&self, device: &HomeDeviceData) {
        debug!("Status update: {:?}", device);
        if let Some(accessory) = self.accessories.get(&device.id()) {
            let id = accessory.key().to_string();
            if let HomeDeviceData::Light(lightbulb) = device {
                if let Some(state) = lightbulb.data.status.as_ref() {
                    let mut accessory = accessory.lock().await;
                    let service =
                        accessory.get_mut_service(hap::HapType::Lightbulb).unwrap();
                    let power_state = service
                        .get_mut_characteristic(hap::HapType::PowerState)
                        .unwrap();
                    if power_state.set_value(Value::Bool(*state == DeviceStatus::On)).await.is_err() {
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
    }
}

pub async fn start_bridge(user: &str, password: &str, host: Option<String>, port: Option<u16>) -> Result<()> {
    let (mqtt_user, mqtt_password) = get_secrets();
    let options = ComelitOptions::builder()
        .user(Some(user.into()))
        .password(Some(password.into()))
        .mqtt_user(mqtt_user)
        .mqtt_password(mqtt_password)
        .host(host)
        .port(port)
        .build()
        .map_err(|e| ComelitClientError::Generic(e.to_string()))?;
    let updater = Arc::new(Updater::default());
    let client = Arc::new(ComelitClient::new(options, updater.clone()).await?);
    if let Err(e) = client.login(State::Disconnected).await {
        error!("Login failed: {}", e);
        return Err(e.into());
    } else {
        info!("Login successful");
    }

    let bridge = BridgeAccessory::new(
        10000,
        AccessoryInformation {
            name: "Comelit Bridge".into(),
            ..Default::default()
        },
    )?;

    let mut storage = FileStorage::current_dir().await?;

    let config = match storage.load_config().await {
        Ok(mut config) => {
            config.redetermine_local_ip();
            storage.save_config(&config).await?;
            config
        }
        Err(_) => {
            let config = Config {
                pin: Pin::new([1, 1, 2, 2, 3, 3, 4, 4])?,
                name: "Comelit Bridge".into(),
                device_id: MacAddress::from([10, 20, 30, 40, 50, 60]),
                category: AccessoryCategory::Bridge,
                ..Default::default()
            };
            storage.save_config(&config).await?;
            config
        }
    };

    let server = IpServer::new(config, storage).await?;
    info!("IP server created, adding bridge accessory...");
    server.add_accessory(bridge).await?;

    info!("Fetching device index...");
    let index = client
        .fetch_index()
        .await
        .context("Failed to fetch index")?;
    let accessories: Vec<ComelitLightbulbAccessory> = index
        .into_iter()
        .filter_map(|(_, v)| match v {
            HomeDeviceData::Light(light) => Some(light),
            _ => None,
        })
        .enumerate()
        .filter_map(|(index, light)| {
            // Process each light device here
            info!("Adding light device: {}", light.data.id);
            ComelitLightbulbAccessory::new(index as u64, light.clone(), client.clone()).ok()
        })
        .collect();
    for lightbulb in accessories {
        let ptr = server.add_accessory(lightbulb.lightbulb_accessory).await?;
        updater.accessories.insert(lightbulb.id, ptr);
    }

    info!("Starting HAP bridge server...");
    let handle = server.run_handle();
    qr2term::print_qr("11223344")?;
    handle.await.context("Failed to run server")?;
    client
        .as_ref()
        .disconnect()
        .await
        .context("Failed to disconnect client")?;
    Ok(())
}
