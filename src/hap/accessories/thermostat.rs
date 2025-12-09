use std::sync::Arc;

use anyhow::Result;

use futures::FutureExt;
use hap::{
    HapType,
    accessory::{AccessoryInformation, HapAccessory},
    characteristic::AsyncCharacteristicCallbacks,
    server::{IpServer, Server},
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
use tracing::{debug, error};

use crate::{
    hap::accessories::{
        AccessoryPointer, ComelitAccessory,
        state::thermostat::{TargetHeatingCoolingState, ThermostatState},
    },
    protocol::{
        client::ComelitClient,
        out_data_messages::{ClimaMode, ClimaOnOff, ThermoSeason, ThermostatDeviceData},
    },
};

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
    pub async fn new(id: u64, name: &str) -> Result<Self> {
        let accessory_information = AccessoryInformation {
            manufacturer: "Comelit".into(),
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

async fn set_values(accessory: AccessoryPointer, data: &ThermostatDeviceData) -> Result<()> {
    let mut guard = accessory.lock().await;
    let thermostat_sensor = guard.get_mut_service(HapType::Thermostat).unwrap();
    let state = ThermostatState::from(data);

    thermostat_sensor
        .get_mut_characteristic(HapType::CurrentTemperature)
        .unwrap()
        .set_value(Value::from(state.temperature))
        .await?;

    thermostat_sensor
        .get_mut_characteristic(HapType::TargetTemperature)
        .unwrap()
        .set_value(Value::from(state.target_temperature))
        .await?;

    thermostat_sensor
        .get_mut_characteristic(HapType::CurrentHeatingCoolingState)
        .unwrap()
        .set_value(Value::from(state.heating_cooling_state as u8))
        .await?;

    thermostat_sensor
        .get_mut_characteristic(HapType::TargetHeatingCoolingState)
        .unwrap()
        .set_value(Value::from(state.target_heating_cooling_state as u8))
        .await?;

    thermostat_sensor
        .get_mut_characteristic(HapType::CurrentRelativeHumidity)
        .unwrap()
        .set_value(Value::from(state.humidity))
        .await?;

    thermostat_sensor
        .get_mut_characteristic(HapType::TargetRelativeHumidity)
        .unwrap()
        .set_value(Value::from(state.target_humidity))
        .await?;

    Ok(())
}

pub(crate) struct ComelitThermostatAccessory {
    thermostat_accessory: AccessoryPointer,
    id: String,
}

impl ComelitAccessory<ThermostatDeviceData> for ComelitThermostatAccessory {
    fn get_comelit_id(&self) -> &str {
        self.id.as_str()
    }

    async fn update(&mut self, thermostat_data: &ThermostatDeviceData) -> Result<()> {
        set_values(self.thermostat_accessory.clone(), thermostat_data)
            .await
            .unwrap_or_else(|e| error!("Error updating thermostat: {}", e));
        Ok(())
    }
}

impl ComelitThermostatAccessory {
    pub async fn new(
        id: u64,
        data: &ThermostatDeviceData,
        client: Arc<ComelitClient>,
        server: &IpServer,
    ) -> Result<Self> {
        let name = data
            .data
            .description
            .clone()
            .unwrap_or(data.data.id.clone());
        let comelit_id = data.data.id.clone();
        let mut accessory = ComelitThermostat::new(id, name.as_str()).await?;
        let state = ThermostatState::from(data);

        debug!("Creating thermostat accessory with state: {:?}", state);

        let client_ = client.clone();
        let comelit_id_ = comelit_id.clone();
        accessory
            .thermostat
            .target_temperature
            .on_update_async(Some(move |prev, new| {
                let client = client_.clone();
                let comelit_id = comelit_id_.clone();
                async move {
                    debug!("Target temperature updated from {} to {}", prev, new);
                    let temperature = (new * 10.0) as i32;
                    client
                        .set_thermostat_temperature(&comelit_id, temperature)
                        .await?;
                    Ok(())
                }
                .boxed()
            }));

        let client_ = client.clone();
        let comelit_id_ = comelit_id.clone();
        accessory
            .thermostat
            .target_relative_humidity
            .as_mut()
            .unwrap()
            .on_update_async(Some(move |prev, new| {
                let client = client_.clone();
                let comelit_id = comelit_id_.clone();
                async move {
                    debug!("Target humidity updated from {} to {}", prev, new);
                    let humidity = (new * 10.0) as i32;
                    client.set_humidity(&comelit_id, humidity).await?;
                    Ok(())
                }
                .boxed()
            }));

        let client = client.clone();
        let comelit_id = comelit_id.clone();
        accessory
            .thermostat
            .target_heating_cooling_state
            .on_update_async(Some(move |prev: u8, new: u8| {
                let client = client.clone();
                let comelit_id = comelit_id.clone();
                async move {
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
                                .set_thermostat_season(comelit_id.as_str(), ThermoSeason::Summer)
                                .await?;
                        }
                        TargetHeatingCoolingState::Heat => {
                            client
                                .set_thermostat_season(comelit_id.as_str(), ThermoSeason::Winter)
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
                    Ok(())
                }
                .boxed()
            }));
        let thermostat_accessory = server.add_accessory(accessory).await?;

        set_values(thermostat_accessory.clone(), data).await?;
        Ok(Self {
            thermostat_accessory,
            id: data.data.id.clone(),
        })
    }
}
