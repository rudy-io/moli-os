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
use std::sync::{Mutex, PoisonError};

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

const MAGIC: &[u8; 8] = b"MOLISEC1";
const AAD: &[u8] = b"moli-os secrets v1";

pub(crate) struct SecretStore {
    path: PathBuf,
    key: LessSafeKey,
    values: Mutex<BTreeMap<String, String>>,
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
            values: Mutex::new(values),
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
        let mut next = self.current()?;
        next.insert(name.to_owned(), value.to_owned());
        self.commit(next).await
    }

    pub(crate) async fn remove(&self, name: &str) -> io::Result<bool> {
        let _guard = self.write.lock().await;
        let mut next = self.current()?;
        if next.remove(name).is_none() {
            return Ok(false);
        }
        self.commit(next).await.map(|()| true)
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
    async fn commit(&self, next: BTreeMap<String, String>) -> io::Result<()> {
        let bytes = encrypt(&self.key, &next)?;
        crate::labels::write_atomic(&self.path, &bytes).await?;
        set_owner_only(&self.path);
        *self.values.lock().unwrap_or_else(PoisonError::into_inner) = next;
        Ok(())
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
fn set_owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_owner_only(_: &Path) {}

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
}
