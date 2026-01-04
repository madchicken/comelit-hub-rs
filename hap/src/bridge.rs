use crate::accessories::{
    ComelitAccessory, ComelitDoorAccessory, ComelitDoorbellAccessory, ComelitLightbulbAccessory,
    ComelitThermostatAccessory, ComelitWindowCoveringAccessory, DoorConfig, WindowCoveringConfig,
};
use crate::settings::Settings;
use anyhow::{Context, Result};
use async_trait::async_trait;
use comelit_hub_rs::{
    ComelitClient, ComelitClientError, ComelitOptions, DoorbellDeviceData, HomeDeviceData, State,
    StatusUpdate, get_secrets,
};
use comelit_hub_rs::{DoorDeviceData, ROOT_ID};
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
    doors: DashMap<String, ComelitDoorAccessory>,
    doorbells: DashMap<String, ComelitDoorbellAccessory>,
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
            HomeDeviceData::Doorbell(_bell_device_data) => {}
            HomeDeviceData::Door(door_device_data) => {
                if let Some(mut accessory) = self.doors.get_mut(&device.id()) {
                    accessory
                        .update(door_device_data)
                        .await
                        .unwrap_or_else(|e| {
                            error!("Failed to update door accessory {}: {}", device.id(), e);
                        });
                } else {
                    warn!("Received update for unknown door device");
                }
            }
        }
    }
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

    let bridge_name = "ComelitHUB-HK";
    let bridge = BridgeAccessory::new(
        1,
        AccessoryInformation {
            name: bridge_name.into(),
            serial_number: "20003150".into(),
            manufacturer: "Comelit".into(),
            model: "20003150".into(),
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
            info!(
                "Creating new config, device id is {:?}",
                client.mac_address()
            );
            let pin = loop {
                if let Ok(pin) = Pin::new(settings.pairing_code) {
                    break pin;
                } else {
                    continue;
                }
            };
            let config = Config {
                pin,
                name: bridge_name.into(),
                device_id: MacAddress::from(*client.mac_address().as_bytes()),
                category: AccessoryCategory::Bridge,
                ..Default::default()
            };
            storage.save_config(&config).await?;
            config
        }
    };

    let pin = config.pin.clone().to_string();
    let url = config.setup_url();
    let server = IpServer::new(config, storage).await?;
    info!("IP server created, adding bridge accessory...");
    server.add_accessory(bridge).await?;

    info!("Fetching device index...");
    let index = client
        .fetch_index(1)
        .await
        .context("Failed to fetch index")?;

    info!("Fetching device index...");
    let external_index = client
        .fetch_external_devices()
        .await
        .context("Failed to fetch index")?;

    let mut lights = vec![];
    let mut thermostats = vec![];
    let mut window_coverings = vec![];
    let mut doors = vec![];
    let mut bells = vec![];
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
    for (_, v) in external_index.clone().into_iter() {
        match v {
            HomeDeviceData::Door(door) => {
                doors.push(door.clone());
            }
            HomeDeviceData::Doorbell(bell) => {
                bells.push(bell.clone());
            }
            _ => {}
        }
    }

    lights.sort_by_key(|l| l.id.clone());
    window_coverings.sort_by_key(|wc| wc.id.clone());
    thermostats.sort_by_key(|t| t.id.clone());
    doors.sort_by_key(|t| t.id.clone());

    let mut i: u64 = 1;
    for light in lights {
        if settings.mount_lights.unwrap_or_default() {
            i += 1;
            info!("Adding light device: {} with id {i}", light.id);
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
        if settings.mount_window_covering.unwrap_or_default() {
            i += 1;
            info!(
                "Adding window covering device: {} with id {i}",
                window_covering.id
            );
            match ComelitWindowCoveringAccessory::new(
                i,
                &window_covering,
                client.clone(),
                &server,
                WindowCoveringConfig {
                    closing_time: Duration::from_secs(settings.window_covering.closing_time),
                    opening_time: Duration::from_secs(settings.window_covering.opening_time),
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
        if settings.mount_thermo.unwrap_or_default() {
            i += 1;
            info!("Adding thermostat device: {} with id {i}", thermostat.id);
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

    for door in doors {
        if settings.mount_doors.unwrap_or_default() {
            i += 1;
            info!("Adding door device: {} with id {i}", door.id);
            let data = client.info::<DoorDeviceData>(&door.id, 1).await?;
            match ComelitDoorAccessory::new(
                i,
                data.first().unwrap(),
                client.clone(),
                &server,
                DoorConfig {
                    opening_closing_time: Duration::from_secs(settings.door.opening_closing_time),
                    opened_time: Duration::from_secs(settings.door.opened_time),
                    mount_as: crate::accessories::DoorType::Door,
                },
            )
            .await
            {
                Ok(accessory) => {
                    client.subscribe(&door.id).await?;
                    updater
                        .doors
                        .insert(accessory.get_comelit_id().to_string(), accessory);
                }
                Err(err) => error!("Failed to add thermostat device: {}", err),
            };
        }
    }

    for bell in bells {
        if settings.mount_doorbells.unwrap_or_default() {
            i += 1;
            let data = client.info::<DoorbellDeviceData>(&bell.id, 1).await?;
            match ComelitDoorbellAccessory::new(i, data.first().unwrap(), &server).await {
                Ok(accessory) => {
                    client.subscribe(&bell.id).await?;
                    updater
                        .doorbells
                        .insert(accessory.get_comelit_id().to_string(), accessory);
                }
                Err(err) => error!("Failed to add doorbell device: {}", err),
            };
        }
    }

    info!("Starting HAP bridge server...");
    let handle = server.run_handle();

    // Use println! to ensure they are always printed
    qr2term::print_qr(url)?;
    println!("Pair your Comelit Bridge using pin code {pin}");

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
