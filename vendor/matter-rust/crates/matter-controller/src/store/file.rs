//! Default filesystem-backed [`ControllerStore`].

use std::path::PathBuf;

use super::{ControllerStore, StoreError};

/// Stores the snapshot blob as a single file.
///
/// Writes are atomic (temp file + rename) and, on Unix, the file is
/// created with `0600` permissions. The blob holds private keys in the
/// clear: protect the containing directory, or supply a custom
/// [`ControllerStore`] backed by an encrypted store.
#[derive(Debug, Clone)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    /// Create a store backed by `path`. The file need not exist yet.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl ControllerStore for FileStore {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    fn save(&self, snapshot: &[u8]) -> Result<(), StoreError> {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};

        // Unique temp file per save: the controller runs best-effort persists
        // (address hints, resumption records) on detached blocking threads
        // that can overlap a durable save. With a SHARED temp path, writer B
        // truncates writer A's temp mid-write and whoever renames second gets
        // ENOENT — the intermittent "store-persist ENOENT" seen in the
        // integration sweep. Unique names keep each rename atomic and
        // last-writer-wins.
        static SAVE_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SAVE_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = self
            .path
            .with_extension(format!("tmp.{}.{seq}", std::process::id()));
        {
            let mut f = std::fs::File::create(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            f.write_all(snapshot)?;
            f.sync_all()?;
        }
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            // Don't leave key material lying around in a stray temp file.
            let _ = std::fs::remove_file(&tmp);
            return Err(StoreError::Io(e));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests with justification (CONTRIBUTING.md).
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        // Unique per (process, call): a fixed shared path races when two test
        // processes (or a re-run overlapping a prior run) touch the same file.
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let uniq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "matter-controller-test-{name}-{}-{uniq}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("tmp"));
        p
    }

    #[test]
    fn load_missing_returns_none() {
        let store = FileStore::new(temp_path("missing"));
        assert!(store.load().expect("load ok").is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_path("roundtrip");
        let store = FileStore::new(&path);
        store.save(b"hello snapshot").expect("save ok");
        assert_eq!(
            store.load().expect("load ok"),
            Some(b"hello snapshot".to_vec())
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_overwrites_atomically() {
        let path = temp_path("overwrite");
        let store = FileStore::new(&path);
        store.save(b"first").expect("save 1");
        store.save(b"second value longer").expect("save 2");
        assert_eq!(store.load().expect("load").unwrap(), b"second value longer");
        // no temp file may linger (unique names: check the parent dir)
        let stem = path.file_name().unwrap().to_string_lossy().into_owned();
        let stray = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with(&stem) && n.contains(".tmp")
            });
        assert!(!stray, "stray temp file left behind");
        let _ = std::fs::remove_file(&path);
    }

    /// Regression for the shared-temp-path race: concurrent saves from
    /// multiple threads (the controller's overlapping best-effort + durable
    /// persist shape) must ALL succeed, and the surviving file must be one
    /// of the written values, intact.
    #[test]
    fn concurrent_saves_all_succeed() {
        let path = temp_path("concurrent");
        let mut handles = Vec::new();
        for i in 0..8u8 {
            let store = FileStore::new(&path);
            handles.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    store.save(&[i; 64]).expect("concurrent save must not race");
                }
            }));
        }
        for h in handles {
            h.join().expect("saver thread");
        }
        let survivor = FileStore::new(&path).load().expect("load").unwrap();
        assert_eq!(survivor.len(), 64, "no torn write");
        assert!(survivor.iter().all(|b| *b == survivor[0]), "intact value");
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_path("perms");
        let store = FileStore::new(&path);
        store.save(b"secret").expect("save");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_file(&path);
    }
}
