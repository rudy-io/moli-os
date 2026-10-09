//! The engine against a real hub and a fake driver: a switch and a lamp.

use std::sync::Arc;
use std::time::Duration;

use moli_automation::{
    Actor, Author, Automation, AutomationError, Automations, Config, Graph, Mode, RunStatus, Step,
};
use moli_core::{Access, Device, InstanceId, Kind, Origin, PointId, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx, Hub, HubOptions, spawn_driver};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Fake;

fn onoff(key: &str) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: key.into(),
        kind: Kind::Binary,
        access: Access {
            read: true,
            write: true,
        },
        unit: None,
        semantic: Semantic::OnOff,
    }
}

impl Driver for Fake {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            for name in ["switch", "lamp"] {
                let id = ctx.device_id(name);
                ctx.upsert_device(Device {
                    id: id.clone(),
                    instance: ctx.instance().clone(),
                    native_name: name.into(),
                    manufacturer: None,
                    model: None,
                    description: None,
                    native_room: None,
                    members: Vec::new(),
                    points: vec![onoff("on")],
                });
                ctx.set_state(&id, "on", Value::Bool(false));
            }
            // A speaker that can announce (write-only words).
            ctx.upsert_device(Device {
                id: ctx.device_id("speaker"),
                instance: ctx.instance().clone(),
                native_name: "speaker".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points: vec![PointSpec {
                    key: "announce".into(),
                    label: "Annonce".into(),
                    kind: Kind::Text,
                    access: Access {
                        read: false,
                        write: true,
                    },
                    unit: None,
                    semantic: Semantic::Other,
                }],
            });
            ctx.ready();
            while let Some(cmd) = ctx.next_command().await {
                ctx.set_state(&cmd.device.id, &cmd.key, cmd.value.clone());
                cmd.reply(Ok(()));
            }
            Ok(())
        })
    }
}

async fn setup() -> (Hub, Automations, CancellationToken) {
    let hub = Hub::new(HubOptions::default()).unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        InstanceId::from("fake"),
        Arc::new(Fake),
        cancel.clone(),
    );
    for _ in 0..200 {
        if hub.state(&PointId::from("fake:lamp/on")).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (autos, _task) = Automations::start(&hub, Config::default(), None, cancel.clone())
        .await
        .unwrap();
    (hub, autos, cancel)
}

fn automation(graph: serde_json::Value) -> Automation {
    Automation {
        id: String::new(),
        name: "Test".into(),
        enabled: true,
        mode: Mode::Restart,
        graph: serde_json::from_value::<Graph>(graph).unwrap(),
        author: Author::Human,
        approved: None,
        approved_version: None,
        note: None,
        created: 0,
        updated: 0,
    }
}

fn switch_lights_lamp() -> serde_json::Value {
    serde_json::json!({
        "nodes": [
            { "id": "t", "type": "when_state", "point": "fake:switch/on", "to": true },
            { "id": "s", "type": "set", "point": "fake:lamp/on", "value": true }
        ],
        "edges": [{ "from": "t", "to": "s" }]
    })
}

const HUMAN: Actor = Actor {
    human: true,
    author: Author::Human,
};
const AGENT: Actor = Actor {
    human: false,
    author: Author::Agent,
};

async fn lamp_turns(hub: &Hub, on: bool) -> bool {
    for _ in 0..100 {
        if hub.state(&PointId::from("fake:lamp/on")).map(|s| s.value) == Some(Value::Bool(on)) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

async fn flip_switch(hub: &Hub, on: bool) {
    hub.command(
        &PointId::from("fake:switch/on"),
        Value::Bool(on),
        Origin::Api,
        None,
    )
    .await
    .unwrap();
}

/// A person saves, looks, and approves what they saw.
async fn approved(autos: &Automations, graph: serde_json::Value) -> Automation {
    let a = autos.save(automation(graph), HUMAN).await.unwrap();
    assert!(!a.is_live(), "saving alone never approves");
    autos.approve(&a.id, &a.fingerprint(), HUMAN).await.unwrap()
}

#[tokio::test]
async fn listed_fingerprints_follow_every_change() {
    let (_hub, autos, cancel) = setup().await;
    let listed = |autos: &Automations, id: &str| {
        autos
            .list_fingerprinted()
            .into_iter()
            .find(|(a, _)| a.id == id)
            .map(|(_, f)| f)
    };
    let a = approved(&autos, switch_lights_lamp()).await;
    assert_eq!(listed(&autos, &a.id), Some(a.fingerprint()));
    // Another version saved: the listing says so at once.
    let mut changed = a.clone();
    changed.graph.nodes[1].step = Step::Set {
        point: "fake:lamp/on".into(),
        value: serde_json::json!(false),
    };
    let changed = autos.save(changed, HUMAN).await.unwrap();
    assert_ne!(changed.fingerprint(), a.fingerprint());
    assert_eq!(listed(&autos, &a.id), Some(changed.fingerprint()));
    // Back to the approved version (an in-place change).
    let back = autos.restore(&a.id, HUMAN).await.unwrap();
    assert_eq!(listed(&autos, &a.id), Some(back.fingerprint()));
    assert_eq!(back.fingerprint(), a.fingerprint());
    autos.delete(&a.id, HUMAN).await.unwrap();
    assert_eq!(listed(&autos, &a.id), None);
    cancel.cancel();
}

#[tokio::test]
async fn an_approved_automation_acts() {
    let (hub, autos, _c) = setup().await;
    let a = approved(&autos, switch_lights_lamp()).await;
    assert!(a.is_live());
    assert_eq!(
        autos.check(&a.graph).summary,
        "Quand switch s'allume, allumer lamp."
    );

    flip_switch(&hub, true).await;
    assert!(lamp_turns(&hub, true).await, "the lamp follows the switch");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let runs = autos.runs(Some(&a.id), 10);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Done);
    let journal = hub.journal(5);
    assert!(
        journal.iter().any(|e| e.origin == Origin::Automation),
        "journaled as an automation"
    );
}

#[tokio::test]
async fn an_agents_draft_waits_for_a_person() {
    let (hub, autos, _c) = setup().await;
    let a = autos
        .save(automation(switch_lights_lamp()), AGENT)
        .await
        .unwrap();
    assert!(!a.is_live(), "an agent's automation is a draft");
    assert!(
        autos
            .set_enabled(&a.id, true, Some(&a.fingerprint()), AGENT)
            .await
            .is_err()
    );
    assert!(autos.approve(&a.id, &a.fingerprint(), AGENT).await.is_err());
    assert!(
        autos
            .run_now(&a.id, false, Some(&a.fingerprint()), AGENT)
            .await
            .is_err(),
        "agents only get dry runs"
    );

    flip_switch(&hub, true).await;
    assert!(
        !lamp_turns(&hub, true).await,
        "nothing happens before approval"
    );

    autos.approve(&a.id, &a.fingerprint(), HUMAN).await.unwrap();
    flip_switch(&hub, false).await;
    flip_switch(&hub, true).await;
    assert!(lamp_turns(&hub, true).await, "approved, it runs");
}

#[tokio::test]
async fn approving_what_you_did_not_see_is_refused() {
    let (_hub, autos, _c) = setup().await;
    let a = autos
        .save(automation(switch_lights_lamp()), AGENT)
        .await
        .unwrap();
    let seen = a.fingerprint();
    // Someone changes it while the person reads.
    let mut b = a.clone();
    b.graph.nodes[1].step = Step::Set {
        point: "fake:lamp/on".into(),
        value: serde_json::json!(false),
    };
    autos.save(b, AGENT).await.unwrap();
    assert!(matches!(
        autos.approve(&a.id, &seen, HUMAN).await,
        Err(AutomationError::Conflict)
    ));
    assert!(!autos.get(&a.id).unwrap().is_live());
}

#[tokio::test]
async fn an_agent_cannot_switch_it_back_on() {
    let (_hub, autos, _c) = setup().await;
    let a = approved(&autos, switch_lights_lamp()).await;
    autos.set_enabled(&a.id, false, None, AGENT).await.unwrap();
    let mut again = autos.get(&a.id).unwrap();
    again.enabled = true;
    let saved = autos.save(again, AGENT).await.unwrap();
    assert!(!saved.enabled, "an agent's save cannot switch on");
    assert!(
        autos
            .set_enabled(&a.id, true, Some(&a.fingerprint()), AGENT)
            .await
            .is_err()
    );
    assert!(!autos.get(&a.id).unwrap().is_live());
}

#[tokio::test]
async fn a_real_run_needs_the_approved_version() {
    let (_hub, autos, _c) = setup().await;
    let a = autos
        .save(automation(switch_lights_lamp()), HUMAN)
        .await
        .unwrap();
    assert!(
        autos
            .run_now(&a.id, false, Some(&a.fingerprint()), HUMAN)
            .await
            .is_err(),
        "not approved yet"
    );
    let a = autos.approve(&a.id, &a.fingerprint(), HUMAN).await.unwrap();
    assert!(matches!(
        autos.run_now(&a.id, false, Some("other"), HUMAN).await,
        Err(AutomationError::Conflict)
    ));
    assert!(
        autos
            .run_now(&a.id, false, Some(&a.fingerprint()), HUMAN)
            .await
            .is_ok()
    );
    // Switched off, a real run would be cancelled at birth: refused, not « running ».
    autos.set_enabled(&a.id, false, None, HUMAN).await.unwrap();
    assert!(matches!(
        autos
            .run_now(&a.id, false, Some(&a.fingerprint()), HUMAN)
            .await,
        Err(AutomationError::Invalid(_))
    ));
}

#[tokio::test]
async fn a_dry_run_reads_but_never_acts() {
    let (hub, autos, _c) = setup().await;
    let a = autos
        .save(automation(switch_lights_lamp()), AGENT)
        .await
        .unwrap();
    let run = autos.run_now(&a.id, true, None, AGENT).await.unwrap();
    assert!(run.dry);
    assert_eq!(run.steps.len(), 1);
    assert!(!lamp_turns(&hub, true).await);
}

#[tokio::test]
async fn held_for_waits_and_gives_up_if_it_changes_back() {
    let (hub, autos, _c) = setup().await;
    let mut graph = switch_lights_lamp();
    graph["nodes"][0]["for_s"] = serde_json::json!(1);
    approved(&autos, graph).await;

    flip_switch(&hub, true).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    flip_switch(&hub, false).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        hub.state(&PointId::from("fake:lamp/on")).unwrap().value,
        Value::Bool(false),
        "released too early"
    );

    flip_switch(&hub, true).await;
    assert!(lamp_turns(&hub, true).await, "held one second: fires");
}

#[tokio::test]
async fn switching_off_cancels_the_queue() {
    let (hub, autos, _c) = setup().await;
    let mut a = automation(serde_json::json!({
        "nodes": [
            { "id": "t", "type": "when_state", "point": "fake:switch/on" },
            { "id": "w", "type": "wait", "seconds": 1 },
            { "id": "s", "type": "set", "point": "fake:lamp/on", "value": true }
        ],
        "edges": [{ "from": "t", "to": "w" }, { "from": "w", "to": "s" }]
    }));
    a.mode = Mode::Queued;
    let a = autos.save(a, HUMAN).await.unwrap();
    let a = autos.approve(&a.id, &a.fingerprint(), HUMAN).await.unwrap();
    for on in [true, false, true] {
        flip_switch(&hub, on).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    autos.set_enabled(&a.id, false, None, AGENT).await.unwrap();
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert_eq!(
        hub.state(&PointId::from("fake:lamp/on")).unwrap().value,
        Value::Bool(false),
        "no queued run survives switching off"
    );
}

#[tokio::test]
async fn a_changed_graph_needs_a_new_approval() {
    let (_hub, autos, _c) = setup().await;
    let mut a = approved(&autos, switch_lights_lamp()).await;
    a.graph.nodes[1].step = Step::Set {
        point: "fake:lamp/on".into(),
        value: serde_json::json!(false),
    };
    let a = autos.save(a, AGENT).await.unwrap();
    assert!(!a.is_live(), "an agent's edit is not approved");
    let checked = autos.check(&a.graph);
    assert!(checked.ok(), "{:?}", checked.problems);
    assert!(a.approved_version.is_some(), "the approved version is kept");
    let back = autos.restore(&a.id, HUMAN).await.unwrap();
    assert!(back.is_live(), "back to the version the person approved");
}

#[tokio::test]
async fn an_agent_switching_it_off_ends_its_runs_and_is_journaled() {
    let (hub, autos, _c) = setup().await;
    let graph = serde_json::json!({
        "nodes": [
            { "id": "t", "type": "when_state", "point": "fake:switch/on", "to": true },
            { "id": "w", "type": "wait", "seconds": 600 },
            { "id": "s", "type": "set", "point": "fake:lamp/on", "value": true }
        ],
        "edges": [{ "from": "t", "to": "w" }, { "from": "w", "to": "s" }]
    });
    let a = autos.save(automation(graph), HUMAN).await.unwrap();
    let a = autos.approve(&a.id, &a.fingerprint(), HUMAN).await.unwrap();
    flip_switch(&hub, true).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        autos.runs(Some(&a.id), 1).is_empty(),
        "still waiting (finished runs only)"
    );

    // Through a plain save (PUT), not set_enabled: off must still mean off.
    let mut off = a.clone();
    off.enabled = false;
    let saved = autos.save(off, AGENT).await.unwrap();
    assert!(!saved.enabled);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let runs = autos.runs(Some(&a.id), 1);
    assert_eq!(runs.len(), 1, "the waiting run ended");
    assert_eq!(runs[0].status, RunStatus::Cancelled);
    assert!(!lamp_turns(&hub, true).await, "and never acted");

    let changes: Vec<_> = hub
        .journal(20)
        .iter()
        .filter_map(|e| match &e.action {
            moli_core::Action::Automation { id, change, .. } if *id == a.id => {
                Some((e.origin, *change))
            }
            _ => None,
        })
        .collect();
    // Newest first, as the journal is read.
    assert_eq!(
        changes,
        vec![
            (Origin::Api, moli_core::AutomationChange::Updated),
            (Origin::Ui, moli_core::AutomationChange::Approved),
            (Origin::Ui, moli_core::AutomationChange::Created),
        ]
    );
}

#[tokio::test]
async fn said_aloud_through_every_speaker() {
    let (hub, autos, _c) = setup().await;
    assert!(autos.abilities().voice, "the fake speaker can announce");
    let graph = serde_json::json!({
        "nodes": [
            { "id": "t", "type": "when_state", "point": "fake:switch/on", "to": true },
            { "id": "n", "type": "notify", "message": "Quelqu'un est à la porte", "channels": ["voix"] }
        ],
        "edges": [{ "from": "t", "to": "n" }]
    });
    let a = autos.save(automation(graph), HUMAN).await.unwrap();
    assert!(
        autos.check(&a.graph).summary.contains("à voix haute"),
        "{}",
        autos.check(&a.graph).summary
    );
    autos.approve(&a.id, &a.fingerprint(), HUMAN).await.unwrap();
    flip_switch(&hub, true).await;
    let said = PointId::from("fake:speaker/announce");
    for _ in 0..100 {
        if hub.state(&said).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        hub.state(&said).map(|s| s.value),
        Some(Value::Text("Quelqu'un est à la porte".into()))
    );
}
