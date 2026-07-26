use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::Result;
use futures::FutureExt;
use hap::HapType;
use hap::characteristic::{AsyncCharacteristicCallbacks, CharacteristicCallbacks, HapCharacteristic};
use hap::{
    accessory::{AccessoryInformation, lightbulb::LightbulbAccessory},
    pointer::Accessory,
    server::{IpServer, Server},
};
use serde_json::Value;
use tokio::sync::mpsc::{self, Sender};
use tracing::{debug, info, warn};

use crate::accessories::comelit_accessory::ComelitAccessory;
use crate::accessories::state::light::LightState;
use comelit_client_rs::{ComelitClient, DeviceStatus, LightDeviceData};

#[derive(Debug)]
enum LightbulbCommand {
    /// HomeKit wrote a new power state → forward to MQTT
    HapWrite(bool),
    /// Hub pushed a status update → update HAP characteristic
    MqttPush(bool),
    /// Initialise the accessory pointer inside the worker
    SetAccessory(Accessory),
}

struct LightbulbWorker {
    id: String,
    state: Arc<LightState>,
    client: ComelitClient,
    accessory: Option<Accessory>,
}

impl LightbulbWorker {
    fn new(id: String, state: Arc<LightState>, client: ComelitClient) -> Self {
        Self { id, state, client, accessory: None }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<LightbulbCommand>) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                LightbulbCommand::SetAccessory(acc) => {
                    self.accessory = Some(acc);
                }
                LightbulbCommand::HapWrite(new_val) => {
                    let current = self.state.on.load(Ordering::Acquire);
                    if new_val != current {
                        if let Err(e) =
                            self.client.toggle_device_status(&self.id, new_val).await
                        {
                            warn!(
                                "toggle_device_status for lightbulb {} failed: {e}",
                                self.id
                            );
                        } else {
                            info!(
                                "Lightbulb {}: power state set to {}",
                                self.id, new_val
                            );
                            self.state.on.store(new_val, Ordering::Release);
                        }
                    }
                }
                LightbulbCommand::MqttPush(is_on) => {
                    self.state.on.store(is_on, Ordering::Release);
                    if let Some(ref accessory) = self.accessory {
                        let mut acc = accessory.lock().await;
                        let service = acc.get_mut_service(HapType::Lightbulb).unwrap();
                        if let Some(ch) =
                            service.get_mut_characteristic(HapType::PowerState)
                        {
                            if let Err(e) = ch.update_value(Value::from(is_on)).await {
                                warn!(
                                    "update_value for lightbulb {} failed: {e}",
                                    self.id
                                );
                            }
                        }
                    }
                    info!(
                        "Updated power state for device {}: {}",
                        self.id,
                        if is_on { "On" } else { "Off" }
                    );
                }
            }
        }
    }
}

pub(crate) struct ComelitLightbulbAccessory {
    id: String,
    pub name: String,
    state: Arc<LightState>,
    command_sender: Sender<LightbulbCommand>,
    #[allow(dead_code)]
    accessory: Accessory,
}

impl ComelitLightbulbAccessory {
    pub(crate) async fn new(
        id: u64,
        light_data: &LightDeviceData,
        client: ComelitClient,
        server: &IpServer,
    ) -> Result<Self> {
        let device_id = light_data.id.clone();
        let name = light_data.description.clone().unwrap_or(device_id.clone());

        let mut lightbulb_accessory = LightbulbAccessory::new(
            id,
            AccessoryInformation {
                name: name.clone(),
                manufacturer: "Comelit".to_string(),
                serial_number: device_id.clone(),
                ..Default::default()
            },
        )?;

        lightbulb_accessory.lightbulb.brightness = None;
        lightbulb_accessory.lightbulb.color_temperature = None;
        lightbulb_accessory.lightbulb.hue = None;
        lightbulb_accessory.lightbulb.saturation = None;
        lightbulb_accessory
            .lightbulb
            .characteristic_value_active_transition_count = None;
        lightbulb_accessory
            .lightbulb
            .characteristic_value_transition_control = None;
        lightbulb_accessory
            .lightbulb
            .supported_characteristic_value_transition_configuration = None;

        let state = Arc::new(LightState::from(light_data));
        debug!(?state, "Created Lightbulb state: {light_data:#?}");
        lightbulb_accessory
            .lightbulb
            .power_state
            .set_value(Value::Bool(state.on.load(Ordering::Acquire)))
            .await?;

        let (command_sender, command_receiver) = mpsc::channel::<LightbulbCommand>(16);

        // Read callback: reads from atomic state — no lock required
        {
            let id_ = device_id.clone();
            let state_ = state.clone();
            lightbulb_accessory.lightbulb.power_state.on_read(Some(move || {
                let value = state_.on.load(Ordering::Acquire);
                debug!("Lightbulb {} read: {}", id_, value);
                Ok(Some(value))
            }));
        }

        // Write callback: only sends to worker channel; returns immediately
        {
            let tx = command_sender.clone();
            lightbulb_accessory
                .lightbulb
                .power_state
                .on_update_async(Some(move |_current_val: bool, new_val: bool| {
                    let tx = tx.clone();
                    async move {
                        if let Err(e) = tx.send(LightbulbCommand::HapWrite(new_val)).await {
                            warn!("Failed to send lightbulb HapWrite command: {e}");
                        }
                        Ok(())
                    }
                    .boxed()
                }));
        }

        // Spawn worker — acquires Accessory lock only after HAP has released it
        let worker = LightbulbWorker::new(device_id.clone(), state.clone(), client);
        tokio::spawn(worker.run(command_receiver));

        let accessory = server.add_accessory(lightbulb_accessory).await?;
        command_sender
            .send(LightbulbCommand::SetAccessory(accessory.clone()))
            .await
            .ok();

        Ok(Self {
            id: device_id,
            name,
            state,
            command_sender,
            accessory,
        })
    }
}

impl ComelitAccessory<LightDeviceData> for ComelitLightbulbAccessory {
    fn get_comelit_id(&self) -> &str {
        self.id.as_str()
    }

    async fn update(&mut self, light_data: &LightDeviceData) -> Result<()> {
        let is_on = light_data.status.clone().unwrap_or_default() == DeviceStatus::On;
        // Update atomic state synchronously, then hand HAP update to the worker.
        // The worker acquires Accessory.lock() only after HAP has released it.
        self.state.on.store(is_on, Ordering::Release);
        self.command_sender
            .send(LightbulbCommand::MqttPush(is_on))
            .await
            .ok();
        Ok(())
    }
}
