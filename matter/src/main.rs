mod bridge;
mod covering;
mod light;
mod mdns;

use core::pin::pin;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

use async_io::Async;
use async_trait::async_trait;
use clap::Parser;
use embassy_futures::select::select4;
use log::{error, info};
use tokio::sync::RwLock;

use rs_matter::crypto::{default_crypto, Crypto};
use rs_matter::dm::clusters::app::on_off::{self};
use rs_matter::dm::clusters::desc;
use rs_matter::dm::clusters::groups;
use rs_matter::dm::clusters::net_comm::SharedNetworks;
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter::dm::endpoints;
use rs_matter::dm::events::NoEvents;
use rs_matter::dm::networks::eth::EthNetwork;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::subscriptions::Subscriptions;
use rs_matter::dm::{DataModel, DataModelHandler, Dataver};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::pairing::qr::QrTextType;
use rs_matter::persist::{DirKvBlobStore, SharedKvBlobStore};
use rs_matter::respond::DefaultResponder;
use rs_matter::sc::pase::MAX_COMM_WINDOW_TIMEOUT_SECS;
use rs_matter::transport::MATTER_SOCKET_BIND_ADDR;
use rs_matter::utils::select::Coalesce;
use rs_matter::utils::storage::pooled::PooledBuffers;
use rs_matter::{Matter, MATTER_PORT};

use comelit_client_rs::{
    ComelitClient, ComelitObserver, ComelitOptionsBuilder, DeviceStatus, HomeDeviceData, State,
    StatusUpdate, get_secrets,
};
use tokio::sync::mpsc;

use bridge::{BridgeMetadata, BridgedEntry, BridgedInfo, ComelitBridgeHandler, CoveringEntry, LightEntry, NonRootMatcher};
use covering::ComelitCoveringHandler;
use light::{ComelitOnOffHooks, LightState, MultiLightObserver, MqttCommand};

// ── DeferredObserver ──────────────────────────────────────────────────────────
//
// Created before discovery so it can be passed to ComelitClient::new, then
// wired to the real MultiLightObserver after the light list is known.

struct DeferredObserver {
    inner: Arc<RwLock<Option<ComelitObserver>>>,
}

#[async_trait]
impl StatusUpdate for DeferredObserver {
    async fn status_update(&self, device: &HomeDeviceData) {
        let guard = self.inner.read().await;
        if let Some(obs) = guard.as_ref() {
            obs.status_update(device).await;
        }
    }
}

// ── CLI args ──────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "comelit-matter", about = "Comelit → Matter bridge (all lights)")]
struct Args {
    /// Comelit hub hostname or IP
    #[arg(long, env = "COMELIT_HOST")]
    host: String,

    /// Comelit login user
    #[arg(long, env = "COMELIT_USER", default_value = "admin")]
    user: String,

    /// Comelit login password
    #[arg(long, env = "COMELIT_PASSWORD", default_value = "admin")]
    password: String,

    /// Path to the same settings JSON file used by the HAP bridge (only the
    /// `window_covering` section is read; every other field is ignored).
    #[arg(long, env = "COMELIT_SETTINGS")]
    settings: Option<String>,
}

/// Reads only the `window_covering` section out of the same settings JSON
/// file the HAP bridge uses (`hap::settings::Settings`). Unknown fields
/// (pairing_code, mount_*, prometheus_*, ...) are ignored by serde.
#[derive(serde::Deserialize)]
struct MatterSettings {
    #[serde(default)]
    window_covering: comelit_client_rs::covering::WindowCoveringSettings,
}

impl Default for MatterSettings {
    fn default() -> Self {
        Self {
            window_covering: comelit_client_rs::covering::WindowCoveringSettings::default(),
        }
    }
}

fn load_settings(path: &Option<String>) -> MatterSettings {
    match path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|e| {
                error!("Failed to parse settings file {path}: {e}, using defaults");
                MatterSettings::default()
            }),
            Err(e) => {
                error!("Failed to read settings file {path}: {e}, using defaults");
                MatterSettings::default()
            }
        },
        None => MatterSettings::default(),
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    // ── 1. MQTT command channel ───────────────────────────────────────────────

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<MqttCommand>();

    // ── 2. Create client with deferred observer ───────────────────────────────

    let deferred_slot: Arc<RwLock<Option<ComelitObserver>>> = Arc::new(RwLock::new(None));
    let deferred = Arc::new(DeferredObserver { inner: deferred_slot.clone() });

    let (mqtt_user, mqtt_password) = get_secrets();
    let options = ComelitOptionsBuilder::default()
        .host(Some(args.host.clone()))
        .port(None)
        .mqtt_user(mqtt_user)
        .mqtt_password(mqtt_password)
        .user(Some(args.user.clone()))
        .password(Some(args.password.clone()))
        .build()?;

    let client = ComelitClient::new(options, Some(deferred as _)).await?;
    client.login(State::Disconnected).await?;

    // ── 3. Discover all lights and window coverings ──────────────────────────

    let settings = load_settings(&args.settings);

    let index = client.fetch_index(2).await?;
    let mut lights_data: Vec<(String, String, bool)> = index
        .iter()
        .filter_map(|entry| {
            if let HomeDeviceData::Light(l) = entry.value() {
                let initial_on =
                    l.status.as_ref().map(|s| s == &DeviceStatus::On).unwrap_or(false);
                let label = l.description.clone().unwrap_or_else(|| entry.key().clone());
                Some((entry.key().clone(), label, initial_on))
            } else {
                None
            }
        })
        .collect();
    lights_data.sort_by(|a, b| a.0.cmp(&b.0));

    let mut covering_data: Vec<(String, String, comelit_client_rs::covering::WindowCoveringState)> = index
        .iter()
        .filter_map(|entry| {
            if let HomeDeviceData::WindowCovering(wc) = entry.value() {
                let label = wc.description.clone().unwrap_or_else(|| entry.key().clone());
                let initial_state = comelit_client_rs::covering::WindowCoveringState::from(wc);
                Some((entry.key().clone(), label, initial_state))
            } else {
                None
            }
        })
        .collect();
    covering_data.sort_by(|a, b| a.0.cmp(&b.0));

    if lights_data.is_empty() && covering_data.is_empty() {
        return Err(anyhow::anyhow!("No lights or window coverings found in Comelit index"));
    }

    info!("Discovered {} lights, {} window coverings:", lights_data.len(), covering_data.len());
    let mut next_ep: u16 = 2;
    for (id, label, on) in &lights_data {
        info!("  ep{}: light {} ({}) — {}", next_ep, label, id, if *on { "ON" } else { "OFF" });
        next_ep += 1;
    }
    for (id, label, state) in &covering_data {
        info!("  ep{}: window covering {} ({}) — pos={}", next_ep, label, id, state.current_position);
        next_ep += 1;
    }

    // ── 4. Create shared state and wire up observers ──────────────────────────

    let mut light_states: Vec<Arc<LightState>> = Vec::new();
    let mut ep_id: u16 = 2;
    for (id, _, initial_on) in &lights_data {
        let state = Arc::new(LightState::new(ep_id, id.clone(), *initial_on, cmd_tx.clone()));
        state.signal.signal(());
        light_states.push(state);
        ep_id += 1;
    }

    let covering_config = comelit_client_rs::covering::WindowCoveringConfig {
        opening_time: Duration::from_secs(settings.window_covering.opening_time),
        closing_time: Duration::from_secs(settings.window_covering.closing_time),
    };

    let mut covering_states: Vec<Arc<covering::CoveringState>> = Vec::new();
    for (id, _, initial_state) in &covering_data {
        let shared_state = Arc::new(tokio::sync::Mutex::new(*initial_state));
        let handle = comelit_client_rs::covering::spawn_window_covering_worker(
            id.clone(),
            shared_state,
            client.clone(),
            covering_config,
        );
        let state = Arc::new(covering::CoveringState::new(ep_id, id.clone(), *initial_state, handle));
        state.handle.set_sink(Box::new(covering::MatterCoveringSink::new(state.clone()))).await;
        covering_states.push(state);
        ep_id += 1;
    }

    let light_observer = Arc::new(MultiLightObserver { states: light_states.clone() });
    let covering_observer = Arc::new(covering::MultiCoveringObserver { states: covering_states.clone() });

    struct FanOutObserver {
        light: Arc<MultiLightObserver>,
        covering: Arc<covering::MultiCoveringObserver>,
    }

    #[async_trait]
    impl StatusUpdate for FanOutObserver {
        async fn status_update(&self, device: &HomeDeviceData) {
            self.light.status_update(device).await;
            self.covering.status_update(device).await;
        }
    }

    *deferred_slot.write().await = Some(Arc::new(FanOutObserver {
        light: light_observer,
        covering: covering_observer,
    }) as _);

    // ── 5. Subscribe to MQTT push for every discovered device ─────────────────

    for (id, _, _) in &lights_data {
        client.subscribe(id).await?;
    }
    for (id, _, _) in &covering_data {
        client.subscribe(id).await?;
    }

    // ── 6. MQTT command executor ──────────────────────────────────────────────

    let exec_client = client.clone();
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            if let Err(e) = exec_client.toggle_device_status(&cmd.device_id, cmd.on).await {
                error!("MQTT toggle error for {}: {e}", cmd.device_id);
            }
        }
    });

    // ── 7. Matter server (separate thread, futures_lite executor) ─────────────

    let matter_thread = std::thread::Builder::new()
        .name("matter".into())
        .stack_size(600 * 1024)
        .spawn(move || run_matter(light_states, lights_data, covering_states, covering_data))?;

    matter_thread
        .join()
        .map_err(|_| anyhow::anyhow!("Matter thread panicked"))??;

    Ok(())
}

// ── Matter server ─────────────────────────────────────────────────────────────

fn run_matter(
    light_states: Vec<Arc<LightState>>,
    lights_data: Vec<(String, String, bool)>,
    covering_states: Vec<Arc<covering::CoveringState>>,
    covering_data: Vec<(String, String, comelit_client_rs::covering::WindowCoveringState)>,
) -> anyhow::Result<()> {
    let mut matter = Matter::new(&TEST_DEV_DET, TEST_DEV_COMM, &TEST_DEV_ATT, MATTER_PORT);

    let mut kv_buf = [0u8; 4096];
    let mut kv = DirKvBlobStore::new_default();
    futures_lite::future::block_on(matter.load_persist(&mut kv, &mut kv_buf))
        .map_err(|e| anyhow::anyhow!("persist load: {e:?}"))?;

    let buffers = PooledBuffers::<10, _>::new(0);
    let subscriptions: Subscriptions = Subscriptions::new();
    let crypto = default_crypto(rand::thread_rng(), DAC_PRIVKEY);
    let mut rand = crypto.rand().map_err(|e| anyhow::anyhow!("rand: {e:?}"))?;

    // Build one BridgedEntry per light and per window covering.
    let mut entries: Vec<BridgedEntry> = Vec::new();
    for (state, (device_id, label, _)) in light_states.into_iter().zip(lights_data.iter()) {
        let ep_id = state.ep_id;
        let hooks = ComelitOnOffHooks::new(state);
        entries.push(BridgedEntry::Light(LightEntry {
            ep_id,
            on_off: on_off::OnOffHandler::new_standalone(
                Dataver::new_rand(&mut rand),
                ep_id,
                hooks,
            ),
            desc: desc::DescHandler::new(Dataver::new_rand(&mut rand)),
            groups: groups::GroupsHandler::new(Dataver::new_rand(&mut rand)),
            bridged: BridgedInfo::new(
                Dataver::new_rand(&mut rand),
                label.clone(),
                device_id.clone(),
            ),
        }));
    }
    for (state, (device_id, label, _)) in covering_states.into_iter().zip(covering_data.iter()) {
        let ep_id = state.ep_id;
        entries.push(BridgedEntry::WindowCovering(CoveringEntry {
            ep_id,
            window_covering: ComelitCoveringHandler::new(Dataver::new_rand(&mut rand), state),
            desc: desc::DescHandler::new(Dataver::new_rand(&mut rand)),
            groups: groups::GroupsHandler::new(Dataver::new_rand(&mut rand)),
            bridged: BridgedInfo::new(
                Dataver::new_rand(&mut rand),
                label.clone(),
                device_id.clone(),
            ),
        }));
    }

    let agg_desc = desc::DescHandler::new_aggregator(Dataver::new_rand(&mut rand));
    let metadata = BridgeMetadata::new(&entries);
    let bridge = ComelitBridgeHandler::new(agg_desc, entries);

    let events = NoEvents::new();
    let dm = DataModel::new(
        &matter,
        &crypto,
        &buffers,
        &subscriptions,
        &events,
        dm_handler(rand, &metadata, &bridge),
        SharedKvBlobStore::new(kv, kv_buf.as_mut_slice()),
        SharedNetworks::new(EthNetwork::new_default()),
    );

    let responder = DefaultResponder::new(&dm);
    let socket = Async::<UdpSocket>::bind(MATTER_SOCKET_BIND_ADDR)
        .map_err(|e| anyhow::anyhow!("socket bind: {e}"))?;

    if !matter.is_commissioned() {
        matter
            .print_standard_qr_text(DiscoveryCapabilities::IP)
            .map_err(|e| anyhow::anyhow!("qr: {e:?}"))?;
        matter
            .print_standard_qr_code(QrTextType::Unicode, DiscoveryCapabilities::IP)
            .map_err(|e| anyhow::anyhow!("qr code: {e:?}"))?;
        matter
            .open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS, &crypto, &())
            .map_err(|e| anyhow::anyhow!("comm window: {e:?}"))?;
    }

    let mut mdns_fut = pin!(mdns::run_mdns(&matter));
    let mut transport = pin!(matter.run(&crypto, &socket, &socket, &socket));
    let mut respond = pin!(responder.run::<4, 4>());
    let mut dm_job = pin!(dm.run());

    futures_lite::future::block_on(
        select4(&mut transport, &mut mdns_fut, &mut respond, &mut dm_job).coalesce(),
    )
    .map_err(|e| anyhow::anyhow!("matter runtime error: {e:?}"))
}

fn dm_handler<'a>(
    rand: impl rand::RngCore + Copy,
    metadata: &'a BridgeMetadata,
    bridge: &'a ComelitBridgeHandler,
) -> impl DataModelHandler + 'a {
    (
        metadata,
        endpoints::EthSysHandlerBuilder::new()
            .netif_diag(&SysNetifs)
            .build(rand)
            .chain(NonRootMatcher, bridge),
    )
}
