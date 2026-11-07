use super::*;
use moli_runtime::HubOptions;
use serde_json::{Value as Json, json};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static NEXT: AtomicUsize = AtomicUsize::new(0);

mod cloud;
mod diagnostics;
mod modes;

#[tokio::test]
async fn discovery_never_uses_an_address_learned_for_an_old_configuration() {
    let (dir, mut config) = house();
    config.drivers.push(DriverConfig {
        id: "nearby".into(),
        kind: "unused".into(),
        enabled: None,
        options: toml::toml! { host = "10.0.0.2" }.into(),
    });
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
    for (configured, expected) in [
        ("192.168.1.2", vec!["10.0.0.2"]),
        ("10.0.0.2", vec!["10.1.0.3", "10.0.0.2"]),
    ] {
        hub.remember_address(
            &"nearby".into(),
            moli_runtime::KnownAddress {
                configured: configured.into(),
                host: "10.1.0.3".into(),
                mac: None,
                identity: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            manager
                .discovery_hints()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            expected
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

struct WaitingDiscovery(Arc<tokio::sync::Notify>);
impl discovery::Inspector for WaitingDiscovery {
    fn connect<'a>(&'a self, _target: &'a discovery::Target) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.0.notify_one();
            std::future::pending::<bool>().await
        })
    }
    fn probe<'a>(
        &'a self,
        _target: &'a discovery::Target,
    ) -> BoxFuture<'a, Option<moli_airzone::GatewayProbe>> {
        Box::pin(async { panic!("the waiting connection must never reach a probe") })
    }
}

#[tokio::test]
async fn shutting_down_the_manager_cancels_an_active_search() {
    let (dir, mut config) = house();
    config.drivers.push(DriverConfig {
        id: "network-hint".into(),
        kind: "unused".into(),
        enabled: None,
        options: toml::toml! { host = "10.0.0.2" }.into(),
    });
    let manager = Manager::new(
        Hub::new(HubOptions::default()).unwrap(),
        &config,
        CancellationToken::new(),
    )
    .unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let scanning = manager.clone();
    let waiting = started.clone();
    let scan = tokio::spawn(async move {
        scanning
            .discover_with(Arc::new(WaitingDiscovery(waiting)))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();
    manager.shutdown().await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), scan)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code,
        "shutting_down"
    );
    assert_eq!(manager.discover().await.unwrap_err().code, "shutting_down");
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn discovery_does_not_hold_the_registry_lock_while_installing_a_fake_controller() {
    let (dir, mut config) = house();
    config.drivers.push(DriverConfig {
        id: "network-hint".into(),
        kind: "unused".into(),
        enabled: None,
        options: toml::toml! { host = "10.0.0.2" }.into(),
    });
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub, &config, CancellationToken::new()).unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let scanning = manager.clone();
    let waiting = started.clone();
    let scan = tokio::spawn(async move {
        scanning
            .discover_with(Arc::new(WaitingDiscovery(waiting)))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();
    assert!(manager.list().await.unwrap().gateways.is_empty());
    assert_eq!(manager.discover().await.unwrap_err().code, "busy");
    let (port, writes, stop) = controller().await;
    let installed = tokio::time::timeout(
        Duration::from_secs(2),
        manager.install(request(port), "test".into()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(installed.stored && installed.read_only);
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    scan.abort();
    assert!(scan.await.unwrap_err().is_cancelled());
    assert_eq!(
        manager.discover().await.unwrap_err().code,
        "busy",
        "cancelled search keeps rate budget"
    );
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn static_gateway_override_preserves_options_and_withdrawal_survives_restart() {
    let (dir, _) = house();
    let path = dir.join("moli.toml");
    let mut toml = std::fs::read_to_string(&path).unwrap();
    toml.push_str("\n[[driver]]\nid = 'configured'\nkind = 'airzone'\n[driver.options]\nhost = '127.0.0.1'\nport = 3000\nmac = 'aabbccddeeff'\npoll_secs = 67\ndhw_units = 1\nread_only = false\nwrite = 'extended'\n");
    std::fs::write(&path, &toml).unwrap();
    let registry = json!({"version":2,"gateways":[],"overrides":[{"instance":"configured","enabled":false,"connection":null}]});
    std::fs::write(dir.join(FILE), registry.to_string()).unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(config.drivers.len(), 1);
    assert_eq!(
        config.drivers[0].options["write"].as_str(),
        Some("extended")
    );
    assert_eq!(config.drivers[0].enabled, Some(false));
    assert_eq!(
        config.drivers[0].options["poll_secs"].as_integer(),
        Some(67)
    );
    assert_eq!(
        config.drivers[0].options["read_only"].as_bool(),
        Some(false)
    );
    let mut moved = registry;
    moved["overrides"][0]["enabled"] = json!(true);
    moved["overrides"][0]["connection"] = json!({"instance":"configured","host":"127.0.0.2","port":4000,"mac":"aabbccddeeff","read_only":true,"enabled":true});
    std::fs::write(dir.join(FILE), moved.to_string()).unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(
        config.drivers[0].options["host"].as_str(),
        Some("127.0.0.2")
    );
    assert_eq!(config.drivers[0].options["port"].as_integer(), Some(4000));
    assert_eq!(
        config.drivers[0].options["poll_secs"].as_integer(),
        Some(67)
    );
    assert_eq!(config.drivers[0].options["dhw_units"].as_integer(), Some(1));
    assert_eq!(config.drivers[0].options["read_only"].as_bool(), Some(true));
    assert_eq!(
        config.drivers[0].options["write"].as_str(),
        Some("extended")
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), toml);
    std::fs::remove_dir_all(dir).unwrap();
}
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
    gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
) -> (u16, Arc<AtomicUsize>, CancellationToken) {
    controller_with_identity("aa:bb:cc:dd:ee:ff", gate, None).await
}

async fn controller_with_identity(
    mac: &'static str,
    mut gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    requests: Option<Arc<AtomicUsize>>,
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
            // A cancelled request may close a connected socket before sending
            // any HTTP bytes; this is not a controller read or write.
            if used == 0 {
                continue;
            }
            let raw = String::from_utf8_lossy(&raw[..used]);
            let first = raw.lines().next().unwrap_or("");
            if let Some(count) = &requests {
                count.fetch_add(1, Ordering::Relaxed);
            }
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
            let body = if first.starts_with("PUT /api/v1/hvac ") {
                let body: Json =
                    serde_json::from_str(raw.split_once("\r\n\r\n").unwrap().1).unwrap();
                json!({"data":[body]})
            } else if first.contains("/webserver") {
                json!({"mac":mac,"ws_type":"ws_az","ws_firmware":"4.21"})
            } else if first.contains("/hvac") {
                json!({"data":[{"systemID":1,"zoneID":1,"name":"Test zone","on":1,"roomTemp":22.5,"units":0}]})
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
async fn controller_barrier(port: u16) {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(
                b"GET /test-barrier HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    })
    .await
    .unwrap();
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
        while !hub
            .snapshot()
            .devices
            .iter()
            .any(|device| device.device.id.as_str().ends_with("_s1_z1"))
        {
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
async fn static_gateway_is_listed_moved_with_identity_and_withdrawn_without_losing_observations() {
    let (dir, mut config) = house();
    let calls = Arc::new(AtomicUsize::new(0));
    let (port, writes, stop) =
        controller_with_identity("aa:bb:cc:dd:ee:ff", None, Some(calls.clone())).await;
    config.drivers.push(DriverConfig {
        id: "configured".into(), kind: "airzone".into(), enabled: None,
        options: toml::from_str(&format!("host = '127.0.0.1'\nport = {port}\nmac = 'aabbccddeeff'\nread_only = false\npoll_secs = 5\ndhw_units = 1")).unwrap(),
    });
    let cancel = CancellationToken::new();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, cancel.clone()).unwrap();
    let (tasks, _) = crate::spawn_drivers(&config, &hub, &cancel, &manager).unwrap();
    assert!(
        tasks.is_empty(),
        "the generic spawner must not own Airzone supervisors"
    );
    populated(&hub).await;
    let before: Vec<_> = hub
        .snapshot()
        .devices
        .iter()
        .map(|device| device.device.id.clone())
        .collect();
    let list = manager.list().await.unwrap();
    assert_eq!(list.gateways[0].source, "configuration");
    assert!(
        !list.gateways[0].read_only,
        "unmodified static configuration keeps its native mode"
    );
    assert!(list.gateways[0].running);
    let moved_calls = Arc::new(AtomicUsize::new(0));
    let (moved_port, moved_writes, moved_stop) =
        controller_with_identity("aa:bb:cc:dd:ee:ff", None, Some(moved_calls.clone())).await;
    let mut moved = request(moved_port);
    moved.instance = "configured".into();
    manager
        .probe_update("configured".into(), moved.clone())
        .await
        .unwrap();
    manager
        .update("configured".into(), moved, "person".into())
        .await
        .unwrap();
    let list = manager.list().await.unwrap();
    assert_eq!(list.gateways[0].port, moved_port);
    assert!(list.gateways[0].read_only);
    let state = manager.0.state.lock().await;
    let current = manager.existing(&state, "configured").unwrap();
    assert_eq!(current.config.poll_secs, 5);
    assert_eq!(current.config.dhw_units, Some(1));
    drop(state);
    let retired = manager
        .withdraw("configured".into(), "person".into())
        .await
        .unwrap();
    assert!(!retired.enabled);
    assert!(hub.snapshot().drivers.is_empty());
    assert!(
        hub.snapshot()
            .devices
            .iter()
            .all(|device| device.online == Some(false))
    );
    assert!(before.iter().all(|id| {
        hub.snapshot()
            .devices
            .iter()
            .any(|device| device.device.id == *id)
    }));
    // Joining the supervisors prevents new polls, but the mock may still have
    // an already-written request queued in its listener. Its sequential HTTP
    // barrier drains those requests before measuring the absence of new polls.
    controller_barrier(port).await;
    controller_barrier(moved_port).await;
    let after = calls.load(Ordering::Relaxed);
    let moved_after = moved_calls.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(5100)).await;
    assert_eq!(
        calls.load(Ordering::Relaxed),
        after,
        "withdrawal must stop the former poll loop"
    );
    assert_eq!(
        moved_calls.load(Ordering::Relaxed),
        moved_after,
        "withdrawal must stop the current poll loop"
    );
    let list = manager.list().await.unwrap();
    assert!(!list.gateways[0].enabled && !list.gateways[0].running);
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    assert_eq!(moved_writes.load(Ordering::Relaxed), 0);
    cancel.cancel();
    manager.shutdown().await;
    stop.cancel();
    moved_stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn withdrawal_survives_restart_and_reactivation_requires_the_same_gateway() {
    let (dir, config) = house();
    let (port, writes, stop) = controller().await;
    let cancel = CancellationToken::new();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, cancel.clone()).unwrap();
    manager
        .install(request(port), "person".into())
        .await
        .unwrap();
    populated(&hub).await;
    manager
        .withdraw("airzone-added".into(), "person".into())
        .await
        .unwrap();
    assert_eq!(read(&dir.join(FILE))["gateways"][0]["enabled"], false);
    assert_eq!(
        manager
            .install(request(port), "retry".into())
            .await
            .unwrap_err()
            .code,
        "gateway_disabled"
    );
    let journal_before = hub.journal(20).len();
    manager
        .withdraw("airzone-added".into(), "retry".into())
        .await
        .unwrap();
    assert_eq!(hub.journal(20).len(), journal_before);
    cancel.cancel();
    manager.shutdown().await;
    let config = Config::load(&dir.join("moli.toml")).unwrap();
    let reboot_hub = Hub::new(HubOptions::default()).unwrap();
    let reboot_cancel = CancellationToken::new();
    let reboot = Manager::new(reboot_hub.clone(), &config, reboot_cancel.clone()).unwrap();
    reboot.start_configured().unwrap();
    assert!(reboot_hub.snapshot().drivers.is_empty());
    let (wrong_port, wrong_writes, wrong_stop) =
        controller_with_identity("aa:bb:cc:dd:ee:01", None, None).await;
    let wrong = request(wrong_port);
    assert_eq!(
        reboot
            .probe_update("airzone-added".into(), wrong.clone())
            .await
            .unwrap_err()
            .code,
        "identity_changed"
    );
    assert_eq!(
        reboot
            .update("airzone-added".into(), wrong, "person".into())
            .await
            .unwrap_err()
            .code,
        "identity_changed"
    );
    assert_eq!(read(&dir.join(FILE))["gateways"][0]["enabled"], false);
    reboot
        .update("airzone-added".into(), request(port), "person".into())
        .await
        .unwrap();
    populated(&reboot_hub).await;
    assert_eq!(reboot_hub.snapshot().drivers.len(), 1);
    assert!(reboot.list().await.unwrap().gateways[0].enabled);
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    assert_eq!(wrong_writes.load(Ordering::Relaxed), 0);
    reboot_cancel.cancel();
    reboot.shutdown().await;
    stop.cancel();
    wrong_stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_failed_withdrawal_save_stops_supervision_and_reports_the_unsaved_revocation() {
    let (dir, config) = house();
    let (port, _, stop) = controller().await;
    let cancel = CancellationToken::new();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, cancel.clone()).unwrap();
    manager
        .install(request(port), "person".into())
        .await
        .unwrap();
    std::fs::write(dir.join(FILE), "externally changed").unwrap();
    let (moved_port, _, moved_stop) = controller().await;
    assert_eq!(
        manager
            .update("airzone-added".into(), request(moved_port), "person".into())
            .await
            .unwrap_err()
            .code,
        "persistence_failed"
    );
    assert_eq!(manager.list().await.unwrap().gateways[0].port, port);
    assert!(
        !manager
            .withdraw("airzone-added".into(), "person".into())
            .await
            .unwrap()
            .stored
    );
    assert!(hub.snapshot().drivers.is_empty());
    let status = manager.list().await.unwrap().gateways.remove(0);
    assert!(!status.enabled && !status.running && status.temporary_revocation);
    cancel.cancel();
    manager.shutdown().await;
    stop.cancel();
    moved_stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn updating_is_owned_and_a_lost_response_can_be_replayed_without_the_gateway() {
    let (dir, config) = house();
    let (port, writes, stop) = controller().await;
    let cancel = CancellationToken::new();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, cancel.clone()).unwrap();
    manager
        .install(request(port), "person".into())
        .await
        .unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let (moved_port, moved_writes, moved_stop) = controller_with_identity(
        "aa:bb:cc:dd:ee:ff",
        Some((started.clone(), release.clone())),
        Some(calls.clone()),
    )
    .await;
    let caller = manager.clone();
    let http = tokio::spawn(async move {
        caller
            .update("airzone-added".into(), request(moved_port), "person".into())
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    assert_eq!(
        manager
            .withdraw("airzone-added".into(), "second".into())
            .await
            .unwrap_err()
            .code,
        "busy"
    );
    http.abort();
    assert!(http.await.unwrap_err().is_cancelled());
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if manager
                .list()
                .await
                .is_ok_and(|list| list.gateways[0].port == moved_port)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    moved_stop.cancel();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let before = calls.load(Ordering::Relaxed);
    assert!(
        manager
            .update("airzone-added".into(), request(moved_port), "retry".into())
            .await
            .unwrap()
            .already_installed
    );
    assert_eq!(calls.load(Ordering::Relaxed), before);
    assert_eq!(
        hub.journal(30)
            .iter()
            .filter(|entry| matches!(entry.action, moli_core::Action::DriverConfigured { .. }))
            .count(),
        1
    );
    assert_eq!(hub.snapshot().drivers.len(), 1);
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    assert_eq!(moved_writes.load(Ordering::Relaxed), 0);
    cancel.cancel();
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn replacement_uses_a_new_instance_and_keeps_the_old_identity_retired() {
    let (dir, config) = house();
    let (port, _, stop) = controller().await;
    let cancel = CancellationToken::new();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, cancel.clone()).unwrap();
    manager
        .install(request(port), "person".into())
        .await
        .unwrap();
    populated(&hub).await;
    let old_ids: Vec<_> = hub
        .snapshot()
        .devices
        .iter()
        .map(|device| device.device.id.clone())
        .collect();
    manager
        .withdraw("airzone-added".into(), "person".into())
        .await
        .unwrap();
    let (replacement_port, writes, replacement_stop) =
        controller_with_identity("aa:bb:cc:dd:ee:01", None, None).await;
    let mut replacement = request(replacement_port);
    replacement.instance = "airzone-replacement".into();
    replacement.expected_mac = Some("aabbccddee01".into());
    manager.install(replacement, "person".into()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !hub
            .snapshot()
            .devices
            .iter()
            .any(|device| device.device.instance.as_str() == "airzone-replacement")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(old_ids.iter().all(|id| {
        hub.snapshot()
            .devices
            .iter()
            .any(|device| device.device.id == *id && device.online == Some(false))
    }));
    assert_eq!(manager.list().await.unwrap().gateways.len(), 2);
    assert!(
        manager
            .list()
            .await
            .unwrap()
            .gateways
            .iter()
            .any(|gateway| gateway.instance == "airzone-added" && !gateway.enabled)
    );
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    cancel.cancel();
    manager.shutdown().await;
    stop.cancel();
    replacement_stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
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
    let (tasks, _) =
        crate::spawn_drivers(&reopened, &reboot_hub, &reboot_cancel, &reboot_manager).unwrap();
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
        enabled: true,
    }
}

#[tokio::test]
async fn unreadable_registry_preserves_other_drivers_and_blocks_managed_edits() {
    let (dir, _) = house();
    let path = dir.join("moli.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[[driver]]\nid = 'house-tools'\nkind = 'helpers'\n\n[[driver]]\nid = 'configured'\nkind = 'airzone'\n[driver.options]\nhost = '127.0.0.1'\nmac = 'aabbccddeeff'\n");
    std::fs::write(&path, text).unwrap();
    for raw in ["broken json".to_owned(), " ".repeat(65_537)] {
        std::fs::write(dir.join(FILE), &raw).unwrap();
        let config = Config::load(&path).expect("the house must still load");
        crate::check_config(&config).unwrap();
        assert_eq!(config.drivers[0].enabled, None);
        assert_eq!(config.drivers[1].enabled, Some(false));
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = CancellationToken::new();
        let manager = Manager::new(hub.clone(), &config, cancel.clone()).unwrap();
        let (tasks, _) = crate::spawn_drivers(&config, &hub, &cancel, &manager).unwrap();
        assert_eq!(hub.snapshot().drivers.len(), 1);
        assert_eq!(hub.snapshot().drivers[0].instance.as_str(), "house-tools");
        assert_eq!(
            manager
                .install(request(3000), "person".into())
                .await
                .unwrap_err()
                .code,
            "persistence_failed"
        );
        assert!(
            !manager
                .withdraw("configured".into(), "person".into())
                .await
                .unwrap()
                .stored
        );
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), raw);
        cancel.cancel();
        manager.shutdown().await;
        for task in tasks {
            task.await.unwrap();
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn unidentified_override_pauses_managed_and_static_airzone_without_changing_registry() {
    let (dir, _) = house();
    let path = dir.join("moli.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[[driver]]\nid = 'house-tools'\nkind = 'helpers'\n\n[[driver]]\nid = 'configured'\nkind = 'airzone'\n[driver.options]\nhost = '127.0.0.2'\nmac = 'aabbccddee01'\n");
    std::fs::write(&path, text).unwrap();
    for entry in [
        json!({"enabled":false}),
        json!({"instance":false,"enabled":false}),
        json!({"instance":"","enabled":false}),
    ] {
        let raw =
            json!({"version":2,"gateways":[stored_gateway("managed",3000)],"overrides":[entry]})
                .to_string();
        std::fs::write(dir.join(FILE), &raw).unwrap();
        let config = Config::load(&path).unwrap();
        crate::check_config(&config).unwrap();
        assert_eq!(config.drivers.len(), 3);
        assert!(
            config
                .drivers
                .iter()
                .filter(|driver| driver.kind == "airzone")
                .all(|driver| driver.enabled == Some(false))
        );
        let hub = Hub::new(HubOptions::default()).unwrap();
        let cancel = CancellationToken::new();
        let manager = Manager::new(hub.clone(), &config, cancel.clone()).unwrap();
        let (tasks, _) = crate::spawn_drivers(&config, &hub, &cancel, &manager).unwrap();
        assert_eq!(hub.snapshot().drivers.len(), 1);
        assert_eq!(hub.snapshot().drivers[0].instance.as_str(), "house-tools");
        let list = manager.list().await.unwrap();
        assert_eq!(list.gateways.len(), 2);
        assert!(
            list.gateways
                .iter()
                .all(|gateway| !gateway.enabled && !gateway.running)
        );
        assert_eq!(
            manager
                .install(request(3000), "person".into())
                .await
                .unwrap_err()
                .code,
            "persistence_failed"
        );
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), raw);
        cancel.cancel();
        manager.shutdown().await;
        for task in tasks {
            task.await.unwrap();
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn partial_registry_skips_bad_entries_without_reviving_a_withdrawn_driver() {
    let (dir, _) = house();
    let path = dir.join("moli.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[[driver]]\nid = 'configured'\nkind = 'airzone'\n[driver.options]\nhost = '127.0.0.2'\nmac = 'aabbccddee01'\n\n[[driver]]\nid = 'healthy-static'\nkind = 'airzone'\n[driver.options]\nhost = '127.0.0.3'\nmac = 'aabbccddee02'\n");
    std::fs::write(&path, text).unwrap();
    let raw = json!({"version":2,"gateways":[
        stored_gateway("healthy-managed", 3000),
        {"instance":"broken-managed", "host":false, "enabled":false}
    ],"overrides":[
        {"instance":"removed-from-toml", "enabled":false},
        {"instance":"configured", "enabled":false, "mac":"invalid"}
    ]})
    .to_string();
    std::fs::write(dir.join(FILE), &raw).unwrap();
    let config = Config::load(&path).expect("valid entries must survive");
    crate::check_config(&config).unwrap();
    assert_eq!(config.drivers.len(), 3);
    assert_eq!(config.drivers[0].enabled, Some(false));
    assert_eq!(config.drivers[1].enabled, None);
    assert_eq!(config.drivers[2].id, "healthy-managed");
    let manager = Manager::new(
        Hub::new(HubOptions::default()).unwrap(),
        &config,
        CancellationToken::new(),
    )
    .unwrap();
    let list = manager.list().await.unwrap();
    assert_eq!(list.gateways.len(), 3);
    assert!(
        !list
            .gateways
            .iter()
            .find(|g| g.instance == "configured")
            .unwrap()
            .enabled
    );
    assert!(
        list.gateways
            .iter()
            .find(|g| g.instance == "healthy-static")
            .unwrap()
            .enabled
    );
    assert!(
        list.gateways
            .iter()
            .find(|g| g.instance == "healthy-managed")
            .unwrap()
            .enabled
    );
    assert!(
        !manager
            .withdraw("healthy-managed".into(), "person".into())
            .await
            .unwrap()
            .stored
    );
    assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), raw);
    manager.shutdown().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn ambiguous_registry_entries_never_start_duplicate_or_withdrawn_gateways() {
    let (dir, _) = house();
    let mut disabled = stored_gateway("ambiguous", 3000);
    disabled.enabled = false;
    let mut healthy = stored_gateway("healthy", 4000);
    healthy.mac = "aabbccddee01".into();
    let raw = json!({"version":2,"gateways":[
        stored_gateway("ambiguous", 3000), disabled,
        stored_gateway("same-controller", 3001), healthy
    ]})
    .to_string();
    std::fs::write(dir.join(FILE), &raw).unwrap();
    let config = Config::load(&dir.join("moli.toml")).unwrap();
    assert_eq!(config.drivers.len(), 1);
    assert_eq!(config.drivers[0].id, "healthy");
    let manager = Manager::new(
        Hub::new(HubOptions::default()).unwrap(),
        &config,
        CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(manager.list().await.unwrap().gateways.len(), 1);
    assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), raw);
    manager.shutdown().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn conflicting_registry_entry_cannot_claim_an_identical_toml_driver() {
    for enabled in [true, false] {
        let (dir, _) = house();
        let path = dir.join("moli.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        write!(text, "\n[[driver]]\nid = 'configured'\nkind = 'airzone'\nenabled = {enabled}\n[driver.options]\nhost = '127.0.0.1'\nport = 3000\nmac = 'aabbccddeeff'\npoll_secs = 30\nread_only = true\n").unwrap();
        std::fs::write(&path, text).unwrap();
        let mut saved = stored_gateway("configured", 3000);
        saved.enabled = enabled;
        let raw = json!({"version":2,"gateways":[saved]}).to_string();
        std::fs::write(dir.join(FILE), &raw).unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.drivers.len(), 1);
        let manager = Manager::new(
            Hub::new(HubOptions::default()).unwrap(),
            &config,
            CancellationToken::new(),
        )
        .unwrap();
        let list = manager.list().await.unwrap();
        assert_eq!(list.gateways.len(), 1);
        assert_eq!(list.gateways[0].source, "configuration");
        assert_eq!(list.gateways[0].enabled, enabled);
        assert!(
            !manager
                .withdraw("configured".into(), "person".into())
                .await
                .unwrap()
                .stored
        );
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), raw);
        manager.shutdown().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn registry_removed_between_load_and_serve_pauses_airzone_without_stopping_the_house() {
    let (dir, _) = house();
    std::fs::write(
        dir.join(FILE),
        json!({"version":2,"gateways":[stored_gateway("saved", 3000)]}).to_string(),
    )
    .unwrap();
    let config = Config::load(&dir.join("moli.toml")).unwrap();
    std::fs::remove_file(dir.join(FILE)).unwrap();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
    manager.start_configured().unwrap();
    assert!(hub.snapshot().drivers.is_empty());
    assert!(
        manager
            .list()
            .await
            .unwrap()
            .gateways
            .iter()
            .all(|gateway| !gateway.enabled && !gateway.running)
    );
    assert_eq!(
        manager
            .install(request(3000), "person".into())
            .await
            .unwrap_err()
            .code,
        "persistence_failed"
    );
    assert!(!dir.join(FILE).exists());
    manager.shutdown().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_starter_loads_saved_gateways_and_recovery_can_reach_a_corrupt_registry() {
    let (dir, _) = house();
    let store = Store {
        version: 1,
        gateways: vec![stored_gateway("airzone-kept", 3000)],
        overrides: Vec::new(),
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
    assert!(
        Config::load(&dir.join("moli.toml"))
            .unwrap()
            .drivers
            .is_empty()
    );
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

#[tokio::test]
async fn a_registry_changed_after_config_load_does_not_announce_an_inactive_gateway() {
    let (dir, config) = house();
    std::fs::write(
        dir.join(FILE),
        serde_json::to_vec(&Store {
            version: 1,
            gateways: vec![stored_gateway("late", 3000)],
            overrides: Vec::new(),
        })
        .unwrap(),
    )
    .unwrap();
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub, &config, CancellationToken::new()).unwrap();
    assert!(manager.list().await.unwrap().gateways.is_empty());
    assert_eq!(
        manager
            .install(request(3000), "person".into())
            .await
            .unwrap_err()
            .code,
        "persistence_failed"
    );
    manager.shutdown().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn recovery_can_load_base_config_without_discarding_a_broken_registry() {
    let (dir, _) = house();
    std::fs::write(dir.join(FILE), "broken json").unwrap();
    assert!(
        Config::load(&dir.join("moli.toml"))
            .unwrap()
            .drivers
            .is_empty()
    );
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
    changed.expected_mac = Some("aabbccddee01".into());
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
    future["version"] = json!(4);
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
        assert!(
            Config::load(&dir.join("moli.toml"))
                .unwrap()
                .drivers
                .is_empty()
        );
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), invalid);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn loopback_connections_are_rejected_by_the_production_validation_policy() {
    for host in ["127.0.0.1", "127.0.0.2", "127.255.255.254"] {
        let mut local = request(3000);
        local.host = host.into();
        assert_eq!(
            validate_connection(local.clone(), false).unwrap_err().code,
            "invalid_host"
        );
        assert!(
            validate_connection(local, true).is_ok(),
            "unit-test fixture only"
        );
    }
    let mut private = request(3000);
    private.host = "192.168.1.2".into();
    assert!(validate_connection(private, false).is_ok());
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
