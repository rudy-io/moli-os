//! Read-only gateways added by a person. A bounded sidecar leaves the hand-written
//! configuration untouched. Persistence precedes registration; an owned task
//! finishes a committed installation even if the HTTP client disconnects.

use std::collections::HashSet;
use std::fmt;
use std::io::{Read as _, Write as _};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context as _, ensure};
use moli_api::airzone_setup::{
    AirzoneSetup, GatewayInstalled, GatewayPreview, GatewayRequest, SetupError,
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
            enabled: Some(true),
            options: toml::Value::Table(options),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u8,
    gateways: Vec<Gateway>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            gateways: Vec::new(),
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
        store.version == 1 && store.gateways.len() <= MAX_GATEWAYS,
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
                && endpoints.insert((&gateway.host, gateway.port))
                && macs.insert(&gateway.mac),
            "duplicate managed Airzone gateway"
        );
    }
    Ok((store, raw))
}

/// Called after resolving data_dir and before validating/spawning any driver.
/// A corrupt or conflicting file is an explicit startup error, never discarded.
pub fn load_into(config: &mut Config) -> anyhow::Result<()> {
    let (store, _) = read_store(&config.server.data_dir.join(FILE))?;
    for gateway in store.gateways {
        for driver in &config.drivers {
            ensure!(
                driver.id != gateway.instance,
                "managed Airzone instance conflicts with configured driver"
            );
            if driver.kind == "airzone" {
                let other: moli_airzone::Config = driver.options.clone().try_into()?;
                ensure!(
                    !(other.host == gateway.host && other.port == gateway.port)
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

fn normalize(mut request: GatewayRequest) -> Result<GatewayRequest, SetupError> {
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

#[derive(Debug)]
struct StaticGateway {
    instance: String,
    config: moli_airzone::Config,
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
    drivers: Mutex<Vec<JoinHandle<()>>>,
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
                        && driver.enabled != Some(false)
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
                        && driver.enabled != Some(false)
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
                });
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

    fn collision(
        &self,
        state: &State,
        request: &GatewayRequest,
        mac: Option<&str>,
    ) -> Result<(), SetupError> {
        let snapshot = self.0.hub.snapshot();
        if self.0.reserved.contains(&request.instance)
            || state
                .store
                .gateways
                .iter()
                .any(|g| g.instance == request.instance)
            || snapshot
                .drivers
                .iter()
                .any(|d| d.instance.as_str() == request.instance)
        {
            return Err(SetupError::conflict(
                "duplicate_instance",
                &request.instance,
            ));
        }
        for saved in &state.store.gateways {
            if (saved.host == request.host && saved.port == request.port)
                || mac == Some(saved.mac.as_str())
            {
                return Err(SetupError::conflict("duplicate_gateway", &saved.instance));
            }
        }
        for other in &self.0.configured {
            if other.config.host == request.host && other.config.port == request.port {
                return Err(SetupError::conflict("duplicate_gateway", &other.instance));
            }
            let known = other
                .config
                .mac
                .as_deref()
                .and_then(moli_net::lan::mac_hex)
                .or_else(|| {
                    snapshot
                        .devices
                        .iter()
                        .filter(|d| d.device.instance.as_str() == other.instance)
                        .find_map(|d| {
                            d.state.get("mac").and_then(|s| {
                                if let Value::Text(v) = &s.value {
                                    moli_net::lan::mac_hex(v)
                                } else {
                                    None
                                }
                            })
                        })
                });
            if let Some(mac) = mac {
                if known.as_deref() == Some(mac) {
                    return Err(SetupError::conflict("duplicate_gateway", &other.instance));
                }
                if known.is_none() {
                    return Err(SetupError::new("identity_unavailable"));
                }
            }
        }
        if let Some(mac) = mac {
            for device in &snapshot.devices {
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
    ) -> Result<moli_airzone::GatewayProbe, SetupError> {
        self.collision(state, request, None)?;
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
        self.collision(state, request, Some(&report.mac))?;
        Ok(report)
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
            if saved.request() == request {
                let path = self.0.path.clone();
                let expected = state.raw.clone();
                let persisted = tokio::task::spawn_blocking(move || {
                    bytes(&path).map(|current| current == expected)
                })
                .await
                .map_err(|_| SetupError::new("persistence_failed"))?
                .map_err(|_| SetupError::new("persistence_failed"))?;
                if !persisted {
                    return Err(SetupError::new("persistence_failed"));
                }
                return Ok(installed(&request.instance, true));
            }
            return Err(SetupError::conflict(
                "duplicate_instance",
                &request.instance,
            ));
        }
        if state.store.gateways.len() >= MAX_GATEWAYS {
            return Err(SetupError::new("limit_reached"));
        }
        let report = self.verified(&state, &request).await?;
        self.available()?;
        let gateway = Gateway {
            instance: request.instance.clone(),
            host: request.host.clone(),
            port: request.port,
            tls_fingerprint: request.tls_fingerprint.clone(),
            mac: report.mac,
            read_only: true,
        };
        let driver = moli_airzone::Airzone::new(native(&request, Some(gateway.mac.clone())))
            .map_err(|_| SetupError::new("probe_failed"))?;
        let mut next = state.store.clone();
        next.gateways.push(gateway);
        let path = self.0.path.clone();
        let expected = state.raw.clone();
        let saved = next.clone();
        let result = tokio::task::spawn_blocking(move || save(&path, &saved, expected.as_deref()))
            .await
            .map_err(std::io::Error::other)
            .and_then(std::convert::identity);
        let Ok(raw) = result else {
            self.0.hub.record_driver_added(
                InstanceId::from(request.instance.as_str()),
                "airzone",
                true,
                Some(actor),
                Outcome::Err("persistence_failed".into()),
            );
            return Err(SetupError::new("persistence_failed"));
        };
        state.store = next;
        state.raw = Some(raw);
        // No await between the committed state and synchronous registration.
        let task = spawn_driver(
            &self.0.hub,
            InstanceId::from(request.instance.as_str()),
            Arc::new(driver),
            self.0.cancel.clone(),
        );
        self.0
            .drivers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(task);
        self.0.hub.record_driver_added(
            InstanceId::from(request.instance.as_str()),
            "airzone",
            true,
            Some(actor),
            Outcome::Ok,
        );
        Ok(installed(&request.instance, false))
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
        for task in drivers {
            let _ = task.await;
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
            let report = self.verified(&state, &request).await?;
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
            let (send, receive) = tokio::sync::oneshot::channel();
            // Admission and tracking are atomic with respect to shutdown's drain.
            {
                let mut operations = self
                    .0
                    .operations
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                self.available()?;
                operations.retain(|task| !task.is_finished());
                operations.push(tokio::spawn(async move {
                    let result = this.commit(request, actor).await;
                    let _ = send.send(result);
                }));
            }
            receive.await.map_err(|_| SetupError::new("unavailable"))?
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
