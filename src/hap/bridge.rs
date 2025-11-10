use qr2term::qr;
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
use std::fmt::Write;
use tracing::{debug, error, info};
// Use thiserror or anyhow for better error handling
use crate::{
    hap::accessories::{ComelitLightbulbAccessory, ComelitWindowCoveringAccessory},
    protocol::out_data_messages::{LightDeviceData, WindowCoveringDeviceData},
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
    lights: DashMap<String, ComelitLightbulbAccessory>,
    coverings: DashMap<String, ComelitWindowCoveringAccessory>,
}

#[async_trait]
impl StatusUpdate for Updater {
    async fn status_update(&self, device: &HomeDeviceData) {
        debug!("Status update: {:?}", device);
        match device {
            HomeDeviceData::Agent(agent_device_data) => {}
            HomeDeviceData::Data(device_data) => {}
            HomeDeviceData::Other(other_device_data) => {}
            HomeDeviceData::Light(light_device_data) => {
                if let Some(mut accessory) = self.lights.get_mut(&device.id()) {
                    if let HomeDeviceData::Light(lightbulb) = device {
                        accessory.update(lightbulb).await;
                    }
                }
            }
            HomeDeviceData::WindowCovering(window_covering_device_data) => {
                if let Some(mut accessory) = self.coverings.get_mut(&device.id()) {
                    if let HomeDeviceData::WindowCovering(window_covering) = device {
                        accessory.update(window_covering).await;
                    }
                }
            }
            HomeDeviceData::Outlet(outlet_device_data) => {}
            HomeDeviceData::Irrigation(irrigation_device_data) => {}
            HomeDeviceData::Thermostat(thermostat_device_data) => {}
            HomeDeviceData::Supplier(supplier_device_data) => {
                info!("Received update for supplier {supplier_device_data:?}");
            }
            HomeDeviceData::Bell(bell_device_data) => {}
            HomeDeviceData::Door(door_device_data) => {}
        }
    }
}

fn generate_setup_uri(pincode: &str, category: u32, setup_id: &str) -> String {
    // Rimuove i '-' e converte in numero
    let value_low_str = pincode.replace('-', "");
    let mut value_low = u32::from_str_radix(&value_low_str, 10).expect("Invalid pincode format");

    let value_high = category >> 1;

    // Supporta IP (bit 28)
    value_low |= 1 << 28;

    // Simula un buffer di 8 byte (big endian)
    let mut buffer = [0u8; 8];
    buffer[0..4].copy_from_slice(&value_high.to_be_bytes());
    buffer[4..8].copy_from_slice(&value_low.to_be_bytes());

    // Se il category è dispari, imposta il bit 7 di buffer[4]
    if category & 1 != 0 {
        buffer[4] |= 1 << 7;
    }

    // Ricostruisce il valore a 64 bit combinando le due parti big endian
    let high = u64::from(u32::from_be_bytes(buffer[0..4].try_into().unwrap()));
    let low = u64::from(u32::from_be_bytes(buffer[4..8].try_into().unwrap()));
    let combined = (high << 32) | low;

    // Converte in base36 e uppercase
    let mut encoded_payload = base36_encode(combined).to_uppercase();

    // Padding a 9 caratteri
    while encoded_payload.len() < 9 {
        encoded_payload.insert(0, '0');
    }

    format!("X-HM://{}{}", encoded_payload, setup_id)
}

// Funzione per convertire un numero in base 36
fn base36_encode(mut num: u64) -> String {
    let mut chars = Vec::new();
    while num > 0 {
        let rem = (num % 36) as u8;
        chars.push(if rem < 10 {
            (b'0' + rem) as char
        } else {
            (b'A' + rem - 10) as char
        });
        num /= 36;
    }
    chars.reverse();
    if chars.is_empty() {
        chars.push('0');
    }
    chars.into_iter().collect()
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
            serial_number: "ABCD1234".into(),
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
            let device_id: [u8; 6] = client.mac_address.as_bytes()[..6].try_into()?;
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
                name: "Comelit Bridge (Rust)".into(),
                device_id: MacAddress::from(device_id),
                category: AccessoryCategory::Bridge,
                ..Default::default()
            };
            storage.save_config(&config).await?;
            config
        }
    };

    let pin = config.pin.clone().to_string();
    let device_id = config.device_id.to_string();
    let server = IpServer::new(config, storage).await?;
    info!("IP server created, adding bridge accessory...");
    server.add_accessory(bridge).await?;

    info!("Fetching device index...");
    let index = client
        .fetch_index()
        .await
        .context("Failed to fetch index")?;
    let lights: Vec<LightDeviceData> = index
        .clone()
        .into_iter()
        .filter_map(|(_, v)| match v {
            HomeDeviceData::Light(light) => Some(light),
            _ => None,
        })
        .collect();

    for (index, light) in lights.iter().enumerate() {
        info!("Adding light device: {}", light.data.id);
        match ComelitLightbulbAccessory::new(index as u64, light.clone(), client.clone(), &server)
            .await
        {
            Ok(accessory) => {
                info!("Light {} added to the hub", accessory.id());
                updater.lights.insert(accessory.id().to_string(), accessory);
            }
            Err(err) => error!("Failed to add light device: {}", err),
        };
    }

    let window_coverings: Vec<WindowCoveringDeviceData> = index
        .into_iter()
        .filter_map(|(_, v)| match v {
            HomeDeviceData::WindowCovering(covering) => Some(covering),
            _ => None,
        })
        .collect();

    for (index, covering) in window_coverings.iter().enumerate() {
        info!("Adding light device: {}", covering.data.id);
        match ComelitWindowCoveringAccessory::new(
            index as u64,
            covering.clone(),
            client.clone(),
            &server,
        )
        .await
        {
            Ok(accessory) => {
                info!("Window covering {} added to the hub", accessory.id());
                updater
                    .coverings
                    .insert(accessory.id().to_string(), accessory);
            }
            Err(err) => error!("Failed to add light device: {}", err),
        };
    }

    info!("Starting HAP bridge server...");
    let handle = server.run_handle();
    info!("PIN for the Bridge accessory is: {pin}");
    let uri = generate_setup_uri(pin.to_string().as_str(), 2, device_id.as_str());
    qr2term::print_qr(uri)?;
    client.subscribe(ROOT_ID).await?;
    handle.await.context("Failed to run server")?;
    client
        .as_ref()
        .disconnect()
        .await
        .context("Failed to disconnect client")?;
    Ok(())
}
