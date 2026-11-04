use super::*;

fn fixture() -> Json {
    serde_json::from_str(include_str!(
        "../../../integrations/overkiz/fixtures/slow-cover.json"
    ))
    .unwrap()
}
fn entry(raw: Json) -> Entry {
    parse(&Json::Array(vec![raw])).unwrap().remove(0)
}
#[test]
fn slow_capability_publishes_atomic_open_close_and_target_commands() {
    let entry = entry(fixture());
    let device = entry.device(&InstanceId::from("example"));
    let point = device
        .point("percent_control_slow")
        .expect("declared optional speed enables slow target");
    assert!(!point.access.read && point.access.write);
    assert_eq!(point.semantic, Semantic::Other);
    let Kind::Enum { values } = &device.point("control").unwrap().kind else {
        panic!()
    };
    for value in ["open_slow", "close_slow"] {
        assert!(values.iter().any(|choice| choice.as_ref() == value));
    }
    for (key, value, closure) in [
        ("control", Value::from("open_slow"), 0.0),
        ("control", Value::from("close_slow"), 100.0),
        ("percent_control_slow", Value::Float(35.0), 65.0),
    ] {
        let body = entry.apply(key, &value).unwrap();
        assert_eq!(
            body,
            json!({"label":"Moli","actions":[{"deviceURL":"io://example/slow-cover","commands":[{"name":"setClosureAndLinearSpeed","parameters":[closure,"lowspeed"]}]}]})
        );
    }
}

#[test]
fn only_qualified_speed_signatures_publish_or_encode_slow_actions() {
    for command in [
        json!({"commandName":"setClosureAndLinearSpeed","nparams":2}),
        json!({"commandName":"setClosureAndLinearSpeed","nparams":2,"paramsSig":"p1,p2"}),
        json!({"commandName":"setClosureAndLinearSpeed","nparams":1,"paramsSig":"p1,*p2"}),
    ] {
        let mut raw = fixture();
        raw["definition"]["commands"][4] = command;
        assert!(
            entry(raw)
                .apply("percent_control_slow", &Value::Float(40.0))
                .is_ok()
        );
    }
    for (arity, signature) in [
        (json!(0), None),
        (json!(3), None),
        (json!(-1), None),
        (json!(1.5), None),
        (json!("2"), None),
        (Json::Null, None),
        (json!(1), None),
        (json!(1), Some(Json::Null)),
        (json!(2), Some(Json::Null)),
        (json!(2), Some(json!("p1,*p2"))),
        (json!(1), Some(json!("p1,p2"))),
        (json!(1), Some(json!("*p1,p2"))),
        (json!(1), Some(json!("p1,*p2,p3"))),
        (json!(1), Some(json!("p1, *p2"))),
        (json!(2), Some(json!(["p1", "p2"]))),
    ] {
        let mut raw = fixture();
        let command = &mut raw["definition"]["commands"][4];
        command["nparams"] = arity;
        command.as_object_mut().unwrap().remove("paramsSig");
        if let Some(signature) = signature {
            command["paramsSig"] = signature;
        }
        assert_no_slow(&entry(raw));
    }
}

fn assert_no_slow(entry: &Entry) {
    let device = entry.device(&InstanceId::from("example"));
    assert!(device.point("percent_control_slow").is_none());
    if let Some(point) = device.point("control") {
        let Kind::Enum { values } = &point.kind else {
            panic!()
        };
        assert!(!values.iter().any(|value| value.as_ref().ends_with("_slow")));
    }
    for (key, value) in [
        ("control", Value::from("open_slow")),
        ("control", Value::from("close_slow")),
        ("percent_control_slow", Value::Float(50.0)),
    ] {
        assert!(entry.apply(key, &value).is_err());
    }
}

#[test]
fn names_similar_commands_and_duplicate_definitions_never_grant_slow_motion() {
    for name in [
        "setPositionAndLinearSpeed",
        "setLinearSpeed",
        "setDeploymentAndLinearSpeed",
        "open",
    ] {
        let mut raw = fixture();
        raw["label"] = json!("Low speed shutter");
        raw["controllableName"] = json!("io:RollerShutterWithLowSpeedManagementIOComponent");
        raw["definition"]["commands"][4]["commandName"] = json!(name);
        assert_no_slow(&entry(raw));
    }
    let mut raw = fixture();
    let duplicate = raw["definition"]["commands"][4].clone();
    raw["definition"]["commands"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert_no_slow(&entry(raw));
}

#[test]
fn slow_motion_excludes_unqualified_protocols_classes_and_reviewed_openers() {
    for class in ["Gate", "Window", "Unknown"] {
        let mut raw = fixture();
        raw["definition"]["uiClass"] = json!(class);
        assert_no_slow(&entry(raw.clone()));
        let reviewed = [crate::ReviewedOpener {
            device_url: "io://example/slow-cover".into(),
            ui_class: class.into(),
        }];
        let entries = parse_reviewed(&json!([raw]), &reviewed).unwrap();
        assert_no_slow(&entries[0]);
    }
    for url in ["rts://example/slow-cover", "unknown://example/slow-cover"] {
        let mut raw = fixture();
        raw["deviceURL"] = json!(url);
        assert_no_slow(&entry(raw));
    }
    let mut raw = fixture();
    raw["uiClass"] = json!("Gate");
    assert_no_slow(&entry(raw));
}

#[test]
fn slow_closure_axis_is_independent_of_observed_deployment_and_normal_controls() {
    let mut raw = fixture();
    raw["states"] = json!([{"name":"core:DeploymentState","value":25}]);
    raw["definition"]["commands"][3]["commandName"] = json!("setDeployment");
    let entry = entry(raw);
    assert_eq!(entry.position(), Value::Float(25.0));
    for (key, value, name, parameters) in [
        (
            "percent_control",
            Value::Float(35.0),
            "setDeployment",
            json!([35.0]),
        ),
        (
            "percent_control_slow",
            Value::Float(35.0),
            "setClosureAndLinearSpeed",
            json!([65.0, "lowspeed"]),
        ),
        ("control", Value::from("open"), "open", json!([])),
        ("control", Value::from("close"), "close", json!([])),
        ("control", Value::from("stop"), "stop", json!([])),
    ] {
        let body = entry.apply(key, &value).unwrap();
        assert_eq!(
            body["actions"][0]["commands"],
            json!([{"name":name,"parameters":parameters}])
        );
    }
}

#[test]
fn slow_targets_reject_invalid_numbers_and_never_create_measured_speed() {
    let entry = entry(fixture());
    for value in [
        Value::Float(-1.0),
        Value::Float(101.0),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Bool(true),
        Value::from("50"),
        Value::Null,
    ] {
        assert!(entry.apply("percent_control_slow", &value).is_err());
    }
    for opening in [0.0, 100.0] {
        let body = entry
            .apply("percent_control_slow", &Value::Float(opening))
            .unwrap();
        assert_eq!(
            body["actions"][0]["commands"][0]["parameters"],
            json!([100.0 - opening, "lowspeed"])
        );
    }
    assert_eq!(entry.position(), Value::Float(75.0));
    let device = entry.device(&InstanceId::from("example"));
    assert_eq!(
        device
            .points
            .iter()
            .filter(|p| p.semantic == Semantic::Position)
            .count(),
        1
    );
}

#[test]
fn readonly_or_offline_preserve_slow_capability_without_allowing_actuation() {
    let entries = parse_policy(&json!([fixture()]), &[], true).unwrap();
    let device = entries[0].device(&InstanceId::from("example"));
    for key in ["control", "percent_control_slow"] {
        let point = device.point(key).unwrap();
        assert!(!point.access.read && !point.access.write);
    }
    let mut raw = fixture();
    raw["available"] = json!(false);
    let offline = entry(raw);
    for entry in [&entries[0], &offline] {
        for (key, value) in [
            ("control", Value::from("open_slow")),
            ("control", Value::from("close_slow")),
            ("percent_control_slow", Value::Float(35.0)),
        ] {
            assert!(entry.apply(key, &value).is_err());
        }
    }
}
