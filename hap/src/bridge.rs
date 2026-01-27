use crate::accessories::{
    ComelitAccessory, ComelitDoorAccessory, ComelitDoorbellAccessory, ComelitLightbulbAccessory,
    ComelitThermostatAccessory, ComelitWindowCoveringAccessory, DoorConfig, WindowCoveringConfig,
};
use crate::settings::Settings;
use crate::web::metrics::Metrics;
use crate::web::state::{BridgeState, ConnectionStatus, DeviceInfo, DeviceType};
use anyhow::{Context, Result};
use async_trait::async_trait;
use comelit_hub_rs::DeviceStatus;
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
use qrcode_gen::QrCode;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tracing::{error, info, warn};

/// Updater that handles status updates from the Comelit client.
/// Also updates the shared bridge state for the web UI.
struct Updater {
    lights: DashMap<String, ComelitLightbulbAccessory>,
    window_coverings: DashMap<String, ComelitWindowCoveringAccessory>,
    thermostats: DashMap<String, ComelitThermostatAccessory>,
    doors: DashMap<String, ComelitDoorAccessory>,
    doorbells: DashMap<String, ComelitDoorbellAccessory>,
    bridge_state: BridgeState,
}

impl Updater {
    fn new(bridge_state: BridgeState) -> Self {
        Self {
            lights: DashMap::new(),
            window_coverings: DashMap::new(),
            thermostats: DashMap::new(),
            doors: DashMap::new(),
            doorbells: DashMap::new(),
            bridge_state,
        }
    }
}

#[async_trait]
impl StatusUpdate for Updater {
    async fn status_update(&self, device: &HomeDeviceData) {
        match device {
            HomeDeviceData::Agent(_) => {}
            HomeDeviceData::Data(_) => {}
            HomeDeviceData::Other(_) => {}
            HomeDeviceData::Light(data) => {
                Metrics::inc_device_updates("light");
                if let Some(mut accessory) = self.lights.get_mut(&device.id()) {
                    let status = match data.status {
                        Some(DeviceStatus::On) | Some(DeviceStatus::Running) => "on",
                        _ => "off",
                    };
                    self.bridge_state
                        .update_device_status(&device.id(), status.to_string());
                    accessory.update(data).await.unwrap_or_else(|e| {
                        Metrics::inc_device_update_errors("light");
                        error!(
                            "Failed to update light accessory {}: {}",
                            accessory.get_comelit_id(),
                            e
                        );
                    });
                } else {
                    warn!("Received update for unknown light device: {}", device.id());
                }
            }
            HomeDeviceData::WindowCovering(data) => {
                Metrics::inc_device_updates("window_covering");
                if let Some(mut accessory) = self.window_coverings.get_mut(&device.id()) {
                    let status = match &data.status {
                        Some(s) => format!("{:?}", s),
                        None => "unknown".to_string(),
                    };
                    self.bridge_state.update_device_status(&device.id(), status);
                    accessory.update(data).await.unwrap_or_else(|e| {
                        Metrics::inc_device_update_errors("window_covering");
                        error!(
                            "Failed to update window covering accessory {}: {}",
                            accessory.get_comelit_id(),
                            e
                        );
                    })
                } else {
                    warn!(
                        "Received update for unknown window covering device: {}",
                        device.id()
                    );
                }
            }
            HomeDeviceData::Outlet(_outlet_device_data) => {}
            HomeDeviceData::Irrigation(_irrigation_device_data) => {}
            HomeDeviceData::Thermostat(data) => {
                Metrics::inc_device_updates("thermostat");
                if let Some(mut accessory) = self.thermostats.get_mut(&device.id()) {
                    let status = format!("{}°C", data.temperature.as_deref().unwrap_or("--"));
                    self.bridge_state.update_device_status(&device.id(), status);
                    accessory.update(data).await.unwrap_or_else(|e| {
                        Metrics::inc_device_update_errors("thermostat");
                        error!(
                            "Failed to update thermostat accessory {}: {}",
                            device.id(),
                            e
                        );
                    });
                } else {
                    warn!(
                        "Received update for unknown thermostat/dehumidifier device: {}",
                        device.id()
                    );
                }
            }
            HomeDeviceData::Supplier(supplier_device_data) => {
                info!("Received update for supplier {supplier_device_data:?}");
            }
            HomeDeviceData::Doorbell(_bell_device_data) => {
                Metrics::inc_device_updates("doorbell");
            }
            HomeDeviceData::Door(door_device_data) => {
                Metrics::inc_device_updates("door");
                if let Some(mut accessory) = self.doors.get_mut(&device.id()) {
                    let status = match door_device_data.status {
                        Some(DeviceStatus::On) | Some(DeviceStatus::Running) => "open",
                        _ => "closed",
                    };
                    self.bridge_state
                        .update_device_status(&device.id(), status.to_string());
                    accessory
                        .update(door_device_data)
                        .await
                        .unwrap_or_else(|e| {
                            Metrics::inc_device_update_errors("door");
                            error!("Failed to update door accessory {}: {}", device.id(), e);
                        });
                } else {
                    warn!("Received update for unknown door device: {}", device.id());
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
    bridge_state: BridgeState,
) -> Result<()> {
    // Set bridge info metric
    Metrics::set_bridge_info(env!("CARGO_PKG_VERSION"));

    // Update connection status
    bridge_state.set_connection_status(ConnectionStatus::Connecting);

    let (mqtt_user, mqtt_password) = get_secrets();
    let options = ComelitOptions::builder()
        .user(Some(user.into()))
        .password(Some(password.into()))
        .mqtt_user(mqtt_user)
        .mqtt_password(mqtt_password)
        .host(host.clone())
        .port(port)
        .build()
        .map_err(|e| ComelitClientError::Generic(e.to_string()))?;

    let updater = Arc::new(Updater::new(bridge_state.clone()));
    let client = ComelitClient::new(options, Some(updater.clone())).await?;

    // Set the hub host in state
    if let Some(ref h) = host {
        bridge_state.set_hub_host(h.clone());
    }

    if let Ok(ping_task) = client.login(State::Disconnected).await {
        info!("Login successful");
        bridge_state.set_connection_status(ConnectionStatus::Connected);
        Metrics::set_connected(true);

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
                    device_id: MacAddress::from([
                        rand::random::<u8>(),
                        rand::random::<u8>(),
                        rand::random::<u8>(),
                        rand::random::<u8>(),
                        rand::random::<u8>(),
                        rand::random::<u8>(),
                    ]),
                    category: AccessoryCategory::Bridge,
                    ..Default::default()
                };
                storage.save_config(&config).await?;
                config
            }
        };

        let pin = config.pin.clone().to_string();
        let url = config.setup_url();

        // Update bridge state with pairing info
        bridge_state.set_pairing_pin(pin.clone());
        bridge_state.set_pairing_url(url.clone());

        let server = IpServer::new(config, storage).await?;
        info!("IP server created, adding bridge accessory...");
        server.add_accessory(bridge).await?;

        info!("Fetching device index...");
        let index = client
            .fetch_index(1)
            .await
            .context("Failed to fetch index")?;

        info!("Fetching external device index...");
        let external_index = client
            .fetch_external_devices()
            .await
            .context("Failed to fetch external devices")?;

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

                        // Register device in bridge state
                        bridge_state.register_device(DeviceInfo {
                            id: accessory.get_comelit_id().to_string(),
                            name: light
                                .description
                                .clone()
                                .unwrap_or_else(|| light.id.clone()),
                            device_type: DeviceType::Light,
                            status: match light.status {
                                Some(DeviceStatus::On) | Some(DeviceStatus::Running) => {
                                    "on".to_string()
                                }
                                _ => "off".to_string(),
                            },
                            last_update: None,
                        });

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

                        // Register device in bridge state
                        bridge_state.register_device(DeviceInfo {
                            id: accessory.get_comelit_id().to_string(),
                            name: window_covering
                                .description
                                .clone()
                                .unwrap_or_else(|| window_covering.id.clone()),
                            device_type: DeviceType::WindowCovering,
                            status: match &window_covering.status {
                                Some(s) => format!("{:?}", s),
                                None => "unknown".to_string(),
                            },
                            last_update: None,
                        });

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
                match ComelitThermostatAccessory::new(i, &thermostat, client.clone(), &server).await
                {
                    Ok(accessory) => {
                        info!("Thermostat {} added to the hub", accessory.get_comelit_id());

                        // Register device in bridge state
                        bridge_state.register_device(DeviceInfo {
                            id: accessory.get_comelit_id().to_string(),
                            name: thermostat
                                .description
                                .clone()
                                .unwrap_or_else(|| thermostat.id.clone()),
                            device_type: DeviceType::Thermostat,
                            status: format!(
                                "{}°C",
                                thermostat.temperature.as_deref().unwrap_or("--")
                            ),
                            last_update: None,
                        });

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
                        opening_closing_time: Duration::from_secs(
                            settings.door.opening_closing_time,
                        ),
                        opened_time: Duration::from_secs(settings.door.opened_time),
                        mount_as: crate::accessories::DoorType::Door,
                    },
                )
                .await
                {
                    Ok(accessory) => {
                        info!("Door {} added to the hub", accessory.get_comelit_id());
                        client.subscribe(&door.id).await?;

                        // Register device in bridge state
                        bridge_state.register_device(DeviceInfo {
                            id: accessory.get_comelit_id().to_string(),
                            name: door.description.clone().unwrap_or_else(|| door.id.clone()),
                            device_type: DeviceType::Door,
                            status: "closed".to_string(),
                            last_update: None,
                        });

                        updater
                            .doors
                            .insert(accessory.get_comelit_id().to_string(), accessory);
                    }
                    Err(err) => error!("Failed to add door device: {}", err),
                };
            }
        }

        for bell in bells {
            if settings.mount_doorbells.unwrap_or_default() {
                i += 1;
                info!("Adding doorbell device: {} with id {i}", bell.id);
                let data = client.info::<DoorbellDeviceData>(&bell.id, 1).await?;
                match ComelitDoorbellAccessory::new(i, data.first().unwrap(), &server).await {
                    Ok(accessory) => {
                        info!("Doorbell {} added to the hub", accessory.get_comelit_id());
                        client.subscribe(&bell.id).await?;

                        // Register device in bridge state
                        bridge_state.register_device(DeviceInfo {
                            id: accessory.get_comelit_id().to_string(),
                            name: bell.description.clone().unwrap_or_else(|| bell.id.clone()),
                            device_type: DeviceType::Doorbell,
                            status: "idle".to_string(),
                            last_update: None,
                        });

                        updater
                            .doorbells
                            .insert(accessory.get_comelit_id().to_string(), accessory);
                    }
                    Err(err) => error!("Failed to add doorbell device: {}", err),
                };
            }
        }

        // Update device count metrics
        Metrics::set_device_count("light", updater.lights.len());
        Metrics::set_device_count("thermostat", updater.thermostats.len());
        Metrics::set_device_count("window_covering", updater.window_coverings.len());
        Metrics::set_device_count("door", updater.doors.len());
        Metrics::set_device_count("doorbell", updater.doorbells.len());

        info!("Starting HAP bridge server...");
        let handle = server.run_handle();

        // Generate and display QR code
        let code = QrCode::new(url.as_bytes())?;
        let code_string = code
            .render::<char>()
            .quiet_zone(false)
            .module_dimensions(2, 1)
            .build();
        info!("QR code: \n{}", code_string);
        info!("Pair your Comelit Bridge using pin code {pin}");

        info!("Subscribing to root device updates...");
        client.subscribe(ROOT_ID).await?;

        // Clone bridge_state for the ping monitoring task
        let ping_state = bridge_state.clone();

        // Wrap ping_task to record metrics
        let monitored_ping_task = async move {
            // The ping_task from the client
            let _ = ping_task.await;
            // When it completes, record failure
            ping_state.record_ping(false);
            Metrics::record_ping(false);
        };

        // Spawn a task to periodically record successful pings while connected
        let ping_monitor_state = bridge_state.clone();
        let _ping_monitor = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                if ping_monitor_state.connection_status() == ConnectionStatus::Connected {
                    ping_monitor_state.record_ping(true);
                    Metrics::record_ping(true);
                }
            }
        });

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
            _ = monitored_ping_task => {
                info!("Ping task exited, gracefully shutting down");
                bridge_state.set_connection_status(ConnectionStatus::Disconnected);
                Metrics::set_connected(false);
                client
                    .disconnect()
                    .await
                    .context("Failed to disconnect client")
            }
            _ = handle => {
                bridge_state.set_connection_status(ConnectionStatus::Disconnected);
                Metrics::set_connected(false);
                client
                    .disconnect()
                    .await
                    .context("Failed to disconnect client")
            }
            _ = ctrl_c => {
                info!("signal received, starting graceful shutdown");
                bridge_state.set_connection_status(ConnectionStatus::Disconnected);
                Metrics::set_connected(false);
                client
                    .disconnect()
                    .await
                    .context("Failed to disconnect client")
            },
            _ = terminate => {
                info!("signal received, starting graceful shutdown");
                bridge_state.set_connection_status(ConnectionStatus::Disconnected);
                Metrics::set_connected(false);
                client
                    .disconnect()
                    .await
                    .context("Failed to disconnect client")
            },
        }
    } else {
        bridge_state.set_connection_status(ConnectionStatus::Error);
        bridge_state.set_error(Some("Login failed".to_string()));
        Metrics::set_connected(false);
        Err(ComelitClientError::Login("Login failed".to_string()).into())
    }
}
