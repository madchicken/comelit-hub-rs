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
use comelit_hub_rs::{ClimaMode, ClimaOnOff, ComelitClient, ThermoSeason, ThermostatDeviceData};

#[derive(Debug)]
struct ComelitThermostat {
    id: u64,
    /// Accessory Information service.
    pub accessory_information: AccessoryInformationService,
    /// Thermostat service.
    pub thermostat: ThermostatService,
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
        vec![&self.accessory_information, &self.thermostat]
    }

    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        vec![&mut self.accessory_information, &mut self.thermostat]
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
    pub async fn new(id: u64, name: &str, device_id: &str) -> Result<Self> {
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

        Ok(Self {
            id,
            accessory_information,
            thermostat: thermostat_sensor,
        })
    }
}

pub(crate) struct ComelitThermostatAccessory {
    id: String,
    state: Arc<Mutex<ThermostatState>>,
    accessory: Accessory,
    enable_live_update: bool,
}

impl ComelitAccessory<ThermostatDeviceData> for ComelitThermostatAccessory {
    fn get_comelit_id(&self) -> &str {
        self.id.as_str()
    }

    async fn update(&mut self, thermostat_data: &ThermostatDeviceData) -> Result<()> {
        let mut state = self.state.lock().await;
        let new_state = ThermostatState::from(thermostat_data);
        state.heating_cooling_state = new_state.heating_cooling_state;
        state.temperature = new_state.temperature;
        state.target_temperature = new_state.target_temperature;
        state.humidity = new_state.humidity;
        state.target_humidity = new_state.target_humidity;
        state.target_heating_cooling_state = new_state.target_heating_cooling_state;

        if self.enable_live_update {
            let mut accessory = self.accessory.lock().await;
            let service = accessory
                .get_mut_service(HapType::Thermostat)
                .context("Thermostat service not found")?;

            if let Some(characteristic) =
                service.get_mut_characteristic(HapType::CurrentTemperature)
            {
                characteristic
                    .update_value(Value::from(state.temperature))
                    .await?;
            }

            if let Some(characteristic) = service.get_mut_characteristic(HapType::TargetTemperature)
            {
                characteristic
                    .update_value(Value::from(state.target_temperature))
                    .await?;
            }

            if let Some(characteristic) =
                service.get_mut_characteristic(HapType::CurrentHeatingCoolingState)
            {
                characteristic
                    .update_value(Value::from(state.heating_cooling_state as u8))
                    .await?;
            }

            if let Some(characteristic) =
                service.get_mut_characteristic(HapType::TargetHeatingCoolingState)
            {
                characteristic
                    .update_value(Value::from(state.target_heating_cooling_state as u8))
                    .await?;
            }

            if let Some(characteristic) =
                service.get_mut_characteristic(HapType::CurrentRelativeHumidity)
            {
                characteristic
                    .update_value(Value::from(state.humidity))
                    .await?;
            }

            if let Some(characteristic) =
                service.get_mut_characteristic(HapType::TargetRelativeHumidity)
            {
                characteristic
                    .update_value(Value::from(state.target_humidity))
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
        enable_thermostat_update: bool,
    ) -> Result<Self> {
        let name = data.description.clone().unwrap_or(data.id.clone());
        let comelit_id = data.id.clone();
        let mut accessory = ComelitThermostat::new(id, name.as_str(), comelit_id.as_str()).await?;
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

        accessory
            .thermostat
            .current_relative_humidity
            .as_mut()
            .unwrap()
            .set_value(Value::from(state.humidity))
            .await?;

        let arc_state_clone = Arc::clone(&arc_state);
        accessory
            .thermostat
            .current_relative_humidity
            .as_mut()
            .unwrap()
            .on_read_async(Some(move || {
                let arc_state_clone = arc_state_clone.clone();
                async move {
                    let state = arc_state_clone.lock().await;
                    Ok(Some(state.humidity))
                }
                .boxed()
            }));

        accessory
            .thermostat
            .target_relative_humidity
            .as_mut()
            .unwrap()
            .set_value(Value::from(state.target_humidity))
            .await?;

        let arc_state_clone = Arc::clone(&arc_state);
        accessory
            .thermostat
            .target_relative_humidity
            .as_mut()
            .unwrap()
            .on_read_async(Some(move || {
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

        let client_ = client.clone();
        let comelit_id_ = comelit_id.clone();
        let state_ = Arc::clone(&arc_state);
        accessory
            .thermostat
            .target_relative_humidity
            .as_mut()
            .unwrap()
            .on_update_async(Some(move |_prev, new| {
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
                    if prev != new {
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

                        let state = TargetHeatingCoolingState::from(new);
                        match state {
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
                            TargetHeatingCoolingState::Off => {
                                client
                                    .toggle_thermostat_status(
                                        comelit_id.as_str(),
                                        ClimaOnOff::OffThermo,
                                    )
                                    .await?;
                            }
                        }
                    }
                    Ok(())
                }
                .boxed()
            }));

        let accessory = server.add_accessory(accessory).await?;
        Ok(Self {
            id: data.id.clone(),
            state: arc_state,
            accessory,
            enable_live_update: enable_thermostat_update,
        })
    }
}
