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
    hub.set_state(&id(), "on", &Value::Bool(true));
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
async fn removing_an_absent_device_does_not_write_another_cache_generation() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", &Value::Bool(true));
    assert!(hub.persist_state().await.unwrap());
    hub.remove_device(&"synthetic:missing".into());
    assert!(!hub.persist_state().await.unwrap());
    hub.remove_device(&id());
    assert!(hub.persist_state().await.unwrap());
    hub.remove_device(&id());
    assert!(!hub.persist_state().await.unwrap());
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
    hub.set_state(&id(), "on", &Value::Bool(true));
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
    hub.set_state(&id(), "on", &Value::Bool(false));
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, moli_core::Event::State { .. }))
    );
}

#[tokio::test]
async fn an_invalid_replacement_drops_only_its_definition_and_keeps_valid_legacy_values() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let hub = f.hub();
    let _commands = register(&hub);
    let mut oversized = definition();
    oversized.native_name = "a".repeat(8193).into();
    hub.upsert_device(oversized);
    hub.set_state(&id(), "on", &Value::Bool(false));
    assert!(hub.persist_state().await.unwrap());
    assert!(!hub.persist_state().await.unwrap());
    let reboot = f.hub();
    let _next = register(&reboot);
    assert!(reboot.device(&id()).is_none());
    reboot.upsert_device(definition());
    assert_eq!(
        reboot.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(false)
    );
}

#[tokio::test]
async fn deletion_is_durable_even_when_another_definition_is_invalid() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let hub = f.hub();
    let _commands = register(&hub);
    hub.remove_device(&id());
    let mut other = definition();
    other.id = "synthetic:other".into();
    other.native_name = "a".repeat(8193).into();
    hub.upsert_device(other);
    assert!(hub.persist_state().await.unwrap());
    let reboot = f.hub();
    let _next = register(&reboot);
    assert!(reboot.known_device_ids_of(&instance()).is_empty());
}

#[tokio::test]
async fn failed_observation_write_keeps_the_previous_snapshot_and_retries() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", &Value::Bool(true));
    hub.persist_state().await.unwrap();
    hub.set_state(&id(), "on", &Value::Bool(false));
    let blocked = f.0.join("state.observations.json.tmp");
    std::fs::create_dir(&blocked).unwrap();
    assert!(hub.persist_state().await.is_err());
    let before_retry = f.hub();
    let _before = register(&before_retry);
    assert_eq!(
        before_retry
            .state(&PointId::new(&id(), "on"))
            .unwrap()
            .value,
        Value::Bool(true)
    );
    std::fs::remove_dir(blocked).unwrap();
    assert!(hub.persist_state().await.unwrap());
    assert!(!hub.persist_state().await.unwrap());
    let reboot = f.hub();
    let _next = register(&reboot);
    assert_eq!(
        reboot.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(false)
    );
}

#[tokio::test]
async fn failed_legacy_write_cannot_resurrect_a_committed_inventory_deletion() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let hub = f.hub();
    let _commands = register(&hub);
    hub.remove_device(&id());
    let blocked = f.0.join("state.json.tmp");
    std::fs::create_dir(&blocked).unwrap();
    assert!(hub.persist_state().await.is_err());
    let reboot = f.hub();
    let _next = register(&reboot);
    assert!(reboot.known_device_ids_of(&instance()).is_empty());
    reboot.upsert_device(definition());
    assert!(reboot.device(&id()).unwrap().state.is_empty());
    std::fs::remove_dir(blocked).unwrap();
    assert!(hub.persist_state().await.unwrap());
    assert!(!hub.persist_state().await.unwrap());
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
    hub.set_state(&id(), "on", &Value::Bool(true));
    let first: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    assert_eq!(
        first
            .iter()
            .filter(|event| matches!(event, moli_core::Event::State { .. }))
            .count(),
        1
    );
    assert!(hub.state(&PointId::new(&id(), "on")).unwrap().ts > 1);
    hub.set_state(&id(), "on", &Value::Bool(true));
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
    hub.set_state(&id(), "on", &Value::Bool(true));
    assert_eq!(hub.current_state(&point).unwrap().value, Value::Bool(true));
    hub.set_availability(&id(), false);
    assert!(hub.current_state(&point).is_none());
    let plain = Hub::new(HubOptions::default()).unwrap();
    let _plain_commands = register(&plain);
    plain.upsert_device(definition());
    plain.set_state(&id(), "on", &Value::Bool(true));
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

#[tokio::test]
async fn state_only_flush_keeps_definition_file_and_restores_latest_observations() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", &Value::Bool(true));
    hub.persist_state().await.unwrap();
    let definitions = std::fs::read(f.0.join("state.devices.json")).unwrap();
    // Any attempt to rewrite the inventory now fails, even with identical bytes.
    std::fs::create_dir(f.0.join("state.devices.json.tmp")).unwrap();
    hub.upsert_device(definition());
    assert!(!hub.persist_state().await.unwrap());
    hub.set_state(&id(), "on", &Value::Bool(false));
    let latest = hub.state(&PointId::new(&id(), "on")).unwrap();
    assert!(hub.persist_state().await.unwrap());
    assert_eq!(
        std::fs::read(f.0.join("state.devices.json")).unwrap(),
        definitions
    );
    let reboot = f.hub();
    let _next = register(&reboot);
    assert_eq!(reboot.state(&PointId::new(&id(), "on")).unwrap(), latest);
    assert!(reboot.current_state(&PointId::new(&id(), "on")).is_none());
}

#[tokio::test]
async fn invalid_definitions_are_omitted_without_losing_healthy_devices_or_retrying() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", &Value::Bool(true));
    for case in ["enum", "text", "nan"] {
        let mut invalid = definition();
        invalid.id = format!("synthetic:{case}").into();
        match case {
            "enum" => {
                invalid.points[0].kind = Kind::Enum {
                    values: vec!["v".into(); 1025],
                }
            }
            "text" => invalid.native_name = "a".repeat(8193).into(),
            _ => {
                invalid.points[0].kind = Kind::Numeric {
                    min: Some(f64::NAN),
                    max: None,
                    step: None,
                }
            }
        }
        hub.upsert_device(invalid);
    }
    assert!(hub.persist_state().await.unwrap());
    assert!(
        !hub.persist_state().await.unwrap(),
        "permanent invalid input is not retryable I/O"
    );
    let reboot = f.hub();
    let _next = register(&reboot);
    assert_eq!(reboot.known_device_ids_of(&instance()), vec![id()]);
    assert_eq!(
        reboot.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(true)
    );
}

#[tokio::test]
async fn invalid_readings_do_not_poison_other_devices_or_reappear_as_old_samples() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    let mut text = definition();
    text.id = "synthetic:text".into();
    text.points[0].kind = Kind::Text;
    hub.upsert_device(text.clone());
    hub.set_state(&text.id, "on", &Value::Text("old".into()));
    hub.persist_state().await.unwrap();
    hub.set_state(&text.id, "on", &Value::Text("a".repeat(65537).into()));
    hub.set_state(&id(), "on", &Value::Bool(true));
    assert!(hub.persist_state().await.unwrap());
    assert!(!hub.persist_state().await.unwrap());
    let reboot = f.hub();
    let _next = register(&reboot);
    assert!(reboot.state(&PointId::new(&text.id, "on")).is_none());
    assert_eq!(
        reboot.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(true)
    );
}

#[tokio::test]
async fn deletion_and_readdition_ignore_observations_from_the_previous_definition_generation() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", &Value::Bool(true));
    hub.persist_state().await.unwrap();
    hub.set_state(&id(), "on", &Value::Bool(false));
    hub.persist_state().await.unwrap();
    let old_values = std::fs::read(f.0.join("state.json")).unwrap();
    hub.remove_device(&id());
    hub.persist_state().await.unwrap();
    hub.upsert_device(definition());
    hub.persist_state().await.unwrap();
    std::fs::write(f.0.join("state.json"), old_values).unwrap();
    let reboot = f.hub();
    let _next = register(&reboot);
    assert!(reboot.device(&id()).unwrap().state.is_empty());
}

#[tokio::test]
async fn failed_definition_write_keeps_the_last_snapshot_and_retries_the_deletion() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let hub = f.hub();
    let _commands = register(&hub);
    let old = std::fs::read(f.0.join("state.devices.json")).unwrap();
    hub.remove_device(&id());
    let blocked = f.0.join("state.devices.json.tmp");
    std::fs::create_dir(&blocked).unwrap();
    assert!(hub.persist_state().await.is_err());
    assert_eq!(std::fs::read(f.0.join("state.devices.json")).unwrap(), old);
    std::fs::remove_dir(blocked).unwrap();
    assert!(hub.persist_state().await.unwrap());
    assert!(!hub.persist_state().await.unwrap());
    let reboot = f.hub();
    let _next = register(&reboot);
    assert!(reboot.known_device_ids_of(&instance()).is_empty());
}

#[tokio::test]
async fn one_oversized_serialized_definition_does_not_exhaust_the_inventory_budget() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    let mut oversized = definition();
    oversized.id = "synthetic:a-oversized".into();
    oversized.points[0].kind = Kind::Enum {
        values: vec!["a".repeat(8192).into(); 1024],
    };
    hub.upsert_device(oversized);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", &Value::Bool(true));
    assert!(hub.persist_state().await.unwrap());
    assert!(!hub.persist_state().await.unwrap());
    assert!(
        std::fs::metadata(f.0.join("state.devices.json"))
            .unwrap()
            .len()
            <= 8 * 1024 * 1024
    );
    let reboot = f.hub();
    let _next = register(&reboot);
    assert_eq!(reboot.known_device_ids_of(&instance()), vec![id()]);
}

#[tokio::test]
async fn version_one_inventory_migrates_without_using_an_unrelated_observation_file() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    hub.upsert_device(definition());
    hub.set_state(&id(), "on", &Value::Bool(true));
    hub.persist_state().await.unwrap();
    hub.set_state(&id(), "on", &Value::Bool(false));
    hub.persist_state().await.unwrap();
    let path = f.0.join("state.devices.json");
    let mut old: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    old["version"] = 1.into();
    old.as_object_mut().unwrap().remove("generation");
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let migrated = f.hub();
    let _next = register(&migrated);
    assert_eq!(
        migrated.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(true)
    );
    assert!(migrated.persist_state().await.unwrap());
    let upgraded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(upgraded["version"], 2);
    migrated.set_state(&id(), "on", &Value::Bool(false));
    migrated.persist_state().await.unwrap();
    let reboot = f.hub();
    let _reboot_commands = register(&reboot);
    assert_eq!(
        reboot.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(false)
    );
}

#[tokio::test]
async fn one_malformed_serialized_definition_does_not_discard_its_valid_peer() {
    let f = Fixture::new();
    save_fixture(&f).await;
    let path = f.0.join("state.devices.json");
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data["devices"]["synthetic:malformed"] = serde_json::json!({"device":{"points":"invalid"}});
    std::fs::write(path, serde_json::to_vec(&data).unwrap()).unwrap();
    let reboot = f.hub();
    let _next = register(&reboot);
    assert_eq!(reboot.known_device_ids_of(&instance()), vec![id()]);
    assert_eq!(
        reboot.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(true)
    );
}

#[tokio::test]
async fn corrupt_observations_keep_the_coherent_baseline_and_ignore_legacy_values() {
    let f = Fixture::new();
    save_fixture(&f).await;
    std::fs::write(f.0.join("state.observations.json"), b"{broken").unwrap();
    std::fs::write(
        f.0.join("state.json"),
        r#"{"synthetic:device":{"on":{"value":false,"ts":1}}}"#,
    )
    .unwrap();
    let reboot = f.hub();
    let _next = register(&reboot);
    assert_eq!(
        reboot.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(true)
    );
}

#[tokio::test]
async fn oversized_observation_maps_are_bounded_without_losing_other_devices() {
    let f = Fixture::new();
    let hub = f.hub();
    let _commands = register(&hub);
    let mut large = definition();
    large.id = "synthetic:a-large".into();
    large.points[0].kind = Kind::Text;
    let point = large.points[0].clone();
    large.points = (0..129)
        .map(|i| PointSpec {
            key: format!("point-{i}").into(),
            ..point.clone()
        })
        .collect();
    hub.upsert_device(large.clone());
    hub.upsert_device(definition());
    hub.set_state(&large.id, "point-0", &Value::Text("previous".into()));
    hub.persist_state().await.unwrap();
    for point in &large.points {
        hub.set_state(
            &large.id,
            &point.key,
            &Value::Text("a".repeat(65536).into()),
        );
    }
    hub.set_state(&id(), "on", &Value::Bool(true));
    assert!(hub.persist_state().await.unwrap());
    assert!(!hub.persist_state().await.unwrap());
    for name in [
        "state.json",
        "state.devices.json",
        "state.observations.json",
    ] {
        assert!(std::fs::metadata(f.0.join(name)).unwrap().len() <= 8 * 1024 * 1024);
    }
    let reboot = f.hub();
    let _next = register(&reboot);
    assert_eq!(
        reboot.state(&PointId::new(&id(), "on")).unwrap().value,
        Value::Bool(true)
    );
    assert!(reboot.device(&large.id).unwrap().state.is_empty());
}

#[test]
fn cancelled_flush_cannot_lose_a_deletion_or_race_the_next_writer() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            let f = Fixture::new();
            let hub = f.hub();
            let _commands = register(&hub);
            hub.upsert_device(definition());
            hub.set_state(&id(), "on", &Value::Bool(true));
            hub.persist_state().await.unwrap();
            hub.remove_device(&id());
            // Hold the only filesystem worker: the next flush consumes its flags
            // then waits before its first atomic-file operation can complete.
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, waiting) = std::sync::mpsc::channel();
            let blocked = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                waiting.recv().unwrap();
            });
            ready.await.unwrap();
            let mut flush = Box::pin(hub.persist_state());
            assert!(futures::poll!(flush.as_mut()).is_pending());
            tokio::task::yield_now().await;
            drop(flush);
            release.send(()).unwrap();
            blocked.await.unwrap();
            hub.persist_state().await.unwrap();
            assert!(!hub.persist_state().await.unwrap());
            let reboot = f.hub();
            let _next = register(&reboot);
            assert!(reboot.known_device_ids_of(&instance()).is_empty());
        });
}
