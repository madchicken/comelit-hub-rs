use std::sync::Arc;

use anyhow::{Context, Result};

use futures::FutureExt;
use hap::characteristic::HapCharacteristic;
use hap::pointer::Accessory;
use hap::server::Server;
use hap::{
    HapType,
    accessory::HapAccessory,
    characteristic::AsyncCharacteristicCallbacks,
    server::IpServer,
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
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, Sender};
use tracing::{debug, info, warn};

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
        let mut v: Vec<&dyn HapService> = vec![&self.accessory_information, &self.thermostat];
        if let Some(ref hd) = self.humidifier_dehumidifier {
            v.push(hd);
        }
        v
    }

    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        let mut v: Vec<&mut dyn HapService> =
            vec![&mut self.accessory_information, &mut self.thermostat];
        if let Some(ref mut hd) = self.humidifier_dehumidifier {
            v.push(hd);
        }
        v
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
    pub async fn new(id: u64, name: &str, device_id: &str, has_dehumidifier: bool) -> Result<Self> {
        let information = hap::accessory::AccessoryInformation {
            name: name.to_string(),
            manufacturer: "Comelit".to_string(),
            serial_number: device_id.to_string(),
            ..Default::default()
        };
        let accessory_information = information.to_service(1, id)?;
        let info_len = accessory_information.get_characteristics().len() as u64;

        let mut thermostat = ThermostatService::new(1 + info_len + 1, id);
        thermostat.set_primary(true);

        let humidifier_dehumidifier = if has_dehumidifier {
            let offset = 1 + info_len + 1 + thermostat.get_characteristics().len() as u64 + 1;
            Some(HumidifierDehumidifierService::new(offset, id))
        } else {
            None
        };

        Ok(Self {
            id,
            accessory_information,
            thermostat,
            humidifier_dehumidifier,
        })
    }
}

// ── Commands ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
enum ThermostatCommand {
    /// HomeKit changed target temperature
    SetTargetTemperature(f32),
    /// HomeKit changed target humidity (thermostat service)
    SetTargetHumidity(f32),
    /// HomeKit changed HVAC mode
    SetHvacMode(u8),
    /// HomeKit toggled dehumidifier on/off
    SetDehumidifierActive(u8),
    /// HomeKit changed dehumidifier threshold
    SetDehumidifierThreshold(f32),
    /// MQTT hub pushed a status update → update HAP characteristics
    MqttPush(ThermostatState),
    /// Provide the HAP accessory pointer to the worker after server registration
    SetAccessory(Accessory),
}

// ── Worker ──────────────────────────────────────────────────────────────────────

struct ThermostatWorker {
    id: String,
    state: Arc<Mutex<ThermostatState>>,
    client: ComelitClient,
    accessory: Option<Accessory>,
}

impl ThermostatWorker {
    fn new(id: String, state: Arc<Mutex<ThermostatState>>, client: ComelitClient) -> Self {
        Self {
            id,
            state,
            client,
            accessory: None,
        }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<ThermostatCommand>) {
        while let Some(cmd) = rx.recv().await {
            if let Err(e) = self.handle(cmd).await {
                warn!("ThermostatWorker {}: {e}", self.id);
            }
        }
    }

    async fn handle(&mut self, cmd: ThermostatCommand) -> Result<()> {
        match cmd {
            ThermostatCommand::SetAccessory(acc) => {
                self.accessory = Some(acc);
            }

            ThermostatCommand::SetTargetTemperature(new) => {
                let temperature = (new * 10.0) as i32;
                if let Err(e) = self
                    .client
                    .set_thermostat_temperature(&self.id, temperature)
                    .await
                {
                    warn!("set_thermostat_temperature failed: {e}");
                }
            }

            ThermostatCommand::SetTargetHumidity(humidity) => {
                if let Err(e) = self.client.set_humidity(&self.id, humidity).await {
                    warn!("set_humidity failed: {e}");
                }
            }

            ThermostatCommand::SetHvacMode(new) => {
                let prev = self.state.lock().await.target_heating_cooling_state as u8;
                debug!(
                    "Target heating cooling state updated from {} to {}",
                    prev, new
                );

                if let Err(e) = self
                    .client
                    .toggle_thermostat_status(
                        &self.id,
                        if TargetHeatingCoolingState::Off as u8 == new {
                            ClimaOnOff::OffThermo
                        } else {
                            ClimaOnOff::OnThermo
                        },
                    )
                    .await
                {
                    warn!("toggle_thermostat_status failed: {e}");
                }

                if prev == TargetHeatingCoolingState::Auto as u8
                    && new != TargetHeatingCoolingState::Off as u8
                {
                    if let Err(e) = self
                        .client
                        .set_thermostat_mode(&self.id, ClimaMode::Manual)
                        .await
                    {
                        warn!("set_thermostat_mode(Manual) failed: {e}");
                    }
                }

                match TargetHeatingCoolingState::from(new) {
                    TargetHeatingCoolingState::Auto => {
                        if let Err(e) = self
                            .client
                            .set_thermostat_mode(&self.id, ClimaMode::Auto)
                            .await
                        {
                            warn!("set_thermostat_mode(Auto) failed: {e}");
                        }
                    }
                    TargetHeatingCoolingState::Cool => {
                        if let Err(e) = self
                            .client
                            .set_thermostat_season(&self.id, ThermoSeason::Summer)
                            .await
                        {
                            warn!("set_thermostat_season(Summer) failed: {e}");
                        }
                    }
                    TargetHeatingCoolingState::Heat => {
                        if let Err(e) = self
                            .client
                            .set_thermostat_season(&self.id, ThermoSeason::Winter)
                            .await
                        {
                            warn!("set_thermostat_season(Winter) failed: {e}");
                        }
                    }
                    TargetHeatingCoolingState::Off => {}
                }
            }

            ThermostatCommand::SetDehumidifierActive(new) => {
                debug!("Dehumidifier active updated to {}", new);
                if let Err(e) = self
                    .client
                    .toggle_thermostat_status(
                        &self.id,
                        if new == 1 {
                            ClimaOnOff::OnHumi
                        } else {
                            ClimaOnOff::OffHumi
                        },
                    )
                    .await
                {
                    warn!("toggle_thermostat_status (humi) failed: {e}");
                }
            }

            ThermostatCommand::SetDehumidifierThreshold(humidity) => {
                if let Err(e) = self.client.set_humidity(&self.id, humidity).await {
                    warn!("set_humidity (threshold) failed: {e}");
                }
            }

            ThermostatCommand::MqttPush(new_state) => {
                *self.state.lock().await = new_state.clone();
                self.update_accessory(&new_state).await?;
                info!("Updated thermostat {} from MQTT push", self.id);
            }
        }
        Ok(())
    }

    /// Push all characteristic values into the HAP accessory.
    /// Called only from the worker task — never from inside an on_update_async callback.
    async fn update_accessory(&self, state: &ThermostatState) -> Result<()> {
        let Some(ref accessory) = self.accessory else {
            return Ok(());
        };

        let mut acc = accessory.lock().await;

        let thermostat_service = acc
            .get_mut_service(HapType::Thermostat)
            .context("Thermostat service not found")?;

        if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::CurrentTemperature) {
            ch.update_value(Value::from(state.temperature)).await?;
        }
        if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::TargetTemperature) {
            ch.update_value(Value::from(state.target_temperature))
                .await?;
        }
        if let Some(ch) =
            thermostat_service.get_mut_characteristic(HapType::CurrentHeatingCoolingState)
        {
            ch.update_value(Value::from(state.heating_cooling_state as u8))
                .await?;
        }
        if let Some(ch) =
            thermostat_service.get_mut_characteristic(HapType::TargetHeatingCoolingState)
        {
            ch.update_value(Value::from(state.target_heating_cooling_state as u8))
                .await?;
        }
        if let Some(ch) =
            thermostat_service.get_mut_characteristic(HapType::CurrentRelativeHumidity)
        {
            ch.update_value(Value::from(state.humidity)).await?;
        }
        if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::TargetRelativeHumidity)
        {
            ch.update_value(Value::from(state.target_humidity)).await?;
        }

        if let Some(hd_service) = acc.get_mut_service(HapType::HumidifierDehumidifier) {
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::Active) {
                ch.update_value(Value::from(state.dehumidifier_active as u8))
                    .await?;
            }
            if let Some(ch) =
                hd_service.get_mut_characteristic(HapType::CurrentHumidifierDehumidifierState)
            {
                ch.update_value(Value::from(state.dehumidifier_current_state))
                    .await?;
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::CurrentRelativeHumidity) {
                ch.update_value(Value::from(state.humidity)).await?;
            }
            if let Some(ch) =
                hd_service.get_mut_characteristic(HapType::RelativeHumidityDehumidifierThreshold)
            {
                ch.update_value(Value::from(state.target_humidity)).await?;
            }
        }

        Ok(())
    }
}

// ── Public accessory ────────────────────────────────────────────────────────────

pub(crate) struct ComelitThermostatAccessory {
    id: String,
    pub name: String,
    command_sender: Sender<ThermostatCommand>,
    #[allow(dead_code)]
    accessory: Accessory,
}

impl ComelitAccessory<ThermostatDeviceData> for ComelitThermostatAccessory {
    fn get_comelit_id(&self) -> &str {
        self.id.as_str()
    }

    async fn update(&mut self, thermostat_data: &ThermostatDeviceData) -> Result<()> {
        let new_state = ThermostatState::from(thermostat_data);
        // Hand the update to the worker: it will acquire Accessory.lock() only after
        // HAP has released it, eliminating the lock-contention freeze.
        self.command_sender
            .send(ThermostatCommand::MqttPush(new_state))
            .await
            .ok();
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

        // ── Initial values ──────────────────────────────────────────────────────

        accessory
            .thermostat
            .current_temperature
            .set_value(Value::from(state.temperature))
            .await?;

        accessory
            .thermostat
            .target_temperature
            .set_value(Value::from(state.target_temperature))
            .await?;

        accessory
            .thermostat
            .current_heating_cooling_state
            .set_value(Value::from(state.heating_cooling_state as u8))
            .await?;

        accessory
            .thermostat
            .target_heating_cooling_state
            .set_value(Value::from(state.target_heating_cooling_state as u8))
            .await?;

        if let Some(ref mut char) = accessory.thermostat.current_relative_humidity {
            char.set_value(Value::from(state.humidity)).await?;
        }

        if let Some(ref mut char) = accessory.thermostat.target_relative_humidity {
            char.set_value(Value::from(state.target_humidity)).await?;
        }

        // ── Read callbacks (read from shared state — no accessory lock needed) ──

        {
            let s = Arc::clone(&arc_state);
            accessory
                .thermostat
                .current_temperature
                .on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Ok(Some(s.lock().await.temperature)) }.boxed()
                }));
        }
        {
            let s = Arc::clone(&arc_state);
            accessory
                .thermostat
                .target_temperature
                .on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Ok(Some(s.lock().await.target_temperature)) }.boxed()
                }));
        }
        {
            let s = Arc::clone(&arc_state);
            accessory
                .thermostat
                .current_heating_cooling_state
                .on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Ok(Some(s.lock().await.heating_cooling_state as u8)) }.boxed()
                }));
        }
        {
            let s = Arc::clone(&arc_state);
            accessory
                .thermostat
                .target_heating_cooling_state
                .on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Ok(Some(s.lock().await.target_heating_cooling_state as u8)) }
                        .boxed()
                }));
        }
        if let Some(ref mut char) = accessory.thermostat.current_relative_humidity {
            let s = Arc::clone(&arc_state);
            char.on_read_async(Some(move || {
                let s = s.clone();
                async move { Ok(Some(s.lock().await.humidity)) }.boxed()
            }));
        }
        if let Some(ref mut char) = accessory.thermostat.target_relative_humidity {
            let s = Arc::clone(&arc_state);
            char.on_read_async(Some(move || {
                let s = s.clone();
                async move { Ok(Some(s.lock().await.target_humidity)) }.boxed()
            }));
        }

        // ── Write callbacks: only send to channel, return immediately ───────────

        let (command_sender, command_receiver) = mpsc::channel::<ThermostatCommand>(32);

        {
            let tx = command_sender.clone();
            accessory
                .thermostat
                .target_temperature
                .on_update_async(Some(move |_, new: f32| {
                    let tx = tx.clone();
                    async move {
                        tx.send(ThermostatCommand::SetTargetTemperature(new))
                            .await
                            .ok();
                        Ok(())
                    }
                    .boxed()
                }));
        }

        if let Some(ref mut char) = accessory.thermostat.target_relative_humidity {
            let tx = command_sender.clone();
            char.on_update_async(Some(move |_prev, new: f32| {
                let tx = tx.clone();
                async move {
                    tx.send(ThermostatCommand::SetTargetHumidity(new))
                        .await
                        .ok();
                    Ok(())
                }
                .boxed()
            }));
        }

        {
            let tx = command_sender.clone();
            accessory
                .thermostat
                .target_heating_cooling_state
                .on_update_async(Some(move |_prev: u8, new: u8| {
                    let tx = tx.clone();
                    async move {
                        tx.send(ThermostatCommand::SetHvacMode(new)).await.ok();
                        Ok(())
                    }
                    .boxed()
                }));
        }

        // ── Dehumidifier service ────────────────────────────────────────────────

        if let Some(ref mut hd) = accessory.humidifier_dehumidifier {
            hd.target_humidifier_dehumidifier_state
                .set_value(Value::from(2u8))
                .await?;

            hd.active
                .set_value(Value::from(state.dehumidifier_active as u8))
                .await?;

            {
                let s = Arc::clone(&arc_state);
                hd.active.on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Ok(Some(s.lock().await.dehumidifier_active as u8)) }.boxed()
                }));
            }

            hd.current_humidifier_dehumidifier_state
                .set_value(Value::from(state.dehumidifier_current_state))
                .await?;

            {
                let s = Arc::clone(&arc_state);
                hd.current_humidifier_dehumidifier_state
                    .on_read_async(Some(move || {
                        let s = s.clone();
                        async move { Ok(Some(s.lock().await.dehumidifier_current_state)) }.boxed()
                    }));
            }

            hd.current_relative_humidity
                .set_value(Value::from(state.humidity))
                .await?;

            {
                let s = Arc::clone(&arc_state);
                hd.current_relative_humidity.on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Ok(Some(s.lock().await.humidity)) }.boxed()
                }));
            }

            if let Some(ref mut threshold) = hd.relative_humidity_dehumidifier_threshold {
                threshold
                    .set_value(Value::from(state.target_humidity))
                    .await?;

                {
                    let s = Arc::clone(&arc_state);
                    threshold.on_read_async(Some(move || {
                        let s = s.clone();
                        async move { Ok(Some(s.lock().await.target_humidity)) }.boxed()
                    }));
                }

                let tx = command_sender.clone();
                threshold.on_update_async(Some(move |_prev, new: f32| {
                    let tx = tx.clone();
                    async move {
                        tx.send(ThermostatCommand::SetDehumidifierThreshold(new))
                            .await
                            .ok();
                        Ok(())
                    }
                    .boxed()
                }));
            }

            {
                let tx = command_sender.clone();
                hd.active.on_update_async(Some(move |_prev: u8, new: u8| {
                    let tx = tx.clone();
                    async move {
                        tx.send(ThermostatCommand::SetDehumidifierActive(new))
                            .await
                            .ok();
                        Ok(())
                    }
                    .boxed()
                }));
            }
        }

        // ── Spawn worker ────────────────────────────────────────────────────────

        let worker = ThermostatWorker::new(comelit_id.clone(), arc_state.clone(), client);
        tokio::spawn(worker.run(command_receiver));

        let accessory = server.add_accessory(accessory).await?;
        command_sender
            .send(ThermostatCommand::SetAccessory(accessory.clone()))
            .await
            .ok();

        Ok(Self {
            id: data.id.clone(),
            name,
            command_sender,
            accessory,
        })
    }
}
