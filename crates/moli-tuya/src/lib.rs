//! Tuya Wi-Fi devices, local first: no cloud once imported, except for
//! what never listens on the LAN (battery sensors, devices behind a
//! gateway), read from the household's cloud project when its keys are
//! there (`cloud`).
//!
//! `moli-os tuya import` reads, once, what the Tuya cloud knows (device
//! list, data points, local keys — through the account Home Assistant
//! already uses) and files it: keys in the encrypted secret store, the rest
//! in `data/tuya.json`. From then on each device is reached on the LAN
//! (TCP 6668); addresses and protocol versions come from the device file,
//! the devices' own UDP broadcasts, or, when those are not heard, from
//! asking the listeners of the home network with each key (`seek`).

pub mod cloud;
mod connections;
mod device;
pub mod model;
pub mod protocol;
mod seek;

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use moli_core::DeviceId;
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, watch};

use device::{Link, Order, Up};
pub use model::{DeviceFile, Dp, DpKind, TuyaDevice};
use protocol::Version;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Written by `moli-os tuya import`.
    #[serde(default = "default_file")]
    pub devices_file: PathBuf,
    /// Listen to the devices' broadcasts for their address and version.
    #[serde(default = "yes")]
    pub discovery: bool,
    /// Look for offline local devices on the home network
    /// (the /24 of the known ones), with their keys.
    #[serde(default = "yes")]
    pub seek: bool,
    /// The cloud project's keys (Access ID then Secret, a line each), put
    /// there by the start script from the vault. Missing or empty: no cloud.
    #[serde(default = "default_cloud_keys")]
    pub cloud_keys: PathBuf,
    /// Where the Tuya account lives: `eu` (Central Europe), `eu-west`, `us`…
    #[serde(default = "default_region")]
    pub cloud_region: String,
}

fn default_cloud_keys() -> PathBuf {
    PathBuf::from("/run/secrets/tuya-cloud")
}

fn default_region() -> String {
    "eu".into()
}

fn default_file() -> PathBuf {
    PathBuf::from("data/tuya.json")
}

fn yes() -> bool {
    true
}

/// Secret name of a device's local key.
#[must_use]
pub fn key_name(device_id: &str) -> String {
    format!("key-{device_id}")
}

#[derive(Debug)]
pub struct Tuya {
    config: Config,
}

impl Tuya {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Tuya {
    fn kind(&self) -> &'static str {
        "tuya"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn parse_key(key: &str) -> Option<[u8; 16]> {
    <[u8; 16]>::try_from(key.as_bytes()).ok()
}

#[allow(clippy::too_many_lines)] // one pass: each device, its link, its sub-devices
async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let file: DeviceFile = match tokio::fs::read(&config.devices_file).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ctx.wait_for(moli_i18n::tr!(
                "pilotes.tuya.aucun_appareil",
                file = config.devices_file.display()
            ));
            ctx.cancelled().await;
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let (up_tx, mut up) = mpsc::channel::<Up>(256);
    let mut orders: HashMap<DeviceId, (usize, mpsc::Sender<Order>)> = HashMap::new();
    let mut addresses: HashMap<String, seek::Wanted> = HashMap::new();
    let mut wanted = Vec::new();
    let mut devices: Vec<(DeviceId, Arc<TuyaDevice>)> = Vec::new();
    let mut tasks = tokio::task::JoinSet::new();
    let connections = Arc::new(connections::Connections::default());
    let mut missing_keys = 0;
    let keys = cloud_keys(config, ctx).await;
    let mut silent_seen = false;
    let mut watched = cloud::Watched::new();
    let mut fallback = Fallback::default();
    let (mut children, rest) = split_children(ctx, file.devices, &mut devices);
    // Sub-devices read locally still need the cloud when their gateway is
    // out of reach: keys given later restart the driver for them too.
    silent_seen |= !children.is_empty();
    for dev in rest {
        // Never on the LAN: a battery sensor without an address, a device
        // behind a gateway. The cloud speaks for them, when it can.
        let silent = dev.gateway.is_some()
            || (dev.ip.is_none() && cloud::SLEEPY.contains(&dev.category.as_str()));
        silent_seen |= silent;
        if silent && keys.is_some() {
            let device = dev.device(ctx.instance());
            let id = device.id.clone();
            ctx.upsert_device(device);
            let dev = Arc::new(dev);
            watched.insert(dev.id.clone(), (devices.len(), Arc::clone(&dev)));
            devices.push((id, dev));
            continue;
        }
        if dev.gateway.is_some() {
            continue; // behind a hub: not reachable on its own
        }
        let Some(key) = ctx
            .secret(&key_name(&dev.id))
            .as_deref()
            .and_then(parse_key)
        else {
            missing_keys += 1;
            continue;
        };
        let device = dev.device(ctx.instance());
        let id = device.id.clone();
        ctx.upsert_device(device);
        let dev = Arc::new(dev);
        let local = local_device(&dev, key, Arc::clone(&connections));
        let addr_rx = local.address.subscribe();
        let reachability = Arc::clone(&local.reachability);
        if !silent {
            wanted.push(local.clone());
        }
        let (order_tx, order_rx) = mpsc::channel(8);
        let index = devices.len();
        let carried = children.remove(&dev.id).unwrap_or_default();
        for child in &carried {
            orders.insert(
                devices[child.index].0.clone(),
                (child.index, order_tx.clone()),
            );
            // The cloud, when it can, speaks for the sub-device while its
            // gateway is out of reach.
            if keys.is_some() {
                fallback.gateway.insert(child.index, index);
                watched.insert(
                    child.device.id.clone(),
                    (child.index, Arc::clone(&child.device)),
                );
            }
        }
        addresses.insert(dev.id.clone(), local);
        orders.insert(id.clone(), (index, order_tx));
        devices.push((id, Arc::clone(&dev)));
        tasks.spawn(
            Link {
                index,
                device: dev,
                key,
                address: addr_rx,
                reachability,
                connections: Arc::clone(&connections),
                orders: order_rx,
                up: up_tx.clone(),
                children: carried,
            }
            .run(),
        );
    }
    // Keys given later in the dashboard: a restart takes them.
    let await_keys = keys.is_none() && silent_seen;
    // Apart from the links: what the cloud says of a sub-device is used
    // only while its gateway is out of reach.
    let (cloud_tx, mut cloud_up) = mpsc::channel::<Up>(256);
    // The devices the cloud alone speaks for (not the sub-devices a local
    // gateway carries).
    let cloud_only: Vec<usize> = watched
        .values()
        .map(|(index, _)| *index)
        .filter(|index| !fallback.gateway.contains_key(index))
        .collect();
    let cloud = keys.map_or(Cloud::Idle, |keys| {
        start_cloud(config, keys, watched, &cloud_tx, &mut tasks)
    });
    // Kept here while no task holds it: the cloud's channel then closes
    // only if the cloud stops, never because it did not start.
    let idle_cloud = (!matches!(cloud, Cloud::Started)).then_some(cloud_tx);
    drop(up_tx);
    if missing_keys > 0 {
        tracing::warn!(instance = %ctx.instance(), missing_keys, "tuya devices without a local key are skipped (re-run the import)");
    }
    locate(config, &devices, addresses, wanted, &mut tasks);
    let refused = if let Cloud::Refused(why) = cloud {
        // No cloud: what it alone reads is offline (before `ready`, which
        // would bring back what a previous run showed), and a gateway out of
        // reach has nothing to fall back on.
        for index in cloud_only {
            ctx.set_availability(&devices[index].0, false);
        }
        fallback = Fallback::default();
        Some(why)
    } else {
        None
    };
    ctx.ready();
    tracing::info!(instance = %ctx.instance(), devices = devices.len(), "tuya devices loaded");
    if let Some(why) = refused {
        // The local devices go on; the house is told what to fix.
        ctx.wait_for(why);
    }

    let mut sources = Sources {
        links: &mut up,
        cloud: &mut cloud_up,
        fallback,
    };
    let keys_came = serve(ctx, &mut sources, &orders, &devices, await_keys).await;
    drop(idle_cloud);
    tasks.abort_all();
    if keys_came {
        anyhow::bail!(moli_i18n::tr!("pilotes.tuya.cles_cloud"));
    }
    Ok(())
}

/// Sub-devices whose gateway is reached locally speak through it: they are
/// registered here, grouped by gateway; the other devices are returned.
fn split_children(
    ctx: &mut DriverCtx,
    all: Vec<TuyaDevice>,
    devices: &mut Vec<(DeviceId, Arc<TuyaDevice>)>,
) -> (HashMap<String, Vec<device::Child>>, Vec<TuyaDevice>) {
    let local_gateways: HashSet<String> = all
        .iter()
        .filter(|d| {
            d.gateway.is_none()
                && ctx
                    .secret(&key_name(&d.id))
                    .as_deref()
                    .and_then(parse_key)
                    .is_some()
        })
        .map(|d| d.id.clone())
        .collect();
    let mut children: HashMap<String, Vec<device::Child>> = HashMap::new();
    let mut rest = Vec::new();
    for dev in all {
        let parent = dev
            .parent
            .clone()
            .filter(|p| dev.gateway.is_some() && local_gateways.contains(p));
        let (Some(parent), Some(cid)) = (parent, dev.node_id.clone()) else {
            rest.push(dev);
            continue;
        };
        let device = dev.device(ctx.instance());
        let id = device.id.clone();
        ctx.upsert_device(device);
        let dev = Arc::new(dev);
        children.entry(parent).or_default().push(device::Child {
            cid,
            index: devices.len(),
            device: Arc::clone(&dev),
        });
        devices.push((id, dev));
    }
    (children, rest)
}

fn local_device(
    dev: &TuyaDevice,
    key: [u8; 16],
    connections: Arc<connections::Connections>,
) -> seek::Wanted {
    let start = dev
        .ip
        .clone()
        .map(|ip| (ip, dev.version.unwrap_or(Version::V33)));
    seek::Wanted {
        id: dev.id.clone(),
        name: dev.name.trim().to_owned(),
        key,
        connections,
        address: Arc::new(watch::channel(start).0),
        reachability: Arc::new(
            watch::channel(device::Reachability {
                online: false,
                connected_ip: None,
                version: dev.version.filter(|version| version.negotiates()),
            })
            .0,
        ),
    }
}

/// The vault's file first, else what a person gave in the dashboard.
async fn cloud_keys(config: &Config, ctx: &DriverCtx) -> Option<cloud::Keys> {
    match tokio::fs::read_to_string(&config.cloud_keys).await {
        Ok(text) => cloud::Keys::parse(&text),
        Err(_) => None,
    }
    .or_else(|| stored_keys(ctx))
}

/// The cloud keys a person gave in the dashboard, filed under this driver.
fn stored_keys(ctx: &DriverCtx) -> Option<cloud::Keys> {
    let id = ctx.secret(cloud::STORED_ID)?;
    let secret = ctx.secret(cloud::STORED_SECRET)?;
    cloud::Keys::new(&id, &secret)
}

/// Sub-devices carried by a local gateway and also followed by the cloud:
/// the gateway speaks for them while its link is up, the cloud otherwise.
#[derive(Debug, Default)]
struct Fallback {
    /// A sub-device's index → its gateway's.
    gateway: HashMap<usize, usize>,
    /// Gateways whose local link is up.
    up: HashSet<usize>,
    /// What the cloud last said of each sub-device's availability.
    cloud_online: HashMap<usize, bool>,
}

impl Fallback {
    /// Whether what the cloud says of this device is to be used.
    fn cloud_speaks(&self, index: usize) -> bool {
        self.gateway
            .get(&index)
            .is_none_or(|g| !self.up.contains(g))
    }

    /// A link reaches this device or not: the availability to show. A
    /// sub-device whose gateway is out of reach is as the cloud last saw it.
    fn local(&mut self, index: usize, online: bool) -> bool {
        if self.gateway.values().any(|g| *g == index) {
            if online {
                self.up.insert(index);
            } else {
                self.up.remove(&index);
            }
        }
        if !online && self.gateway.contains_key(&index) {
            return self.cloud_online.get(&index).copied().unwrap_or(false);
        }
        online
    }

    /// The cloud's availability for this device: the one to show, if any.
    fn cloud(&mut self, index: usize, online: bool) -> Option<bool> {
        if self.gateway.contains_key(&index) {
            self.cloud_online.insert(index, online);
        }
        self.cloud_speaks(index).then_some(online)
    }
}

/// Where the devices' news comes from.
struct Sources<'a> {
    links: &'a mut mpsc::Receiver<Up>,
    cloud: &'a mut mpsc::Receiver<Up>,
    fallback: Fallback,
}

/// Orders go to the devices, what they say goes to the hub, until the end.
/// Returns whether cloud keys arrived meanwhile (then the driver restarts).
async fn serve(
    ctx: &mut DriverCtx,
    sources: &mut Sources<'_>,
    orders: &HashMap<DeviceId, (usize, mpsc::Sender<Order>)>,
    devices: &[(DeviceId, Arc<TuyaDevice>)],
    await_keys: bool,
) -> bool {
    let mut look = tokio::time::interval(std::time::Duration::from_secs(20));
    // A channel closes once its senders are gone: the links' when no link
    // is left (none at all in a house the cloud alone speaks for), the
    // cloud's when its task stops (never if none started: `run` keeps its
    // sender). The driver goes on while either is open.
    let (mut links_open, mut cloud_open) = (true, true);
    while links_open || cloud_open {
        tokio::select! {
            _ = look.tick(), if await_keys => {
                if stored_keys(ctx).is_some() {
                    tracing::info!(instance = %ctx.instance(), "tuya cloud keys given in the dashboard");
                    return true;
                }
            }
            // `None` once the driver is cancelled.
            command = ctx.next_command() => {
                let Some(command) = command else { break };
                dispatch(command, orders, devices);
            }
            message = sources.links.recv(), if links_open => match message {
                Some(Up::State(index, values)) => set_states(ctx, &devices[index].0, values),
                Some(Up::Online(index, online)) => {
                    let was_up = sources.fallback.up.contains(&index);
                    let online = sources.fallback.local(index, online);
                    if was_up && !sources.fallback.up.contains(&index) {
                        tracing::info!(
                            gateway = devices[index].1.name,
                            "tuya gateway out of reach: its sub-devices are read through the cloud"
                        );
                    }
                    ctx.set_availability(&devices[index].0, online);
                }
                None => links_open = false,
            },
            message = sources.cloud.recv(), if cloud_open => match message {
                Some(Up::State(index, values)) => {
                    if sources.fallback.cloud_speaks(index) {
                        set_states(ctx, &devices[index].0, values);
                    }
                }
                Some(Up::Online(index, online)) => {
                    if let Some(online) = sources.fallback.cloud(index, online) {
                        ctx.set_availability(&devices[index].0, online);
                    }
                }
                None => cloud_open = false,
            },
        }
    }
    false
}

fn set_states(ctx: &DriverCtx, id: &DeviceId, values: Vec<(String, moli_core::Value)>) {
    for (key, value) in values {
        ctx.set_state(id, &key, value);
    }
}

/// What became of the cloud.
enum Cloud {
    /// A task follows the devices.
    Started,
    /// No keys, or nothing to follow.
    Idle,
    /// It cannot follow them: why, for the house to see.
    Refused(String),
}

/// Follows, through the cloud, the devices that never speak on the LAN.
fn start_cloud(
    config: &Config,
    keys: cloud::Keys,
    watched: cloud::Watched,
    up: &mpsc::Sender<Up>,
    tasks: &mut tokio::task::JoinSet<()>,
) -> Cloud {
    if watched.is_empty() {
        return Cloud::Idle;
    }
    let Some(region) = cloud::Region::named(&config.cloud_region) else {
        tracing::warn!(region = %config.cloud_region, "tuya cloud: unknown region");
        return Cloud::Refused(moli_i18n::tr!(
            "pilotes.tuya.region_inconnue",
            region = config.cloud_region,
            known = cloud::Region::names()
        ));
    };
    tasks.spawn(cloud::watch(keys, region, watched, up.clone()));
    Cloud::Started
}

/// Starts what finds the devices' addresses: their broadcasts, and the
/// search by key for local devices whose link is offline.
fn locate(
    config: &Config,
    devices: &[(DeviceId, Arc<TuyaDevice>)],
    addresses: HashMap<String, seek::Wanted>,
    wanted: Vec<seek::Wanted>,
    tasks: &mut tokio::task::JoinSet<()>,
) {
    // The network is the one the known devices are on.
    let known: HashSet<Ipv4Addr> = devices
        .iter()
        .filter_map(|(_, d)| d.ip.as_deref()?.parse().ok())
        .collect();
    if config.seek
        && !wanted.is_empty()
        && let Some(first) = known.iter().min()
    {
        tasks.spawn(seek::seek(seek::neighbours(*first), wanted));
    }
    if config.discovery {
        tasks.spawn(discover(addresses));
    }
}

/// Hands a command to its device's link; the reply comes from the link.
fn dispatch(
    command: moli_runtime::CommandRequest,
    orders: &HashMap<DeviceId, (usize, mpsc::Sender<Order>)>,
    devices: &[(DeviceId, Arc<TuyaDevice>)],
) {
    let Some((index, sender)) = orders.get(&command.device.id) else {
        let known = devices.iter().any(|(id, _)| *id == command.device.id);
        command.reply(Err(if known {
            moli_i18n::tr!("pilotes.tuya.lecture_seule_cloud")
        } else {
            moli_i18n::tr!("pilotes.tuya.appareil_inconnu")
        }));
        return;
    };
    let raw = devices[*index]
        .1
        .dp_by_code(&command.key)
        .and_then(|dp| Some((dp.id, dp.encode(&command.value)?)));
    let Some((dp, raw)) = raw else {
        let message = moli_i18n::tr!("pilotes.tuya.ne_se_regle_pas", point = command.key);
        command.reply(Err(message));
        return;
    };
    let (done, outcome) = oneshot::channel();
    let mut dps = serde_json::Map::new();
    dps.insert(dp.to_string(), raw);
    let target = &devices[*index].1;
    let order = Order {
        dps,
        cid: target.node_id.clone().filter(|_| target.gateway.is_some()),
        done,
        at: tokio::time::Instant::now(),
    };
    if sender.try_send(order).is_err() {
        command.reply(Err(moli_i18n::tr!("pilotes.tuya.occupe")));
        return;
    }
    tokio::spawn(async move {
        let result = outcome
            .await
            .unwrap_or_else(|_| Err(moli_i18n::tr!("pilotes.tuya.connexion_perdue")));
        command.reply(result);
    });
}

/// Listens to the devices' broadcasts (3.1–3.3 on 6666/6667, 3.4+ also on
/// 7000) and keeps every device's address and version up to date. Devices
/// with a verified healthy connection are left alone. Anyone can broadcast: a
/// device is never spoken to in an older version than it proved, or than
/// its file says (`device::Floor`).
async fn discover(addresses: HashMap<String, seek::Wanted>) {
    let mut sockets = Vec::new();
    for port in [6666u16, 6667, 7000] {
        match broadcast_socket(port) {
            Ok(socket) => sockets.push(socket),
            Err(e) => tracing::warn!(port, error = %e, "tuya discovery port unavailable"),
        }
    }
    if sockets.is_empty() {
        // Keep the senders: links go on with the addresses they have.
        std::future::pending::<()>().await;
    }
    let (found_tx, mut found) = mpsc::channel::<(String, String, Version)>(64);
    for socket in sockets {
        let found_tx = found_tx.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                let Ok((n, from)) = socket.recv_from(&mut buf).await else {
                    return;
                };
                let Some(json) = protocol::open_broadcast(&buf[..n]) else {
                    continue;
                };
                let (Some(id), Some(version)) = (
                    json.get("gwId").and_then(|v| v.as_str()),
                    json.get("version")
                        .and_then(|v| v.as_str())
                        .and_then(Version::parse),
                ) else {
                    continue;
                };
                // Where the broadcast came from, not what it claims.
                let ip = from.ip().to_string();
                if found_tx.send((id.to_owned(), ip, version)).await.is_err() {
                    return;
                }
            }
        });
    }
    drop(found_tx);
    while let Some((id, ip, version)) = found.recv().await {
        if let Some(device) = addresses.get(&id) {
            let state = *device.reachability.borrow();
            if state.online {
                continue;
            }
            let version = version.max(state.version.unwrap_or(Version::V33));
            device.address.send_if_modified(|current| {
                let next = Some((ip.clone(), version));
                if *current == next {
                    return false;
                }
                tracing::info!(device = id, ip, ?version, "tuya device found");
                *current = next;
                true
            });
        }
    }
}

/// Shared with anything else listening (Home Assistant's localtuya):
/// broadcasts reach every socket bound with SO_REUSEPORT.
fn broadcast_socket(port: u16) -> std::io::Result<UdpSocket> {
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_broadcast(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    UdpSocket::from_std(socket.into())
}

// ---- import ------------------------------------------------------------------

/// What the import script sends: devices with their local key.
#[derive(Debug, Deserialize)]
pub struct ImportFile {
    pub devices: Vec<ImportDevice>,
}

#[derive(Deserialize)]
pub struct ImportDevice {
    #[serde(flatten)]
    pub device: TuyaDevice,
    #[serde(default)]
    pub local_key: Option<String>,
}

impl std::fmt::Debug for ImportDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportDevice")
            .field("device", &self.device)
            .field("local_key", &self.local_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// The device file after an import, and the keys to store. Hand-edited
/// fields (`ip`, `version`, `room`) of known devices are kept.
#[must_use]
pub fn merge_import(
    import: ImportFile,
    existing: Option<DeviceFile>,
) -> (DeviceFile, Vec<(String, String)>) {
    let previous: HashMap<String, TuyaDevice> = existing
        .map(|f| f.devices.into_iter().map(|d| (d.id.clone(), d)).collect())
        .unwrap_or_default();
    let mut keys = Vec::new();
    let devices = import
        .devices
        .into_iter()
        .map(
            |ImportDevice {
                 mut device,
                 local_key,
             }| {
                if let Some(key) = local_key.filter(|k| parse_key(k).is_some()) {
                    keys.push((key_name(&device.id), key));
                }
                if let Some(old) = previous.get(&device.id) {
                    device.ip = device.ip.or_else(|| old.ip.clone());
                    device.version = device.version.or(old.version);
                    device.room = device.room.or_else(|| old.room.clone());
                    device.parent = device.parent.or_else(|| old.parent.clone());
                    device.node_id = device.node_id.or_else(|| old.node_id.clone());
                }
                device
            },
        )
        .collect();
    (DeviceFile { devices }, keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gateway (0) carrying a probe (1), both followed by the cloud.
    fn gateway_and_probe() -> Fallback {
        Fallback {
            gateway: HashMap::from([(1, 0)]),
            ..Fallback::default()
        }
    }

    #[test]
    fn the_gateway_speaks_for_its_sub_devices_while_it_is_reached() {
        let mut fallback = gateway_and_probe();
        assert!(fallback.local(0, true));
        assert!(fallback.local(1, true));
        assert!(!fallback.cloud_speaks(1), "the cloud's states are not used");
        assert_eq!(fallback.cloud(1, false), None, "nor its availability");
        assert!(
            fallback.cloud_speaks(0),
            "the gateway itself is not concerned"
        );
    }

    #[test]
    fn the_cloud_takes_over_when_the_gateway_is_out_of_reach() {
        let mut fallback = gateway_and_probe();
        assert!(fallback.local(0, true));
        // Heard, kept, not shown while the gateway is up.
        assert_eq!(fallback.cloud(1, true), None);
        // The gateway goes: the probe is as the cloud last saw it.
        assert!(!fallback.local(0, false));
        assert!(fallback.local(1, false), "online through the cloud");
        assert!(fallback.cloud_speaks(1));
        assert_eq!(fallback.cloud(1, false), Some(false));
        // Back: the gateway speaks again.
        assert!(fallback.local(0, true));
        assert!(fallback.local(1, true));
        assert!(!fallback.cloud_speaks(1));
    }

    #[test]
    fn a_gateway_never_reached_leaves_its_sub_devices_to_the_cloud() {
        let mut fallback = gateway_and_probe();
        // The link finds no address: offline, and the cloud has not spoken yet.
        assert!(!fallback.local(0, false));
        assert!(!fallback.local(1, false));
        assert_eq!(fallback.cloud(1, true), Some(true));
    }

    /// The driver's statuses over 1.5 s, for `devices` (a `tuya.json` list)
    /// and cloud keys, its cloud in `region`; then whether each device is
    /// online.
    async fn statuses(
        name: &str,
        devices: serde_json::Value,
        region: &str,
    ) -> (Vec<Option<moli_core::DriverStatus>>, Vec<Option<bool>>) {
        use moli_core::InstanceId;
        use moli_runtime::{Hub, HubOptions, spawn_driver};
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        let dir = std::env::temp_dir().join(format!("moli-tuya-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let devices_file = dir.join("tuya.json");
        let file = serde_json::json!({ "devices": devices });
        std::fs::write(&devices_file, file.to_string()).unwrap();
        let cloud_keys = dir.join("tuya-cloud");
        std::fs::write(
            &cloud_keys,
            "test-access-id\ntest-access-secret-0123456789\n",
        )
        .unwrap();
        let config = Config {
            devices_file,
            discovery: false,
            seek: false,
            cloud_keys,
            cloud_region: region.into(),
        };
        let hub = Hub::new(HubOptions::default()).unwrap();
        let tuya = InstanceId::from("tuya");
        let cancel = CancellationToken::new();
        spawn_driver(
            &hub,
            tuya.clone(),
            Arc::new(Tuya::new(config)),
            cancel.clone(),
        );
        let status = || {
            hub.snapshot()
                .drivers
                .into_iter()
                .find(|d| d.instance == tuya)
                .map(|d| d.status)
        };
        let mut seen = Vec::new();
        for _ in 0..150 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let now = status();
            if seen.last() != Some(&now) {
                seen.push(now);
            }
        }
        let online = hub.snapshot().devices.iter().map(|d| d.online).collect();
        cancel.cancel();
        let _ = std::fs::remove_dir_all(&dir);
        (seen, online)
    }

    fn never_restarted(seen: &[Option<moli_core::DriverStatus>]) {
        assert!(
            seen.iter()
                .all(|s| !matches!(s, Some(moli_core::DriverStatus::Backoff { .. }))),
            "the driver stopped: {seen:?}"
        );
    }

    /// A battery door sensor the cloud alone speaks for.
    fn door() -> serde_json::Value {
        serde_json::json!([{ "id": "bf00000000000000door", "name": "Porte", "category": "mcs" }])
    }

    /// Only cloud-read devices, nothing on the LAN: the driver keeps
    /// running, it is not restarted in a loop. Its cloud is this very
    /// machine: nothing goes to Tuya.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_house_read_only_through_the_cloud_keeps_its_driver_running() {
        let (seen, _) = statuses("cloud-only", door(), "test").await;
        never_restarted(&seen);
        assert_eq!(seen.last(), Some(&Some(moli_core::DriverStatus::Running)));
    }

    /// Its cloud never starts: the house is told why, the door is offline
    /// (not shown as it last was), and the driver is not restarted in a loop.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_cloud_region_is_told_not_restarted() {
        let (seen, online) = statuses("unknown-region", door(), "eu-central").await;
        never_restarted(&seen);
        let Some(Some(moli_core::DriverStatus::Waiting { reason })) = seen.last() else {
            panic!("the house is not told: {seen:?}");
        };
        assert!(
            reason.contains("\"eu-central\"") && reason.contains(&cloud::Region::names()),
            "{reason}"
        );
        assert_eq!(online, [Some(false)]);
    }

    /// Cloud keys, nothing for the cloud nor for a link (no device yet):
    /// the driver keeps running.
    #[tokio::test(flavor = "multi_thread")]
    async fn cloud_keys_without_devices_keep_the_driver_running() {
        let (seen, _) = statuses("no-device", serde_json::json!([]), "test").await;
        never_restarted(&seen);
        assert_eq!(seen.last(), Some(&Some(moli_core::DriverStatus::Running)));
    }

    #[test]
    fn without_the_cloud_a_link_says_what_it_sees() {
        let mut fallback = Fallback::default();
        assert!(!fallback.local(1, false));
        assert!(fallback.local(1, true));
        assert!(
            fallback.cloud_speaks(7),
            "a battery sensor the cloud follows"
        );
    }

    #[test]
    fn imports_keep_hand_edits_and_separate_keys() {
        let import: ImportFile = serde_json::from_value(serde_json::json!({
            "devices": [
                { "id": "a", "name": "Salon", "local_key": "0123456789abcdef", "dps": [] },
                { "id": "b", "name": "Volet", "local_key": "short", "dps": [] },
            ]
        }))
        .unwrap();
        let existing =
            DeviceFile {
                devices: vec![serde_json::from_value(serde_json::json!({
                "id": "b", "name": "Volet", "room": "Chambre à coucher", "ip": "192.168.0.13"
            }))
            .unwrap()],
            };
        let (file, keys) = merge_import(import, Some(existing));
        assert_eq!(
            keys,
            vec![("key-a".to_owned(), "0123456789abcdef".to_owned())]
        );
        assert_eq!(file.devices[1].room.as_deref(), Some("Chambre à coucher"));
        assert_eq!(file.devices[1].ip.as_deref(), Some("192.168.0.13"));
        let text = serde_json::to_string(&file).unwrap();
        assert!(
            !text.contains("0123456789abcdef"),
            "no key in the device file"
        );
    }
}
