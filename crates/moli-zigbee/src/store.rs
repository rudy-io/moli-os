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
}

/// A device that joined the network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecord {
    /// `0x` and 16 hex digits, as Zigbee2MQTT writes it.
    pub ieee: String,
    /// Its current short (network) address.
    pub nwk: u16,
    /// Whether its descriptors and `Basic` cluster were read.
    pub interviewed: bool,
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
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: VERSION,
            network: None,
            devices: BTreeMap::new(),
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
                nwk: 0x4f2a,
                interviewed: true,
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
