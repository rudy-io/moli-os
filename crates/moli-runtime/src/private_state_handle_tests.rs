use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "moli-private-handle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn store(&self) -> SecretStore {
        SecretStore::open(self.0.join("secrets.enc"), &[31; 32]).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn handle(store: &SecretStore, instance: &str) -> PrivateState {
    PrivateState::new(store.clone(), instance.into()).unwrap()
}
fn catalog(value: &str) -> Vec<(String, String, String)> {
    vec![("catalog".into(), value.into(), value.into())]
}

#[tokio::test]
async fn purge_retires_old_handles_and_only_removes_selected_credentials() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let old = handle(&store, "bridge");
    store
        .set("bridge/token", "synthetic native token")
        .await
        .unwrap();
    old.store_secret("cloud_session", "synthetic cloud session")
        .await
        .unwrap();
    old.set_if_changed(catalog("private catalogue"))
        .await
        .unwrap();
    old.insert_if_absent("pending", "queued request")
        .await
        .unwrap();
    let other = handle(&store, "other");
    other
        .set_if_changed(catalog("other catalogue"))
        .await
        .unwrap();
    old.purge(vec!["cloud_session".into()]).await.unwrap();
    assert!(!old.is_current());
    assert!(old.get("catalog").is_none());
    assert!(
        old.store_secret("cloud_session", "must not return")
            .await
            .is_err()
    );
    assert!(
        old.set_if_changed(catalog("must not return"))
            .await
            .is_err()
    );
    assert!(
        old.insert_if_absent("pending", "must not return")
            .await
            .is_err()
    );
    assert!(old.take("pending").await.is_err());
    assert_eq!(
        store.get("bridge/token").as_deref(),
        Some("synthetic native token")
    );
    assert!(store.get("bridge/cloud_session").is_none());
    assert!(store.get(&format!("bridge/{PURGE_PENDING_NAME}")).is_none());
    assert_eq!(other.get("catalog").as_deref(), Some("other catalogue"));
    let reopened = fixture.store();
    let fresh = handle(&reopened, "bridge");
    assert!(fresh.get("catalog").is_none());
    assert!(fresh.get("pending").is_none());
    fresh
        .store_secret("cloud_session", "new connection")
        .await
        .unwrap();
}

#[tokio::test]
async fn stale_vault_cache_cannot_bypass_the_on_disk_generation_fence() {
    let fixture = Fixture::new();
    let first = fixture.store();
    let second = fixture.store();
    let stale = handle(&second, "bridge");
    handle(&first, "bridge").purge(vec![]).await.unwrap();
    // Its memory predates the other process, but mutation rechecks disk.
    assert!(stale.is_current());
    let error = stale
        .store_secret("session", "old process")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(fixture.store().get("bridge/session").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purge_waits_for_a_cancelled_callers_active_transaction_then_removes_its_writes() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let old = handle(&store, "bridge");
    let writer = old.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        writer
            .transaction(move |data, values, instance, epoch| {
                let _ = started.send(());
                blocked.recv().unwrap();
                private(data)?.insert_if_absent(
                    &data.key,
                    instance,
                    epoch,
                    "catalog",
                    "in flight",
                )?;
                values.insert(format!("{instance}/session"), "in flight".into());
                Ok(((), true))
            })
            .await
    });
    ready.await.unwrap();
    task.abort();
    let purger = old.clone();
    let purge = tokio::spawn(async move { purger.purge(vec!["session".into()]).await });
    tokio::task::yield_now().await;
    assert!(
        !purge.is_finished(),
        "purge must retain ordering against a running disk job"
    );
    release.send(()).unwrap();
    purge.await.unwrap().unwrap();
    assert!(!old.is_current());
    assert!(old.set_if_changed(catalog("resurrection")).await.is_err());
    let reopened = fixture.store();
    assert!(reopened.get("bridge/session").is_none());
    assert!(handle(&reopened, "bridge").get("catalog").is_none());
}

#[tokio::test]
async fn migration_is_retryable_and_never_replaces_a_newer_private_record() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let state = handle(&store, "bridge");
    store
        .set("bridge/legacy_catalog", "old catalogue")
        .await
        .unwrap();
    handle(&fixture.store(), "bridge")
        .set_if_changed(catalog("newer catalogue"))
        .await
        .unwrap();
    assert!(
        state.get("catalog").is_none(),
        "this process has not read the other writer's cache"
    );
    state
        .migrate(catalog("old catalogue"), vec!["legacy_catalog".into()])
        .await
        .unwrap();
    assert_eq!(state.get("catalog").as_deref(), Some("newer catalogue"));
    assert!(store.get("bridge/legacy_catalog").is_none());
    state
        .migrate(catalog("old catalogue"), vec!["legacy_catalog".into()])
        .await
        .unwrap();
    assert_eq!(
        handle(&fixture.store(), "bridge").get("catalog").as_deref(),
        Some("newer catalogue")
    );
}

#[tokio::test]
async fn failed_migration_keeps_legacy_data_until_the_private_copy_is_durable() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let state = handle(&store, "bridge");
    store
        .set("bridge/legacy_catalog", "old catalogue")
        .await
        .unwrap();
    std::fs::create_dir(fixture.0.join("private-state.enc")).unwrap();
    assert!(
        state
            .migrate(catalog("old catalogue"), vec!["legacy_catalog".into()])
            .await
            .is_err()
    );
    assert_eq!(
        store.get("bridge/legacy_catalog").as_deref(),
        Some("old catalogue")
    );
    std::fs::remove_dir(fixture.0.join("private-state.enc")).unwrap();
    state
        .migrate(
            vec![(
                "catalog".into(),
                "fresh must not be restored".into(),
                "stale catalogue".into(),
            )],
            vec!["legacy_catalog".into()],
        )
        .await
        .unwrap();
    assert!(store.get("bridge/legacy_catalog").is_none());
    assert_eq!(
        handle(&fixture.store(), "bridge").get("catalog").as_deref(),
        Some("stale catalogue")
    );
}

#[tokio::test]
async fn reserved_epoch_cannot_be_overwritten_and_corrupt_cache_does_not_break_credentials() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let state = handle(&store, "bridge");
    assert!(
        state
            .store_secret(EPOCH_NAME, "forced epoch")
            .await
            .is_err()
    );
    assert!(state.purge(vec![EPOCH_NAME.into()]).await.is_err());
    assert!(
        state
            .store_secret(PURGE_PENDING_NAME, "forced marker")
            .await
            .is_err()
    );
    assert!(state.purge(vec![PURGE_PENDING_NAME.into()]).await.is_err());
    store.set("native/token", "kept").await.unwrap();
    std::fs::write(
        fixture.0.join("private-state.enc"),
        b"corrupt encrypted file",
    )
    .unwrap();
    let reopened = fixture.store();
    assert!(PrivateState::new(reopened.clone(), "bridge".into()).is_none());
    assert_eq!(reopened.get("native/token").as_deref(), Some("kept"));
    reopened
        .set("native/token", "native still works")
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_cache_purge_keeps_a_durable_cleanup_marker_after_revoking_credentials() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let old = handle(&store, "bridge");
    old.store_secret("cloud_session", "synthetic session")
        .await
        .unwrap();
    old.set_if_changed(catalog("private catalogue"))
        .await
        .unwrap();
    let sidecar = fixture.0.join("private-state.enc");
    std::fs::remove_file(&sidecar).unwrap();
    std::fs::create_dir(&sidecar).unwrap();
    assert!(old.purge(vec!["cloud_session".into()]).await.is_err());
    let reopened = fixture.store();
    assert!(reopened.get("bridge/cloud_session").is_none());
    assert_eq!(
        reopened
            .get("bridge/private_state_purge_pending_v1")
            .as_deref(),
        Some("1")
    );
    assert!(!old.is_current());
}

#[tokio::test]
async fn owner_purge_revokes_credentials_with_corrupt_cache_and_can_retry_scoped_cleanup() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let old = handle(&store, "bridge");
    old.store_secret("cloud_session", "synthetic cloud session")
        .await
        .unwrap();
    store.set("bridge/token", "native kept").await.unwrap();
    old.set_if_changed(catalog("private catalogue"))
        .await
        .unwrap();
    handle(&store, "other")
        .set_if_changed(catalog("other catalogue"))
        .await
        .unwrap();
    store
        .set("other/cloud_session", "other kept")
        .await
        .unwrap();
    let sidecar = fixture.0.join("private-state.enc");
    let valid_cache = std::fs::read(&sidecar).unwrap();
    std::fs::write(&sidecar, b"corrupt encrypted file").unwrap();
    let reopened = fixture.store();
    assert!(PrivateState::new(reopened.clone(), "bridge".into()).is_none());
    reopened
        .set("bridge/token", "native still works")
        .await
        .unwrap();
    assert!(
        !purge_retained(
            reopened.clone(),
            "bridge".into(),
            vec!["cloud_session".into()]
        )
        .await
        .unwrap()
    );
    assert!(reopened.get("bridge/cloud_session").is_none());
    assert!(reopened.get(&format!("bridge/{EPOCH_NAME}")).is_some());
    assert_eq!(
        reopened
            .get(&format!("bridge/{PURGE_PENDING_NAME}"))
            .as_deref(),
        Some("1")
    );
    assert_eq!(
        reopened.get("bridge/token").as_deref(),
        Some("native still works")
    );
    assert_eq!(
        reopened.get("other/cloud_session").as_deref(),
        Some("other kept")
    );
    assert_eq!(std::fs::read(&sidecar).unwrap(), b"corrupt encrypted file");
    assert_eq!(
        old.store_secret("cloud_session", "must not return")
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(!old.is_current());

    // A repaired cache can be cleaned on retry, without touching another owner.
    std::fs::write(&sidecar, valid_cache).unwrap();
    let repaired = fixture.store();
    assert!(handle(&repaired, "bridge").get("catalog").is_none());
    assert!(
        purge_retained(
            repaired.clone(),
            "bridge".into(),
            vec!["cloud_session".into()]
        )
        .await
        .unwrap()
    );
    let final_store = fixture.store();
    assert!(
        final_store
            .get(&format!("bridge/{PURGE_PENDING_NAME}"))
            .is_none()
    );
    assert!(handle(&final_store, "bridge").get("catalog").is_none());
    assert_eq!(
        handle(&final_store, "other").get("catalog").as_deref(),
        Some("other catalogue")
    );
}

#[tokio::test]
async fn owner_purge_lock_failure_leaves_credentials_and_cache_unchanged() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let state = handle(&store, "bridge");
    state
        .store_secret("cloud_session", "synthetic session")
        .await
        .unwrap();
    state
        .set_if_changed(catalog("private catalogue"))
        .await
        .unwrap();
    let vault = fixture.0.join("secrets.enc");
    let sidecar = fixture.0.join("private-state.enc");
    let before_vault = std::fs::read(&vault).unwrap();
    let before_cache = std::fs::read(&sidecar).unwrap();
    let lock = fixture.0.join("secrets.enc.lock");
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    assert!(
        purge_retained(store.clone(), "bridge".into(), vec!["cloud_session".into()])
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(vault).unwrap(), before_vault);
    assert_eq!(std::fs::read(sidecar).unwrap(), before_cache);
    assert_eq!(
        store.get("bridge/cloud_session").as_deref(),
        Some("synthetic session")
    );
    assert!(store.get(&format!("bridge/{PURGE_PENDING_NAME}")).is_none());
    assert!(state.is_current());
}
