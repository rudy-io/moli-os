//! Encrypted secret store for credentials that drivers obtain at runtime
//! (pairing keys, tokens). Never served by any surface.
//!
//! One file, `MOLISEC1 ‖ nonce(12) ‖ ChaCha20-Poly1305(json)`, keyed by a
//! 256-bit master key that only ever arrives through the environment
//! (for example from a secrets manager). Without the key the file is
//! noise; without the file the key is useless.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

const MAGIC: &[u8; 8] = b"MOLISEC1";
const AAD: &[u8] = b"moli-os secrets v1";

/// Instance-scoped synchronous access for protocol libraries that require a
/// durable store callback. Writes must run on a blocking worker, never on an
/// async executor thread. Values are encrypted with the existing vault key.
#[derive(Clone)]
pub struct DurableSecrets {
    store: Arc<SecretStore>,
    instance: String,
}

/// Exclusive lifetime ownership of protocol state. Dropping it releases the
/// operating-system lock, including when its process exits unexpectedly.
pub struct DurableLease(std::fs::File);

impl fmt::Debug for DurableLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DurableLease(<held>)")
    }
}

impl Drop for DurableLease {
    fn drop(&mut self) {
        // Reading the field also makes the lifetime of the lock explicit.
        let _ = self.0.unlock();
    }
}

impl fmt::Debug for DurableSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DurableSecrets")
            .field("instance", &self.instance)
            .finish_non_exhaustive()
    }
}

impl DurableSecrets {
    pub(crate) fn new(store: Arc<SecretStore>, instance: String) -> Self {
        Self { store, instance }
    }

    /// Reads from the running vault without exposing another instance.
    pub fn get(&self, name: &str) -> Option<String> {
        self.store.get(&format!("{}/{name}", self.instance))
    }

    /// Prevents two controllers from using the same persistent identity and
    /// counter reservations. Hold the returned lease until the controller stops.
    /// Run on a blocking worker; unlike `set`, acquisition never waits for a peer.
    pub fn try_lease(&self, name: &str) -> io::Result<DurableLease> {
        self.try_lease_inner(name, false)
    }

    /// Acquire protocol ownership and reload this instance's encrypted private
    /// cache under the same vault lock. Unlike `try_lease`, unavailable private
    /// storage refuses this lease; other protocols remain independent.
    /// Run on a blocking worker. Restored observations are deliberately stale.
    pub fn try_private_lease(&self, name: &str) -> io::Result<DurableLease> {
        self.try_lease_inner(name, true)
    }

    fn try_lease_inner(&self, name: &str, private: bool) -> io::Result<DurableLease> {
        use std::fmt::Write as _;
        let digest = ring::digest::digest(
            &ring::digest::SHA256,
            format!("{}/{name}", self.instance).as_bytes(),
        );
        let mut suffix = String::from(".lease.");
        for byte in digest.as_ref() {
            let _ = write!(suffix, "{byte:02x}");
        }
        std::fs::create_dir_all(parent_dir(&self.store.0.path))?;
        let mut path = self.store.0.path.as_os_str().to_owned();
        path.push(suffix);
        let file = private_file(Path::new(&path), false)?;
        file.try_lock().map_err(io::Error::other)?;
        // A second process may have opened this vault before the previous
        // controller reserved more counters. Ownership alone must not restore
        // that stale cached snapshot when the lease changes hands.
        let _guard = self.store.0.write.blocking_lock();
        let _file_lock = lock_file(&self.store.0.path)?;
        let next = self.store.0.current()?;
        *self
            .store
            .0
            .values
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = next;
        if private {
            self.store
                .0
                .private
                .as_ref()
                .ok_or_else(|| io::Error::other("private state unavailable"))?
                .reload_instance(&self.store.0.key, &self.instance)?;
        }
        Ok(DurableLease(file))
    }

    /// Replaces one value, syncing the encrypted file and its parent directory
    /// before success. A failed write never updates the in-memory value.
    ///
    /// # Panics
    /// Called on a Tokio executor thread rather than a blocking worker.
    pub fn set(&self, name: &str, value: &str) -> io::Result<()> {
        let _guard = self.store.0.write.blocking_lock();
        let _file_lock = lock_file(&self.store.0.path)?;
        let mut next = self.store.0.current()?;
        next.insert(format!("{}/{name}", self.instance), value.to_owned());
        let bytes = encrypt(&self.store.0.key, &next)?;
        write_durable(&self.store.0.path, &bytes)?;
        *self
            .store
            .0
            .values
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = next;
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct SecretStore(pub(crate) Arc<SecretData>);

pub(crate) struct SecretData {
    pub(crate) path: PathBuf,
    pub(crate) key: LessSafeKey,
    pub(crate) values: Mutex<BTreeMap<String, String>>,
    pub(crate) private: Option<crate::private_state::PrivateStateStore>,
    needs_directory_sync: std::sync::atomic::AtomicBool,
    /// Serializes writes so the file always matches memory.
    write: Arc<tokio::sync::Mutex<()>>,
}

impl fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Names may be listed; values never.
        let values = self.0.values.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("SecretStore")
            .field("path", &self.0.path)
            .field("names", &values.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// The master key, as received from the environment. Never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct MasterKey(String);

impl MasterKey {
    #[must_use]
    pub fn new(hex: String) -> Self {
        Self(hex)
    }

    /// A fresh random key, for a new house. Never printed: written where
    /// the operator chose to keep it.
    pub fn generate() -> io::Result<Self> {
        use std::fmt::Write as _;
        let mut raw = [0u8; 32];
        SystemRandom::new()
            .fill(&mut raw)
            .map_err(|_| io::Error::other("no system randomness"))?;
        let hex = raw.iter().fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
        Ok(Self(hex))
    }

    /// The key as written in a key file (64 hex digits). The caller writes
    /// it to a private file and nowhere else.
    #[must_use]
    pub fn expose_for_file(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey(<redacted>)")
    }
}

/// Removes one stored secret (operator tool: forget a pairing, a pinned
/// certificate…). Returns whether it existed.
pub async fn forget_secret(path: &Path, key: &MasterKey, name: &str) -> io::Result<bool> {
    let store = SecretStore::open(path.to_path_buf(), &parse_master_key(&key.0)?)?;
    store.remove(name).await
}

/// Stores one secret (operator tool: a device key the human provides).
/// Every writer holds the same interprocess lock through read/merge/rename.
/// A running hub sees an external value after its next write or a restart.
pub async fn set_secret(path: &Path, key: &MasterKey, name: &str, value: &str) -> io::Result<()> {
    let store = SecretStore::open(path.to_path_buf(), &parse_master_key(&key.0)?)?;
    store.set(name, value).await
}

pub(crate) fn master_key_bytes(key: &MasterKey) -> io::Result<[u8; 32]> {
    parse_master_key(&key.0)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Parses a 64-hex-digit master key.
pub(crate) fn parse_master_key(hex: &str) -> io::Result<[u8; 32]> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return Err(invalid("master key must be 64 hex digits (256 bits)"));
    }
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|_| invalid("master key is not hexadecimal"))?;
    }
    Ok(key)
}

impl SecretStore {
    pub(crate) fn open(path: PathBuf, master_key: &[u8; 32]) -> io::Result<Self> {
        let key = LessSafeKey::new(
            UnboundKey::new(&CHACHA20_POLY1305, master_key).map_err(|_| invalid("bad key"))?,
        );
        let values = match std::fs::read(&path) {
            Ok(bytes) => decrypt(&key, &bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        // A broken private catalogue must not disable unrelated vault users.
        let private = crate::private_state::PrivateStateStore::open(&path, &key).ok();
        Ok(Self(Arc::new(SecretData {
            private,
            needs_directory_sync: std::sync::atomic::AtomicBool::new(false),
            path,
            key,
            values: Mutex::new(values),
            write: Arc::new(tokio::sync::Mutex::new(())),
        })))
    }

    pub(crate) fn get(&self, name: &str) -> Option<String> {
        self.0
            .values
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    pub(crate) async fn set(&self, name: &str, value: &str) -> io::Result<()> {
        let (name, value) = (name.to_owned(), value.to_owned());
        self.transaction(move |next| {
            next.insert(name, value);
            ((), true)
        })
        .await
    }

    pub(crate) async fn insert_if_absent(&self, name: &str, value: &str) -> io::Result<bool> {
        let (name, value) = (name.to_owned(), value.to_owned());
        self.transaction(move |next| {
            if let std::collections::btree_map::Entry::Vacant(slot) = next.entry(name) {
                slot.insert(value);
                (true, true)
            } else {
                (false, false)
            }
        })
        .await
    }

    pub(crate) async fn take(&self, name: &str) -> io::Result<Option<String>> {
        let name = name.to_owned();
        self.transaction(move |next| {
            let value = next.remove(&name);
            let changed = value.is_some();
            (value, changed)
        })
        .await
    }

    pub(crate) async fn remove(&self, name: &str) -> io::Result<bool> {
        self.take(name).await.map(|value| value.is_some())
    }

    /// Once dispatched, the job retains serialization through the disk and cache
    /// commit even if the asynchronous caller is cancelled. No fsync blocks Tokio.
    async fn transaction<T: Send + 'static>(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, String>) -> (T, bool) + Send + 'static,
    ) -> io::Result<T> {
        self.transaction_with_sync(change, sync_parent_directory)
            .await
    }

    async fn transaction_with_sync<T: Send + 'static>(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, String>) -> (T, bool) + Send + 'static,
        sync_parent: impl FnOnce(&Path) -> io::Result<()> + Send + 'static,
    ) -> io::Result<T> {
        self.transaction_data_with_sync(move |_, next| Ok(change(next)), sync_parent)
            .await
    }

    pub(crate) async fn transaction_data<T: Send + 'static>(
        &self,
        change: impl FnOnce(&SecretData, &mut BTreeMap<String, String>) -> io::Result<(T, bool)>
        + Send
        + 'static,
    ) -> io::Result<T> {
        self.transaction_data_with_sync(change, sync_parent_directory)
            .await
    }

    async fn transaction_data_with_sync<T: Send + 'static>(
        &self,
        change: impl FnOnce(&SecretData, &mut BTreeMap<String, String>) -> io::Result<(T, bool)>
        + Send
        + 'static,
        sync_parent: impl FnOnce(&Path) -> io::Result<()> + Send + 'static,
    ) -> io::Result<T> {
        let guard = Arc::clone(&self.0.write).lock_owned().await;
        let data = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let _file_lock = lock_file(&data.path)?;
            let mut next = data.current()?;
            // A failed CAS must still notice another process's durable revocation.
            *data.values.lock().unwrap_or_else(PoisonError::into_inner) = next.clone();
            let (result, changed) = change(&data, &mut next)?;
            if changed || (!data.path.is_file() && !next.is_empty()) {
                data.commit(next, sync_parent)?;
            } else {
                if data
                    .needs_directory_sync
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    sync_parent(parent_dir(&data.path))?;
                    data.needs_directory_sync
                        .store(false, std::sync::atomic::Ordering::Release);
                }
                *data.values.lock().unwrap_or_else(PoisonError::into_inner) = next;
            }
            Ok(result)
        })
        .await
        .map_err(|_| io::Error::other("secret transaction interrupted"))?
    }
}

impl SecretData {
    /// Reread disk while holding both the owned writer guard and interprocess lock.
    fn current(&self) -> io::Result<BTreeMap<String, String>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => decrypt(&self.key, &bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(self
                .values
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn commit(
        &self,
        next: BTreeMap<String, String>,
        sync_parent: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<()> {
        let bytes = encrypt(&self.key, &next)?;
        write_durable_with(&self.path, &bytes, |parent| {
            // A completed rename retired the old token even if directory sync fails.
            *self.values.lock().unwrap_or_else(PoisonError::into_inner) = next;
            self.needs_directory_sync
                .store(true, std::sync::atomic::Ordering::Release);
            sync_parent(parent)?;
            self.needs_directory_sync
                .store(false, std::sync::atomic::Ordering::Release);
            Ok(())
        })
    }
}

#[cfg(unix)]
pub(crate) fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}
#[cfg(not(unix))]
pub(crate) fn sync_parent_directory(_: &Path) -> io::Result<()> {
    Ok(())
}

/// Every writer, including the CLI in another process, holds this lock through
/// read/merge/rename. A lock on the data inode would be lost across rename.
fn lock_file(path: &Path) -> io::Result<std::fs::File> {
    lock_file_with_timeout(path, Duration::from_secs(3))
}

fn lock_file_with_timeout(path: &Path, timeout: Duration) -> io::Result<std::fs::File> {
    std::fs::create_dir_all(parent_dir(path))?;
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    let file = private_file(Path::new(&lock), false)?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            // Unsupported filesystems and other I/O failures must fail closed:
            // never read/merge/write without exclusive ownership.
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
            Err(std::fs::TryLockError::WouldBlock) => {
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out acquiring secret store write lock",
                    ));
                }
                std::thread::sleep(remaining.min(Duration::from_millis(25)));
            }
        }
    }
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn private_file(path: &Path, truncate: bool) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_durable_with(path, bytes, sync_parent_directory)
}

pub(crate) fn write_durable_with(
    path: &Path,
    bytes: &[u8],
    after_rename: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    use std::io::Write as _;
    std::fs::create_dir_all(parent_dir(path))?;
    let mut random = [0; 8];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| io::Error::other("no system randomness"))?;
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{:016x}.tmp", u64::from_ne_bytes(random)));
    let temp = Path::new(&temp);
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(temp, path)?;
        after_rename(parent_dir(path))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

pub(crate) fn encrypt(key: &LessSafeKey, values: &BTreeMap<String, String>) -> io::Result<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| io::Error::other("no system randomness"))?;
    let mut sealed = serde_json::to_vec(values).map_err(io::Error::other)?;
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(AAD),
        &mut sealed,
    )
    .map_err(|_| io::Error::other("encryption failed"))?;
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + sealed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

pub(crate) fn decrypt(key: &LessSafeKey, bytes: &[u8]) -> io::Result<BTreeMap<String, String>> {
    let rest = bytes
        .strip_prefix(MAGIC.as_slice())
        .ok_or_else(|| invalid("not a moli-os secret store"))?;
    if rest.len() < NONCE_LEN {
        return Err(invalid("truncated secret store"));
    }
    let (nonce, sealed) = rest.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| invalid("bad nonce"))?;
    let mut sealed = sealed.to_vec();
    let plain = key
        .open_in_place(nonce, Aad::from(AAD), &mut sealed)
        .map_err(|_| invalid("secret store cannot be decrypted (wrong master key?)"))?;
    serde_json::from_slice(plain).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    #[test]
    fn contended_write_lock_times_out_without_changing_the_vault() {
        let dir = std::env::temp_dir().join(format!("moli-lock-timeout-{}", std::process::id()));
        let path = dir.join("secrets.enc");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key = parse_master_key(KEY).unwrap();
        let store = SecretStore::open(path.clone(), &key).unwrap();
        let mut values = BTreeMap::new();
        values.insert("matter/snapshot".into(), "reserved-counters".into());
        let bytes = encrypt(&store.0.key, &values).unwrap();
        write_durable(&path, &bytes).unwrap();

        let held = lock_file(&path).unwrap();
        let timeout = Duration::from_millis(40);
        let started = Instant::now();
        let error = lock_file_with_timeout(&path, timeout).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= timeout);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        drop(held);
        let acquired = lock_file_with_timeout(&path, timeout).unwrap();
        drop(acquired);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn round_trip_and_wrong_key() {
        let dir = std::env::temp_dir().join(format!("moli-secrets-{}", std::process::id()));
        let path = dir.join("secrets.enc");
        let _ = std::fs::remove_dir_all(&dir);
        let key = parse_master_key(KEY).unwrap();

        let store = SecretStore::open(path.clone(), &key).unwrap();
        store.set("hue/app_key", "s3cret-value").await.unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(
            !raw.windows(12).any(|w| w == b"s3cret-value"),
            "never in clear on disk"
        );
        assert!(
            !format!("{store:?}").contains("s3cret"),
            "never in debug output"
        );

        let reopened = SecretStore::open(path.clone(), &key).unwrap();
        assert_eq!(reopened.get("hue/app_key").as_deref(), Some("s3cret-value"));

        assert!(reopened.remove("hue/app_key").await.unwrap());
        assert!(!reopened.remove("hue/app_key").await.unwrap());
        assert!(
            SecretStore::open(path.clone(), &key)
                .unwrap()
                .get("hue/app_key")
                .is_none()
        );

        let mut wrong = key;
        wrong[0] ^= 1;
        assert!(SecretStore::open(path, &wrong).is_err());
        assert_eq!(
            format!("{:?}", MasterKey::new(KEY.into())),
            "MasterKey(<redacted>)"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn sequential_external_writer_is_not_erased() {
        let dir = std::env::temp_dir().join(format!("moli-secrets-merge-{}", std::process::id()));
        let path = dir.join("secrets.enc");
        let _ = std::fs::remove_dir_all(&dir);
        let key = parse_master_key(KEY).unwrap();

        let hub = SecretStore::open(path.clone(), &key).unwrap();
        hub.set("hue/app_key", "a").await.unwrap();
        // The CLI writes behind the running hub's back…
        let cli = SecretStore::open(path.clone(), &key).unwrap();
        cli.set("meross/key", "b").await.unwrap();
        // …and the hub's next write keeps it.
        hub.set("hue/cert", "c").await.unwrap();
        let after = SecretStore::open(path, &key).unwrap();
        assert_eq!(after.get("meross/key").as_deref(), Some("b"));
        assert_eq!(after.get("hue/app_key").as_deref(), Some("a"));
        assert_eq!(after.get("hue/cert").as_deref(), Some("c"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn pending_slot_has_exactly_one_producer_and_one_consumer() {
        let path =
            std::env::temp_dir().join(format!("moli-secret-slot-{}.enc", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = SecretStore::open(path.clone(), &parse_master_key(KEY).unwrap()).unwrap();
        let (first, second) = tokio::join!(
            store.insert_if_absent("pending", "first"),
            store.insert_if_absent("pending", "second")
        );
        assert_ne!(first.unwrap(), second.unwrap());
        let (first, second) = tokio::join!(store.take("pending"), store.take("pending"));
        let values = [first.unwrap(), second.unwrap()];
        assert_eq!(values.iter().filter(|v| v.is_some()).count(), 1);
        assert!(store.get("pending").is_none());
        assert!(
            SecretStore::open(path.clone(), &parse_master_key(KEY).unwrap())
                .unwrap()
                .get("pending")
                .is_none()
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn failed_transaction_does_not_change_cache_or_acknowledge_input() {
        let dir = std::env::temp_dir().join(format!("moli-secret-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("secrets.enc");
        let store = SecretStore::open(path.clone(), &parse_master_key(KEY).unwrap()).unwrap();
        store.set("pending", "retained").await.unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(store.take("pending").await.is_err());
        assert_eq!(store.get("pending").as_deref(), Some("retained"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn cancelled_caller_keeps_transaction_lock_until_disk_and_cache_commit() {
        let path =
            std::env::temp_dir().join(format!("moli-secret-cancel-{}.enc", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = SecretStore::open(path.clone(), &parse_master_key(KEY).unwrap()).unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_store = store.clone();
        let first = tokio::spawn(async move {
            first_store
                .transaction(move |next| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    next.insert("first".into(), "committed despite cancellation".into());
                    ((), true)
                })
                .await
        });
        entered_rx.await.unwrap();
        first.abort();
        let _ = first.await;
        let second_store = store.clone();
        let second = tokio::spawn(async move { second_store.set("second", "after first").await });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            !second.is_finished(),
            "cancelled caller must not release the writer lock"
        );
        release_tx.send(()).unwrap();
        second.await.unwrap().unwrap();
        let reopened = SecretStore::open(path.clone(), &parse_master_key(KEY).unwrap()).unwrap();
        for name in ["first", "second"] {
            assert_eq!(store.get(name), reopened.get(name));
            assert!(store.get(name).is_some());
        }
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn pending_operations_require_a_vault_and_are_instance_scoped() {
        let hub = crate::Hub::new(crate::HubOptions::default()).unwrap();
        let instance = moli_core::InstanceId::from("test-instance");
        assert!(
            hub.insert_secret_if_absent(&instance, "pending", "input")
                .await
                .is_err()
        );
        assert!(hub.take_secret(&instance, "pending").await.is_err());
        let path =
            std::env::temp_dir().join(format!("moli-secret-scope-{}.enc", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let hub = crate::Hub::new(crate::HubOptions {
            secrets: Some((path.clone(), MasterKey::new(KEY.into()))),
            ..crate::HubOptions::default()
        })
        .unwrap();
        assert!(
            hub.insert_secret_if_absent(&instance, "pending", "first")
                .await
                .unwrap()
        );
        assert!(
            !hub.insert_secret_if_absent(&instance, "pending", "newer")
                .await
                .unwrap()
        );
        assert_eq!(
            hub.take_secret(&moli_core::InstanceId::from("another"), "pending")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            hub.take_secret(&instance, "pending")
                .await
                .unwrap()
                .as_deref(),
            Some("first")
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn review_post_rename_sync_and_recovery_read_failure_never_revive_old_tokens() {
        let dir =
            std::env::temp_dir().join(format!("moli-secret-post-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("secrets.enc");
        let replacement_path = dir.join("replaced.enc");
        let key = parse_master_key(KEY).unwrap();
        let store = SecretStore::open(path.clone(), &key).unwrap();
        store
            .set("cloud/session", "OLD_REFRESH_SENTINEL")
            .await
            .unwrap();
        let injected_path = path.clone();
        let injected_replacement = replacement_path.clone();
        let result = store
            .transaction_with_sync(
                |next| {
                    next.insert("cloud/session".into(), "NEW_REFRESH_SENTINEL".into());
                    ((), true)
                },
                move |_| {
                    // Called only after actual rename: ciphertext already holds the replacement.
                    let replaced = SecretStore::open(injected_path.clone(), &key).unwrap();
                    assert_eq!(
                        replaced.get("cloud/session").as_deref(),
                        Some("NEW_REFRESH_SENTINEL")
                    );
                    // Make the attempted recovery read fail deterministically as well.
                    std::fs::rename(&injected_path, &injected_replacement).unwrap();
                    std::fs::create_dir(&injected_path).unwrap();
                    Err(io::Error::other("injected directory sync failure"))
                },
            )
            .await;
        assert!(
            result.is_err(),
            "no durability ACK after failed parent sync"
        );
        assert!(
            std::fs::read(&path).is_err(),
            "recovery read is unavailable"
        );
        assert_eq!(
            store.get("cloud/session").as_deref(),
            Some("NEW_REFRESH_SENTINEL"),
            "known replacement must survive even when recovery cannot read disk"
        );
        assert_eq!(
            SecretStore::open(replacement_path, &key)
                .unwrap()
                .get("cloud/session"),
            store.get("cloud/session")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn master_key_format() {
        let fresh = MasterKey::generate().unwrap();
        assert!(parse_master_key(fresh.expose_for_file()).is_ok());
        assert_ne!(
            fresh.expose_for_file(),
            MasterKey::generate().unwrap().expose_for_file()
        );
        assert!(parse_master_key(KEY).is_ok());
        assert!(parse_master_key("abcd").is_err());
        assert!(parse_master_key(&"zz".repeat(32)).is_err());
    }

    #[tokio::test]
    async fn durable_snapshot_shares_the_vault_and_is_instance_scoped() {
        let dir = std::env::temp_dir().join(format!("moli-durable-{}", std::process::id()));
        let path = dir.join("secrets.enc");
        let _ = std::fs::remove_dir_all(&dir);
        let key = parse_master_key(KEY).unwrap();
        let vault = Arc::new(SecretStore::open(path.clone(), &key).unwrap());
        let first = DurableSecrets::new(vault.clone(), "matter_a".into());
        let second = DurableSecrets::new(vault.clone(), "matter_b".into());
        let lease_store = first.clone();
        let other_lease = second.clone();
        tokio::task::spawn_blocking(move || {
            let held = lease_store.try_lease("controller").unwrap();
            assert!(lease_store.try_lease("controller").is_err());
            assert!(other_lease.try_lease("controller").is_ok());
            drop(held);
            assert!(lease_store.try_lease("controller").is_ok());
        })
        .await
        .unwrap();
        let writer = first.clone();
        tokio::task::spawn_blocking(move || writer.set("snapshot", "private-snapshot-data"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            first.get("snapshot").as_deref(),
            Some("private-snapshot-data")
        );
        assert!(second.get("snapshot").is_none());
        assert!(!format!("{first:?}").contains("private-snapshot"));
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows(21)
                .any(|part| part == b"private-snapshot-data")
        );
        let reopened = SecretStore::open(path.clone(), &key).unwrap();
        assert_eq!(
            reopened.get("matter_a/snapshot").as_deref(),
            Some("private-snapshot-data")
        );

        // Independent handles mirror a running hub and the separate CLI process.
        // Their read/merge/write transactions must retain both updates.
        let another = Arc::new(SecretStore::open(path.clone(), &key).unwrap());
        let other = DurableSecrets::new(another, "matter_b".into());
        let task = tokio::task::spawn_blocking(move || other.set("snapshot", "other-state"));
        vault.set("hue/app_key", "existing-key").await.unwrap();
        task.await.unwrap().unwrap();
        let reopened = SecretStore::open(path, &key).unwrap();
        assert_eq!(reopened.get("hue/app_key").as_deref(), Some("existing-key"));
        assert_eq!(
            reopened.get("matter_b/snapshot").as_deref(),
            Some("other-state")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn durable_failure_does_not_publish_an_unsaved_value() {
        let dir = std::env::temp_dir().join(format!("moli-durable-fail-{}", std::process::id()));
        let path = dir.join("secrets.enc");
        let _ = std::fs::remove_dir_all(&dir);
        let key = parse_master_key(KEY).unwrap();
        let vault = Arc::new(SecretStore::open(path.clone(), &key).unwrap());
        // Rename onto a directory fails after encryption and writing the temp.
        std::fs::create_dir_all(&path).unwrap();
        let store = DurableSecrets::new(vault, "matter".into());
        let writer = store.clone();
        assert!(
            tokio::task::spawn_blocking(move || writer.set("snapshot", "never-saved"))
                .await
                .unwrap()
                .is_err()
        );
        assert!(store.get("snapshot").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lease_handoff_refreshes_the_snapshot_before_restore() {
        let dir = std::env::temp_dir().join(format!("moli-lease-refresh-{}", std::process::id()));
        let path = dir.join("secrets.enc");
        let _ = std::fs::remove_dir_all(&dir);
        let key = parse_master_key(KEY).unwrap();
        let first = DurableSecrets::new(
            Arc::new(SecretStore::open(path.clone(), &key).unwrap()),
            "matter".into(),
        );
        let second = DurableSecrets::new(
            Arc::new(SecretStore::open(path, &key).unwrap()),
            "matter".into(),
        );
        let lease = first.try_lease("controller").unwrap();
        first.set("snapshot", "new-counter-reservation").unwrap();
        assert!(second.get("snapshot").is_none());
        assert!(second.try_lease("controller").is_err());
        drop(lease);
        let _lease = second.try_lease("controller").unwrap();
        assert_eq!(
            second.get("snapshot").as_deref(),
            Some("new-counter-reservation")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
