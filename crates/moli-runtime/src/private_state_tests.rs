use super::*;
use ring::aead::{CHACHA20_POLY1305, UnboundKey};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "moli-private-state-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn vault(&self) -> PathBuf {
        self.0.join("secrets.enc")
    }
    fn private_file(&self) -> PathBuf {
        self.0.join(FILE_NAME)
    }
    fn store(&self, key: &LessSafeKey) -> PrivateStateStore {
        PrivateStateStore::open(&self.vault(), key).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn key(seed: u8) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, &[seed; 32]).unwrap())
}

fn entry(name: &str, live: &str, durable: &str) -> (String, String, String) {
    (name.into(), live.into(), durable.into())
}

#[test]
fn unchanged_durable_data_refreshes_live_cache_without_rewriting_ciphertext() {
    let fixture = Fixture::new();
    let key = key(17);
    let store = fixture.store(&key);
    let old = r#"{"name":"Synthetic scene","observed":10}"#;
    let fresh = r#"{"name":"Synthetic scene","observed":20}"#;
    let durable = r#"{"name":"Synthetic scene","observed":null}"#;
    assert!(
        store
            .set_if_changed(
                &key,
                "bridge",
                "generation-a",
                vec![entry("catalog", old, durable)]
            )
            .unwrap()
    );
    let bytes = std::fs::read(fixture.private_file()).unwrap();
    let modified = std::fs::metadata(fixture.private_file())
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        !bytes
            .windows("Synthetic scene".len())
            .any(|w| w == b"Synthetic scene")
    );
    assert!(
        !fixture.vault().exists(),
        "observations never create the credentials vault"
    );
    assert!(!format!("{store:?}").contains("Synthetic"));
    assert!(
        !store
            .set_if_changed(
                &key,
                "bridge",
                "generation-a",
                vec![entry("catalog", fresh, durable)]
            )
            .unwrap()
    );
    assert_eq!(std::fs::read(fixture.private_file()).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(fixture.private_file())
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    assert_eq!(
        store.get("bridge", "generation-a", "catalog").as_deref(),
        Some(fresh)
    );
    let reopened = fixture.store(&key);
    assert_eq!(
        reopened.get("bridge", "generation-a", "catalog").as_deref(),
        Some(durable)
    );
    assert!(PrivateStateStore::open(&fixture.vault(), &self::key(18)).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(fixture.private_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn a_changed_payload_is_written_and_epochs_do_not_share_data() {
    let fixture = Fixture::new();
    let key = key(17);
    let store = fixture.store(&key);
    store
        .set_if_changed(
            &key,
            "bridge",
            "a",
            vec![entry("catalog", "first", "first")],
        )
        .unwrap();
    store
        .set_if_changed(
            &key,
            "other",
            "a",
            vec![entry("catalog", "unrelated", "unrelated")],
        )
        .unwrap();
    let before = std::fs::read(fixture.private_file()).unwrap();
    assert!(
        store
            .set_if_changed(
                &key,
                "bridge",
                "a",
                vec![entry("catalog", "second", "second")]
            )
            .unwrap()
    );
    assert_ne!(std::fs::read(fixture.private_file()).unwrap(), before);
    assert!(store.get("bridge", "b", "catalog").is_none());
    store
        .set_if_changed(
            &key,
            "bridge",
            "b",
            vec![entry("state", "new session", "new session")],
        )
        .unwrap();
    assert!(store.get("bridge", "a", "catalog").is_none());
    assert!(store.get("bridge", "b", "catalog").is_none());
    assert_eq!(
        store.get("other", "a", "catalog").as_deref(),
        Some("unrelated")
    );
    assert_eq!(store.instances(), vec!["bridge", "other"]);
}

#[test]
fn pending_requests_survive_restart_and_are_consumed_once() {
    let fixture = Fixture::new();
    let key = key(17);
    let store = fixture.store(&key);
    assert!(
        store
            .insert_if_absent(&key, "bridge", "a", "pending", "request-a")
            .unwrap()
    );
    assert!(
        !store
            .insert_if_absent(&key, "bridge", "a", "pending", "request-b")
            .unwrap()
    );
    let reopened = fixture.store(&key);
    assert!(
        reopened
            .take(&key, "bridge", "b", "pending")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        reopened
            .take(&key, "bridge", "a", "pending")
            .unwrap()
            .as_deref(),
        Some("request-a")
    );
    assert!(
        reopened
            .take(&key, "bridge", "a", "pending")
            .unwrap()
            .is_none()
    );
    assert!(fixture.store(&key).get("bridge", "a", "pending").is_none());
}

#[test]
fn purging_one_namespace_preserves_others_and_reopen_cannot_restore_it() {
    let fixture = Fixture::new();
    let key = key(17);
    let store = fixture.store(&key);
    for instance in ["bridge", "other"] {
        store
            .insert_if_absent(&key, instance, "a", "catalog", "retained")
            .unwrap();
    }
    assert!(store.purge_namespace(&key, "bridge").unwrap());
    assert!(!store.purge_namespace(&key, "bridge").unwrap());
    assert!(store.get("bridge", "a", "catalog").is_none());
    assert!(fixture.store(&key).get("bridge", "a", "catalog").is_none());
    assert_eq!(
        store.get("other", "a", "catalog").as_deref(),
        Some("retained")
    );
}

#[test]
fn a_missing_file_is_empty_and_does_not_resurrect_the_memory_snapshot() {
    let fixture = Fixture::new();
    let key = key(17);
    let store = fixture.store(&key);
    store
        .insert_if_absent(&key, "old", "a", "catalog", "old private data")
        .unwrap();
    std::fs::remove_file(fixture.private_file()).unwrap();
    store
        .insert_if_absent(&key, "new", "b", "state", "new private data")
        .unwrap();
    assert!(store.get("old", "a", "catalog").is_none());
    assert!(fixture.store(&key).get("old", "a", "catalog").is_none());
    assert_eq!(store.instances(), vec!["new"]);
}

#[test]
fn separate_handles_merge_disk_changes_instead_of_overwriting_other_instances() {
    let fixture = Fixture::new();
    let key = key(17);
    let first = fixture.store(&key);
    let second = fixture.store(&key);
    first
        .insert_if_absent(&key, "first", "a", "catalog", "one")
        .unwrap();
    second
        .insert_if_absent(&key, "second", "b", "catalog", "two")
        .unwrap();
    let reopened = fixture.store(&key);
    assert_eq!(
        reopened.get("first", "a", "catalog").as_deref(),
        Some("one")
    );
    assert_eq!(
        reopened.get("second", "b", "catalog").as_deref(),
        Some("two")
    );
}

#[test]
fn invalid_or_tampered_files_fail_closed_without_overwriting_the_file() {
    let fixture = Fixture::new();
    let key = key(17);
    let store = fixture.store(&key);
    store
        .insert_if_absent(&key, "bridge", "a", "catalog", "private data")
        .unwrap();
    let mut bytes = std::fs::read(fixture.private_file()).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(fixture.private_file(), &bytes).unwrap();
    assert!(PrivateStateStore::open(&fixture.vault(), &key).is_err());
    assert!(
        store
            .set_if_changed(&key, "bridge", "a", vec![entry("catalog", "new", "new")])
            .is_err()
    );
    assert_eq!(std::fs::read(fixture.private_file()).unwrap(), bytes);
    assert_eq!(
        store.get("bridge", "a", "catalog").as_deref(),
        Some("private data")
    );
    let wrong_format = encrypt(
        &key,
        &BTreeMap::from([("session".into(), "not private state".into())]),
    )
    .unwrap();
    std::fs::write(fixture.private_file(), wrong_format).unwrap();
    assert!(PrivateStateStore::open(&fixture.vault(), &key).is_err());
}

#[test]
fn invalid_entries_cannot_partially_publish_a_batch() {
    let fixture = Fixture::new();
    let key = key(17);
    let store = fixture.store(&key);
    let entries = vec![
        entry("catalog", "valid", "valid"),
        entry("catalog", "duplicate", "duplicate"),
    ];
    assert!(store.set_if_changed(&key, "bridge", "a", entries).is_err());
    assert!(store.instances().is_empty());
    assert!(!fixture.private_file().exists());
    let oversized = "x".repeat(MAX_VALUE_BYTES + 1);
    assert!(
        store
            .insert_if_absent(&key, "bridge", "a", "catalog", &oversized)
            .is_err()
    );
    assert!(!fixture.private_file().exists());
}

#[test]
fn directory_sync_failure_reports_error_but_cache_tracks_the_completed_rename() {
    let fixture = Fixture::new();
    let key = key(17);
    let store = fixture.store(&key);
    store
        .insert_if_absent(&key, "bridge", "a", "pending", "already dispatched")
        .unwrap();
    let current = read(&fixture.private_file(), &key).unwrap();
    let result = store.commit(&key, &current, &Snapshot::new(), Snapshot::new(), |_| {
        Err(io::Error::other("synthetic directory sync failure"))
    });
    assert!(
        result.is_err(),
        "failed durability must never be acknowledged"
    );
    assert!(store.get("bridge", "a", "pending").is_none());
    assert!(fixture.store(&key).get("bridge", "a", "pending").is_none());
    let bytes = std::fs::read(fixture.private_file()).unwrap();
    let current = read(&fixture.private_file(), &key).unwrap();
    assert!(
        store
            .commit(&key, &current, &current, current.clone(), |_| {
                Err(io::Error::other("directory sync still unavailable"))
            })
            .is_err(),
        "an identical retry must not acknowledge uncertain durability"
    );
    let synced = std::cell::Cell::new(false);
    assert!(
        !store
            .commit(&key, &current, &current, current.clone(), |_| {
                synced.set(true);
                Ok(())
            })
            .unwrap()
    );
    assert!(synced.get());
    assert_eq!(
        std::fs::read(fixture.private_file()).unwrap(),
        bytes,
        "retry fsync does not rewrite ciphertext"
    );
    let current = read(&fixture.private_file(), &key).unwrap();
    assert!(
        !store
            .commit(&key, &current, &current, current.clone(), |_| {
                panic!("confirmed no-op must not sync again")
            })
            .unwrap()
    );
}

#[test]
fn ownership_reload_sees_later_disk_data_and_keeps_other_instance_live() {
    let fixture = Fixture::new();
    let key = key(17);
    let store = fixture.store(&key);
    store
        .set_if_changed(
            &key,
            "first",
            "epoch",
            vec![entry("catalog", "live-first", "durable-first")],
        )
        .unwrap();
    store
        .set_if_changed(
            &key,
            "other",
            "epoch",
            vec![entry("catalog", "live-other", "durable-other")],
        )
        .unwrap();
    let writer = fixture.store(&key);
    writer
        .set_if_changed(
            &key,
            "first",
            "epoch",
            vec![entry("catalog", "live-new", "durable-new")],
        )
        .unwrap();
    let bytes = std::fs::read(fixture.private_file()).unwrap();
    store.reload_instance(&key, "first").unwrap();
    assert_eq!(
        store.get("first", "epoch", "catalog").as_deref(),
        Some("durable-new")
    );
    assert_eq!(
        store.get("other", "epoch", "catalog").as_deref(),
        Some("live-other")
    );
    assert_eq!(std::fs::read(fixture.private_file()).unwrap(), bytes);
}
