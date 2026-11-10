use super::*;
use moli_core::{DeviceId, Origin, PointId};
use moli_runtime::CommandError;

const INSTANCE: &str = "airzone-added";

fn mode(read_only: bool, expected_read_only: bool) -> GatewayModeRequest {
    GatewayModeRequest {
        read_only,
        expected_read_only,
    }
}

fn static_house(port: u16, read_only: bool, enabled: bool, identity: bool) -> (PathBuf, Config) {
    let (dir, _) = house();
    let path = dir.join("moli.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    write!(text, "\n[[driver]]\nid = '{INSTANCE}'\nkind = 'airzone'\nenabled = {enabled}\n[driver.options]\nhost = '127.0.0.1'\nport = {port}\npoll_secs = 67\ndhw_units = 1\nread_only = {read_only}\n").unwrap();
    if identity {
        text.push_str("mac = 'aabbccddeeff'\n");
    }
    std::fs::write(&path, text).unwrap();
    let config = Config::load(&path).unwrap();
    (dir, config)
}

// A sidecar revocation is reversible; an explicit static read-only lock is not.
fn managed_read_only_house(port: u16) -> (PathBuf, Config) {
    let (dir, _) = static_house(port, false, true, true);
    std::fs::write(dir.join(FILE), serde_json::to_vec(&json!({"version":3,"gateways":[],"overrides":[{"instance":INSTANCE,"enabled":true,"read_only":true,"mac":"aabbccddeeff"}]})).unwrap()).unwrap();
    let config = Config::load(&dir.join("moli.toml")).unwrap();
    (dir, config)
}

fn zone() -> DeviceId {
    DeviceId::from("airzone-added:aabbccddeeff_s1_z1")
}

async fn writable(hub: &Hub, expected: bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if hub.device(&zone()).is_some_and(|device| {
                device
                    .device
                    .points
                    .iter()
                    .any(|point| point.key.as_ref() == "on" && point.access.write == expected)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn command(hub: &Hub) -> Result<(), CommandError> {
    hub.command(
        &PointId::new(&zone(), "on"),
        Value::Bool(false),
        Origin::Ui,
        None,
    )
    .await
}

async fn rejects_extended_name(hub: &Hub) {
    assert!(
        !hub.device(&zone())
            .unwrap()
            .device
            .point("name")
            .unwrap()
            .access
            .write
    );
    assert_eq!(
        hub.command(
            &PointId::new(&zone(), "name"),
            Value::from("Renamed"),
            Origin::Ui,
            None
        )
        .await,
        Err(CommandError::ReadOnly),
        "authorizing or restoring control must not opt in to thermostat renaming"
    );
}

#[tokio::test]
async fn explicit_mode_round_trip_persists_and_only_authorized_mock_commands_write() {
    for configured in [false, true] {
        let (port, writes, stop) = controller().await;
        let (dir, config) = if configured {
            managed_read_only_house(port)
        } else {
            house()
        };
        let before = std::fs::read(dir.join("moli.toml")).unwrap();
        let hub = Hub::new(HubOptions::default()).unwrap();
        let manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
        if configured {
            manager.start_configured().unwrap();
        } else {
            manager
                .install(request(port), "person".into())
                .await
                .unwrap();
        }
        writable(&hub, false).await;
        assert_eq!(command(&hub).await, Err(CommandError::ReadOnly));
        let allowed = manager
            .set_mode(INSTANCE.into(), mode(false, true), "person".into())
            .await
            .unwrap();
        assert!(allowed.stored && allowed.running && !allowed.read_only);
        writable(&hub, true).await;
        rejects_extended_name(&hub).await;
        assert_eq!(
            writes.load(Ordering::Relaxed),
            0,
            "a mode change is not an equipment command"
        );
        assert_eq!(
            manager
                .set_mode(INSTANCE.into(), mode(false, true), "stale".into())
                .await
                .unwrap_err()
                .code,
            "mode_conflict"
        );
        command(&hub).await.unwrap();
        assert_eq!(writes.load(Ordering::Relaxed), 1);
        assert_eq!(std::fs::read(dir.join("moli.toml")).unwrap(), before);
        assert_eq!(read(&dir.join(FILE))["version"], 3);
        manager.shutdown().await;

        let reopened = Config::load(&dir.join("moli.toml")).unwrap();
        let options = &reopened.drivers[0].options;
        assert_eq!(options["read_only"].as_bool(), Some(false));
        if configured {
            assert_eq!(options["poll_secs"].as_integer(), Some(67));
            assert_eq!(options["dhw_units"].as_integer(), Some(1));
        }
        let hub = Hub::new(HubOptions::default()).unwrap();
        let reboot = Manager::new(hub.clone(), &reopened, CancellationToken::new()).unwrap();
        reboot.start_configured().unwrap();
        writable(&hub, true).await;
        rejects_extended_name(&hub).await;
        let readonly = reboot
            .set_mode(INSTANCE.into(), mode(true, false), "person".into())
            .await
            .unwrap();
        assert!(readonly.stored && readonly.running && readonly.read_only);
        assert!(
            hub.snapshot()
                .devices
                .iter()
                .flat_map(|device| &device.device.points)
                .all(|point| !point.access.write)
        );
        assert_eq!(command(&hub).await, Err(CommandError::ReadOnly));
        let before_replay = hub.journal(100).len();
        assert!(
            reboot
                .set_mode(INSTANCE.into(), mode(true, true), "retry".into())
                .await
                .unwrap()
                .read_only
        );
        assert_eq!(
            hub.journal(100).len(),
            before_replay,
            "a no-op is not a new mode change"
        );
        assert_eq!(writes.load(Ordering::Relaxed), 1);
        reboot.shutdown().await;
        assert_eq!(
            Config::load(&dir.join("moli.toml")).unwrap().drivers[0].options["read_only"].as_bool(),
            Some(true)
        );
        assert_eq!(std::fs::read(dir.join("moli.toml")).unwrap(), before);
        stop.cancel();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn read_only_revocation_works_offline_and_clears_retained_writable_points() {
    let (port, writes, stop) = controller().await;
    let (dir, config) = static_house(port, false, true, true);
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
    manager.start_configured().unwrap();
    writable(&hub, true).await;
    let before = hub.device(&zone()).unwrap().state;
    controller_barrier(port).await;
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(1), async {
        while tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // No subsequent network request is needed to revoke authority.
    let result = manager
        .set_mode(INSTANCE.into(), mode(true, false), "person".into())
        .await
        .unwrap();
    assert!(result.read_only && result.running);
    let device = hub.device(&zone()).unwrap();
    assert_eq!(device.online, Some(false));
    assert_eq!(device.state, before);
    assert!(device.device.points.iter().all(|point| !point.access.write));
    assert_eq!(command(&hub).await, Err(CommandError::ReadOnly));
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    manager.shutdown().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_paused_or_unknown_identity_gateway_can_be_revoked_without_network_access() {
    for enabled in [false, true] {
        let (dir, config) = static_house(9, false, enabled, false);
        let manager = Manager::new(
            Hub::new(HubOptions::default()).unwrap(),
            &config,
            CancellationToken::new(),
        )
        .unwrap();
        let result = manager
            .set_mode(INSTANCE.into(), mode(true, false), "person".into())
            .await
            .unwrap();
        assert_eq!(result.running, enabled);
        assert!(result.read_only);
        let saved = Config::load(&dir.join("moli.toml")).unwrap();
        assert_eq!(saved.drivers[0].enabled, Some(enabled));
        assert_eq!(saved.drivers[0].options["read_only"].as_bool(), Some(true));
        let error = manager
            .set_mode(INSTANCE.into(), mode(false, true), "person".into())
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            if enabled {
                "identity_unavailable"
            } else {
                "configuration_locked"
            }
        );
        manager.shutdown().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn enabling_refuses_identity_changes_and_never_writes_or_saves_them() {
    let (port, writes, stop) = controller_with_identity("aa:bb:cc:dd:ee:01", None, None).await;
    let (dir, config) = managed_read_only_house(port);
    let registry_before = std::fs::read(dir.join(FILE)).unwrap();
    let manager = Manager::new(
        Hub::new(HubOptions::default()).unwrap(),
        &config,
        CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        manager
            .set_mode(INSTANCE.into(), mode(false, true), "person".into())
            .await
            .unwrap_err()
            .code,
        "identity_changed"
    );
    assert!(manager.list().await.unwrap().gateways[0].read_only);
    assert_eq!(std::fs::read(dir.join(FILE)).unwrap(), registry_before);
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn failed_authorization_persistence_never_grants_control() {
    let (port, writes, stop) = controller().await;
    let (dir, config) = managed_read_only_house(port);
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
    manager.start_configured().unwrap();
    writable(&hub, false).await;
    std::fs::write(dir.join(FILE), "external edit").unwrap();
    assert_eq!(
        manager
            .set_mode(INSTANCE.into(), mode(false, true), "person".into())
            .await
            .unwrap_err()
            .code,
        "persistence_failed"
    );
    let actual = manager.list().await.unwrap().gateways.remove(0);
    assert!(actual.read_only && actual.running);
    writable(&hub, false).await;
    assert_eq!(
        std::fs::read_to_string(dir.join(FILE)).unwrap(),
        "external edit"
    );
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_mode_change_finishes_when_its_http_caller_disappears_and_serializes_other_edits() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (port, writes, stop) = controller_with_gate(Some((started.clone(), release.clone()))).await;
    let (dir, config) = managed_read_only_house(port);
    let manager = Manager::new(
        Hub::new(HubOptions::default()).unwrap(),
        &config,
        CancellationToken::new(),
    )
    .unwrap();
    let caller = manager.clone();
    let http = tokio::spawn(async move {
        caller
            .set_mode(INSTANCE.into(), mode(false, true), "person".into())
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    assert_eq!(
        manager
            .withdraw(INSTANCE.into(), "other".into())
            .await
            .unwrap_err()
            .code,
        "busy"
    );
    assert_eq!(
        manager
            .set_mode(INSTANCE.into(), mode(false, true), "other".into())
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
                .is_ok_and(|list| !list.gateways[0].read_only)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    manager.shutdown().await;
    assert_eq!(
        Config::load(&dir.join("moli.toml")).unwrap().drivers[0].options["read_only"].as_bool(),
        Some(false)
    );
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn editing_an_authorized_connection_revokes_even_when_the_address_is_unchanged() {
    for configured in [false, true] {
        let (port, writes, stop) = controller().await;
        let (dir, config) = if configured {
            managed_read_only_house(port)
        } else {
            house()
        };
        let manager = Manager::new(
            Hub::new(HubOptions::default()).unwrap(),
            &config,
            CancellationToken::new(),
        )
        .unwrap();
        if configured {
            manager.start_configured().unwrap();
        } else {
            manager
                .install(request(port), "person".into())
                .await
                .unwrap();
        }
        manager
            .update(INSTANCE.into(), request(port), "person".into())
            .await
            .unwrap();
        manager
            .set_mode(INSTANCE.into(), mode(false, true), "person".into())
            .await
            .unwrap();
        assert_eq!(
            Config::load(&dir.join("moli.toml")).unwrap().drivers[0].options["read_only"].as_bool(),
            Some(false)
        );
        if !configured {
            assert!(
                !manager
                    .install(request(port), "retry".into())
                    .await
                    .unwrap()
                    .read_only,
                "install replay reports the actual mode"
            );
        }
        let edited = manager
            .update(INSTANCE.into(), request(port), "person".into())
            .await
            .unwrap();
        assert!(edited.read_only && !edited.already_installed);
        assert!(manager.list().await.unwrap().gateways[0].read_only);
        assert_eq!(
            Config::load(&dir.join("moli.toml")).unwrap().drivers[0].options["read_only"].as_bool(),
            Some(true)
        );
        assert_eq!(writes.load(Ordering::Relaxed), 0);
        manager.shutdown().await;
        stop.cancel();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn legacy_invalid_writable_entries_cannot_gain_authority_after_an_upgrade() {
    let (dir, _) = house();
    let mut gateway = stored_gateway(INSTANCE, 3000);
    gateway.read_only = false;
    for version in [1, 2, 3] {
        let raw = json!({"version":version,"gateways":[gateway]}).to_string();
        std::fs::write(dir.join(FILE), &raw).unwrap();
        let config = Config::load(&dir.join("moli.toml")).unwrap();
        assert_eq!(config.drivers.len(), usize::from(version == 3));
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), raw);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn writable_static_overrides_require_version_three_and_a_pinned_identity() {
    let (dir, _) = static_house(3000, false, true, true);
    for (version, mac, valid) in [
        (2, Some("aabbccddeeff"), false),
        (3, None, false),
        (3, Some("aabbccddeeff"), true),
    ] {
        let raw = json!({"version":version,"gateways":[],"overrides":[{"instance":INSTANCE,"enabled":true,"read_only":false,"mac":mac}]}).to_string();
        std::fs::write(dir.join(FILE), &raw).unwrap();
        let config = Config::load(&dir.join("moli.toml")).unwrap();
        assert_eq!(config.drivers[0].enabled, Some(valid));
        if valid {
            assert_eq!(
                config.drivers[0].options["read_only"].as_bool(),
                Some(false)
            );
        }
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), raw);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn disabled_incomplete_static_entries_never_block_startup_or_reactivate_from_registry() {
    for options in ["", "host = '127.0.0.1'\nunknown_option = true\n"] {
        let (dir, _) = house();
        let path = dir.join("moli.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        write!(text, "\n[[driver]]\nid = '{INSTANCE}'\nkind = 'airzone'\nenabled = false\n[driver.options]\n{options}").unwrap();
        std::fs::write(&path, text).unwrap();
        std::fs::write(dir.join(FILE), serde_json::to_vec(&json!({"version":3,"gateways":[],"overrides":[{"instance":INSTANCE,"enabled":true,"read_only":false,"mac":"aabbccddeeff"}]})).unwrap()).unwrap();
        let config = Config::load(&path).unwrap();
        let manager = Manager::new(
            Hub::new(HubOptions::default()).unwrap(),
            &config,
            CancellationToken::new(),
        )
        .expect("disabled invalid options are ignored");
        manager.start_configured().unwrap();
        assert!(
            manager
                .list()
                .await
                .unwrap()
                .gateways
                .iter()
                .all(|gateway| !gateway.enabled && !gateway.running)
        );
        manager.shutdown().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn explicit_static_restrictions_dominate_persisted_and_live_authorizations() {
    for (enabled, read_only) in [(false, false), (true, true)] {
        let (dir, _) = static_house(3000, read_only, enabled, true);
        std::fs::write(dir.join(FILE), serde_json::to_vec(&json!({"version":3,"gateways":[],"overrides":[{"instance":INSTANCE,"enabled":true,"read_only":false,"mac":"aabbccddeeff"}]})).unwrap()).unwrap();
        let config = Config::load(&dir.join("moli.toml")).unwrap();
        let entry = config
            .drivers
            .iter()
            .find(|driver| driver.id == INSTANCE)
            .unwrap();
        assert_eq!(entry.enabled != Some(false), enabled);
        assert_eq!(entry.options["read_only"].as_bool(), Some(read_only));
        let manager = Manager::new(
            Hub::new(HubOptions::default()).unwrap(),
            &config,
            CancellationToken::new(),
        )
        .unwrap();
        let listed = manager.list().await.unwrap();
        let status = serde_json::to_value(&listed.gateways[0]).unwrap();
        assert_eq!(
            status["configuration_lock"],
            if enabled { "read_only" } else { "disabled" }
        );
        assert_eq!(
            manager
                .set_mode(INSTANCE.into(), mode(false, read_only), "person".into())
                .await
                .unwrap_err()
                .code,
            "configuration_locked"
        );
        manager.shutdown().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[tokio::test]
async fn failed_persistence_revokes_in_memory_and_cannot_reactivate_through_other_edits() {
    let (port, writes, stop) = controller().await;
    let (dir, config) = static_house(port, false, true, true);
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
    manager.start_configured().unwrap();
    writable(&hub, true).await;
    std::fs::write(dir.join(FILE), b"external edit").unwrap();
    for expected in [false, true] {
        let revoked = manager
            .set_mode(INSTANCE.into(), mode(true, expected), "person".into())
            .await
            .unwrap();
        assert!(revoked.read_only && !revoked.stored);
        writable(&hub, false).await;
        assert_eq!(command(&hub).await, Err(CommandError::ReadOnly));
        let status = serde_json::to_value(&manager.list().await.unwrap().gateways[0]).unwrap();
        assert_eq!(status["temporary_revocation"], true);
        assert_eq!(
            manager
                .set_mode(INSTANCE.into(), mode(false, true), "person".into())
                .await
                .unwrap_err()
                .code,
            "revocation_not_persisted"
        );
        assert_eq!(
            manager
                .update(INSTANCE.into(), request(port), "person".into())
                .await
                .unwrap_err()
                .code,
            "revocation_not_persisted"
        );
    }
    for _ in 0..2 {
        let withdrawn = manager
            .withdraw(INSTANCE.into(), "person".into())
            .await
            .unwrap();
        assert!(!withdrawn.stored && !withdrawn.enabled);
        let status = manager.list().await.unwrap().gateways.remove(0);
        assert!(!status.enabled && !status.running);
    }
    let revoked = manager
        .set_mode(INSTANCE.into(), mode(true, true), "person".into())
        .await
        .unwrap();
    assert!(!revoked.stored && !revoked.running);
    assert!(!manager.list().await.unwrap().gateways[0].enabled);
    assert_eq!(std::fs::read(dir.join(FILE)).unwrap(), b"external edit");
    // Repair only this synthetic file, then save the stronger withdrawal.
    std::fs::remove_file(dir.join(FILE)).unwrap();
    let durable = manager
        .set_mode(INSTANCE.into(), mode(true, true), "person".into())
        .await
        .unwrap();
    assert!(durable.stored && !durable.running);
    let status = serde_json::to_value(&manager.list().await.unwrap().gateways[0]).unwrap();
    assert_eq!(status["temporary_revocation"], false);
    assert_eq!(status["enabled"], false);
    assert_eq!(read(&dir.join(FILE))["overrides"][0]["enabled"], false);
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn degraded_registry_still_allows_read_only_revocation_and_withdrawal() {
    for withdrawal in [false, true] {
        let (dir, _) = house();
        let (port, writes, stop) = controller().await;
        let mut gateway = stored_gateway(INSTANCE, port);
        gateway.read_only = false;
        let raw = json!({"version":3,"gateways":[gateway,{"instance":"broken","host":"invalid"}]})
            .to_string();
        std::fs::write(dir.join(FILE), &raw).unwrap();
        let config = Config::load(&dir.join("moli.toml")).unwrap();
        let hub = Hub::new(HubOptions::default()).unwrap();
        let manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
        manager.start_configured().unwrap();
        writable(&hub, true).await;
        if withdrawal {
            assert!(
                !manager
                    .withdraw(INSTANCE.into(), "person".into())
                    .await
                    .unwrap()
                    .stored
            );
        } else {
            assert!(
                !manager
                    .set_mode(INSTANCE.into(), mode(true, false), "person".into())
                    .await
                    .unwrap()
                    .stored
            );
        }
        assert!(
            hub.device(&zone())
                .unwrap()
                .device
                .points
                .iter()
                .all(|point| !point.access.write)
        );
        assert_eq!(command(&hub).await, Err(CommandError::ReadOnly));
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), raw);
        assert_eq!(writes.load(Ordering::Relaxed), 0);
        manager.shutdown().await;
        stop.cancel();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[derive(Debug)]
struct WaitingCommands(moli_core::Device);
impl moli_runtime::Driver for WaitingCommands {
    fn kind(&self) -> &'static str {
        "airzone"
    }
    fn run<'a>(
        &'a self,
        ctx: &'a mut moli_runtime::DriverCtx,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            ctx.upsert_device(self.0.clone());
            ctx.set_availability(&self.0.id, true);
            ctx.ready();
            std::future::pending().await
        })
    }
}

#[tokio::test]
async fn volatile_revocation_rejects_already_queued_commands_before_acknowledging() {
    let (port, writes, stop) = controller().await;
    let (dir, config) = static_house(port, false, true, true);
    let hub = Hub::new(HubOptions::default()).unwrap();
    let manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
    manager.start_configured().unwrap();
    writable(&hub, true).await;
    let definition = hub.device(&zone()).unwrap().device.as_ref().clone();
    manager.stop(INSTANCE).await;
    // Hold a synthetic supervisor before it consumes the queue. This makes
    // cancellation/draining observable without any controller command.
    let cancel = CancellationToken::new();
    let task = spawn_driver(
        &hub,
        INSTANCE.into(),
        Arc::new(WaitingCommands(definition)),
        cancel.clone(),
    );
    manager
        .0
        .drivers
        .lock()
        .unwrap()
        .insert(INSTANCE.into(), Running { cancel, task });
    writable(&hub, true).await;
    let pending = command(&hub);
    tokio::pin!(pending);
    tokio::select! {
        biased;
        result = &mut pending => panic!("the command should be queued: {result:?}"),
        () = tokio::task::yield_now() => {}
    }
    std::fs::write(dir.join(FILE), b"external edit").unwrap();
    let revoked = manager
        .set_mode(INSTANCE.into(), mode(true, false), "person".into())
        .await
        .unwrap();
    assert!(!revoked.stored && revoked.read_only);
    assert!(matches!(
        pending.await,
        Err(CommandError::Driver(_) | CommandError::DriverUnavailable)
    ));
    assert_eq!(command(&hub).await, Err(CommandError::ReadOnly));
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}

fn fail_directory_sync(_: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("synthetic directory sync failure"))
}

#[tokio::test]
async fn unsynced_revocation_is_honest_and_retry_tracks_the_already_renamed_registry() {
    for withdrawal in [false, true] {
        let (port, writes, stop) = controller().await;
        let (dir, config) = static_house(port, false, true, true);
        let hub = Hub::new(HubOptions::default()).unwrap();
        let mut manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
        Arc::get_mut(&mut manager.0).unwrap().sync_directory = fail_directory_sync;
        manager.start_configured().unwrap();
        writable(&hub, true).await;
        for expected in [false, true] {
            if withdrawal {
                let result = manager
                    .commit_withdraw(INSTANCE.into(), "person".into())
                    .await
                    .unwrap();
                assert!(!result.stored && !result.enabled && result.read_only);
            } else {
                let result = manager
                    .commit_mode(INSTANCE.into(), mode(true, expected), "person".into())
                    .await
                    .unwrap();
                assert!(!result.stored && result.read_only);
            }
            assert_eq!(command(&hub).await, Err(CommandError::ReadOnly));
            let status = manager.list().await.unwrap().gateways.remove(0);
            assert!(status.temporary_revocation && status.read_only);
            assert!(
                manager
                    .persisted(&*manager.0.state.lock().await)
                    .await
                    .is_err()
            );
        }
        // The rename already occurred; a retry must compare against these bytes,
        // not the original absent registry, and require successful directory sync.
        assert!(dir.join(FILE).is_file());
        Arc::get_mut(&mut manager.0).unwrap().sync_directory = persistence::sync_directory;
        let result = manager
            .commit_mode(INSTANCE.into(), mode(true, true), "person".into())
            .await
            .unwrap();
        assert!(result.stored && result.read_only);
        let status = manager.list().await.unwrap().gateways.remove(0);
        assert!(!status.temporary_revocation);
        assert_eq!(status.enabled, !withdrawal);
        assert_eq!(
            read(&dir.join(FILE))["overrides"][0]["enabled"],
            !withdrawal
        );
        assert!(
            manager
                .persisted(&*manager.0.state.lock().await)
                .await
                .is_ok()
        );
        assert_eq!(writes.load(Ordering::Relaxed), 0);
        manager.shutdown().await;
        stop.cancel();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn unsynced_authorization_keeps_read_only_and_warns_until_durable_revocation() {
    let (port, writes, stop) = controller().await;
    let (dir, config) = managed_read_only_house(port);
    let hub = Hub::new(HubOptions::default()).unwrap();
    let mut manager = Manager::new(hub.clone(), &config, CancellationToken::new()).unwrap();
    Arc::get_mut(&mut manager.0).unwrap().sync_directory = fail_directory_sync;
    manager.start_configured().unwrap();
    writable(&hub, false).await;
    assert_eq!(
        manager
            .commit_mode(INSTANCE.into(), mode(false, true), "person".into())
            .await
            .unwrap_err()
            .code,
        "persistence_failed"
    );
    assert_eq!(command(&hub).await, Err(CommandError::ReadOnly));
    let status = manager.list().await.unwrap().gateways.remove(0);
    assert!(status.read_only && status.temporary_revocation);
    // An uncertain grant on disk must not enable an idempotent replay in memory.
    assert_eq!(read(&dir.join(FILE))["overrides"][0]["read_only"], false);
    assert_eq!(
        manager
            .commit_mode(INSTANCE.into(), mode(false, true), "person".into())
            .await
            .unwrap_err()
            .code,
        "revocation_not_persisted"
    );
    Arc::get_mut(&mut manager.0).unwrap().sync_directory = persistence::sync_directory;
    let revoked = manager
        .commit_mode(INSTANCE.into(), mode(true, true), "person".into())
        .await
        .unwrap();
    assert!(revoked.stored && revoked.read_only);
    let status = manager.list().await.unwrap().gateways.remove(0);
    assert!(status.read_only && !status.temporary_revocation);
    assert_eq!(read(&dir.join(FILE))["overrides"][0]["read_only"], true);
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    manager.shutdown().await;
    stop.cancel();
    std::fs::remove_dir_all(dir).unwrap();
}
