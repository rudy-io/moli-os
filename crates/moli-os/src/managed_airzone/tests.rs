use super::*;
use moli_runtime::HubOptions;
use serde_json::{Value as Json, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static NEXT: AtomicUsize = AtomicUsize::new(0);
fn house() -> (PathBuf, Config) {
    let dir = std::env::temp_dir().join(format!(
        "moli-airzone-setup-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("moli.toml"),
        format!(
            "# preserved comment\n[server]\ndata_dir = {}\n",
            toml::Value::String(dir.to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let config = Config::load(&dir.join("moli.toml")).unwrap();
    (dir, config)
}
async fn controller() -> (u16, Arc<AtomicUsize>, CancellationToken) {
    controller_with_gate(None).await
}

async fn controller_with_gate(
    mut gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
) -> (u16, Arc<AtomicUsize>, CancellationToken) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let writes = Arc::new(AtomicUsize::new(0));
    let watched = writes.clone();
    let stop = CancellationToken::new();
    let closing = stop.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = tokio::select! {()=closing.cancelled()=>break, accepted=listener.accept()=>accepted.unwrap()};
            let mut raw = vec![0_u8; 8192];
            let mut used = 0;
            loop {
                let n = socket.read(&mut raw[used..]).await.unwrap();
                if n == 0 {
                    break;
                }
                used += n;
                let text = String::from_utf8_lossy(&raw[..used]);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if body.len() >= len {
                        break;
                    }
                }
            }
            let raw = String::from_utf8_lossy(&raw[..used]);
            let first = raw.lines().next().unwrap_or("");
            if first.contains("/webserver")
                && let Some((started, release)) = gate.take()
            {
                started.notify_one();
                release.notified().await;
            }
            if !first.starts_with("GET ")
                && !first.starts_with("POST /api/v1/webserver ")
                && !first.starts_with("POST /api/v1/hvac ")
                && !first.starts_with("POST /api/v1/integration ")
                && !first.starts_with("POST /api/v1/version ")
            {
                watched.fetch_add(1, Ordering::Relaxed);
            }
            let body = if first.contains("/webserver") {
                json!({"mac":"aa:bb:cc:dd:ee:ff","ws_type":"ws_az","ws_firmware":"4.21"})
            } else if first.contains("/hvac") {
                json!({"data":[{"systemID":1,"zoneID":1,"on":1,"roomTemp":22.5,"units":0}]})
            } else {
                json!({})
            }
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (port, writes, stop)
}
fn request(port: u16) -> GatewayRequest {
    GatewayRequest {
        instance: "airzone-added".into(),
        host: "127.0.0.1".into(),
        port,
        tls_fingerprint: None,
        expected_mac: Some("aabbccddeeff".into()),
    }
}
async fn populated(hub: &Hub) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while hub.snapshot().devices.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
fn read(path: &Path) -> Json {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn installed_gateway_is_saved_started_readonly_and_replayed_once() {
    let (dir, config) = house();
    let before = std::fs::read(dir.join("moli.toml")).unwrap();
    let (port, writes, stop) = controller().await;
    let cancel = CancellationToken::new();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, cancel.clone()).unwrap();
    let mut events = hub.subscribe();
    let preview = manager.probe(request(port)).await.unwrap();
    assert_eq!(preview.zones, 1);
    assert!(preview.read_only);
    let installed = manager
        .install(request(port), "test person".into())
        .await
        .unwrap();
    assert!(installed.stored);
    assert!(!installed.already_installed);
    let registered = events.recv().await.unwrap();
    let registered = serde_json::to_value(registered).unwrap();
    assert_eq!(registered["type"], "driver_registered");
    assert_eq!(registered["kind"], "airzone");
    populated(&hub).await;
    assert_eq!(hub.snapshot().drivers.len(), 1);
    assert!(
        hub.snapshot()
            .devices
            .iter()
            .flat_map(|d| &d.device.points)
            .all(|p| !p.access.write)
    );
    let stored = read(&dir.join(FILE));
    assert_eq!(stored["gateways"][0]["mac"], "aabbccddeeff");
    assert_eq!(stored["gateways"][0]["read_only"], true);
    let again = manager
        .install(request(port), "test person".into())
        .await
        .unwrap();
    assert!(again.already_installed);
    assert_eq!(hub.snapshot().drivers.len(), 1);
    assert_eq!(std::fs::read(dir.join("moli.toml")).unwrap(), before);
    let reopened = Config::load(&dir.join("moli.toml")).unwrap();
    assert_eq!(reopened.drivers.len(), 1);
    assert_eq!(
        reopened.drivers[0].options["read_only"].as_bool(),
        Some(true)
    );
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    cancel.cancel();
    manager.shutdown().await;
    let reboot_hub = Hub::new(HubOptions::default()).unwrap();
    let reboot_cancel = CancellationToken::new();
    let reboot_manager =
        Manager::new(reboot_hub.clone(), &reopened, reboot_cancel.clone()).unwrap();
    let (tasks, _) = crate::spawn_drivers(&reopened, &reboot_hub, &reboot_cancel).unwrap();
    populated(&reboot_hub).await;
    assert_eq!(reboot_hub.snapshot().drivers.len(), 1);
    assert!(
        reboot_hub
            .snapshot()
            .devices
            .iter()
            .flat_map(|d| &d.device.points)
            .all(|p| !p.access.write)
    );
    assert!(
        reboot_manager
            .install(request(port), "test person".into())
            .await
            .unwrap()
            .already_installed
    );
    reboot_cancel.cancel();
    reboot_manager.shutdown().await;
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn failed_save_never_starts_an_unsaved_driver() {
    let (dir, config) = house();
    let (port, _, stop) = controller().await;
    let cancel = CancellationToken::new();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, cancel.clone()).unwrap();
    std::fs::create_dir(dir.join(FILE)).unwrap();
    let error = manager
        .install(request(port), "test".into())
        .await
        .unwrap_err();
    assert_eq!(error.code, "persistence_failed");
    assert!(hub.snapshot().drivers.is_empty());
    assert!(dir.join(FILE).is_dir());
    cancel.cancel();
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

fn stored_gateway(instance: &str, port: u16) -> Gateway {
    Gateway {
        instance: instance.into(),
        host: "127.0.0.1".into(),
        port,
        tls_fingerprint: None,
        mac: "aabbccddeeff".into(),
        read_only: true,
    }
}

#[test]
fn a_starter_loads_saved_gateways_and_recovery_can_reach_a_corrupt_registry() {
    let (dir, _) = house();
    let store = Store {
        version: 1,
        gateways: vec![stored_gateway("airzone-kept", 3000)],
    };
    std::fs::write(dir.join(FILE), serde_json::to_vec(&store).unwrap()).unwrap();
    std::fs::remove_file(dir.join("moli.toml")).unwrap();
    let config = Config::starter(&dir.join("moli.toml")).unwrap();
    assert_eq!(
        config.drivers.len(),
        1,
        "starter must spawn restored managed gateways"
    );
    std::fs::write(dir.join(FILE), "broken json").unwrap();
    assert!(Config::load(&dir.join("moli.toml")).is_err());
    let recoverable = Config::load_for_recovery(&dir.join("moli.toml")).unwrap();
    assert_eq!(recoverable.server.data_dir, dir);
    assert_eq!(
        std::fs::read_to_string(dir.join(FILE)).unwrap(),
        "broken json"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn replay_never_claims_a_removed_registry_is_still_persisted() {
    let (dir, config) = house();
    let (port, _, stop) = controller().await;
    let cancel = CancellationToken::new();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub, &config, cancel.clone()).unwrap();
    manager.install(request(port), "test".into()).await.unwrap();
    std::fs::remove_file(dir.join(FILE)).unwrap();
    assert_eq!(
        manager
            .install(request(port), "test".into())
            .await
            .unwrap_err()
            .code,
        "persistence_failed"
    );
    cancel.cancel();
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_registry_changed_after_config_load_does_not_announce_an_inactive_gateway() {
    let (dir, config) = house();
    std::fs::write(
        dir.join(FILE),
        serde_json::to_vec(&Store {
            version: 1,
            gateways: vec![stored_gateway("late", 3000)],
        })
        .unwrap(),
    )
    .unwrap();
    let hub = Hub::new(HubOptions::default()).unwrap();
    assert!(Manager::new(hub, &config, CancellationToken::new()).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn recovery_can_load_base_config_without_discarding_a_broken_registry() {
    let (dir, _) = house();
    std::fs::write(dir.join(FILE), "broken json").unwrap();
    assert!(Config::load(&dir.join("moli.toml")).is_err());
    assert_eq!(
        Config::load_for_recovery(&dir.join("moli.toml"))
            .unwrap()
            .server
            .data_dir,
        dir
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(FILE)).unwrap(),
        "broken json"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn disabled_static_gateway_with_unknown_identity_cannot_be_duplicated_by_ip() {
    let (dir, mut config) = house();
    let (port, _, stop) = controller().await;
    config.drivers.push(DriverConfig {
        id: "existing".into(),
        kind: "airzone".into(),
        enabled: Some(false),
        options: toml::from_str("host = 'controller.local'\nport = 3000\nread_only = true")
            .unwrap(),
    });
    let cancel = CancellationToken::new();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub, &config, cancel.clone()).unwrap();
    assert_eq!(
        manager.probe(request(port)).await.unwrap_err().code,
        "identity_unavailable"
    );
    cancel.cancel();
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn disconnect_does_not_cancel_an_owned_install_or_duplicate_its_journal() {
    let (dir, config) = house();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (port, writes, stop) = controller_with_gate(Some((started.clone(), release.clone()))).await;
    let cancel = CancellationToken::new();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, cancel.clone()).unwrap();
    let caller = manager.clone();
    let http = tokio::spawn(async move { caller.install(request(port), "person".into()).await });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    assert_eq!(
        manager
            .install(request(port), "second".into())
            .await
            .unwrap_err()
            .code,
        "busy"
    );
    http.abort();
    assert!(http.await.unwrap_err().is_cancelled());
    release.notify_one();
    populated(&hub).await;
    assert_eq!(
        read(&dir.join(FILE))["gateways"].as_array().unwrap().len(),
        1
    );
    let journal = hub.journal(20);
    let adds: Vec<_> = journal
        .iter()
        .filter(|entry| matches!(entry.action, moli_core::Action::DriverAdded { .. }))
        .collect();
    assert_eq!(adds.len(), 1);
    assert_eq!(adds[0].actor.as_deref(), Some("person"));
    assert_eq!(adds[0].origin, moli_core::Origin::Ui);
    assert_eq!(adds[0].outcome, Outcome::Ok);
    assert!(
        manager
            .install(request(port), "retry".into())
            .await
            .unwrap()
            .already_installed
    );
    assert_eq!(hub.snapshot().drivers.len(), 1);
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), manager.shutdown())
        .await
        .unwrap();
    assert_eq!(
        manager.probe(request(port)).await.unwrap_err().code,
        "shutting_down"
    );
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn identities_endpoints_and_reserved_ids_cannot_be_added_twice() {
    let (dir, mut config) = house();
    let (port, _, stop) = controller().await;
    config.drivers.push(DriverConfig {
        id: "reserved".into(),
        kind: "airzone".into(),
        enabled: Some(false),
        options: toml::from_str(
            "host = 'controller.local'\nmac = '001122334455'\nread_only = true",
        )
        .unwrap(),
    });
    let cancel = CancellationToken::new();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, cancel.clone()).unwrap();
    let mut missing = request(port);
    missing.expected_mac = None;
    assert_eq!(
        manager
            .install(missing, "test".into())
            .await
            .unwrap_err()
            .code,
        "expected_identity_required"
    );
    let mut changed = request(port);
    changed.expected_mac = Some("112233445566".into());
    assert_eq!(
        manager
            .install(changed, "test".into())
            .await
            .unwrap_err()
            .code,
        "identity_changed"
    );
    let mut reserved = request(port);
    reserved.instance = "reserved".into();
    assert_eq!(
        manager.probe(reserved).await.unwrap_err().code,
        "duplicate_instance"
    );
    manager.install(request(port), "test".into()).await.unwrap();
    let mut endpoint = request(port);
    endpoint.instance = "different".into();
    assert_eq!(
        manager
            .install(endpoint, "test".into())
            .await
            .unwrap_err()
            .code,
        "duplicate_gateway"
    );
    let (other_port, _, other_stop) = controller().await;
    let mut alias = request(other_port);
    alias.instance = "alias".into();
    assert_eq!(
        manager
            .install(alias, "test".into())
            .await
            .unwrap_err()
            .code,
        "duplicate_gateway"
    );
    assert_eq!(hub.snapshot().drivers.len(), 1);
    assert_eq!(
        read(&dir.join(FILE))["gateways"].as_array().unwrap().len(),
        1
    );
    cancel.cancel();
    manager.shutdown().await;
    stop.cancel();
    other_stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn registry_is_restored_and_invalid_content_is_never_silently_reset() {
    let (dir, _) = house();
    let stored = json!({"version":1,"gateways":[stored_gateway("airzone-kept",3000)]});
    std::fs::write(dir.join(FILE), stored.to_string()).unwrap();
    let saved = dir.join("backup");
    crate::backup::backup(&dir, &saved).unwrap();
    assert!(read(&saved.join("manifest.json"))["files"][FILE].is_number());
    std::fs::write(dir.join(FILE), "broken json").unwrap();
    let recovery = Config::load_for_recovery(&dir.join("moli.toml")).unwrap();
    assert!(crate::backup::restore(&recovery.server.data_dir, &saved, false).is_err());
    crate::backup::restore(&recovery.server.data_dir, &saved, true).unwrap();
    assert_eq!(
        Config::load(&dir.join("moli.toml")).unwrap().drivers.len(),
        1
    );
    let mut writable = stored.clone();
    writable["gateways"][0]["read_only"] = json!(false);
    let mut future = stored.clone();
    future["version"] = json!(2);
    let mut duplicate = stored.clone();
    duplicate["gateways"]
        .as_array_mut()
        .unwrap()
        .push(stored["gateways"][0].clone());
    for invalid in [
        writable.to_string(),
        future.to_string(),
        duplicate.to_string(),
        " ".repeat(65_537),
    ] {
        std::fs::write(dir.join(FILE), &invalid).unwrap();
        assert!(Config::load(&dir.join("moli.toml")).is_err());
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), invalid);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn onboarding_accepts_only_bounded_private_ipv4_endpoints_and_normalizes_identity() {
    for host in [
        "example.com",
        "8.8.8.8",
        "169.254.1.2",
        "224.0.0.1",
        "0.0.0.0",
        "255.255.255.255",
        "::1",
        "127.0.0.1/path",
    ] {
        let mut invalid = request(3000);
        invalid.host = host.into();
        assert_eq!(
            normalize(invalid).unwrap_err().code,
            "invalid_host",
            "{host}"
        );
    }
    let mut canonical = request(3000);
    canonical.host = "192.168.1.2".into();
    canonical.expected_mac = Some("AA:BB:CC:DD:EE:FF".into());
    canonical.tls_fingerprint = Some("AB:".repeat(31) + "AB");
    let canonical = normalize(canonical).unwrap();
    assert_eq!(canonical.expected_mac.as_deref(), Some("aabbccddeeff"));
    assert_eq!(canonical.tls_fingerprint, Some("ab".repeat(32)));
    let mut invalid = request(0);
    assert_eq!(normalize(invalid.clone()).unwrap_err().code, "invalid_port");
    invalid.port = 3000;
    invalid.instance = "../../escape".into();
    assert_eq!(
        normalize(invalid.clone()).unwrap_err().code,
        "invalid_instance"
    );
    invalid.instance = "airzone-new".into();
    invalid.tls_fingerprint = Some("not-a-pin".into());
    assert_eq!(normalize(invalid).unwrap_err().code, "invalid_fingerprint");
}
