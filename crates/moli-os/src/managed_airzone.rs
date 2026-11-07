//! Gateways managed by a person. A bounded sidecar leaves the hand-written
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
    AirzoneSetup, GatewayInstalled, GatewayList, GatewayModeChanged, GatewayModeRequest,
    GatewayPreview, GatewayRequest, GatewayStatus, GatewayWithdrawn, SetupError,
};
use moli_core::{InstanceId, Outcome, Value};
use moli_runtime::{BoxFuture, Hub, spawn_driver};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, DriverConfig};

mod discovery;

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
        options.insert("read_only".into(), self.read_only.into());
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
/// enabled and command states are overlaid, so unrelated native options survive edits.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GatewayOverride {
    instance: String,
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection: Option<Gateway>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    read_only: Option<bool>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            version: 3,
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

/// Restrictions from the original TOML, captured before any sidecar overlay.
#[derive(Clone, Copy, Debug, Default)]
struct StaticPolicy {
    disabled: bool,
    read_only: bool,
}
impl StaticPolicy {
    fn from_driver(driver: &DriverConfig) -> Self {
        Self {
            disabled: driver.enabled == Some(false),
            read_only: driver
                .options
                .get("read_only")
                .and_then(toml::Value::as_bool)
                == Some(true),
        }
    }
    const fn reason(self) -> Option<&'static str> {
        if self.disabled {
            Some("disabled")
        } else if self.read_only {
            Some("read_only")
        } else {
            None
        }
    }
}

/// Parsing is bounded, but entries are independent: one broken connection must
/// not take the house down. A degraded registry is never eligible for rewriting.
#[derive(Clone, Debug)]
pub(crate) struct Registry {
    store: Store,
    raw: Option<Vec<u8>>,
    editable: bool,
    suppressed: HashSet<String>,
    suppress_all: bool,
    static_policy: HashMap<String, StaticPolicy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStore {
    version: u8,
    gateways: Vec<serde_json::Value>,
    #[serde(default)]
    overrides: Vec<serde_json::Value>,
}

impl Registry {
    fn skip(&mut self, instance: &str, reason: &str) {
        self.editable = false;
        tracing::warn!(
            instance,
            reason,
            "ignoring managed Airzone entry; registry retained, edits disabled until repair and restart"
        );
    }

    fn reconcile_overrides(&mut self, configured: &mut [StaticGateway]) {
        let overrides = std::mem::take(&mut self.store.overrides);
        for entry in overrides {
            let policy = self
                .static_policy
                .get(&entry.instance)
                .copied()
                .unwrap_or_default();
            if policy.disabled {
                // Disabled TOML may intentionally omit even required options.
                self.store.overrides.push(entry);
                continue;
            }
            let current = configured
                .iter_mut()
                .find(|gateway| gateway.instance == entry.instance);
            let matches = current.as_ref().is_some_and(|current| {
                current.enabled == entry.enabled
                    && entry
                        .mac
                        .as_ref()
                        .is_none_or(|mac| current.config.mac.as_ref() == Some(mac))
                    && entry.connection.as_ref().is_none_or(|connection| {
                        current.config.host == connection.host
                            && current.config.port == connection.port
                            && current.config.mac.as_deref() == Some(&connection.mac)
                            && current.config.tls_fingerprint == connection.tls_fingerprint
                            && current.config.read_only
                                == (entry.read_only.unwrap_or(true) || policy.read_only)
                    })
                    && entry.read_only.is_none_or(|read_only| {
                        current.config.read_only == (read_only || policy.read_only)
                    })
            });
            if matches && !self.suppress_all {
                self.store.overrides.push(entry);
            } else {
                self.skip(
                    &entry.instance,
                    "override was not loaded or changed during startup",
                );
                if let Some(current) = current {
                    current.enabled = false;
                }
            }
        }
    }

    fn unreadable(&mut self) {
        self.editable = false;
        self.suppress_all = true;
        tracing::warn!(
            "cannot read managed Airzone registry; Airzone paused, other drivers unaffected; retain or restore the file before restarting"
        );
    }
}

fn valid_gateway(gateway: &Gateway) -> bool {
    normalize(gateway.request()).is_ok_and(|request| request == gateway.request())
}

fn valid_override(entry: &GatewayOverride) -> bool {
    !entry.instance.is_empty()
        && (entry.read_only != Some(false) || entry.mac.is_some() || entry.connection.is_some())
        && entry
            .mac
            .as_deref()
            .is_none_or(|mac| moli_net::lan::mac_hex(mac).as_deref() == Some(mac))
        && entry.connection.as_ref().is_none_or(|gateway| {
            let request = gateway.request();
            gateway.instance == entry.instance
                && gateway.read_only
                && entry.mac.as_ref().is_none_or(|mac| mac == &gateway.mac)
                && normalize_connection(request.clone())
                    .is_ok_and(|normalized| normalized == request)
        })
}

/// Include recognizable claims from malformed entries, too. In particular an
/// invalid withdrawal cannot lose to an enabled duplicate earlier in the file.
fn conflicting_claims(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    let id = left.get("instance").and_then(serde_json::Value::as_str);
    if id.is_some() && id == right.get("instance").and_then(serde_json::Value::as_str) {
        return true;
    }
    let a = left
        .get("connection")
        .filter(|value| value.is_object())
        .unwrap_or(left);
    let b = right
        .get("connection")
        .filter(|value| value.is_object())
        .unwrap_or(right);
    let mac = |entry: &serde_json::Value| {
        entry
            .get("mac")
            .and_then(serde_json::Value::as_str)
            .and_then(moli_net::lan::mac_hex)
    };
    let identities = [mac(left), mac(a)];
    let other_identities = [mac(right), mac(b)];
    if identities.iter().flatten().any(|identity| {
        other_identities
            .iter()
            .flatten()
            .any(|other| other == identity)
    }) {
        return true;
    }
    let host = a.get("host").and_then(serde_json::Value::as_str);
    let port = a.get("port").and_then(serde_json::Value::as_u64);
    left.get("enabled").and_then(serde_json::Value::as_bool) != Some(false)
        && right.get("enabled").and_then(serde_json::Value::as_bool) != Some(false)
        && host.is_some()
        && port.is_some()
        && host == b.get("host").and_then(serde_json::Value::as_str)
        && port == b.get("port").and_then(serde_json::Value::as_u64)
}

fn read_store(path: &Path) -> Registry {
    let mut registry = Registry {
        store: Store::default(),
        raw: None,
        editable: true,
        suppressed: HashSet::new(),
        suppress_all: false,
        static_policy: HashMap::new(),
    };
    let Ok(raw) = bytes(path) else {
        registry.unreadable();
        return registry;
    };
    let Some(data) = raw.as_deref() else {
        return registry;
    };
    let parsed = serde_json::from_slice::<RawStore>(data);
    registry.raw = raw;
    let Ok(parsed) = parsed else {
        registry.unreadable();
        return registry;
    };
    if !matches!(parsed.version, 1..=3)
        || parsed.gateways.len() + parsed.overrides.len() > MAX_GATEWAYS
        || (parsed.version == 1 && !parsed.overrides.is_empty())
    {
        registry.unreadable();
        return registry;
    }
    registry.store.version = parsed.version;
    let entries: Vec<_> = parsed.gateways.iter().chain(&parsed.overrides).collect();
    for (index, raw) in entries.iter().enumerate() {
        let is_override = index >= parsed.gateways.len();
        let instance = raw
            .get("instance")
            .and_then(serde_json::Value::as_str)
            .filter(|instance| !instance.is_empty());
        let ambiguous = entries
            .iter()
            .enumerate()
            .any(|(other, entry)| other != index && conflicting_claims(raw, entry));
        let accepted = if ambiguous {
            false
        } else if is_override {
            serde_json::from_value::<GatewayOverride>((*raw).clone())
                .ok()
                .filter(valid_override)
                .filter(|entry| parsed.version >= 3 || entry.read_only.is_none())
                .is_some_and(|entry| {
                    registry.store.overrides.push(entry);
                    true
                })
        } else {
            serde_json::from_value::<Gateway>((*raw).clone())
                .ok()
                .filter(valid_gateway)
                // A false mode was invalid in older registries. Upgrading the
                // executable must never turn that formerly rejected entry on.
                .filter(|entry| parsed.version >= 3 || entry.read_only)
                .is_some_and(|entry| {
                    registry.store.gateways.push(entry);
                    true
                })
        };
        if !accepted {
            registry.skip(
                instance.unwrap_or("<unknown>"),
                "malformed or ambiguous entry",
            );
            if let Some(instance) = instance {
                registry.suppressed.insert(instance.to_owned());
            } else if is_override {
                // The withdrawal's owner cannot be identified safely.
                registry.suppress_all = true;
            }
        }
    }
    if registry.suppress_all {
        // An unidentified withdrawal may own a managed gateway, too. Pause the
        // accepted in-memory entries without changing the original file.
        for gateway in &mut registry.store.gateways {
            gateway.enabled = false;
        }
    }
    registry
}

fn overlay(
    driver: &DriverConfig,
    entry: &GatewayOverride,
    policy: StaticPolicy,
) -> anyhow::Result<DriverConfig> {
    ensure!(
        driver.kind == "airzone",
        "override targets a different driver kind"
    );
    let mut options = driver.options.clone();
    let table = options.as_table_mut().context("invalid Airzone options")?;
    if let Some(mac) = &entry.mac {
        table.insert("mac".into(), mac.clone().into());
    }
    if let Some(gateway) = &entry.connection {
        table.insert("host".into(), gateway.host.clone().into());
        table.insert("port".into(), i64::from(gateway.port).into());
        table.insert("mac".into(), gateway.mac.clone().into());
        table.insert("read_only".into(), true.into());
        table.remove("tls_fingerprint");
        if let Some(pin) = &gateway.tls_fingerprint {
            table.insert("tls_fingerprint".into(), pin.clone().into());
        }
    }
    if let Some(read_only) = entry.read_only {
        table.insert("read_only".into(), read_only.into());
    }
    if policy.read_only {
        table.insert("read_only".into(), true.into());
    }
    moli_airzone::Airzone::new(options.clone().try_into()?)?;
    Ok(DriverConfig {
        id: driver.id.clone(),
        kind: driver.kind.clone(),
        enabled: Some(entry.enabled),
        options,
    })
}

fn duplicates(candidate: &DriverConfig, drivers: &[DriverConfig], except: Option<usize>) -> bool {
    let Ok(current) = candidate.options.clone().try_into::<moli_airzone::Config>() else {
        return true;
    };
    drivers.iter().enumerate().any(|(index, other)| {
        if Some(index) == except {
            return false;
        }
        if candidate.id == other.id {
            return true;
        }
        if other.kind != "airzone" {
            return false;
        }
        let Ok(other_config) = other.options.clone().try_into::<moli_airzone::Config>() else {
            return false;
        };
        (candidate.enabled != Some(false)
            && other.enabled != Some(false)
            && current.host == other_config.host
            && current.port == other_config.port)
            || current
                .mac
                .as_deref()
                .and_then(moli_net::lan::mac_hex)
                .is_some_and(|mac| {
                    other_config
                        .mac
                        .as_deref()
                        .and_then(moli_net::lan::mac_hex)
                        .as_deref()
                        == Some(&mac)
                })
    })
}

/// Called before validating/spawning drivers. Retain the original registry on
/// disk, pause only ambiguous Airzone entries, and keep unrelated drivers alive.
#[allow(clippy::unnecessary_wraps)] // Keep the configuration loader's fallible hook.
pub fn load_into(config: &mut Config) -> anyhow::Result<()> {
    let mut registry = read_store(&config.server.data_dir.join(FILE));
    registry.static_policy = config
        .drivers
        .iter()
        .filter(|driver| driver.kind == "airzone")
        .map(|driver| (driver.id.clone(), StaticPolicy::from_driver(driver)))
        .collect();
    for driver in &mut config.drivers {
        if driver.kind == "airzone"
            && (registry.suppress_all || registry.suppressed.contains(&driver.id))
        {
            driver.enabled = Some(false);
        }
    }
    for entry in std::mem::take(&mut registry.store.overrides) {
        let Some(index) = config
            .drivers
            .iter()
            .position(|driver| driver.id == entry.instance)
        else {
            registry.skip(
                &entry.instance,
                "override no longer has a configured driver",
            );
            continue;
        };
        let policy = registry
            .static_policy
            .get(&entry.instance)
            .copied()
            .unwrap_or_default();
        if policy.disabled {
            // Keep a persisted revocation visible without validating options
            // of a driver the static configuration explicitly disables.
            if entry.read_only == Some(true)
                && let Some(table) = config.drivers[index].options.as_table_mut()
            {
                table.insert("read_only".into(), true.into());
            }
            registry.store.overrides.push(entry);
            continue;
        }
        let candidate = overlay(&config.drivers[index], &entry, policy);
        match candidate {
            Ok(candidate)
                if !duplicates(&candidate, &config.drivers, Some(index))
                    && !registry.suppress_all =>
            {
                config.drivers[index] = candidate;
                registry.store.overrides.push(entry);
            }
            _ => {
                registry.skip(&entry.instance, "invalid or conflicting override");
                if config.drivers[index].kind == "airzone" {
                    config.drivers[index].enabled = Some(false);
                }
            }
        }
    }
    for gateway in std::mem::take(&mut registry.store.gateways) {
        let candidate = gateway.driver();
        if duplicates(&candidate, &config.drivers, None) {
            registry.skip(
                &gateway.instance,
                "gateway conflicts with configured driver",
            );
        } else {
            config.drivers.push(candidate);
            registry.store.gateways.push(gateway);
        }
    }
    config.managed_airzone = Some(registry);
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

fn normalize_connection(request: GatewayRequest) -> Result<GatewayRequest, SetupError> {
    // Loopback is solely for in-process unit-test controllers. It is never
    // allowed in a production build, including through a saved override.
    validate_connection(request, cfg!(test))
}

fn validate_connection(
    mut request: GatewayRequest,
    test_loopback: bool,
) -> Result<GatewayRequest, SetupError> {
    let ip = request
        .host
        .parse::<Ipv4Addr>()
        .map_err(|_| SetupError::new("invalid_host"))?;
    if !(ip.is_private() || (test_loopback && ip.is_loopback())) {
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
        write: moli_airzone::WriteMode::Legacy,
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
    policy: StaticPolicy,
    instance: String,
    config: moli_airzone::Config,
    enabled: bool,
}
impl StaticGateway {
    fn load(driver: &DriverConfig, registry: &Registry) -> anyhow::Result<Option<Self>> {
        if driver.kind != "airzone" {
            return Ok(None);
        }
        let native = match driver.options.clone().try_into::<moli_airzone::Config>() {
            Ok(config) => config,
            Err(_) if driver.enabled == Some(false) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(Self {
            policy: registry
                .static_policy
                .get(&driver.id)
                .copied()
                .unwrap_or_else(|| StaticPolicy::from_driver(driver)),
            instance: driver.id.clone(),
            config: native,
            enabled: driver.enabled != Some(false)
                && !registry.suppress_all
                && !registry.suppressed.contains(&driver.id),
        }))
    }
}
struct Running {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
struct EffectiveGateway {
    policy: StaticPolicy,
    instance: String,
    config: moli_airzone::Config,
    enabled: bool,
    source: &'static str,
    withdrawn: bool,
}
struct State {
    store: Store,
    raw: Option<Vec<u8>>,
    editable: bool,
}
impl State {
    fn editable(&self) -> Result<(), SetupError> {
        if self.editable {
            Ok(())
        } else {
            Err(SetupError::new("persistence_failed"))
        }
    }
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
    discovery: discovery::Gate,
    discovery_cancel: CancellationToken,
    discovery_connections: Vec<(InstanceId, Ipv4Addr)>,
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
        let mut registry = config
            .managed_airzone
            .clone()
            .unwrap_or_else(|| read_store(&path));
        // Preserve the loader's accepted subset, including collision decisions.
        // If the file changes before serve, no withdrawal state can be trusted.
        if !bytes(&path).is_ok_and(|current| current == registry.raw) {
            registry.unreadable();
            registry.store = Store::default();
        }
        // Only entries actually reconciled into this configuration may start.
        let gateways = std::mem::take(&mut registry.store.gateways);
        for saved in gateways {
            if config.drivers.iter().any(|driver| {
                driver.id == saved.instance
                    && driver.kind == "airzone"
                    && (driver.enabled != Some(false)) == saved.enabled
                    && driver.options == saved.driver().options
            }) {
                registry.store.gateways.push(saved);
            } else {
                registry.skip(
                    &saved.instance,
                    "gateway was not loaded or changed during startup",
                );
                registry.suppressed.insert(saved.instance);
            }
        }
        let mut reserved = HashSet::new();
        let mut configured = Vec::new();
        for driver in &config.drivers {
            if registry
                .store
                .gateways
                .iter()
                .any(|saved| saved.instance == driver.id)
            {
                continue;
            }
            reserved.insert(driver.id.clone());
            if let Some(gateway) = StaticGateway::load(driver, &registry)? {
                configured.push(gateway);
            }
        }
        registry.reconcile_overrides(&mut configured);
        let Registry {
            store,
            raw,
            editable,
            ..
        } = registry;
        Ok(Self(Arc::new(Inner {
            hub,
            path,
            reserved,
            configured,
            state: Arc::new(tokio::sync::Mutex::new(State {
                store,
                raw,
                editable,
            })),
            discovery_cancel: cancel.child_token(),
            cancel,
            accepting: AtomicBool::new(true),
            drivers: Mutex::default(),
            operations: Mutex::default(),
            discovery: discovery::Gate::default(),
            discovery_connections: config
                .drivers
                .iter()
                .filter_map(|driver| {
                    let host = driver.options.get("host").and_then(toml::Value::as_str)?;
                    Some((
                        InstanceId::from(driver.id.as_str()),
                        discovery::private_host(host)?,
                    ))
                })
                .take(512)
                .collect(),
        })))
    }

    fn available(&self) -> Result<(), SetupError> {
        if !self.0.accepting.load(Ordering::Acquire) || self.0.cancel.is_cancelled() {
            return Err(SetupError::new("shutting_down"));
        }
        Ok(())
    }

    async fn discover_with(
        &self,
        inspector: Arc<dyn discovery::Inspector>,
    ) -> Result<moli_api::airzone_setup::GatewayDiscovery, SetupError> {
        self.available()?;
        let _search = self.0.discovery.enter()?;
        // Copy the registry while holding its lock, then release it before
        // any network I/O. A search never blocks an installation or withdrawal.
        let known = self.list().await?.gateways;
        let hints = self.discovery_hints();
        discovery::search(known, hints, self.0.discovery_cancel.clone(), inspector).await
    }

    fn discovery_hints(&self) -> Vec<Ipv4Addr> {
        let mut hints = Vec::new();
        for (instance, configured) in &self.0.discovery_connections {
            if let Some(address) = self.0.hub.known_address(instance)
                && address.configured.parse::<Ipv4Addr>().ok() == Some(*configured)
                && let Some(host) = discovery::private_host(&address.host)
            {
                hints.push(host);
            }
            hints.push(*configured);
        }
        hints
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
                if let Some(read_only) = overlay.and_then(|entry| entry.read_only) {
                    config.read_only = read_only;
                }
                config.read_only |= saved.policy.read_only;
                EffectiveGateway {
                    policy: saved.policy,
                    instance: saved.instance.clone(),
                    config,
                    enabled: !saved.policy.disabled
                        && overlay.map_or(saved.enabled, |entry| entry.enabled),
                    source: "configuration",
                    withdrawn: overlay.is_some_and(|entry| !entry.enabled),
                }
            })
            .collect();
        gateways.extend(state.store.gateways.iter().map(|saved| EffectiveGateway {
            policy: StaticPolicy::default(),
            instance: saved.instance.clone(),
            config: {
                let mut config = native(&saved.request(), Some(saved.mac.clone()));
                config.read_only = saved.read_only;
                config
            },
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
        state.editable()?;
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
        state.editable()?;
        next.version = 3;
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
        state.editable()?;
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
                return Ok(installed(&request.instance, true, saved.read_only));
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
        Ok(installed(&request.instance, false, true))
    }

    async fn verified_update(
        &self,
        state: &State,
        request: &GatewayRequest,
    ) -> Result<moli_airzone::GatewayProbe, SetupError> {
        let existing = self.existing(state, &request.instance)?;
        if existing.policy.disabled {
            return Err(SetupError::new("configuration_locked"));
        }
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
        state.editable()?;
        let current = self.existing(&state, &request.instance)?;
        let replay = current.config.read_only
            && (state
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
                }));
        if replay {
            self.persisted(&state).await?;
            return Ok(installed(&request.instance, true, true));
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
                read_only: Some(true),
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
        Ok(installed(&request.instance, false, true))
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
        state.editable()?;
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
                read_only: None,
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

    fn mode_store(
        state: &State,
        current: &EffectiveGateway,
        read_only: bool,
        mac: Option<String>,
    ) -> Store {
        let mut next = state.store.clone();
        if let Some(saved) = next
            .gateways
            .iter_mut()
            .find(|saved| saved.instance == current.instance)
        {
            saved.read_only = read_only;
        } else if let Some(saved) = next
            .overrides
            .iter_mut()
            .find(|saved| saved.instance == current.instance)
        {
            saved.read_only = Some(read_only);
            if mac.is_some() {
                saved.mac = mac;
            }
        } else {
            next.overrides.push(GatewayOverride {
                instance: current.instance.clone(),
                enabled: current.enabled,
                mac,
                connection: None,
                read_only: Some(read_only),
            });
        }
        next
    }

    async fn commit_mode(
        &self,
        instance: String,
        request: GatewayModeRequest,
        actor: String,
    ) -> Result<GatewayModeChanged, SetupError> {
        self.available()?;
        let mut state = self
            .0
            .state
            .clone()
            .try_lock_owned()
            .map_err(|_| SetupError::new("busy"))?;
        state.editable()?;
        let current = self.existing(&state, &instance)?;
        if request.expected_read_only != current.config.read_only {
            return Err(SetupError::new("mode_conflict"));
        }
        if !request.read_only && current.policy.reason().is_some() {
            return Err(SetupError::new("configuration_locked"));
        }
        if !request.read_only && !current.enabled {
            return Err(SetupError::new("gateway_disabled"));
        }
        if request.read_only == current.config.read_only {
            self.persisted(&state).await?;
            return Ok(self.mode_result(&current, request.read_only));
        }
        let mac = self.known_mac(&current);
        if !request.read_only {
            let checked = GatewayRequest {
                instance: instance.clone(),
                host: current.config.host.clone(),
                port: current.config.port,
                tls_fingerprint: current.config.tls_fingerprint.clone(),
                expected_mac: Some(
                    mac.clone()
                        .ok_or_else(|| SetupError::new("identity_unavailable"))?,
                ),
            };
            self.verified(&state, &checked, Some(&instance)).await?;
            self.available()?;
        }
        let mut config = current.config.clone();
        config.read_only = request.read_only;
        config.mac = mac.clone();
        let driver =
            moli_airzone::Airzone::new(config).map_err(|_| SetupError::new("probe_failed"))?;
        let next = Self::mode_store(&state, &current, request.read_only, mac);
        let saved = self.persist(&mut state, next).await;
        if saved.is_ok() {
            // Acknowledgement waits for the former supervisor and queued
            // commands to finish; unregistering also revokes offline metadata.
            self.stop(&instance).await;
            if current.enabled {
                self.start(&instance, driver);
            }
        }
        self.0.hub.record_driver_configured(
            InstanceId::from(instance.as_str()),
            "airzone",
            current.enabled,
            request.read_only,
            Some(actor),
            if saved.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Err("persistence_failed".into())
            },
        );
        saved?;
        Ok(self.mode_result(&current, request.read_only))
    }

    fn mode_result(&self, gateway: &EffectiveGateway, read_only: bool) -> GatewayModeChanged {
        GatewayModeChanged {
            instance: gateway.instance.clone(),
            stored: true,
            read_only,
            running: self
                .0
                .drivers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&gateway.instance)
                .is_some_and(|running| !running.task.is_finished()),
        }
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
        self.0.discovery_cancel.cancel();
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

fn installed(instance: &str, already_installed: bool, read_only: bool) -> GatewayInstalled {
    GatewayInstalled {
        instance: instance.into(),
        stored: true,
        read_only,
        already_installed,
    }
}

impl AirzoneSetup for Manager {
    fn discover(
        &self,
    ) -> BoxFuture<'_, Result<moli_api::airzone_setup::GatewayDiscovery, SetupError>> {
        Box::pin(self.discover_with(Arc::new(discovery::Network)))
    }
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
                        configuration_lock: gateway.policy.reason(),
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

    fn set_mode(
        &self,
        instance: String,
        request: GatewayModeRequest,
        actor: String,
    ) -> BoxFuture<'_, Result<GatewayModeChanged, SetupError>> {
        Box::pin(async move {
            let this = self.clone();
            self.owned(async move { this.commit_mode(instance, request, actor).await })
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
