//! A scoped generation fence shared by credential and private-cache writes.

use std::collections::BTreeMap;
use std::{fmt, io};

use ring::rand::{SecureRandom as _, SystemRandom};

use crate::private_state::{PrivateStateStore, validate_entry, validate_identity};
use crate::secrets::{SecretData, SecretStore, sync_parent_directory};

const EPOCH_NAME: &str = "private_state_epoch_v1";
pub(crate) const PURGE_PENDING_NAME: &str = "private_state_purge_pending_v1";

/// Private observations and queued requests, bound to one instance and lifetime.
/// A successful purge retires every existing handle, including writes in flight.
#[derive(Clone)]
pub struct PrivateState {
    store: SecretStore,
    instance: String,
    epoch: String,
}

impl fmt::Debug for PrivateState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateState(<scoped>)")
    }
}

impl PrivateState {
    pub(crate) fn new(store: SecretStore, instance: String) -> Option<Self> {
        store.0.private.as_ref()?;
        let epoch = store
            .get(&format!("{instance}/{EPOCH_NAME}"))
            .unwrap_or_default();
        validate_identity(&instance, &epoch).ok()?;
        Some(Self {
            store,
            instance,
            epoch,
        })
    }

    /// Reads live cached data only while this handle's generation is current.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<String> {
        validate_name(name).ok()?;
        if !self.is_current() {
            return None;
        }
        self.store
            .0
            .private
            .as_ref()?
            .get(&self.instance, &self.epoch, name)
    }

    /// A quick memory check for stopping an old controller. Every mutation also
    /// rechecks the generation from disk while holding the interprocess lock.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.store
            .get(&format!("{}/{EPOCH_NAME}", self.instance))
            .unwrap_or_default()
            == self.epoch
    }

    /// Recheck the lifetime on disk without changing stored values. Callers
    /// authorizing an external effect must also recheck memory after this await;
    /// the storage lock is deliberately not held during network operations.
    pub async fn confirm_current(&self) -> io::Result<()> {
        self.transaction(|data, _, _, _| {
            // Generic secret mutations can recreate a removed vault from cache.
            // An external-effect authorization must require its disk proof.
            std::fs::metadata(&data.path)?;
            Ok(((), false))
        })
        .await
    }

    /// Persist a credential checkpoint only if this lifetime is still current.
    pub async fn store_secret(&self, name: &str, value: &str) -> io::Result<()> {
        validate_name(name)?;
        validate_entry(name, value)?;
        let (name, value) = (name.to_owned(), value.to_owned());
        self.transaction(move |_, values, instance, _| {
            let name = format!("{instance}/{name}");
            let changed = values.get(&name) != Some(&value);
            if changed {
                values.insert(name, value);
            }
            Ok(((), changed))
        })
        .await
    }

    /// Publish live values, persisting only changes to their durable forms.
    pub async fn set_if_changed(&self, entries: Vec<(String, String, String)>) -> io::Result<bool> {
        for (name, _, _) in &entries {
            validate_name(name)?;
        }
        self.transaction(move |data, _, instance, epoch| {
            private(data)?
                .set_if_changed(&data.key, instance, epoch, entries)
                .map(|changed| (changed, false))
        })
        .await
    }

    /// Insert a durable queued request without replacing an unconsumed one.
    pub async fn insert_if_absent(&self, name: &str, value: &str) -> io::Result<bool> {
        validate_name(name)?;
        let (name, value) = (name.to_owned(), value.to_owned());
        self.transaction(move |data, _, instance, epoch| {
            private(data)?
                .insert_if_absent(&data.key, instance, epoch, &name, &value)
                .map(|inserted| (inserted, false))
        })
        .await
    }

    /// Return a request only after its deletion is durable; never replay a take.
    pub async fn take(&self, name: &str) -> io::Result<Option<String>> {
        validate_name(name)?;
        let name = name.to_owned();
        self.transaction(move |data, _, instance, epoch| {
            private(data)?
                .take(&data.key, instance, epoch, &name)
                .map(|value| (value, false))
        })
        .await
    }

    /// Retire this lifetime and remove only the named credentials. The epoch is
    /// committed first, so a crash before cache removal cannot revive old data.
    pub async fn purge(&self, secret_names: Vec<String>) -> io::Result<()> {
        validate_names(&secret_names)?;
        self.transaction(move |data, values, instance, _| {
            if !purge_data(data, values, instance, secret_names)? {
                return Err(io::Error::other("private state unavailable"));
            }
            Ok(((), true))
        })
        .await
    }

    /// Copy legacy data before removing it from the credential vault. Existing
    /// private records win on retry, so migration cannot replace newer reads.
    /// Migrated values are the stale durable forms, never fresh observations.
    pub async fn migrate(
        &self,
        entries: Vec<(String, String, String)>,
        legacy_names: Vec<String>,
    ) -> io::Result<()> {
        validate_names(&legacy_names)?;
        for (name, _, durable) in &entries {
            validate_name(name)?;
            validate_entry(name, durable)?;
        }
        self.transaction(move |data, values, instance, epoch| {
            let store = private(data)?;
            store.insert_many_if_absent(
                &data.key,
                instance,
                epoch,
                entries
                    .into_iter()
                    .map(|(name, _, durable)| (name, durable))
                    .collect(),
            )?;
            let mut changed = false;
            for name in legacy_names {
                changed |= values.remove(&format!("{instance}/{name}")).is_some();
            }
            Ok(((), changed))
        })
        .await
    }

    async fn transaction<T: Send + 'static>(
        &self,
        change: impl FnOnce(
            &SecretData,
            &mut BTreeMap<String, String>,
            &str,
            &str,
        ) -> io::Result<(T, bool)>
        + Send
        + 'static,
    ) -> io::Result<T> {
        let (instance, epoch) = (self.instance.clone(), self.epoch.clone());
        self.store
            .transaction_data(move |data, values| {
                if values
                    .get(&format!("{instance}/{EPOCH_NAME}"))
                    .map_or("", String::as_str)
                    != epoch
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "private state lifetime retired",
                    ));
                }
                change(data, values, &instance, &epoch)
            })
            .await
    }
}

/// Owner cleanup can retire credentials even when the private file is unreadable.
/// `false` leaves a durable marker and the file intact for a later scoped retry.
pub(crate) async fn purge_retained(
    store: SecretStore,
    instance: String,
    names: Vec<String>,
) -> io::Result<bool> {
    validate_identity(&instance, "")?;
    validate_names(&names)?;
    store
        .transaction_data(move |data, values| {
            let removed = purge_data(data, values, &instance, names)?;
            Ok((removed, removed))
        })
        .await
}

fn purge_data(
    data: &SecretData,
    values: &mut BTreeMap<String, String>,
    instance: &str,
    names: Vec<String>,
) -> io::Result<bool> {
    let mut random = [0_u8; 16];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| io::Error::other("no system randomness"))?;
    let epoch = random
        .iter()
        .fold(String::with_capacity(32), |mut value, byte| {
            use std::fmt::Write as _;
            let _ = write!(value, "{byte:02x}");
            value
        });
    let marker = format!("{instance}/{PURGE_PENDING_NAME}");
    values.insert(format!("{instance}/{EPOCH_NAME}"), epoch);
    values.insert(marker.clone(), "1".into());
    for name in names {
        values.remove(&format!("{instance}/{name}"));
    }
    // Commit the fence before touching the cache, including on a partial purge.
    data.commit(values.clone(), sync_parent_directory)?;
    let Some(private) = &data.private else {
        return Ok(false);
    };
    private.purge_namespace(&data.key, instance)?;
    values.remove(&marker);
    // The enclosing transaction commits marker removal before acknowledging.
    Ok(true)
}

fn private(data: &SecretData) -> io::Result<&PrivateStateStore> {
    data.private
        .as_ref()
        .ok_or_else(|| io::Error::other("private state unavailable"))
}

fn validate_name(name: &str) -> io::Result<()> {
    validate_entry(name, "")?;
    if matches!(name, EPOCH_NAME | PURGE_PENDING_NAME) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reserved private state name",
        ));
    }
    Ok(())
}

fn validate_names(names: &[String]) -> io::Result<()> {
    if names.len() > 128 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many private state names",
        ));
    }
    for name in names {
        validate_name(name)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "private_state_handle_tests.rs"]
mod tests;
