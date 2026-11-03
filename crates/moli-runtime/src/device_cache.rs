//! Versioned, bounded inventory sidecar. The legacy state file stays readable
//! by older binaries. Each definition carries its own coherent sample snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::state_cache::{self, States};
use moli_core::{Device, DeviceId, Kind, Sample, Value};
use serde::{Deserialize, Serialize};

const VERSION: u8 = 1;
const MAX_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_DEVICES: usize = 4096;
const MAX_POINTS: usize = 1024;
const MAX_TEXT: usize = 8192;
const MAX_VALUE_TEXT: usize = 64 * 1024;

pub(crate) type Devices = BTreeMap<DeviceId, CachedDevice>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CachedDevice {
    pub kind: Arc<str>,
    pub integration: Arc<str>,
    pub device: Arc<Device>,
    pub states: BTreeMap<Arc<str>, Sample>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Inventory {
    version: u8,
    pub devices: Devices,
    /// Legacy samples not claimed by a current definition. Required even when
    /// empty: a committed deletion must override a stale legacy state file.
    pub unclaimed: States,
}

pub(crate) fn path(state_path: &Path) -> PathBuf {
    state_path.with_extension("devices.json")
}

pub(crate) fn load(state_path: &Path) -> io::Result<Option<Inventory>> {
    let file = match std::fs::File::open(path(state_path)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(u64::try_from(MAX_BYTES + 1).unwrap())
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("device cache exceeds 8 MiB"));
    }
    let cache: Inventory =
        serde_json::from_slice(&bytes).map_err(|error| invalid(&error.to_string()))?;
    if cache.version != VERSION {
        return Err(invalid("unsupported device cache version"));
    }
    validate(&cache.devices)?;
    validate_unclaimed(&cache.unclaimed)?;
    Ok(Some(cache))
}

/// The sidecar is authoritative while present. Retire an obsolete sidecar
/// before committing fallback values, so no reboot can prefer the old copy.
pub(crate) async fn save_snapshot(
    state_path: &Path,
    devices: Devices,
    unclaimed: States,
    states: &States,
) -> io::Result<()> {
    let Err(inventory_error) = save(state_path, devices, unclaimed).await else {
        return state_cache::save(state_path, states).await;
    };
    let inventory_path = path(state_path);
    let mut stale = inventory_path.as_os_str().to_owned();
    stale.push(".stale");
    let stale = PathBuf::from(stale);
    let moved = match tokio::fs::rename(&inventory_path, &stale).await {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(io::Error::other(format!(
                "inventory cache: {inventory_error}; could not quarantine stale inventory: {error}"
            )));
        }
    };
    if let Err(legacy_error) = state_cache::save(state_path, states).await {
        // A failed legacy replacement leaves the previous generation valid.
        // Put its inventory back; if that also fails, preserve it in .stale
        // and report every failure instead of claiming a successful fallback.
        if moved && let Err(restore_error) = tokio::fs::rename(&stale, &inventory_path).await {
            return Err(io::Error::other(format!(
                "inventory cache: {inventory_error}; state cache: {legacy_error}; could not restore quarantined inventory: {restore_error}"
            )));
        }
        return Err(io::Error::other(format!(
            "inventory cache: {inventory_error}; state cache: {legacy_error}"
        )));
    }
    Err(inventory_error)
}

async fn save(state_path: &Path, devices: Devices, unclaimed: States) -> io::Result<()> {
    validate(&devices)?;
    validate_unclaimed(&unclaimed)?;
    let mut writer = BoundedWriter(Vec::new());
    serde_json::to_writer(
        &mut writer,
        &Inventory {
            version: VERSION,
            devices,
            unclaimed,
        },
    )
    .map_err(io::Error::other)?;
    crate::labels::write_atomic(&path(state_path), &writer.0).await
}

fn validate_unclaimed(states: &States) -> io::Result<()> {
    if states.len() > MAX_DEVICES
        || states.iter().any(|(id, samples)| {
            !valid_text(id.as_str())
                || samples.len() > MAX_POINTS
                || samples.iter().any(|(key, sample)| {
                    key.is_empty()
                        || !valid_text(key)
                        || match &sample.value {
                            Value::Text(text) => text.len() > MAX_VALUE_TEXT,
                            Value::Float(value) => !value.is_finite(),
                            _ => false,
                        }
                })
        })
    {
        return Err(invalid("invalid or oversized unclaimed state cache"));
    }
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn valid_text(value: &str) -> bool {
    value.len() <= MAX_TEXT
}

fn validate(devices: &Devices) -> io::Result<()> {
    if devices.len() > MAX_DEVICES {
        return Err(invalid("too many cached devices"));
    }
    for (id, cached) in devices {
        let device = &cached.device;
        let identity = id == &device.id
            && id.instance() == device.instance
            && id
                .as_str()
                .split_once(':')
                .is_some_and(|(owner, native)| !owner.is_empty() && !native.is_empty())
            && !id.as_str().contains('/')
            && valid_text(id.as_str())
            && !cached.kind.is_empty()
            && valid_text(&cached.kind)
            && !cached.integration.is_empty()
            && valid_text(&cached.integration);
        let metadata = valid_text(&device.native_name)
            && [
                &device.manufacturer,
                &device.model,
                &device.description,
                &device.native_room,
            ]
            .into_iter()
            .all(|value| value.as_deref().is_none_or(valid_text))
            && device.members.len() <= MAX_DEVICES
            && device
                .members
                .iter()
                .all(|member| valid_text(member.as_str()));
        let mut keys = BTreeSet::new();
        let points = device.points.len() <= MAX_POINTS
            && device.points.iter().all(|point| {
                !point.key.is_empty()
                    && valid_text(&point.key)
                    && keys.insert(&point.key)
                    && valid_text(&point.label)
                    && point
                        .unit
                        .as_ref()
                        .is_none_or(|unit| valid_text(unit.symbol()))
                    && match &point.kind {
                        Kind::Enum { values } => {
                            values.len() <= MAX_POINTS
                                && values.iter().all(|value| valid_text(value))
                        }
                        Kind::Numeric { min, max, step } => [min, max, step]
                            .into_iter()
                            .all(|value| value.is_none_or(f64::is_finite)),
                        _ => true,
                    }
            });
        let states = cached.states.len() <= device.points.len()
            && cached.states.iter().all(|(key, sample)| {
                device
                    .point(key)
                    .is_some_and(|point| sample_matches(&point.kind, &sample.value))
                    && match &sample.value {
                        Value::Text(text) => text.len() <= MAX_VALUE_TEXT,
                        Value::Float(value) => value.is_finite(),
                        _ => true,
                    }
            });
        if !(identity && metadata && points && states) {
            return Err(invalid(
                "invalid device cache identity, definition or sample",
            ));
        }
    }
    Ok(())
}

/// Readings may be unknown or outside a command's allowed range. Their type,
/// however, must still describe the same point after a definition changes.
pub(crate) fn sample_matches(kind: &Kind, value: &Value) -> bool {
    match (kind, value) {
        (_, Value::Null)
        | (Kind::Binary, Value::Bool(_))
        | (Kind::Numeric { .. }, Value::Int(_))
        | (Kind::Text | Kind::Enum { .. }, Value::Text(_)) => true,
        (Kind::Numeric { .. }, Value::Float(value)) => value.is_finite(),
        _ => false,
    }
}

// Bound the serializer too: a large live inventory never allocates an
// unbounded temporary JSON buffer before discovering that it cannot be saved.
struct BoundedWriter(Vec<u8>);
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_BYTES {
            return Err(invalid("device cache exceeds 8 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
