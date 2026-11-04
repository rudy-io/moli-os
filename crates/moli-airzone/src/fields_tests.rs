use super::*;
use moli_core::{Kind, Unit};
use serde_json::json;

#[test]
fn projection_discards_unknown_padding_and_bounds_known_text_before_retention() {
    let raw = json!({"systemid":1,"zoneID":2,"name":"é".repeat(20000),"on":1,
        "units":0,"roomtemp":21.25,"unknown_padding":"x".repeat(4*1024*1024)});
    let retained = project(&raw).unwrap();
    assert!(retained.get("unknown_padding").is_none());
    assert_eq!(retained["systemID"], 1);
    assert_eq!(retained["zoneID"], 2);
    assert_eq!(retained["roomTemp"], 21.25);
    assert_eq!(retained["name"].as_str().unwrap().chars().count(), 240);
    assert!(serde_json::to_vec(&retained).unwrap().len() <= 16 * 1024);
}

#[test]
fn projection_never_truncates_an_oversized_capability_list_into_authorization() {
    let raw = json!({"mode":3,"modes":[1,3],"speed":3,"speeds":7,
        "speed_values":(0..33).collect::<Vec<_>>(),"heat_angle_values":[45,90],
        "battery":"invalid", "energy_data":{"energy_consump":12.5,"power_gen_heat":null,
        "unknown_padding":"x".repeat(20000)}});
    let retained = project(&raw).unwrap();
    // Keep the explicit invalid capability unavailable; dropping its presence
    // would incorrectly authorize the legacy `speeds` fallback.
    assert_eq!(retained.get("speed_values"), Some(&Json::Null));
    assert_eq!(retained.get("battery"), Some(&Json::Null));
    assert_eq!(retained["heat_angle_values"], json!([45, 90]));
    assert_eq!(
        retained["energy_data"],
        json!({"energy_consump":12.5,"power_gen_heat":null})
    );
    assert!(
        !point(Scope::System, &retained, "fan_speed", false)
            .0
            .access
            .write
    );
}

#[test]
fn projection_retains_invalid_core_fields_as_fail_closed_nulls() {
    let invalid = project(&json!({
        "on":2,"roomTemp":"bad","minTemp":[],"maxTemp":{},"setpoint":true,
        "units":"0","temp_step":"0.5","modes":{},"master_zoneID":1.5,"erv_modes":["1"]
    }))
    .unwrap();
    for key in [
        "on",
        "roomTemp",
        "minTemp",
        "maxTemp",
        "setpoint",
        "units",
        "temp_step",
        "modes",
        "master_zoneID",
        "erv_modes",
    ] {
        assert_eq!(
            invalid.get(key),
            Some(&Json::Null),
            "{key} must retain the fail-closed signal"
        );
    }
}

#[test]
fn projection_bounds_alarm_entries_and_rejects_alias_conflicts() {
    let raw = json!({"errors":(0..100).map(|_|json!({"Zone":"x".repeat(20000),
        "private_padding":"x".repeat(20000)})).collect::<Vec<_>>()});
    let retained = project(&raw).unwrap();
    let errors = retained["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 32);
    assert!(
        errors
            .iter()
            .all(|entry| entry.get("private_padding").is_none()
                && entry["Zone"].as_str().unwrap().chars().count() == 240)
    );
    assert!(serde_json::to_vec(&retained).unwrap().len() <= 16 * 1024);
    assert!(project(&json!({"roomTemp":20,"roomtemp":21})).is_err());
}

#[test]
fn projection_rejects_a_record_over_the_total_budget_and_does_not_change_ack_normalization() {
    let raw = json!({"errors":(0..32).map(|_|json!({"Zone":"é".repeat(240),
        "system":"é".repeat(240)})).collect::<Vec<_>>()});
    assert!(project(&raw).is_err());
    let name = "x".repeat(300);
    let normalized =
        normalize(&json!({"name":name,"unknown_padding":"keep for exact comparison"})).unwrap();
    assert_eq!(normalized["name"].as_str().unwrap().len(), 300);
    assert_eq!(normalized["unknown_padding"], "keep for exact comparison");
}
fn point(scope: Scope, raw: &Json, key: &str, read_only: bool) -> (PointSpec, Value) {
    points(scope, raw, read_only)
        .into_iter()
        .find(|(p, _)| p.key.as_ref() == key)
        .unwrap_or_else(|| panic!("missing {key}"))
}
#[test]
fn documented_case_aliases_are_normalized_and_conflicts_rejected() {
    let raw =
        normalize(&json!({"roomtemp":21.25,"coldstage":2,"IAQ_Mode_Vent":1,"systemid":1})).unwrap();
    assert_eq!(raw["roomTemp"], 21.25);
    assert_eq!(raw["coldStage"], 2);
    assert_eq!(raw["iaq_mode_vent"], 1);
    assert_eq!(raw["systemID"], 1);
    assert!(normalize(&json!({"roomtemp":21,"roomTemp":22})).is_err());
}
#[test]
fn zone_metadata_and_double_setpoints_are_typed_without_fabricated_single_setpoint() {
    let raw = json!({"units":1,"roomTemp":72.25,"battery":41,"coverage":86,"heatsetpoint":68,"heatmintemp":60,"heatmaxtemp":78,"coolsetpoint":76,"coolmintemp":70,"coolmaxtemp":86,"temp_step":0.5});
    let (p, v) = point(Scope::Zone, &raw, "temperature", false);
    assert_eq!(p.unit, Some(Unit::Other("°F".into())));
    assert_eq!(v, Value::Float(72.25));
    let (p, _) = point(Scope::Zone, &raw, "heatsetpoint", false);
    assert!(p.access.write);
    assert_eq!(
        point(Scope::Zone, &raw, "battery", false).0.unit,
        Some(Unit::Percent)
    );
    assert!(
        !points(Scope::Zone, &raw, false)
            .iter()
            .any(|(p, _)| p.key.as_ref() == "target_temperature")
    );
    assert!(encode(Scope::Zone, &raw, "heatsetpoint", &Value::Float(68.25)).is_err());
    assert_eq!(
        encode(Scope::Zone, &raw, "heatsetpoint", &Value::Float(68.5)).unwrap(),
        ("heatsetpoint".into(), json!(68.5))
    );
}
#[test]
fn shared_controls_never_become_zone_writes_and_capabilities_are_required() {
    let raw = json!({"mode":3,"modes":[1,3],"speed":2,"speed_values":[0,2,4],"slats_vertical":2,"qAdapt":1});
    for key in ["mode", "fan_speed", "slats_vertical", "qAdapt"] {
        assert!(!point(Scope::Zone, &raw, key, false).0.access.write);
        assert!(point(Scope::System, &raw, key, false).0.access.write);
    }
    assert_eq!(
        encode(Scope::System, &raw, "mode", &Value::from("heat")).unwrap(),
        ("mode".into(), json!(3))
    );
    assert!(encode(Scope::System, &raw, "fan_speed", &Value::from("3")).is_err());
    assert!(
        !point(Scope::System, &json!({"mode":3}), "mode", false)
            .0
            .access
            .write
    );
}
#[test]
fn capabilities_constrain_stages_angles_and_air_quality_thresholds() {
    let raw = json!({"heatStage":2,"heatStages":2,"coldStage":1,"coldStages":3,"heat_angle_conf":45,"heat_angle_values":[45,90],"aq_thrlow":10,"aq_thrhigh":40});
    assert_eq!(
        encode(Scope::Zone, &raw, "heatStage", &Value::from("2"))
            .unwrap()
            .1,
        json!(2)
    );
    assert!(encode(Scope::Zone, &raw, "heatStage", &Value::from("1")).is_err());
    assert!(encode(Scope::Zone, &raw, "heat_angle_conf", &Value::from("50")).is_err());
    assert!(encode(Scope::Zone, &raw, "aq_thrlow", &Value::Float(40.)).is_err());
    assert!(encode(Scope::Zone, &raw, "aq_thrhigh", &Value::Float(10.)).is_err());
    assert!(encode(Scope::Zone, &raw, "aq_thrhigh", &Value::Float(40.5)).is_err());
    assert_eq!(
        encode(Scope::Zone, &raw, "aq_thrlow", &Value::Float(20.))
            .unwrap()
            .1,
        json!(20)
    );
}
#[test]
fn dhw_unknown_units_are_not_guessed_and_iaq_null_does_not_enable_writes() {
    let dhw =
        json!({"acs_temp":45.3,"acs_setpoint":50,"acs_mintemp":25,"acs_maxtemp":65,"acs_power":1});
    assert_eq!(point(Scope::Dhw, &dhw, "acs_temp", false).0.unit, None);
    assert!(
        !point(Scope::Dhw, &dhw, "acs_setpoint", false)
            .0
            .access
            .write
    );
    let iaq = json!({"name":"Air sensor","iaq_mode_vent":null,"co2_value":650,"iaq_score":90});
    assert!(
        !points(Scope::Iaq, &iaq, false)
            .iter()
            .any(|(p, _)| p.key.as_ref() == "iaq_mode_vent")
    );
    assert_eq!(
        point(Scope::Iaq, &iaq, "co2_value", false).1,
        Value::Float(650.)
    );
}
#[test]
fn read_only_unknown_fields_and_bounded_text_cannot_authorize_commands() {
    let raw = json!({"name":"Zone 01","on":1,"units":0,"setpoint":21,"minTemp":15,"maxTemp":30,"temp_step":0.5,"unknown_write":1});
    assert!(
        points(Scope::Zone, &raw, true)
            .iter()
            .all(|(p, _)| !p.access.write)
    );
    assert!(encode(Scope::Zone, &raw, "unknown_write", &Value::Bool(true)).is_err());
    assert!(encode(Scope::Zone, &raw, "name", &Value::from("012345678901")).is_err());
    assert!(encode(Scope::Zone, &raw, "name", &Value::from("Zone\n01")).is_err());
    assert_eq!(
        encode(Scope::Zone, &raw, "name", &Value::from("Zone 02"))
            .unwrap()
            .1,
        json!("Zone 02")
    );
    let p = point(
        Scope::Webserver,
        &json!({"ws_firmware":"x".repeat(5000)}),
        "ws_firmware",
        true,
    );
    assert!(matches!(p.0.kind, Kind::Text));
    assert!(p.1.to_json().as_str().unwrap().chars().count() <= 240);
}
#[test]
fn energy_subfields_preserve_documented_units_and_ignore_nulls() {
    let raw = json!({"power":2.5,"energy_data":{"energy_consump":26.5,"power_gen_heat":3.0,"energy_gen_cool":null}});
    assert_eq!(
        point(Scope::System, &raw, "energy_data.energy_consump", true)
            .0
            .unit,
        Some(Unit::KiloWattHour)
    );
    assert_eq!(
        point(Scope::System, &raw, "energy_data.power_gen_heat", true)
            .0
            .unit,
        Some(Unit::Other("kW".into()))
    );
    assert_ne!(
        point(Scope::System, &raw, "power", true).0.semantic,
        moli_core::Semantic::Power
    );
    assert!(
        !points(Scope::System, &raw, true)
            .iter()
            .any(|(p, _)| p.key.as_ref() == "energy_data.energy_gen_cool")
    );
}
#[test]
fn only_documented_cloud_boolean_string_alias_is_accepted() {
    assert_eq!(
        point(
            Scope::Webserver,
            &json!({"cloud_connected":"1"}),
            "cloud_connected",
            true
        )
        .1,
        Value::Bool(true)
    );
    assert_eq!(
        point(
            Scope::Webserver,
            &json!({"cloud_connected":"0"}),
            "cloud_connected",
            true
        )
        .1,
        Value::Bool(false)
    );
    assert!(
        !points(Scope::Zone, &json!({"on":"1"}), false)
            .iter()
            .any(|(p, _)| p.key.as_ref() == "on")
    );
}
#[test]
fn humidity_zero_sentinel_is_not_a_measured_zero_percent() {
    assert!(
        !points(Scope::Zone, &json!({"humidity":0}), true)
            .iter()
            .any(|(p, _)| p.key.as_ref() == "humidity")
    );
    assert_eq!(
        point(Scope::Zone, &json!({"humidity":1}), "humidity", true).1,
        Value::Int(1)
    );
}
#[test]
fn registry_covers_all_documented_parameter_schemas_and_write_contracts() {
    let inventory: Json =
        serde_json::from_str(include_str!("../../../integrations/airzone/coverage.json")).unwrap();
    let actual = inventory["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["schema"].as_str().unwrap().split('.').next().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    // Independent schema inventory from the official 1.78 document, including
    // metadata and addressing; example-only fields are checked separately.
    let expected="systemID acs_temp acs_setpoint acs_mintemp acs_maxtemp acs_power acs_powerful mc_connected power system_firmware system_type manufacturer user_mode system_technology zoneID iaqsensorID name thermos_type thermos_firmware thermos_radio battery coverage consumption_ue disch_comp_temp_ue exch_heat_temp_iu exch_heat_temp_ue pe_ue ext_temp temp_return work_temp on double_sp setpoint roomtemp maxtemp mintemp coolsetpoint coolmaxtemp coolmintemp heatsetpoint heatmaxtemp heatmintemp sleep temp_step modes erv_modes mode erv_mode speeds speed erv_speed erv_speed_2 speed_values erv_speed_values erv_speed_values_2 speed_type erv_bypass_position erv_co2_sensor erv_pm25_sensor erv_extract_air_temp erv_supply_air_temp erv_outdoor_air_inlet_temp erv_tvoc_sensor erv_use_filter_time erv_humidity erv_use_filter_percentage erv_pollution iaq_mode_vent iaq_index iaq_score co2_value pm2_5_value pm10_value tvoc_value pressure_value coldstages coldstage heatstages heatstage humidity hp_configuration units air_demand floor_demand cold_demand heat_demand aq_mode aq_quality aq_thrlow aq_thrhigh slats_vswing slats_hswing slats_vertical slats_horizontal heatangle coldangle errors master_zoneID eco_adapt antifreeze open_window heat_angle_values cold_angle_values heat_angle_conf cold_angle_conf qAdapt driver version mac wifi_channel wifi_quality wifi_rssi interface ws_firmware lmachine_firmware cloud_connected ws_type energy_data".split_whitespace().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(expected.len(), 118);
    assert!(expected.is_subset(&actual));
    let expected_writes="acs_setpoint acs_power acs_powerful user_mode name on setpoint coolsetpoint heatsetpoint sleep mode erv_mode speed erv_speed erv_speed_2 iaq_mode_vent coldstage heatstage aq_mode aq_thrlow aq_thrhigh slats_vswing slats_hswing slats_vertical slats_horizontal heatangle coldangle heat_angle_conf cold_angle_conf qAdapt driver".split_whitespace().collect::<std::collections::BTreeSet<_>>();
    let writes = inventory["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| !f["write_scopes"].as_array().unwrap().is_empty())
        .map(|f| f["schema"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(writes, expected_writes);
    let keys = registry()
        .iter()
        .map(|f| f.key.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(keys.len(), registry().len());
    for lang in [
        include_str!("../../../locales/fr/airzone_fields.json"),
        include_str!("../../../locales/en/airzone_fields.json"),
    ] {
        let labels: Json = serde_json::from_str(lang).unwrap();
        assert!(
            registry()
                .iter()
                .all(|f| labels[&f.label].as_str().is_some_and(|s| !s.is_empty()))
        );
    }
}
#[test]
fn known_field_filter_only_accepts_documented_wire_keys_and_aliases() {
    for key in [
        "roomTemp",
        "roomtemp",
        "CO2_Value",
        "energy_data",
        "energy_consump",
        "acs_power",
        "driver",
    ] {
        assert!(is_known_field(key), "{key}");
    }
    for key in ["authorization", "token", "arbitrary", "setpoint_backup"] {
        assert!(!is_known_field(key));
    }
}
#[test]
fn humidity_sentinel_and_documented_version_alias_are_preserved_as_diagnostics() {
    let (p, v) = point(
        Scope::Zone,
        &json!({"humidity":0}),
        "humidity_reported",
        true,
    );
    assert_eq!(p.semantic, moli_core::Semantic::Other);
    assert_eq!(v, Value::Int(0));
    assert_eq!(
        point(Scope::Version, &json!({"schema":"1.78"}), "version", true).1,
        Value::from("1.78")
    );
    assert!(normalize(&json!({"schema":"1.78","version":"1.80"})).is_err());
}
#[test]
#[allow(clippy::too_many_lines)] // One independently specified example per documented write parameter.
fn every_documented_writable_parameter_has_a_typed_address_free_encoder() {
    let cases = [
        (
            Scope::Dhw,
            json!({"units":0,"acs_setpoint":50,"acs_mintemp":25,"acs_maxtemp":65}),
            "acs_setpoint",
            Value::Float(55.),
            json!(55.),
        ),
        (
            Scope::Dhw,
            json!({"acs_power":0}),
            "acs_power",
            Value::Bool(true),
            json!(1),
        ),
        (
            Scope::Dhw,
            json!({"acs_powerful":1}),
            "acs_powerful",
            Value::Bool(false),
            json!(0),
        ),
        (
            Scope::System,
            json!({"user_mode":1}),
            "user_mode",
            Value::from("5"),
            json!(5),
        ),
        (
            Scope::Zone,
            json!({"name":"Zone 01"}),
            "name",
            Value::from("Zone 02"),
            json!("Zone 02"),
        ),
        (
            Scope::Zone,
            json!({"on":1}),
            "on",
            Value::Bool(false),
            json!(0),
        ),
        (
            Scope::Zone,
            json!({"units":0,"setpoint":21,"minTemp":15,"maxTemp":30,"temp_step":0.5}),
            "target_temperature",
            Value::Float(21.5),
            json!(21.5),
        ),
        (
            Scope::Zone,
            json!({"units":0,"coolsetpoint":25,"coolmintemp":18,"coolmaxtemp":30}),
            "coolsetpoint",
            Value::Float(24.),
            json!(24.),
        ),
        (
            Scope::Zone,
            json!({"units":0,"heatsetpoint":21,"heatmintemp":15,"heatmaxtemp":30}),
            "heatsetpoint",
            Value::Float(20.),
            json!(20.),
        ),
        (
            Scope::Zone,
            json!({"sleep":0}),
            "sleep",
            Value::from("60"),
            json!(60),
        ),
        (
            Scope::System,
            json!({"mode":3,"modes":[1,2,3]}),
            "mode",
            Value::from("cool"),
            json!(2),
        ),
        (
            Scope::System,
            json!({"erv_mode":2,"erv_modes":[2,6]}),
            "erv_mode",
            Value::from("6"),
            json!(6),
        ),
        (
            Scope::System,
            json!({"speed":2,"speed_values":[0,2]}),
            "fan_speed",
            Value::from("0"),
            json!(0),
        ),
        (
            Scope::System,
            json!({"erv_speed":3,"erv_speed_values":[1,3,7]}),
            "erv_speed",
            Value::from("7"),
            json!(7),
        ),
        (
            Scope::System,
            json!({"erv_speed_2":1,"erv_speed_values_2":[1,3]}),
            "erv_speed_2",
            Value::from("3"),
            json!(3),
        ),
        (
            Scope::Iaq,
            json!({"iaq_mode_vent":1}),
            "iaq_mode_vent",
            Value::from("2"),
            json!(2),
        ),
        (
            Scope::Zone,
            json!({"coldStage":3,"coldStages":3}),
            "coldStage",
            Value::from("1"),
            json!(1),
        ),
        (
            Scope::Zone,
            json!({"heatStage":1,"heatStages":3}),
            "heatStage",
            Value::from("3"),
            json!(3),
        ),
        (
            Scope::Zone,
            json!({"aq_mode":1}),
            "aq_mode",
            Value::from("2"),
            json!(2),
        ),
        (
            Scope::Zone,
            json!({"aq_thrlow":10,"aq_thrhigh":40}),
            "aq_thrlow",
            Value::Float(20.),
            json!(20),
        ),
        (
            Scope::Zone,
            json!({"aq_thrlow":10,"aq_thrhigh":40}),
            "aq_thrhigh",
            Value::Float(80.),
            json!(80),
        ),
        (
            Scope::System,
            json!({"slats_vswing":0}),
            "slats_vswing",
            Value::Bool(true),
            json!(1),
        ),
        (
            Scope::System,
            json!({"slats_hswing":1}),
            "slats_hswing",
            Value::Bool(false),
            json!(0),
        ),
        (
            Scope::System,
            json!({"slats_vertical":1}),
            "slats_vertical",
            Value::from("3"),
            json!(3),
        ),
        (
            Scope::System,
            json!({"slats_horizontal":1}),
            "slats_horizontal",
            Value::from("2"),
            json!(2),
        ),
        (
            Scope::Zone,
            json!({"heatangle":0}),
            "heatangle",
            Value::from("1"),
            json!(1),
        ),
        (
            Scope::Zone,
            json!({"coldangle":0}),
            "coldangle",
            Value::from("2"),
            json!(2),
        ),
        (
            Scope::Zone,
            json!({"heat_angle_conf":45,"heat_angle_values":[45,90]}),
            "heat_angle_conf",
            Value::from("90"),
            json!(90),
        ),
        (
            Scope::Zone,
            json!({"cold_angle_conf":90,"cold_angle_values":[45,90]}),
            "cold_angle_conf",
            Value::from("45"),
            json!(45),
        ),
        (
            Scope::System,
            json!({"qAdapt":0}),
            "qAdapt",
            Value::from("4"),
            json!(4),
        ),
        (
            Scope::Integration,
            json!({"driver":"old"}),
            "driver",
            Value::from("moli-0.1"),
            json!("moli-0.1"),
        ),
    ];
    assert_eq!(cases.len(), 31);
    for (scope, raw, key, value, want) in cases {
        let (wire, got) = encode(scope, &raw, key, &value).unwrap_or_else(|e| panic!("{key}: {e}"));
        assert_eq!(got, want, "{key}");
        assert!(!["systemID", "zoneID", "iaqsensorID"].contains(&wire.as_str()));
        assert!(
            points(scope, &raw, true)
                .iter()
                .all(|(p, _)| !p.access.write)
        );
    }
}
#[test]
fn malformed_capabilities_and_unknown_states_never_expand_writable_choices() {
    for options in [json!([1, 1]), json!([1, 99]), json!(3), json!(null)] {
        let raw = json!({"erv_speed":3,"erv_speed_values":options});
        let (p, v) = point(Scope::System, &raw, "erv_speed", false);
        assert_eq!(v, Value::from("3"));
        assert!(!p.access.write);
    }
    let raw = json!({"erv_speed":3,"erv_speed_values":[1,2,4,5,6,7]});
    assert_eq!(
        point(Scope::System, &raw, "erv_speed", false).1,
        Value::from("3")
    );
    assert!(encode(Scope::System, &raw, "erv_speed", &Value::from("3")).is_err());
    assert_eq!(
        encode(Scope::System, &raw, "erv_speed", &Value::from("7"))
            .unwrap()
            .1,
        json!(7)
    );
    for key in [
        "erv_power",
        "eco_adapt",
        "antifreeze",
        "open_window",
        "hp_configuration",
    ] {
        let raw = json!({"erv_power":1,"eco_adapt":"manual","antifreeze":1,"open_window":0,"hp_configuration":1});
        assert!(
            encode(Scope::Zone, &raw, key, &Value::Int(1)).is_err(),
            "{key}"
        );
    }
}
