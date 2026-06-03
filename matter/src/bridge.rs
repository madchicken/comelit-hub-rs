use std::pin::Pin;

use embassy_futures::select::select;
use rs_matter::dm::clusters::app::on_off::{self, ClusterAsyncHandler as _, NoLevelControl, OnOffHooks as _};
use rs_matter::dm::clusters::decl::bridged_device_basic_information::{
    self, ClusterHandler as BridgedCH, KeepActiveRequest,
};
use rs_matter::dm::clusters::desc::{self, ClusterHandler as DescCH};
use rs_matter::dm::clusters::groups::{self, ClusterHandler as GroupsCH};
use rs_matter::dm::devices::{DEV_TYPE_AGGREGATOR, DEV_TYPE_BRIDGED_NODE, DEV_TYPE_ON_OFF_LIGHT};
use rs_matter::dm::{
    AsyncHandler, Async as DmAsync, Cluster, Dataver, DeviceType, Endpoint, HandlerContext,
    InvokeContext, InvokeReply, MatchContext, Matcher, Metadata, Node, ReadContext, ReadReply,
    WriteContext,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{TLVBuilderParent, Utf8StrBuilder};
use rs_matter::utils::select::Coalesce;
use rs_matter::{root_endpoint, with};

use crate::light::ComelitOnOffHooks;

// ── Module-level statics for 'static endpoint data ───────────────────────────

const ROOT_EP: Endpoint<'static> = root_endpoint!(eth);

static AGG_DEVICE_TYPES: [DeviceType; 1] = [DEV_TYPE_AGGREGATOR];
static AGG_CLUSTERS: [Cluster<'static>; 1] = [desc::DescHandler::CLUSTER];

static LIGHT_DEVICE_TYPES: [DeviceType; 2] = [DEV_TYPE_ON_OFF_LIGHT, DEV_TYPE_BRIDGED_NODE];
static LIGHT_CLUSTERS: [Cluster<'static>; 4] = [
    desc::DescHandler::CLUSTER,
    groups::GroupsHandler::CLUSTER,
    <BridgedInfo as BridgedCH>::CLUSTER,
    ComelitOnOffHooks::CLUSTER,
];

// ── BridgedInfo ───────────────────────────────────────────────────────────────

/// Implements the Bridged Device Basic Information cluster for a single bridged light.
pub struct BridgedInfo {
    dataver: Dataver,
    label: String,
    unique_id: String,
}

impl BridgedInfo {
    pub fn new(dataver: Dataver, label: String, unique_id: String) -> Self {
        Self { dataver, label, unique_id }
    }
}

impl BridgedCH for BridgedInfo {
    const CLUSTER: Cluster<'static> = bridged_device_basic_information::FULL_CLUSTER
        .with_features(0)
        .with_attrs(with!(required; bridged_device_basic_information::AttributeId::NodeLabel))
        .with_cmds(with!());

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn node_label<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        builder.set(&self.label)
    }

    fn reachable(&self, _ctx: impl ReadContext) -> Result<bool, Error> {
        Ok(true)
    }

    fn unique_id<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        builder.set(&self.unique_id)
    }

    fn handle_keep_active(
        &self,
        _ctx: impl InvokeContext,
        _request: KeepActiveRequest<'_>,
    ) -> Result<(), Error> {
        Ok(())
    }
}

// ── LightEntry ────────────────────────────────────────────────────────────────

/// All handlers and shared state for a single bridged light endpoint.
pub struct LightEntry {
    pub ep_id: u16,
    pub on_off: on_off::OnOffHandler<'static, ComelitOnOffHooks, NoLevelControl>,
    pub desc: desc::DescHandler<'static>,
    pub groups: groups::GroupsHandler,
    pub bridged: BridgedInfo,
}

// ── ComelitBridgeHandler ──────────────────────────────────────────────────────

/// Handles all non-root endpoints: aggregator (ep1) and bridged lights (ep2+).
pub struct ComelitBridgeHandler {
    agg_desc: desc::DescHandler<'static>,
    lights: Vec<LightEntry>,
}

impl ComelitBridgeHandler {
    pub fn new(agg_desc: desc::DescHandler<'static>, lights: Vec<LightEntry>) -> Self {
        Self { agg_desc, lights }
    }

    /// Recursive balanced select tree over boxed futures.
    fn select_all<'a>(
        mut futs: Vec<Pin<Box<dyn core::future::Future<Output = Result<(), Error>> + 'a>>>,
    ) -> Pin<Box<dyn core::future::Future<Output = Result<(), Error>> + 'a>> {
        match futs.len() {
            0 => Box::pin(core::future::pending()),
            1 => futs.pop().unwrap(),
            _ => {
                let mid = futs.len() / 2;
                let right = Self::select_all(futs.split_off(mid));
                let left = Self::select_all(futs);
                Box::pin(async move { select(left, right).coalesce().await })
            }
        }
    }
}

impl AsyncHandler for ComelitBridgeHandler {
    async fn read(
        &self,
        ctx: impl ReadContext,
        reply: impl ReadReply,
    ) -> Result<(), Error> {
        let ep_id = ctx.attr().endpoint_id;
        let cluster_id = ctx.attr().cluster_id;

        if ep_id == 1 {
            DmAsync(desc::HandlerAdaptor(&self.agg_desc)).read(ctx, reply).await
        } else if let Some(light) = self.lights.iter().find(|l| l.ep_id == ep_id) {
            match cluster_id {
                c if c == desc::DescHandler::CLUSTER.id =>
                    DmAsync(desc::HandlerAdaptor(&light.desc)).read(ctx, reply).await,
                c if c == groups::GroupsHandler::CLUSTER.id =>
                    DmAsync(groups::HandlerAdaptor(&light.groups)).read(ctx, reply).await,
                c if c == bridged_device_basic_information::FULL_CLUSTER.id =>
                    DmAsync(bridged_device_basic_information::HandlerAdaptor(&light.bridged)).read(ctx, reply).await,
                c if c == ComelitOnOffHooks::CLUSTER.id =>
                    on_off::HandlerAsyncAdaptor(&light.on_off).read(ctx, reply).await,
                _ => Err(ErrorCode::ClusterNotFound.into()),
            }
        } else {
            Err(ErrorCode::EndpointNotFound.into())
        }
    }

    async fn write(&self, ctx: impl WriteContext) -> Result<(), Error> {
        let ep_id = ctx.attr().endpoint_id;
        let cluster_id = ctx.attr().cluster_id;

        if let Some(light) = self.lights.iter().find(|l| l.ep_id == ep_id) {
            match cluster_id {
                c if c == ComelitOnOffHooks::CLUSTER.id =>
                    on_off::HandlerAsyncAdaptor(&light.on_off).write(ctx).await,
                _ => Err(ErrorCode::AttributeNotFound.into()),
            }
        } else {
            Err(ErrorCode::EndpointNotFound.into())
        }
    }

    async fn invoke(
        &self,
        ctx: impl InvokeContext,
        reply: impl InvokeReply,
    ) -> Result<(), Error> {
        let ep_id = ctx.cmd().endpoint_id;
        let cluster_id = ctx.cmd().cluster_id;

        if let Some(light) = self.lights.iter().find(|l| l.ep_id == ep_id) {
            match cluster_id {
                c if c == ComelitOnOffHooks::CLUSTER.id =>
                    on_off::HandlerAsyncAdaptor(&light.on_off).invoke(ctx, reply).await,
                _ => Err(ErrorCode::CommandNotFound.into()),
            }
        } else {
            Err(ErrorCode::EndpointNotFound.into())
        }
    }

    fn bump_dataver(&self, ctx: impl MatchContext) {
        let ep = ctx.endpt();
        let cl = ctx.cluster();

        if ep.map(|e| e == 1).unwrap_or(true) {
            if cl.map(|c| c == desc::DescHandler::CLUSTER.id).unwrap_or(true) {
                DescCH::dataver_changed(&self.agg_desc);
            }
        }

        for light in &self.lights {
            if ep.map(|e| e == light.ep_id).unwrap_or(true) {
                if cl.map(|c| c == desc::DescHandler::CLUSTER.id).unwrap_or(true) {
                    DescCH::dataver_changed(&light.desc);
                }
                if cl.map(|c| c == groups::GroupsHandler::CLUSTER.id).unwrap_or(true) {
                    GroupsCH::dataver_changed(&light.groups);
                }
                if cl.map(|c| c == bridged_device_basic_information::FULL_CLUSTER.id).unwrap_or(true) {
                    BridgedCH::dataver_changed(&light.bridged);
                }
                if cl.map(|c| c == ComelitOnOffHooks::CLUSTER.id).unwrap_or(true) {
                    on_off::HandlerAsyncAdaptor(&light.on_off).bump_dataver(&ctx);
                }
            }
        }
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        type DynFut<'a> = Pin<Box<dyn core::future::Future<Output = Result<(), Error>> + 'a>>;
        let futs: Vec<DynFut<'_>> = self
            .lights
            .iter()
            .map(|light| -> DynFut<'_> { Box::pin(light.on_off.run(&ctx)) })
            .collect();

        if futs.is_empty() {
            core::future::pending::<Result<(), Error>>().await
        } else {
            Self::select_all(futs).await
        }
    }
}

// ── NonRootMatcher ────────────────────────────────────────────────────────────

/// Matches any endpoint >= 1 (excludes root endpoint 0).
pub struct NonRootMatcher;

impl Matcher for NonRootMatcher {
    fn matches(&self, ctx: impl MatchContext) -> bool {
        ctx.endpt().map(|e| e >= 1).unwrap_or(true)
    }
}

// ── BridgeMetadata ────────────────────────────────────────────────────────────

/// `Metadata` backed by a runtime-built endpoint list.
pub struct BridgeMetadata {
    endpoints: Vec<Endpoint<'static>>,
}

impl BridgeMetadata {
    pub fn new(lights: &[LightEntry]) -> Self {
        let mut endpoints = vec![
            ROOT_EP,
            Endpoint::new(1, &AGG_DEVICE_TYPES, &AGG_CLUSTERS),
        ];
        for light in lights {
            endpoints.push(Endpoint::new(light.ep_id, &LIGHT_DEVICE_TYPES, &LIGHT_CLUSTERS));
        }
        Self { endpoints }
    }
}

impl Metadata for BridgeMetadata {
    fn access<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Node<'_>) -> R,
    {
        f(&Node { endpoints: &self.endpoints })
    }
}
