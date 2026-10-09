//! The house's language reaches the engine's words. The language is global to
//! the process, so this lives in a binary of its own: one test, in order.

use std::sync::Arc;
use std::time::Duration;

use moli_automation::{
    Abilities, Actor, Author, Automation, Automations, Config, Graph, Level, Mode, Problem,
    StepStatus,
};
use moli_core::{Access, Device, InstanceId, Kind, PointId, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx, Hub, HubOptions, spawn_driver};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Fake;

fn point(
    key: &str,
    label: &str,
    kind: Kind,
    write: bool,
    unit: Option<Unit>,
    semantic: Semantic,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read: true, write },
        unit,
        semantic,
    }
}

fn device(ctx: &DriverCtx, name: &str, points: Vec<PointSpec>) -> Device {
    Device {
        id: ctx.device_id(name),
        instance: ctx.instance().clone(),
        native_name: name.into(),
        manufacturer: None,
        model: None,
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

impl Driver for Fake {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let lamp = point("on", "On", Kind::Binary, true, None, Semantic::OnOff);
            let contact = point(
                "contact",
                "Contact",
                Kind::Binary,
                false,
                None,
                Semantic::Other,
            );
            let level = Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: None,
            };
            let dial = point(
                "level",
                "Level",
                level,
                true,
                Some(Unit::Percent),
                Semantic::Other,
            );
            let thermo = Kind::Numeric {
                min: None,
                max: None,
                step: None,
            };
            let room = point(
                "temperature",
                "Temperature",
                thermo,
                false,
                Some(Unit::Celsius),
                Semantic::Other,
            );
            for (name, points) in [
                ("lamp", vec![lamp]),
                ("door", vec![contact]),
                ("dial", vec![dial]),
                ("room", vec![room]),
            ] {
                let d = device(ctx, name, points);
                ctx.upsert_device(d);
            }
            ctx.set_state(&ctx.device_id("lamp"), "on", Value::Bool(false));
            ctx.ready();
            while let Some(cmd) = ctx.next_command().await {
                cmd.reply(Ok(()));
            }
            Ok(())
        })
    }
}

fn graph(json: serde_json::Value) -> Graph {
    serde_json::from_value(json).unwrap()
}

/// A trigger, then one step.
fn simple(trigger: serde_json::Value, step: serde_json::Value) -> serde_json::Value {
    let (mut t, mut s) = (trigger, step);
    t["id"] = json!("t");
    s["id"] = json!("s");
    json!({ "nodes": [t, s], "edges": [{ "from": "t", "to": "s" }] })
}

fn lamp_on() -> serde_json::Value {
    json!({ "type": "set", "point": "fake:lamp/on", "value": true })
}

// One language switch at a time: the whole story is one test.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn the_engine_speaks_the_language_of_the_house() {
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
    let can = Abilities::default();
    let say = |g: serde_json::Value| autos.check_with(&graph(g), can).summary;

    let door_closes_for_a_while = simple(
        json!({ "type": "when_state", "point": "fake:door/contact", "to": true, "for_s": 90 }),
        lamp_on(),
    );
    let at_sunset = simple(
        json!({ "type": "at_sun", "event": "set", "offset_min": -30, "days": [1, 3, 5] }),
        lamp_on(),
    );
    let weekday_mornings = simple(
        json!({ "type": "at_time", "at": "07:30", "days": [1, 2, 3, 4, 5] }),
        json!({ "type": "set", "point": "fake:dial/level", "value": 50.5 }),
    );
    let hot = simple(
        json!({ "type": "when_threshold", "point": "fake:room/temperature", "above": 26.5, "for_s": 600 }),
        json!({ "type": "notify", "message": "Hot", "channels": ["maison", "voix", "telegram"] }),
    );
    let evening = json!({
        "nodes": [
            { "id": "t", "type": "when_state", "point": "fake:door/contact", "to": false },
            { "id": "c", "type": "if", "rules": [
                { "kind": "state", "point": "fake:lamp/on", "value": false },
                { "kind": "time", "after": "22:00", "before": "06:00" } ] },
            { "id": "y", "type": "set", "point": "fake:lamp/on", "value": true },
            { "id": "w", "type": "wait", "seconds": 5400 },
            { "id": "n", "type": "notify", "message": "No", "channels": ["maison"] }
        ],
        "edges": [
            { "from": "t", "to": "c" }, { "from": "c", "port": "yes", "to": "y" },
            { "from": "y", "to": "w" }, { "from": "c", "port": "no", "to": "n" }
        ]
    });

    assert!(moli_i18n::set_language("en"));
    assert_eq!(
        say(door_closes_for_a_while.clone()),
        "When door closes and stays that way for 1 min 30 s, turn on lamp."
    );
    assert_eq!(
        say(at_sunset),
        "30 min before sunset on Monday, Wednesday and Friday, turn on lamp."
    );
    assert_eq!(
        say(weekday_mornings),
        "On weekdays at 07:30, set the level of dial to 50.5 %."
    );
    assert_eq!(
        say(hot),
        "When the temperature of room goes above 26.5 °C for 10 min, \
         notify the house, out loud and Telegram: “Hot”."
    );
    assert_eq!(
        say(evening.clone()),
        "When door opens, if lamp is off and it is between 22:00 and 06:00: \
         turn on lamp, then wait 1 h 30 min; otherwise: notify the house: “No”."
    );

    // Problems, in words; and the one the assistant looks for by kind.
    let broken = graph(json!({
        "nodes": [
            { "id": "t", "type": "at_time", "at": "7h" },
            { "id": "x", "type": "set", "point": "fake:door/contact", "value": true },
            { "id": "z", "type": "wait", "seconds": 5 }
        ],
        "edges": [{ "from": "t", "to": "x" }]
    }));
    let check = autos.check_with(&broken, can);
    let said: Vec<String> = check.problems.iter().map(|p| p.message.clone()).collect();
    assert_eq!(
        said,
        [
            "Time “7h” is not readable (use the format 07:30).",
            "“Contact” cannot be controlled, it can only be read.",
            "Never reached: no trigger leads here.",
        ]
    );
    assert!(!check.ok());
    assert!(check.problems.iter().any(Problem::is_unreached));
    assert_eq!(
        check
            .problems
            .iter()
            .filter(|p| p.level == Level::Error)
            .count(),
        2
    );

    // A dry run says why and what it would do.
    let manual = json!({
        "nodes": [
            { "id": "t", "type": "manual" },
            { "id": "w", "type": "wait", "seconds": 90 },
            { "id": "n", "type": "notify", "message": "Level {{fake:dial/level}} / {{fake:room/temperature}}", "channels": ["maison"] }
        ],
        "edges": [{ "from": "t", "to": "w" }, { "from": "w", "to": "n" }]
    });
    let saved = autos
        .save(
            Automation {
                id: String::new(),
                name: "Probe".into(),
                enabled: false,
                mode: Mode::Restart,
                graph: graph(manual),
                author: Author::Human,
                approved: None,
                approved_version: None,
                note: None,
                created: 0,
                updated: 0,
            },
            Actor {
                human: true,
                author: Author::Human,
            },
        )
        .await
        .unwrap();
    let run = autos
        .run_now(
            &saved.id,
            true,
            None,
            Actor {
                human: false,
                author: Author::Agent,
            },
        )
        .await
        .unwrap();
    assert_eq!(run.why, "Dry run");
    assert_eq!(run.steps[0].status, StepStatus::Simulated);
    assert_eq!(run.steps[0].detail.as_deref(), Some("would wait 90 s"));
    assert_eq!(run.steps[1].detail.as_deref(), Some("Level ? / ?"));

    // Who did it, as the journal and the notices say it.
    let who = |human, author| Actor { human, author }.who();
    assert_eq!(who(true, Author::Human), "a person (house code)");
    assert_eq!(who(false, Author::Assistant), "Moli");
    assert_eq!(who(false, Author::Agent), "an agent");
    assert_eq!(who(false, Author::Import), "the Home Assistant import");

    // Back to French: the same graphs, the same sentences as always.
    assert!(moli_i18n::set_language("fr"));
    assert_eq!(
        say(door_closes_for_a_while),
        "Quand door se ferme depuis 1 min 30 s, allumer lamp."
    );
    assert_eq!(
        say(evening),
        "Quand door s'ouvre, si lamp est éteint et on est entre 22:00 et 06:00 : \
         allumer lamp, puis attendre 1 h 30 ; sinon : prévenir la maison : « No »."
    );
    assert_eq!(who(true, Author::Human), "une personne (code)");
    cancel.cancel();
}
