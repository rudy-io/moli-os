//! Encrypted, instance-scoped cached data, separate from rotating credentials.
//!
//! Every mutating call runs on a blocking worker while the caller holds the
//! vault's writer guard and interprocess lock. The caller must check its opaque
//! epoch against the vault under those same locks before entering this module.
//! This keeps an old worker from publishing after a disconnect or replacement.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use ring::aead::LessSafeKey;
use serde::{Deserialize, Serialize};

use crate::secrets::{decrypt, encrypt, write_durable_with};

const FILE_NAME: &str = "private-state.enc";
const FORMAT_KEY: &str = "format";
const FORMAT_VALUE: &str = "moli-private-state-v1";
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_VALUE_BYTES: usize = 2 * 1024 * 1024;
const MAX_INSTANCES: usize = 128;
const MAX_ENTRIES: usize = 128;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Namespace {
    epoch: String,
    values: BTreeMap<String, String>,
}

type Snapshot = BTreeMap<String, Namespace>;

#[derive(Default)]
struct Cache {
    durable: Snapshot,
    live: Snapshot,
    needs_directory_sync: bool,
}

/// All paths are derived from the configured vault; names never choose a path.
pub(crate) struct PrivateStateStore {
    path: PathBuf,
    cache: Mutex<Cache>,
}

impl fmt::Debug for PrivateStateStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateStateStore(<encrypted>)")
    }
}

impl PrivateStateStore {
    pub(crate) fn open(vault_path: &Path, key: &LessSafeKey) -> io::Result<Self> {
        let path = vault_path.with_file_name(FILE_NAME);
        let durable = read(&path, key)?;
        Ok(Self {
            path,
            cache: Mutex::new(Cache {
                live: durable.clone(),
                durable,
                needs_directory_sync: false,
            }),
        })
    }

    /// Reconcile a new protocol owner with disk while retaining unrelated live
    /// observations. The acquiring namespace always starts from stale durable data.
    /// The caller holds the vault writer and interprocess locks.
    pub(crate) fn reload_instance(&self, key: &LessSafeKey, instance: &str) -> io::Result<()> {
        let current = read(&self.path, key)?;
        let mut live = self.reconcile(&current);
        if let Some(namespace) = current.get(instance) {
            live.insert(instance.to_owned(), namespace.clone());
        } else {
            live.remove(instance);
        }
        self.commit(key, &current, &current, live, sync_parent)?;
        Ok(())
    }

    pub(crate) fn get(&self, instance: &str, epoch: &str, name: &str) -> Option<String> {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live
            .get(instance)
            .filter(|namespace| namespace.epoch == epoch)?
            .values
            .get(name)
            .cloned()
    }

    pub(crate) fn instances(&self) -> Vec<String> {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live
            .keys()
            .cloned()
            .collect()
    }

    /// Updates live observations without touching disk when their durable
    /// representation did not change. The caller removes liveness timestamps
    /// from durable values; values loaded after restart therefore stay stale.
    pub(crate) fn set_if_changed(
        &self,
        key: &LessSafeKey,
        instance: &str,
        epoch: &str,
        entries: Vec<(String, String, String)>,
    ) -> io::Result<bool> {
        validate_identity(instance, epoch)?;
        let mut names = std::collections::BTreeSet::new();
        for (name, live, durable) in &entries {
            validate_entry(name, live)?;
            validate_entry(name, durable)?;
            if !names.insert(name) {
                return Err(invalid("duplicate private state name"));
            }
        }
        let current = read(&self.path, key)?;
        let mut next = current.clone();
        let mut live = self.reconcile(&current);
        if !entries.is_empty() {
            let stored = namespace(&mut next, instance, epoch);
            let observed = namespace(&mut live, instance, epoch);
            for (name, value, durable) in entries {
                stored.values.insert(name.clone(), durable);
                observed.values.insert(name, value);
            }
        }
        self.commit(key, &current, &next, live, sync_parent)
    }

    /// Durable queue insertion: an acknowledged pending request survives restart.
    pub(crate) fn insert_if_absent(
        &self,
        key: &LessSafeKey,
        instance: &str,
        epoch: &str,
        name: &str,
        value: &str,
    ) -> io::Result<bool> {
        self.insert_many_if_absent(key, instance, epoch, vec![(name.into(), value.into())])
    }

    /// Migration writes one batch, preserving any newer records already on disk.
    pub(crate) fn insert_many_if_absent(
        &self,
        key: &LessSafeKey,
        instance: &str,
        epoch: &str,
        entries: Vec<(String, String)>,
    ) -> io::Result<bool> {
        validate_identity(instance, epoch)?;
        for (name, value) in &entries {
            validate_entry(name, value)?;
        }
        let current = read(&self.path, key)?;
        let mut next = current.clone();
        let mut live = self.reconcile(&current);
        let mut inserted = false;
        for (name, value) in entries {
            let stored = namespace(&mut next, instance, epoch);
            if let std::collections::btree_map::Entry::Vacant(slot) =
                stored.values.entry(name.clone())
            {
                slot.insert(value.clone());
                namespace(&mut live, instance, epoch)
                    .values
                    .insert(name, value);
                inserted = true;
            }
        }
        self.commit(key, &current, &next, live, sync_parent)?;
        Ok(inserted)
    }

    /// Consume only after the encrypted deletion has committed durably.
    pub(crate) fn take(
        &self,
        key: &LessSafeKey,
        instance: &str,
        epoch: &str,
        name: &str,
    ) -> io::Result<Option<String>> {
        validate_identity(instance, epoch)?;
        validate_entry(name, "")?;
        let current = read(&self.path, key)?;
        let mut next = current.clone();
        let mut live = self.reconcile(&current);
        let value = next
            .get_mut(instance)
            .filter(|namespace| namespace.epoch == epoch)
            .and_then(|namespace| namespace.values.remove(name));
        if value.is_some() {
            if let Some(namespace) = live.get_mut(instance) {
                namespace.values.remove(name);
            }
            if next[instance].values.is_empty() {
                next.remove(instance);
                live.remove(instance);
            }
        }
        self.commit(key, &current, &next, live, sync_parent)?;
        Ok(value)
    }

    /// The caller durably advances the vault epoch first. Never unlink the file:
    /// commit the removal and evict its live data under the same writer lock.
    pub(crate) fn purge_namespace(&self, key: &LessSafeKey, instance: &str) -> io::Result<bool> {
        validate_identity(instance, "")?;
        let current = read(&self.path, key)?;
        let mut next = current.clone();
        let mut live = self.reconcile(&current);
        next.remove(instance);
        live.remove(instance);
        self.commit(key, &current, &next, live, sync_parent)
    }

    fn reconcile(&self, current: &Snapshot) -> Snapshot {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        current
            .iter()
            .map(|(instance, namespace)| {
                let observed = if cache.durable.get(instance) == Some(namespace) {
                    cache.live.get(instance).unwrap_or(namespace)
                } else {
                    namespace
                };
                (instance.clone(), observed.clone())
            })
            .collect()
    }

    fn commit(
        &self,
        key: &LessSafeKey,
        current: &Snapshot,
        next: &Snapshot,
        live: Snapshot,
        sync: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<bool> {
        validate_snapshot(next)?;
        validate_snapshot(&live)?;
        let publish = |needs_directory_sync| {
            *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = Cache {
                durable: next.clone(),
                live,
                needs_directory_sync,
            };
        };
        if current == next {
            let needs_sync = self
                .cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .needs_directory_sync;
            publish(needs_sync);
            if needs_sync {
                sync(self.path.parent().unwrap_or_else(|| Path::new(".")))?;
                self.cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .needs_directory_sync = false;
            }
            return Ok(false);
        }
        let bytes = encode(key, next)?;
        write_durable_with(&self.path, &bytes, |parent| {
            // Rename has already happened. A directory-sync error must not leave
            // the cache pretending that the retired on-disk value still exists.
            publish(true);
            sync(parent)?;
            self.cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .needs_directory_sync = false;
            Ok(())
        })?;
        Ok(true)
    }
}

fn namespace<'a>(snapshot: &'a mut Snapshot, instance: &str, epoch: &str) -> &'a mut Namespace {
    let value = snapshot
        .entry(instance.into())
        .or_insert_with(|| Namespace {
            epoch: epoch.into(),
            values: BTreeMap::new(),
        });
    if value.epoch != epoch {
        *value = Namespace {
            epoch: epoch.into(),
            values: BTreeMap::new(),
        };
    }
    value
}

fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

pub(crate) fn validate_identity(instance: &str, epoch: &str) -> io::Result<()> {
    if !bounded(instance, 256) || (!epoch.is_empty() && !bounded(epoch, 128)) {
        return Err(invalid("invalid private state identity"));
    }
    Ok(())
}

pub(crate) fn validate_entry(name: &str, value: &str) -> io::Result<()> {
    if !bounded(name, 128) || value.len() > MAX_VALUE_BYTES {
        return Err(invalid("invalid private state entry"));
    }
    Ok(())
}

fn validate_snapshot(snapshot: &Snapshot) -> io::Result<()> {
    if snapshot.len() > MAX_INSTANCES {
        return Err(invalid("private state has too many instances"));
    }
    let mut total = 0_u64;
    for (instance, namespace) in snapshot {
        validate_identity(instance, &namespace.epoch)?;
        if namespace.values.len() > MAX_ENTRIES {
            return Err(invalid("private state has too many entries"));
        }
        for (name, value) in &namespace.values {
            validate_entry(name, value)?;
            total = total.saturating_add((name.len() + value.len()) as u64);
            if total > MAX_FILE_BYTES {
                return Err(invalid("private state exceeds byte limit"));
            }
        }
    }
    Ok(())
}

fn read(path: &Path, key: &LessSafeKey) -> io::Result<Snapshot> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        // A deleted file is empty, never an invitation to resurrect cached data.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(invalid("private state exceeds byte limit"));
    }
    let mut values = decrypt(key, &bytes)?;
    if values.remove(FORMAT_KEY).as_deref() != Some(FORMAT_VALUE) {
        return Err(invalid("not a private state snapshot"));
    }
    let mut snapshot = Snapshot::new();
    for (name, value) in values {
        let instance = name
            .strip_prefix("instance/")
            .ok_or_else(|| invalid("invalid private state namespace"))?;
        let namespace =
            serde_json::from_str(&value).map_err(|_| invalid("invalid private state record"))?;
        snapshot.insert(instance.into(), namespace);
    }
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

fn encode(key: &LessSafeKey, snapshot: &Snapshot) -> io::Result<Vec<u8>> {
    let mut values = BTreeMap::from([(FORMAT_KEY.into(), FORMAT_VALUE.into())]);
    for (instance, namespace) in snapshot {
        values.insert(
            format!("instance/{instance}"),
            serde_json::to_string(namespace)
                .map_err(|_| invalid("cannot encode private state record"))?,
        );
    }
    let bytes = encrypt(key, &values)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(invalid("private state exceeds byte limit"));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_: &Path) -> io::Result<()> {
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[path = "private_state_tests.rs"]
mod tests;
