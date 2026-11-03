use super::*;
use serde_json::json;

#[test]
fn system_metadata_accepts_object_array_and_broadcast_without_inventing_zone() {
    let metadata = json!({"systemID":2,"system_firmware":"5.34"});
    for envelope in [
        json!({"data":metadata}),
        json!({"data":[metadata]}),
        json!({"systems":[metadata]}),
        json!({"systems":[{"data":[metadata]}]}),
    ] {
        let parsed = records(&envelope, Family::System).unwrap();
        assert_eq!(parsed, vec![metadata.clone()]);
        assert!(parsed[0].get("zoneID").is_none());
    }
}

#[test]
fn erv_system_record_preserves_its_explicit_zone_routing() {
    let record = json!({"systemID":1,"zoneID":2,"erv_modes":[1,2,6],"erv_mode":6});
    assert_eq!(
        records(&json!({"data":record}), Family::System).unwrap(),
        vec![record]
    );
}

#[test]
fn resource_identity_errors_are_not_silently_skipped() {
    for envelope in [
        json!({"data":{"systemID":0}}),
        json!({"data":{"systemID":33}}),
        json!({"data":{"systemID":1,"zoneID":0}}),
        json!({"data":[{"systemID":1},{"systemID":1}]}),
        json!({"systems":[{"systemID":1,"data":[{"systemID":2}]}]}),
        json!({"data":null}),
    ] {
        assert!(records(&envelope, Family::System).is_err(), "{envelope}");
    }
}

#[test]
fn dhw_zero_id_is_a_specific_resource_not_a_zone_broadcast() {
    assert_eq!(
        records(&json!({"data":{"systemID":0,"acs_temp":45.3}}), Family::Dhw)
            .unwrap()
            .len(),
        1
    );
    for record in [
        json!({"systemID":1}),
        json!({"systemID":0,"zoneID":0}),
        json!({"acs_temp":45}),
    ] {
        assert!(records(&json!({"data":record}), Family::Dhw).is_err());
    }
}

#[test]
fn iaq_records_match_requested_system_and_reject_duplicates_or_broadcast_ids() {
    let valid = json!({"data":[{"systemID":2,"iaqsensorID":3,"CO2_Value":411}]});
    let parsed = records(&valid, Family::Iaq(2)).unwrap();
    assert_eq!(parsed[0]["co2_value"], 411);
    for value in [
        json!({"data":[{"systemID":1,"iaqsensorID":3}]}),
        json!({"data":[{"systemID":2,"iaqsensorID":0}]}),
        json!({"data":[{"systemID":2,"iaqsensorID":3},{"systemID":2,"iaqsensorID":3}]}),
        json!({"data":[{"systemID":2,"iaqsensorID":3,"zoneID":1}]}),
    ] {
        assert!(records(&value, Family::Iaq(2)).is_err());
    }
}

#[tokio::test]
async fn optional_sections_publish_real_resources_and_route_addressed_commands() {
    use axum::{Json as Reply, Router, routing::post};
    use std::time::Duration;
    let app = Router::new()
        .route("/api/v1/version", post(|| async { Reply(json!({"version":"1.78"})) }))
        .route("/api/v1/integration", post(|| async { Reply(json!({"driver":"Moli test"})) }))
        .route("/api/v1/hvac", post(|Reply(body): Reply<Json>| async move {
            Reply(match body["systemID"].as_u64().unwrap() {
                127 => json!({"systems":[{"systemID":1,"num_airqsensor":1,"system_firmware":"5.34"},{"systemID":2,"system_type":2}]}),
                0 => json!({"data":{"systemID":0,"acs_temp":45.3,"acs_power":1}}),
                _ => panic!("unexpected query: {body}"),
            })
        }))
        .route("/api/v1/iaq", post(|Reply(body): Reply<Json>| async move {
            assert_eq!(body,json!({"systemID":1,"iaqsensorID":0}));
            Reply(json!({"data":[{"systemID":1,"iaqsensorID":3,"name":"Sensor 03","co2_value":411,"iaq_mode_vent":2}]}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config: Config = serde_json::from_value(
        json!({"host":"127.0.0.1","port":listener.local_addr().unwrap().port(),"read_only":false}),
    )
    .unwrap();
    let task = tokio::spawn(async {
        axum::serve(listener, app).await.unwrap();
    });
    let zones = Snapshot::parse(
        &serde_json::from_str(include_str!(
            "../../../integrations/airzone/fixtures/zones.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let resources = ResourceSnapshot::read(
        &config,
        &zones,
        &json!({"mac":"aa:bb:cc:dd:ee:ff","ws_firmware":"4.21"}),
        Instant::now() + Duration::from_secs(3),
    )
    .await;
    let instance = InstanceId::from("az");
    let entities = resources.entities(&instance, "aabbccddeeff", &zones, false);
    let ids = entities
        .iter()
        .map(|(d, _)| d.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            "az:aabbccddeeff_gateway",
            "az:aabbccddeeff_s1_system",
            "az:aabbccddeeff_s2_system",
            "az:aabbccddeeff_dhw",
            "az:aabbccddeeff_s1_iaq3"
        ]
    );
    let (path, body) = resources
        .encode(
            &instance,
            "aabbccddeeff",
            &zones,
            &DeviceId::from("az:aabbccddeeff_dhw"),
            "acs_power",
            &Value::Bool(false),
        )
        .unwrap();
    assert_eq!(path, "hvac");
    assert_eq!(body, json!({"systemID":0,"acs_power":0}));
    assert!(entities.iter().all(|(d, _)| !d.members.is_empty()));
    task.abort();
}

fn fixture_zones() -> Snapshot {
    Snapshot::parse(
        &serde_json::from_str(include_str!(
            "../../../integrations/airzone/fixtures/zones.json"
        ))
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn unknown_system_metadata_cannot_shrink_installation_command_scope() {
    let zones = fixture_zones();
    let resources = ResourceSnapshot {
        read_only: false,
        integration: Some(json!({"driver":"Moli test"})),
        systems: vec![json!({"systemID":3,"system_type":2})],
        systems_available: true,
        dhw: Some(json!({"systemID":0,"acs_power":1})),
        ..ResourceSnapshot::default()
    };
    let instance = InstanceId::from("az");
    for (device, _) in resources.entities(&instance, "aabbccddeeff", &zones, false) {
        assert!(
            device.points.iter().all(|p| !p.access.write),
            "{} has an unsafe write",
            device.id
        );
    }
    assert!(
        resources
            .encode(
                &instance,
                "aabbccddeeff",
                &zones,
                &DeviceId::from("az:aabbccddeeff_dhw"),
                "acs_power",
                &Value::Bool(false)
            )
            .is_err()
    );
}

#[test]
fn previously_seen_unmapped_system_remains_a_guard_member_after_it_disappears() {
    let zones = fixture_zones();
    let instance = InstanceId::from("az");
    let mut resources = ResourceSnapshot {
        read_only: false,
        integration: Some(json!({"driver":"Moli test"})),
        systems: vec![json!({"systemID":1}), json!({"systemID":2})],
        systems_available: true,
        dhw: Some(json!({"systemID":0,"acs_power":1})),
        ..ResourceSnapshot::default()
    };
    resources.retain_known_scope(
        &instance,
        "aabbccddeeff",
        &zones,
        &[DeviceId::from("az:aabbccddeeff_s3_system")],
    );
    let entities = resources.entities(&instance, "aabbccddeeff", &zones, false);
    assert!(
        !entities
            .iter()
            .find(|(d, _)| d.id.as_str() == "az:aabbccddeeff_dhw")
            .unwrap()
            .0
            .point("acs_power")
            .unwrap()
            .access
            .write
    );
}

#[test]
fn read_only_snapshot_blocks_encode_even_when_caller_requests_writable_entities() {
    let zones = fixture_zones();
    let resources = ResourceSnapshot {
        integration: Some(json!({"driver":"Moli test"})),
        dhw: Some(json!({"systemID":0,"acs_power":1})),
        ..ResourceSnapshot::default()
    };
    let instance = InstanceId::from("az");
    assert!(
        resources
            .entities(&instance, "aabbccddeeff", &zones, false)
            .iter()
            .all(|(d, _)| d.points.iter().all(|p| !p.access.write))
    );
    assert!(
        resources
            .encode(
                &instance,
                "aabbccddeeff",
                &zones,
                &DeviceId::from("az:aabbccddeeff_dhw"),
                "acs_power",
                &Value::Bool(false)
            )
            .is_err()
    );
}

#[test]
fn version_metadata_rejects_conflicting_documented_aliases() {
    assert_eq!(
        metadata(&json!({"schema":"1.78"}), "version").unwrap(),
        json!({"version":"1.78"})
    );
    assert!(metadata(&json!({"version":"1.78","schema":"1.77"}), "version").is_err());
    assert!(metadata(&json!({"driver":"unsafe\ntext"}), "driver").is_err());
}

#[derive(Clone, Default)]
struct OptionalServer {
    phase: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    queries: std::sync::Arc<std::sync::Mutex<Vec<Json>>>,
}

async fn optional_server() -> (Config, OptionalServer, tokio::task::JoinHandle<()>) {
    use axum::{Json as Reply, Router, extract::State, http::StatusCode, routing::post};
    use std::sync::atomic::Ordering;
    let state = OptionalServer::default();
    let app=Router::new()
        .route("/api/v1/version",post(||async { Reply(json!({"version":"1.78"})) }))
        .route("/api/v1/integration",post(|State(s):State<OptionalServer>|async move {
            if s.phase.load(Ordering::SeqCst)==1 { (StatusCode::SERVICE_UNAVAILABLE,Reply(json!({}))) }
            else { (StatusCode::OK,Reply(json!({"driver":"Moli test"}))) }
        }))
        .route("/api/v1/hvac",post(|State(s):State<OptionalServer>,Reply(body):Reply<Json>|async move {
            s.queries.lock().unwrap().push(body.clone());
            let phase=s.phase.load(Ordering::SeqCst);
            let (status,value)=match body["systemID"].as_u64().unwrap() {
                127 if phase==3 => (StatusCode::OK,json!({"systems":[{"systemID":33}]})),
                127 => (StatusCode::NOT_FOUND,json!({})),
                1 => (StatusCode::OK,json!({"data":{"systemID":1,"num_airqsensor":if phase==2 {2}else{1}}})),
                2 => (StatusCode::OK,json!({"data":{"systemID":2}})),
                0 if phase==1 => (StatusCode::SERVICE_UNAVAILABLE,json!({})),
                0 => (StatusCode::OK,json!({"data":{"systemID":0,"acs_power":1,"acs_setpoint":50,"acs_mintemp":25,"acs_maxtemp":65,"units":1}})),
                _ => panic!("unexpected read {body}"),
            };
            (status,Reply(value))
        }))
        .route("/api/v1/iaq",post(|State(s):State<OptionalServer>|async move {
            if s.phase.load(Ordering::SeqCst)==1 { (StatusCode::SERVICE_UNAVAILABLE,Reply(json!({}))) }
            else { (StatusCode::OK,Reply(json!({"data":[{"systemID":1,"iaqsensorID":3,"name":"Sensor 03","iaq_mode_vent":2,"co2_value":411}]}))) }
        })).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config=serde_json::from_value(json!({"host":"127.0.0.1","port":listener.local_addr().unwrap().port(),"read_only":false,"dhw_units":0})).unwrap();
    let task = tokio::spawn(async {
        axum::serve(listener, app).await.unwrap();
    });
    (config, state, task)
}

#[tokio::test]
async fn optional_failures_remove_stale_resources_and_independent_reads_survive() {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let (config, state, task) = optional_server().await;
    let zones = fixture_zones();
    let instance = InstanceId::from("az");
    let gateway = json!({"mac":"aa:bb:cc:dd:ee:ff"});
    let first = ResourceSnapshot::read(
        &config,
        &zones,
        &gateway,
        Instant::now() + Duration::from_secs(3),
    )
    .await;
    assert_eq!(
        first
            .entities(&instance, "aabbccddeeff", &zones, false)
            .len(),
        5
    );
    assert!(first.systems_available);
    // The explicit operator fallback must not overwrite a reported unit.
    assert_eq!(first.dhw.as_ref().unwrap()["units"], 1);
    assert!(
        state
            .queries
            .lock()
            .unwrap()
            .contains(&json!({"systemID":1}))
    );
    state.phase.store(1, Ordering::SeqCst);
    let failed = ResourceSnapshot::read(
        &config,
        &zones,
        &gateway,
        Instant::now() + Duration::from_secs(3),
    )
    .await;
    let entities = failed.entities(&instance, "aabbccddeeff", &zones, false);
    assert_eq!(entities.len(), 3);
    assert!(entities[0].0.point("version").is_some());
    assert!(entities[0].0.point("driver").is_none());
    assert!(
        failed
            .encode(
                &instance,
                "aabbccddeeff",
                &zones,
                &DeviceId::from("az:aabbccddeeff_dhw"),
                "acs_power",
                &Value::Bool(false)
            )
            .is_err()
    );
    assert!(failed.systems_available);
    assert!(!failed.iaq_available);
    state.phase.store(2, Ordering::SeqCst);
    let partial = ResourceSnapshot::read(
        &config,
        &zones,
        &gateway,
        Instant::now() + Duration::from_secs(3),
    )
    .await;
    let entities = partial.entities(&instance, "aabbccddeeff", &zones, false);
    let sensor = entities
        .iter()
        .find(|(d, _)| d.id.as_str().ends_with("_iaq3"))
        .unwrap();
    assert!(sensor.0.points.iter().all(|p| !p.access.write));
    assert!(!partial.iaq_available);
    task.abort();
}

#[tokio::test]
async fn expired_optional_refresh_does_not_make_network_calls_or_publish_fake_resources() {
    use std::time::Duration;
    let (config, state, task) = optional_server().await;
    let zones = fixture_zones();
    let resources = ResourceSnapshot::read(
        &config,
        &zones,
        &json!({"mac":"aa:bb:cc:dd:ee:ff"}),
        Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
    )
    .await;
    assert_eq!(state.queries.lock().unwrap().len(), 0);
    let entities = resources.entities(&InstanceId::from("az"), "aabbccddeeff", &zones, false);
    assert_eq!(entities.len(), 1);
    assert!(entities[0].0.points.iter().all(|p| !p.access.write));
    task.abort();
}

#[test]
fn hostile_unknown_padding_is_not_retained_in_optional_records() {
    let raw = json!({"data":{"systemID":1,"system_firmware":"5.34","unknown_padding":"x".repeat(1024*1024)}});
    let rows = records(&raw, Family::System).unwrap();
    assert!(rows[0].get("unknown_padding").is_none());
    assert!(serde_json::to_vec(&rows).unwrap().len() < 128);
}

#[test]
fn aggregate_snapshot_budget_rejects_whole_sections_without_consuming_budget() {
    let mut budget = SnapshotBudget::default();
    let record = json!({"name":"x".repeat(8*1024)});
    let section = vec![record; 32];
    for _ in 0..7 {
        assert!(budget.reserve(&section));
    }
    let used = budget.used;
    assert!(used > 1024 * 1024 && used < MAX_SNAPSHOT_BYTES);
    assert!(!budget.reserve(&section));
    assert_eq!(budget.used, used);
    assert!(budget.reserve(&[json!({"systemID":1})]));
}

#[tokio::test]
async fn gateway_unknown_padding_is_projected_before_retention() {
    use std::time::Duration;
    let config = serde_json::from_value(json!({"host":"controller.example"})).unwrap();
    let gateway =
        json!({"mac":"aa:bb:cc:dd:ee:ff","ws_firmware":"4.21","padding":"x".repeat(1024*1024)});
    let resources = ResourceSnapshot::read(
        &config,
        &fixture_zones(),
        &gateway,
        Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
    )
    .await;
    assert!(resources.webserver.get("padding").is_none());
    assert!(serde_json::to_vec(&resources.webserver).unwrap().len() < 128);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One closed operation table and its loopback fixture.
async fn command_preflight_reads_only_the_target_and_its_guard_dependencies() {
    use axum::{
        Json as Reply, Router,
        body::Bytes,
        http::{Method, Uri},
        routing::any,
    };
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    let calls = Arc::new(Mutex::new(Vec::new()));
    let observed = calls.clone();
    let app = Router::new().fallback(any(move |method: Method, uri: Uri, body: Bytes| {
        let observed = observed.clone();
        async move {
            assert_eq!(method, Method::POST);
            let body: Json = serde_json::from_slice(&body).unwrap();
            observed
                .lock()
                .unwrap()
                .push((uri.path().to_string(), body.clone()));
            Reply(match uri.path() {
                "/api/v1/version" => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    json!({"version":"1.78"})
                }
                "/api/v1/integration" => json!({"driver":"Moli test"}),
                "/api/v1/hvac" => match body["systemID"].as_u64().unwrap() {
                    127 => json!({"systems":[{"systemID":1,"num_airqsensor":1},{"systemID":2}]}),
                    0 => json!({"data":{"systemID":0,"acs_power":1}}),
                    1 => json!({"data":{"systemID":1,"num_airqsensor":1,"qAdapt":0}}),
                    _ => panic!("unexpected selector {body}"),
                },
                "/api/v1/iaq" => json!({"data":[{"systemID":1,"iaqsensorID":3,"co2_value":411}]}),
                _ => panic!("unexpected endpoint {uri}"),
            })
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config: Config = serde_json::from_value(
        json!({"host":"127.0.0.1","port":listener.local_addr().unwrap().port(),"read_only":false}),
    )
    .unwrap();
    let task = tokio::spawn(async {
        axum::serve(listener, app).await.unwrap();
    });
    let zones = fixture_zones();
    for (native, expected) in [
        (
            "aabbccddeeff_gateway",
            vec![
                ("/api/v1/hvac", json!({"systemID":127})),
                ("/api/v1/integration", json!({})),
            ],
        ),
        (
            "aabbccddeeff_dhw",
            vec![
                ("/api/v1/hvac", json!({"systemID":127})),
                ("/api/v1/hvac", json!({"systemID":0})),
            ],
        ),
        (
            "aabbccddeeff_s1_system",
            vec![("/api/v1/hvac", json!({"systemID":1}))],
        ),
        (
            "aabbccddeeff_s1_iaq3",
            vec![
                ("/api/v1/hvac", json!({"systemID":1})),
                ("/api/v1/iaq", json!({"systemID":1,"iaqsensorID":0})),
            ],
        ),
    ] {
        calls.lock().unwrap().clear();
        let resources = ResourceSnapshot::read_for(
            &config,
            &zones,
            &json!({"mac":"aa:bb:cc:dd:ee:ff"}),
            native,
            "aabbccddeeff",
            Instant::now() + Duration::from_secs(1),
        )
        .await;
        assert!(
            resources
                .entities(&InstanceId::from("az"), "aabbccddeeff", &zones, false)
                .iter()
                .any(|(d, _)| d.id == DeviceId::new(&InstanceId::from("az"), native))
        );
        assert_eq!(
            *calls.lock().unwrap(),
            expected
                .into_iter()
                .map(|(p, b)| (p.to_owned(), b))
                .collect::<Vec<_>>(),
            "{native}"
        );
    }
    for native in [
        "different_gateway",
        "aabbccddeeff_s0_system",
        "aabbccddeeff_s1_iaq0",
        "aabbccddeeff_s1_iaq3_extra",
    ] {
        calls.lock().unwrap().clear();
        let resources = ResourceSnapshot::read_for(
            &config,
            &zones,
            &json!({}),
            native,
            "aabbccddeeff",
            Instant::now() + Duration::from_secs(1),
        )
        .await;
        assert!(calls.lock().unwrap().is_empty());
        assert!(resources.read_only);
    }
    task.abort();
}

#[tokio::test]
async fn rejected_broadcast_inventory_cannot_be_replaced_by_a_smaller_known_scope() {
    use std::{sync::atomic::Ordering, time::Duration};
    let (config, state, task) = optional_server().await;
    state.phase.store(3, Ordering::SeqCst);
    let zones = fixture_zones();
    let resources = ResourceSnapshot::read(
        &config,
        &zones,
        &json!({"mac":"aa:bb:cc:dd:ee:ff"}),
        Instant::now() + Duration::from_secs(3),
    )
    .await;
    assert_eq!(resources.systems.len(), 2);
    assert!(!resources.systems_available);
    let instance = InstanceId::from("az");
    assert!(
        resources
            .encode(
                &instance,
                "aabbccddeeff",
                &zones,
                &DeviceId::from("az:aabbccddeeff_dhw"),
                "acs_power",
                &Value::Bool(false)
            )
            .is_err()
    );
    task.abort();
}

#[tokio::test]
async fn many_optional_sections_share_one_retention_budget_without_partial_sensor_lists() {
    use axum::{Json as Reply, Router, routing::post};
    use std::time::Duration;
    let app=Router::new()
        .route("/api/v1/hvac",post(|Reply(body):Reply<Json>|async move {
            Reply(if body["systemID"]==127 {
                json!({"systems":(1..=32).map(|id|json!({"systemID":id,"num_airqsensor":32})).collect::<Vec<_>>()})
            } else { json!({"data":{"systemID":0,"acs_power":1}}) })
        }))
        .route("/api/v1/iaq",post(|Reply(body):Reply<Json>|async move {
            let records=(1..=32).map(|sensor| {
                let mut raw=json!({"systemID":body["systemID"],"iaqsensorID":sensor,"co2_value":411});
                for key in ["system_firmware","manufacturer","name","thermos_firmware","eco_adapt","driver","version","mac","interface","ws_firmware","lmachine_firmware","ws_type"] {
                    raw[key]=json!("x".repeat(240));
                }
                raw
            }).collect::<Vec<_>>();
            Reply(json!({"data":records}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config: Config = serde_json::from_value(
        json!({"host":"127.0.0.1","port":listener.local_addr().unwrap().port()}),
    )
    .unwrap();
    let task = tokio::spawn(async {
        axum::serve(listener, app).await.unwrap();
    });
    let resources = ResourceSnapshot::read(
        &config,
        &fixture_zones(),
        &json!({"mac":"aa:bb:cc:dd:ee:ff"}),
        Instant::now() + Duration::from_secs(10),
    )
    .await;
    assert!(resources.budget.used <= MAX_SNAPSHOT_BYTES);
    assert!(!resources.iaq.is_empty() && resources.iaq.len() < 1024);
    assert_eq!(resources.iaq.len() % 32, 0);
    assert!(!resources.iaq_available);
    assert!(!resources.incomplete_iaq.is_empty());
    let serialized=serde_json::to_vec(&json!({"systems":resources.systems,"dhw":resources.dhw,"iaq":resources.iaq,"webserver":resources.webserver})).unwrap();
    assert!(serialized.len() < MAX_SNAPSHOT_BYTES);
    task.abort();
}
