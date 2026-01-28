use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use comelit_client_rs::{ComelitClient, DoorDeviceData};
use futures::FutureExt;
use hap::{
    accessory::{AccessoryInformation, door::DoorAccessory},
    characteristic::{AsyncCharacteristicCallbacks, CharacteristicCallbacks, HapCharacteristic},
    server::{IpServer, Server},
};
use serde_json::Value;
use tracing::info;

use crate::accessories::{
    ComelitAccessory,
    state::door::{DoorPositionState, DoorState, FULLY_CLOSED, FULLY_OPENED},
};

#[allow(dead_code)]
pub enum DoorType {
    Door,
    GarageDoor,
    Lock,
}

pub struct DoorConfig {
    pub opening_closing_time: Duration,
    pub opened_time: Duration,
    pub mount_as: DoorType,
}

pub(crate) struct ComelitDoorAccessory {
    id: String,
    state: Arc<Mutex<DoorState>>,
}

impl ComelitDoorAccessory {
    pub(crate) async fn new(
        id: u64,
        door_data: &DoorDeviceData,
        client: ComelitClient,
        server: &IpServer,
        config: DoorConfig,
    ) -> Result<Self> {
        let device_id = door_data.id.clone();
        let name = door_data.description.clone().unwrap_or(device_id.clone());

        if !matches!(config.mount_as, DoorType::Door) {
            return Err(anyhow::Error::msg("Invalid door mount type".to_string()));
        }

        let mut door_accessory = DoorAccessory::new(
            id,
            AccessoryInformation {
                name,
                manufacturer: "Comelit".to_string(),
                serial_number: device_id.clone(),
                ..Default::default()
            },
        )?;
        door_accessory.door.hold_position = None;
        door_accessory.door.obstruction_detected = None;
        door_accessory
            .door
            .target_position
            .set_step_value(Some(Value::from(100)))?;

        let state = DoorState::from(door_data);
        info!(
            "Setting initial door {} position to {}",
            device_id, state.current_position
        );
        door_accessory
            .door
            .current_position
            .set_value(Value::from(state.current_position))
            .await
            .context("Cannot set current position")?;
        door_accessory
            .door
            .position_state
            .set_value(Value::from(state.position_state))
            .await
            .context("Cannot set position state")?;
        door_accessory
            .door
            .target_position
            .set_value(Value::from(state.target_position))
            .await
            .context("Cannot set current target position")?;

        let state = Arc::new(Mutex::new(state));

        Self::setup_read_characteristics(&device_id, &mut door_accessory, state.clone());
        Self::setup_update_target_position(
            &device_id,
            client.clone(),
            &mut door_accessory,
            config.opening_closing_time,
            config.opened_time,
            state.clone(),
        );

        server.add_accessory(door_accessory).await?;
        Ok(Self {
            id: device_id,
            state,
        })
    }

    fn setup_read_characteristics(
        id: &str,
        accessory: &mut DoorAccessory,
        state: Arc<Mutex<DoorState>>,
    ) {
        let id_ = id.to_string();
        let state_ = state.clone();
        accessory.door.position_state.on_read(Some(move || {
            info!("Door POSITION STATE read {}", id_);
            let state = state_.lock().unwrap();
            Ok(Some(state.position_state))
        }));

        let id_ = id.to_string();
        let state_ = state.clone();
        accessory.door.current_position.on_read(Some(move || {
            info!("Door CURRENT POSITION read {}", id_);
            let state = state_.lock().unwrap();
            Ok(Some(state.current_position))
        }));

        let id_ = id.to_string();
        let state_ = state.clone();
        accessory.door.target_position.on_read(Some(move || {
            info!("Door TARGET POSITION read {}", id_);
            let state = state_.lock().unwrap();
            Ok(Some(state.target_position))
        }));
    }

    fn setup_update_target_position(
        id: &str,
        client: ComelitClient,
        accessory: &mut DoorAccessory,
        opening_closing_time: Duration, // the time the door takes to open/close
        opened_time: Duration,          // the time the door remains open
        state: Arc<Mutex<DoorState>>,
    ) {
        let id = id.to_string();
        let state = state.clone();
        let client = client.clone();
        accessory
            .door
            .target_position
            .on_update_async(Some(move |_, new_pos| {
                // For blinds/shades/awnings, a value of 0 indicates a position that permits the least light and a value
                // of 100 indicates a position that allows most light.
                // This means:
                // 0   -> FULLY CLOSED
                // 100 -> FULLY OPENED

                let state = state.clone();
                let client = client.clone();
                let id = id.to_string();
                async move {
                    if new_pos != FULLY_OPENED {
                        info!(
                            "Target position equals current position for door {}, no action taken",
                            id
                        );
                        return Ok(());
                    }
                    info!("Door {id} started opening");
                    client.toggle_device_status(&id, true).await?;
                    {
                        let mut state = state.lock().unwrap();
                        state.target_position = FULLY_OPENED;
                        state.position_state = DoorPositionState::Opening as u8;
                    };

                    tokio::spawn(async move {
                        // sleep for the required time
                        tokio::time::sleep(opening_closing_time).await;
                        {
                            let mut state = state.lock().unwrap();
                            state.target_position = FULLY_OPENED;
                            state.current_position = FULLY_OPENED;
                            state.position_state = DoorPositionState::Stopped as u8;
                        };
                        info!("Door {id} reached the requested position {new_pos}");

                        // sleep for the required time
                        tokio::time::sleep(opened_time).await;
                        {
                            let mut state = state.lock().unwrap();
                            state.target_position = FULLY_CLOSED;
                            state.position_state = DoorPositionState::Closing as u8;
                        };

                        info!("Door {id} started closing");
                        // sleep for the required time
                        tokio::time::sleep(opening_closing_time).await;
                        {
                            let mut state = state.lock().unwrap();
                            state.target_position = FULLY_CLOSED;
                            state.current_position = FULLY_CLOSED;
                            state.position_state = DoorPositionState::Stopped as u8;
                        };
                        info!("Door {id} is closed");
                    });

                    Ok(())
                }
                .boxed()
            }));
    }
}

impl ComelitAccessory<DoorDeviceData> for ComelitDoorAccessory {
    fn get_comelit_id(&self) -> &str {
        &self.id
    }

    async fn update(&mut self, data: &DoorDeviceData) -> Result<()> {
        let new_state = DoorState::from(data);
        let mut state = self.state.lock().unwrap();
        *state = new_state;
        info!("Updated door {} state to {:?}", self.id, *state);
        Ok(())
    }
}
