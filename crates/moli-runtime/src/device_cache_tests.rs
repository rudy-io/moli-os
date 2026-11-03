//! A dormant driver must not make its known devices disappear or commandable.
use std::sync::atomic::{AtomicU64, Ordering};

use moli_core::{
    Access, Device, DeviceId, DriverStatus, InstanceId, Kind, Origin, PointId, PointSpec, Semantic,
    Value,
};

use crate::{CommandError, Hub, HubOptions};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "moli-definitions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn options(&self) -> HubOptions {
        HubOptions {
            state_path: Some(self.0.join("state.json")),
            ..HubOptions::default()
        }
    }
    fn hub(&self) -> Hub {
        Hub::new(self.options()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn instance() -> InstanceId {
    "synthetic".into()
}
fn id() -> DeviceId {
    "synthetic:device".into()
}
fn definition() -> Device {
    Device {
        id: id(),
        instance: instance(),
        native_name: "Synthetic device".into(),
        manufacturer: None,
        model: None,
        description: None,
        native_room: None,
        members: vec![],
        points: vec![PointSpec {
            key: "on".into(),
            label: "On".into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: Semantic::OnOff,
        }],
    }
}
fn register(hub: &Hub) -> tokio::sync::mpsc::Receiver<crate::CommandRequest> {
    hub.register_driver(&instance(), "synthetic", "synthetic-package".into())
}
async fn save_fixture(f: &Fixture) {
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", Value::Bool(true));
    assert!(hub.persist_state().await.unwrap());
}

#[tokio::test]
async fn reboot_restores_definition_offline_with_its_original_samples() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let before: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.0.join("state.json")).unwrap()).unwrap();
    let hub = f.hub();
    assert!(
        hub.device(&id()).is_none(),
        "disabled instances stay absent"
    );
    let _commands = register(&hub);
    let known = hub
        .device(&id())
        .expect("an unavailable driver retains its inventory after reboot");
    assert_eq!(known.online, Some(false));
    assert!(!known.device.points[0].access.write);
    assert_eq!(known.state["on"].value, Value::Bool(true));
    assert_eq!(
        known.state["on"].ts,
        before["synthetic:device"]["on"]["ts"].as_u64().unwrap()
    );
    assert_eq!(
        hub.command(
            &PointId::new(&id(), "on"),
            Value::Bool(false),
            Origin::Ui,
            None
        )
        .await,
        Err(CommandError::ReadOnly)
    );
}

#[tokio::test]
async fn cached_definitions_require_the_same_active_driver_identity() {
    let f = Fixture::new();
    save_fixture(&f).await;
    for (owner, kind, integration) in [
        ("other", "synthetic", "synthetic-package"),
        ("synthetic", "other", "synthetic-package"),
        ("synthetic", "synthetic", "other-package"),
    ] {
        let hub = f.hub();
        let _commands = hub.register_driver(&owner.into(), kind, integration.into());
        assert!(hub.device(&id()).is_none());
    }
}

#[tokio::test]
async fn availability_and_ready_cannot_rearm_a_restored_definition() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let hub = f.hub();
    let _commands = register(&hub);
    hub.set_availability(&id(), true);
    hub.set_driver_status(&instance(), DriverStatus::Running);
    hub.mark_instance_offline(&instance());
    hub.set_driver_status(&instance(), DriverStatus::Starting);
    hub.set_driver_status(&instance(), DriverStatus::Running);
    let known = hub.device(&id()).unwrap();
    assert_eq!(known.online, Some(false));
    assert!(!known.device.points[0].access.write);
    hub.upsert_device(definition());
    let fresh = hub.device(&id()).unwrap();
    assert!(fresh.device.points[0].access.write);
    assert_ne!(
        fresh.online,
        Some(false),
        "fresh publication clears only the cache-imposed unavailability"
    );
}

#[tokio::test]
async fn definitions_without_state_are_saved_and_removal_is_durable() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    assert!(
        hub.persist_state().await.unwrap(),
        "a definition change dirties the cache without a sample"
    );
    let reboot = f.hub();
    let _reboot_commands = register(&reboot);
    assert!(reboot.device(&id()).is_some());
    reboot.remove_device(&id());
    assert!(reboot.persist_state().await.unwrap());
    let after = f.hub();
    let _after_commands = register(&after);
    assert!(after.device(&id()).is_none());
    assert!(after.known_device_ids_of(&instance()).is_empty());
}

#[tokio::test]
async fn sidecar_uses_one_coherent_definition_and_sample_snapshot() {
    let f = Fixture::new();
    save_fixture(&f).await;
    // Simulate a crash with state.json from a different save generation.
    std::fs::write(
        f.0.join("state.json"),
        r#"{"synthetic:device":{"on":{"value":false,"ts":1}}}"#,
    )
    .unwrap();
    let hub = f.hub();
    let _commands = register(&hub);
    assert_eq!(
        hub.device(&id()).unwrap().state["on"].value,
        Value::Bool(true)
    );
}

#[tokio::test]
async fn removed_samples_do_not_return_after_interrupted_legacy_replacement() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let old_values = std::fs::read(f.0.join("state.json")).unwrap();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.remove_device(&id());
    hub.persist_state().await.unwrap();
    // The sidecar commit succeeded, but the legacy replacement was interrupted.
    std::fs::write(f.0.join("state.json"), old_values).unwrap();
    let after = f.hub();
    let _next = register(&after);
    assert!(after.known_device_ids_of(&instance()).is_empty());
    after.upsert_device(definition());
    assert!(after.device(&id()).unwrap().state.is_empty());
}

#[tokio::test]
async fn legacy_values_still_load_and_corrupt_sidecars_never_restore_commands() {
    let f = Fixture::new();
    std::fs::write(
        f.0.join("state.json"),
        r#"{"synthetic:device":{"on":{"value":true,"ts":123}}}"#,
    )
    .unwrap();
    std::fs::write(f.0.join("state.devices.json"), b"{broken").unwrap();
    let hub = f.hub();
    let _commands = register(&hub);
    assert!(hub.device(&id()).is_none());
    hub.upsert_device(definition());
    assert_eq!(hub.device(&id()).unwrap().state["on"].ts, 123);
}

#[tokio::test]
async fn changed_schemas_do_not_replay_a_sample_of_the_previous_type() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let hub = f.hub();
    let _commands = register(&hub);
    let mut updated = definition();
    updated.points[0].kind = Kind::Text;
    hub.upsert_device(updated);
    assert!(hub.state(&PointId::new(&id(), "on")).is_none());
    assert!(hub.persist_state().await.unwrap());
    let after = f.hub();
    let _after_commands = register(&after);
    assert!(after.device(&id()).unwrap().state.is_empty());
}

#[tokio::test]
async fn explicit_driver_unavailability_survives_a_fresh_definition() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let hub = f.hub();
    let _commands = register(&hub);
    hub.set_availability(&id(), false);
    hub.upsert_device(definition());
    assert_eq!(hub.device(&id()).unwrap().online, Some(false));
}

#[tokio::test]
async fn concurrent_flushes_are_serialized_and_definitions_stay_legacy_compatible() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", Value::Bool(true));
    let (a, b) = tokio::join!(hub.persist_state(), hub.persist_state());
    assert_ne!(
        a.unwrap(),
        b.unwrap(),
        "one generation needs exactly one write"
    );
    let values: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.0.join("state.json")).unwrap()).unwrap();
    assert!(values.get("version").is_none());
    assert_eq!(values["synthetic:device"]["on"]["value"], true);
    assert!(f.0.join("state.devices.json").is_file());
}

#[tokio::test]
async fn unregister_disables_commands_and_reregistration_keeps_newer_live_samples() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", Value::Bool(true));
    hub.persist_state().await.unwrap();
    hub.set_state(&id(), "on", Value::Bool(false));
    let latest = hub.state(&PointId::new(&id(), "on")).unwrap();
    hub.unregister_driver(&instance());
    assert!(hub.snapshot().drivers.is_empty());
    let retired = hub.device(&id()).unwrap();
    assert_eq!(retired.online, Some(false));
    assert!(!retired.device.points[0].access.write);
    let _next_commands = register(&hub);
    assert_eq!(hub.state(&PointId::new(&id(), "on")).unwrap(), latest);
    hub.set_availability(&id(), true);
    assert!(!hub.device(&id()).unwrap().device.points[0].access.write);
    hub.upsert_device(definition());
    assert!(hub.device(&id()).unwrap().device.points[0].access.write);
}

#[tokio::test]
async fn malformed_identity_and_oversized_sidecars_are_discarded() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let path = f.0.join("state.devices.json");
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data["devices"]["synthetic:device"]["device"]["instance"] = "foreign".into();
    std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    let hub = f.hub();
    let _commands = register(&hub);
    assert!(hub.device(&id()).is_none());
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(8 * 1024 * 1024 + 1).unwrap();
    let oversized = f.hub();
    let _oversized_commands = register(&oversized);
    assert!(oversized.device(&id()).is_none());
}

#[tokio::test]
async fn reregistered_instance_does_not_expose_another_integration_inventory() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", Value::Bool(true));
    hub.unregister_driver(&instance());
    let _other = hub.register_driver(&instance(), "other", "other-package".into());
    assert!(hub.device(&id()).is_none());
    assert!(hub.known_device_ids_of(&instance()).is_empty());
    hub.unregister_driver(&instance());
    let _original = register(&hub);
    assert_eq!(
        hub.device(&id()).unwrap().state["on"].value,
        Value::Bool(true)
    );
}

#[derive(Debug)]
struct PendingInventory;
impl crate::Driver for PendingInventory {
    fn kind(&self) -> &'static str {
        "synthetic"
    }
    fn run<'a>(
        &'a self,
        ctx: &'a mut crate::DriverCtx,
    ) -> crate::BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            ctx.upsert_device(definition());
            ctx.set_availability(&id(), true);
            ctx.ready();
            std::future::pending::<()>().await;
            Ok(())
        })
    }
}

#[tokio::test]
async fn cancellation_retires_reachability_and_replies_to_queued_commands() {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = crate::spawn_driver(
        &hub,
        instance(),
        std::sync::Arc::new(PendingInventory),
        cancel.clone(),
    );
    for _ in 0..100 {
        if hub.device(&id()).is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(hub.device(&id()).unwrap().online, Some(true));
    let command_hub = hub.clone();
    let command = tokio::spawn(async move {
        command_hub
            .command(
                &PointId::new(&id(), "on"),
                Value::Bool(false),
                Origin::Ui,
                None,
            )
            .await
    });
    tokio::task::yield_now().await;
    cancel.cancel();
    task.await.unwrap();
    assert!(matches!(
        command.await.unwrap(),
        Err(CommandError::Driver(_) | CommandError::DriverUnavailable)
    ));
    assert_eq!(hub.device(&id()).unwrap().online, Some(false));
}

#[tokio::test]
async fn restoration_does_not_replay_cached_values_as_new_measurement_events() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let hub = f.hub();
    let mut events = hub.subscribe();
    let _commands = register(&hub);
    let mut saw_snapshot = false;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, moli_core::Event::State { .. }),
            "retained values must not trigger energy or automations"
        );
        if let moli_core::Event::DeviceRestored { device, states, .. } = event {
            assert_eq!(device.id, id());
            assert!(device.points.iter().all(|point| !point.access.write));
            assert_eq!(states["on"].value, Value::Bool(true));
            saw_snapshot = true;
        }
    }
    assert!(saw_snapshot);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", Value::Bool(false));
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, moli_core::Event::State { .. }))
    );
}

#[tokio::test]
async fn an_unsavable_inventory_does_not_block_legacy_values_and_can_be_retried() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", Value::Bool(true));
    hub.persist_state().await.unwrap();
    let previous_inventory = std::fs::read(f.0.join("state.devices.json")).unwrap();
    let mut oversized = definition();
    oversized.native_name = "a".repeat(8193).into();
    hub.upsert_device(oversized);
    hub.set_state(&id(), "on", Value::Bool(false));
    assert!(hub.persist_state().await.is_err());
    let values: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.0.join("state.json")).unwrap()).unwrap();
    assert_eq!(values["synthetic:device"]["on"]["value"], false);
    let reboot = f.hub();
    let _reboot_commands = register(&reboot);
    assert!(
        reboot.device(&id()).is_none(),
        "a stale authoritative sidecar must not override the saved legacy values"
    );
    reboot.upsert_device(definition());
    assert_eq!(
        reboot.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(false)
    );
    assert_eq!(
        std::fs::read(f.0.join("state.devices.json.stale")).unwrap(),
        previous_inventory
    );
    hub.upsert_device(definition());
    assert!(hub.persist_state().await.unwrap());
    assert!(!hub.persist_state().await.unwrap());
}

#[tokio::test]
async fn failed_inventory_deletion_does_not_resurrect_after_restart() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let hub = f.hub();
    let _commands = register(&hub);
    hub.remove_device(&id());
    let mut other = definition();
    other.id = "synthetic:other".into();
    other.native_name = "a".repeat(8193).into();
    hub.upsert_device(other.clone());
    hub.set_state(&other.id, "on", Value::Bool(true));
    assert!(hub.persist_state().await.is_err());
    let reboot = f.hub();
    let _next_commands = register(&reboot);
    assert!(reboot.device(&id()).is_none());
    assert!(!reboot.known_device_ids_of(&instance()).contains(&id()));
    other.native_name = "Synthetic other device".into();
    reboot.upsert_device(other.clone());
    assert_eq!(
        reboot.state(&PointId::new(&other.id, "on")).unwrap().value,
        Value::Bool(true)
    );
}

#[tokio::test]
async fn quarantine_without_a_previous_sidecar_keeps_the_legacy_fallback() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    let mut oversized = definition();
    oversized.native_name = "a".repeat(8193).into();
    hub.upsert_device(oversized);
    hub.set_state(&id(), "on", Value::Bool(false));
    assert!(hub.persist_state().await.is_err());
    assert!(!f.0.join("state.devices.json.stale").exists());
    let reboot = f.hub();
    let _next_commands = register(&reboot);
    reboot.upsert_device(definition());
    assert_eq!(
        reboot.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(false)
    );
}

#[tokio::test]
async fn quarantine_failure_is_reported_and_the_flush_can_be_retried() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let previous_values = std::fs::read(f.0.join("state.json")).unwrap();
    let hub = f.hub();
    let _commands = register(&hub);
    let mut oversized = definition();
    oversized.native_name = "a".repeat(8193).into();
    hub.upsert_device(oversized);
    hub.set_state(&id(), "on", Value::Bool(false));
    let blocked = f.0.join("state.devices.json.stale");
    std::fs::create_dir(&blocked).unwrap();
    let error = hub.persist_state().await.unwrap_err().to_string();
    assert!(
        error.contains("quarantine"),
        "the failed move is reported: {error}"
    );
    assert!(f.0.join("state.devices.json").is_file());
    assert_eq!(
        std::fs::read(f.0.join("state.json")).unwrap(),
        previous_values,
        "a blocked quarantine must not commit values that the old authority would ignore"
    );
    std::fs::remove_dir(blocked).unwrap();
    assert!(
        hub.persist_state().await.is_err(),
        "dirty retry still attempts the inadmissible inventory"
    );
    assert!(!f.0.join("state.devices.json").exists());
    let reboot = f.hub();
    let _next_commands = register(&reboot);
    reboot.upsert_device(definition());
    assert_eq!(
        reboot.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(false)
    );
}

#[tokio::test]
async fn failed_legacy_write_keeps_the_last_coherent_inventory_and_reports_both_errors() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let previous_inventory = std::fs::read(f.0.join("state.devices.json")).unwrap();
    let hub = f.hub();
    let _commands = register(&hub);
    let mut oversized = definition();
    oversized.native_name = "a".repeat(8193).into();
    hub.upsert_device(oversized);
    hub.set_state(&id(), "on", Value::Bool(false));
    std::fs::remove_file(f.0.join("state.json")).unwrap();
    std::fs::create_dir(f.0.join("state.json")).unwrap();
    let error = hub.persist_state().await.unwrap_err().to_string();
    assert!(
        error.contains("inventory cache") && error.contains("state cache"),
        "both failures are reported: {error}"
    );
    assert_eq!(
        std::fs::read(f.0.join("state.devices.json")).unwrap(),
        previous_inventory
    );
    assert!(!f.0.join("state.devices.json.stale").exists());
}

#[tokio::test]
async fn the_first_identical_live_reading_after_restore_is_a_fresh_measurement() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let path = f.0.join("state.devices.json");
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data["devices"]["synthetic:device"]["states"]["on"]["ts"] = 1.into();
    std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    assert_eq!(hub.state(&PointId::new(&id(), "on")).unwrap().ts, 1);
    let mut events = hub.subscribe();
    hub.set_state(&id(), "on", Value::Bool(true));
    let first: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    assert_eq!(
        first
            .iter()
            .filter(|event| matches!(event, moli_core::Event::State { .. }))
            .count(),
        1
    );
    assert!(hub.state(&PointId::new(&id(), "on")).unwrap().ts > 1);
    hub.set_state(&id(), "on", Value::Bool(true));
    assert!(
        events.try_recv().is_err(),
        "subsequent identical values keep normal deduplication"
    );
}

#[tokio::test]
async fn current_state_requires_fresh_samples_but_supports_drivers_without_availability() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let hub = f.hub();
    let _commands = register(&hub);
    let point = PointId::new(&id(), "on");
    assert!(
        hub.state(&point).is_some(),
        "retained values remain available for display"
    );
    assert!(
        hub.current_state(&point).is_none(),
        "retained values must not drive automations"
    );
    hub.upsert_device(definition());
    assert!(
        hub.current_state(&point).is_none(),
        "a fresh definition is not a fresh measurement"
    );
    hub.set_state(&id(), "on", Value::Bool(true));
    assert_eq!(hub.current_state(&point).unwrap().value, Value::Bool(true));
    hub.set_availability(&id(), false);
    assert!(hub.current_state(&point).is_none());
    let plain = Hub::new(HubOptions::default()).unwrap();
    let _plain_commands = register(&plain);
    plain.upsert_device(definition());
    plain.set_state(&id(), "on", Value::Bool(true));
    assert_eq!(plain.device(&id()).unwrap().online, None);
    assert!(plain.current_state(&point).is_some());
}

#[tokio::test]
async fn restored_inventory_events_include_persisted_local_labels() {
    let f = Fixture::new();
    let options = HubOptions {
        labels_path: Some(f.0.join("labels.toml")),
        ..f.options()
    };
    let hub = Hub::new(options.clone()).unwrap();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_label(
        &id(),
        crate::LabelPatch {
            name: Some("Synthetic local name".into()),
            room: Some("Synthetic room".into()),
        },
        Origin::Ui,
        None,
    )
    .await
    .unwrap();
    hub.persist_state().await.unwrap();
    let reboot = Hub::new(options).unwrap();
    let mut events = reboot.subscribe();
    let _next = register(&reboot);
    let event = std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| matches!(event, moli_core::Event::DeviceRestored { .. }))
        .unwrap();
    let moli_core::Event::DeviceRestored { label, .. } = event else {
        unreachable!()
    };
    assert_eq!(label.name.as_deref(), Some("Synthetic local name"));
    assert_eq!(label.room.as_deref(), Some("Synthetic room"));
}
