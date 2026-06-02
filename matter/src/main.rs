mod light;
mod mdns;

use core::pin::pin;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

use async_io::Async;
use clap::Parser;
use embassy_futures::select::select4;
use log::{error, info};

use rs_matter::crypto::{Crypto, default_crypto};
use rs_matter::dm::clusters::app::on_off::{self, NoLevelControl, OnOffHooks};
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::clusters::groups::{self, ClusterHandler as _};
use rs_matter::dm::clusters::net_comm::SharedNetworks;
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter::dm::devices::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter::dm::endpoints;
use rs_matter::dm::events::NoEvents;
use rs_matter::dm::networks::eth::EthNetwork;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::subscriptions::Subscriptions;
use rs_matter::dm::{Async as DmAsync, DataModel, DataModelHandler, Dataver, Endpoint, EpClMatcher, Node};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::pairing::qr::QrTextType;
use rs_matter::persist::{DirKvBlobStore, SharedKvBlobStore};
use rs_matter::respond::DefaultResponder;
use rs_matter::sc::pase::MAX_COMM_WINDOW_TIMEOUT_SECS;
use rs_matter::transport::MATTER_SOCKET_BIND_ADDR;
use rs_matter::utils::select::Coalesce;
use rs_matter::utils::storage::pooled::PooledBuffers;
use rs_matter::{Matter, MATTER_PORT, clusters, devices, root_endpoint};

use comelit_client_rs::{
    ComelitClient, ComelitOptionsBuilder, DeviceStatus, HomeDeviceData, State, get_secrets,
};
use tokio::sync::mpsc;

use light::{ComelitOnOffHooks, LightState, MatterObserver, MqttCommand};

#[derive(Parser, Debug)]
#[command(name = "comelit-matter", about = "Comelit → Matter bridge PoC (lights only)")]
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

    /// Comelit device ID of the light to expose (e.g. "DOM#LT#1.1")
    #[arg(long, env = "COMELIT_LIGHT_ID")]
    light_id: String,

    /// Action rate limit in milliseconds
    #[arg(long, default_value = "1000")]
    rate_limit_ms: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    // ── 1. Shared state (created before the client, so the observer can borrow it) ──

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<MqttCommand>();
    let light_state = Arc::new(LightState::new(args.light_id.clone(), false, cmd_tx));

    let observer = Arc::new(MatterObserver {
        state: light_state.clone(),
    });

    // ── 2. Create and connect the Comelit client ──────────────────────────────

    let (mqtt_user, mqtt_password) = get_secrets();
    let options = ComelitOptionsBuilder::default()
        .host(Some(args.host.clone()))
        .port(None)
        .mqtt_user(mqtt_user)
        .mqtt_password(mqtt_password)
        .user(Some(args.user.clone()))
        .password(Some(args.password.clone()))
        .action_rate_limit(Duration::from_millis(args.rate_limit_ms))
        .build()?;

    let client = ComelitClient::new(options, Some(observer as _)).await?;
    client.login(State::Disconnected).await?;

    // Fetch index to get the initial on/off state.
    let index = client.fetch_index(2).await?;
    let light_data = index
        .get(&args.light_id)
        .and_then(|d| {
            if let HomeDeviceData::Light(l) = d.value() {
                Some(l.clone())
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow::anyhow!("Light '{}' not found in Comelit index", args.light_id))?;

    let initial_on = light_data
        .status
        .as_ref()
        .map(|s| s == &DeviceStatus::On)
        .unwrap_or(false);

    info!(
        "Found light '{}' (id={}), initially {}",
        light_data.description.as_deref().unwrap_or("?"),
        light_data.id,
        if initial_on { "ON" } else { "OFF" }
    );

    // Now that we know the real initial state, update the shared atom.
    light_state.on.store(initial_on, std::sync::atomic::Ordering::Relaxed);
    // Prime the signal so the Matter run() loop fires an immediate Update
    // notification when it first starts, pushing the correct initial state
    // to any already-subscribed HomeKit controllers.
    light_state.signal.signal(());

    // Subscribe to push updates for this device.
    client.subscribe(&args.light_id).await?;

    // ── 3. MQTT command executor ──────────────────────────────────────────────

    let exec_client = client.clone();
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            if let Err(e) = exec_client.toggle_device_status(&cmd.device_id, cmd.on).await {
                error!("MQTT toggle error for {}: {e}", cmd.device_id);
            }
        }
    });

    // ── 4. Matter server (separate thread, futures_lite executor) ─────────────
    //
    // The built-in mDNS responder and its UDP socket use async-io, which must
    // run on the async-io reactor (a background OS thread).  Keeping the whole
    // Matter stack on futures_lite::block_on ensures it stays on that reactor.

    let matter_state = light_state.clone();
    let matter_thread = std::thread::Builder::new()
        .name("matter".into())
        .stack_size(600 * 1024)
        .spawn(move || run_matter(matter_state))?;

    matter_thread
        .join()
        .map_err(|_| anyhow::anyhow!("Matter thread panicked"))??;

    Ok(())
}

// ── Matter server (runs under futures_lite::block_on) ─────────────────────────

fn run_matter(state: Arc<LightState>) -> anyhow::Result<()> {
    let hooks = ComelitOnOffHooks::new(state);

    let mut matter = Matter::new(&TEST_DEV_DET, TEST_DEV_COMM, &TEST_DEV_ATT, MATTER_PORT);

    let mut kv_buf = [0u8; 4096];
    let mut kv = DirKvBlobStore::new_default();
    futures_lite::future::block_on(matter.load_persist(&mut kv, &mut kv_buf))
        .map_err(|e| anyhow::anyhow!("persist load: {e:?}"))?;

    let buffers = PooledBuffers::<10, _>::new(0);
    let subscriptions: Subscriptions = Subscriptions::new();
    let crypto = default_crypto(rand::thread_rng(), DAC_PRIVKEY);
    let mut rand = crypto.rand().map_err(|e| anyhow::anyhow!("rand: {e:?}"))?;

    let on_off_handler = on_off::OnOffHandler::<_, NoLevelControl>::new_standalone(
        Dataver::new_rand(&mut rand),
        1,
        hooks,
    );

    let events = NoEvents::new();
    let dm = DataModel::new(
        &matter,
        &crypto,
        &buffers,
        &subscriptions,
        &events,
        dm_handler(rand, &on_off_handler),
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

// ── Static node topology ───────────────────────────────────────────────────────

const NODE: Node<'static> = Node {
    endpoints: &[
        root_endpoint!(eth),
        Endpoint::new(
            1,
            devices!(DEV_TYPE_ON_OFF_LIGHT),
            clusters!(
                desc::DescHandler::CLUSTER,
                groups::GroupsHandler::CLUSTER,
                // Access via the OnOffHooks trait (imported above)
                ComelitOnOffHooks::CLUSTER
            ),
        ),
    ],
};

fn dm_handler<'a>(
    mut rand: impl rand::RngCore + Copy,
    on_off: &'a on_off::OnOffHandler<'a, ComelitOnOffHooks, NoLevelControl>,
) -> impl DataModelHandler + 'a {
    (
        NODE,
        endpoints::EthSysHandlerBuilder::new()
            .netif_diag(&SysNetifs)
            .build(rand)
            .chain(
                EpClMatcher::new(Some(1), Some(desc::DescHandler::CLUSTER.id)),
                DmAsync(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                EpClMatcher::new(Some(1), Some(groups::GroupsHandler::CLUSTER.id)),
                DmAsync(groups::GroupsHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                EpClMatcher::new(Some(1), Some(ComelitOnOffHooks::CLUSTER.id)),
                on_off::HandlerAsyncAdaptor(on_off),
            ),
    )
}
