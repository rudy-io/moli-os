//! Native IP Matter controller. Fabric private keys live only in the encrypted vault.
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
    collections::BTreeMap,
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
    /// Opt-in initial Thread provisioning over BLE. The standard image keeps
    /// the Bluetooth/D-Bus dependency disabled.
    #[serde(default)]
    pub ble_thread: bool,
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
    label: Option<String>,
    boolean: Option<&'static str>,
    bridged: bool,
    reachable: bool,
    connected: bool,
}
impl Endpoint {
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
        if clusters.is_empty() {
            continue;
        }
        if found.len() == 128 && !found.contains_key(&p.endpoint) {
            continue;
        }
        found.insert(
            p.endpoint,
            Endpoint {
                clusters,
                label: endpoint_label(report, p.endpoint),
                boolean,
                bridged: ids.contains(&0x13) || list.contains(&Tlv::Uint(0x39)),
                reachable: false,
                connected: false,
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
async fn watch(node: Node, tx: mpsc::Sender<Update>) {
    let id = node.node_id();
    loop {
        let result = watch_once(&node, &tx).await;
        if tx.send(Update::Online(id, false)).await.is_err() {
            return;
        }
        // Bound retries even if a peer repeatedly changes its descriptor.
        tokio::time::sleep(Duration::from_secs(if result.is_err() { 10 } else { 1 })).await;
    }
}
async fn watch_once(node: &Node, tx: &mpsc::Sender<Update>) -> anyhow::Result<()> {
    let report = tokio::time::timeout(
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
    let endpoints = endpoints(&report);
    let mut paths: Vec<_> = endpoints
        .iter()
        .flat_map(|(ep, meta)| {
            meta.clusters
                .iter()
                .map(move |c| ReadPath::concrete(*ep, *c, 0))
                .chain(meta.bridged.then_some(ReadPath::concrete(*ep, 0x39, 0x11)))
        })
        .collect();
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
    let mut sub = tokio::time::timeout(Duration::from_secs(30), node.subscribe(&paths, &[], 1, 60))
        .await
        .map_err(|_| failure())?
        .map_err(|_| failure())?;
    while let Some(event) = sub.next().await {
        if let Some(online) = connection(&event) {
            tx.send(Update::Online(node.node_id(), online))
                .await
                .map_err(|_| failure())?;
        }
        let update = match event {
            SubscriptionEvent::Report(r) => {
                // Initial subscription reports echo discovery; only changes rebuild.
                if descriptor_changed(&report, r.path, &r.value) {
                    return Ok(());
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
    }
    Err(failure())
}
fn descriptor_changed(report: &[(AttributePath, Tlv)], path: AttributePath, value: &Tlv) -> bool {
    ((path.cluster == 0x1d && matches!(path.attribute, 0 | 1))
        || (path.cluster == 0x39 && path.attribute == 5))
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
            .build()
            .await
            .map_err(|_| failure())?;
        ensure_fabric(&controller).await?;
        pairing::pair(&controller, ctx, self.config.ble_thread).await?;
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
                        if let Some(on) = on_command(&command.key,&command.value,meta.online(),&meta.clusters,command.abandoned()) {
                            let node = controller.node(*node);
                            let deadline = command.deadline;
                            tokio::select! {
                                biased;
                                () = ctx.cancelled() => return Ok(()),
                                () = command.cancelled() => continue,
                                result = node.invoke_before(CommandPath { endpoint:*ep,cluster:6,command:on },Tlv::Structure(vec![]),deadline) => match result {
                                    Ok(InvokeResult::Status(ImStatus::Success)) => Ok(()),
                                    _ => Err(failure().to_string()),
                                }
                            }
                        } else { Err(failure().to_string()) }
                    } else { Err(failure().to_string()) };
                    command.reply(result);
                },
                Some(update) = rx.recv() => match update {
                    Update::Discovered(node,endpoints) => {
                        let removed: Vec<_> = devices.iter().filter(|(_, (n,_,_))| *n == node).map(|(id,_)|id.clone()).collect();
                        for id in removed { devices.remove(&id); ctx.remove_device(&id); }
                        for (ep,meta) in endpoints {
                        let id = ctx.device_id(&native(node,ep));
                        ctx.upsert_device(Device { id:id.clone(),instance:ctx.instance().clone(),native_name:meta.label.clone().unwrap_or_else(|| format!("Matter {node:016x} / {ep}")).into(),manufacturer:None,model:None,description:None,native_room:None,members:vec![],points:meta.clusters.iter().map(|c|point(*c,meta.boolean)).collect() });
                        ctx.set_availability(&id,false);
                        devices.insert(id,(node,ep,meta));
                        }
                    },
                    Update::Online(node,online) => for (id,(n,_,meta)) in &mut devices { if *n == node { meta.set_connection(online); ctx.set_availability(id,meta.online()); } },
                    Update::State(node,path,value) => {
                        let id = ctx.device_id(&native(node,path.endpoint));
                        if let Some((_,_,meta)) = devices.get_mut(&id) {
                            if meta.bridged && path.cluster == 0x39 && path.attribute == 0x11 {
                                meta.set_reachable(&value);
                                ctx.set_availability(&id,meta.online());
                            } else if meta.clusters.contains(&path.cluster)
                                && let Some((key,value)) = mapped(path,&value,meta.boolean) { ctx.set_state(&id,key,value); }
                        }
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
        // Synthetic catalogue records: endpoint, device types, servers, expected point.
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
            assert_eq!(
                point(meta.clusters[0], meta.boolean).key.as_ref(),
                fields[3]
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
            label: None,
            boolean: None,
            bridged: true,
            reachable: false,
            connected: true,
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
    }
}
