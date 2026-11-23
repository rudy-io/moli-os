//! Devices a person added from the dashboard by their address (`data/devices-added.json`).
//!
//! The hand-written configuration stays untouched: this sidecar lists, for
//! each one, its instance, the built-in profile Moli recognized it as, its
//! address and whether it has a key. Only a profile that recognizes its
//! devices (`recognize`) is accepted, only at a private IPv4 address. A file
//! that cannot be read is never rewritten: what it lists is not started, the
//! rest of the house runs, and adding waits for a person to repair it.

use std::collections::BTreeSet;
use std::io::{Read as _, Write as _};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use moli_api::device_setup::{
    DeviceInstall, DeviceInstalled, DevicePreview, DeviceProbe, DeviceSetup, SetupError,
};
use moli_core::{DeviceId, InstanceId, Origin, Outcome};
use moli_profile::{KeyUse, Recognition, Recognized};
use moli_runtime::{BoxFuture, Hub, LabelPatch, spawn_driver};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, DriverConfig};

pub const FILE: &str = "devices-added.json";
const VERSION: u8 = 1;
const MAX_DEVICES: usize = 64;
const MAX_BYTES: u64 = 65_536;
/// How long an addition waits for the new device to answer, to give it its
/// name and room.
const LABEL_WAIT: Duration = Duration::from_secs(20);
/// One probe asks a few messages, each bounded at 5 s.
const PROBE_DEADLINE: Duration = Duration::from_secs(45);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Added {
    instance: String,
    profile: String,
    host: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    empty_secret: bool,
}

impl Added {
    /// `port`: tests only (their fake device listens elsewhere than port
    /// 80), never read from the file.
    fn driver(&self, port: Option<u16>) -> DriverConfig {
        let mut options = toml::Table::new();
        options.insert("profile".into(), self.profile.clone().into());
        options.insert("host".into(), self.host.clone().into());
        if self.empty_secret {
            options.insert("empty_secret".into(), true.into());
        }
        if let Some(port) = port {
            options.insert("port".into(), i64::from(port).into());
        }
        DriverConfig {
            id: self.instance.clone(),
            kind: "profile".into(),
            enabled: None,
            options: toml::Value::Table(options),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u8,
    devices: Vec<Added>,
}

/// A private IPv4 of the home network, written plainly (`192.168.1.20`).
fn private_host(host: &str) -> Option<Ipv4Addr> {
    let ip: Ipv4Addr = host.trim().parse().ok()?;
    // The loopback only for the fake devices of the unit tests.
    let allowed = ip.is_private() || (cfg!(test) && ip.is_loopback());
    (allowed && ip.to_string() == host.trim()).then_some(ip)
}

fn valid_instance(instance: &str) -> bool {
    let mut chars = instance.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && instance.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// What the file lists, and whether it may be rewritten.
fn read_store(path: &Path) -> (Vec<Added>, bool) {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), true),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the devices added from the dashboard: none started, adding paused");
            return (Vec::new(), false);
        }
    };
    let mut data = Vec::new();
    if file.take(MAX_BYTES + 1).read_to_end(&mut data).is_err() || data.len() as u64 > MAX_BYTES {
        tracing::warn!(
            "the devices added from the dashboard: unreadable or too big; none started, adding paused"
        );
        return (Vec::new(), false);
    }
    match serde_json::from_slice::<Store>(&data) {
        Ok(store) if store.version == VERSION && store.devices.len() <= MAX_DEVICES => {
            (store.devices, true)
        }
        _ => {
            tracing::warn!(
                "the devices added from the dashboard: not a file Moli wrote; none started, adding paused"
            );
            (Vec::new(), false)
        }
    }
}

/// Written aside, flushed, then put in place: a crash leaves the old file
/// or the new one, never half of one.
fn write_store(path: &Path, devices: &[Added]) -> std::io::Result<()> {
    let store = Store {
        version: VERSION,
        devices: devices.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&store).map_err(std::io::Error::other)?;
    let aside = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&aside)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&aside, path)?;
    if let Some(dir) = path.parent()
        && let Ok(dir) = std::fs::File::open(dir)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// The secret a built-in profile signs with (`key`), if any.
fn secret_of(profile: &str) -> Option<String> {
    moli_profile::open(profile).ok()?.0.meta.secret
}

/// A profile driver of the hand-written configuration: its instance, profile, host.
#[derive(Clone, Debug)]
struct Configured {
    instance: String,
    profile: Option<String>,
    host: Option<String>,
    empty_secret: bool,
}

#[derive(Debug)]
struct State {
    devices: Vec<Added>,
    editable: bool,
    tasks: Vec<JoinHandle<()>>,
}

#[derive(Debug)]
struct Inner {
    hub: Hub,
    path: PathBuf,
    trust_file: PathBuf,
    cancel: CancellationToken,
    /// Every instance of the configuration (any kind): never reused.
    taken: BTreeSet<String>,
    configured: Vec<Configured>,
    state: tokio::sync::Mutex<State>,
    /// One probe at a time: each one asks a device of the network.
    probing: tokio::sync::Mutex<()>,
    /// Tests only: the port their fake devices listen on.
    port: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct Manager(Arc<Inner>);

impl Manager {
    /// What the file lists, started.
    pub fn start(hub: Hub, config: &Config, cancel: CancellationToken) -> Self {
        let manager = Self::new(hub, config, cancel);
        manager.start_added();
        manager
    }

    fn new(hub: Hub, config: &Config, cancel: CancellationToken) -> Self {
        let data_dir = &config.server.data_dir;
        let path = data_dir.join(FILE);
        let (listed, mut editable) = read_store(&path);
        let taken: BTreeSet<String> = config.drivers.iter().map(|d| d.id.clone()).collect();
        let configured = config
            .drivers
            .iter()
            .filter(|d| d.kind == "profile" && d.enabled != Some(false))
            .map(|d| {
                let text = |key: &str| {
                    d.options
                        .get(key)
                        .and_then(toml::Value::as_str)
                        .map(str::to_owned)
                };
                Configured {
                    instance: d.id.clone(),
                    profile: text("profile"),
                    host: text("host"),
                    empty_secret: d.options.get("empty_secret").and_then(toml::Value::as_bool)
                        == Some(true),
                }
            })
            .collect();
        let mut devices = Vec::new();
        for added in listed {
            let duplicate = taken.contains(&added.instance)
                || devices.iter().any(|d: &Added| d.instance == added.instance);
            if !valid_instance(&added.instance)
                || duplicate
                || !moli_profile::addable(&added.profile)
                || private_host(&added.host).is_none()
            {
                tracing::warn!(instance = %added.instance, "ignoring a device added from the dashboard: invalid or taken; adding paused until it is repaired");
                editable = false;
                continue;
            }
            devices.push(added);
        }
        Self(Arc::new(Inner {
            hub,
            path,
            trust_file: data_dir.join(moli_profile::TRUST_FILE),
            cancel,
            taken,
            configured,
            state: tokio::sync::Mutex::new(State {
                devices,
                editable,
                tasks: Vec::new(),
            }),
            probing: tokio::sync::Mutex::new(()),
            port: None,
        }))
    }

    fn spawn(&self, added: &Added) -> anyhow::Result<JoinHandle<()>> {
        let options = added.driver(self.0.port).options.try_into()?;
        let driver = moli_profile::ProfileDriver::new(options, self.0.trust_file.clone())?;
        Ok(spawn_driver(
            &self.0.hub,
            InstanceId::from(added.instance.as_str()),
            Arc::new(driver),
            self.0.cancel.clone(),
        ))
    }

    /// Starts what the file lists (at boot: nobody holds the state yet).
    fn start_added(&self) {
        let Ok(mut state) = self.0.state.try_lock() else {
            return;
        };
        let devices = state.devices.clone();
        for added in &devices {
            match self.spawn(added) {
                Ok(task) => state.tasks.push(task),
                Err(e) => {
                    tracing::warn!(instance = %added.instance, error = %e, "cannot start a device added from the dashboard");
                }
            }
        }
    }

    /// Waits for the drivers it started (after the cancellation).
    pub async fn shutdown(&self) {
        let tasks = std::mem::take(&mut self.0.state.lock().await.tasks);
        for task in tasks {
            let _ = task.await;
        }
    }

    /// The keys the house holds for the devices that sign: `(instance, secret, value)`.
    fn known_keys(&self, devices: &[Added]) -> Vec<(String, String, String)> {
        let configured = self
            .0
            .configured
            .iter()
            .filter(|c| !c.empty_secret)
            .filter_map(|c| {
                let profile = c.profile.as_deref()?;
                moli_profile::addable(profile).then(|| (c.instance.clone(), profile.to_owned()))
            });
        let added = devices
            .iter()
            .filter(|d| !d.empty_secret)
            .map(|d| (d.instance.clone(), d.profile.clone()));
        let mut keys: Vec<(String, String, String)> = Vec::new();
        for (instance, profile) in configured.chain(added) {
            let Some(secret) = secret_of(&profile) else {
                continue;
            };
            if let Some(value) = self
                .0
                .hub
                .secret(&InstanceId::from(instance.as_str()), &secret)
                && !value.is_empty()
                && !keys.iter().any(|(_, _, known)| *known == value)
            {
                keys.push((instance, secret, value));
            }
        }
        keys
    }

    /// Who answers at `host`, with the keys the house holds and `given`.
    async fn recognize(
        &self,
        host: Ipv4Addr,
        devices: &[Added],
        given: Option<&str>,
    ) -> Result<(Recognition, Vec<String>, usize), SetupError> {
        let Ok(_probing) = self.0.probing.try_lock() else {
            return Err(SetupError::new("busy"));
        };
        let mut keys: Vec<String> = self
            .known_keys(devices)
            .into_iter()
            .map(|(_, _, value)| value)
            .collect();
        let held = keys.len();
        if let Some(given) = given {
            keys.push(given.to_owned());
        }
        let recognized = tokio::time::timeout(
            PROBE_DEADLINE,
            moli_profile::recognize(host, self.0.port, &keys),
        )
        .await
        .map_err(|_| SetupError::new("probe_failed"))?;
        Ok((recognized, keys, held))
    }

    /// The instance that already runs this device or this address.
    fn existing(
        &self,
        host: &str,
        identity: &str,
        devices: &[Added],
    ) -> Option<(String, Option<String>)> {
        let snapshot = self.0.hub.snapshot();
        if let Some(device) = snapshot.devices.iter().find(|d| {
            d.device
                .id
                .as_str()
                .split_once(':')
                .is_some_and(|(_, native)| native == identity)
        }) {
            return Some((
                device.device.instance.as_str().to_owned(),
                Some(device.device.id.as_str().to_owned()),
            ));
        }
        // Where each instance's device really is: a device the profile
        // driver followed to a new address leaves its configured one free
        // (another device may have it now).
        let here = |instance: &str, configured: &str| {
            let at = self
                .0
                .hub
                .known_address(&InstanceId::from(instance))
                .filter(|known| known.configured == configured)
                .map_or_else(|| configured.to_owned(), |known| known.host);
            at == host
        };
        self.0
            .configured
            .iter()
            .filter(|c| c.host.as_deref().is_some_and(|h| here(&c.instance, h)))
            .map(|c| c.instance.clone())
            .chain(
                devices
                    .iter()
                    .filter(|d| here(&d.instance, &d.host))
                    .map(|d| d.instance.clone()),
            )
            .next()
            .map(|instance| (instance, None))
    }

    fn next_instance(&self, profile: &str, devices: &[Added]) -> String {
        let running: BTreeSet<String> = self
            .0
            .hub
            .snapshot()
            .drivers
            .iter()
            .map(|d| d.instance.as_str().to_owned())
            .collect();
        (1..=MAX_DEVICES * 4)
            .map(|n| format!("{profile}-{n}"))
            .find(|candidate| {
                !self.0.taken.contains(candidate)
                    && !running.contains(candidate)
                    && !devices.iter().any(|d| d.instance == *candidate)
            })
            .unwrap_or_else(|| profile.to_owned())
    }

    async fn probe_now(&self, request: DeviceProbe) -> Result<DevicePreview, SetupError> {
        let host = private_host(&request.host).ok_or_else(|| SetupError::new("invalid_host"))?;
        // An addition holds the state while it asks the device again: a
        // probe meanwhile answers « busy » rather than wait for it.
        let Ok(state) = self.0.state.try_lock() else {
            return Err(SetupError::new("busy"));
        };
        let devices = state.devices.clone();
        drop(state);
        let (recognition, _, held) = self
            .recognize(host, &devices, request.key.as_deref())
            .await?;
        Ok(match recognition {
            Recognition::Recognized(found) => {
                let mut preview = preview(&found, held);
                if let Some((instance, device)) =
                    self.existing(&request.host, &found.identity, &devices)
                {
                    preview.existing_instance = Some(instance);
                    preview.existing_device = device;
                }
                preview
            }
            Recognition::NeedsKey { manufacturer, .. } => DevicePreview {
                status: "needs_key",
                manufacturer,
                ..DevicePreview::default()
            },
            Recognition::Unknown => DevicePreview {
                status: "unknown",
                ..DevicePreview::default()
            },
            Recognition::Silent => DevicePreview {
                status: "silent",
                ..DevicePreview::default()
            },
        })
    }

    #[allow(clippy::too_many_lines)] // one addition, in the order it must happen
    async fn install_now(
        &self,
        request: DeviceInstall,
        actor: String,
    ) -> Result<DeviceInstalled, SetupError> {
        if self.0.cancel.is_cancelled() {
            return Err(SetupError::new("shutting_down"));
        }
        let host = private_host(&request.host).ok_or_else(|| SetupError::new("invalid_host"))?;
        let mut state = self.0.state.lock().await;
        if !state.editable {
            return Err(SetupError::new("persistence_failed"));
        }
        if state.devices.len() >= MAX_DEVICES {
            return Err(SetupError::new("limit_reached"));
        }
        // Asked again: the device the person saw must be the one added.
        let (recognition, keys, held) = self
            .recognize(host, &state.devices, request.key.as_deref())
            .await?;
        let found = match recognition {
            Recognition::Recognized(found) => found,
            Recognition::NeedsKey { .. } => return Err(SetupError::new("needs_key")),
            Recognition::Silent => return Err(SetupError::new("silent")),
            Recognition::Unknown => return Err(SetupError::new("not_recognized")),
        };
        if found.identity != request.identity {
            return Err(SetupError::new("identity_changed"));
        }
        if let Some((instance, _)) = self.existing(&request.host, &found.identity, &state.devices) {
            return Err(SetupError::conflict("duplicate_device", &instance));
        }
        let instance = self.next_instance(found.profile, &state.devices);
        let id = InstanceId::from(instance.as_str());
        let added = Added {
            instance: instance.clone(),
            profile: found.profile.to_owned(),
            host: request.host.trim().to_owned(),
            empty_secret: found.key == KeyUse::Empty,
        };
        // The key first: the driver reads it as it starts.
        let secret = secret_of(found.profile);
        if let (KeyUse::Known(i), Some(secret)) = (found.key, &secret) {
            let stored = self.0.hub.store_secret(&id, secret, &keys[i]).await;
            if stored.is_err() {
                return Err(SetupError::new("persistence_failed"));
            }
        }
        let mut devices = state.devices.clone();
        devices.push(added.clone());
        if let Err(e) = write_store(&self.0.path, &devices) {
            tracing::warn!(error = %e, "cannot write the devices added from the dashboard");
            if let (KeyUse::Known(_), Some(secret)) = (found.key, &secret) {
                let _ = self.0.hub.take_secret(&id, secret).await;
            }
            self.0.hub.record_driver_added(
                id,
                "profile",
                false,
                Some(actor),
                Outcome::Err("not stored".into()),
            );
            return Err(SetupError::new("persistence_failed"));
        }
        let task = match self.spawn(&added) {
            Ok(task) => task,
            Err(e) => {
                // Not started: nothing stays of it, and the journal says so.
                tracing::warn!(instance = %instance, error = %e, "cannot start the device just added");
                match write_store(&self.0.path, &state.devices) {
                    Ok(()) => {
                        if let (KeyUse::Known(_), Some(secret)) = (found.key, &secret) {
                            let _ = self.0.hub.take_secret(&id, secret).await;
                        }
                    }
                    Err(e) => {
                        // Still listed, key kept: it starts with Moli next time.
                        tracing::warn!(error = %e, "cannot take back the device that did not start");
                        state.devices = devices;
                    }
                }
                self.0.hub.record_driver_added(
                    id,
                    "profile",
                    false,
                    Some(actor),
                    Outcome::Err("not started".into()),
                );
                return Err(SetupError::new("probe_failed"));
            }
        };
        state.devices = devices;
        state.tasks.push(task);
        drop(state);
        self.0.hub.record_driver_added(
            id.clone(),
            "profile",
            false,
            Some(actor.clone()),
            Outcome::Ok,
        );
        tracing::info!(%instance, profile = %found.profile, key = key_word(found.key, held), "device added from the dashboard");

        // Its name and room, once it answered.
        let device = DeviceId::new(&id, &found.identity);
        let deadline = tokio::time::Instant::now() + LABEL_WAIT;
        while self.0.hub.device(&device).is_none() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let room = request
            .room
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty());
        let labelled = self.0.hub.device(&device).is_some()
            && self
                .0
                .hub
                .set_label(
                    &device,
                    LabelPatch {
                        name: Some(request.name.trim().to_owned()),
                        room: room.map(str::to_owned),
                    },
                    Origin::Ui,
                    Some(actor),
                )
                .await
                .is_ok();
        Ok(DeviceInstalled {
            instance,
            device: device.as_str().to_owned(),
            labelled,
        })
    }
}

/// How the device took its key, said without the key.
const fn key_word(key: KeyUse, held: usize) -> &'static str {
    match key {
        KeyUse::NotNeeded => "none",
        KeyUse::Empty => "empty",
        KeyUse::Known(i) if i < held => "known",
        KeyUse::Known(_) => "given",
    }
}

fn preview(found: &Recognized, held: usize) -> DevicePreview {
    DevicePreview {
        status: "recognized",
        profile: Some(found.profile.to_owned()),
        name: Some(found.name.clone()),
        manufacturer: found.manufacturer.clone(),
        model: Some(found.model.clone()),
        identity: Some(found.identity.clone()),
        mac: found.mac.clone(),
        key: Some(key_word(found.key, held)),
        existing_instance: None,
        existing_device: None,
    }
}

impl DeviceSetup for Manager {
    fn probe(&self, request: DeviceProbe) -> BoxFuture<'_, Result<DevicePreview, SetupError>> {
        Box::pin(self.probe_now(request))
    }

    fn install(
        &self,
        request: DeviceInstall,
        actor: String,
    ) -> BoxFuture<'_, Result<DeviceInstalled, SetupError>> {
        // Its own task: a person who closes the page does not leave a
        // device half added.
        let this = self.clone();
        Box::pin(async move {
            tokio::spawn(async move { this.install_now(request, actor).await })
                .await
                .unwrap_or_else(|_| Err(SetupError::new("persistence_failed")))
        })
    }
}

#[cfg(test)]
impl Manager {
    /// Its devices at `port` (a fake one on the loopback).
    fn at_port(hub: Hub, config: &Config, cancel: CancellationToken, port: u16) -> Self {
        let manager = Self::new(hub, config, cancel);
        let mut inner = Arc::try_unwrap(manager.0).expect("just made");
        inner.port = Some(port);
        Self(Arc::new(inner))
    }
}

#[cfg(test)]
mod tests;
