use std::sync::Arc;

use anyhow::{Context, Result};

use futures::FutureExt;
use hap::characteristic::HapCharacteristic;
use hap::pointer::Accessory;
use hap::server::Server;
use hap::{
    HapType,
    accessory::{AccessoryInformation, HapAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    server::IpServer,
    service::{
        HapService, accessory_information::AccessoryInformationService,
        humidifier_dehumidifier::HumidifierDehumidifierService,
        thermostat::ThermostatService,
    },
};
use serde::{
    Serialize,
    ser::{SerializeStruct, Serializer},
};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::accessories::{
    ComelitAccessory,
    state::thermostat::{TargetHeatingCoolingState, ThermostatState},
};
use comelit_client_rs::{
    ClimaMode, ClimaOnOff, ComelitClient, ObjectSubtype, ThermoSeason, ThermostatDeviceData,
};

#[derive(Debug)]
struct ComelitThermostat {
    id: u64,
    /// Accessory Information service.
    pub accessory_information: AccessoryInformationService,
    /// Thermostat service.
    pub thermostat: ThermostatService,
    /// Optional Humidifier-Dehumidifier service (only for ClimaThermostatDehumidifier sub-type).
    pub humidifier_dehumidifier: Option<HumidifierDehumidifierService>,
}

impl HapAccessory for ComelitThermostat {
    fn get_id(&self) -> u64 {
        self.id
    }

    fn set_id(&mut self, id: u64) {
        self.id = id;
    }

    fn get_service(&self, hap_type: HapType) -> Option<&dyn HapService> {
        self.get_services()
            .into_iter()
            .find(|&service| service.get_type() == hap_type)
    }

    fn get_mut_service(&mut self, hap_type: HapType) -> Option<&mut dyn HapService> {
        self.get_mut_services()
            .into_iter()
            .find(|service| service.get_type() == hap_type)
    }

    fn get_services(&self) -> Vec<&dyn HapService> {
        let mut services: Vec<&dyn HapService> =
            vec![&self.accessory_information, &self.thermostat];
        if let Some(ref hd) = self.humidifier_dehumidifier {
            services.push(hd);
        }
        services
    }

    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        let mut services: Vec<&mut dyn HapService> =
            vec![&mut self.accessory_information, &mut self.thermostat];
        if let Some(ref mut hd) = self.humidifier_dehumidifier {
            services.push(hd);
        }
        services
    }
}

impl Serialize for ComelitThermostat {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("HapAccessory", 2)?;
        state.serialize_field("aid", &self.get_id())?;
        state.serialize_field("services", &self.get_services())?;
        state.end()
    }
}

impl ComelitThermostat {
    pub async fn new(
        id: u64,
        name: &str,
        device_id: &str,
        has_dehumidifier: bool,
    ) -> Result<Self> {
        let accessory_information = AccessoryInformation {
            manufacturer: "Comelit".to_string(),
            serial_number: device_id.to_string(),
            name: name.to_string(),
            ..Default::default()
        }
        .to_service(1, id)?;

        let thermo_id = accessory_information.get_characteristics().len() as u64;
        debug!("Thermostat ID: {}", thermo_id);
        let mut thermostat_sensor = ThermostatService::new(1 + thermo_id + 1, id);
        thermostat_sensor.cooling_threshold_temperature = None;
        thermostat_sensor.set_primary(true);

        let humidifier_dehumidifier = if has_dehumidifier {
            // Humidity characteristics move to the dehumidifier service
            thermostat_sensor.current_relative_humidity = None;
            thermostat_sensor.target_relative_humidity = None;

            // ThermostatService occupies 10 characteristic slots (5 required + 5 optional)
            let hd_start_id = 1 + thermo_id + 1 + 10 + 1;
            let mut hd = HumidifierDehumidifierService::new(hd_start_id, id);
            hd.lock_physical_controls = None;
            hd.name = None;
            hd.relative_humidity_humidifier_threshold = None;
            hd.rotation_speed = None;
            hd.swing_mode = None;
            hd.current_water_level = None;
            Some(hd)
        } else {
            None
        };

        Ok(Self {
            id,
            accessory_information,
            thermostat: thermostat_sensor,
            humidifier_dehumidifier,
        })
    }
}

pub(crate) struct ComelitThermostatAccessory {
    id: String,
    pub name: String,
    state: Arc<Mutex<ThermostatState>>,
    accessory: Accessory,
}

impl ComelitAccessory<ThermostatDeviceData> for ComelitThermostatAccessory {
    fn get_comelit_id(&self) -> &str {
        self.id.as_str()
    }

    async fn update(&mut self, thermostat_data: &ThermostatDeviceData) -> Result<()> {
        // Compute new state and update the shared Arc — release the lock before touching the
        // accessory to avoid a deadlock: the HAP framework holds `accessory` while calling
        // on_read callbacks, which in turn try to acquire `state`.
        let new_state = ThermostatState::from(thermostat_data);
        {
            let mut state = self.state.lock().await;
            *state = new_state.clone();
        }

        let mut accessory = self.accessory.lock().await;

        let thermostat_service = accessory
            .get_mut_service(HapType::Thermostat)
            .context("Thermostat service not found")?;

        if let Some(characteristic) =
            thermostat_service.get_mut_characteristic(HapType::CurrentTemperature)
        {
            characteristic
                .update_value(Value::from(new_state.temperature))
                .await?;
        }

        if let Some(characteristic) =
            thermostat_service.get_mut_characteristic(HapType::TargetTemperature)
        {
            characteristic
                .update_value(Value::from(new_state.target_temperature))
                .await?;
        }

        if let Some(characteristic) =
            thermostat_service.get_mut_characteristic(HapType::CurrentHeatingCoolingState)
        {
            characteristic
                .update_value(Value::from(new_state.heating_cooling_state as u8))
                .await?;
        }

        if let Some(characteristic) =
            thermostat_service.get_mut_characteristic(HapType::TargetHeatingCoolingState)
        {
            characteristic
                .update_value(Value::from(new_state.target_heating_cooling_state as u8))
                .await?;
        }

        // Humidity on the thermostat service (only when no dehumidifier service)
        if let Some(characteristic) =
            thermostat_service.get_mut_characteristic(HapType::CurrentRelativeHumidity)
        {
            characteristic
                .update_value(Value::from(new_state.humidity))
                .await?;
        }

        if let Some(characteristic) =
            thermostat_service.get_mut_characteristic(HapType::TargetRelativeHumidity)
        {
            characteristic
                .update_value(Value::from(new_state.target_humidity))
                .await?;
        }

        // Update dehumidifier service if present
        if let Some(hd_service) =
            accessory.get_mut_service(HapType::HumidifierDehumidifier)
        {
            if let Some(characteristic) = hd_service.get_mut_characteristic(HapType::Active) {
                characteristic
                    .update_value(Value::from(new_state.dehumidifier_active as u8))
                    .await?;
            }

            if let Some(characteristic) = hd_service
                .get_mut_characteristic(HapType::CurrentHumidifierDehumidifierState)
            {
                characteristic
                    .update_value(Value::from(new_state.dehumidifier_current_state))
                    .await?;
            }

            if let Some(characteristic) =
                hd_service.get_mut_characteristic(HapType::CurrentRelativeHumidity)
            {
                characteristic
                    .update_value(Value::from(new_state.humidity))
                    .await?;
            }

            if let Some(characteristic) = hd_service
                .get_mut_characteristic(HapType::RelativeHumidityDehumidifierThreshold)
            {
                characteristic
                    .update_value(Value::from(new_state.target_humidity))
                    .await?;
            }
        }

        Ok(())
    }
}

impl ComelitThermostatAccessory {
    pub async fn new(
        id: u64,
        data: &ThermostatDeviceData,
        client: ComelitClient,
        server: &IpServer,
    ) -> Result<Self> {
        let name = data.description.clone().unwrap_or(data.id.clone());
        let comelit_id = data.id.clone();
        let has_dehumidifier = data.sub_type == ObjectSubtype::ClimaThermostatDehumidifier;
        let mut accessory =
            ComelitThermostat::new(id, name.as_str(), comelit_id.as_str(), has_dehumidifier)
                .await?;
        let state = ThermostatState::from(data);
        let arc_state = Arc::new(Mutex::new(ThermostatState::from(data)));

        info!("Creating thermostat accessory with state: {:?}", state);

        accessory
            .thermostat
            .current_temperature
            .set_value(Value::from(state.temperature))
            .await?;

        let arc_state_clone = Arc::clone(&arc_state);
        accessory
            .thermostat
            .current_temperature
            .on_read_async(Some(move || {
                let arc_state_clone = arc_state_clone.clone();
                async move {
                    let state = arc_state_clone.lock().await;
                    Ok(Some(state.temperature))
                }
                .boxed()
            }));

        accessory
            .thermostat
            .target_temperature
            .set_value(Value::from(state.target_temperature))
            .await?;

        let arc_state_clone = Arc::clone(&arc_state);
        accessory
            .thermostat
            .target_temperature
            .on_read_async(Some(move || {
                let arc_state_clone = arc_state_clone.clone();
                async move {
                    let state = arc_state_clone.lock().await;
                    Ok(Some(state.target_temperature))
                }
                .boxed()
            }));

        accessory
            .thermostat
            .current_heating_cooling_state
            .set_value(Value::from(state.heating_cooling_state as u8))
            .await?;

        let arc_state_clone = Arc::clone(&arc_state);
        accessory
            .thermostat
            .current_heating_cooling_state
            .on_read_async(Some(move || {
                let arc_state_clone = arc_state_clone.clone();
                async move {
                    let state = arc_state_clone.lock().await;
                    Ok(Some(state.heating_cooling_state as u8))
                }
                .boxed()
            }));

        accessory
            .thermostat
            .target_heating_cooling_state
            .set_value(Value::from(state.target_heating_cooling_state as u8))
            .await?;

        let arc_state_clone = Arc::clone(&arc_state);
        accessory
            .thermostat
            .target_heating_cooling_state
            .on_read_async(Some(move || {
                let arc_state_clone = arc_state_clone.clone();
                async move {
                    let state = arc_state_clone.lock().await;
                    Ok(Some(state.target_heating_cooling_state as u8))
                }
                .boxed()
            }));

        if let Some(ref mut char) = accessory.thermostat.current_relative_humidity {
            char.set_value(Value::from(state.humidity)).await?;
            let arc_state_clone = Arc::clone(&arc_state);
            char.on_read_async(Some(move || {
                let arc_state_clone = arc_state_clone.clone();
                async move {
                    let state = arc_state_clone.lock().await;
                    Ok(Some(state.humidity))
                }
                .boxed()
            }));
        }

        if let Some(ref mut char) = accessory.thermostat.target_relative_humidity {
            char.set_value(Value::from(state.target_humidity)).await?;
            let arc_state_clone = Arc::clone(&arc_state);
            char.on_read_async(Some(move || {
                let arc_state_clone = arc_state_clone.clone();
                async move {
                    let state = arc_state_clone.lock().await;
                    Ok(Some(state.target_humidity))
                }
                .boxed()
            }));
        }

        let client_ = client.clone();
        let comelit_id_ = comelit_id.clone();
        let state_ = Arc::clone(&arc_state);
        accessory
            .thermostat
            .target_temperature
            .on_update_async(Some(move |_, new| {
                let client = client_.clone();
                let comelit_id = comelit_id_.clone();
                let state = state_.clone();
                async move {
                    let prev = state.lock().await.target_temperature;
                    if prev != new {
                        debug!("Target temperature updated from {} to {}", prev, new);
                        let temperature = (new * 10.0) as i32;
                        client
                            .set_thermostat_temperature(&comelit_id, temperature)
                            .await?;
                    }
                    Ok(())
                }
                .boxed()
            }));

        if let Some(ref mut char) = accessory.thermostat.target_relative_humidity {
            let client_ = client.clone();
            let comelit_id_ = comelit_id.clone();
            let state_ = Arc::clone(&arc_state);
            char.on_update_async(Some(move |_prev, new| {
                let client = client_.clone();
                let comelit_id = comelit_id_.clone();
                let state = state_.clone();
                async move {
                    let prev = state.lock().await.target_humidity;
                    if prev != new {
                        debug!("Target humidity updated from {} to {}", prev, new);
                        let humidity = (new * 10.0) as i32;
                        client.set_humidity(&comelit_id, humidity).await?;
                    }
                    Ok(())
                }
                .boxed()
            }));
        }

        let client_ = client.clone();
        let comelit_id_ = comelit_id.clone();
        let state_ = Arc::clone(&arc_state);
        accessory
            .thermostat
            .target_heating_cooling_state
            .on_update_async(Some(move |_prev: u8, new: u8| {
                let client = client_.clone();
                let comelit_id = comelit_id_.clone();
                let state = state_.clone();
                async move {
                    let prev = state.lock().await.target_heating_cooling_state as u8;
                    debug!(
                        "Target heating cooling state updated from {} to {}",
                        prev, new
                    );

                    client
                        .toggle_thermostat_status(
                            comelit_id.as_str(),
                            if TargetHeatingCoolingState::Off as u8 == new {
                                ClimaOnOff::OffThermo
                            } else {
                                ClimaOnOff::OnThermo
                            },
                        )
                        .await?;

                    if prev == TargetHeatingCoolingState::Auto as u8
                        && new != TargetHeatingCoolingState::Off as u8
                    {
                        // if in AUTO mode, switch to MANUAL here
                        client
                            .set_thermostat_mode(comelit_id.as_str(), ClimaMode::Manual)
                            .await?;
                    }

                    match TargetHeatingCoolingState::from(new) {
                        TargetHeatingCoolingState::Auto => {
                            client
                                .set_thermostat_mode(comelit_id.as_str(), ClimaMode::Auto)
                                .await?;
                        }
                        TargetHeatingCoolingState::Cool => {
                            client
                                .set_thermostat_season(
                                    comelit_id.as_str(),
                                    ThermoSeason::Summer,
                                )
                                .await?;
                        }
                        TargetHeatingCoolingState::Heat => {
                            client
                                .set_thermostat_season(
                                    comelit_id.as_str(),
                                    ThermoSeason::Winter,
                                )
                                .await?;
                        }
                        TargetHeatingCoolingState::Off => {}
                    }
                    Ok(())
                }
                .boxed()
            }));

        // Wire up dehumidifier service callbacks if present
        if let Some(ref mut hd) = accessory.humidifier_dehumidifier {
            // Fixed to Dehumidifier mode (2)
            hd.target_humidifier_dehumidifier_state
                .set_value(Value::from(2u8))
                .await?;

            hd.active
                .set_value(Value::from(state.dehumidifier_active as u8))
                .await?;

            let arc_state_clone = Arc::clone(&arc_state);
            hd.active.on_read_async(Some(move || {
                let arc_state_clone = arc_state_clone.clone();
                async move {
                    let state = arc_state_clone.lock().await;
                    Ok(Some(state.dehumidifier_active as u8))
                }
                .boxed()
            }));

            hd.current_humidifier_dehumidifier_state
                .set_value(Value::from(state.dehumidifier_current_state))
                .await?;

            let arc_state_clone = Arc::clone(&arc_state);
            hd.current_humidifier_dehumidifier_state
                .on_read_async(Some(move || {
                    let arc_state_clone = arc_state_clone.clone();
                    async move {
                        let state = arc_state_clone.lock().await;
                        Ok(Some(state.dehumidifier_current_state))
                    }
                    .boxed()
                }));

            hd.current_relative_humidity
                .set_value(Value::from(state.humidity))
                .await?;

            let arc_state_clone = Arc::clone(&arc_state);
            hd.current_relative_humidity.on_read_async(Some(move || {
                let arc_state_clone = arc_state_clone.clone();
                async move {
                    let state = arc_state_clone.lock().await;
                    Ok(Some(state.humidity))
                }
                .boxed()
            }));

            if let Some(ref mut threshold) = hd.relative_humidity_dehumidifier_threshold {
                threshold
                    .set_value(Value::from(state.target_humidity))
                    .await?;

                let arc_state_clone = Arc::clone(&arc_state);
                threshold.on_read_async(Some(move || {
                    let arc_state_clone = arc_state_clone.clone();
                    async move {
                        let state = arc_state_clone.lock().await;
                        Ok(Some(state.target_humidity))
                    }
                    .boxed()
                }));

                let client_ = client.clone();
                let comelit_id_ = comelit_id.clone();
                let state_ = Arc::clone(&arc_state);
                threshold.on_update_async(Some(move |_prev, new| {
                    let client = client_.clone();
                    let comelit_id = comelit_id_.clone();
                    let state = state_.clone();
                    async move {
                        let prev = state.lock().await.target_humidity;
                        if prev != new {
                            debug!("Dehumidifier threshold updated from {} to {}", prev, new);
                            let humidity = (new * 10.0) as i32;
                            client.set_humidity(&comelit_id, humidity).await?;
                        }
                        Ok(())
                    }
                    .boxed()
                }));
            }

            let client_ = client.clone();
            let comelit_id_ = comelit_id.clone();
            let state_ = Arc::clone(&arc_state);
            hd.active.on_update_async(Some(move |_prev: u8, new: u8| {
                let client = client_.clone();
                let comelit_id = comelit_id_.clone();
                let state = state_.clone();
                async move {
                    let prev = state.lock().await.dehumidifier_active as u8;
                    if prev != new {
                        debug!("Dehumidifier active updated from {} to {}", prev, new);
                        client
                            .toggle_thermostat_status(
                                comelit_id.as_str(),
                                if new == 1 {
                                    ClimaOnOff::OnHumi
                                } else {
                                    ClimaOnOff::OffHumi
                                },
                            )
                            .await?;
                    }
                    Ok(())
                }
                .boxed()
            }));
        }

        let accessory = server.add_accessory(accessory).await?;
        Ok(Self {
            id: data.id.clone(),
            name,
            state: arc_state,
            accessory,
        })
    }
}
