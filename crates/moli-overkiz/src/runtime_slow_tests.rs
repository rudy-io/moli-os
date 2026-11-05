use super::*;
use serde_json::{Value as Json, json};

fn fixture() -> Json {
    let cover: Json = serde_json::from_str(include_str!(
        "../../../integrations/overkiz/fixtures/slow-cover.json"
    ))
    .unwrap();
    json!([cover])
}

#[tokio::test]
async fn slow_actions_dispatch_once_without_inventing_position_or_speed() {
    let rig = Rig::start(fixture(), vec![], Duration::from_secs(5)).await;
    let position = rig.point(0, "percent_control");
    for (index, (key, value, native, parameters)) in [
        (
            "control",
            Value::from("open_slow"),
            "setClosureAndLinearSpeed",
            json!([0.0, "lowspeed"]),
        ),
        (
            "control",
            Value::from("close_slow"),
            "setClosureAndLinearSpeed",
            json!([100.0, "lowspeed"]),
        ),
        (
            "percent_control_slow",
            Value::Float(35.0),
            "setClosureAndLinearSpeed",
            json!([65.0, "lowspeed"]),
        ),
        ("control", Value::from("stop"), "stop", json!([])),
    ]
    .into_iter()
    .enumerate()
    {
        rig.hub
            .command(&rig.point(0, key), value, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(rig.posts.load(Ordering::SeqCst), index + 1);
        let body = rig.payloads.lock().unwrap()[index].clone();
        assert_eq!(
            body,
            json!({"label":"Moli","actions":[{"deviceURL":"io://example/slow-cover","commands":[{"name":native,"parameters":parameters}]}]})
        );
        assert_eq!(rig.hub.state(&position).unwrap().value, Value::Float(75.0));
        assert!(
            rig.hub
                .state(&rig.point(0, "percent_control_slow"))
                .is_none()
        );
    }
}

#[tokio::test]
async fn changed_slow_signature_class_or_availability_denies_previously_advertised_actions() {
    for mutation in ["signature", "missing", "class", "offline"] {
        let rig = Rig::start(fixture(), vec![], Duration::from_secs(5)).await;
        {
            let mut raw = rig.snapshot.lock().unwrap();
            match mutation {
                "signature" => raw[0]["definition"]["commands"][4]["paramsSig"] = json!("p1,p2"),
                "missing" => {
                    raw[0]["definition"]["commands"]
                        .as_array_mut()
                        .unwrap()
                        .pop();
                }
                "class" => raw[0]["definition"]["uiClass"] = json!("Window"),
                _ => raw[0]["available"] = json!(false),
            }
        }
        assert!(
            rig.hub
                .command(
                    &rig.point(0, "control"),
                    Value::from("open_slow"),
                    Origin::Ui,
                    None
                )
                .await
                .is_err()
        );
        assert_eq!(rig.posts.load(Ordering::SeqCst), 0, "{mutation}");
        assert!(
            rig.gets.load(Ordering::SeqCst) >= 2,
            "fresh definition was read"
        );
    }
}

#[tokio::test]
async fn readonly_slow_commands_are_refused_for_all_origins_even_with_stale_writable_metadata() {
    for stale in [false, true] {
        let rig =
            Rig::start_mode(fixture(), vec![], Duration::from_secs(5), true, stale, 300).await;
        let gets = rig.gets.load(Ordering::SeqCst);
        for origin in [
            Origin::Ui,
            Origin::Household,
            Origin::Api,
            Origin::Mcp,
            Origin::Cli,
            Origin::System,
            Origin::Assistant,
            Origin::Automation,
        ] {
            for (key, value) in [
                ("control", Value::from("open_slow")),
                ("control", Value::from("close_slow")),
                ("percent_control_slow", Value::Float(35.0)),
            ] {
                assert!(
                    rig.hub
                        .command(&rig.point(0, key), value, origin, None)
                        .await
                        .is_err(),
                    "{stale} {origin:?}"
                );
            }
        }
        assert_eq!(rig.posts.load(Ordering::SeqCst), 0);
        assert_eq!(
            rig.gets.load(Ordering::SeqCst),
            gets,
            "read-only denies before preflight"
        );
    }
}

#[tokio::test]
async fn slow_commands_preserve_protected_room_guards_and_single_approved_dispatch() {
    let rig = Rig::start(fixture(), vec![], Duration::from_secs(5)).await;
    let point = rig.point(0, "percent_control_slow");
    rig.hub
        .set_label(
            &point.split().unwrap().0,
            moli_runtime::LabelPatch {
                name: None,
                room: Some("Protected slow fixture".into()),
            },
            Origin::Ui,
            None,
        )
        .await
        .unwrap();
    let mut pending = None;
    for origin in [
        Origin::Api,
        Origin::Mcp,
        Origin::Cli,
        Origin::Assistant,
        Origin::Household,
    ] {
        for (key, value) in [
            ("control", Value::from("open_slow")),
            ("control", Value::from("close_slow")),
            ("percent_control_slow", Value::Float(35.0)),
        ] {
            let point = rig.point(0, key);
            let result = rig.hub.command(&point, value, origin, None).await;
            let Err(moli_runtime::CommandError::NeedsApproval { id, .. }) = result else {
                panic!("unguarded slow action: {origin:?} {result:?}")
            };
            pending = Some((id, point));
        }
    }
    assert_eq!(rig.posts.load(Ordering::SeqCst), 0);
    let (id, point) = pending.unwrap();
    rig.hub
        .resolve_approval(id, &point, true, Origin::Ui, None)
        .await
        .unwrap();
    assert_eq!(rig.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn slow_preflight_deadline_or_cancellation_never_posts() {
    for cancel_caller in [false, true] {
        let timeout = if cancel_caller {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(200)
        };
        let rig = Rig::start(fixture(), vec![], timeout).await;
        rig.preflight_delay.store(500, Ordering::SeqCst);
        let hub = rig.hub.clone();
        let point = rig.point(0, "percent_control_slow");
        let command = tokio::spawn(async move {
            hub.command(&point, Value::Float(35.0), Origin::Ui, None)
                .await
        });
        while rig.gets.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
        if cancel_caller {
            command.abort();
        } else {
            assert!(command.await.unwrap().is_err());
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(rig.posts.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn refused_slow_apply_is_not_replayed() {
    let rig = Rig::start(fixture(), vec![], Duration::from_secs(5)).await;
    rig.post_status.store(500, Ordering::SeqCst);
    assert!(
        rig.hub
            .command(
                &rig.point(0, "control"),
                Value::from("close_slow"),
                Origin::Ui,
                None
            )
            .await
            .is_err()
    );
    assert_eq!(rig.posts.load(Ordering::SeqCst), 1);
    assert_eq!(rig.payloads.lock().unwrap().len(), 1);
}
