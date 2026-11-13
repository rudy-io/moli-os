//! Native IP Matter controller. Fabric private keys live only in the encrypted vault.
mod features;
mod pairing;
mod store;
use matter_codec::Tag;
use matter_controller::{
    AttestationTrust, AttributePath, CommandPath, FabricConfig, ImStatus, InvokeResult,
    MatterController, MatterTime, Node, ReadPath, SubscriptionEvent, Value as Tlv,
};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::mpsc, task::JoinSet};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub paa_roots: PathBuf,
    pub cd_roots: PathBuf,
    /// Stable listening port for hosts with an inbound firewall; zero is ephemeral.
    #[serde(default)]
    pub udp_port: u16,
}
#[derive(Debug)]
pub struct Matter {
    config: Config,
}
impl Matter {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}
impl Driver for Matter {
    fn kind(&self) -> &'static str {
        "matter"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.run(ctx))
    }
}
fn failure() -> anyhow::Error {
    anyhow::anyhow!(moli_i18n::tr!("pilotes.matter.failed"))
}
const CLUSTERS: &[(u32, &str)] = &[
    (6, "on"),
    (0x402, "temperature"),
    (0x405, "humidity"),
    (0x406, "occupancy"),
    (0x45, "contact"),
];
fn mapped(
    path: AttributePath,
    value: &Tlv,
    boolean: Option<&'static str>,
) -> Option<(&'static str, Value)> {
    if path.attribute != 0 {
        return None;
    }
    match (path.cluster, value) {
        (6, Tlv::Bool(b)) => Some(("on", Value::Bool(*b))),
        (0x406, Tlv::Uint(n)) if *n <= 255 => Some(("occupancy", Value::Bool(n & 1 != 0))),
        (0x402, Tlv::Int(n)) if (-27315..=32767).contains(n) => Some((
            "temperature",
            Value::Float(f64::from(i32::try_from(*n).ok()?) / 100.0),
        )),
        (0x402, Tlv::Uint(n)) if *n <= 32767 => Some((
            "temperature",
            Value::Float(f64::from(i32::try_from(*n).ok()?) / 100.0),
        )),
        (0x405, Tlv::Uint(n)) if *n <= 10000 => Some((
            "humidity",
            Value::Float(f64::from(i32::try_from(*n).ok()?) / 100.0),
        )),
        (0x45, Tlv::Bool(b)) => Some((boolean?, Value::Bool(*b))),
        (0x45, _) => Some((boolean?, Value::Null)),
        (0x402, _) => Some(("temperature", Value::Null)),
        (0x405, _) => Some(("humidity", Value::Null)),
        (0x406, _) => Some(("occupancy", Value::Null)),
        _ => None,
    }
}
#[derive(Debug, Clone, PartialEq)]
struct Endpoint {
    clusters: Vec<u32>,
    family_clusters: Vec<u32>,
    label: Option<String>,
    boolean: Option<&'static str>,
    bridged: bool,
    reachable: bool,
    connected: bool,
    extra: features::Expanded,
    // Discovery baseline excludes samples accumulated by `state_updates`.
    discovered_extra: features::Expanded,
    extra_keys: BTreeSet<String>,
}
impl Endpoint {
    fn points(&self) -> Vec<PointSpec> {
        self.clusters
            .iter()
            .filter(|c| CLUSTERS.iter().any(|(known, _)| known == *c))
            .map(|c| point(*c, self.boolean))
            .filter(|p| !self.extra_keys.contains(p.key.as_ref()))
            .chain(self.extra.points())
            .collect()
    }
    fn plan(&self, key: &str, value: &Value, abandoned: bool) -> Option<features::Plan> {
        if !self.online()
            || abandoned
            || !self
                .points()
                .iter()
                .any(|p| p.key.as_ref() == key && p.access.write)
        {
            return None;
        }
        // Brightness zero means off: clamping the level would otherwise leave the
        // lamp at its declared minimum instead of dark.
        if key == "brightness" && value.as_f64() == Some(0.0) && self.clusters.contains(&6) {
            return Some(features::Plan::Invoke {
                cluster: 6,
                command: 0,
                fields: Tlv::Structure(vec![]),
                timed: false,
            });
        }
        on_command(key, value, true, &self.clusters, false)
            .map(|command| features::Plan::Invoke {
                cluster: 6,
                command,
                fields: Tlv::Structure(vec![]),
                timed: false,
            })
            .or_else(|| self.extra.plan(key, value))
    }
    fn state_updates(&mut self, path: AttributePath, value: &Tlv) -> Vec<(&'static str, Value)> {
        let mut updates = self.extra.update(path, value);
        if let Some((key, value)) = mapped(path, value, self.boolean)
            && !self.extra_keys.contains(key)
        {
            updates.push((key, value));
        }
        updates
    }
    fn set_connection(&mut self, online: bool) {
        self.connected = online;
        if !online {
            self.reachable = false;
        }
    }
    fn set_reachable(&mut self, value: &Tlv) {
        self.reachable = value == &Tlv::Bool(true);
    }
    fn online(&self) -> bool {
        self.connected && (!self.bridged || self.reachable)
    }
}
fn endpoints(report: &[(AttributePath, Tlv)]) -> BTreeMap<u16, Endpoint> {
    let mut found = BTreeMap::new();
    for (p, v) in report {
        if p.endpoint == 0 || p.cluster != 0x1d || p.attribute != 1 {
            continue;
        }
        let Tlv::Array(list) = v else {
            continue;
        };
        if list.len() > 128 {
            continue;
        }
        let types = report
            .iter()
            .find(|(q, _)| q.endpoint == p.endpoint && q.cluster == 0x1d && q.attribute == 0);
        let Some((_, Tlv::Array(types))) = types else {
            continue;
        };
        if types.len() > 32 {
            continue;
        }
        let mut ids = Vec::new();
        for t in types {
            let Tlv::Structure(fields) = t else {
                continue;
            };
            if fields.len() > 8 {
                continue;
            }
            if let Some((_, Tlv::Uint(id))) = fields.iter().find(|(tag, _)| *tag == Tag::Context(0))
            {
                ids.push(*id);
            }
        }
        // Root and aggregator endpoints are containers, never duplicate child devices.
        if ids.contains(&0x16) || ids.contains(&0xe) {
            continue;
        }
        let boolean = match (ids.contains(&0x15), ids.contains(&0x43)) {
            (true, false) => Some("contact"),
            (false, true) => Some("leak"),
            _ => None,
        };
        let clusters: Vec<_> = CLUSTERS
            .iter()
            .filter_map(|(c, _)| {
                (list.contains(&Tlv::Uint(u64::from(*c))) && (*c != 0x45 || boolean.is_some()))
                    .then_some(*c)
            })
            .collect();
        let extra = features::Expanded::discover(p.endpoint, report);
        let extra_keys = extra
            .points()
            .iter()
            .map(|p| p.key.to_string())
            .collect::<BTreeSet<_>>();
        if clusters.is_empty() && extra_keys.is_empty() {
            continue;
        }
        let family_clusters = list
            .iter()
            .filter_map(features::uint)
            .filter_map(|c| u32::try_from(c).ok())
            .filter(|c| features::supported_cluster(*c))
            .collect();
        if found.len() == 128 && !found.contains_key(&p.endpoint) {
            continue;
        }
        found.insert(
            p.endpoint,
            Endpoint {
                clusters,
                family_clusters,
                label: endpoint_label(report, p.endpoint),
                boolean,
                bridged: ids.contains(&0x13) || list.contains(&Tlv::Uint(0x39)),
                reachable: false,
                connected: false,
                discovered_extra: extra.clone(),
                extra,
                extra_keys,
            },
        );
    }
    found
}
fn endpoint_label(report: &[(AttributePath, Tlv)], endpoint: u16) -> Option<String> {
    report.iter().find_map(|(path, value)| {
        if path.endpoint == endpoint
            && path.cluster == 0x39
            && path.attribute == 5
            && let Tlv::Utf8(label) = value
            && !label.trim().is_empty()
            && label.len() <= 128
        {
            return Some(label.clone());
        }
        None
    })
}
fn point(cluster: u32, boolean: Option<&'static str>) -> PointSpec {
    let key = if cluster == 0x45 {
        boolean.expect("typed boolean")
    } else {
        CLUSTERS
            .iter()
            .find(|(c, _)| *c == cluster)
            .expect("supported cluster")
            .1
    };
    let numeric = cluster == 0x402 || cluster == 0x405;
    PointSpec {
        key: key.into(),
        label: match cluster {
            6 => moli_i18n::tr!("pilotes.matter.on"),
            0x402 => moli_i18n::tr!("pilotes.matter.temperature"),
            0x405 => moli_i18n::tr!("pilotes.matter.humidity"),
            0x406 => moli_i18n::tr!("pilotes.matter.occupancy"),
            0x45 if boolean == Some("contact") => moli_i18n::tr!("pilotes.matter.contact"),
            0x45 => moli_i18n::tr!("pilotes.matter.leak"),
            _ => unreachable!("supported cluster"),
        }
        .into(),
        kind: if numeric {
            Kind::Numeric {
                min: None,
                max: None,
                step: Some(0.01),
            }
        } else {
            Kind::Binary
        },
        access: Access {
            read: true,
            write: cluster == 6,
        },
        unit: match cluster {
            0x402 => Unit::parse("°C"),
            0x405 => Some(Unit::Percent),
            _ => None,
        },
        semantic: if boolean == Some("leak") && cluster == 0x45 {
            Semantic::WaterLeak
        } else if cluster == 6 {
            Semantic::OnOff
        } else {
            Semantic::infer(key, None)
        },
    }
}
fn native(node: u64, endpoint: u16) -> String {
    format!("{node:016x}-{endpoint}")
}
fn on_command(
    key: &str,
    value: &Value,
    online: bool,
    clusters: &[u32],
    abandoned: bool,
) -> Option<u32> {
    match value {
        Value::Bool(on) if key == "on" && online && clusters.contains(&6) && !abandoned => {
            Some(u32::from(*on))
        }
        _ => None,
    }
}
fn connection(event: &SubscriptionEvent) -> Option<bool> {
    match event {
        SubscriptionEvent::Established { .. } => Some(true),
        SubscriptionEvent::Resubscribing { .. } | SubscriptionEvent::Lagged { .. } => Some(false),
        _ => None,
    }
}
enum Update {
    Discovered(u64, BTreeMap<u16, Endpoint>),
    State(u64, AttributePath, Tlv),
    Online(u64, bool),
}
// Keep runtime state for surviving endpoints; upsert handles obsolete point readings.
fn rediscover(
    node: u64,
    endpoints: BTreeMap<u16, Endpoint>,
    devices: &mut BTreeMap<DeviceId, (u64, u16, Endpoint)>,
    device_id: impl Fn(u16) -> DeviceId,
) -> Vec<DeviceId> {
    let removed: Vec<_> = devices
        .iter()
        .filter(|(_, (n, ep, _))| *n == node && !endpoints.contains_key(ep))
        .map(|(id, _)| id.clone())
        .collect();
    for id in &removed {
        devices.remove(id);
    }
    for (ep, mut meta) in endpoints {
        let id = device_id(ep);
        if let Some((_, _, previous)) = devices.get(&id) {
            meta.connected = previous.connected;
            // A newly bridged endpoint must obtain its own Reachable report.
            meta.reachable = previous.bridged && previous.reachable;
            if meta.discovered_extra == previous.discovered_extra {
                meta.extra = previous.extra.clone();
            }
        }
        devices.insert(id, (node, ep, meta));
    }
    removed
}
async fn watch(node: Node, tx: mpsc::Sender<Update>) {
    let id = node.node_id();
    let mut failures = 0;
    loop {
        let result = watch_once(&node, &tx).await;
        if tx.send(Update::Online(id, false)).await.is_err() {
            return;
        }
        // Bound retries even if a peer repeatedly changes its descriptor. A node
        // with nothing we serve fails every cycle, so the delay grows instead of
        // waking a battery device every 10 s; it resets as soon as one succeeds.
        let wait = if result.is_err() {
            let wait = retry_delay(failures);
            failures += 1;
            wait
        } else {
            failures = 0;
            Duration::from_secs(1)
        };
        tokio::time::sleep(wait).await;
    }
}
fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs(10 << failures.min(4))
}
fn subscription_paths(
    report: &[(AttributePath, Tlv)],
    endpoints: &BTreeMap<u16, Endpoint>,
) -> Vec<ReadPath> {
    endpoints
        .iter()
        .flat_map(|(ep, meta)| {
            meta.clusters
                .iter()
                .filter(|c| CLUSTERS.iter().any(|(known, _)| known == *c))
                .map(|c| ReadPath::concrete(*ep, *c, 0))
                .chain(meta.bridged.then_some(ReadPath::concrete(*ep, 0x39, 0x11)))
                .chain(meta.extra.paths(*ep))
                .chain(features::policy_paths(*ep, report))
                .chain(
                    meta.family_clusters
                        .iter()
                        .filter(|c| features::supported_cluster(**c))
                        .flat_map(|c| {
                            [0xfff9, 0xfffb, 0xfffc, 0xfffd].map(|a| ReadPath::concrete(*ep, *c, a))
                        }),
                )
        })
        .collect()
}
// Attribute reports carry their concrete endpoint. Subscribe once per cluster/
// attribute across the node instead of repeating identical filters per endpoint;
// dispatch still accepts only discovered endpoints and their validated capabilities.
fn compact_subscription_paths(paths: &mut Vec<ReadPath>) {
    for path in paths.iter_mut() {
        path.endpoint = None;
    }
    paths.sort_unstable_by_key(|p| (p.cluster, p.attribute));
    paths.dedup_by_key(|p| (p.cluster, p.attribute));
}
// A discovery batch that fails or times out is fatal for this cycle. Skipping it
// would leave those attributes out of `report`, the subscription returns them at
// startup, `descriptor_changed` reads that as a change, and discovery restarts
// every second while the node's devices blink offline.
fn absorb_batch(
    outcome: Option<anyhow::Result<Vec<(AttributePath, Tlv)>>>,
    report: &mut Vec<(AttributePath, Tlv)>,
) -> anyhow::Result<()> {
    match outcome {
        Some(Ok(values)) => {
            report.extend(values);
            Ok(())
        }
        Some(Err(_)) | None => Err(failure()),
    }
}
async fn watch_once(node: &Node, tx: &mpsc::Sender<Update>) -> anyhow::Result<()> {
    tracing::debug!("Matter descriptor read started");
    let mut report = tokio::time::timeout(
        Duration::from_secs(30),
        node.read(&[
            ReadPath::new(None, Some(0x1d), Some(0)),
            ReadPath::new(None, Some(0x1d), Some(1)),
            ReadPath::new(None, Some(0x39), Some(5)),
        ]),
    )
    .await
    .map_err(|_| failure())?
    .map_err(|_| failure())?;
    tracing::debug!(reports = report.len(), "Matter descriptor read complete");
    let Some(discovery) = features::discovery_paths(&report) else {
        return Err(failure());
    };
    let metadata_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    for paths in discovery.chunks(32) {
        let outcome = tokio::time::timeout_at(metadata_deadline, node.read(paths))
            .await
            .ok()
            .map(|read| read.map_err(|_| failure()));
        absorb_batch(outcome, &mut report)?;
    }
    tracing::debug!(reports = report.len(), "Matter capability read complete");
    let endpoints = endpoints(&report);
    let mut paths = subscription_paths(&report, &endpoints);
    let empty = paths.is_empty();
    tx.send(Update::Discovered(node.node_id(), endpoints))
        .await
        .map_err(|_| failure())?;
    // Publish even an empty snapshot so the last removed endpoint disappears.
    if empty {
        return Err(failure());
    }
    paths.extend([
        ReadPath::new(None, Some(0x1d), Some(0)),
        ReadPath::new(None, Some(0x1d), Some(1)),
        ReadPath::new(None, Some(0x39), Some(5)),
    ]);
    compact_subscription_paths(&mut paths);
    let mut color_modes: BTreeMap<_, _> = report
        .iter()
        .filter(|(p, _)| p.cluster == 0x300 && [8, 0x4001].contains(&p.attribute))
        .map(|(p, v)| ((p.endpoint, p.attribute), v.clone()))
        .collect();
    tracing::debug!(paths = paths.len(), "Matter subscription started");
    let mut sub = tokio::time::timeout(Duration::from_secs(30), node.subscribe(&paths, &[], 1, 60))
        .await
        .map_err(|_| failure())?
        .map_err(|error| {
            tracing::warn!(kinds = %pairing::error_kinds(&format!("{error:?}")), "Matter subscription failed");
            failure()
        })?;
    tracing::debug!("Matter subscription established");
    while let Some(event) = sub.next().await {
        if let SubscriptionEvent::Established { max_interval, .. } = &event {
            tracing::debug!(max_interval, "Matter subscription interval negotiated");
        }
        if let Some(online) = connection(&event) {
            tx.send(Update::Online(node.node_id(), online))
                .await
                .map_err(|_| failure())?;
        }
        let mut refresh = vec![];
        let update = match event {
            SubscriptionEvent::Report(r) => {
                // Initial subscription reports echo discovery; only changes rebuild.
                if descriptor_changed(&report, r.path, &r.value) {
                    return Ok(());
                }
                if r.path.cluster == 0x300 && [8, 0x4001].contains(&r.path.attribute) {
                    let key = (r.path.endpoint, r.path.attribute);
                    if color_modes.get(&key) != Some(&r.value) {
                        refresh = features::color_refresh_paths(r.path, &r.value, &report);
                        color_modes.insert(key, r.value.clone());
                    }
                }
                Update::State(node.node_id(), r.path, r.value)
            }
            SubscriptionEvent::Resubscribing { .. } => return Err(failure()),
            SubscriptionEvent::Lagged { .. } => {
                return Err(failure());
            }
            _ => continue,
        };
        tx.send(update).await.map_err(|_| failure())?;
        if !refresh.is_empty()
            && let Ok(Ok(values)) =
                tokio::time::timeout(Duration::from_secs(5), node.read(&refresh)).await
        {
            for (path, value) in values {
                tx.send(Update::State(node.node_id(), path, value))
                    .await
                    .map_err(|_| failure())?;
            }
        }
    }
    Err(failure())
}
fn descriptor_changed(report: &[(AttributePath, Tlv)], path: AttributePath, value: &Tlv) -> bool {
    ((path.cluster == 0x1d && matches!(path.attribute, 0 | 1))
        || (path.cluster == 0x39 && path.attribute == 5)
        || features::metadata_attribute(path))
        && !report.iter().any(|(p, v)| *p == path && v == value)
}
async fn ensure_fabric(controller: &MatterController) -> anyhow::Result<()> {
    if controller
        .fabrics()
        .await
        .map_err(|_| failure())?
        .is_empty()
    {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        controller
            .create_fabric(FabricConfig::new(
                1,
                1,
                1,
                (
                    MatterTime::from_unix_secs(now.saturating_sub(3600)),
                    MatterTime::NO_EXPIRY,
                ),
            ))
            .await
            .map_err(|_| failure())?;
    }
    Ok(())
}
impl Matter {
    async fn run(&self, ctx: &mut DriverCtx) -> anyhow::Result<()> {
        let secrets = ctx.durable_secrets().ok_or_else(failure)?;
        let config = self.config.clone();
        let trust = tokio::task::spawn_blocking(move || {
            AttestationTrust::from_dirs(&config.paa_roots, &config.cd_roots)
        })
        .await
        .map_err(|_| failure())?
        .map_err(|_| failure())?;
        let store = tokio::task::spawn_blocking(move || store::VaultStore::open(secrets))
            .await
            .map_err(|_| failure())?
            .map_err(|_| failure())?;
        let controller = MatterController::builder(Arc::new(store))
            .attestation_trust(trust)
            .listen_port(self.config.udp_port)
            .build()
            .await
            .map_err(|_| failure())?;
        ensure_fabric(&controller).await?;
        pairing::pair(&controller, ctx).await?;
        let nodes = controller.nodes().await.map_err(|_| failure())?;
        anyhow::ensure!(nodes.len() <= 64, moli_i18n::tr!("pilotes.matter.failed"));
        let (tx, mut rx) = mpsc::channel(128);
        let mut workers = JoinSet::new();
        for n in &nodes {
            workers.spawn(watch(controller.node(n.node_id), tx.clone()));
        }
        drop(tx);
        let mut devices: BTreeMap<DeviceId, (u64, u16, Endpoint)> = BTreeMap::new();
        if nodes.is_empty() {
            ctx.wait_for(moli_i18n::tr!("pilotes.matter.waiting"));
        } else {
            ctx.ready();
        }
        loop {
            tokio::select! {
                _ = workers.join_next(), if !workers.is_empty() => return Err(failure()),
                command = ctx.next_command() => {
                    let Some(mut command) = command else { return Ok(()); };
                    let target = devices.get(&command.device.id);
                    let result = if let Some((node,ep,meta)) = target {
                        if let Some(plan) = meta.plan(&command.key,&command.value,command.abandoned()) {
                            let node = controller.node(*node);
                            let deadline = command.deadline;
                            tokio::select! {
                                biased;
                                () = ctx.cancelled() => return Ok(()),
                                () = command.cancelled() => continue,
                                result = execute_plan(&node,*ep,plan,deadline) => result,
                            }
                        } else { Err(failure().to_string()) }
                    } else { Err(failure().to_string()) };
                    command.reply(result);
                },
                Some(update) = rx.recv() => match update {
                    Update::Discovered(node,endpoints) => {
                        for id in rediscover(node, endpoints, &mut devices, |ep| ctx.device_id(&native(node, ep))) {
                            ctx.remove_device(&id);
                        }
                        for (id, (n, ep, meta)) in &devices {
                            if *n != node { continue; }
                            ctx.upsert_device(Device { id:id.clone(),instance:ctx.instance().clone(),native_name:meta.label.clone().unwrap_or_else(|| format!("Matter {node:016x} / {ep}")).into(),manufacturer:None,model:None,description:None,native_room:None,members:vec![],points:meta.points() });
                            ctx.set_availability(id,meta.online());
                        }
                    },
                    Update::Online(node,online) => for (id,(n,_,meta)) in &mut devices { if *n == node { meta.set_connection(online); ctx.set_availability(id,meta.online()); } },
                    Update::State(node,path,value) => {
                        let id = ctx.device_id(&native(node,path.endpoint));
                        if let Some((_,_,meta)) = devices.get_mut(&id) {
                            if meta.bridged && path.cluster == 0x39 && path.attribute == 0x11 {
                                meta.set_reachable(&value);
                                ctx.set_availability(&id,meta.online());
                            } else if meta.clusters.contains(&path.cluster) || meta.family_clusters.contains(&path.cluster) {
                                for (key,value) in meta.state_updates(path,&value) { ctx.set_state(&id,key,value); }
                            }
                        }
                    }
                }
            }
        }
    }
}
async fn execute_plan(
    node: &Node,
    endpoint: u16,
    plan: features::Plan,
    deadline: std::time::Instant,
) -> Result<(), String> {
    match plan {
        features::Plan::Invoke {
            cluster,
            command,
            fields,
            timed,
        } => {
            let path = CommandPath {
                endpoint,
                cluster,
                command,
            };
            let result = if timed {
                node.invoke_timed_before(path, fields, None, deadline).await
            } else {
                node.invoke_before(path, fields, deadline).await
            };
            order_outcome(&result.map(|r| matches!(r, InvokeResult::Status(ImStatus::Success))))
        }
        features::Plan::Write {
            cluster,
            attribute,
            value,
        } => {
            let path = AttributePath {
                endpoint,
                cluster,
                attribute,
            };
            order_outcome(
                &node
                    .write_before(&[(path, value)], deadline)
                    .await
                    .map(|statuses| {
                        statuses.len() == 1
                            && statuses[0].0 == path
                            && statuses[0].1 == ImStatus::Success
                    }),
            )
        }
    }
}
// A refused or unanswered order is a failure; one whose deadline elapsed after it
// was queued is unknown — the device may have carried it out (a lock, a cover).
fn order_outcome(result: &Result<bool, matter_controller::Error>) -> Result<(), String> {
    match result {
        Ok(true) => Ok(()),
        Err(matter_controller::Error::OutcomeUnknown { .. }) => {
            Err(moli_i18n::tr!("pilotes.matter.outcome_unknown").to_string())
        }
        Ok(false) | Err(_) => Err(failure().to_string()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_subscription_filters_do_not_grow_with_endpoint_count() {
        let mut paths: Vec<_> = (1..=100)
            .flat_map(|ep| {
                [
                    ReadPath::concrete(ep, 6, 0),
                    ReadPath::concrete(ep, 0x39, 0x11),
                ]
            })
            .collect();
        paths.push(ReadPath::new(None, Some(0x1d), Some(0)));
        paths.push(ReadPath::new(None, Some(0x1d), Some(1)));
        compact_subscription_paths(&mut paths);
        assert_eq!(paths.len(), 4);
        assert!(paths.iter().all(|p| p.endpoint.is_none()));
        assert!(
            paths
                .iter()
                .any(|p| p.cluster == Some(0x1d) && p.attribute == Some(0))
        );
        assert!(
            paths
                .iter()
                .any(|p| p.cluster == Some(0x1d) && p.attribute == Some(1))
        );
        assert!(
            paths
                .iter()
                .any(|p| p.cluster == Some(0x39) && p.attribute == Some(0x11))
        );
    }

    #[test]
    fn rediscovery_preserves_survivors_and_replaces_changed_definitions() {
        let report = vec![
            (path(1, 0x1d, 0), Tlv::Array(vec![])),
            (
                path(1, 0x1d, 1),
                Tlv::Array(vec![Tlv::Uint(6), Tlv::Uint(0x300)]),
            ),
            (path(1, 0x300, 0xfffc), Tlv::Uint(1)),
            (path(1, 0x300, 0xfffd), Tlv::Uint(1)),
            (
                path(1, 0x300, 0xfffb),
                Tlv::Array(vec![0, 1, 8, 0x400a].into_iter().map(Tlv::Uint).collect()),
            ),
            (path(1, 0x300, 0xfff9), Tlv::Array(vec![Tlv::Uint(6)])),
            (path(1, 0x300, 0x400a), Tlv::Uint(1)),
        ];
        let id = |node, ep| DeviceId::from(format!("test:{}", native(node, ep)));
        let mut devices = BTreeMap::new();
        assert!(rediscover(1, endpoints(&report), &mut devices, |ep| id(1, ep)).is_empty());
        let meta = &mut devices.get_mut(&id(1, 1)).unwrap().2;
        meta.set_connection(true);
        meta.state_updates(path(1, 0x300, 8), &Tlv::Uint(0));
        meta.state_updates(path(1, 0x300, 0), &Tlv::Uint(50));
        meta.state_updates(path(1, 0x300, 1), &Tlv::Uint(200));
        assert_ne!(meta.extra, meta.discovered_extra);
        let previous = meta.clone();
        // Neither a sibling endpoint nor another node may be removed by replay.
        devices.insert(id(1, 2), (1, 2, previous.clone()));
        devices.insert(id(2, 1), (2, 1, previous.clone()));
        assert_eq!(
            rediscover(1, endpoints(&report), &mut devices, |ep| id(1, ep)),
            vec![id(1, 2)]
        );
        assert_eq!(devices[&id(1, 1)].2, previous);
        assert_eq!(devices[&id(2, 1)].2, previous);
        assert!(rediscover(1, endpoints(&report), &mut devices, |ep| id(1, ep)).is_empty());
        assert_eq!(devices[&id(1, 1)].2, previous);

        let mut changed = report.clone();
        changed[1].1 = Tlv::Array(vec![Tlv::Uint(0x45)]);
        changed[0].1 = Tlv::Array(vec![Tlv::Structure(vec![(
            Tag::Context(0),
            Tlv::Uint(0x43),
        )])]);
        // Remove color metadata as well as the server; add a new identity label.
        changed.retain(|(p, _)| p.cluster != 0x300);
        changed.push((path(1, 0x39, 5), Tlv::Utf8("Renamed endpoint".into())));
        assert!(rediscover(1, endpoints(&changed), &mut devices, |ep| id(1, ep)).is_empty());
        let meta = &devices[&id(1, 1)].2;
        assert!(meta.online());
        assert_eq!(meta.label.as_deref(), Some("Renamed endpoint"));
        assert_eq!(
            meta.points()
                .iter()
                .map(|p| p.key.as_ref())
                .collect::<Vec<_>>(),
            vec!["leak"]
        );
        assert!(meta.plan("on", &Value::Bool(true), false).is_none());
        assert_eq!(meta.extra, features::Expanded::default());
        assert_eq!(
            rediscover(1, BTreeMap::new(), &mut devices, |ep| id(1, ep)),
            vec![id(1, 1)]
        );
        assert_eq!(devices.len(), 1);
        assert!(devices.contains_key(&id(2, 1)));
    }

    #[test]
    fn refusal_and_reconnect() {
        assert_eq!(
            on_command("on", &Value::Bool(true), true, &[6], false),
            Some(1)
        );
        assert_eq!(
            on_command("on", &Value::Bool(false), true, &[6], false),
            Some(0)
        );
        for (key, value, online, clusters, abandoned) in [
            ("on", Value::Bool(true), false, vec![6], false),
            ("on", Value::Bool(true), true, vec![6], true),
            ("toggle", Value::Bool(true), true, vec![6], false),
            ("on", Value::Int(1), true, vec![6], false),
            ("on", Value::Bool(true), true, vec![0x402], false),
        ] {
            assert!(on_command(key, &value, online, &clusters, abandoned).is_none());
        }
        assert_eq!(
            connection(&SubscriptionEvent::Resubscribing {
                cause: matter_controller::Error::Operational("test".into())
            }),
            Some(false)
        );
        assert_eq!(
            connection(&SubscriptionEvent::Lagged { dropped: 1 }),
            Some(false)
        );
    }

    fn path(ep: u16, cluster: u32, attribute: u32) -> AttributePath {
        AttributePath {
            endpoint: ep,
            cluster,
            attribute,
        }
    }
    #[test]
    fn catalogue_descriptor_replay() {
        // Synthetic records: endpoint, device types, servers, exact expected points.
        for line in include_str!("../../../integrations/matter/fixtures/descriptor.csv")
            .lines()
            .filter(|l| !l.starts_with('#'))
        {
            let fields: Vec<_> = line.split(',').collect();
            let ep = fields[0].parse().unwrap();
            let nums = |s: &str| {
                s.split_whitespace()
                    .map(|n| u64::from_str_radix(n, 16).unwrap())
                    .collect::<Vec<_>>()
            };
            let types = nums(fields[1])
                .into_iter()
                .map(|n| {
                    Tlv::Structure(vec![
                        (Tag::Context(0), Tlv::Uint(n)),
                        (Tag::Context(1), Tlv::Uint(1)),
                    ])
                })
                .collect();
            let servers = nums(fields[2]).into_iter().map(Tlv::Uint).collect();
            let report = [
                (path(ep, 0x1d, 0), Tlv::Array(types)),
                (path(ep, 0x1d, 1), Tlv::Array(servers)),
            ];
            let found = endpoints(&report);
            if fields[3] == "ignored" {
                assert!(found.is_empty(), "{line}");
                continue;
            }
            let meta = &found[&ep];
            let points: Vec<_> = meta
                .clusters
                .iter()
                .map(|cluster| point(*cluster, meta.boolean))
                .collect();
            let keys: Vec<_> = points.iter().map(|p| p.key.as_ref()).collect();
            assert_eq!(
                keys,
                fields[3].split_whitespace().collect::<Vec<_>>(),
                "{line}"
            );
            for spec in &points {
                assert!(spec.access.read, "{line}");
                assert_eq!(spec.access.write, spec.key.as_ref() == "on", "{line}");
            }
            // Vendor/product identity is not a capability filter. Both identities
            // must yield the same descriptor mapping (including bridged endpoints).
            for vendor in [0xfff1, 0xfff2] {
                let mut branded = report.to_vec();
                branded.extend([
                    (path(0, 0x28, 2), Tlv::Uint(vendor)),
                    (path(0, 0x28, 4), Tlv::Uint(0x8000)),
                ]);
                assert_eq!(endpoints(&branded), found, "{line}");
            }
            for server in nums(fields[2]) {
                let cluster = u32::try_from(server).unwrap();
                if !meta.clusters.contains(&cluster) {
                    assert_eq!(
                        mapped(path(ep, cluster, 0), &Tlv::Bool(true), meta.boolean),
                        None,
                        "unclaimed server in {line}"
                    );
                }
            }
            assert_eq!(
                on_command("on", &Value::Bool(true), true, &meta.clusters, false),
                keys.contains(&"on").then_some(1),
                "{line}"
            );
            for state in [true, false] {
                if let Some(key) = meta.boolean {
                    assert_eq!(
                        mapped(path(ep, 0x45, 0), &Tlv::Bool(state), meta.boolean),
                        Some((key, Value::Bool(state)))
                    );
                }
            }
            assert_ne!(native(1, ep), native(2, ep));
        }
    }
    #[test]
    fn descriptor_change_and_final_endpoint_removal() {
        let report = vec![
            (
                path(1, 0x1d, 0),
                Tlv::Array(vec![Tlv::Structure(vec![(
                    Tag::Context(0),
                    Tlv::Uint(0x15),
                )])]),
            ),
            (path(1, 0x1d, 1), Tlv::Array(vec![Tlv::Uint(0x45)])),
        ];
        assert_eq!(endpoints(&report)[&1].boolean, Some("contact"));
        for (p, v) in &report {
            assert!(!descriptor_changed(&report, *p, v));
        }
        let mut changed = report.clone();
        changed[0].1 = Tlv::Array(vec![Tlv::Structure(vec![(
            Tag::Context(0),
            Tlv::Uint(0x43),
        )])]);
        assert!(descriptor_changed(&report, changed[0].0, &changed[0].1));
        assert_eq!(endpoints(&changed)[&1].boolean, Some("leak"));
        changed[1].1 = Tlv::Array(vec![]);
        assert!(descriptor_changed(&report, changed[1].0, &changed[1].1));
        assert!(endpoints(&changed).is_empty());
        assert!(descriptor_changed(
            &report,
            path(2, 0x1d, 1),
            &Tlv::Array(vec![])
        ));
        assert!(!descriptor_changed(
            &report,
            path(1, 6, 0),
            &Tlv::Bool(true)
        ));
    }

    #[test]
    fn invalid_samples_clear_and_unknown_boolean_is_ignored() {
        for (cluster, key, valid) in [
            (0x402, "temperature", Tlv::Int(2150)),
            (0x405, "humidity", Tlv::Uint(4500)),
            (0x406, "occupancy", Tlv::Uint(1)),
            (0x45, "contact", Tlv::Bool(true)),
        ] {
            assert!(mapped(path(3, cluster, 0), &valid, Some("contact")).is_some());
            for invalid in [
                Tlv::Null,
                Tlv::Int(-32768),
                Tlv::Uint(65535),
                Tlv::Utf8("invalid".into()),
            ] {
                assert_eq!(
                    mapped(path(3, cluster, 0), &invalid, Some("contact")),
                    Some((key, Value::Null))
                );
            }
        }
        assert_eq!(mapped(path(3, 0x45, 0), &Tlv::Bool(true), None), None);
        assert_eq!(mapped(path(3, 0x402, 1), &Tlv::Null, None), None);
    }
    #[test]
    fn bridged_labels_are_bounded_and_refresh_on_change() {
        let p = path(2, 0x39, 5);
        let report = vec![(p, Tlv::Utf8("Test light".into()))];
        assert_eq!(endpoint_label(&report, 2).as_deref(), Some("Test light"));
        assert_eq!(endpoint_label(&report, 3), None);
        assert!(!descriptor_changed(
            &report,
            p,
            &Tlv::Utf8("Test light".into())
        ));
        assert!(descriptor_changed(&report, p, &Tlv::Utf8("Renamed".into())));
        for value in [Tlv::Null, Tlv::Utf8(" ".into()), Tlv::Utf8("x".repeat(129))] {
            assert_eq!(endpoint_label(&[(p, value)], 2), None);
        }
    }

    #[test]
    fn bridge_availability_blocks_writes_until_primed() {
        let mut meta = Endpoint {
            clusters: vec![6],
            family_clusters: vec![],
            label: None,
            boolean: None,
            bridged: true,
            reachable: false,
            connected: true,
            extra: features::Expanded::default(),
            discovered_extra: features::Expanded::default(),
            extra_keys: BTreeSet::new(),
        };
        assert!(!meta.online());
        assert_eq!(
            on_command(
                "on",
                &Value::Bool(true),
                meta.online(),
                &meta.clusters,
                false
            ),
            None
        );
        meta.set_reachable(&Tlv::Bool(true));
        assert!(meta.online());
        meta.set_connection(false);
        assert!(!meta.online());
        meta.set_connection(true);
        assert!(!meta.online());
        for value in [Tlv::Null, Tlv::Bool(false), Tlv::Uint(1)] {
            meta.set_reachable(&value);
            assert!(!meta.online());
            assert_eq!(
                on_command(
                    "on",
                    &Value::Bool(true),
                    meta.online(),
                    &meta.clusters,
                    false
                ),
                None
            );
        }
    }
    #[test]
    fn an_unanswered_order_reports_an_unknown_state() {
        assert_eq!(
            order_outcome(&Err(matter_controller::Error::OutcomeUnknown {
                node_id: 1
            })),
            Err(moli_i18n::tr!("pilotes.matter.outcome_unknown").to_string())
        );
        assert_eq!(order_outcome(&Ok(false)), Err(failure().to_string()));
        assert!(order_outcome(&Ok(true)).is_ok());
    }
    #[test]
    fn retries_back_off_up_to_a_cap() {
        assert_eq!(retry_delay(0), Duration::from_secs(10));
        assert_eq!(retry_delay(1), Duration::from_secs(20));
        assert_eq!(retry_delay(4), Duration::from_secs(160));
        assert_eq!(retry_delay(9), Duration::from_secs(160));
        assert_eq!(retry_delay(u32::MAX), Duration::from_secs(160));
    }
    #[test]
    fn brightness_zero_switches_off_instead_of_dimming_to_the_floor() {
        let report = vec![
            (path(1, 8, 0xfffc), Tlv::Uint(2)),
            (path(1, 8, 0xfffd), Tlv::Uint(7)),
            (
                path(1, 8, 0xfffb),
                Tlv::Array(vec![Tlv::Uint(0), Tlv::Uint(2), Tlv::Uint(3)]),
            ),
            (path(1, 8, 0xfff9), Tlv::Array(vec![Tlv::Uint(0)])),
            (path(1, 8, 2), Tlv::Uint(1)),
            (path(1, 8, 3), Tlv::Uint(254)),
        ];
        let meta = Endpoint {
            clusters: vec![6, 8],
            family_clusters: vec![],
            label: None,
            boolean: None,
            bridged: false,
            reachable: true,
            connected: true,
            extra: features::Expanded::discover(1, &report),
            discovered_extra: features::Expanded::discover(1, &report),
            extra_keys: BTreeSet::new(),
        };
        // Zero is Off on cluster 6; the level cluster may not accept level 0.
        for zero in [Value::Int(0), Value::Float(0.0)] {
            assert!(matches!(
                meta.plan("brightness", &zero, false),
                Some(features::Plan::Invoke {
                    cluster: 6,
                    command: 0,
                    ..
                })
            ));
        }
        // Any other value stays a MoveToLevel on the level cluster.
        assert!(matches!(
            meta.plan("brightness", &Value::Float(1.0), false),
            Some(features::Plan::Invoke {
                cluster: 8,
                command: 0,
                ..
            })
        ));
    }
    #[test]
    fn discovery_bounds_and_missing_types() {
        let report = [(path(1, 0x1d, 1), Tlv::Array(vec![Tlv::Uint(6)]))];
        assert!(endpoints(&report).is_empty());
        let mut report = report.to_vec();
        report.push((path(1, 0x1d, 0), Tlv::Array(vec![])));
        assert_eq!(endpoints(&report).len(), 1);
        report[0].1 = Tlv::Array(vec![Tlv::Uint(6); 129]);
        assert!(endpoints(&report).is_empty());
        let report: Vec<_> = (1..=129)
            .flat_map(|ep| {
                [
                    (path(ep, 0x1d, 0), Tlv::Array(vec![])),
                    (path(ep, 0x1d, 1), Tlv::Array(vec![Tlv::Uint(6)])),
                ]
            })
            .collect();
        assert_eq!(endpoints(&report).len(), 128);
        // Past the bound the metadata read cannot be complete: refuse the cycle
        // instead of subscribing on a partial report.
        assert!(features::discovery_paths(&report).is_none());
    }

    #[test]
    fn failed_discovery_batch_aborts_instead_of_looping() {
        let mut report = vec![(path(1, 0x1d, 1), Tlv::Array(vec![Tlv::Uint(6)]))];
        let before = report.clone();
        // A timed-out or failed batch must stop this cycle: skipping it leaves the
        // attributes out of `report`, so the subscription's first reports look like
        // descriptor changes and discovery restarts every second.
        assert!(absorb_batch(None, &mut report).is_err());
        assert!(absorb_batch(Some(Err(failure())), &mut report).is_err());
        assert_eq!(report, before);
        let batch = vec![(path(1, 0x2a, 0), Tlv::Uint(1))];
        assert!(absorb_batch(Some(Ok(batch.clone())), &mut report).is_ok());
        assert_eq!(report, [before, batch].concat());
    }
}
