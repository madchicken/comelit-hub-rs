use crate::accessories::{
    ComelitAccessory, ComelitLightbulbAccessory, ComelitThermostatAccessory,
    ComelitWindowCoveringAccessory, WindowCoveringConfig,
};
use crate::settings::Settings;
use anyhow::{Context, Result};
use async_trait::async_trait;
use comelit_hub_rs::ROOT_ID;
use comelit_hub_rs::{
    ComelitClient, ComelitClientError, ComelitOptions, HomeDeviceData, State, StatusUpdate,
    get_secrets,
};
use dashmap::DashMap;
use hap::{
    Config, MacAddress, Pin,
    accessory::{AccessoryCategory, AccessoryInformation, bridge::BridgeAccessory},
    server::{IpServer, Server},
    storage::{FileStorage, Storage},
};
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tracing::{error, info, warn};

#[derive(Default)]
struct Updater {
    lights: DashMap<String, ComelitLightbulbAccessory>,
    window_coverings: DashMap<String, ComelitWindowCoveringAccessory>,
    thermostats: DashMap<String, ComelitThermostatAccessory>,
}

#[async_trait]
impl StatusUpdate for Updater {
    async fn status_update(&self, device: &HomeDeviceData) {
        match device {
            HomeDeviceData::Agent(_) => {}
            HomeDeviceData::Data(_) => {}
            HomeDeviceData::Other(_) => {}
            HomeDeviceData::Light(data) => {
                if let Some(mut accessory) = self.lights.get_mut(&device.id()) {
                    accessory.update(data).await.unwrap_or_else(|e| {
                        error!(
                            "Failed to update light accessory {}: {}",
                            accessory.get_comelit_id(),
                            e
                        );
                    });
                } else {
                    warn!("Received update for unknown light device");
                }
            }
            HomeDeviceData::WindowCovering(data) => {
                if let Some(mut accessory) = self.window_coverings.get_mut(&device.id()) {
                    accessory.update(data).await.unwrap_or_else(|e| {
                        error!(
                            "Failed to update window covering accessory {}: {}",
                            accessory.get_comelit_id(),
                            e
                        );
                    })
                } else {
                    warn!("Received update for unknown window covering device");
                }
            }
            HomeDeviceData::Outlet(_outlet_device_data) => {}
            HomeDeviceData::Irrigation(_irrigation_device_data) => {}
            HomeDeviceData::Thermostat(data) => {
                if let Some(mut accessory) = self.thermostats.get_mut(&device.id()) {
                    accessory.update(data).await.unwrap_or_else(|e| {
                        error!(
                            "Failed to update thermostat accessory {}: {}",
                            device.id(),
                            e
                        );
                    });
                } else {
                    warn!("Received update for unknown thermostat/dehumidifier device");
                }
            }
            HomeDeviceData::Supplier(supplier_device_data) => {
                info!("Received update for supplier {supplier_device_data:?}");
            }
            HomeDeviceData::Bell(_bell_device_data) => {}
            HomeDeviceData::Door(_door_device_data) => {}
        }
    }
}

fn generate_setup_uri(pincode: &str, category: u64, setup_id: &str) -> String {
    // Rimuove i '-' e converte in numero
    let value_low_str = pincode.replace('-', "");
    let value_low = value_low_str.parse::<u64>().unwrap_or(0);

    let version = 0;
    let reserved = 0;
    let flag = 2;
    let mut payload: u64 = 0;

    payload |= version & 0x7;
    payload <<= 4;
    payload |= reserved & 0xf;

    payload <<= 8;
    payload |= category & 0xff;

    payload <<= 4;
    payload |= flag & 0xf;
    payload <<= 27u64;
    payload |= value_low & 0x07ff_ffff;

    // Converte in base36 e uppercase
    let mut encoded_payload = base36_encode(payload).to_uppercase();

    // Padding a 9 caratteri
    while encoded_payload.len() < 9 {
        encoded_payload.insert(0, '0');
    }

    format!("X-HM://{encoded_payload}{setup_id}")
}

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
    settings: Settings,
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
    let client = ComelitClient::new(options, Some(updater.clone())).await?;
    if let Err(e) = client.login(State::Disconnected).await {
        error!("Login failed: {}", e);
        return Err(e.into());
    } else {
        info!("Login successful");
    }

    let bridge = BridgeAccessory::new(
        1,
        AccessoryInformation {
            name: "Comelit HUB".into(),
            serial_number: "20003150".into(),
            manufacturer: "Comelit".into(),
            model: "COMELIT HUB".into(),
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
            let device_id: [u8; 6] = client.mac_address().as_bytes()[..6].try_into()?;
            let pin = loop {
                if let Ok(pin) = Pin::new(settings.pairing_code) {
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
    let server = IpServer::new(config, storage).await?;
    info!("IP server created, adding bridge accessory...");
    server.add_accessory(bridge).await?;

    info!("Fetching device index...");
    let index = client
        .fetch_index()
        .await
        .context("Failed to fetch index")?;

    let Settings {
        mount_lights,
        mount_window_covering,
        mount_thermo,
        ..
    } = settings;

    let mut lights = vec![];
    let mut thermostats = vec![];
    let mut window_coverings = vec![];
    for (_, v) in index.clone().into_iter() {
        match v {
            HomeDeviceData::Light(light) => {
                lights.push(light.clone());
            }
            HomeDeviceData::WindowCovering(window_covering) => {
                window_coverings.push(window_covering.clone());
            }
            HomeDeviceData::Thermostat(thermo) => {
                thermostats.push(thermo.clone());
            }
            _ => {}
        }
    }
    lights.sort_by_key(|l| l.data.id.clone());
    window_coverings.sort_by_key(|wc| wc.data.id.clone());
    thermostats.sort_by_key(|t| t.data.id.clone());

    let mut i: u64 = 1;
    for light in lights {
        if mount_lights.unwrap_or_default() {
            i += 1;
            info!("Adding light device: {} with id {i}", light.data.id);
            match ComelitLightbulbAccessory::new(i, &light, client.clone(), &server).await {
                Ok(accessory) => {
                    info!("Light {} added to the hub", accessory.get_comelit_id());
                    updater
                        .lights
                        .insert(accessory.get_comelit_id().to_string(), accessory);
                }
                Err(err) => error!("Failed to add light device: {}", err),
            }
        }
    }

    for window_covering in window_coverings {
        if mount_window_covering.unwrap_or_default() {
            i += 1;
            info!(
                "Adding window covering device: {} with id {i}",
                window_covering.data.id
            );
            match ComelitWindowCoveringAccessory::new(
                i,
                &window_covering,
                client.clone(),
                &server,
                WindowCoveringConfig {
                    closing_time: Duration::from_secs(30),
                    opening_time: Duration::from_secs(30),
                },
            )
            .await
            {
                Ok(accessory) => {
                    info!(
                        "Window covering {} added to the hub",
                        accessory.get_comelit_id()
                    );
                    updater
                        .window_coverings
                        .insert(accessory.get_comelit_id().to_string(), accessory);
                }
                Err(err) => error!("Failed to add window covering device: {}", err),
            }
        }
    }

    for thermostat in thermostats {
        if mount_thermo.unwrap_or_default() {
            i += 1;
            info!(
                "Adding thermostat device: {} with id {i}",
                thermostat.data.id
            );
            match ComelitThermostatAccessory::new(i, &thermostat, client.clone(), &server).await {
                Ok(accessory) => {
                    updater
                        .thermostats
                        .insert(accessory.get_comelit_id().to_string(), accessory);
                }
                Err(err) => error!("Failed to add thermostat device: {}", err),
            };
        }
    }

    info!("Starting HAP bridge server...");
    let handle = server.run_handle();
    let setup_id = "XYZK"; // some random string
    info!("PIN for the Bridge accessory is: {pin}, setup ID: {setup_id}");
    let uri = generate_setup_uri(pin.to_string().as_str(), 2, setup_id);
    qr2term::print_qr(uri)?;
    info!("Subscribing to root device updates...");
    client.subscribe(ROOT_ID).await?;

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        res = handle => {
            client
                .disconnect()
                .await
                .context("Failed to disconnect client")?;
            res.with_context(|| "Failed to disconnect client")
        }
        _ = ctrl_c => {
            info!("signal received, starting graceful shutdown");
            client
                .disconnect()
                .await
                .context("Failed to disconnect client")?;
            Ok(())
        },
        _ = terminate => {
            info!("signal received, starting graceful shutdown");
            client
                .disconnect()
                .await
                .context("Failed to disconnect client")?;
            Ok(())
        },
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_generate_setup_uri() {
        let pincode = "841-31-633";
        let category = 8; // Switch
        let setup_id = "";
        let uri = super::generate_setup_uri(pincode, category, setup_id);
        assert_eq!(uri, "X-HM://0081YCYEP");
    }

    #[test]
    fn test_generate_setup_uri_with_setup_id() {
        let pincode = "841-31-633";
        let category = 8; // Switch
        let setup_id = "3QYT";
        let uri = super::generate_setup_uri(pincode, category, setup_id);
        assert_eq!(uri, "X-HM://0081YCYEP3QYT");
    }
}
