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
        use std::fmt::Write as _;
        let digest = ring::digest::digest(
            &ring::digest::SHA256,
            format!("{}/{name}", self.instance).as_bytes(),
        );
        let mut suffix = String::from(".lease.");
        for byte in digest.as_ref() {
            let _ = write!(suffix, "{byte:02x}");
        }
        std::fs::create_dir_all(parent_dir(&self.store.path))?;
        let mut path = self.store.path.as_os_str().to_owned();
        path.push(suffix);
        let file = private_file(Path::new(&path), false)?;
        file.try_lock().map_err(io::Error::other)?;
        // A second process may have opened this vault before the previous
        // controller reserved more counters. Ownership alone must not restore
        // that stale cached snapshot when the lease changes hands.
        let _guard = self.store.write.blocking_lock();
        let _file_lock = lock_file(&self.store.path)?;
        let next = self.store.current()?;
        *self
            .store
            .values
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = next;
        Ok(DurableLease(file))
    }

    /// Replaces one value, syncing the encrypted file and its parent directory
    /// before success. A failed write never updates the in-memory value.
    ///
    /// # Panics
    /// Called on a Tokio executor thread rather than a blocking worker.
    pub fn set(&self, name: &str, value: &str) -> io::Result<()> {
        let _guard = self.store.write.blocking_lock();
        let _file_lock = lock_file(&self.store.path)?;
        let mut next = self.store.current()?;
        next.insert(format!("{}/{name}", self.instance), value.to_owned());
        let bytes = encrypt(&self.store.key, &next)?;
        write_durable(&self.store.path, &bytes)?;
        *self
            .store
            .values
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = next;
        Ok(())
    }
}

pub(crate) struct SecretStore {
    path: PathBuf,
    key: LessSafeKey,
    values: Arc<Mutex<BTreeMap<String, String>>>,
    /// Serializes writes so the file always matches memory.
    write: tokio::sync::Mutex<()>,
}

impl fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Names may be listed; values never.
        let values = self.values.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("SecretStore")
            .field("path", &self.path)
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
/// Safe while moli-os runs (each write starts from the file), but a running
/// hub reads the new value only after its next write or a restart.
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
        Ok(Self {
            path,
            key,
            values: Arc::new(Mutex::new(values)),
            write: tokio::sync::Mutex::new(()),
        })
    }

    pub(crate) fn get(&self, name: &str) -> Option<String> {
        self.values
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    pub(crate) async fn set(&self, name: &str, value: &str) -> io::Result<()> {
        let _guard = self.write.lock().await;
        let path = self.path.clone();
        let file_lock = tokio::task::spawn_blocking(move || lock_file(&path))
            .await
            .map_err(io::Error::other)??;
        let mut next = self.current()?;
        next.insert(name.to_owned(), value.to_owned());
        self.commit(next, file_lock).await
    }

    pub(crate) async fn remove(&self, name: &str) -> io::Result<bool> {
        let _guard = self.write.lock().await;
        let path = self.path.clone();
        let file_lock = tokio::task::spawn_blocking(move || lock_file(&path))
            .await
            .map_err(io::Error::other)??;
        let mut next = self.current()?;
        if next.remove(name).is_none() {
            return Ok(false);
        }
        self.commit(next, file_lock).await.map(|()| true)
    }

    /// The file as it is now, falling back to memory if it vanished.
    /// Another process (`moli-os secrets set` while the hub runs) may have
    /// written since we loaded: start every write from the disk, never
    /// from a stale copy that would erase its change. Caller holds `write`.
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

    /// Encrypts and writes `next`, then makes it current. Caller holds `write`.
    async fn commit(
        &self,
        next: BTreeMap<String, String>,
        file_lock: std::fs::File,
    ) -> io::Result<()> {
        let bytes = encrypt(&self.key, &next)?;
        let path = self.path.clone();
        let values = self.values.clone();
        tokio::task::spawn_blocking(move || {
            let _file_lock = file_lock;
            write_durable(&path, &bytes)?;
            *values.lock().unwrap_or_else(PoisonError::into_inner) = next;
            Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }
}

/// Every writer, including the CLI in another process, holds this lock through
/// read/merge/rename. A lock on the data inode would be lost across rename.
fn lock_file(path: &Path) -> io::Result<std::fs::File> {
    std::fs::create_dir_all(parent_dir(path))?;
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    let file = private_file(Path::new(&lock), false)?;
    file.lock()?;
    Ok(file)
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
    use std::io::Write as _;
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
        #[cfg(unix)]
        std::fs::File::open(parent_dir(path))?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

fn encrypt(key: &LessSafeKey, values: &BTreeMap<String, String>) -> io::Result<Vec<u8>> {
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

fn decrypt(key: &LessSafeKey, bytes: &[u8]) -> io::Result<BTreeMap<String, String>> {
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
    async fn concurrent_writer_is_not_erased() {
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
