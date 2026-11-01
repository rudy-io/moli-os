use super::*;

fn cover(class: &str, commands: Json, states: Json) -> Json {
    let mut raw = json!({"deviceURL":"io://example/cover#1","available":true,"enabled":true,
        "label":"Example cover","definition":{"uiClass":class}});
    raw["definition"]["commands"] = commands;
    raw["states"] = states;
    raw
}
fn load(raw: Json) -> Entry {
    parse(&Json::Array(vec![raw])).unwrap().remove(0)
}
fn command(entry: &Entry, key: &str, value: &Value) -> Json {
    entry.apply(key, value).unwrap()["actions"][0]["commands"][0].clone()
}
#[test]
fn ordinary_cover_classes_publish_and_encode_native_movement() {
    for class in [
        "AdjustableSlatsRollerShutter",
        "Awning",
        "Curtain",
        "ExteriorScreen",
        "ExteriorVenetianBlind",
        "Pergola",
        "PergolaHorizontalAwning",
        "PositionableHorizontalAwning",
        "PositionableRollerShutter",
        "PositionableScreen",
        "RollerShutter",
        "Screen",
        "Shutter",
        "SwingingShutter",
        "UpDownHorizontalAwning",
        "UpDownRollerShutter",
        "UpDownScreen",
        "VenetianBlind",
    ] {
        let entry = load(cover(
            class,
            json!([{"commandName":"open","nparams":0}]),
            json!([]),
        ));
        let device = entry.device(&InstanceId::from("test"));
        assert!(device.point("control").is_some(), "{class}");
        assert_eq!(
            command(&entry, "control", &Value::from("open")),
            json!({"name":"open","parameters":[]})
        );
    }
}
#[test]
fn deployment_state_and_setter_use_the_same_non_inverted_family() {
    let entry = load(cover(
        "Awning",
        json!([{"commandName":"setDeployment","nparams":1}]),
        json!([{"name":"core:DeploymentState","value":35}]),
    ));
    assert_eq!(entry.position(), Value::Float(35.0));
    assert!(
        entry
            .device(&InstanceId::from("test"))
            .point("percent_control")
            .unwrap()
            .access
            .write
    );
    assert_eq!(
        command(&entry, "percent_control", &Value::Float(60.0)),
        json!({"name":"setDeployment","parameters":[60.0]})
    );
}
#[test]
fn root_class_fallback_is_supported_but_conflicts_never_grant_writes() {
    let mut raw = cover(
        "RollerShutter",
        json!([{"commandName":"open","nparams":0}]),
        json!([]),
    );
    raw["uiClass"] = json!("Gate");
    let entry = load(raw.clone());
    assert!(entry.apply("control", &Value::from("open")).is_err());
    assert!(
        entry
            .device(&InstanceId::from("test"))
            .points
            .iter()
            .all(|p| !p.access.write)
    );
    raw["uiClass"] = json!("Awning");
    raw["definition"].as_object_mut().unwrap().remove("uiClass");
    assert_eq!(
        command(&load(raw), "control", &Value::from("open")),
        json!({"name":"open","parameters":[]})
    );
}
#[test]
fn favourites_publish_recall_and_convert_numeric_store_by_family() {
    for (class, state, setter, measured, want) in [
        (
            "RollerShutter",
            "core:ClosureState",
            "setClosure",
            75.0,
            40.0,
        ),
        (
            "Awning",
            "core:DeploymentState",
            "setDeployment",
            25.0,
            60.0,
        ),
    ] {
        let entry = load(cover(
            class,
            json!([
            {"commandName":setter,"nparams":1},
            {"commandName":"goToMemorized1Position","nparams":0},
            {"commandName":"setMemorized1Position","nparams":1}]),
            json!([{"name":state,"value":25},{"name":"core:Memorized1PositionState","value":25}]),
        ));
        let device = entry.device(&InstanceId::from("test"));
        let Kind::Enum { values } = &device.point("control").unwrap().kind else {
            panic!()
        };
        assert!(values.iter().any(|v| &**v == "favourite"));
        assert!(device.point("favourite_position").unwrap().access.write);
        assert_eq!(entry.position(), Value::Float(measured));
        assert_eq!(
            command(&entry, "control", &Value::from("favourite")),
            json!({"name":"goToMemorized1Position","parameters":[]})
        );
        assert_eq!(
            command(&entry, "favourite_position", &Value::Float(60.0)),
            json!({"name":"setMemorized1Position","parameters":[want]})
        );
    }
}
#[test]
fn unqualified_zero_parameter_store_never_receives_a_numeric_argument_or_a_save_action() {
    let entry = load(cover(
        "RollerShutter",
        json!([{"commandName":"setMemorized1Position","nparams":0}]),
        json!([]),
    ));
    assert!(
        entry
            .apply("favourite_position", &Value::Float(30.0))
            .is_err()
    );
    assert!(
        entry
            .apply("control", &Value::from("save_favourite"))
            .is_err()
    );
    assert!(
        entry
            .device(&InstanceId::from("test"))
            .point("control")
            .is_none()
    );
}
#[test]
fn rts_my_one_parameter_is_an_explicit_duration_binding() {
    let mut raw = cover(
        "RollerShutter",
        json!([{"commandName":"my","nparams":1}]),
        json!([]),
    );
    raw["deviceURL"] = json!("rts://example/cover#1");
    let entry = load(raw.clone());
    assert_eq!(
        command(&entry, "control", &Value::from("favourite")),
        json!({"name":"my","parameters":[0]})
    );
    raw["deviceURL"] = json!("io://example/cover#1");
    assert!(
        load(raw)
            .apply("control", &Value::from("favourite"))
            .is_err()
    );
}
#[test]
fn favourite_arity_and_duplicate_commands_fail_closed() {
    for arity in [json!(null), json!("0"), json!(-1), json!(1), json!(2)] {
        let entry = load(cover(
            "RollerShutter",
            json!([
            {"commandName":"goToMemorized1Position","nparams":arity},
            {"commandName":"setMemorized1Position","nparams":2}]),
            json!([]),
        ));
        assert!(entry.apply("control", &Value::from("favourite")).is_err());
        assert!(
            entry
                .apply("control", &Value::from("save_favourite"))
                .is_err()
        );
        assert!(
            entry
                .apply("favourite_position", &Value::Float(20.0))
                .is_err()
        );
        assert!(
            entry
                .device(&InstanceId::from("test"))
                .point("control")
                .is_none()
        );
    }
    let entry = load(cover(
        "RollerShutter",
        json!([
        {"commandName":"goToMemorized1Position","nparams":0},
        {"commandName":"goToMemorized1Position","nparams":0}]),
        json!([]),
    ));
    assert!(entry.apply("control", &Value::from("favourite")).is_err());
}
#[test]
fn numeric_favourite_rejects_invalid_values_without_clamping() {
    let entry = load(cover(
        "RollerShutter",
        json!([{"commandName":"setMemorized1Position","nparams":1},{"commandName":"setClosure","nparams":1}]),
        json!([]),
    ));
    for value in [
        Value::Float(-1.0),
        Value::Float(101.0),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NAN),
        Value::from("20"),
    ] {
        assert!(entry.apply("favourite_position", &value).is_err());
    }
    assert_eq!(
        command(&entry, "favourite_position", &Value::Float(0.0)),
        json!({"name":"setMemorized1Position","parameters":[100.0]})
    );
}
#[test]
fn sensitive_favourites_require_exact_opt_in_and_control_guard() {
    for class in ["Gate", "Window"] {
        let raw = cover(
            class,
            json!([
            {"commandName":"goToMemorized1Position","nparams":0},
            {"commandName":"setMemorized1Position","nparams":1},
            {"commandName":"setClosure","nparams":1}]),
            json!([]),
        );
        assert!(
            load(raw.clone())
                .apply("control", &Value::from("favourite"))
                .is_err()
        );
        let reviewed = [crate::ReviewedOpener {
            device_url: "io://example/cover#1".into(),
            ui_class: class.into(),
        }];
        let entry = parse_reviewed(&json!([raw]), &reviewed).unwrap().remove(0);
        assert!(entry.apply("control", &Value::from("favourite")).is_ok());
        assert!(
            entry
                .apply("favourite_position", &Value::Float(50.0))
                .is_ok()
        );
        assert!(
            entry
                .device(&InstanceId::from("test"))
                .points
                .iter()
                .filter(|p| p.access.write)
                .all(|p| p.semantic == Semantic::Control)
        );
    }
}

#[test]
fn read_only_preserves_physical_capabilities_but_model_rejects_every_write() {
    let raw = json!([cover(
        "RollerShutter",
        json!([
            {"commandName":"open","nparams":0},
            {"commandName":"setClosure","nparams":1},
            {"commandName":"goToMemorized1Position","nparams":0},
            {"commandName":"setMemorized1Position","nparams":1}
        ]),
        json!([{"name":"core:ClosureState","value":25},{"name":"core:Memorized1PositionState","value":40}])
    )]);
    let writable = parse(&raw).unwrap().remove(0);
    let observed = parse_policy(&raw, &[], true).unwrap().remove(0);
    let before = writable.device(&InstanceId::from("test"));
    let after = observed.device(&InstanceId::from("test"));
    assert_eq!(before.id, after.id);
    assert_eq!(before.points.len(), after.points.len());
    for point in &before.points {
        let observed_point = after.point(&point.key).unwrap();
        assert_eq!(point.kind, observed_point.kind);
        assert_eq!(point.semantic, observed_point.semantic);
        assert!(!observed_point.access.write);
    }
    assert_eq!(observed.position(), Value::Float(75.0));
    assert_eq!(observed.favourite(), Value::Float(60.0));
    for (key, value) in [
        ("control", Value::from("open")),
        ("control", Value::from("favourite")),
        ("percent_control", Value::Float(50.0)),
        ("favourite_position", Value::Float(50.0)),
    ] {
        assert!(writable.apply(key, &value).is_ok());
        assert!(observed.apply(key, &value).is_err());
    }
}
#[test]
fn unknown_positions_and_memorized_values_do_not_fabricate_measurement() {
    for family in [
        ("RollerShutter", "core:ClosureState", "setClosure"),
        ("Awning", "core:DeploymentState", "setDeployment"),
    ] {
        for value in [
            json!(108),
            json!(124),
            json!(-1),
            json!(101),
            json!(null),
            json!("25"),
            json!(true),
        ] {
            let entry = load(cover(
                family.0,
                json!([{"commandName":family.2,"nparams":1}]),
                json!([
                    {"name":family.1,"value":value},
                    {"name":"core:Memorized1PositionState","value":40}
                ]),
            ));
            assert_eq!(entry.position(), Value::Null);
        }
        let entry = load(cover(
            family.0,
            json!([{"commandName":family.2,"nparams":1}]),
            json!([
                {"name":"core:Memorized1PositionState","value":108}
            ]),
        ));
        assert_eq!(entry.favourite(), Value::Null);
    }
}

#[test]
fn rts_memorized_configuration_is_readable_without_measured_position() {
    let mut raw = cover(
        "RollerShutter",
        json!([{"commandName":"my","nparams":1}]),
        json!([
            {"name":"core:ClosureState","value":25},
            {"name":"core:Memorized1PositionState","value":40}
        ]),
    );
    raw["deviceURL"] = json!("rts://example/cover#1");
    let entry = load(raw.clone());
    assert_eq!(entry.favourite(), Value::Float(60.0));
    let device = entry.device(&InstanceId::from("test"));
    let point = device.point("favourite_position").unwrap();
    assert!(point.access.read);
    assert!(!point.access.write);
    assert!(device.point("percent_control").is_none());
    assert_eq!(entry.position(), Value::Null);
    for value in [
        json!(null),
        json!(108),
        json!(124),
        json!(-1),
        json!(101),
        json!("40"),
    ] {
        raw["states"][1]["value"] = value;
        assert_eq!(load(raw.clone()).favourite(), Value::Null);
    }
    raw["states"] = json!([]);
    assert_eq!(load(raw).favourite(), Value::Null);
}

#[test]
fn measured_closure_axis_wins_over_an_extra_deployment_setter() {
    let entry = load(cover(
        "RollerShutter",
        json!([
            {"commandName":"setClosure","nparams":1},
            {"commandName":"setDeployment","nparams":1},
            {"commandName":"setMemorized1Position","nparams":1}
        ]),
        json!([
            {"name":"core:ClosureState","value":25},
            {"name":"core:Memorized1PositionState","value":40}
        ]),
    ));
    assert_eq!(entry.position(), Value::Float(75.0));
    assert_eq!(entry.favourite(), Value::Float(60.0));
    assert_eq!(
        command(&entry, "percent_control", &Value::Float(60.0)),
        json!({"name":"setClosure","parameters":[40.0]})
    );
    assert_eq!(
        command(&entry, "favourite_position", &Value::Float(60.0)),
        json!({"name":"setMemorized1Position","parameters":[40.0]})
    );
}
#[test]
fn no_axis_with_both_position_setters_forbids_ambiguous_numeric_writes() {
    let entry = load(cover(
        "RollerShutter",
        json!([
            {"commandName":"setClosure","nparams":1},
            {"commandName":"setDeployment","nparams":1},
            {"commandName":"setMemorized1Position","nparams":1},
            {"commandName":"goToMemorized1Position","nparams":0},
            {"commandName":"open","nparams":0}
        ]),
        json!([{"name":"core:Memorized1PositionState","value":40}]),
    ));
    assert!(entry.apply("percent_control", &Value::Float(50.0)).is_err());
    assert!(
        entry
            .apply("favourite_position", &Value::Float(50.0))
            .is_err()
    );
    assert_eq!(entry.position(), Value::Null);
    assert_eq!(entry.favourite(), Value::Null);
    let device = entry.device(&InstanceId::from("test"));
    assert!(device.point("percent_control").is_none());
    assert!(!device.point("favourite_position").unwrap().access.write);
    assert!(entry.apply("control", &Value::from("open")).is_ok());
    assert!(entry.apply("control", &Value::from("favourite")).is_ok());
}
#[test]
fn two_explicit_axes_keep_deployment_priority_for_both_conversion_directions() {
    let entry = load(cover(
        "Awning",
        json!([
            {"commandName":"setClosure","nparams":1},
            {"commandName":"setDeployment","nparams":1}
        ]),
        json!([
            {"name":"core:ClosureState","value":25},
            {"name":"core:DeploymentState","value":35}
        ]),
    ));
    assert_eq!(entry.position(), Value::Float(35.0));
    assert_eq!(
        command(&entry, "percent_control", &Value::Float(60.0)),
        json!({"name":"setDeployment","parameters":[60.0]})
    );
}

#[test]
fn memorized_value_without_any_axis_never_claims_a_numeric_direction() {
    for class in ["RollerShutter", "Awning"] {
        let entry = load(cover(
            class,
            json!([
                {"commandName":"goToMemorized1Position","nparams":0},
                {"commandName":"setMemorized1Position","nparams":1}
            ]),
            json!([{"name":"core:Memorized1PositionState","value":40}]),
        ));
        assert_eq!(entry.favourite(), Value::Null, "{class}");
        assert!(
            entry
                .apply("favourite_position", &Value::Float(50.0))
                .is_err()
        );
        assert!(entry.apply("control", &Value::from("favourite")).is_ok());
        assert!(
            !entry
                .device(&InstanceId::from("test"))
                .point("favourite_position")
                .unwrap()
                .access
                .write
        );
    }
}
