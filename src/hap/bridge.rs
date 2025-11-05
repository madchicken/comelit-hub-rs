use rand::Rng;
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use hap::{
    Config, MacAddress, Pin,
    accessory::{AccessoryCategory, AccessoryInformation, bridge::BridgeAccessory},
    server::{IpServer, Server},
    storage::{FileStorage, Storage},
};
use tracing::{debug, error, info};
// Use thiserror or anyhow for better error handling
use crate::{
    hap::accessories::ComelitLightbulbAccessory, protocol::out_data_messages::LightDeviceData,
};
use anyhow::{Context, Result};

use crate::protocol::client::ROOT_ID;
use crate::protocol::{
    client::{ComelitClient, ComelitClientError, ComelitOptions, State, StatusUpdate},
    credentials::get_secrets,
    out_data_messages::HomeDeviceData,
};

#[derive(Default)]
struct Updater {
    accessories: DashMap<String, ComelitLightbulbAccessory>,
}

#[async_trait]
impl StatusUpdate for Updater {
    async fn status_update(&self, device: &HomeDeviceData) {
        debug!("Status update: {:?}", device);
        if let Some(mut accessory) = self.accessories.get_mut(&device.id()) {
            if let HomeDeviceData::Light(lightbulb) = device {
                accessory.update(lightbulb).await;
            }
        }
    }
}

pub async fn start_bridge(
    user: &str,
    password: &str,
    host: Option<String>,
    port: Option<u16>,
) -> Result<()> {
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
            info!("Loaded config");
            config.redetermine_local_ip();
            storage.save_config(&config).await?;
            config
        }
        Err(_) => {
            info!("Creating new config");
            let device_id: [u8; 6] = client.mac_address.as_bytes()[..6].try_into().unwrap();
            let mut rng = rand::thread_rng();
            let pin = loop {
                if let Ok(pin) = Pin::new([
                    rng.gen_range(0..10),
                    rng.gen_range(0..10),
                    rng.gen_range(0..10),
                    rng.gen_range(0..10),
                    rng.gen_range(0..10),
                    rng.gen_range(0..10),
                    rng.gen_range(0..10),
                    rng.gen_range(0..10),
                ]) {
                    break pin;
                } else {
                    continue;
                }
            };
            let config = Config {
                pin,
                name: "Comelit Bridge".into(),
                device_id: MacAddress::from(device_id),
                category: AccessoryCategory::Bridge,
                ..Default::default()
            };
            storage.save_config(&config).await?;
            config
        }
    };

    let pin = config.pin.clone().to_string();
    let server = IpServer::new(config, storage).await?;
    info!("IP server created, adding bridge accessory...");
    server.add_accessory(bridge).await?;

    info!("Fetching device index...");
    let index = client
        .fetch_index()
        .await
        .context("Failed to fetch index")?;
    let accessories: Vec<LightDeviceData> = index
        .into_iter()
        .filter_map(|(_, v)| match v {
            HomeDeviceData::Light(light) => Some(light),
            _ => None,
        })
        .collect();

    for (index, light) in accessories.iter().enumerate() {
        info!("Adding light device: {}", light.data.id);
        match ComelitLightbulbAccessory::new(index as u64, light.clone(), client.clone(), &server)
            .await
        {
            Ok(accessory) => {
                info!("Light {} added to the hub", accessory.id());
                updater
                    .accessories
                    .insert(accessory.id().to_string(), accessory);
            }
            Err(err) => error!("Failed to add light device: {}", err),
        };
    }

    info!("Starting HAP bridge server...");
    let handle = server.run_handle();
    qr2term::print_qr(pin)?;
    client.subscribe(ROOT_ID).await?;
    handle.await.context("Failed to run server")?;
    client
        .as_ref()
        .disconnect()
        .await
        .context("Failed to disconnect client")?;
    Ok(())
}
