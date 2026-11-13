use super::*;
use moli_api::airzone_setup::GatewayDiagnosticRequest as Request;

fn configured(port: u16) -> (PathBuf, Config) {
    let (dir, mut config) = house();
    config.drivers.push(DriverConfig {
        id: "diagnostic".into(),
        kind: "airzone".into(),
        enabled: Some(false),
        options: toml::from_str(&format!(
            "host = '127.0.0.1'\nport = {port}\nmac = 'aabbccddeeff'\nread_only = false"
        ))
        .unwrap(),
    });
    (dir, config)
}

#[tokio::test]
async fn manual_diagnostics_use_configured_identity_without_importing_or_commanding() {
    let (port, writes, stop) = controller().await;
    let (dir, config) = configured(port);
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
    let before = std::fs::read(dir.join("moli.toml")).unwrap();
    let report = manager
        .diagnostic(
            "diagnostic".into(),
            Request::Hvac {
                system: 1,
                zone: Some(1),
                get: true,
            },
        )
        .await
        .unwrap();
    assert!(report.read_only && !report.synthetic);
    assert_eq!(report.response.unwrap()["data"][0]["roomTemp"], 22.5);
    assert!(
        hub.snapshot().devices.is_empty(),
        "a manual read is not driver publication"
    );
    assert!(
        hub.snapshot().drivers.is_empty(),
        "a manual read must not start a paused driver"
    );
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    assert_eq!(before, std::fs::read(dir.join("moli.toml")).unwrap());
    assert!(!dir.join(FILE).exists());
    assert_eq!(
        manager
            .diagnostic("unknown".into(), Request::Webserver {})
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn diagnostics_reject_bad_selectors_and_a_different_gateway() {
    let reads = Arc::new(AtomicUsize::new(0));
    let (port, writes, stop) =
        controller_with_identity("aa:bb:cc:dd:ee:00", None, Some(reads.clone())).await;
    let (dir, config) = configured(port);
    let manager = Manager::new(
        Hub::new(HubOptions::default()).unwrap(),
        &config,
        CancellationToken::new(),
    )
    .unwrap();
    for request in [
        Request::Hvac {
            system: 127,
            zone: Some(1),
            get: false,
        },
        Request::Iaq {
            system: 1,
            sensor: 33,
            get: true,
        },
    ] {
        assert_eq!(
            manager
                .diagnostic("diagnostic".into(), request)
                .await
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    assert_eq!(
        manager
            .diagnostic("diagnostic".into(), Request::Version {})
            .await
            .unwrap_err()
            .code,
        "probe_failed"
    );
    assert_eq!(
        reads.load(Ordering::Relaxed),
        1,
        "identity mismatch stops before the requested resource"
    );
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn manual_diagnostics_are_single_flight_and_cancel_with_shutdown() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (port, _, stop) = controller_with_gate(Some((started.clone(), release.clone()))).await;
    let (dir, config) = configured(port);
    let cancel = CancellationToken::new();
    let manager = Manager::new(
        Hub::new(HubOptions::default()).unwrap(),
        &config,
        cancel.clone(),
    )
    .unwrap();
    let inspector = manager.clone();
    let pending = tokio::spawn(async move {
        inspector
            .diagnostic("diagnostic".into(), Request::Webserver {})
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();
    assert_eq!(
        manager
            .diagnostic("diagnostic".into(), Request::Webserver {})
            .await
            .unwrap_err()
            .code,
        "busy"
    );
    assert!(
        manager.list().await.is_ok(),
        "network inspection cannot hold the registry lock"
    );
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code,
        "shutting_down"
    );
    release.notify_one();
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}
