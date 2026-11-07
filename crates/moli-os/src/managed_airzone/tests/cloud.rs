use super::*;
use moli_api::airzone_cloud::{LoginRequest, ReadRequest};

#[tokio::test]
async fn cloud_consultation_requires_a_vault_and_never_starts_local_drivers() {
    let (dir, config) = house();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
    let status = manager.cloud_status().await.unwrap();
    assert_eq!(status["state"], "disconnected");
    assert_eq!(status["read_only"], true);
    assert_eq!(status["persistence_available"], false);
    assert_eq!(
        manager
            .cloud_login(LoginRequest {
                email: "user@example.com".into(),
                password: "fixture-password".into()
            })
            .await
            .unwrap_err()
            .error,
        "storage"
    );
    assert_eq!(
        manager
            .cloud_read(ReadRequest::Installations {})
            .await
            .unwrap_err()
            .error,
        "authentication_required"
    );
    assert!(hub.snapshot().devices.is_empty());
    assert!(hub.snapshot().drivers.is_empty());
    manager.shutdown().await;
    assert_eq!(
        manager.cloud_status().await.unwrap_err().error,
        "unavailable"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn shutdown_cancels_inflight_cloud_work_before_it_can_continue() {
    let (dir, config) = house();
    let manager = Manager::new(
        Hub::new(HubOptions::default()).unwrap(),
        &config,
        CancellationToken::new(),
    )
    .unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let completed = Arc::new(AtomicUsize::new(0));
    let pending = manager.clone();
    let signal = started.clone();
    let blocked = release.clone();
    let finished = completed.clone();
    let operation = tokio::spawn(async move {
        pending
            .cloud_operation(async move {
                signal.notify_one();
                blocked.notified().await;
                finished.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();
    manager.shutdown().await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), operation)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .error,
        "unavailable"
    );
    release.notify_one();
    assert_eq!(completed.load(Ordering::Relaxed), 0);
    std::fs::remove_dir_all(dir).unwrap();
}
