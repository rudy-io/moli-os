use matter_controller::{ControllerStore, StoreError};
use moli_runtime::DurableSecrets;
use std::io;
const LIMIT: usize = 1024 * 1024;
const KEY: &str = "controller_snapshot";

pub struct VaultStore {
    secrets: DurableSecrets,
    _lease: moli_runtime::DurableLease,
    cache: std::sync::Mutex<Option<Vec<u8>>>,
    write: std::sync::Mutex<()>,
}
impl VaultStore {
    pub fn open(secrets: DurableSecrets) -> Result<Self, StoreError> {
        let lease = secrets.try_lease("controller").map_err(|_| error())?;
        let cache = secrets.get(KEY).map(|s| decode(&s)).transpose()?;
        Ok(Self {
            secrets,
            _lease: lease,
            cache: std::sync::Mutex::new(cache),
            write: std::sync::Mutex::new(()),
        })
    }
}
impl std::fmt::Debug for VaultStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultStore([redacted])")
    }
}
fn error() -> StoreError {
    io::Error::other(moli_i18n::tr!("pilotes.matter.storage")).into()
}
fn decode(s: &str) -> Result<Vec<u8>, StoreError> {
    if s.len() > LIMIT * 2 || !s.len().is_multiple_of(2) {
        return Err(error());
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| {
            let digit = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                _ => None,
            };
            Ok(digit(b[0]).ok_or_else(error)? * 16 + digit(b[1]).ok_or_else(error)?)
        })
        .collect()
}
impl ControllerStore for VaultStore {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.cache.lock().map_err(|_| error())?.clone())
    }
    fn save(&self, snapshot: &[u8]) -> Result<(), StoreError> {
        use std::fmt::Write as _;
        if snapshot.len() > LIMIT {
            return Err(error());
        }
        let mut hex = String::with_capacity(snapshot.len() * 2);
        for b in snapshot {
            write!(hex, "{b:02x}").map_err(|_| error())?;
        }
        let _write = self.write.lock().map_err(|_| error())?;
        self.secrets.set(KEY, &hex).map_err(|_| error())?;
        *self.cache.lock().map_err(|_| error())? = Some(snapshot.to_vec());
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_snapshot() {
        assert_eq!(decode("00ff10").unwrap(), [0, 255, 16]);
        for s in ["0", "zz", "é", "FF"] {
            assert!(decode(s).is_err());
        }
        assert!(decode(&"a".repeat(LIMIT * 2 + 2)).is_err());
    }
}
