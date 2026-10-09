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

use device::{Address, Link, Order, Up};
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
    /// Look for the devices still without an address on the home network
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
    let mut addresses: HashMap<String, Arc<watch::Sender<Address>>> = HashMap::new();
    let mut wanted = Vec::new();
    let mut devices: Vec<(DeviceId, Arc<TuyaDevice>)> = Vec::new();
    let mut tasks = tokio::task::JoinSet::new();
    let mut missing_keys = 0;
    let keys = cloud_keys(config, ctx).await;
    let mut silent_seen = false;
    let mut watched = cloud::Watched::new();
    for dev in file.devices {
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
        let start = dev
            .ip
            .clone()
            .map(|ip| (ip, dev.version.unwrap_or(Version::V33)));
        let (addr_tx, addr_rx) = watch::channel(start);
        let addr_tx = Arc::new(addr_tx);
        if dev.ip.is_none() {
            wanted.push(seek::Wanted {
                id: dev.id.clone(),
                name: dev.name.trim().to_owned(),
                key,
                address: Arc::clone(&addr_tx),
            });
        }
        let (order_tx, order_rx) = mpsc::channel(8);
        let index = devices.len();
        addresses.insert(dev.id.clone(), addr_tx);
        orders.insert(id.clone(), (index, order_tx));
        devices.push((id, Arc::clone(&dev)));
        tasks.spawn(
            Link {
                index,
                device: dev,
                key,
                address: addr_rx,
                orders: order_rx,
                up: up_tx.clone(),
            }
            .run(),
        );
    }
    // Keys given later in the dashboard: a restart takes them.
    let await_keys = keys.is_none() && silent_seen;
    if let Some(keys) = keys {
        start_cloud(config, keys, watched, up_tx.clone(), &mut tasks);
    }
    drop(up_tx);
    if missing_keys > 0 {
        tracing::warn!(instance = %ctx.instance(), missing_keys, "tuya devices without a local key are skipped (re-run the import)");
    }
    locate(config, &devices, addresses, wanted, &mut tasks);
    ctx.ready();
    tracing::info!(instance = %ctx.instance(), devices = devices.len(), "tuya devices loaded");

    let keys_came = serve(ctx, &mut up, &orders, &devices, await_keys).await;
    tasks.abort_all();
    if keys_came {
        anyhow::bail!(moli_i18n::tr!("pilotes.tuya.cles_cloud"));
    }
    Ok(())
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

/// Orders go to the devices, what they say goes to the hub, until the end.
/// Returns whether cloud keys arrived meanwhile (then the driver restarts).
async fn serve(
    ctx: &mut DriverCtx,
    up: &mut mpsc::Receiver<Up>,
    orders: &HashMap<DeviceId, (usize, mpsc::Sender<Order>)>,
    devices: &[(DeviceId, Arc<TuyaDevice>)],
    await_keys: bool,
) -> bool {
    let mut look = tokio::time::interval(std::time::Duration::from_secs(20));
    loop {
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
            message = up.recv() => match message {
                Some(Up::State(index, values)) => {
                    let id = &devices[index].0;
                    for (key, value) in values {
                        ctx.set_state(id, &key, value);
                    }
                }
                Some(Up::Online(index, online)) => ctx.set_availability(&devices[index].0, online),
                None => break,
            },
        }
    }
    false
}

/// Follows, through the cloud, the devices that never speak on the LAN.
fn start_cloud(
    config: &Config,
    keys: cloud::Keys,
    watched: cloud::Watched,
    up: mpsc::Sender<Up>,
    tasks: &mut tokio::task::JoinSet<()>,
) {
    if watched.is_empty() {
        return;
    }
    if let Some(region) = cloud::Region::named(&config.cloud_region) {
        tasks.spawn(cloud::watch(keys, region, watched, up));
    } else {
        tracing::warn!(region = %config.cloud_region, "tuya cloud: unknown region");
    }
}

/// Starts what finds the devices' addresses: their broadcasts, and the
/// search by key for those still without one.
fn locate(
    config: &Config,
    devices: &[(DeviceId, Arc<TuyaDevice>)],
    addresses: HashMap<String, Arc<watch::Sender<Address>>>,
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
        tasks.spawn(seek::seek(seek::neighbours(*first), wanted, known.clone()));
    }
    if config.discovery {
        let pinned: HashMap<String, bool> = devices
            .iter()
            .map(|(_, d)| (d.id.clone(), d.ip.is_some() && d.version.is_some()))
            .collect();
        tasks.spawn(discover(addresses, pinned));
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
    let order = Order {
        dps,
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
/// with both pinned in the file are left alone.
async fn discover(
    addresses: HashMap<String, Arc<watch::Sender<Address>>>,
    pinned: HashMap<String, bool>,
) {
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
        if pinned.get(&id).copied().unwrap_or(false) {
            continue;
        }
        if let Some(sender) = addresses.get(&id) {
            sender.send_if_modified(|current| {
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
