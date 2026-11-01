use super::*;
use serde_json::json;
fn fixture() -> Json {
    serde_json::from_str(include_str!(
        "../../../integrations/overkiz/fixtures/devices.json"
    ))
    .unwrap()
}
fn entry(protocol: &str, class: &str, commands: Json, state: Json) -> Json {
    let mut raw = json!({"deviceURL":format!("{protocol}://example/123#2"),"available":true,"enabled":true,"label":"Example","definition":{"uiClass":class}});
    raw["definition"]["commands"] = commands;
    raw["states"] = state;
    raw
}
#[test]
fn fixture_nested_definitions_and_exact_payloads() {
    let entries = parse(&fixture()).unwrap();
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].position(), Value::Float(75.0));
    assert_eq!(
        entries[0]
            .apply("percent_control", &Value::Float(60.0))
            .unwrap(),
        json!({"label":"Moli","actions":[{"deviceURL":"io://example/123#2","commands":[{"name":"setClosure","parameters":[40.0]}]}]})
    );
    assert_eq!(
        entries[1].apply("control", &Value::from("open")).unwrap()["actions"][0]["commands"][0],
        json!({"name":"up","parameters":[0]})
    );
    assert_eq!(entries[1].position(), Value::Null);
    for e in &entries[2..] {
        assert!(e.apply("control", &Value::from("open")).is_err());
    }
}
#[test]
fn identities_are_exact_and_not_labels() {
    let mut e = fixture();
    let before = parse(&e).unwrap()[0].native();
    e[0]["label"] = json!("Renamed");
    assert_eq!(before, parse(&e).unwrap()[0].native());
    e[0]["deviceURL"] = json!("io://example_123#2");
    assert_ne!(before, parse(&e).unwrap()[0].native());
    assert!(!before.contains('/'));
}
#[test]
fn malformed_special_positions_and_arity_never_gain_capabilities() {
    for state in [
        json!(108),
        json!(124),
        json!(-1),
        json!("25"),
        json!(true),
        json!(null),
    ] {
        let e = entry(
            "io",
            "RollerShutter",
            json!([]),
            json!([{"name":"core:ClosureState","value":state}]),
        );
        assert_eq!(parse(&json!([e])).unwrap()[0].position(), Value::Null);
    }
    let e = entry(
        "io",
        "RollerShutter",
        json!([{"commandName":"open","nparams":1},{"commandName":"setClosure","nparams":2}]),
        json!([]),
    );
    let e = parse(&json!([e])).unwrap().remove(0);
    assert!(e.apply("control", &Value::from("open")).is_err());
    assert!(e.apply("percent_control", &Value::Float(20.0)).is_err());
    assert!(parse(&json!({})).is_err());
    assert!(parse(&json!([{"deviceURL":4}])).is_err());
    let mut duplicate = fixture();
    duplicate.as_array_mut().unwrap().push(fixture()[0].clone());
    assert!(parse(&duplicate).is_err());
}
#[test]
fn unavailable_unknown_and_names_cannot_unlock_actuation() {
    for class in ["Gate", "Window", "Unknown", "OpenCloseGate4T"] {
        let mut e = entry(
            "io",
            class,
            json!([{"commandName":"open","nparams":0}]),
            json!([]),
        );
        e["label"] = json!("RollerShutter");
        assert!(
            parse(&json!([e])).unwrap()[0]
                .apply("control", &Value::from("open"))
                .is_err()
        );
    }
    let mut e = fixture();
    e[0]["available"] = json!(false);
    assert!(
        parse(&e).unwrap()[0]
            .apply("percent_control", &Value::Float(20.0))
            .is_err()
    );
    for value in [
        Value::Float(f64::NAN),
        Value::Float(101.0),
        Value::from("20"),
    ] {
        assert!(
            parse(&fixture()).unwrap()[0]
                .apply("percent_control", &value)
                .is_err()
        );
    }
}
#[test]
fn publication_only_exposes_supported_typed_points() {
    let entries = parse(&fixture()).unwrap();
    let instance = InstanceId::from("test");
    let io = entries[0].device(&instance);
    assert!(io.point("percent_control").unwrap().access.write);
    assert!(io.point("control").unwrap().access.write);
    let rts = entries[1].device(&instance);
    assert!(rts.point("percent_control").is_none());
    assert!(rts.point("control").is_some());
    for e in &entries[2..] {
        assert!(e.device(&instance).points.iter().all(|p| !p.access.write));
    }
    assert_eq!(entries[2].opening(), Value::from("closed"));
    let mut raw = fixture();
    raw[0]["states"] =
        json!([{"name":"core:ClosureState","value":10},{"name":"core:ClosureState","value":20}]);
    assert_eq!(parse(&raw).unwrap()[0].position(), Value::Null);
    raw[1]["states"] = json!([{"name":"core:ClosureState","value":20}]);
    assert_eq!(parse(&raw).unwrap()[1].position(), Value::Null);
    raw[0]["definition"]["commands"] = json!([{"commandName":"open","nparams":0},{"commandName":"open","nparams":1},{"commandName":"setPosition","nparams":1}]);
    let parsed = parse(&raw).unwrap();
    assert!(parsed[0].apply("control", &Value::from("open")).is_err());
    assert!(
        parsed[0]
            .apply("percent_control", &Value::Float(30.0))
            .is_err()
    );
}
#[test]
fn partial_rts_shutter_retains_its_display_family_without_inventing_capabilities() {
    let raw = entry(
        "rts",
        "RollerShutter",
        json!([
            {"commandName":"down","nparams":1},
            {"commandName":"stop","nparams":1}
        ]),
        json!([]),
    );
    let entries = parse(&json!([raw])).unwrap();
    let device = entries[0].device(&InstanceId::from("test"));
    assert_eq!(device.model.as_deref(), Some("Overkiz RollerShutter"));
    assert!(device.point("percent_control").is_none());
    assert_eq!(entries[0].position(), Value::Null);
    assert!(entries[0].apply("control", &Value::from("open")).is_err());
    assert!(entries[0].apply("control", &Value::from("close")).is_ok());
    assert!(entries[0].apply("control", &Value::from("stop")).is_ok());
}
fn reviewed(url: &str, class: &str) -> crate::ReviewedOpener {
    crate::ReviewedOpener {
        device_url: url.into(),
        ui_class: class.into(),
    }
}
#[test]
fn exact_reviewed_openers_are_control_guarded_and_names_do_not_matter() {
    let mut raw = fixture();
    raw[2]["label"] = json!("Renamed blind");
    raw[2]["definition"]["commands"] = json!([
        {"commandName":"open","nparams":0}, {"commandName":"close","nparams":0},
        {"commandName":"stop","nparams":0}, {"commandName":"setClosure","nparams":1}
    ]);
    raw[2]["states"] = json!([{"name":"core:ClosureState","value":30}]);
    for (index, class, url) in [
        (2, "Gate", "io://example/789"),
        (3, "Window", "io://example/123#1"),
    ] {
        let policy = vec![reviewed(url, class)];
        let entries = parse_reviewed(&raw, &policy).unwrap();
        let device = entries[index].device(&InstanceId::from("test"));
        assert_eq!(
            device.model.as_deref(),
            Some(if class == "Gate" {
                "Overkiz Gate"
            } else {
                "Overkiz Window"
            })
        );
        assert!(device.points.iter().any(|p| p.access.write));
        assert!(
            device
                .points
                .iter()
                .filter(|p| p.access.write)
                .all(|p| p.semantic == Semantic::Control)
        );
        assert!(
            entries[index]
                .apply("control", &Value::from("open"))
                .is_ok()
        );
    }
    let entries = parse_reviewed(&raw, &[reviewed("io://example/789", "Gate")]).unwrap();
    assert!(
        entries[2]
            .device(&InstanceId::from("test"))
            .point("percent_control")
            .unwrap()
            .access
            .write
    );
    assert!(
        entries[2]
            .apply("percent_control", &Value::Float(40.0))
            .is_ok()
    );
    for policy in [
        vec![],
        vec![reviewed("io://other/789", "Gate")],
        vec![reviewed("io://example/789", "Window")],
    ] {
        assert!(
            parse_reviewed(&raw, &policy).unwrap()[2]
                .apply("control", &Value::from("open"))
                .is_err()
        );
    }
}
#[test]
fn cycle_is_a_distinct_guarded_impulse_and_class_changes_fail_closed() {
    let mut raw = fixture();
    raw[2]["definition"]["commands"] = json!([{"commandName":"cycle","nparams":0}]);
    raw[2]["states"] = json!([]);
    let policy = vec![reviewed("io://example/789", "Gate")];
    let entries = parse_reviewed(&raw, &policy).unwrap();
    let device = entries[2].device(&InstanceId::from("test"));
    assert_eq!(
        device.point("control").unwrap().kind,
        Kind::Enum {
            values: vec!["cycle".into()]
        }
    );
    assert_eq!(device.point("control").unwrap().semantic, Semantic::Control);
    assert!(device.point("percent_control").is_none());
    assert_eq!(
        entries[2].apply("control", &Value::from("cycle")).unwrap()["actions"][0]["commands"][0],
        json!({"name":"cycle","parameters":[]})
    );
    for command in ["open", "close", "stop"] {
        assert!(entries[2].apply("control", &Value::from(command)).is_err());
    }
    for class in ["Window", "Unknown", "RollerShutter"] {
        raw[2]["definition"]["uiClass"] = json!(class);
        raw[2]["definition"]["commands"] = json!([{"commandName":"open","nparams":0}]);
        let entries = parse_reviewed(&raw, &policy).unwrap();
        assert!(entries[2].apply("control", &Value::from("open")).is_err());
        assert!(
            entries[2]
                .device(&InstanceId::from("test"))
                .points
                .iter()
                .all(|p| !p.access.write)
        );
    }
    raw[2]["definition"]["uiClass"] = json!("Unknown");
    assert!(
        parse_reviewed(&raw, &[reviewed("io://example/789", "Unknown")]).unwrap()[2]
            .apply("control", &Value::from("open"))
            .is_err()
    );
}
#[test]
fn hashed_identity_bounds_long_native_urls_without_losing_routing() {
    let mut raw = fixture();
    let native = format!("io://example/{}", "a".repeat(450));
    raw[0]["deviceURL"] = json!(native);
    let entry = parse(&raw).unwrap().remove(0);
    assert_eq!(entry.native().len(), 64);
    assert!(entry.native().bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(
        moli_core::PointId::new(
            &entry.device(&InstanceId::from("test")).id,
            "percent_control"
        )
        .as_str()
        .len()
            < 256
    );
    assert_eq!(
        entry.apply("control", &Value::from("open")).unwrap()["actions"][0]["deviceURL"],
        native
    );
}
