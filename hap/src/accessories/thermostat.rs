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
use tracing::{info, warn};

use crate::accessories::ComelitAccessory;
use crate::web::metrics::Metrics;
use comelit_client_rs::humidity::{HumidityHandle, HumiditySink, HumidityState, spawn_humidity_worker};
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
    device_id: String,
    accessory: Accessory,
    state: Arc<Mutex<ThermostatState>>,
}

#[async_trait]
impl ThermostatSink for HapThermostatSink {
    async fn update(&self, state: ThermostatState) {
        // The state lock is taken in its own scope so it is guaranteed released
        // before the accessory lock below. `hap-rs` locks in the opposite
        // order: `AccessoryDatabase::read_characteristic` holds the accessory
        // lock for the whole of `get_value()`, which invokes the
        // `on_read_async` closures registered in
        // `ComelitThermostatAccessory::new` — and those lock this very same
        // state mutex. Holding state while acquiring accessory would therefore
        // be an ABBA deadlock, and the accessory-database lock is global, so it
        // would freeze the entire HAP bridge, not just this accessory. Keep
        // this an explicit block; do not collapse it into a bare
        // `*self.state.lock().await = state;` whose guard survives only until
        // the end of the statement by accident.
        {
            let mut guard = self.state.lock().await;
            *guard = state;
        }

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

        info!("Updated thermostat {} from thermal state push", self.device_id);
    }
}

/// Writes humidity/dehumidifier state updates into the HomeKit `Thermostat`
/// service's humidity characteristics and the separate
/// `HumidifierDehumidifier` service.
///
/// `state` mirrors the shared worker's private humidity state so that the
/// read-callback closures registered in `ComelitThermostatAccessory::new`
/// (which cannot reach into the worker task directly) always observe the
/// latest value rather than a stale, construction-time snapshot. It shares
/// the same `Arc` as `humidity_arc_state` in `ComelitThermostatAccessory::new`.
struct HapHumiditySink {
    device_id: String,
    accessory: Accessory,
    state: Arc<Mutex<HumidityState>>,
}

#[async_trait]
impl HumiditySink for HapHumiditySink {
    async fn update(&self, state: HumidityState) {
        // Same lock-ordering constraint as `HapThermostatSink::update`:
        // release the humidity-state lock before taking the accessory lock,
        // since `hap-rs` holds the accessory lock while running the
        // `on_read_async` closures that lock this very same state mutex.
        // Keep this an explicit block.
        {
            let mut guard = self.state.lock().await;
            *guard = state;
        }

        let mut acc = self.accessory.lock().await;

        if let Some(thermostat_service) = acc.get_mut_service(HapType::Thermostat) {
            if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::CurrentRelativeHumidity) {
                if let Err(e) = ch.update_value(Value::from(state.humidity)).await {
                    warn!("update_value for thermostat {} CurrentRelativeHumidity failed: {e}", self.device_id);
                }
            }
            if let Some(ch) = thermostat_service.get_mut_characteristic(HapType::TargetRelativeHumidity) {
                if let Err(e) = ch.update_value(Value::from(state.target_humidity)).await {
                    warn!("update_value for thermostat {} TargetRelativeHumidity failed: {e}", self.device_id);
                }
            }
        }

        if let Some(hd_service) = acc.get_mut_service(HapType::HumidifierDehumidifier) {
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::Active) {
                if let Err(e) = ch.update_value(Value::from(state.dehumidifier_active as u8)).await {
                    warn!("update_value for thermostat {} Active failed: {e}", self.device_id);
                }
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::CurrentHumidifierDehumidifierState) {
                if let Err(e) = ch.update_value(Value::from(state.dehumidifier_current_state)).await {
                    warn!("update_value for thermostat {} CurrentHumidifierDehumidifierState failed: {e}", self.device_id);
                }
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::CurrentRelativeHumidity) {
                if let Err(e) = ch.update_value(Value::from(state.humidity)).await {
                    warn!("update_value for thermostat {} humidifier CurrentRelativeHumidity failed: {e}", self.device_id);
                }
            }
            if let Some(ch) = hd_service.get_mut_characteristic(HapType::RelativeHumidityDehumidifierThreshold) {
                if let Err(e) = ch.update_value(Value::from(state.target_humidity)).await {
                    warn!("update_value for thermostat {} RelativeHumidityDehumidifierThreshold failed: {e}", self.device_id);
                }
            }
        }
    }
}

pub(crate) struct ComelitThermostatAccessory {
    id: String,
    pub name: String,
    thermostat_handle: ThermostatHandle,
    #[allow(dead_code)]
    humidity_handle: HumidityHandle,
    #[allow(dead_code)]
    accessory: Accessory,
}

impl ComelitAccessory<ThermostatDeviceData> for ComelitThermostatAccessory {
    fn get_comelit_id(&self) -> &str {
        self.id.as_str()
    }

    async fn update(&mut self, thermostat_data: &ThermostatDeviceData) -> Result<()> {
        self.thermostat_handle.mqtt_push(ThermostatState::from(thermostat_data)).await;
        self.humidity_handle.mqtt_push(HumidityState::from(thermostat_data)).await;
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
            accessory.thermostat.target_temperature.on_update_async(Some(move |prev: f32, new: f32| {
                let handle = handle.clone();
                async move {
                    Metrics::inc_hap_requests();
                    // hap-rs's get_value() re-invokes this on_update_async after
                    // *every* read of this characteristic (not just real writes),
                    // since it always calls set_value() with whatever the read
                    // callback returned. Without this guard, every routine
                    // HomeKit poll re-sends the same command to the Comelit hub.
                    if prev != new {
                        handle.set_target_temperature(new).await?;
                    }
                    Ok(())
                }
                .boxed()
            }));
        }

        {
            let handle = thermostat_handle.clone();
            accessory.thermostat.target_heating_cooling_state.on_update_async(Some(move |prev: u8, new: u8| {
                let handle = handle.clone();
                async move {
                    Metrics::inc_hap_requests();
                    // See target_temperature above: skip the no-op re-send that
                    // hap-rs's get_value() triggers on every plain read.
                    if prev != new {
                        handle.set_hvac_mode(TargetHeatingCoolingState::from(new)).await?;
                    }
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

        // ── Humidity/dehumidifier handle (shared client::humidity module) ──

        let humidity_arc_state = Arc::new(Mutex::new(humidity_state));
        let humidity_handle = spawn_humidity_worker(comelit_id.clone(), humidity_state, client);

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
                let handle = humidity_handle.clone();
                // hap-rs's get_value() re-invokes on_update_async after every
                // read of this characteristic, not just real writes — skip the
                // no-op re-send that would otherwise happen on every poll.
                threshold.on_update_async(Some(move |prev, new: f32| {
                    let handle = handle.clone();
                    async move {
                        Metrics::inc_hap_requests();
                        if prev != new {
                            handle.set_target_humidity(new).await?;
                        }
                        Ok(())
                    }
                    .boxed()
                }));
            }

            {
                let handle = humidity_handle.clone();
                // Same get_value()-on-every-read guard as above.
                hd.active.on_update_async(Some(move |prev: u8, new: u8| {
                    let handle = handle.clone();
                    async move {
                        Metrics::inc_hap_requests();
                        if prev != new {
                            handle.set_dehumidifier_active(new == 1).await?;
                        }
                        Ok(())
                    }
                    .boxed()
                }));
            }
        }

        if let Some(ref mut char) = accessory.thermostat.current_relative_humidity {
            let s = Arc::clone(&humidity_arc_state);
            char.on_read_async(Some(move || {
                let s = s.clone();
                async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.humidity)) }.boxed()
            }));
        }

        if let Some(ref mut char) = accessory.thermostat.target_relative_humidity {
            {
                let s = Arc::clone(&humidity_arc_state);
                char.on_read_async(Some(move || {
                    let s = s.clone();
                    async move { Metrics::inc_hap_requests(); Ok(Some(s.lock().await.target_humidity)) }.boxed()
                }));
            }
            let handle = humidity_handle.clone();
            // Same get_value()-on-every-read guard as the dehumidifier's
            // characteristics above.
            char.on_update_async(Some(move |prev, new: f32| {
                let handle = handle.clone();
                async move {
                    Metrics::inc_hap_requests();
                    if prev != new {
                        handle.set_target_humidity(new).await?;
                    }
                    Ok(())
                }
                .boxed()
            }));
        }

        // ── Register accessory, wire sinks ──────────────────────────────────

        let accessory = server.add_accessory(accessory).await?;

        thermostat_handle
            .set_sink(Box::new(HapThermostatSink {
                device_id: data.id.clone(),
                accessory: accessory.clone(),
                state: Arc::clone(&thermal_state_ro),
            }))
            .await;
        humidity_handle
            .set_sink(Box::new(HapHumiditySink {
                device_id: data.id.clone(),
                accessory: accessory.clone(),
                state: Arc::clone(&humidity_arc_state),
            }))
            .await;

        Ok(Self {
            id: data.id.clone(),
            name,
            thermostat_handle,
            humidity_handle,
            accessory,
        })
    }
}
