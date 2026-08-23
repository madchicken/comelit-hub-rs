use std::sync::Arc;

use anyhow::Result;

use async_trait::async_trait;
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
use tracing::{debug, warn};

use crate::accessories::{ComelitAccessory, state::thermostat::HumidityState};
use crate::web::metrics::Metrics;
use comelit_client_rs::thermostat::{
    TargetHeatingCoolingState, ThermostatHandle, ThermostatSink, ThermostatState,
    spawn_thermostat_worker,
};
use comelit_client_rs::{ComelitClient, ObjectSubtype, ThermostatDeviceData};

#[derive(Debug)]
struct ComelitThermostat {
    id: u64,
    pub accessory_information: AccessoryInformationService,
    pub thermostat: ThermostatService,
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
        self.get_services().into_iter().find(|&s| s.get_type() == hap_type).map(|v| v as _)
    }

    fn get_mut_service(&mut self, hap_type: HapType) -> Option<&mut dyn HapService> {
        self.get_mut_services().into_iter().find(|s| s.get_type() == hap_type).map(|v| v as _)
    }

    fn get_services(&self) -> Vec<&dyn HapService> {
        let mut v: Vec<&dyn HapService> = vec![&self.accessory_information, &self.thermostat];
        if let Some(ref hd) = self.humidifier_dehumidifier {
            v.push(hd);
        }
        v
    }

    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        let mut v: Vec<&mut dyn HapService> = vec![&mut self.accessory_information, &mut self.thermostat];
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

        Ok(Self { id, accessory_information, thermostat, humidifier_dehumidifier })
    }
}

/// Writes thermal state updates into the HomeKit `Thermostat` service's
/// characteristics. Humidity/dehumidifier characteristics are NOT written
/// here — they're written directly by `ComelitThermostatAccessory::update`,
/// since that state never passes through the shared worker.
///
/// `state` mirrors the shared worker's private thermal state so that the
/// read-callback closures registered in `ComelitThermostatAccessory::new`
/// (which cannot reach into the worker task directly) always observe the
/// latest value rather than a stale, construction-time snapshot. It shares
/// the same `Arc` as `thermal_state_ro` in `ComelitThermostatAccessory::new`.
struct HapThermostatSink {
    accessory: Accessory,
    state: Arc<Mutex<ThermostatState>>,
}

#[async_trait]
impl ThermostatSink for HapThermostatSink {
    async fn update(&self, state: ThermostatState) {
        *self.state.lock().await = state;

        let mut acc = self.accessory.lock().await;
        let Some(thermostat_service) = acc.get_mut_service(HapType::Thermostat) else {
            warn!("Thermostat service not found while updating characteristics");
            return;
        };

        if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::CurrentTemperature) {
            if let Err(e) = ch.update_value(Value::from(state.temperature)).await {
                warn!("Failed to update CurrentTemperature: {e}");
            }
        }
        if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::TargetTemperature) {
            if let Err(e) = ch.update_value(Value::from(state.target_temperature)).await {
                warn!("Failed to update TargetTemperature: {e}");
            }
        }
        if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::CurrentHeatingCoolingState) {
            if let Err(e) = ch.update_value(Value::from(u8::from(state.heating_cooling_state))).await {
                warn!("Failed to update CurrentHeatingCoolingState: {e}");
            }
        }
        if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::TargetHeatingCoolingState) {
            if let Err(e) = ch.update_value(Value::from(u8::from(state.target_heating_cooling_state))).await {
                warn!("Failed to update TargetHeatingCoolingState: {e}");
            }
        }
    }
}

/// Commands the local (HAP-only) humidity/dehumidifier worker handles —
/// none of this crosses into the shared `client::thermostat` module.
#[derive(Debug)]
enum HumidityCommand {
    SetTargetHumidity(f32),
    SetDehumidifierActive(u8),
    SetDehumidifierThreshold(f32),
    MqttPush(HumidityState),
    SetAccessory(Accessory),
}

struct HumidityWorker {
    id: String,
    state: Arc<Mutex<HumidityState>>,
    client: ComelitClient,
    accessory: Option<Accessory>,
}

impl HumidityWorker {
    fn new(id: String, state: Arc<Mutex<HumidityState>>, client: ComelitClient) -> Self {
        Self { id, state, client, accessory: None }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<HumidityCommand>) {
        while let Some(cmd) = rx.recv().await {
            if let Err(e) = self.handle(cmd).await {
                warn!("HumidityWorker {}: {e}", self.id);
            }
        }
    }

    async fn handle(&mut self, cmd: HumidityCommand) -> Result<()> {
        match cmd {
            HumidityCommand::SetAccessory(acc) => {
                self.accessory = Some(acc);
            }
            HumidityCommand::SetTargetHumidity(humidity) => {
                match self.client.set_humidity(&self.id, humidity as i32).await {
                    Ok(()) => {
                        let state = {
                            let mut guard = self.state.lock().await;
                            guard.target_humidity = humidity;
                            *guard
                        };
                        self.update_accessory(&state).await?;
                    }
                    Err(e) => warn!("set_humidity failed: {e}"),
                }
            }
            HumidityCommand::SetDehumidifierActive(new) => {
                debug!("Dehumidifier active updated to {}", new);
                match self
                    .client
                    .toggle_thermostat_status(
                        &self.id,
                        if new == 1 { comelit_client_rs::ClimaOnOff::OnHumi } else { comelit_client_rs::ClimaOnOff::OffHumi },
                    )
                    .await
                {
                    Ok(()) => {
                        let active = new == 1;
                        let state = {
                            let mut guard = self.state.lock().await;
                            guard.dehumidifier_active = active;
                            guard.dehumidifier_current_state = if active { 1 } else { 0 };
                            *guard
                        };
                        self.update_accessory(&state).await?;
                    }
                    Err(e) => warn!("toggle_thermostat_status (humi) failed: {e}"),
                }
            }
            HumidityCommand::SetDehumidifierThreshold(humidity) => {
                match self.client.set_humidity(&self.id, humidity as i32).await {
                    Ok(()) => {
                        let state = {
                            let mut guard = self.state.lock().await;
                            guard.target_humidity = humidity;
                            *guard
                        };
                        self.update_accessory(&state).await?;
                    }
                    Err(e) => warn!("set_humidity (threshold) failed: {e}"),
                }
            }
            HumidityCommand::MqttPush(new_state) => {
                *self.state.lock().await = new_state;
                self.update_accessory(&new_state).await?;
            }
        }
        Ok(())
    }

    async fn update_accessory(&self, state: &HumidityState) -> Result<()> {
        let Some(ref accessory) = self.accessory else { return Ok(()) };
        let mut acc = accessory.lock().await;

        if let Some(thermostat_service) = acc.get_mut_service(HapType::Thermostat) {
            if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::CurrentRelativeHumidity) {
                ch.update_value(Value::from(state.humidity)).await?;
            }
            if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::TargetRelativeHumidity) {
                ch.update_value(Value::from(state.target_humidity)).await?;
            }
        }

        if let Some(hd_service) = acc.get_mut_service(HapType::HumidifierDehumidifier) {
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::Active) {
                ch.update_value(Value::from(state.dehumidifier_active as u8)).await?;
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::CurrentHumidifierDehumidifierState) {
                ch.update_value(Value::from(state.dehumidifier_current_state)).await?;
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::CurrentRelativeHumidity) {
                ch.update_value(Value::from(state.humidity)).await?;
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::RelativeHumidityDehumidifierThreshold) {
                ch.update_value(Value::from(state.target_humidity)).await?;
            }
        }

        Ok(())
    }
}

pub(crate) struct ComelitThermostatAccessory {
    id: String,
    pub name: String,
    thermostat_handle: ThermostatHandle,
    humidity_sender: Sender<HumidityCommand>,
    #[allow(dead_code)]
    accessory: Accessory,
}

impl ComelitAccessory<ThermostatDeviceData> for ComelitThermostatAccessory {
    fn get_comelit_id(&self) -> &str {
        self.id.as_str()
    }

    async fn update(&mut self, thermostat_data: &ThermostatDeviceData) -> Result<()> {
        self.thermostat_handle.mqtt_push(ThermostatState::from(thermostat_data)).await;
        self.humidity_sender
            .send(HumidityCommand::MqttPush(HumidityState::from(thermostat_data)))
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
        let mut accessory = ComelitThermostat::new(id, name.as_str(), comelit_id.as_str(), has_dehumidifier).await?;

        let thermal_state = ThermostatState::from(data);
        let humidity_state = HumidityState::from(data);

        // ── Initial values ──────────────────────────────────────────────────

        accessory.thermostat.current_temperature.set_value(Value::from(thermal_state.temperature)).await?;
        accessory.thermostat.target_temperature.set_value(Value::from(thermal_state.target_temperature)).await?;
        accessory.thermostat.current_heating_cooling_state
            .set_value(Value::from(u8::from(thermal_state.heating_cooling_state))).await?;
        accessory.thermostat.target_heating_cooling_state
            .set_value(Value::from(u8::from(thermal_state.target_heating_cooling_state))).await?;

        if let Some(ref mut char) = accessory.thermostat.current_relative_humidity {
            char.set_value(Value::from(humidity_state.humidity)).await?;
        }
        if let Some(ref mut char) = accessory.thermostat.target_relative_humidity {
            char.set_value(Value::from(humidity_state.target_humidity)).await?;
        }

        // ── Thermal handle + read/update callbacks ─────────────────────────

        let thermostat_handle = spawn_thermostat_worker(comelit_id.clone(), thermal_state, client.clone());

        let thermal_state_ro = Arc::new(Mutex::new(thermal_state));
        {
            let s = Arc::clone(&thermal_state_ro);
            accessory.thermostat.current_temperature.on_read_async(Some(move || {
                let s = s.clone();
                async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.temperature)) }.boxed()
            }));
        }
        {
            let s = Arc::clone(&thermal_state_ro);
            accessory.thermostat.target_temperature.on_read_async(Some(move || {
                let s = s.clone();
                async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.target_temperature)) }.boxed()
            }));
        }
        {
            let s = Arc::clone(&thermal_state_ro);
            accessory.thermostat.current_heating_cooling_state.on_read_async(Some(move || {
                let s = s.clone();
                async move { Metrics::inc_hap_requests(); Ok(Some(u8::from(s.lock().await.heating_cooling_state))) }.boxed()
            }));
        }
        {
            let s = Arc::clone(&thermal_state_ro);
            accessory.thermostat.target_heating_cooling_state.on_read_async(Some(move || {
                let s = s.clone();
                async move { Metrics::inc_hap_requests(); Ok(Some(u8::from(s.lock().await.target_heating_cooling_state))) }.boxed()
            }));
        }

        {
            let handle = thermostat_handle.clone();
            accessory.thermostat.target_temperature.on_update_async(Some(move |_, new: f32| {
                let handle = handle.clone();
                async move {
                    Metrics::inc_hap_requests();
                    handle.set_target_temperature(new).await;
                    Ok(())
                }
                .boxed()
            }));
        }

        {
            let handle = thermostat_handle.clone();
            accessory.thermostat.target_heating_cooling_state.on_update_async(Some(move |_prev: u8, new: u8| {
                let handle = handle.clone();
                async move {
                    Metrics::inc_hap_requests();
                    handle.set_hvac_mode(TargetHeatingCoolingState::from(new)).await;
                    Ok(())
                }
                .boxed()
            }));
        }

        // NOTE: `thermal_state_ro` mirrors the worker's internal state purely
        // for the read-callback closures above, which cannot reach into the
        // worker task directly. It is kept in sync by `HapThermostatSink`
        // below (which shares this same `Arc` via its `state` field, written
        // on every `ThermostatSink::update` call) once the accessory is
        // registered and the sink is wired in below — until then, reads
        // return the construction-time snapshot, matching the old code's
        // behavior (old code also only updated its shared `arc_state` from
        // within the worker task, which only runs after `SetAccessory`).

        // ── Humidity/dehumidifier worker (local, unchanged from before) ────

        let humidity_arc_state = Arc::new(Mutex::new(humidity_state));
        let (humidity_sender, humidity_receiver) = mpsc::channel::<HumidityCommand>(32);

        if let Some(ref mut hd) = accessory.humidifier_dehumidifier {
            hd.target_humidifier_dehumidifier_state.set_value(Value::from(2u8)).await?;
            hd.active.set_value(Value::from(humidity_state.dehumidifier_active as u8)).await?;
            hd.current_humidifier_dehumidifier_state.set_value(Value::from(humidity_state.dehumidifier_current_state)).await?;
            hd.current_relative_humidity.set_value(Value::from(humidity_state.humidity)).await?;

            {
                let s = Arc::clone(&humidity_arc_state);
                hd.active.on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.dehumidifier_active as u8)) }.boxed()
                }));
            }
            {
                let s = Arc::clone(&humidity_arc_state);
                hd.current_humidifier_dehumidifier_state.on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.dehumidifier_current_state)) }.boxed()
                }));
            }
            {
                let s = Arc::clone(&humidity_arc_state);
                hd.current_relative_humidity.on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.humidity)) }.boxed()
                }));
            }

            if let Some(ref mut threshold) = hd.relative_humidity_dehumidifier_threshold {
                threshold.set_value(Value::from(humidity_state.target_humidity)).await?;
                {
                    let s = Arc::clone(&humidity_arc_state);
                    threshold.on_read_async(Some(move || {
                        let s = s.clone();
                        async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.target_humidity)) }.boxed()
                    }));
                }
                let tx = humidity_sender.clone();
                threshold.on_update_async(Some(move |_prev, new: f32| {
                    let tx = tx.clone();
                    async move {
                        Metrics::inc_hap_requests();
                        tx.send(HumidityCommand::SetDehumidifierThreshold(new)).await.ok();
                        Ok(())
                    }
                    .boxed()
                }));
            }

            {
                let tx = humidity_sender.clone();
                hd.active.on_update_async(Some(move |_prev: u8, new: u8| {
                    let tx = tx.clone();
                    async move {
                        Metrics::inc_hap_requests();
                        tx.send(HumidityCommand::SetDehumidifierActive(new)).await.ok();
                        Ok(())
                    }
                    .boxed()
                }));
            }
        }

        if let Some(ref mut char) = accessory.thermostat.target_relative_humidity {
            let tx = humidity_sender.clone();
            char.on_update_async(Some(move |_prev, new: f32| {
                let tx = tx.clone();
                async move {
                    Metrics::inc_hap_requests();
                    tx.send(HumidityCommand::SetTargetHumidity(new)).await.ok();
                    Ok(())
                }
                .boxed()
            }));
        }

        let humidity_worker = HumidityWorker::new(comelit_id.clone(), humidity_arc_state, client);
        tokio::spawn(humidity_worker.run(humidity_receiver));

        // ── Register accessory, wire sinks ──────────────────────────────────

        let accessory = server.add_accessory(accessory).await?;

        thermostat_handle
            .set_sink(Box::new(HapThermostatSink { accessory: accessory.clone(), state: Arc::clone(&thermal_state_ro) }))
            .await;
        humidity_sender.send(HumidityCommand::SetAccessory(accessory.clone())).await.ok();

        Ok(Self {
            id: data.id.clone(),
            name,
            thermostat_handle,
            humidity_sender,
            accessory,
        })
    }
}
