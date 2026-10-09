//! Runtime behaviour with an in-memory fake driver.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use moli_core::{
    Access, Action, Device, DriverStatus, Event, InstanceId, Kind, Origin, Outcome, PointId,
    PointSpec, Semantic, Value,
};
use moli_runtime::{
    BoxFuture, CommandError, Driver, DriverCtx, Hub, HubOptions, LabelPatch, spawn_driver,
};
use tokio_util::sync::CancellationToken;

/// A smart plug: one writable on/off point, one read-only power point.
/// Commands are applied to its own state, like a real device would echo them.
/// Panics on its first run when `panic_once` is set.
struct FakePlug {
    runs: AtomicU32,
    panic_once: bool,
}

impl FakePlug {
    fn new(panic_once: bool) -> Arc<Self> {
        Arc::new(Self {
            runs: AtomicU32::new(0),
            panic_once,
        })
    }
}

fn spec(key: &str, kind: Kind, write: bool, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: key.into(),
        kind,
        access: Access { read: true, write },
        unit: None,
        semantic,
    }
}

impl Driver for FakePlug {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let first_run = self.runs.fetch_add(1, Ordering::SeqCst) == 0;
            assert!(!(first_run && self.panic_once), "boom");
            let id = ctx.device_id("plug1");
            ctx.upsert_device(Device {
                id: id.clone(),
                instance: ctx.instance().clone(),
                native_name: "plug1".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points: vec![
                    spec("state", Kind::Binary, true, Semantic::OnOff),
                    spec(
                        "power",
                        Kind::Numeric {
                            min: None,
                            max: None,
                            step: None,
                        },
                        false,
                        Semantic::Power,
                    ),
                    spec(
                        "control",
                        Kind::Enum {
                            values: vec!["pause".into(), "resume".into()],
                        },
                        true,
                        Semantic::Control,
                    ),
                ],
            });
            ctx.set_state(&id, "state", Value::Bool(false));
            ctx.set_state(&id, "ignored_unknown_key", Value::Int(1));
            ctx.ready();
            while let Some(cmd) = ctx.next_command().await {
                ctx.set_state(&cmd.device.id, &cmd.key, cmd.value.clone());
                cmd.reply(Ok(()));
            }
            Ok(())
        })
    }
}

async fn start(panic_once: bool) -> (Hub, CancellationToken) {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        FakePlug::new(panic_once),
        cancel.clone(),
    );
    wait_running(&hub).await;
    (hub, cancel)
}

async fn wait_running(hub: &Hub) {
    for _ in 0..200 {
        if hub.stats().drivers_running == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("driver never became running: {:?}", hub.snapshot().drivers);
}

fn point(key: &str) -> PointId {
    PointId::from(format!("fake:plug1/{key}"))
}

#[tokio::test]
async fn command_round_trip_updates_state_and_journal() {
    let (hub, _cancel) = start(false).await;
    let mut events = hub.subscribe();

    hub.command(
        &point("state"),
        Value::Bool(true),
        Origin::Api,
        Some("test".into()),
    )
    .await
    .unwrap();

    assert_eq!(hub.state(&point("state")).unwrap().value, Value::Bool(true));
    let mut saw_state = false;
    while let Ok(event) = events.try_recv() {
        saw_state |=
            matches!(event, Event::State { ref point, .. } if point.as_str() == "fake:plug1/state");
    }
    assert!(saw_state, "state change must be broadcast");

    let journal = hub.journal(10);
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].outcome, Outcome::Ok);
    assert_eq!(journal[0].origin, Origin::Api);
    assert!(matches!(journal[0].action, Action::Command { .. }));
}

#[tokio::test]
async fn invalid_writes_are_refused_and_journaled() {
    let (hub, _cancel) = start(false).await;

    let read_only = hub
        .command(&point("power"), Value::Int(5), Origin::Mcp, None)
        .await;
    assert_eq!(read_only, Err(CommandError::ReadOnly));

    let wrong_type = hub
        .command(&point("state"), Value::from("ON"), Origin::Mcp, None)
        .await;
    assert!(matches!(wrong_type, Err(CommandError::Invalid(_))));

    let unknown = hub
        .command(&point("nope"), Value::Bool(true), Origin::Mcp, None)
        .await;
    assert_eq!(unknown, Err(CommandError::UnknownPoint));

    let journal = hub.journal(10);
    assert_eq!(journal.len(), 3);
    assert!(journal.iter().all(|e| matches!(e.outcome, Outcome::Err(_))));
}

#[tokio::test]
async fn unknown_keys_from_drivers_are_ignored() {
    let (hub, _cancel) = start(false).await;
    let view = hub.snapshot().devices.pop().unwrap();
    assert_eq!(view.state.len(), 1);
    assert!(view.state.contains_key("state"));
}

#[tokio::test(start_paused = true)]
async fn panicking_driver_is_restarted() {
    let (hub, _cancel) = start(true).await;
    assert_eq!(hub.snapshot().devices.len(), 1);
    assert_eq!(hub.snapshot().drivers[0].status, DriverStatus::Running);
}

#[tokio::test]
async fn labels_persist_and_reload() {
    let dir = std::env::temp_dir().join(format!("moli-labels-{}", std::process::id()));
    let path = dir.join("labels.toml");
    let _ = std::fs::remove_file(&path);
    let opts = HubOptions {
        labels_path: Some(path.clone()),
        ..HubOptions::default()
    };

    let hub = Hub::new(opts.clone()).unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        FakePlug::new(false),
        cancel.clone(),
    );
    wait_running(&hub).await;

    let device = moli_core::DeviceId::from("fake:plug1");
    // Two agents patch different fields concurrently: neither write is lost.
    let (a, b) = tokio::join!(
        hub.set_label(
            &device,
            LabelPatch {
                name: Some("  Prise bureau ".into()),
                room: None
            },
            Origin::Mcp,
            None,
        ),
        hub.set_label(
            &device,
            LabelPatch {
                name: None,
                room: Some("Bureau".into())
            },
            Origin::Mcp,
            None,
        ),
    );
    a.unwrap();
    b.unwrap();
    let saved = hub.device(&device).unwrap().label;
    assert_eq!(saved.name.as_deref(), Some("Prise bureau"));
    assert_eq!(saved.room.as_deref(), Some("Bureau"));

    let unknown = hub
        .set_label(
            &moli_core::DeviceId::from("fake:ghost"),
            LabelPatch::default(),
            Origin::Ui,
            None,
        )
        .await;
    assert!(unknown.is_err());

    // A fresh hub reloads the label from disk.
    let reloaded = Hub::new(opts).unwrap();
    let cancel2 = CancellationToken::new();
    spawn_driver(
        &reloaded,
        InstanceId::from("fake"),
        FakePlug::new(false),
        cancel2.clone(),
    );
    wait_running(&reloaded).await;
    assert_eq!(
        reloaded.device(&device).unwrap().label.room.as_deref(),
        Some("Bureau")
    );

    cancel.cancel();
    cancel2.cancel();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn cached_values_are_restored_with_their_age() {
    let dir = std::env::temp_dir().join(format!("moli-state-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.json");
    std::fs::write(
        &path,
        r#"{"fake:plug1":{"power":{"value":42,"ts":1000},"gone":{"value":1,"ts":1}}}"#,
    )
    .unwrap();
    let opts = HubOptions {
        state_path: Some(path.clone()),
        ..HubOptions::default()
    };

    let hub = Hub::new(opts).unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        FakePlug::new(false),
        cancel.clone(),
    );
    wait_running(&hub).await;

    let power = hub.state(&point("power")).unwrap();
    assert_eq!(power.value, Value::Int(42));
    assert_eq!(power.ts, 1000, "restored values keep their original age");
    assert!(
        hub.state(&point("gone")).is_none(),
        "unknown keys are not resurrected"
    );

    // Live values win over cached ones and are persisted.
    assert!(hub.persist_state().await.unwrap());
    assert!(
        !hub.persist_state().await.unwrap(),
        "nothing changed, nothing written"
    );
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("\"state\""));

    cancel.cancel();
    let _ = std::fs::remove_dir_all(dir);
}

fn guarded_hub() -> HubOptions {
    HubOptions {
        guard: moli_runtime::guard::GuardPolicy {
            protected_rooms: vec!["Chambre".into()],
            quiet_hours: None,
            protect_unassigned: false,
            dashboard: moli_runtime::guard::Dashboard::Trusted,
        },
        ..HubOptions::default()
    }
}

async fn start_with(opts: HubOptions) -> (Hub, CancellationToken) {
    let hub = Hub::new(opts).unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        FakePlug::new(false),
        cancel.clone(),
    );
    wait_running(&hub).await;
    (hub, cancel)
}

async fn into_room(hub: &Hub, room: &str) {
    let patch = LabelPatch {
        name: None,
        room: Some(room.into()),
    };
    hub.set_label(&"fake:plug1".into(), patch, Origin::Ui, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn guard_holds_agents_and_obeys_humans() {
    let (hub, _cancel) = start_with(guarded_hub()).await;
    into_room(&hub, "chambre").await;

    // An agent is held: nothing happens, a request exists.
    let held = hub
        .command(
            &point("state"),
            Value::Bool(true),
            Origin::Mcp,
            Some("agent".into()),
        )
        .await;
    let Err(CommandError::NeedsApproval { id, reason }) = held else {
        panic!("expected a held order, got {held:?}");
    };
    assert!(reason.contains("Chambre"));
    assert_eq!(
        hub.state(&point("state")).unwrap().value,
        Value::Bool(false)
    );
    assert_eq!(hub.approvals().len(), 1);
    assert!(matches!(hub.journal(1)[0].outcome, Outcome::Pending(_)));

    // An agent cannot approve its own request.
    let self_approved = hub
        .resolve_approval(id, &point("state"), true, Origin::Mcp, None)
        .await;
    assert_eq!(self_approved, Err(moli_runtime::ApprovalError::HumanOnly));
    assert_eq!(hub.approvals().len(), 1);

    // A decision naming another point than the held one is refused.
    let wrong = hub
        .resolve_approval(id, &point("power"), true, Origin::Ui, None)
        .await;
    assert_eq!(wrong, Err(moli_runtime::ApprovalError::Mismatch));
    assert_eq!(hub.approvals().len(), 1);
    assert_eq!(
        hub.approvals()[0].device_name,
        "plug1",
        "name frozen at hold time"
    );

    // The human approves: the order runs, exactly once.
    hub.resolve_approval(id, &point("state"), true, Origin::Ui, Some("sam".into()))
        .await
        .unwrap();
    assert_eq!(hub.state(&point("state")).unwrap().value, Value::Bool(true));
    assert!(hub.approvals().is_empty());
    assert_eq!(
        hub.resolve_approval(id, &point("state"), true, Origin::Ui, None)
            .await,
        Err(moli_runtime::ApprovalError::Unknown)
    );

    // A human in the dashboard is never held.
    hub.command(&point("state"), Value::Bool(false), Origin::Ui, None)
        .await
        .unwrap();
    assert_eq!(
        hub.state(&point("state")).unwrap().value,
        Value::Bool(false)
    );
}

#[tokio::test]
async fn denied_requests_never_run_and_invalid_ones_are_never_held() {
    let (hub, _cancel) = start_with(guarded_hub()).await;
    into_room(&hub, "Chambre").await;

    let Err(CommandError::NeedsApproval { id, .. }) = hub
        .command(&point("state"), Value::Bool(true), Origin::Api, None)
        .await
    else {
        panic!("expected a held order");
    };
    hub.resolve_approval(id, &point("state"), false, Origin::Ui, None)
        .await
        .unwrap();
    assert_eq!(
        hub.state(&point("state")).unwrap().value,
        Value::Bool(false)
    );
    let decision = &hub.journal(1)[0].action;
    assert!(matches!(
        decision,
        Action::Approval {
            decision: moli_core::Decision::Denied,
            ..
        }
    ));

    // Invalid orders fail right away instead of waiting for a human.
    let invalid = hub
        .command(&point("state"), Value::from("ON"), Origin::Mcp, None)
        .await;
    assert!(matches!(invalid, Err(CommandError::Invalid(_))));
    assert!(hub.approvals().is_empty());
}

#[tokio::test]
async fn agents_cannot_relabel_their_way_out_of_a_protected_room() {
    let (hub, _cancel) = start_with(guarded_hub()).await;
    into_room(&hub, "Chambre").await;
    let escape = LabelPatch {
        name: None,
        room: Some("Grenier".into()),
    };
    let refused = hub
        .set_label(&"fake:plug1".into(), escape.clone(), Origin::Mcp, None)
        .await;
    assert!(matches!(refused, Err(moli_runtime::LabelError::Guarded(_))));
    // Renaming without moving stays allowed.
    let rename = LabelPatch {
        name: Some("Veilleuse".into()),
        room: None,
    };
    hub.set_label(&"fake:plug1".into(), rename, Origin::Mcp, None)
        .await
        .unwrap();
    // A human can move it.
    hub.set_label(&"fake:plug1".into(), escape, Origin::Ui, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn stopping_a_machine_is_a_humans_call_in_any_room() {
    // No guard on rooms at all: the order to a machine at work still waits.
    let (hub, _cancel) = start(false).await;
    let held = hub
        .command(
            &point("control"),
            Value::from("pause"),
            Origin::Assistant,
            None,
        )
        .await;
    assert!(matches!(held, Err(CommandError::NeedsApproval { .. })));
    assert!(hub.state(&point("control")).is_none(), "nothing ran");
    for origin in [Origin::Ui, Origin::Automation] {
        hub.command(&point("control"), Value::from("pause"), origin, None)
            .await
            .unwrap();
    }
    // Other points of the same device stay free.
    hub.command(&point("state"), Value::Bool(true), Origin::Mcp, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn unguarded_rooms_are_free() {
    let (hub, _cancel) = start_with(guarded_hub()).await;
    into_room(&hub, "Grenier").await;
    hub.command(&point("state"), Value::Bool(true), Origin::Mcp, None)
        .await
        .unwrap();
    assert_eq!(hub.state(&point("state")).unwrap().value, Value::Bool(true));
}

#[tokio::test]
async fn journal_survives_restarts() {
    let dir = std::env::temp_dir().join(format!("moli-journal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let opts = HubOptions {
        journal_path: Some(dir.join("journal.jsonl")),
        ..HubOptions::default()
    };
    {
        let hub = Hub::new(opts.clone()).unwrap();
        let cancel = CancellationToken::new();
        spawn_driver(
            &hub,
            InstanceId::from("fake"),
            FakePlug::new(false),
            cancel.clone(),
        );
        wait_running(&hub).await;
        hub.command(
            &point("state"),
            Value::Bool(true),
            Origin::Cli,
            Some("t".into()),
        )
        .await
        .unwrap();
        cancel.cancel();
    }
    // The writer thread flushes asynchronously.
    for _ in 0..100 {
        if std::fs::read_to_string(dir.join("journal.jsonl")).is_ok_and(|t| !t.is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let reloaded = Hub::new(opts).unwrap();
    let journal = reloaded.journal(10);
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].actor.as_deref(), Some("t"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn foreign_devices_are_refused() {
    struct Rogue;
    impl Driver for Rogue {
        fn kind(&self) -> &'static str {
            "rogue"
        }
        fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                ctx.upsert_device(Device {
                    id: moli_core::DeviceId::from("other:x"),
                    instance: InstanceId::from("other"),
                    native_name: "x".into(),
                    manufacturer: None,
                    model: None,
                    description: None,
                    native_room: None,
                    members: Vec::new(),
                    points: vec![],
                });
                ctx.ready();
                ctx.cancelled().await;
                Ok(())
            })
        }
    }
    let hub = Hub::new(HubOptions::default()).unwrap();
    spawn_driver(
        &hub,
        InstanceId::from("rogue"),
        Arc::new(Rogue),
        CancellationToken::new(),
    );
    wait_running(&hub).await;
    assert!(hub.snapshot().devices.is_empty());
}

/// A Hue-like room: a group device in « Salon » whose member lamp sits in
/// « Chambre à coucher ».
struct FakeGroup;

impl Driver for FakeGroup {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let lamp = ctx.device_id("lamp");
            let group = ctx.device_id("group");
            for (id, name, room, members) in [
                (lamp.clone(), "lamp", "Chambre à coucher", Vec::new()),
                (group.clone(), "group", "Salon", vec![lamp.clone()]),
            ] {
                ctx.upsert_device(Device {
                    id: id.clone(),
                    instance: ctx.instance().clone(),
                    native_name: name.into(),
                    manufacturer: None,
                    model: None,
                    description: None,
                    native_room: Some(room.into()),
                    members,
                    points: vec![spec("state", Kind::Binary, true, Semantic::OnOff)],
                });
                ctx.set_state(&id, "state", Value::Bool(false));
            }
            ctx.ready();
            while let Some(cmd) = ctx.next_command().await {
                ctx.set_state(&cmd.device.id, &cmd.key, cmd.value.clone());
                cmd.reply(Ok(()));
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn unassigned_devices_are_protected_for_images_when_asked() {
    let plug = moli_core::DeviceId::from("fake:plug1");
    let (hub, _cancel) = start_with(guarded_hub()).await;
    assert_eq!(hub.protected_room(&plug), None);

    let mut opts = guarded_hub();
    opts.guard.protect_unassigned = true;
    let (hub, _cancel) = start_with(opts).await;
    assert_eq!(
        hub.protected_room(&plug).as_deref(),
        Some("appareil sans pièce")
    );
}

#[tokio::test]
async fn groups_are_guarded_by_their_members_rooms_and_lost_rooms_are_reported() {
    let hub = Hub::new(HubOptions {
        guard: moli_runtime::guard::GuardPolicy {
            // Typed without the accent, plus a room no device is in.
            protected_rooms: vec!["chambre a  coucher".into(), "Bureau".into()],
            quiet_hours: None,
            protect_unassigned: false,
            dashboard: moli_runtime::guard::Dashboard::Trusted,
        },
        ..HubOptions::default()
    })
    .unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        Arc::new(FakeGroup),
        cancel.clone(),
    );
    wait_running(&hub).await;

    // The group lives in « Salon », but reaches a lamp in the bedroom.
    let held = hub
        .command(
            &PointId::from("fake:group/state".to_owned()),
            Value::Bool(true),
            Origin::Mcp,
            None,
        )
        .await;
    assert!(
        matches!(held, Err(CommandError::NeedsApproval { .. })),
        "{held:?}"
    );
    assert_eq!(hub.unmatched_protected_rooms(), vec!["Bureau".to_owned()]);
    assert_eq!(
        hub.guard_status().unmatched_protected_rooms,
        vec!["Bureau".to_owned()]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn snapshots_never_deadlock_against_writers() {
    let (hub, _cancel) = start_with(guarded_hub()).await;
    let writer = {
        let hub = hub.clone();
        tokio::spawn(async move {
            for i in 0..300 {
                let room = if i % 2 == 0 { "Grenier" } else { "Salon" };
                into_room(&hub, room).await;
            }
        })
    };
    let reader = {
        let hub = hub.clone();
        tokio::task::spawn_blocking(move || {
            for _ in 0..20_000 {
                let _ = hub.snapshot();
            }
        })
    };
    tokio::time::timeout(Duration::from_secs(20), async {
        writer.await.unwrap();
        reader.await.unwrap();
    })
    .await
    .expect("snapshot deadlocked against a writer");
}

/// A camera whose every image is counted.
#[derive(Debug, Default)]
struct CountingLens(AtomicU32);

impl moli_runtime::media::SnapshotSource for CountingLens {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<moli_runtime::media::Image>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(moli_runtime::media::Image {
                content_type: "image/jpeg".into(),
                bytes: vec![0xFF, 0xD8, 0xFF, 0xD9],
            })
        })
    }
}

struct FakeCamera(Arc<CountingLens>);

impl Driver for FakeCamera {
    fn kind(&self) -> &'static str {
        "fake-camera"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let id = ctx.device_id("cam");
            ctx.upsert_device(Device {
                id: id.clone(),
                instance: ctx.instance().clone(),
                native_name: "Porte".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: Some("Entrée".into()),
                members: Vec::new(),
                points: Vec::new(),
            });
            ctx.provide_snapshots(
                &id,
                Arc::clone(&self.0) as Arc<dyn moli_runtime::media::SnapshotSource>,
            );
            ctx.ready();
            ctx.cancelled().await;
            Ok(())
        })
    }
}

#[tokio::test]
async fn two_screens_on_one_camera_cost_one_image() {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let lens = Arc::new(CountingLens::default());
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        Arc::new(FakeCamera(Arc::clone(&lens))),
        cancel.clone(),
    );
    wait_running(&hub).await;
    let cam = moli_core::DeviceId::from("fake:cam");
    let (a, b) = tokio::join!(hub.camera_image(&cam), hub.camera_image(&cam));
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(
        lens.0.load(Ordering::SeqCst),
        1,
        "both viewers share one fetch"
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    hub.camera_image(&cam).await.unwrap();
    assert_eq!(
        lens.0.load(Ordering::SeqCst),
        2,
        "a second later, a fresh image"
    );
    cancel.cancel();
}

/// A lamp whose driver is slow to read its orders, or fails after a while.
struct Sluggish {
    read_after: Duration,
    fail_after: Option<Duration>,
    executed: Arc<AtomicU32>,
}

impl Driver for Sluggish {
    fn kind(&self) -> &'static str {
        "sluggish"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let id = ctx.device_id("lamp");
            ctx.upsert_device(Device {
                id: id.clone(),
                instance: ctx.instance().clone(),
                native_name: "Lampe".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: Some("Salon".into()),
                members: Vec::new(),
                points: vec![spec("state", Kind::Binary, true, Semantic::OnOff)],
            });
            ctx.set_availability(&id, true);
            ctx.ready();
            if let Some(after) = self.fail_after {
                tokio::time::sleep(after).await;
                anyhow::bail!("connection lost");
            }
            tokio::time::sleep(self.read_after).await;
            while let Some(command) = ctx.next_command().await {
                self.executed.fetch_add(1, Ordering::SeqCst);
                command.reply(Ok(()));
            }
            Ok(())
        })
    }
}

fn lamp() -> PointId {
    PointId::from("fake:lamp/state".to_owned())
}

#[tokio::test]
async fn stale_orders_never_run() {
    let hub = Hub::new(HubOptions {
        command_timeout: Duration::from_millis(300),
        ..HubOptions::default()
    })
    .unwrap();
    let executed = Arc::new(AtomicU32::new(0));
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        Arc::new(Sluggish {
            read_after: Duration::from_millis(1200),
            fail_after: None,
            executed: Arc::clone(&executed),
        }),
        cancel.clone(),
    );
    wait_running(&hub).await;
    let late = hub
        .command(&lamp(), Value::Bool(true), Origin::Ui, None)
        .await;
    assert_eq!(late, Err(CommandError::Timeout));
    // The driver wakes up after the hub gave up: the order must not run.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        executed.load(Ordering::SeqCst),
        0,
        "a timed-out order ran later"
    );
    // A fresh order still works.
    assert_eq!(
        hub.command(&lamp(), Value::Bool(false), Origin::Ui, None)
            .await,
        Ok(())
    );
    assert_eq!(executed.load(Ordering::SeqCst), 1);
    cancel.cancel();
}

#[tokio::test]
async fn a_restarting_driver_drops_its_queue_and_its_devices_go_offline() {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let executed = Arc::new(AtomicU32::new(0));
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        Arc::new(Sluggish {
            read_after: Duration::ZERO,
            fail_after: Some(Duration::from_millis(300)),
            executed: Arc::clone(&executed),
        }),
        cancel.clone(),
    );
    wait_running(&hub).await;
    let lamp_id = moli_core::DeviceId::from("fake:lamp");
    assert_eq!(hub.device(&lamp_id).unwrap().online, Some(true));
    // Queued while the driver is about to fail: refused, not kept for later.
    let started = std::time::Instant::now();
    let refused = hub
        .command(&lamp(), Value::Bool(true), Origin::Ui, None)
        .await;
    assert!(
        matches!(&refused, Err(CommandError::Driver(why)) if why.contains("restarting")),
        "{refused:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "refused at once, not after the timeout"
    );
    assert_eq!(
        hub.device(&lamp_id).unwrap().online,
        Some(false),
        "offline while down"
    );
    assert_eq!(executed.load(Ordering::SeqCst), 0);
    cancel.cancel();
}

/// Announces its group's availability on the first run only (like a Hue
/// group), then fails once.
#[derive(Debug, Default)]
struct OnceAnnounced {
    runs: AtomicU32,
}

impl Driver for OnceAnnounced {
    fn kind(&self) -> &'static str {
        "once"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let run = self.runs.fetch_add(1, Ordering::SeqCst);
            let id = ctx.device_id("group");
            ctx.upsert_device(Device {
                id: id.clone(),
                instance: ctx.instance().clone(),
                native_name: "Groupe".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: Some("Salon".into()),
                members: Vec::new(),
                points: vec![spec("state", Kind::Binary, true, Semantic::OnOff)],
            });
            if run == 0 {
                ctx.set_availability(&id, true);
                ctx.ready();
                tokio::time::sleep(Duration::from_millis(200)).await;
                anyhow::bail!("connection lost");
            }
            ctx.ready();
            while let Some(command) = ctx.next_command().await {
                command.reply(Ok(()));
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn devices_come_back_when_their_driver_does() {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let cancel = CancellationToken::new();
    let driver = Arc::new(OnceAnnounced::default());
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        driver.clone(),
        cancel.clone(),
    );
    let group = moli_core::DeviceId::from("fake:group");
    let mut seen_offline = false;
    for _ in 0..400 {
        let online = hub.device(&group).and_then(|d| d.online);
        seen_offline |= online == Some(false);
        if seen_offline && driver.runs.load(Ordering::SeqCst) >= 2 && online == Some(true) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(seen_offline, "offline while its driver was down");
    assert_eq!(
        hub.device(&group).unwrap().online,
        Some(true),
        "back online with its driver, though never re-announced"
    );
    cancel.cancel();
}

#[tokio::test]
async fn agents_name_but_never_rename() {
    let (hub, _cancel) = start(false).await;
    let plug = moli_core::DeviceId::from("fake:plug1");
    let name = |n: &str| LabelPatch {
        name: Some(n.into()),
        room: None,
    };
    // Naming a device nobody named: allowed (onboarding), cleaned and bounded.
    let named = hub
        .set_label(&plug, name("  Prise\n du   salon "), Origin::Mcp, None)
        .await
        .unwrap();
    assert_eq!(named.name.as_deref(), Some("Prise du salon"));
    // Renaming it: only a person.
    let renamed = hub
        .set_label(&plug, name("Lampe du couloir"), Origin::Mcp, None)
        .await;
    assert!(
        matches!(renamed, Err(moli_runtime::LabelError::Guarded(_))),
        "{renamed:?}"
    );
    let by_human = hub
        .set_label(&plug, name(&"x".repeat(200)), Origin::Ui, None)
        .await
        .unwrap();
    assert_eq!(
        by_human.name.unwrap().chars().count(),
        moli_runtime::MAX_LABEL
    );
}

#[tokio::test]
async fn a_newer_request_replaces_the_older_one() {
    let (hub, _cancel) = start_with(HubOptions {
        guard: moli_runtime::guard::GuardPolicy {
            protected_rooms: vec![],
            // From midnight to midnight: the whole day is quiet for agents.
            quiet_hours: Some(
                moli_runtime::guard::QuietHours::parse("00:00", "00:00", "Europe/Paris", vec![])
                    .unwrap(),
            ),
            protect_unassigned: false,
            dashboard: moli_runtime::guard::Dashboard::Trusted,
        },
        ..HubOptions::default()
    })
    .await;
    for value in [true, false, true] {
        let held = hub
            .command(
                &point("state"),
                Value::Bool(value),
                Origin::Mcp,
                Some("agent".into()),
            )
            .await;
        assert!(
            matches!(held, Err(CommandError::NeedsApproval { .. })),
            "{held:?}"
        );
    }
    let pending = hub.approvals();
    assert_eq!(
        pending.len(),
        1,
        "one card per point and origin, the latest"
    );
    assert_eq!(pending[0].value, Value::Bool(true));
    let other = hub
        .command(&point("state"), Value::Bool(false), Origin::Api, None)
        .await;
    assert!(matches!(other, Err(CommandError::NeedsApproval { .. })));
    assert_eq!(
        hub.approvals().len(),
        2,
        "another origin is another request"
    );
}

#[tokio::test]
async fn oversized_orders_are_refused_and_journaled_without_their_bulk() {
    let (hub, _cancel) = start(false).await;
    let huge = PointId::from(format!("fake:plug1/{}", "x".repeat(10_000)));
    let refused = hub
        .command(
            &huge,
            Value::Bool(true),
            Origin::Api,
            Some("y".repeat(10_000)),
        )
        .await;
    assert!(
        matches!(refused, Err(CommandError::Invalid(_))),
        "{refused:?}"
    );
    let last = hub.journal(1);
    let entry = last.last().unwrap();
    assert!(entry.actor.as_deref().unwrap().chars().count() <= 64);
    match &entry.action {
        Action::Command { point, value } => {
            assert!(point.as_str().len() <= 256);
            assert_eq!(*value, Value::Null);
        }
        other => panic!("{other:?}"),
    }
}
