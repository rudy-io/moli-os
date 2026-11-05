//! Bounded inventory baselines and generation-matched observation updates.
//! The legacy state file remains readable by older binaries.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::state_cache::{self, States};
use moli_core::{Device, DeviceId, Kind, Sample, Value};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const VERSION: u8 = 2;
const MAX_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_DEVICES: usize = 4096;
const MAX_POINTS: usize = 1024;
const MAX_TEXT: usize = 8192;
const MAX_VALUE_TEXT: usize = 64 * 1024;

pub(crate) type Devices = BTreeMap<DeviceId, CachedDevice>;
pub(crate) type Generation = [u8; 16];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CachedDevice {
    pub kind: Arc<str>,
    pub integration: Arc<str>,
    pub device: Arc<Device>,
    pub states: BTreeMap<Arc<str>, Sample>,
}

#[derive(Debug)]
pub(crate) struct Inventory {
    pub generation: Generation,
    pub devices: Devices,
    pub unclaimed: States,
}

// Decode entries separately: a malformed device must not discard its peers.
#[derive(Deserialize)]
struct WireInventory {
    version: u8,
    #[serde(default)]
    generation: Generation,
    devices: BTreeMap<DeviceId, serde_json::Value>,
    // Required even when empty: deletions override stale legacy values.
    unclaimed: BTreeMap<DeviceId, serde_json::Value>,
}

#[derive(Deserialize)]
struct Observations {
    version: u8,
    generation: Generation,
    states: BTreeMap<DeviceId, serde_json::Value>,
}

pub(crate) fn path(state_path: &Path) -> PathBuf {
    state_path.with_extension("devices.json")
}

fn observations_path(state_path: &Path) -> PathBuf {
    state_path.with_extension("observations.json")
}

fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    let file = match std::fs::File::open(path) {
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
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| invalid(&error.to_string()))
}

pub(crate) fn load(state_path: &Path) -> io::Result<Option<Inventory>> {
    let Some(wire) = read_json::<WireInventory>(&path(state_path))? else {
        return Ok(None);
    };
    if !matches!(wire.version, 1 | VERSION)
        || (wire.version == VERSION && wire.generation == [0; 16])
    {
        return Err(invalid("unsupported device cache version or generation"));
    }
    let mut devices = Devices::new();
    for (id, raw) in wire.devices.into_iter().take(MAX_DEVICES) {
        match serde_json::from_value::<CachedDevice>(raw) {
            Ok(mut cached) if valid_definition(&id, &cached) => {
                cached.states.retain(|key, sample| {
                    valid_sample(key, sample)
                        && cached
                            .device
                            .point(key)
                            .is_some_and(|point| sample_matches(&point.kind, &sample.value))
                });
                devices.insert(id, cached);
            }
            _ => tracing::warn!("invalid cached definition omitted"),
        }
    }
    let mut inventory = Inventory {
        generation: if wire.version == 1 {
            [0; 16]
        } else {
            wire.generation
        },
        devices,
        unclaimed: decode_states(wire.unclaimed),
    };
    if wire.version == VERSION {
        match read_json::<Observations>(&observations_path(state_path)) {
            Ok(Some(observations))
                if observations.version == VERSION
                    && observations.generation == inventory.generation =>
            {
                inventory.unclaimed = decode_states(observations.states);
                for cached in inventory.devices.values_mut() {
                    cached.states = claim_states(&cached.device, &mut inventory.unclaimed);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "observation cache unreadable; keeping inventory baseline");
            }
            _ => {} // Missing/older observations never override a newer definition or deletion.
        }
    }
    Ok(Some(inventory))
}

fn decode_states(raw: BTreeMap<DeviceId, serde_json::Value>) -> States {
    let mut states = States::new();
    for (id, value) in raw.into_iter().take(MAX_DEVICES) {
        if let Ok(samples) = serde_json::from_value(value) {
            states.insert(id, samples);
        } else {
            tracing::warn!("invalid cached observations omitted");
        }
    }
    sanitize_states(states)
}

/// Only definition changes create a new baseline. State-only writes carry its
/// opaque generation, never clone/serialize definitions, and replace all values.
/// Commit the authoritative file first, then the legacy mirror. I/O failures
/// leave the last atomic snapshot intact and are retryable; bad entries are not.
pub(crate) async fn save_snapshot(
    state_path: &Path,
    devices: Option<Devices>,
    states: States,
    generation: &mut Generation,
) -> io::Result<()> {
    let states = sanitize_states(states);
    if let Some(devices) = devices {
        let mut next = [0; 16];
        SystemRandom::new()
            .fill(&mut next)
            .map_err(|_| io::Error::other("cache generation unavailable"))?;
        save_inventory(state_path, devices, states.clone(), next).await?;
        *generation = next;
    } else {
        let mut writer = BoundedWriter::new();
        write!(&mut writer, "{{\"version\":{VERSION},\"generation\":")?;
        serde_json::to_writer(&mut writer, generation).map_err(io::Error::other)?;
        writer.write_all(b",\"states\":")?;
        write_map(&mut writer, states.iter().collect())?;
        writer.write_all(b"}")?;
        crate::labels::write_atomic(&observations_path(state_path), &writer.bytes).await?;
    }
    state_cache::save(state_path, &states).await
}

async fn save_inventory(
    state_path: &Path,
    mut devices: Devices,
    mut unclaimed: States,
    generation: Generation,
) -> io::Result<()> {
    devices.retain(|id, cached| valid_definition(id, cached));
    for cached in devices.values_mut() {
        cached.states = unclaimed
            .get(&cached.device.id)
            .into_iter()
            .flatten()
            .filter(|(key, sample)| {
                cached
                    .device
                    .point(key)
                    .is_some_and(|point| sample_matches(&point.kind, &sample.value))
            })
            .map(|(key, sample)| (key.clone(), sample.clone()))
            .collect();
    }
    let mut writer = BoundedWriter::new();
    write!(&mut writer, "{{\"version\":{VERSION},\"generation\":")?;
    serde_json::to_writer(&mut writer, &generation).map_err(io::Error::other)?;
    writer.write_all(b",\"devices\":")?;
    let admitted = write_map(&mut writer, devices.iter().collect())?;
    for id in admitted {
        claim_states(&devices[&id].device, &mut unclaimed);
    }
    writer.write_all(b",\"unclaimed\":")?;
    write_map(&mut writer, unclaimed.iter().collect())?;
    writer.write_all(b"}")?;
    crate::labels::write_atomic(&path(state_path), &writer.bytes).await
}

fn claim_states(device: &Device, states: &mut States) -> BTreeMap<Arc<str>, Sample> {
    let mut claimed = BTreeMap::new();
    if let Some(samples) = states.get_mut(&device.id) {
        samples.retain(|key, sample| {
            let Some(point) = device.point(key) else {
                return true;
            };
            if sample_matches(&point.kind, &sample.value) {
                claimed.insert(key.clone(), sample.clone());
            }
            false
        });
        if samples.is_empty() {
            states.remove(&device.id);
        }
    }
    claimed
}

fn valid_sample(key: &str, sample: &Sample) -> bool {
    !key.is_empty()
        && valid_text(key)
        && match &sample.value {
            Value::Text(text) => text.len() <= MAX_VALUE_TEXT,
            Value::Float(value) => value.is_finite(),
            _ => true,
        }
}

fn sanitize_states(mut states: States) -> States {
    states.retain(|id, samples| {
        if !valid_text(id.as_str()) {
            return false;
        }
        let before = samples.len();
        let mut count = 0;
        samples.retain(|key, sample| {
            let keep = count < MAX_POINTS && valid_sample(key, sample);
            count += usize::from(keep);
            keep
        });
        if samples.len() != before {
            tracing::warn!("invalid or excessive cached observations omitted");
        }
        !samples.is_empty() // Missing maps clear obsolete baseline readings, too.
    });
    let mut writer = BoundedWriter::new();
    // Serialization cannot fail outside an individual entry: framing is tiny
    // and write_map leaves room for it. This also bounds the legacy mirror.
    let admitted = write_map(&mut writer, states.iter().collect()).expect("bounded map framing");
    states.retain(|id, _| admitted.contains(id));
    states
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn valid_text(value: &str) -> bool {
    value.len() <= MAX_TEXT
}

pub(crate) fn valid_definition(id: &DeviceId, cached: &CachedDevice) -> bool {
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
                        values.len() <= MAX_POINTS && values.iter().all(|value| valid_text(value))
                    }
                    Kind::Numeric { min, max, step } => [min, max, step]
                        .into_iter()
                        .all(|value| value.is_none_or(f64::is_finite)),
                    _ => true,
                }
        });
    identity && metadata && points
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

// Serialize independently, rolling back only an oversized entry. Reserve
// framing space so the cache stays valid JSON even when its budget is full.
fn write_map<T: Serialize>(
    writer: &mut BoundedWriter,
    entries: BTreeMap<&DeviceId, &T>,
) -> io::Result<BTreeSet<DeviceId>> {
    writer.write_all(b"{")?;
    let mut admitted = BTreeSet::new();
    for (id, value) in entries {
        if admitted.len() >= MAX_DEVICES {
            break;
        }
        let before = writer.bytes.len();
        writer.limit = MAX_BYTES - 256;
        let result = (|| {
            if !admitted.is_empty() {
                writer.write_all(b",")?;
            }
            serde_json::to_writer(&mut *writer, id).map_err(io::Error::other)?;
            writer.write_all(b":")?;
            serde_json::to_writer(&mut *writer, value).map_err(io::Error::other)
        })();
        writer.limit = MAX_BYTES;
        if result.is_ok() {
            admitted.insert(id.clone());
        } else {
            writer.bytes.truncate(before);
            tracing::warn!("oversized cached entry omitted");
        }
    }
    writer.write_all(b"}")?;
    Ok(admitted)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl BoundedWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            limit: MAX_BYTES,
        }
    }
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.limit {
            return Err(invalid("device cache exceeds 8 MiB"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
