use std::sync::Arc;

use anyhow::Result;
use comelit_client_rs::{DeviceStatus, DoorbellDeviceData};
use hap::{
    HapType,
    accessory::{AccessoryInformation, HapAccessory},
    characteristic::HapCharacteristic,
    pointer::Accessory,
    server::{IpServer, Server},
    service::{HapService, accessory_information::AccessoryInformationService, doorbell::DoorbellService},
};
use serde::ser::{Serialize, SerializeStruct, Serializer};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::info;

use crate::accessories::ComelitAccessory;

/// Doorbell accessory — wraps a single DoorbellService with ProgrammableSwitchEvent.
#[derive(Debug, Default)]
pub struct DoorbellAccessory {
    id: u64,
    pub accessory_information: AccessoryInformationService,
    pub doorbell: DoorbellService,
}

impl DoorbellAccessory {
    pub fn new(id: u64, information: AccessoryInformation) -> Result<Self> {
        let accessory_information = information.to_service(1, id)?;
        let info_len = accessory_information.get_characteristics().len() as u64;
        let mut doorbell = DoorbellService::new(1 + info_len + 1, id);
        doorbell.set_primary(true);

        Ok(Self {
            id,
            accessory_information,
            doorbell,
        })
    }
}

impl HapAccessory for DoorbellAccessory {
    fn get_id(&self) -> u64 {
        self.id
    }

    fn set_id(&mut self, id: u64) {
        self.id = id;
    }

    fn get_service(&self, hap_type: HapType) -> Option<&dyn HapService> {
        self.get_services()
            .into_iter()
            .find(|&s| s.get_type() == hap_type)
            .map(|v| v as _)
    }

    fn get_mut_service(&mut self, hap_type: HapType) -> Option<&mut dyn HapService> {
        self.get_mut_services()
            .into_iter()
            .find(|s| s.get_type() == hap_type)
            .map(|v| v as _)
    }

    fn get_services(&self) -> Vec<&dyn HapService> {
        vec![&self.accessory_information, &self.doorbell]
    }

    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        vec![&mut self.accessory_information, &mut self.doorbell]
    }
}

impl Serialize for DoorbellAccessory {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("HapAccessory", 2)?;
        state.serialize_field("aid", &self.get_id())?;
        state.serialize_field("services", &self.get_services())?;
        state.end()
    }
}

struct State {
    accessory: Option<Accessory>,
}

pub(crate) struct ComelitDoorbellAccessory {
    pub(crate) id: String,
    #[allow(dead_code)]
    pub(crate) accessory: Accessory,
    state: Arc<Mutex<State>>,
}

impl ComelitDoorbellAccessory {
    pub(crate) async fn new(
        id: u64,
        door_data: &DoorbellDeviceData,
        server: &IpServer,
    ) -> Result<Self> {
        let device_id = door_data.id.clone();
        let name = door_data.description.clone().unwrap_or(device_id.clone());
        let mut doorbell_accessory = DoorbellAccessory::new(
            id,
            AccessoryInformation {
                name: name.clone(),
                model: "VIP Doorbell".to_string(),
                manufacturer: "Comelit".to_string(),
                serial_number: device_id.clone(),
                ..Default::default()
            },
        )?;

        // Strip optional characteristics we don't use
        doorbell_accessory.doorbell.brightness = None;
        doorbell_accessory.doorbell.mute = None;
        doorbell_accessory.doorbell.operating_state_response = None;
        doorbell_accessory.doorbell.volume = None;

        // Restrict ProgrammableSwitchEvent to Single Press only (value 0).
        // hap-rs sets event_only=true on this characteristic so GET returns null,
        // which is what iOS uses to recognize the accessory as a proper Doorbell
        // rather than a generic Stateless Programmable Switch.
        let pse = &mut doorbell_accessory.doorbell.programmable_switch_event;
        pse.set_valid_values(Some(vec![Value::from(0u8)]))?;
        pse.set_min_value(None)?;
        pse.set_max_value(None)?;
        pse.set_step_value(None)?;

        let state = Arc::new(Mutex::new(State { accessory: None }));

        let accessory = server.add_accessory(doorbell_accessory).await?;
        state.lock().await.accessory = Some(accessory.clone());

        Ok(Self {
            id: device_id,
            accessory,
            state,
        })
    }
}

/// Send a Single Press event on ProgrammableSwitchEvent.
/// Value 0 = Single Press (HAP spec), which triggers the iOS doorbell sound.
async fn ring(id: &str, accessory: Accessory) -> Result<()> {
    info!("Doorbell {} ringing — sending Single Press event", id);
    let mut acc = accessory.lock().await;
    let service = acc.get_mut_service(HapType::Doorbell).unwrap();
    let programmable_switch = service
        .get_mut_characteristic(HapType::ProgrammableSwitchEvent)
        .unwrap();
    // 0 = Single Press per HAP spec (1 = Double Press, 2 = Long Press)
    programmable_switch.update_value(Value::from(0u8)).await?;
    Ok(())
}

impl ComelitAccessory<DoorbellDeviceData> for ComelitDoorbellAccessory {
    fn get_comelit_id(&self) -> &str {
        &self.id
    }

    async fn update(&mut self, data: &DoorbellDeviceData) -> Result<()> {
        // Only ring when Comelit reports the bell as pressed (status On)
        if data.status == Some(DeviceStatus::On) {
            if let Some(accessory) = self.state.lock().await.accessory.clone() {
                ring(&self.id, accessory).await?;
            }
        }
        Ok(())
    }
}
