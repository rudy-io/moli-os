//! Encrypted secret store for credentials that drivers obtain at runtime
//! (pairing keys, tokens). Never served by any surface.
//!
//! One file, `MOLISEC1 ‖ nonce(12) ‖ ChaCha20-Poly1305(json)`, keyed by a
//! 256-bit master key that only ever arrives through the environment
//! (for example from a secrets manager). Without the key the file is
//! noise; without the file the key is useless.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

const MAGIC: &[u8; 8] = b"MOLISEC1";
const AAD: &[u8] = b"moli-os secrets v1";

#[derive(Clone)]
pub(crate) struct SecretStore(Arc<SecretData>);

struct SecretData {
    path: PathBuf,
    key: LessSafeKey,
    values: Mutex<BTreeMap<String, String>>,
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
/// Rereading disk preserves sequential external writes; it does not provide
/// concurrent cross-process atomicity. Use one active writer per vault.
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
        Ok(Self(Arc::new(SecretData {
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
        let guard = Arc::clone(&self.0.write).lock_owned().await;
        let data = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let mut next = data.current()?;
            let (result, changed) = change(&mut next);
            if changed && let Err(error) = data.commit(&next) {
                // A directory sync can fail after rename. Report the failure, but
                // keep memory aligned with the actual file rather than revive old tokens.
                if let Ok(actual) = data.current() {
                    *data.values.lock().unwrap_or_else(PoisonError::into_inner) = actual;
                }
                return Err(error);
            }
            *data.values.lock().unwrap_or_else(PoisonError::into_inner) = next;
            Ok(result)
        })
        .await
        .map_err(|_| io::Error::other("secret transaction interrupted"))?
    }
}

impl SecretData {
    /// Reread disk for sequential external updates. Caller holds the owned
    /// same-process write guard; concurrent processes are deliberately unsupported.
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

    fn commit(&self, next: &BTreeMap<String, String>) -> io::Result<()> {
        let bytes = encrypt(&self.key, next)?;
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temporary = self.path.with_extension("tmp");
        let result = (|| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            set_owner_only(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)?;
            #[cfg(unix)]
            std::fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
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

#[cfg(unix)]
fn set_owner_only(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_: &Path) -> io::Result<()> {
    Ok(())
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
}
