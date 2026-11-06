//! What the driver keeps of its network on disk: the network parameters
//! (never the key, which lives in the vault) and the devices that joined.
//!
//! One readable JSON file per instance, `<data_dir>/zigbee-<instance>.json`,
//! versioned, written atomically (a `.tmp` file next to it, then renamed):
//! a crash mid-write leaves the previous file whole.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

/// Format of the file this driver writes; a newer one is refused.
pub const VERSION: u32 = 1;

/// The parameters of the network the coordinator runs, without its key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRecord {
    pub pan_id: u16,
    /// As the stack stores it (`EmberNetworkParameters.extendedPanId`).
    pub extended_pan_id: [u8; 8],
    pub channel: u8,
    /// IEEE address of the dongle that runs the network, `0x` and 16 hex digits.
    pub coordinator_ieee: String,
    pub nwk_update_id: u8,
    /// Outgoing NWK frame counter of the network key, as an imported backup
    /// recorded it or as the driver last read it from the dongle (at once
    /// when the network runs, then hourly): a restore or an export starts
    /// above it (devices drop frames whose counter they already saw). Not
    /// secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_counter: Option<u32>,
    /// Whether `frame_counter` is fresh, that is restorable: set by an
    /// import (no dongle has sent since) or by a live read of the dongle
    /// (the restore margin covers what it sent since, up to an hour);
    /// cleared when a dongle takes the network over from a saved counter,
    /// until its own is read. Only a fresh counter is restored or exported:
    /// a stale one would start below what the devices already saw, and
    /// they would drop every frame of the coordinator.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub frame_counter_fresh: bool,
    /// When `frame_counter` was recorded, in unix seconds: by a live read
    /// of the dongle, or by `moli-os zigbee import` (the time of the
    /// import). A dongle left powered while Moli is stopped keeps sending:
    /// a restore or an export refuses a counter older than
    /// [`crate::backup::COUNTER_MAX_AGE`] (or of no known age), fresh or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_counter_recorded_at: Option<u64>,
    /// Sequence number of the network key, as a backup recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_sequence_number: Option<u8>,
}

impl NetworkRecord {
    /// Whether `other` is the same network: same PAN ID and extended PAN ID
    /// (its channel and update id may have moved).
    #[must_use]
    pub fn same_network(&self, other: &Self) -> bool {
        (self.pan_id, self.extended_pan_id) == (other.pan_id, other.extended_pan_id)
    }

    /// This record (as the dongle reports it) with what only a backup knows
    /// of the same network, kept from `known`: the frame counter (no longer
    /// fresh: a dongle runs the network, its counter has moved on) and its
    /// time, and the key sequence number.
    #[must_use]
    pub fn keeping_backup_of(self, known: &Self) -> Self {
        Self {
            frame_counter: known.frame_counter,
            frame_counter_fresh: false,
            frame_counter_recorded_at: known.frame_counter_recorded_at,
            key_sequence_number: known.key_sequence_number,
            ..self
        }
    }
}

/// A device that joined the network, and what its interview learnt of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecord {
    /// `0x` and 16 hex digits, as Zigbee2MQTT writes it.
    pub ieee: String,
    /// Its current short (network) address, when known (an imported backup
    /// may not have it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nwk: Option<u16>,
    /// Whether its descriptors and `Basic` cluster were read.
    pub interviewed: bool,
    /// `Basic`: `manufacturerName`, `modelId`, `swBuildId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sw_build: Option<String>,
    /// Node descriptor: manufacturer code, 0 coordinator / 1 router / 2 end
    /// device, MAC capability flags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_type: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<EndpointRecord>,
    /// What a lamp's Color Control says it can do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorCaps>,
    /// IAS Zone: what the sensor is (`zoneType`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_type: Option<u16>,
    /// What turns raw metering and electrical values into units.
    #[serde(default, skip_serializing_if = "Scale::is_empty")]
    pub scale: Scale,
    /// Whether its bindings and reports were set up.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub configured: bool,
    /// How long it had been silent when last written down, in seconds, to
    /// the hour: its availability goes on counting across a restart, without
    /// the time Moli itself was away (it may have spoken then, to nobody).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silent_for: Option<u64>,
    /// It left the network and has not come back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub left: bool,
}

/// MAC capability flag: mains-powered.
const MAC_MAINS: u8 = 0x04;
/// MAC capability flag: its receiver stays on when idle.
const MAC_RECEIVER_ON: u8 = 0x08;

impl DeviceRecord {
    /// Mains-powered (the node descriptor said so).
    #[must_use]
    pub fn mains(&self) -> bool {
        self.capability.is_some_and(|flags| flags & MAC_MAINS != 0)
    }

    /// Always listening: can be read on demand, unlike a sleepy device.
    #[must_use]
    pub fn listening(&self) -> bool {
        self.capability
            .is_some_and(|flags| flags & MAC_RECEIVER_ON != 0)
    }
}

/// One endpoint's simple descriptor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointRecord {
    pub endpoint: u8,
    pub profile: u16,
    pub device_id: u16,
    #[serde(default)]
    pub in_clusters: Vec<u16>,
    #[serde(default)]
    pub out_clusters: Vec<u16>,
}

/// What a lamp's Color Control says it can do (`colorCapabilities`, the
/// physical range of its white).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorCaps {
    pub xy: bool,
    pub temp: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_min: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_max: Option<u16>,
}

/// A raw value times `multiplier`, over `divisor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ratio {
    pub multiplier: u32,
    pub divisor: u32,
}

impl Ratio {
    /// `raw` in the unit this ratio leads to; a divisor of 0 (a device that
    /// does not know) counts as 1.
    #[must_use]
    pub fn apply(self, raw: f64) -> f64 {
        raw * f64::from(self.multiplier) / f64::from(self.divisor.max(1))
    }
}

/// `seMetering` and `haElectricalMeasurement` ratios, as the device reports
/// them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scale {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metering: Option<Ratio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage: Option<Ratio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<Ratio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<Ratio>,
}

impl Scale {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The whole file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    pub version: u32,
    #[serde(default)]
    pub network: Option<NetworkRecord>,
    /// By IEEE address (`0x…`).
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceRecord>,
    /// `moli-os zigbee import` wrote this store but may not have put the
    /// keys into the vault yet: the network and the vault may not match.
    /// The driver waits until the same import is run again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub import_pending: bool,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: VERSION,
            network: None,
            devices: BTreeMap::new(),
            import_pending: false,
        }
    }
}

/// Where an instance keeps its store.
#[must_use]
pub fn path(data_dir: &Path, instance: &str) -> PathBuf {
    data_dir.join(format!("zigbee-{instance}.json"))
}

impl Store {
    /// Reads the store at `path`; an absent file is an empty store.
    ///
    /// # Errors
    ///
    /// The file cannot be read, is not a store, or was written by a newer Moli.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };
        let store: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a zigbee store", path.display()))?;
        if store.version > VERSION {
            bail!(
                "{} is version {}, newer than this Moli reads ({VERSION})",
                path.display(),
                store.version
            );
        }
        Ok(store)
    }

    /// Writes the store to `path` atomically: `<path>.tmp`, flushed to disk,
    /// then renamed over `path`.
    ///
    /// # Errors
    ///
    /// The directory cannot be created or the file written or renamed.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let mut json = serde_json::to_vec_pretty(self)?;
        json.push(b'\n');
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = tmp_path(path);
        let written = std::fs::File::create(&tmp).and_then(|mut file| {
            file.write_all(&json)?;
            file.sync_all()
        });
        if let Err(error) = written.and_then(|()| std::fs::rename(&tmp, path)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error).with_context(|| format!("writing {}", path.display()));
        }
        Ok(())
    }
}

impl Store {
    /// A device arrived (joined, rejoined or announced itself) at `nwk`: a
    /// new one is recorded, not yet interviewed; a known one keeps what was
    /// learnt of it, at its new address. Returns whether anything changed
    /// (the store then needs saving).
    pub fn record_join(&mut self, ieee: &str, nwk: u16) -> bool {
        match self.devices.get_mut(ieee) {
            Some(device) if device.nwk == Some(nwk) && !device.left => false,
            Some(device) => {
                device.nwk = Some(nwk);
                device.left = false;
                true
            }
            None => {
                self.devices.insert(
                    ieee.to_owned(),
                    DeviceRecord {
                        ieee: ieee.to_owned(),
                        nwk: Some(nwk),
                        ..DeviceRecord::default()
                    },
                );
                true
            }
        }
    }
}

impl Store {
    /// A known device joined from nothing: it was reset, and has forgotten
    /// its bindings, its reports and where to send its alarms. What was
    /// learnt of it no longer holds. Returns whether anything was forgotten
    /// (the store then needs saving).
    pub fn forget_interview(&mut self, ieee: &str) -> bool {
        let Some(device) = self.devices.get_mut(ieee) else {
            return false;
        };
        let blank = DeviceRecord {
            ieee: device.ieee.clone(),
            nwk: device.nwk,
            ..DeviceRecord::default()
        };
        let changed = *device != blank;
        *device = blank;
        changed
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// An empty directory of its own, removed when dropped.
    #[derive(Debug)]
    pub(crate) struct TempDir(pub(crate) PathBuf);

    impl TempDir {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "moli-zigbee-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn record(pan_id: u16) -> NetworkRecord {
        NetworkRecord {
            pan_id,
            extended_pan_id: [0xDD; 8],
            channel: 15,
            coordinator_ieee: "0x00124b0000000001".into(),
            nwk_update_id: 0,
            frame_counter: None,
            frame_counter_fresh: false,
            frame_counter_recorded_at: None,
            key_sequence_number: None,
        }
    }

    fn full() -> Store {
        let mut store = Store {
            network: Some(record(0x1a62)),
            ..Store::default()
        };
        store.devices.insert(
            "0x00124b0000000002".into(),
            DeviceRecord {
                ieee: "0x00124b0000000002".into(),
                nwk: Some(0x4f2a),
                interviewed: true,
                manufacturer: Some("Some maker".into()),
                model: Some("Some lamp".into()),
                sw_build: Some("1.0".into()),
                manufacturer_code: Some(0x1166),
                logical_type: Some(1),
                capability: Some(0x8E),
                endpoints: vec![EndpointRecord {
                    endpoint: 1,
                    profile: 0x0104,
                    device_id: 0x010D,
                    in_clusters: vec![0x0000, 0x0006, 0x0008, 0x0300],
                    out_clusters: vec![0x0019],
                }],
                color: Some(ColorCaps {
                    xy: true,
                    temp: true,
                    temp_min: Some(153),
                    temp_max: Some(556),
                }),
                zone_type: None,
                scale: Scale {
                    power: Some(Ratio {
                        multiplier: 1,
                        divisor: 10,
                    }),
                    ..Scale::default()
                },
                configured: true,
                silent_for: Some(3600),
                left: false,
            },
        );
        store
    }

    #[test]
    fn store_round_trip_is_atomic() {
        let dir = TempDir::new();
        let file = path(&dir.0, "zigbee");
        assert_eq!(file, dir.0.join("zigbee-zigbee.json"));
        // Absent: empty, at the current version.
        assert_eq!(Store::load(&file).unwrap(), Store::default());
        assert_eq!(Store::default().version, VERSION);
        let store = full();
        store.save(&file).unwrap();
        // Saved again over itself: the rename replaces the previous file.
        store.save(&file).unwrap();
        assert_eq!(Store::load(&file).unwrap(), store);
        let names: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["zigbee-zigbee.json"], "no .tmp left behind");
        // Readable JSON, versioned.
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"version\": 1"), "{text}");
        assert!(text.contains("\"pan_id\": 6754"), "{text}");
        // A counter's time, kept; a file written before it was: no time.
        let mut timed = full();
        let network = timed.network.as_mut().unwrap();
        network.frame_counter = Some(42);
        network.frame_counter_recorded_at = Some(1_800_000_000);
        timed.save(&file).unwrap();
        assert_eq!(Store::load(&file).unwrap(), timed);
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(json["network"]["frame_counter_recorded_at"], 1_800_000_000);
        json["network"]
            .as_object_mut()
            .unwrap()
            .remove("frame_counter_recorded_at");
        std::fs::write(&file, json.to_string()).unwrap();
        let older = Store::load(&file).unwrap();
        assert_eq!(older.network.unwrap().frame_counter_recorded_at, None);
    }

    #[test]
    fn store_never_contains_key() {
        // The key of the docs; the store has no field that could carry it.
        let key = "01030507090b0d0f00020406080a0c0d";
        let dir = TempDir::new();
        let file = path(&dir.0, "zigbee");
        full().save(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap().to_lowercase();
        assert!(!text.contains(key), "{text}");
        assert!(!text.contains("key"), "{text}");
    }

    #[test]
    fn a_join_is_recorded_once() {
        let mut store = full();
        // A new device: recorded, not interviewed.
        assert!(store.record_join("0x00124b0000000003", 0x1234));
        assert_eq!(
            store.devices["0x00124b0000000003"],
            DeviceRecord {
                ieee: "0x00124b0000000003".into(),
                nwk: Some(0x1234),
                ..DeviceRecord::default()
            }
        );
        // Its announce after its join: nothing new.
        assert!(!store.record_join("0x00124b0000000003", 0x1234));
        // A known device at a new address keeps its interview.
        let before = store.devices["0x00124b0000000002"].clone();
        assert!(store.record_join("0x00124b0000000002", 0x0001));
        assert_eq!(
            store.devices["0x00124b0000000002"],
            DeviceRecord {
                nwk: Some(0x0001),
                ..before
            }
        );
    }

    #[test]
    fn a_device_coming_back_or_reset() {
        let mut store = full();
        let known = "0x00124b0000000002";
        // Back at the same address, after leaving: something changed.
        store.devices.get_mut(known).unwrap().left = true;
        assert!(store.record_join(known, 0x4f2a));
        assert!(!store.devices[known].left);
        // Reset: only who and where it is remain, and only once.
        assert!(store.forget_interview(known));
        assert_eq!(
            store.devices[known],
            DeviceRecord {
                ieee: known.into(),
                nwk: Some(0x4f2a),
                ..DeviceRecord::default()
            }
        );
        assert!(!store.forget_interview(known));
        assert!(!store.forget_interview("0x00124b00000000ff"));
    }

    #[test]
    fn a_record_without_an_interview_reads_as_before() {
        // What a store of the previous phase holds.
        let json = r#"{"version":1,"devices":{"0x00124b0000000002":
            {"ieee":"0x00124b0000000002","nwk":4660,"interviewed":false}}}"#;
        let store: Store = serde_json::from_str(json).unwrap();
        assert_eq!(
            store.devices["0x00124b0000000002"],
            DeviceRecord {
                ieee: "0x00124b0000000002".into(),
                nwk: Some(4660),
                ..DeviceRecord::default()
            }
        );
        // And nothing more is written back for it.
        let text = serde_json::to_string(&store.devices["0x00124b0000000002"]).unwrap();
        assert_eq!(
            text,
            r#"{"ieee":"0x00124b0000000002","nwk":4660,"interviewed":false}"#
        );
    }

    #[test]
    fn what_a_device_is_comes_from_its_capabilities() {
        let lamp = DeviceRecord {
            capability: Some(0x8E),
            ..DeviceRecord::default()
        };
        assert!(lamp.mains() && lamp.listening());
        let sensor = DeviceRecord {
            capability: Some(0x80),
            ..DeviceRecord::default()
        };
        assert!(!sensor.mains() && !sensor.listening());
        let unknown = DeviceRecord::default();
        assert!(!unknown.mains() && !unknown.listening());
        let ratio = Ratio {
            multiplier: 3,
            divisor: 2,
        };
        assert!((ratio.apply(10.0) - 15.0).abs() < 1e-9);
        let unset = Ratio {
            multiplier: 1,
            divisor: 0,
        };
        assert!((unset.apply(10.0) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn a_newer_or_broken_store_is_refused() {
        let dir = TempDir::new();
        let file = path(&dir.0, "zigbee");
        std::fs::write(&file, r#"{"version": 2}"#).unwrap();
        let error = Store::load(&file).unwrap_err().to_string();
        assert!(error.contains("version 2"), "{error}");
        std::fs::write(&file, "{").unwrap();
        assert!(Store::load(&file).is_err());
        // An older or minimal file: what it lacks is empty.
        std::fs::write(&file, r#"{"version": 1}"#).unwrap();
        assert_eq!(Store::load(&file).unwrap(), Store::default());
    }
}
