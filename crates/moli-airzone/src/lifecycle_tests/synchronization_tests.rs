use super::*;

const ZONE: &str = "az:aabbccddeeff_s1_z2";
const GATEWAY: &str = "az:aabbccddeeff_gateway";

async fn until(mut condition: impl FnMut() -> bool, seconds: u64) {
    tokio::time::timeout(Duration::from_secs(seconds), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

fn generation(hub: &Hub, device: &str) -> i64 {
    match hub.state(&point(device, "sync_generation")).unwrap().value {
        Value::Int(value) => value,
        other => panic!("invalid generation {other:?}"),
    }
}

async fn completed_cycle(hub: &Hub) {
    until(
        || {
            hub.state(&point(GATEWAY, "sync_in_progress"))
                .is_none_or(|sample| sample.value == Value::Bool(false))
        },
        2,
    )
    .await;
}

#[tokio::test]
async fn external_temperature_is_published_before_optional_metadata_finishes() {
    let (hub, cancel, sim, server) = start().await;
    completed_cycle(&hub).await;
    let p = point(ZONE, "temperature");
    let before = sim.optional_started.load(Ordering::SeqCst);
    *sim.optional_delay.lock().unwrap() = 6000;
    sim.zones.lock().unwrap()["systems"][0]["data"][0]["roomTemp"] = json!(24.7);
    until(|| sim.optional_started.load(Ordering::SeqCst) > before, 7).await;
    // Entering /version proves the preceding required /hvac response arrived.
    let received = Instant::now();
    until(
        || {
            hub.state(&p)
                .is_some_and(|sample| sample.value == Value::Float(24.7))
        },
        6,
    )
    .await;
    let delayed = received.elapsed();
    cancel.cancel();
    server.abort();
    assert!(
        delayed < Duration::from_millis(300),
        "core data was withheld behind optional metadata for {delayed:?}"
    );
}

#[tokio::test]
async fn unchanged_polls_emit_heartbeats_without_fabricating_temperature_changes() {
    let (hub, cancel, sim, server) = start().await;
    completed_cycle(&hub).await;
    let initial = generation(&hub, ZONE);
    let temperature = point(ZONE, "temperature");
    let before = hub.state(&temperature).unwrap();
    let mut events = hub.subscribe();
    until(|| generation(&hub, ZONE) >= initial + 2, 12).await;
    let after = hub.state(&temperature).unwrap();
    assert_eq!(before.value, after.value);
    assert!(after.ts > before.ts);
    let heartbeat = point(ZONE, "sync_generation");
    let mut heartbeats = 0;
    while let Ok(event) = events.try_recv() {
        if let Event::State { point, .. } = event {
            assert_ne!(
                point, temperature,
                "unchanged equipment values are not changes"
            );
            if point == heartbeat {
                heartbeats += 1;
            }
        }
    }
    assert!(
        heartbeats >= 2,
        "each successful unchanged poll reaches SSE clients"
    );
    assert!(sim.writes.lock().unwrap().is_empty());
    cancel.cancel();
    server.abort();
}

#[tokio::test]
async fn external_changes_continue_for_three_cycles_without_commands_or_reload() {
    let (hub, cancel, sim, server) = start().await;
    let temperature = point(ZONE, "temperature");
    let mut events = hub.subscribe();
    for value in [24.7, 19.2, 23.1] {
        sim.zones.lock().unwrap()["systems"][0]["data"][0]["roomTemp"] = json!(value);
        until(
            || {
                hub.state(&temperature)
                    .is_some_and(|s| s.value == Value::Float(value))
            },
            7,
        )
        .await;
        let mut propagated = false;
        while let Ok(event) = events.try_recv() {
            if let Event::State {
                point,
                value: actual,
                ..
            } = event
            {
                propagated |= point == temperature && actual == Value::Float(value);
            }
        }
        assert!(
            propagated,
            "external observation must propagate through SSE"
        );
    }
    assert!(sim.writes.lock().unwrap().is_empty());
    cancel.cancel();
    server.abort();
}

#[tokio::test]
async fn authentication_failure_keeps_old_receipt_offline_then_recovers_automatically() {
    let (hub, cancel, sim, server) = start().await;
    completed_cycle(&hub).await;
    let received = point(ZONE, "sync_last_received");
    let before = hub.state(&received).unwrap();
    let old_generation = generation(&hub, ZONE);
    sim.response_status.store(401, Ordering::SeqCst);
    until(
        || {
            hub.snapshot()
                .drivers
                .iter()
                .any(|d| matches!(d.status, DriverStatus::Backoff { .. }))
        },
        7,
    )
    .await;
    assert_eq!(
        hub.device(&DeviceId::from(ZONE)).unwrap().online,
        Some(false)
    );
    assert_eq!(hub.state(&received).unwrap(), before);
    assert_eq!(generation(&hub, ZONE), old_generation);
    assert_eq!(
        hub.state(&point(GATEWAY, "sync_last_error")).unwrap().value,
        Value::from("http_401")
    );
    assert_eq!(
        hub.state(&point(GATEWAY, "sync_in_progress"))
            .unwrap()
            .value,
        Value::Bool(false)
    );
    sim.zones.lock().unwrap()["systems"][0]["data"][0]["roomTemp"] = json!(18.6);
    sim.response_status.store(200, Ordering::SeqCst);
    until(
        || {
            hub.device(&DeviceId::from(ZONE))
                .is_some_and(|d| d.online == Some(true))
                && hub
                    .state(&point(ZONE, "temperature"))
                    .is_some_and(|s| s.value == Value::Float(18.6))
        },
        5,
    )
    .await;
    assert!(generation(&hub, ZONE) > old_generation);
    assert_eq!(
        hub.state(&point(GATEWAY, "sync_last_error")).unwrap().value,
        Value::from("")
    );
    assert!(sim.writes.lock().unwrap().is_empty());
    cancel.cancel();
    server.abort();
}

#[tokio::test]
async fn missing_field_is_removed_and_missing_zone_does_not_receive_a_fresh_heartbeat() {
    let (hub, cancel, sim, server) = start().await;
    let other = "az:aabbccddeeff_s1_z8";
    let humidity = point(other, "humidity");
    assert_eq!(hub.state(&humidity).unwrap().value, Value::Int(42));
    let receipt = hub.state(&point(ZONE, "sync_last_received")).unwrap();
    let other_generation = generation(&hub, other);
    {
        let mut data = sim.zones.lock().unwrap();
        data["systems"][0]["data"][1]
            .as_object_mut()
            .unwrap()
            .remove("humidity");
        data["systems"][0]["data"].as_array_mut().unwrap().remove(0);
    }
    until(|| generation(&hub, other) > other_generation, 7).await;
    assert!(hub.state(&humidity).is_none());
    assert!(
        hub.device(&DeviceId::from(other))
            .unwrap()
            .device
            .point("humidity")
            .is_none()
    );
    assert_eq!(
        hub.device(&DeviceId::from(ZONE)).unwrap().online,
        Some(false)
    );
    assert_eq!(
        hub.state(&point(ZONE, "sync_last_received")).unwrap(),
        receipt
    );
    assert!(sim.writes.lock().unwrap().is_empty());
    cancel.cancel();
    server.abort();
}

#[tokio::test]
async fn command_ack_has_a_readback_barrier_and_never_fabricates_equipment_state() {
    let (hub, cancel, sim, server) = start().await;
    completed_cycle(&hub).await;
    let target = point(ZONE, "target_temperature");
    let original = hub.state(&target).unwrap().value;
    hub.command(&target, Value::Float(23.5), Origin::Ui, None)
        .await
        .unwrap();
    let marker = match hub
        .state(&point(ZONE, "sync_last_command_generation"))
        .unwrap()
        .value
    {
        Value::Int(value) => value,
        other => panic!("missing ACK barrier {other:?}"),
    };
    assert_eq!(hub.state(&target).unwrap().value, original);
    until(|| generation(&hub, ZONE) > marker, 2).await;
    assert_eq!(
        hub.state(&target).unwrap().value,
        original,
        "gateway ACK without changed equipment must remain unconfirmed"
    );
    assert_eq!(
        sim.writes.lock().unwrap().as_slice(),
        &[json!({"systemID":1,"zoneID":2,"setpoint":23.5})]
    );
    cancel.cancel();
    server.abort();
}

#[tokio::test]
async fn core_heartbeat_does_not_freshen_gateway_fields_waiting_for_optional_metadata() {
    let (hub, cancel, sim, server) = start().await;
    completed_cycle(&hub).await;
    let connected = point(GATEWAY, "cloud_connected");
    assert_eq!(hub.state(&connected).unwrap().value, Value::Bool(true));
    let before = hub.state(&point(GATEWAY, "sync_last_received")).unwrap();
    let generation_before = generation(&hub, GATEWAY);
    let optional_before = sim.optional_started.load(Ordering::SeqCst);
    *sim.optional_delay.lock().unwrap() = 6000;
    *sim.webserver_extra.lock().unwrap() = json!({});
    until(
        || sim.optional_started.load(Ordering::SeqCst) > optional_before,
        7,
    )
    .await;
    assert_eq!(
        hub.state(&point(GATEWAY, "sync_last_received"))
            .unwrap()
            .value,
        before.value,
        "core success cannot freshen fields not yet published"
    );
    assert_eq!(generation(&hub, GATEWAY), generation_before);
    assert_eq!(hub.state(&connected).unwrap().value, Value::Bool(true));
    until(|| generation(&hub, GATEWAY) > generation_before, 6).await;
    assert!(
        hub.state(&connected).is_none(),
        "absent field must be pruned"
    );
    assert!(hub.state(&point(GATEWAY, "sync_last_received")).unwrap().ts > before.ts);
    cancel.cancel();
    server.abort();
}

#[tokio::test]
async fn setpoint_and_turn_on_uses_one_guarded_put_and_no_fabricated_state() {
    let (hub, cancel, sim, server) = start().await;
    let target = point(ZONE, "target_temperature_on");
    let original = hub.state(&point(ZONE, "target_temperature")).unwrap().value;
    hub.command(&target, Value::Float(23.5), Origin::Ui, None)
        .await
        .unwrap();
    assert_eq!(
        sim.writes.lock().unwrap().as_slice(),
        &[json!({"systemID":1,"zoneID":2,"setpoint":23.5,"on":1})]
    );
    assert_eq!(
        hub.state(&point(ZONE, "target_temperature")).unwrap().value,
        original
    );
    assert!(
        hub.state(&target).is_none(),
        "write-only action has no fabricated report"
    );
    assert!(
        hub.command(&target, Value::Float(23.3), Origin::Ui, None)
            .await
            .is_err()
    );
    assert_eq!(sim.writes.lock().unwrap().len(), 1);
    sim.zones.lock().unwrap()["systems"][0]["data"][0]
        .as_object_mut()
        .unwrap()
        .remove("on");
    assert!(
        hub.command(&target, Value::Float(24.), Origin::Ui, None)
            .await
            .is_err()
    );
    assert_eq!(
        sim.writes.lock().unwrap().len(),
        1,
        "fresh on capability must be present"
    );
    cancel.cancel();
    server.abort();
}

#[tokio::test]
async fn composed_action_keeps_the_zone_guard_and_read_only_boundary() {
    for read_only in [false, true] {
        let zones = serde_json::from_str(include_str!(
            "../../../../integrations/airzone/fixtures/zones.json"
        ))
        .unwrap();
        let hub = Hub::new(HubOptions {
            guard: moli_runtime::guard::GuardPolicy {
                protect_unassigned: true,
                ..Default::default()
            },
            ..HubOptions::default()
        })
        .unwrap();
        let (hub, cancel, sim, server) = start_with_settings(zones, hub, read_only).await;
        let result = hub
            .command(
                &point(ZONE, "target_temperature_on"),
                Value::Float(23.5),
                Origin::Assistant,
                None,
            )
            .await;
        if read_only {
            assert_eq!(result, Err(moli_runtime::CommandError::ReadOnly));
        } else {
            assert!(matches!(
                result,
                Err(moli_runtime::CommandError::NeedsApproval { .. })
            ));
        }
        assert!(sim.writes.lock().unwrap().is_empty());
        cancel.cancel();
        server.abort();
    }
}

#[tokio::test]
async fn a_cancelled_optional_snapshot_cannot_restore_pre_command_temperature() {
    let (hub, cancel, sim, server) = start().await;
    completed_cycle(&hub).await;
    *sim.optional_delay.lock().unwrap() = 6000;
    let optional_before = sim.optional_started.load(Ordering::SeqCst);
    until(
        || sim.optional_started.load(Ordering::SeqCst) > optional_before,
        7,
    )
    .await;
    sim.zones.lock().unwrap()["systems"][0]["data"][0]["roomTemp"] = json!(24.7);
    let started = Instant::now();
    hub.command(&point(ZONE, "on"), Value::Bool(false), Origin::Ui, None)
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_millis(1500));
    let temperature = point(ZONE, "temperature");
    assert_eq!(hub.state(&temperature).unwrap().value, Value::Float(24.7));
    let mut events = hub.subscribe();
    let expected = generation(&hub, ZONE) + 2;
    until(|| generation(&hub, ZONE) >= expected, 12).await;
    while let Ok(event) = events.try_recv() {
        if let Event::State { point, value, .. } = event
            && point == temperature
        {
            assert_eq!(value, Value::Float(24.7));
        }
    }
    assert_eq!(hub.state(&temperature).unwrap().value, Value::Float(24.7));
    cancel.cancel();
    server.abort();
}

#[tokio::test]
async fn slow_iaq_dependencies_keep_core_cadence_and_skipped_resource_receipts() {
    let app = Router::new()
        .route(
            "/api/v1/webserver",
            any(|| async { Reply(json!({"mac":"aa:bb:cc:dd:ee:ff"})) }),
        )
        .route(
            "/api/v1/hvac",
            any(|Reply(body): Reply<Json>| async move {
                let response = if body.get("zoneID").is_some() {
                    json!({"data":[{"systemID":1,"zoneID":1,"roomTemp":20.}]})
                } else if body["systemID"] == 127 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    json!({"systems":[{"systemID":1,"num_airqsensor":1}]})
                } else {
                    assert_eq!(body, json!({"systemID":0}));
                    json!({"data":{"systemID":0,"acs_temp":45.3}})
                };
                Reply(response)
            }),
        )
        .route(
            "/api/v1/iaq",
            any(|Reply(body): Reply<Json>| async move {
                assert_eq!(body, json!({"systemID":1,"iaqsensorID":0}));
                tokio::time::sleep(Duration::from_secs(3)).await;
                Reply(json!({"data":[{"systemID":1,"iaqsensorID":1,"co2_value":411}]}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config: Config = serde_json::from_value(json!({"host":"127.0.0.1","port":listener.local_addr().unwrap().port(),"poll_secs":5,"read_only":true})).unwrap();
    let server = tokio::spawn(async {
        axum::serve(listener, app).await.unwrap();
    });
    let hub = Hub::new(HubOptions::default()).unwrap();
    let cancel = CancellationToken::new();
    spawn_driver(
        &hub,
        "az".into(),
        Arc::new(Airzone::new(config).unwrap()),
        cancel.clone(),
    );
    let zone = "az:aabbccddeeff_s1_z1";
    until(|| hub.state(&point(zone, "sync_generation")).is_some(), 1).await;
    for expected in 2..=4 {
        let started = Instant::now();
        until(|| generation(&hub, zone) >= expected, 6).await;
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "optional requests must not pause core polling"
        );
    }
    let sensor = "az:aabbccddeeff_s1_iaq1";
    until(|| hub.state(&point(sensor, "co2_value")).is_some(), 6).await;
    assert_eq!(
        hub.state(&point(sensor, "co2_value"))
            .unwrap()
            .value
            .as_f64(),
        Some(411.)
    );
    assert_eq!(
        hub.state(&point(sensor, "sync_poll_interval"))
            .unwrap()
            .value,
        Value::Int(25)
    );
    assert_eq!(
        hub.state(&point(zone, "sync_poll_interval")).unwrap().value,
        Value::Int(5)
    );
    let dhw = "az:aabbccddeeff_dhw";
    let previous = hub.state(&point(dhw, "sync_last_received")).unwrap();
    until(|| generation(&hub, GATEWAY) >= 5, 6).await;
    assert_eq!(hub.device(&DeviceId::from(dhw)).unwrap().online, Some(true));
    assert_eq!(
        hub.state(&point(dhw, "sync_last_received")).unwrap(),
        previous,
        "skipped family must not become offline or acquire a new receipt"
    );
    cancel.cancel();
    server.abort();
}
