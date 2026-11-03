//! Read-only gateways added by a person. A bounded sidecar leaves the hand-written
//! configuration untouched. Persistence precedes registration; an owned task
//! finishes a committed installation even if the HTTP client disconnects.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{Read as _, Write as _};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context as _, ensure};
use moli_api::airzone_setup::{
    AirzoneSetup, GatewayInstalled, GatewayList, GatewayPreview, GatewayRequest, GatewayStatus,
    GatewayWithdrawn, SetupError,
};
use moli_core::{InstanceId, Outcome, Value};
use moli_runtime::{BoxFuture, Hub, spawn_driver};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, DriverConfig};

pub const FILE: &str = "airzone-gateways.json";
const MAX_GATEWAYS: usize = 32;
const MAX_BYTES: u64 = 65_536;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Gateway {
    instance: String,
    host: String,
    port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tls_fingerprint: Option<String>,
    mac: String,
    read_only: bool,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
}

const fn enabled_by_default() -> bool {
    true
}

impl Gateway {
    fn request(&self) -> GatewayRequest {
        GatewayRequest {
            instance: self.instance.clone(),
            host: self.host.clone(),
            port: self.port,
            tls_fingerprint: self.tls_fingerprint.clone(),
            expected_mac: Some(self.mac.clone()),
        }
    }
    fn driver(&self) -> DriverConfig {
        let mut options = toml::Table::new();
        options.insert("host".into(), self.host.clone().into());
        options.insert("port".into(), i64::from(self.port).into());
        options.insert("poll_secs".into(), 30_i64.into());
        options.insert("mac".into(), self.mac.clone().into());
        options.insert("read_only".into(), true.into());
        if let Some(pin) = &self.tls_fingerprint {
            options.insert("tls_fingerprint".into(), pin.clone().into());
        }
        DriverConfig {
            id: self.instance.clone(),
            kind: "airzone".into(),
            enabled: Some(self.enabled),
            options: toml::Value::Table(options),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u8,
    gateways: Vec<Gateway>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    overrides: Vec<GatewayOverride>,
}

/// A static entry remains in TOML. Only its explicitly managed connection and
/// enabled state are overlaid, so unrelated native options survive edits.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GatewayOverride {
    instance: String,
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection: Option<Gateway>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            version: 2,
            gateways: Vec::new(),
            overrides: Vec::new(),
        }
    }
}

fn bytes(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut data = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_BYTES {
        return Err(std::io::Error::other("Airzone registry exceeds its bound"));
    }
    Ok(Some(data))
}

fn read_store(path: &Path) -> anyhow::Result<(Store, Option<Vec<u8>>)> {
    let raw = bytes(path).context("cannot read managed Airzone registry")?;
    let store: Store = raw
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()
        .context("invalid managed Airzone registry")?
        .unwrap_or_default();
    ensure!(
        matches!(store.version, 1 | 2)
            && store.gateways.len() + store.overrides.len() <= MAX_GATEWAYS
            && (store.version == 2 || store.overrides.is_empty()),
        "unsupported managed Airzone registry"
    );
    let mut ids = HashSet::new();
    let mut endpoints = HashSet::new();
    let mut macs = HashSet::new();
    for gateway in &store.gateways {
        let normalized = normalize(gateway.request())
            .map_err(|_| anyhow::anyhow!("invalid managed Airzone gateway"))?;
        ensure!(
            gateway.read_only && normalized == gateway.request(),
            "managed Airzone gateway must be canonical and read-only"
        );
        ensure!(
            ids.insert(&gateway.instance)
                && (!gateway.enabled || endpoints.insert((&gateway.host, gateway.port)))
                && macs.insert(&gateway.mac),
            "duplicate managed Airzone gateway"
        );
    }
    for entry in &store.overrides {
        ensure!(
            ids.insert(&entry.instance),
            "duplicate managed Airzone override"
        );
        ensure!(
            entry
                .mac
                .as_deref()
                .is_none_or(|mac| moli_net::lan::mac_hex(mac).as_deref() == Some(mac)),
            "invalid managed Airzone identity"
        );
        if let Some(gateway) = &entry.connection {
            let request = gateway.request();
            ensure!(
                gateway.instance == entry.instance
                    && gateway.read_only
                    && normalize_connection(request.clone())
                        .is_ok_and(|normalized| normalized == request),
                "invalid managed Airzone override"
            );
        }
    }
    Ok((store, raw))
}

/// Called after resolving data_dir and before validating/spawning any driver.
/// A corrupt or conflicting file is an explicit startup error, never discarded.
pub fn load_into(config: &mut Config) -> anyhow::Result<()> {
    let (store, _) = read_store(&config.server.data_dir.join(FILE))?;
    for entry in &store.overrides {
        let driver = config
            .drivers
            .iter_mut()
            .find(|driver| driver.id == entry.instance)
            .context("managed Airzone override has no configured driver")?;
        ensure!(
            driver.kind == "airzone",
            "managed Airzone override conflicts with configured driver"
        );
        driver.enabled = Some(entry.enabled);
        if let Some(mac) = &entry.mac {
            driver
                .options
                .as_table_mut()
                .context("invalid Airzone options")?
                .insert("mac".into(), mac.clone().into());
        }
        if let Some(gateway) = &entry.connection {
            let options = driver
                .options
                .as_table_mut()
                .context("invalid Airzone options")?;
            options.insert("host".into(), gateway.host.clone().into());
            options.insert("port".into(), i64::from(gateway.port).into());
            options.insert("mac".into(), gateway.mac.clone().into());
            options.insert("read_only".into(), true.into());
            options.remove("tls_fingerprint");
            if let Some(pin) = &gateway.tls_fingerprint {
                options.insert("tls_fingerprint".into(), pin.clone().into());
            }
        }
    }
    for gateway in store.gateways {
        for driver in &config.drivers {
            ensure!(
                driver.id != gateway.instance,
                "managed Airzone instance conflicts with configured driver"
            );
            if driver.kind == "airzone" {
                let other: moli_airzone::Config = driver.options.clone().try_into()?;
                ensure!(
                    !(driver.enabled != Some(false)
                        && gateway.enabled
                        && other.host == gateway.host
                        && other.port == gateway.port)
                        && other
                            .mac
                            .as_deref()
                            .and_then(moli_net::lan::mac_hex)
                            .as_deref()
                            != Some(&gateway.mac),
                    "managed Airzone gateway duplicates configured gateway"
                );
            }
        }
        config.drivers.push(gateway.driver());
    }
    Ok(())
}

fn normalize(request: GatewayRequest) -> Result<GatewayRequest, SetupError> {
    let id = &request.instance;
    if id.is_empty()
        || id.len() > 32
        || !id.as_bytes()[0].is_ascii_lowercase()
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        || crate::config::reserved_instance(id)
    {
        return Err(SetupError::new("invalid_instance"));
    }
    normalize_connection(request)
}

fn normalize_connection(mut request: GatewayRequest) -> Result<GatewayRequest, SetupError> {
    let ip = request
        .host
        .parse::<Ipv4Addr>()
        .map_err(|_| SetupError::new("invalid_host"))?;
    if !(ip.is_private() || ip.is_loopback()) {
        return Err(SetupError::new("invalid_host"));
    }
    request.host = ip.to_string();
    if request.port == 0 {
        return Err(SetupError::new("invalid_port"));
    }
    if let Some(pin) = &mut request.tls_fingerprint {
        if pin.len() > 128 {
            return Err(SetupError::new("invalid_fingerprint"));
        }
        *pin = pin
            .chars()
            .filter(|c| *c != ':' && !c.is_ascii_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        if pin.len() != 64 || !pin.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(SetupError::new("invalid_fingerprint"));
        }
    }
    if let Some(mac) = &mut request.expected_mac {
        *mac = moli_net::lan::mac_hex(mac).ok_or_else(|| SetupError::new("invalid_identity"))?;
    }
    Ok(request)
}

fn native(request: &GatewayRequest, mac: Option<String>) -> moli_airzone::Config {
    moli_airzone::Config {
        host: request.host.clone(),
        port: request.port,
        poll_secs: 30,
        mac,
        read_only: true,
        dhw_units: None,
        tls_fingerprint: request.tls_fingerprint.clone(),
    }
}

fn apply_connection(config: &mut moli_airzone::Config, request: &GatewayRequest) {
    config.host.clone_from(&request.host);
    config.port = request.port;
    config.mac.clone_from(&request.expected_mac);
    config.tls_fingerprint.clone_from(&request.tls_fingerprint);
    config.read_only = true;
}

fn gateway(request: &GatewayRequest, enabled: bool) -> Gateway {
    Gateway {
        instance: request.instance.clone(),
        host: request.host.clone(),
        port: request.port,
        tls_fingerprint: request.tls_fingerprint.clone(),
        mac: request.expected_mac.clone().unwrap_or_default(),
        read_only: true,
        enabled,
    }
}

#[derive(Clone, Debug)]
struct StaticGateway {
    instance: String,
    config: moli_airzone::Config,
    enabled: bool,
}
struct Running {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
struct EffectiveGateway {
    instance: String,
    config: moli_airzone::Config,
    enabled: bool,
    source: &'static str,
    withdrawn: bool,
}
struct State {
    store: Store,
    raw: Option<Vec<u8>>,
}
struct Inner {
    hub: Hub,
    path: PathBuf,
    reserved: HashSet<String>,
    configured: Vec<StaticGateway>,
    state: Arc<tokio::sync::Mutex<State>>,
    cancel: CancellationToken,
    accepting: AtomicBool,
    drivers: Mutex<HashMap<String, Running>>,
    operations: Mutex<Vec<JoinHandle<()>>>,
}
#[derive(Clone)]
pub struct Manager(Arc<Inner>);
impl fmt::Debug for Manager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AirzoneSetup")
    }
}

impl Manager {
    pub fn new(hub: Hub, config: &Config, cancel: CancellationToken) -> anyhow::Result<Self> {
        let path = config.server.data_dir.join(FILE);
        let (store, raw) = read_store(&path)?;
        for saved in &store.gateways {
            ensure!(
                config
                    .drivers
                    .iter()
                    .any(|driver| driver.id == saved.instance
                        && driver.kind == "airzone"
                        && (driver.enabled != Some(false)) == saved.enabled
                        && driver.options == saved.driver().options),
                "managed Airzone registry changed during startup"
            );
        }
        let mut reserved = HashSet::new();
        let mut configured = Vec::new();
        for driver in &config.drivers {
            if let Some(saved) = store
                .gateways
                .iter()
                .find(|saved| saved.instance == driver.id)
            {
                ensure!(
                    saved.driver().options == driver.options
                        && (driver.enabled != Some(false)) == saved.enabled
                        && driver.kind == "airzone",
                    "managed Airzone registry changed during startup"
                );
                continue;
            }
            reserved.insert(driver.id.clone());
            if driver.kind == "airzone" {
                configured.push(StaticGateway {
                    instance: driver.id.clone(),
                    config: driver.options.clone().try_into()?,
                    enabled: driver.enabled != Some(false),
                });
            }
        }
        for entry in &store.overrides {
            let current = configured
                .iter()
                .find(|gateway| gateway.instance == entry.instance)
                .context("managed Airzone override changed during startup")?;
            ensure!(
                current.enabled == entry.enabled,
                "managed Airzone registry changed during startup"
            );
            ensure!(
                entry
                    .mac
                    .as_ref()
                    .is_none_or(|mac| current.config.mac.as_ref() == Some(mac)),
                "managed Airzone identity changed during startup"
            );
            if let Some(connection) = &entry.connection {
                ensure!(
                    current.config.host == connection.host
                        && current.config.port == connection.port
                        && current.config.mac.as_deref() == Some(&connection.mac)
                        && current.config.tls_fingerprint == connection.tls_fingerprint
                        && current.config.read_only,
                    "managed Airzone connection changed during startup"
                );
            }
        }
        Ok(Self(Arc::new(Inner {
            hub,
            path,
            reserved,
            configured,
            state: Arc::new(tokio::sync::Mutex::new(State { store, raw })),
            cancel,
            accepting: AtomicBool::new(true),
            drivers: Mutex::default(),
            operations: Mutex::default(),
        })))
    }

    fn available(&self) -> Result<(), SetupError> {
        if !self.0.accepting.load(Ordering::Acquire) || self.0.cancel.is_cancelled() {
            return Err(SetupError::new("shutting_down"));
        }
        Ok(())
    }

    fn effective(&self, state: &State) -> Vec<EffectiveGateway> {
        let mut gateways: Vec<_> = self
            .0
            .configured
            .iter()
            .map(|saved| {
                let mut config = saved.config.clone();
                let overlay = state
                    .store
                    .overrides
                    .iter()
                    .find(|entry| entry.instance == saved.instance);
                if let Some(mac) = overlay.and_then(|entry| entry.mac.as_ref()) {
                    config.mac = Some(mac.clone());
                }
                if let Some(connection) = overlay.and_then(|entry| entry.connection.as_ref()) {
                    apply_connection(&mut config, &connection.request());
                }
                EffectiveGateway {
                    instance: saved.instance.clone(),
                    config,
                    enabled: overlay.map_or(saved.enabled, |entry| entry.enabled),
                    source: "configuration",
                    withdrawn: overlay.is_some_and(|entry| !entry.enabled),
                }
            })
            .collect();
        gateways.extend(state.store.gateways.iter().map(|saved| EffectiveGateway {
            instance: saved.instance.clone(),
            config: native(&saved.request(), Some(saved.mac.clone())),
            enabled: saved.enabled,
            source: "moli",
            withdrawn: !saved.enabled,
        }));
        gateways
    }

    fn known_mac(&self, gateway: &EffectiveGateway) -> Option<String> {
        gateway
            .config
            .mac
            .as_deref()
            .and_then(moli_net::lan::mac_hex)
            .or_else(|| {
                self.0
                    .hub
                    .snapshot()
                    .devices
                    .iter()
                    .filter(|device| device.device.instance.as_str() == gateway.instance)
                    .find_map(|device| {
                        device
                            .state
                            .get("mac")
                            .and_then(|reading| match &reading.value {
                                Value::Text(mac) => moli_net::lan::mac_hex(mac),
                                _ => None,
                            })
                    })
            })
    }

    fn existing(&self, state: &State, instance: &str) -> Result<EffectiveGateway, SetupError> {
        self.effective(state)
            .into_iter()
            .find(|gateway| gateway.instance == instance)
            .ok_or_else(|| SetupError::new("not_found"))
    }

    /// Called after history has subscribed. All Airzone lifetimes, including
    /// static TOML entries, belong to this manager, never the generic spawner.
    pub fn start_configured(&self) -> anyhow::Result<()> {
        let state = self
            .0
            .state
            .try_lock()
            .context("Airzone manager is busy at startup")?;
        ensure!(
            self.0
                .drivers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty(),
            "Airzone drivers are already started"
        );
        let prepared: Vec<_> = self
            .effective(&state)
            .into_iter()
            .filter(|gateway| gateway.enabled)
            .map(|gateway| {
                moli_airzone::Airzone::new(gateway.config).map(|driver| (gateway.instance, driver))
            })
            .collect::<anyhow::Result<_>>()?;
        for (instance, driver) in prepared {
            self.start(&instance, driver);
        }
        Ok(())
    }

    fn start(&self, instance: &str, driver: moli_airzone::Airzone) {
        let cancel = self.0.cancel.child_token();
        let task = spawn_driver(
            &self.0.hub,
            InstanceId::from(instance),
            Arc::new(driver),
            cancel.clone(),
        );
        self.0
            .drivers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(instance.to_owned(), Running { cancel, task });
    }

    async fn stop(&self, instance: &str) {
        let running = self
            .0
            .drivers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(instance);
        if let Some(running) = running {
            running.cancel.cancel();
            let _ = running.task.await;
        }
        self.0.hub.unregister_driver(&InstanceId::from(instance));
    }

    fn collision(
        &self,
        state: &State,
        request: &GatewayRequest,
        mac: Option<&str>,
        except: Option<&str>,
    ) -> Result<(), SetupError> {
        let snapshot = self.0.hub.snapshot();
        if except != Some(request.instance.as_str())
            && (self.0.reserved.contains(&request.instance)
                || state
                    .store
                    .gateways
                    .iter()
                    .any(|g| g.instance == request.instance)
                || snapshot
                    .drivers
                    .iter()
                    .any(|d| d.instance.as_str() == request.instance))
        {
            return Err(SetupError::conflict(
                "duplicate_instance",
                &request.instance,
            ));
        }
        for other in self
            .effective(state)
            .iter()
            .filter(|gateway| Some(gateway.instance.as_str()) != except)
        {
            if !other.withdrawn
                && other.config.host == request.host
                && other.config.port == request.port
            {
                return Err(SetupError::conflict("duplicate_gateway", &other.instance));
            }
            let known = self.known_mac(other);
            if let Some(mac) = mac {
                if known.as_deref() == Some(mac) {
                    return Err(SetupError::conflict("duplicate_gateway", &other.instance));
                }
                if known.is_none() && !other.withdrawn {
                    return Err(SetupError::new("identity_unavailable"));
                }
            }
        }
        if let Some(mac) = mac {
            for device in &snapshot.devices {
                if Some(device.device.instance.as_str()) == except {
                    continue;
                }
                if device.state.get("mac").is_some_and(|s|matches!(&s.value,Value::Text(v) if moli_net::lan::mac_hex(v).as_deref()==Some(mac)))
                    && snapshot.drivers.iter().any(|d|d.instance==device.device.instance && d.kind=="airzone") {
                    return Err(SetupError::conflict("duplicate_gateway",device.device.instance.as_str()));
                }
            }
        }
        Ok(())
    }

    async fn verified(
        &self,
        state: &State,
        request: &GatewayRequest,
        except: Option<&str>,
    ) -> Result<moli_airzone::GatewayProbe, SetupError> {
        self.collision(state, request, None, except)?;
        let report = tokio::select! {
            ()=self.0.cancel.cancelled()=>return Err(SetupError::new("shutting_down")),
            result=moli_airzone::probe_read_only(native(request,None))=>result.map_err(|_|SetupError::new("probe_failed"))?,
        };
        if request
            .expected_mac
            .as_ref()
            .is_some_and(|expected| expected != &report.mac)
        {
            return Err(SetupError::new("identity_changed"));
        }
        self.collision(state, request, Some(&report.mac), except)?;
        Ok(report)
    }

    async fn persisted(&self, state: &State) -> Result<(), SetupError> {
        let path = self.0.path.clone();
        let expected = state.raw.clone();
        let unchanged =
            tokio::task::spawn_blocking(move || bytes(&path).map(|current| current == expected))
                .await
                .map_err(|_| SetupError::new("persistence_failed"))?
                .map_err(|_| SetupError::new("persistence_failed"))?;
        if !unchanged {
            return Err(SetupError::new("persistence_failed"));
        }
        Ok(())
    }

    async fn persist(&self, state: &mut State, mut next: Store) -> Result<(), SetupError> {
        next.version = 2;
        if next.gateways.len() + next.overrides.len() > MAX_GATEWAYS {
            return Err(SetupError::new("limit_reached"));
        }
        let path = self.0.path.clone();
        let expected = state.raw.clone();
        let saved = next.clone();
        let raw = tokio::task::spawn_blocking(move || save(&path, &saved, expected.as_deref()))
            .await
            .map_err(|_| SetupError::new("persistence_failed"))?
            .map_err(|_| SetupError::new("persistence_failed"))?;
        state.store = next;
        state.raw = Some(raw);
        Ok(())
    }

    async fn commit(
        &self,
        request: GatewayRequest,
        actor: String,
    ) -> Result<GatewayInstalled, SetupError> {
        self.available()?;
        let mut state = self
            .0
            .state
            .clone()
            .try_lock_owned()
            .map_err(|_| SetupError::new("busy"))?;
        // A lost response may be retried after saving or after a process restart.
        if let Some(saved) = state
            .store
            .gateways
            .iter()
            .find(|g| g.instance == request.instance)
        {
            if !saved.enabled {
                return Err(SetupError::new("gateway_disabled"));
            }
            if saved.request() == request {
                self.persisted(&state).await?;
                return Ok(installed(&request.instance, true));
            }
            return Err(SetupError::conflict(
                "duplicate_instance",
                &request.instance,
            ));
        }
        if state.store.gateways.len() + state.store.overrides.len() >= MAX_GATEWAYS {
            return Err(SetupError::new("limit_reached"));
        }
        let report = self.verified(&state, &request, None).await?;
        self.available()?;
        let gateway = Gateway {
            instance: request.instance.clone(),
            host: request.host.clone(),
            port: request.port,
            tls_fingerprint: request.tls_fingerprint.clone(),
            mac: report.mac,
            read_only: true,
            enabled: true,
        };
        let driver = moli_airzone::Airzone::new(native(&request, Some(gateway.mac.clone())))
            .map_err(|_| SetupError::new("probe_failed"))?;
        let mut next = state.store.clone();
        next.gateways.push(gateway);
        if let Err(error) = self.persist(&mut state, next).await {
            self.0.hub.record_driver_added(
                InstanceId::from(request.instance.as_str()),
                "airzone",
                true,
                Some(actor),
                Outcome::Err("persistence_failed".into()),
            );
            return Err(error);
        }
        // No await between the committed state and synchronous registration.
        self.start(&request.instance, driver);
        self.0.hub.record_driver_added(
            InstanceId::from(request.instance.as_str()),
            "airzone",
            true,
            Some(actor),
            Outcome::Ok,
        );
        Ok(installed(&request.instance, false))
    }

    async fn verified_update(
        &self,
        state: &State,
        request: &GatewayRequest,
    ) -> Result<moli_airzone::GatewayProbe, SetupError> {
        let existing = self.existing(state, &request.instance)?;
        let known = self
            .known_mac(&existing)
            .ok_or_else(|| SetupError::new("identity_unavailable"))?;
        if request
            .expected_mac
            .as_deref()
            .is_some_and(|expected| expected != known)
        {
            return Err(SetupError::new("identity_changed"));
        }
        let mut checked = request.clone();
        checked.expected_mac = Some(known);
        self.verified(state, &checked, Some(&request.instance))
            .await
    }

    async fn commit_update(
        &self,
        request: GatewayRequest,
        actor: String,
    ) -> Result<GatewayInstalled, SetupError> {
        self.available()?;
        let mut state = self
            .0
            .state
            .clone()
            .try_lock_owned()
            .map_err(|_| SetupError::new("busy"))?;
        let replay = state
            .store
            .gateways
            .iter()
            .any(|saved| saved.enabled && saved.request() == request)
            || state.store.overrides.iter().any(|saved| {
                saved.enabled
                    && saved
                        .connection
                        .as_ref()
                        .is_some_and(|connection| connection.request() == request)
            });
        if replay {
            self.persisted(&state).await?;
            return Ok(installed(&request.instance, true));
        }
        self.verified_update(&state, &request).await?;
        self.available()?;
        let mut existing = self.existing(&state, &request.instance)?;
        apply_connection(&mut existing.config, &request);
        let driver = moli_airzone::Airzone::new(existing.config)
            .map_err(|_| SetupError::new("probe_failed"))?;
        let mut next = state.store.clone();
        if let Some(saved) = next
            .gateways
            .iter_mut()
            .find(|saved| saved.instance == request.instance)
        {
            *saved = gateway(&request, true);
        } else {
            let overlay = GatewayOverride {
                instance: request.instance.clone(),
                enabled: true,
                mac: request.expected_mac.clone(),
                connection: Some(gateway(&request, true)),
            };
            if let Some(saved) = next
                .overrides
                .iter_mut()
                .find(|saved| saved.instance == request.instance)
            {
                *saved = overlay;
            } else {
                next.overrides.push(overlay);
            }
        }
        let saved = self.persist(&mut state, next).await;
        if saved.is_ok() {
            // The owned operation cannot be cancelled by its HTTP caller. The
            // old supervisor must finish before registration of its successor.
            self.stop(&request.instance).await;
            self.start(&request.instance, driver);
        }
        self.0.hub.record_driver_configured(
            InstanceId::from(request.instance.as_str()),
            "airzone",
            true,
            true,
            Some(actor),
            if saved.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Err("persistence_failed".into())
            },
        );
        saved?;
        Ok(installed(&request.instance, false))
    }

    async fn commit_withdraw(
        &self,
        instance: String,
        actor: String,
    ) -> Result<GatewayWithdrawn, SetupError> {
        self.available()?;
        let mut state = self
            .0
            .state
            .clone()
            .try_lock_owned()
            .map_err(|_| SetupError::new("busy"))?;
        let current = self.existing(&state, &instance)?;
        let result = GatewayWithdrawn {
            instance: instance.clone(),
            stored: true,
            enabled: false,
            read_only: current.config.read_only,
        };
        if current.withdrawn {
            self.persisted(&state).await?;
            return Ok(result);
        }
        let mut next = state.store.clone();
        if let Some(saved) = next
            .gateways
            .iter_mut()
            .find(|saved| saved.instance == instance)
        {
            saved.enabled = false;
        } else if let Some(saved) = next
            .overrides
            .iter_mut()
            .find(|saved| saved.instance == instance)
        {
            saved.enabled = false;
        } else {
            next.overrides.push(GatewayOverride {
                instance: instance.clone(),
                enabled: false,
                mac: self.known_mac(&current),
                connection: None,
            });
        }
        let saved = self.persist(&mut state, next).await;
        if saved.is_ok() {
            self.stop(&instance).await;
        }
        self.0.hub.record_driver_configured(
            InstanceId::from(instance.as_str()),
            "airzone",
            false,
            current.config.read_only,
            Some(actor),
            if saved.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Err("persistence_failed".into())
            },
        );
        saved?;
        Ok(result)
    }

    async fn owned<T: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = Result<T, SetupError>> + Send + 'static,
    ) -> Result<T, SetupError> {
        let (send, receive) = tokio::sync::oneshot::channel();
        {
            let mut operations = self
                .0
                .operations
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            self.available()?;
            operations.retain(|task| !task.is_finished());
            operations.push(tokio::spawn(async move {
                let _ = send.send(future.await);
            }));
        }
        receive.await.map_err(|_| SetupError::new("unavailable"))?
    }

    /// Stop admitting operations, finish any owned commit, and await its drivers.
    pub async fn shutdown(&self) {
        self.0.accepting.store(false, Ordering::Release);
        let operations = std::mem::take(
            &mut *self
                .0
                .operations
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for task in operations {
            let _ = task.await;
        }
        let drivers = std::mem::take(
            &mut *self
                .0
                .drivers
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for running in drivers.values() {
            running.cancel.cancel();
        }
        for (_, running) in drivers {
            let _ = running.task.await;
        }
    }
}

fn installed(instance: &str, already_installed: bool) -> GatewayInstalled {
    GatewayInstalled {
        instance: instance.into(),
        stored: true,
        read_only: true,
        already_installed,
    }
}

impl AirzoneSetup for Manager {
    fn list(&self) -> BoxFuture<'_, Result<GatewayList, SetupError>> {
        Box::pin(async move {
            self.available()?;
            let state = self
                .0
                .state
                .try_lock()
                .map_err(|_| SetupError::new("busy"))?;
            let drivers = self
                .0
                .drivers
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let gateways = self
                .effective(&state)
                .into_iter()
                .map(|gateway| {
                    let mac = self.known_mac(&gateway);
                    GatewayStatus {
                        running: drivers
                            .get(&gateway.instance)
                            .is_some_and(|running| !running.task.is_finished()),
                        instance: gateway.instance,
                        host: gateway.config.host,
                        port: gateway.config.port,
                        tls_fingerprint: gateway.config.tls_fingerprint.map(|pin| {
                            pin.chars()
                                .filter(|c| *c != ':' && !c.is_ascii_whitespace())
                                .collect::<String>()
                                .to_ascii_lowercase()
                        }),
                        mac,
                        enabled: gateway.enabled,
                        source: gateway.source,
                        read_only: gateway.config.read_only,
                    }
                })
                .collect();
            Ok(GatewayList { gateways })
        })
    }
    fn probe(&self, request: GatewayRequest) -> BoxFuture<'_, Result<GatewayPreview, SetupError>> {
        Box::pin(async move {
            self.available()?;
            let request = normalize(request)?;
            let state = self
                .0
                .state
                .clone()
                .try_lock_owned()
                .map_err(|_| SetupError::new("busy"))?;
            let report = self.verified(&state, &request, None).await?;
            Ok(GatewayPreview {
                instance: request.instance,
                mac: report.mac,
                gateway_type: report.gateway_type,
                api_version: report.api_version,
                firmware: report.firmware,
                systems: report.systems,
                zones: report.zones,
                incomplete: report.incomplete,
                read_only: true,
            })
        })
    }
    fn install(
        &self,
        request: GatewayRequest,
        actor: String,
    ) -> BoxFuture<'_, Result<GatewayInstalled, SetupError>> {
        Box::pin(async move {
            self.available()?;
            let request = normalize(request)?;
            if request.expected_mac.is_none() {
                return Err(SetupError::new("expected_identity_required"));
            }
            let this = self.clone();
            self.owned(async move { this.commit(request, actor).await })
                .await
        })
    }

    fn probe_update(
        &self,
        instance: String,
        request: GatewayRequest,
    ) -> BoxFuture<'_, Result<GatewayPreview, SetupError>> {
        Box::pin(async move {
            self.available()?;
            let request = normalize_connection(request)?;
            if instance != request.instance {
                return Err(SetupError::new("instance_mismatch"));
            }
            let state = self
                .0
                .state
                .clone()
                .try_lock_owned()
                .map_err(|_| SetupError::new("busy"))?;
            let report = self.verified_update(&state, &request).await?;
            Ok(GatewayPreview {
                instance,
                mac: report.mac,
                gateway_type: report.gateway_type,
                api_version: report.api_version,
                firmware: report.firmware,
                systems: report.systems,
                zones: report.zones,
                incomplete: report.incomplete,
                read_only: true,
            })
        })
    }

    fn update(
        &self,
        instance: String,
        request: GatewayRequest,
        actor: String,
    ) -> BoxFuture<'_, Result<GatewayInstalled, SetupError>> {
        Box::pin(async move {
            let request = normalize_connection(request)?;
            if instance != request.instance {
                return Err(SetupError::new("instance_mismatch"));
            }
            if request.expected_mac.is_none() {
                return Err(SetupError::new("expected_identity_required"));
            }
            let this = self.clone();
            self.owned(async move { this.commit_update(request, actor).await })
                .await
        })
    }

    fn withdraw(
        &self,
        instance: String,
        actor: String,
    ) -> BoxFuture<'_, Result<GatewayWithdrawn, SetupError>> {
        Box::pin(async move {
            let this = self.clone();
            self.owned(async move { this.commit_withdraw(instance, actor).await })
                .await
        })
    }
}

fn save(path: &Path, store: &Store, expected: Option<&[u8]>) -> std::io::Result<Vec<u8>> {
    if bytes(path)?.as_deref() != expected {
        return Err(std::io::Error::other(
            "Airzone registry changed outside this operation",
        ));
    }
    let raw = serde_json::to_vec_pretty(store).map_err(std::io::Error::other)?;
    if raw.len() as u64 > MAX_BYTES {
        return Err(std::io::Error::other("Airzone registry exceeds its bound"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing registry directory"))?;
    let aside = parent.join(format!(
        ".airzone-gateways-{}-{}.new",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&aside)?;
        file.write_all(&raw)?;
        file.sync_all()?;
        std::fs::rename(&aside, path)?;
        // File contents are synced before the atomic replacement. A best-effort
        // directory sync also retains the rename across a power failure.
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&aside);
    }
    result.map(|()| raw)
}

#[cfg(test)]
mod tests;
