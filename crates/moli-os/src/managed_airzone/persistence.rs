//! Distinguish a failed write from a completed rename awaiting durable confirmation.

use std::io::{self, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{MAX_BYTES, Store, bytes};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

pub(super) enum SaveOutcome {
    Durable(Vec<u8>),
    /// The file already contains these bytes, but its directory sync failed.
    /// Retain this snapshot for conflict detection and require an explicit retry.
    Unconfirmed(Vec<u8>),
}

pub(super) fn sync_directory(parent: &Path) -> io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}

pub(super) fn save(
    path: &Path,
    store: &Store,
    expected: Option<&[u8]>,
    sync_directory: fn(&Path) -> io::Result<()>,
) -> io::Result<SaveOutcome> {
    if bytes(path)?.as_deref() != expected {
        return Err(io::Error::other(
            "Airzone registry changed outside this operation",
        ));
    }
    let raw = serde_json::to_vec_pretty(store).map_err(io::Error::other)?;
    if raw.len() as u64 > MAX_BYTES {
        return Err(io::Error::other("Airzone registry exceeds its bound"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing registry directory"))?;
    let aside = parent.join(format!(
        ".airzone-gateways-{}-{}.new",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let replaced = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&aside)?;
        file.write_all(&raw)?;
        file.sync_all()?;
        std::fs::rename(&aside, path)
    })();
    if let Err(error) = replaced {
        let _ = std::fs::remove_file(&aside);
        return Err(error);
    }
    // A failure here cannot undo the rename. Return its exact bytes so the
    // manager preserves its safe effective mode and can retry against this file.
    Ok(if sync_directory(parent).is_ok() {
        SaveOutcome::Durable(raw)
    } else {
        SaveOutcome::Unconfirmed(raw)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "moli-airzone-persistence-{}-{}",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn registry(&self) -> PathBuf {
            self.0.join("airzone-gateways.json")
        }
        fn no_temporary_files(&self) {
            assert!(std::fs::read_dir(&self.0).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".airzone-gateways-")
            }));
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn failed_sync(_: &Path) -> io::Result<()> {
        Err(io::Error::other("synthetic directory sync failure"))
    }
    fn must_not_sync(_: &Path) -> io::Result<()> {
        panic!("a failed replacement must not reach directory synchronization")
    }

    #[test]
    fn failed_directory_sync_tracks_renamed_bytes_and_retry_never_claims_false_durability() {
        let fixture = Fixture::new();
        let path = fixture.registry();
        let store = Store::default();
        let SaveOutcome::Unconfirmed(first) = save(&path, &store, None, failed_sync).unwrap()
        else {
            panic!("failed directory sync must not confirm durability");
        };
        assert_eq!(std::fs::read(&path).unwrap(), first);
        fixture.no_temporary_files();
        let SaveOutcome::Unconfirmed(retried) =
            save(&path, &store, Some(&first), failed_sync).unwrap()
        else {
            panic!("the same payload still needs a successful directory sync");
        };
        assert_eq!(retried, first);
        let SaveOutcome::Durable(confirmed) =
            save(&path, &store, Some(&retried), sync_directory).unwrap()
        else {
            panic!("successful synchronization must confirm the current file");
        };
        assert_eq!(confirmed, first);
        assert_eq!(std::fs::read(&path).unwrap(), confirmed);
        fixture.no_temporary_files();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn a_stale_expected_snapshot_cannot_overwrite_an_unconfirmed_rename() {
        let fixture = Fixture::new();
        let path = fixture.registry();
        let SaveOutcome::Unconfirmed(raw) =
            save(&path, &Store::default(), None, failed_sync).unwrap()
        else {
            panic!("synthetic directory sync must fail");
        };
        assert!(save(&path, &Store::default(), None, must_not_sync).is_err());
        assert_eq!(std::fs::read(path).unwrap(), raw);
        fixture.no_temporary_files();
    }

    #[test]
    fn failure_before_rename_preserves_the_previous_registry() {
        let fixture = Fixture::new();
        let path = fixture.registry();
        let original = b"previous registry";
        std::fs::write(&path, original).unwrap();
        let too_large = Store {
            gateways: vec![super::super::Gateway {
                instance: "a".repeat(usize::try_from(MAX_BYTES).unwrap()),
                host: "127.0.0.1".into(),
                port: 3000,
                tls_fingerprint: None,
                mac: "aabbccddeeff".into(),
                read_only: true,
                enabled: true,
            }],
            ..Store::default()
        };
        assert!(save(&path, &too_large, Some(original), must_not_sync).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
        fixture.no_temporary_files();
    }

    #[test]
    fn directory_sync_errors_are_not_swallowed() {
        let fixture = Fixture::new();
        assert!(sync_directory(&fixture.0.join("missing")).is_err());
    }
}
