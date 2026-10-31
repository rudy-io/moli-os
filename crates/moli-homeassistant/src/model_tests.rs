use super::*;
use moli_core::{Kind, Semantic};
use serde_json::json;

fn decode(e: &Entity, raw: &Json) -> anyhow::Result<Snapshot> {
    super::decode(e, raw, None)
}

fn entity(domain: &str, writable: &[&str]) -> Entity {
    serde_json::from_value(
        json!({"native_id":"stable-1","entity_id":format!("{domain}.example"),
        "domain":domain,"room":"Workshop","safety":"ordinary","writable":writable}),
    )
    .unwrap()
}

fn state(e: &Entity, state: &str, attrs: Json) -> Json {
    let mut result = json!({"entity_id":e.entity_id,"state":state});
    result["attributes"] = attrs;
    result
}

#[test]
fn binary_contact_is_closed_when_ha_is_off_and_unknown_is_not_false() {
    let e = entity("binary_sensor", &[]);
    let s = decode(&e, &state(&e, "on", json!({"device_class":"door"}))).unwrap();
    assert_eq!(s.values["contact"], Value::Bool(false));
    assert_eq!(s.points[0].semantic, Semantic::Contact);
    let s = decode(&e, &state(&e, "off", json!({"device_class":"window"}))).unwrap();
    assert_eq!(s.values["contact"], Value::Bool(true));
    assert!(decode(&e, &state(&e, "unknown", json!({}))).is_err());
    assert!(decode(&e, &state(&e, "unavailable", json!({}))).is_err());
    for (class, key, semantic) in [
        ("motion", "occupancy", Semantic::Occupancy),
        ("moisture", "water_leak", Semantic::WaterLeak),
        ("smoke", "smoke", Semantic::Smoke),
        ("battery", "battery_low", Semantic::BatteryLow),
    ] {
        let s = decode(&e, &state(&e, "on", json!({"device_class":class}))).unwrap();
        assert_eq!(s.values[key], Value::Bool(true));
        assert_eq!(s.points[0].semantic, semantic);
    }
}

#[test]
fn sensor_numeric_units_text_and_hostile_snapshots_are_conservative() {
    let e = entity("sensor", &[]);
    let s = decode(
        &e,
        &state(
            &e,
            "21.5",
            json!({"unit_of_measurement":"°C","device_class":"temperature"}),
        ),
    )
    .unwrap();
    assert_eq!(s.values["temperature"], Value::Float(21.5));
    let s = decode(&e, &state(&e, "Ready", json!({}))).unwrap();
    assert_eq!(s.points[0].semantic, Semantic::Other);
    assert_eq!(s.values["state"], Value::from("Ready"));
    for bad in ["NaN", "inf", "not-a-number"] {
        assert!(decode(&e, &state(&e, bad, json!({"unit_of_measurement":"W"}))).is_err());
    }
    let mut raw = state(&e, "10", json!({}));
    raw["entity_id"] = json!("sensor.other");
    assert!(decode(&e, &raw).is_err());
    assert!(decode(&e, &state(&e, &"x".repeat(257), json!({}))).is_err());
    assert!(
        decode(
            &e,
            &json!({"entity_id":e.entity_id,"state":"10","attributes":[]})
        )
        .is_err()
    );
}

#[test]
fn climate_maps_only_supported_modes_and_fahrenheit_ranges() {
    let mut e = entity("climate", &["target_temperature", "mode", "on"]);
    e.turn_on_mode = Some("heat".into());
    let raw = state(
        &e,
        "heat",
        json!({"current_temperature":68,"temperature":70,"temperature_unit":"°F","min_temp":50,"max_temp":86,"target_temp_step":1,"supported_features":1,"hvac_modes":["off","heat","cool","heat_cool"]}),
    );
    let s = decode(&e, &raw).unwrap();
    assert_eq!(s.values["temperature"], Value::Float(20.0));
    assert_eq!(s.values["mode"], Value::from("chaud"));
    assert!(matches!(
        s.points
            .iter()
            .find(|p| &*p.key == "target_temperature")
            .unwrap()
            .kind,
        Kind::Numeric {
            min: Some(10.0),
            max: Some(30.0),
            ..
        }
    ));
    let service = command(&e, &s, "target_temperature", &Value::Float(20.0)).unwrap();
    assert_eq!(service.path, "/api/services/climate/set_temperature");
    assert_eq!(
        service.body,
        json!({"entity_id":"climate.example","temperature":68.0})
    );
    assert_eq!(
        command(&e, &s, "mode", &Value::from("froid")).unwrap().body,
        json!({"entity_id":"climate.example","hvac_mode":"cool"})
    );
    assert_eq!(
        command(&e, &s, "on", &Value::Bool(true)).unwrap().body,
        json!({"entity_id":"climate.example","hvac_mode":"heat"})
    );
    assert!(command(&e, &s, "target_temperature", &Value::Float(31.0)).is_err());
    assert!(command(&e, &s, "target_temperature", &Value::Float(20.1)).is_err());
    assert!(command(&e, &s, "mode", &Value::from("heat_cool")).is_err());
}

#[test]
fn light_and_switch_exact_payload_partial_allowlist_and_unknown_effects() {
    let e = entity("light", &["brightness"]);
    let s = decode(
        &e,
        &state(
            &e,
            "on",
            json!({"brightness":255,"supported_color_modes":["rgb"]}),
        ),
    )
    .unwrap();
    assert_eq!(s.values["brightness"], Value::Float(100.0));
    assert_eq!(
        command(&e, &s, "brightness", &Value::Float(50.0))
            .unwrap()
            .body,
        json!({"entity_id":"light.example","brightness":128})
    );
    assert!(command(&e, &s, "on", &Value::Bool(true)).is_err());
    assert!(command(&e, &s, "brightness", &Value::Float(f64::NAN)).is_err());
    let e = entity("switch", &["on"]);
    let s = decode(&e, &state(&e, "off", json!({}))).unwrap();
    let service = command(&e, &s, "on", &Value::Bool(true)).unwrap();
    assert_eq!(service.path, "/api/services/switch/turn_on");
    assert_eq!(service.body, json!({"entity_id":"switch.example"}));
    let mut unknown = e.clone();
    unknown.safety = crate::Safety::Unknown;
    assert!(command(&unknown, &s, "on", &Value::Bool(true)).is_err());
    assert!(decode(&e, &state(&e, "maybe", json!({}))).is_err());
}

#[test]
fn ordinary_cover_honors_feature_bits_and_refuses_renamed_gates_and_groups() {
    let e = entity("cover", &["control", "percent_control"]);
    let s = decode(
        &e,
        &state(
            &e,
            "open",
            json!({"supported_features":5,"current_position":75,"device_class":"shutter"}),
        ),
    )
    .unwrap();
    assert_eq!(s.values["percent_control"], Value::Float(75.0));
    assert_eq!(
        command(&e, &s, "control", &Value::from("open"))
            .unwrap()
            .path,
        "/api/services/cover/open_cover"
    );
    assert!(command(&e, &s, "control", &Value::from("stop")).is_err());
    assert_eq!(
        command(&e, &s, "percent_control", &Value::Float(40.0))
            .unwrap()
            .body,
        json!({"entity_id":"cover.example","position":40.0})
    );
    for class in ["gate", "garage", "door", "window", "unknown"] {
        let s=decode(&e,&state(&e,"open",json!({"supported_features":15,"current_position":75,"device_class":class,"friendly_name":"Curtain"}))).unwrap();
        assert!(command(&e, &s, "percent_control", &Value::Float(40.0)).is_err());
    }
    let s = decode(
        &e,
        &state(
            &e,
            "open",
            json!({"supported_features":15,"current_position":75,"entity_id":["cover.other"]}),
        ),
    )
    .unwrap();
    assert!(command(&e, &s, "control", &Value::from("open")).is_err());
}

#[test]
fn vacuum_machine_controls_and_media_scales_are_primitive_only() {
    let e = entity("vacuum", &["control"]);
    let s = decode(
        &e,
        &state(
            &e,
            "docked",
            json!({"supported_features":8220,"battery_level":85}),
        ),
    )
    .unwrap();
    assert_eq!(
        s.points
            .iter()
            .find(|p| &*p.key == "control")
            .unwrap()
            .semantic,
        Semantic::Control
    );
    assert_eq!(s.values["state"], Value::from("docked"));
    assert_eq!(
        command(&e, &s, "control", &Value::from("start"))
            .unwrap()
            .path,
        "/api/services/vacuum/start"
    );
    assert!(command(&e, &s, "control", &Value::from("send_command")).is_err());
    let e = entity("media_player", &["playing", "volume", "mute"]);
    let s=decode(&e,&state(&e,"playing",json!({"supported_features":16909,"volume_level":0.25,"is_volume_muted":false,"media_title":"Example"}))).unwrap();
    assert_eq!(s.values["volume"], Value::Float(25.0));
    assert_eq!(
        command(&e, &s, "volume", &Value::Float(60.0)).unwrap().body,
        json!({"entity_id":"media_player.example","volume_level":0.6})
    );
    assert_eq!(
        command(&e, &s, "playing", &Value::Bool(false))
            .unwrap()
            .path,
        "/api/services/media_player/media_pause"
    );
    assert_eq!(
        command(&e, &s, "mute", &Value::Bool(true)).unwrap().body,
        json!({"entity_id":"media_player.example","is_volume_muted":true})
    );
    assert!(command(&e, &s, "play_media", &Value::from("https://example.org")).is_err());
}

#[test]
fn catalogue_fixtures_replay_all_eight_domains_read_only_by_default() {
    let raw: Vec<Json> = serde_json::from_str(include_str!(
        "../../../integrations/homeassistant/fixtures/states.json"
    ))
    .unwrap();
    assert_eq!(raw.len(), 8);
    for entry in raw {
        let domain = entry["entity_id"]
            .as_str()
            .unwrap()
            .split_once('.')
            .unwrap()
            .0;
        let e = entity(domain, &[]);
        let snapshot = decode(&e, &entry).unwrap();
        assert!(!snapshot.values.is_empty());
        assert!(snapshot.points.iter().all(|p| !p.access.write));
        for point in &snapshot.points {
            assert!(!point.label.starts_with("homeassistant."));
        }
    }
}

#[test]
fn light_colour_and_kelvin_are_exact_bounded_and_capability_gated() {
    let e = entity("light", &["color", "color_temp"]);
    let raw = state(
        &e,
        "on",
        json!({"supported_color_modes":["rgb","color_temp"],"rgb_color":[255,0,128],"color_temp_kelvin":3000,"min_color_temp_kelvin":2000,"max_color_temp_kelvin":6500}),
    );
    let s = decode(&e, &raw).unwrap();
    assert_eq!(s.values.get("color"), Some(&Value::from("#ff0080")));
    assert_eq!(s.values.get("color_temp"), Some(&Value::Float(3000.0)));
    assert_eq!(
        command(&e, &s, "color", &Value::from("#12ABef")).unwrap(),
        Service {
            path: "/api/services/light/turn_on".into(),
            body: json!({"entity_id":"light.example","rgb_color":[18,171,239]})
        }
    );
    assert_eq!(
        command(&e, &s, "color_temp", &Value::Float(4000.0))
            .unwrap()
            .body,
        json!({"entity_id":"light.example","color_temp_kelvin":4000.0})
    );
    for bad in [
        "red",
        "#123",
        "#1234567",
        "#xyzxyz",
        "#12ABef\n",
        "{{token}}",
        "#ffffff,all",
    ] {
        assert!(
            command(&e, &s, "color", &Value::from(bad)).is_err(),
            "accepted {bad}"
        );
    }
    for bad in [1999.0, 6501.0, 3000.5, f64::NAN] {
        assert!(command(&e, &s, "color_temp", &Value::Float(bad)).is_err());
    }
    let absent=decode(&e,&state(&e,"on",json!({"supported_color_modes":["onoff"],"rgb_color":[1,2,3],"color_temp_kelvin":3000,"min_color_temp_kelvin":2000,"max_color_temp_kelvin":6500}))).unwrap();
    assert!(command(&e, &absent, "color", &Value::from("#ffffff")).is_err());
    assert!(command(&e, &absent, "color_temp", &Value::Float(3000.0)).is_err());
    for attrs in [
        json!({"supported_color_modes":["color_temp"],"min_color_temp_kelvin":6500,"max_color_temp_kelvin":2000}),
        json!({"supported_color_modes":["color_temp"]}),
    ] {
        let snapshot = decode(&e, &state(&e, "on", attrs)).unwrap();
        assert!(command(&e, &snapshot, "color_temp", &Value::Float(3000.0)).is_err());
    }
    let snapshot = decode(
        &e,
        &state(
            &e,
            "on",
            json!({"supported_color_modes":["rgb"],"rgb_color":[256,0,0]}),
        ),
    )
    .unwrap();
    assert_eq!(snapshot.values.get("color"), Some(&Value::Null));
    let readonly = entity("light", &[]);
    let snapshot = decode(&readonly, &raw).unwrap();
    assert!(command(&readonly, &snapshot, "color", &Value::from("#ffffff")).is_err());
}

#[test]
fn declared_readings_are_explicitly_unknown_when_missing_or_invalid() {
    for invalid in [Json::Null, json!("invalid"), json!(99999)] {
        let e = entity("media_player", &[]);
        let snapshot = decode(
            &e,
            &state(
                &e,
                "playing",
                json!({"supported_features":12,"volume_level":invalid,"is_volume_muted":invalid}),
            ),
        )
        .unwrap();
        assert_eq!(snapshot.values.get("volume"), Some(&Value::Null));
        assert_eq!(snapshot.values.get("mute"), Some(&Value::Null));
        let e = entity("light", &[]);
        let snapshot=decode(&e,&state(&e,"on",json!({"supported_color_modes":["rgb","color_temp"],"brightness":invalid,"rgb_color":invalid,"color_temp_kelvin":invalid,"min_color_temp_kelvin":2000,"max_color_temp_kelvin":6500}))).unwrap();
        for key in ["brightness", "color", "color_temp"] {
            assert_eq!(snapshot.values.get(key), Some(&Value::Null));
        }
    }
    let e = entity("media_player", &[]);
    let snapshot = decode(&e, &state(&e, "playing", json!({"supported_features":12}))).unwrap();
    assert_eq!(snapshot.values.get("volume"), Some(&Value::Null));
    assert_eq!(snapshot.values.get("mute"), Some(&Value::Null));
    let e = entity("vacuum", &["control"]);
    let snapshot = decode(&e, &state(&e, "docked", json!({"supported_features":8192}))).unwrap();
    assert!(!snapshot.values.contains_key("control"));
}

#[test]
fn global_climate_unit_converts_fahrenheit_and_preserves_entity_precedence() {
    let e = entity("climate", &["target_temperature"]);
    let mut raw = state(
        &e,
        "heat",
        json!({
            "current_temperature":68,"temperature":70,"min_temp":50,"max_temp":86,
            "target_temp_step":1,"supported_features":1,"hvac_modes":["off","heat"]
        }),
    );
    let global = TemperatureUnit::from_config(&json!({"unit_system":{"temperature":"°F"}}));
    let snapshot = super::decode(&e, &raw, global).unwrap();
    assert_eq!(snapshot.values["temperature"], Value::Float(20.0));
    assert_eq!(
        command(&e, &snapshot, "target_temperature", &Value::Float(20.0))
            .unwrap()
            .body,
        json!({"entity_id":"climate.example","temperature":68.0})
    );
    raw["attributes"]["temperature_unit"] = json!("°C");
    raw["attributes"]["current_temperature"] = json!(21);
    let snapshot = super::decode(&e, &raw, global).unwrap();
    assert_eq!(snapshot.values["temperature"], Value::Float(21.0));
}

#[test]
fn absent_or_invalid_climate_units_never_infer_celsius() {
    let mut e = entity("climate", &[]);
    e.safety = crate::Safety::Unknown;
    let mut raw = state(
        &e,
        "heat",
        json!({"current_temperature":21,"temperature":22,
        "min_temp":7,"max_temp":35,"target_temp_step":0.5,"supported_features":1}),
    );
    for config in [
        json!({}),
        json!({"unit_system":null}),
        json!({"unit_system":{"temperature":"K"}}),
        json!({"unit_system":{"temperature":true}}),
    ] {
        let snapshot = super::decode(&e, &raw, TemperatureUnit::from_config(&config)).unwrap();
        assert!(!snapshot.values.contains_key("temperature"));
        assert!(!snapshot.values.contains_key("target_temperature"));
    }
    let global = TemperatureUnit::from_config(&json!({"unit_system":{"temperature":"°C"}}));
    let snapshot = super::decode(&e, &raw, global).unwrap();
    assert_eq!(snapshot.values["temperature"], Value::Float(21.0));
    assert!(snapshot.points.iter().all(|p| !p.access.write));
    for unit in [json!("K"), Json::Null, json!(false)] {
        raw["attributes"]["temperature_unit"] = unit;
        let snapshot = super::decode(&e, &raw, global).unwrap();
        assert!(!snapshot.values.contains_key("temperature"));
        assert!(!snapshot.values.contains_key("target_temperature"));
    }
}
