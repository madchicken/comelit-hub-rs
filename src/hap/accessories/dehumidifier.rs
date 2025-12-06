use std::sync::Arc;

use anyhow::Result;

use futures::FutureExt;
use hap::{
    HapType,
    accessory::{AccessoryInformation, HapAccessory},
    characteristic::{AsyncCharacteristicCallbacks, HapCharacteristic},
    server::{IpServer, Server},
    service::{
        HapService, accessory_information::AccessoryInformationService,
        humidifier_dehumidifier::HumidifierDehumidifierService, thermostat::ThermostatService,
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
struct ComelitDehumidifier {
    id: u64,
    /// Accessory Information service.
    pub accessory_information: AccessoryInformationService,
    /// Dehumidifier service.
    pub dehumidifier: HumidifierDehumidifierService,
}

impl HapAccessory for ComelitDehumidifier {
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
        vec![&self.accessory_information, &self.dehumidifier]
    }

    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        vec![&mut self.accessory_information, &mut self.dehumidifier]
    }
}

impl Serialize for ComelitDehumidifier {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("HapAccessory", 2)?;
        state.serialize_field("aid", &self.get_id())?;
        state.serialize_field("services", &self.get_services())?;
        state.end()
    }
}

impl ComelitDehumidifier {
    pub async fn new(id: u64, name: &str) -> Result<Self> {
        let accessory_information = AccessoryInformation {
            manufacturer: "Comelit".into(),
            name: name.to_string(),
            ..Default::default()
        }
        .to_service(1, id)?;

        let humi_id = accessory_information.get_characteristics().len() as u64;
        debug!("Dehumidifier ID: {}", humi_id);
        let mut dehumidifier_sensor = HumidifierDehumidifierService::new(1 + humi_id + 1, id);
        dehumidifier_sensor.lock_physical_controls = None;
        dehumidifier_sensor.relative_humidity_humidifier_threshold = None;
        dehumidifier_sensor.rotation_speed = None;
        dehumidifier_sensor.swing_mode = None;
        dehumidifier_sensor.current_water_level = None;

        Ok(Self {
            id,
            accessory_information,
            dehumidifier: dehumidifier_sensor,
        })
    }
}

async fn set_values(accessory: AccessoryPointer, data: &ThermostatDeviceData) -> Result<()> {
    let mut guard = accessory.lock().await;
    let sensor = guard
        .get_mut_service(HapType::HumidifierDehumidifier)
        .unwrap();
    let state = ThermostatState::from(data);

    sensor
        .get_mut_characteristic(HapType::Active)
        .unwrap()
        .set_value(Value::from(
            if state.target_heating_cooling_state == TargetHeatingCoolingState::Cool {
                1
            } else {
                0
            },
        ))
        .await?;

    Ok(())
}

pub(crate) struct ComelitDehumidifierAccessory {
    dehumidifier_accessory: AccessoryPointer,
    id: String,
}

impl ComelitAccessory<ThermostatDeviceData> for ComelitDehumidifierAccessory {
    fn get_comelit_id(&self) -> &str {
        self.id.as_str()
    }

    async fn update(&mut self, thermostat_data: &ThermostatDeviceData) -> Result<()> {
        set_values(self.dehumidifier_accessory.clone(), thermostat_data)
            .await
            .unwrap_or_else(|e| error!("Error updating dehumidifier: {}", e));
        Ok(())
    }
}

impl ComelitDehumidifierAccessory {
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
        let mut accessory = ComelitDehumidifier::new(id, name.as_str()).await?;
        let state = ThermostatState::from(data);

        debug!("Creating thermostat accessory with state: {:?}", state);

        accessory
            .dehumidifier
            .active
            .set_value(Value::from(
                if state.target_heating_cooling_state == TargetHeatingCoolingState::Cool {
                    1
                } else {
                    0
                },
            ))
            .await?;

        accessory
            .dehumidifier
            .current_relative_humidity
            .set_value(Value::from(state.humidity))
            .await?;

        let relative_humidity_dehumidifier_threshold_characteristic = accessory
            .dehumidifier
            .relative_humidity_dehumidifier_threshold
            .as_mut()
            .unwrap();
        relative_humidity_dehumidifier_threshold_characteristic
            .set_value(Value::from(state.target_humidity))
            .await?;

        relative_humidity_dehumidifier_threshold_characteristic.on_update_async(Some(
            move |prev, new| {
                let comelit_id = comelit_id.clone();
                async move {
                    debug!("Target dehumidifier threshold updated from {prev} to {new} for device {comelit_id}");
                    Ok(())
                }
                .boxed()
            },
        ));

        Ok(Self {
            dehumidifier_accessory: server.add_accessory(accessory).await?,
            id: data.data.id.clone(),
        })
    }
}
