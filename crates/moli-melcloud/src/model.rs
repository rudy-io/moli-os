//! The answers of the MELCloud API decoded and its orders built: pure, without
//! any I/O.

use std::collections::HashSet;

use anyhow::Context as _;
use moli_core::Value;
use serde_json::Value as Json;

/// `DeviceType` of an air-to-air unit, the only kind this driver describes.
const AIR_TO_AIR: u64 = 0;
/// `AccessLevel` of a shared account: it can look, not command.
const GUEST: u64 = 3;

/// One air conditioner, as the service last heard from it.
#[derive(Clone, Debug, PartialEq)]
pub struct AirUnit {
    pub id: u64,
    pub building: u64,
    pub name: String,
    pub room: Option<String>,
    pub power: Option<bool>,
    pub mode: Option<&'static str>,
    pub temperature: Option<f64>,
    pub target: Option<f64>,
    pub offline: bool,
    /// Lowest and highest setpoint over every mode, and the setpoint step.
    pub limits: Option<(f64, f64)>,
    pub step: Option<f64>,
    /// What the unit can do, as the service says.
    pub modes: Vec<&'static str>,
    pub fan_speeds: Vec<String>,
    pub fan: Option<String>,
    pub vertical_vane: Option<&'static str>,
    pub horizontal_vane: Option<&'static str>,
    pub has_vertical_vane: bool,
    pub has_horizontal_vane: bool,
    /// Bounds of the setpoint per mode: heat, cool and dry, auto.
    pub bounds: Bounds,
    pub locks: Locks,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bounds {
    pub heat: Option<(f64, f64)>,
    pub cool_dry: Option<(f64, f64)>,
    pub auto: Option<(f64, f64)>,
}

/// What this account may not change on this unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Locks {
    pub guest: bool,
    pub power: bool,
    pub mode: bool,
    pub target: bool,
}

pub const VERTICAL_VANE: [(&str, u64); 7] = [
    ("auto", 0),
    ("1", 1),
    ("2", 2),
    ("3", 3),
    ("4", 4),
    ("5", 5),
    ("balayage", 7),
];

pub const HORIZONTAL_VANE: [(&str, u64); 8] = [
    ("auto", 0),
    ("1", 1),
    ("2", 2),
    ("3", 3),
    ("4", 4),
    ("5", 5),
    ("écartées", 8),
    ("balayage", 12),
];

const MODES: [(&str, u64); 5] = [
    ("chaud", 1),
    ("déshumidification", 2),
    ("froid", 3),
    ("ventilation", 7),
    ("auto", 8),
];

fn vane(table: &[(&'static str, u64)], code: Option<u64>) -> Option<&'static str> {
    let code = code?;
    table
        .iter()
        .find(|(_, value)| *value == code)
        .map(|(name, _)| *name)
}

/// The modes Moli already knows from its other air conditioners.
fn mode(code: u64) -> Option<&'static str> {
    vane(&MODES, Some(code))
}

fn modes(unit: &Json) -> Vec<&'static str> {
    // A flag the service does not send is not a refusal: only `false` is.
    let can = |key: &str| unit[key].as_bool().unwrap_or(true);
    MODES
        .iter()
        .filter(|(name, _)| match *name {
            "chaud" => can("CanHeat"),
            "froid" => can("CanCool"),
            "déshumidification" => can("CanDry"),
            "auto" => can("ModelSupportsAuto"),
            _ => true,
        })
        .map(|(name, _)| *name)
        .collect()
}

fn fan_name(speed: u64) -> String {
    if speed == 0 {
        "auto".to_owned()
    } else {
        speed.to_string()
    }
}

fn fan_speeds(unit: &Json) -> Vec<String> {
    if unit["ModelSupportsFanSpeed"].as_bool() != Some(true) {
        return Vec::new();
    }
    let top = unit["NumberOfFanSpeeds"].as_u64().unwrap_or(0).min(9);
    let first = u64::from(unit["HasAutomaticFanSpeed"].as_bool() != Some(true));
    (first..=top).map(fan_name).collect()
}

fn bound(unit: &Json, low: &str, high: &str) -> Option<(f64, f64)> {
    let pair = (celsius(unit, low)?, celsius(unit, high)?);
    (pair.0 < pair.1).then_some(pair)
}

fn celsius(unit: &Json, key: &str) -> Option<f64> {
    unit[key]
        .as_f64()
        .filter(|n| n.is_finite() && (-100.0..=100.0).contains(n))
}

fn limits(unit: &Json) -> Option<(f64, f64)> {
    let bounds = |keys: [&str; 3]| {
        keys.iter()
            .filter_map(|k| celsius(unit, k))
            .collect::<Vec<_>>()
    };
    let low = bounds(["MinTempHeat", "MinTempCoolDry", "MinTempAutomatic"])
        .into_iter()
        .reduce(f64::min)?;
    let high = bounds(["MaxTempHeat", "MaxTempCoolDry", "MaxTempAutomatic"])
        .into_iter()
        .reduce(f64::max)?;
    (low < high).then_some((low, high))
}

fn entries(node: &Json) -> impl Iterator<Item = &Json> {
    node.as_array().into_iter().flatten()
}

/// A building keeps its devices directly, in areas, on floors, or in a floor's areas.
fn placed(structure: &Json) -> Vec<&Json> {
    let mut found: Vec<&Json> = entries(&structure["Devices"]).collect();
    for area in entries(&structure["Areas"]) {
        found.extend(entries(&area["Devices"]));
    }
    for floor in entries(&structure["Floors"]) {
        found.extend(entries(&floor["Devices"]));
        for area in entries(&floor["Areas"]) {
            found.extend(entries(&area["Devices"]));
        }
    }
    found
}

fn decode(entry: &Json) -> Option<AirUnit> {
    let unit = &entry["Device"];
    if unit["DeviceType"].as_u64() != Some(AIR_TO_AIR) {
        return None;
    }
    Some(AirUnit {
        id: entry["DeviceID"].as_u64()?,
        building: entry["BuildingID"].as_u64().unwrap_or(0),
        name: entry["DeviceName"].as_str()?.to_owned(),
        room: entry["AreaName"].as_str().map(str::to_owned),
        power: unit["Power"].as_bool(),
        mode: unit["OperationMode"].as_u64().and_then(mode),
        temperature: celsius(unit, "RoomTemperature"),
        target: celsius(unit, "SetTemperature"),
        offline: unit["Offline"].as_bool().unwrap_or(false),
        limits: limits(unit),
        step: unit["TemperatureIncrement"]
            .as_f64()
            .filter(|n| *n > 0.0 && *n <= 5.0),
        modes: modes(unit),
        fan_speeds: fan_speeds(unit),
        fan: unit["FanSpeed"].as_u64().map(fan_name),
        vertical_vane: vane(&VERTICAL_VANE, unit["VaneVerticalDirection"].as_u64()),
        horizontal_vane: vane(&HORIZONTAL_VANE, unit["VaneHorizontalDirection"].as_u64()),
        has_vertical_vane: unit["ModelSupportsVaneVertical"].as_bool() == Some(true),
        has_horizontal_vane: unit["ModelSupportsVaneHorizontal"].as_bool() == Some(true),
        bounds: Bounds {
            heat: bound(unit, "MinTempHeat", "MaxTempHeat"),
            cool_dry: bound(unit, "MinTempCoolDry", "MaxTempCoolDry"),
            auto: bound(unit, "MinTempAutomatic", "MaxTempAutomatic"),
        },
        locks: Locks {
            guest: entry["AccessLevel"].as_u64() == Some(GUEST),
            power: unit["ProhibitPower"].as_bool().unwrap_or(false),
            mode: unit["ProhibitOperationMode"].as_bool().unwrap_or(false),
            target: unit["ProhibitSetTemperature"].as_bool().unwrap_or(false),
        },
    })
}

/// Every air conditioner of the account, once each: a shared device is listed
/// again in each building that sees it.
pub fn units(body: &[u8]) -> anyhow::Result<Vec<AirUnit>> {
    let buildings: Json = serde_json::from_slice(body)
        .ok()
        .context(moli_i18n::tr!("pilotes.melcloud.reponse_illisible"))?;
    let buildings = buildings
        .as_array()
        .context(moli_i18n::tr!("pilotes.melcloud.reponse_illisible"))?;
    let mut seen = HashSet::new();
    Ok(buildings
        .iter()
        .flat_map(|building| placed(&building["Structure"]))
        .filter_map(decode)
        .filter(|unit| seen.insert(unit.id))
        .collect())
}

/// One change to a unit: the field `Device/SetAta` reads, and the flag that
/// makes it read that field and leave the others alone.
#[derive(Clone, Debug, PartialEq)]
pub struct Order {
    field: &'static str,
    flag: u64,
    value: Json,
}

/// The order for a point set to `value` (already checked against the point),
/// or what stops it. A setpoint is rounded to the unit's step, and refused
/// outside the range of the mode the unit is in: the point's range covers
/// every mode, the unit would answer with something else than what was asked.
pub fn order(unit: &AirUnit, key: &str, value: &Value) -> Result<Order, String> {
    let refused = || moli_i18n::tr!("pilotes.melcloud.ne_se_regle_pas", point = key);
    let text = || match value {
        Value::Text(text) => Some(text.as_ref()),
        _ => None,
    };
    let found = |table: &[(&str, u64)]| {
        let wanted = text()?;
        table
            .iter()
            .find(|(name, _)| *name == wanted)
            .map(|(_, code)| Json::from(*code))
    };
    let (field, flag, wire) = match key {
        "on" => match value {
            Value::Bool(on) => ("Power", 0x01, Some(Json::Bool(*on))),
            _ => return Err(refused()),
        },
        "mode" => ("OperationMode", 0x02, found(&MODES)),
        "target_temperature" => {
            let wanted = value.as_f64().ok_or_else(refused)?;
            let step = unit.step.unwrap_or(0.5);
            let rounded = (wanted / step).round() * step;
            if let Some((low, high)) = range_of(unit)
                && !(low..=high).contains(&rounded)
            {
                return Err(moli_i18n::tr!(
                    "pilotes.melcloud.hors_plage",
                    low = low,
                    high = high
                ));
            }
            ("SetTemperature", 0x04, Some(Json::from(rounded)))
        }
        "fan_speed" => {
            let code = text().and_then(|name| match name {
                "auto" => Some(0),
                other => other.parse::<u64>().ok(),
            });
            ("SetFanSpeed", 0x08, code.map(Json::from))
        }
        "vertical_vane" => ("VaneVertical", 0x10, found(&VERTICAL_VANE)),
        // Accepted by the service, but an unit that is off never applies it.
        "horizontal_vane" if unit.power == Some(false) => {
            return Err(moli_i18n::tr!("pilotes.melcloud.ailette_a_l_arret"));
        }
        "horizontal_vane" => ("VaneHorizontal", 0x100, found(&HORIZONTAL_VANE)),
        _ => return Err(refused()),
    };
    let value = wire.ok_or_else(refused)?;
    Ok(Order { field, flag, value })
}

/// What an accepted order changes in what the next orders are checked against,
/// while the list has not caught up.
pub fn apply(unit: &mut AirUnit, key: &str, value: &Value) {
    match (key, value) {
        ("on", Value::Bool(on)) => unit.power = Some(*on),
        ("mode", Value::Text(wanted)) => {
            if let Some(known) = MODES.iter().find(|(name, _)| *name == wanted.as_ref()) {
                unit.mode = Some(known.0);
            }
        }
        _ => {}
    }
}

fn range_of(unit: &AirUnit) -> Option<(f64, f64)> {
    match unit.mode? {
        "chaud" => unit.bounds.heat,
        "froid" | "déshumidification" => unit.bounds.cool_dry,
        "auto" => unit.bounds.auto,
        // Ventilation has no setpoint of its own: the widest range holds.
        _ => unit.limits,
    }
}

/// `Device/Get`'s answer turned into the body of `Device/SetAta`: the whole
/// state goes back, the service reads only what the flags name. A body without
/// the unit's state is refused: it would not be an order but a guess.
pub fn with_order(state: &[u8], order: &Order) -> anyhow::Result<Json> {
    let mut state: Json = serde_json::from_slice(state)
        .ok()
        .filter(|state: &Json| state["DeviceID"].is_u64())
        .context(moli_i18n::tr!("pilotes.melcloud.reponse_illisible"))?;
    state["EffectiveFlags"] = Json::from(order.flag);
    state["HasPendingCommand"] = Json::Bool(true);
    state[order.field] = order.value.clone();
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIXTURE: &[u8] =
        include_bytes!("../../../integrations/melcloud/fixtures/list_devices.json");

    fn device(id: u64, kind: u64) -> Json {
        json!({
            "DeviceID": id,
            "DeviceName": format!("Unit {id}"),
            "Device": { "DeviceType": kind, "Power": true, "OperationMode": 1 },
        })
    }

    #[test]
    fn a_real_answer_gives_both_units() {
        let found = units(FIXTURE).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "Salon");
        assert_eq!(found[0].power, Some(false));
        assert_eq!(found[0].mode, Some("chaud"));
        assert_eq!(found[0].temperature, Some(20.0));
        assert_eq!(found[0].target, Some(19.5));
        assert_eq!(found[0].limits, Some((10.0, 31.0)));
        assert_eq!(found[0].step, Some(0.5));
        assert_eq!(found[1].mode, Some("froid"));
        assert_eq!(found[0].fan_speeds, ["auto", "1", "2", "3", "4", "5"]);
        assert_eq!(found[0].vertical_vane, Some("auto"));
        assert_eq!(found[0].horizontal_vane, Some("balayage"));
        assert_eq!(found[0].modes.len(), 5);
    }

    fn salon() -> AirUnit {
        units(FIXTURE).unwrap().remove(0)
    }

    fn body_for(unit: &AirUnit, key: &str, value: &Value) -> Json {
        let state = json!({ "DeviceID": unit.id, "Power": false, "SetTemperature": 19.5,
                            "EffectiveFlags": 0, "WeatherObservations": [1] });
        with_order(
            state.to_string().as_bytes(),
            &order(unit, key, value).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn an_order_names_only_what_it_changes_and_keeps_the_rest() {
        let body = body_for(&salon(), "on", &Value::Bool(true));
        assert_eq!(body["Power"], json!(true));
        assert_eq!(body["EffectiveFlags"], json!(0x01));
        assert_eq!(body["HasPendingCommand"], json!(true));
        assert_eq!(body["SetTemperature"], json!(19.5));
        assert_eq!(body["WeatherObservations"], json!([1]));
    }

    #[test]
    fn every_order_has_its_own_flag_and_wire_value() {
        let mut unit = salon();
        unit.power = Some(true);
        for (key, value, field, flag, wire) in [
            (
                "mode",
                Value::from("froid"),
                "OperationMode",
                0x02,
                json!(3),
            ),
            (
                "target_temperature",
                Value::Float(20.0),
                "SetTemperature",
                0x04,
                json!(20.0),
            ),
            ("fan_speed", Value::from("3"), "SetFanSpeed", 0x08, json!(3)),
            (
                "fan_speed",
                Value::from("auto"),
                "SetFanSpeed",
                0x08,
                json!(0),
            ),
            (
                "vertical_vane",
                Value::from("balayage"),
                "VaneVertical",
                0x10,
                json!(7),
            ),
            (
                "horizontal_vane",
                Value::from("écartées"),
                "VaneHorizontal",
                0x100,
                json!(8),
            ),
        ] {
            let body = body_for(&unit, key, &value);
            assert_eq!(body[field], wire, "{key}");
            assert_eq!(body["EffectiveFlags"], json!(flag), "{key}");
        }
    }

    #[test]
    fn a_setpoint_is_rounded_to_the_step_and_held_to_the_mode_in_use() {
        let mut unit = salon();
        let body = body_for(&unit, "target_temperature", &Value::Float(20.3));
        assert_eq!(body["SetTemperature"], json!(20.5));
        // 10 °C exists in heating only.
        assert!(order(&unit, "target_temperature", &Value::Float(10.0)).is_ok());
        unit.mode = Some("froid");
        assert!(order(&unit, "target_temperature", &Value::Float(10.0)).is_err());
    }

    #[test]
    fn the_horizontal_vane_waits_for_the_unit_to_be_on() {
        let mut unit = salon();
        unit.power = Some(false);
        let wanted = Value::from("3");
        assert!(order(&unit, "horizontal_vane", &wanted).is_err());
        apply(&mut unit, "on", &Value::Bool(true));
        assert!(order(&unit, "horizontal_vane", &wanted).is_ok());
    }

    #[test]
    fn the_next_order_is_checked_against_the_last_one_accepted() {
        let mut unit = salon();
        unit.mode = Some("chaud");
        assert!(order(&unit, "target_temperature", &Value::Float(10.0)).is_ok());
        apply(&mut unit, "mode", &Value::from("froid"));
        assert!(order(&unit, "target_temperature", &Value::Float(10.0)).is_err());
        apply(&mut unit, "mode", &Value::from("turbo"));
        assert_eq!(unit.mode, Some("froid"));
    }

    #[test]
    fn what_the_unit_does_not_have_is_never_sent() {
        let unit = salon();
        assert!(order(&unit, "mode", &Value::from("turbo")).is_err());
        assert!(order(&unit, "fan_speed", &Value::from("vite")).is_err());
        assert!(order(&unit, "temperature", &Value::Float(20.0)).is_err());
        assert!(order(&unit, "on", &Value::from("oui")).is_err());
    }

    #[test]
    fn a_state_that_is_not_a_unit_is_not_answered_with_an_order() {
        let order = order(&salon(), "on", &Value::Bool(true)).unwrap();
        assert!(with_order(b"<html>", &order).is_err());
        assert!(with_order(b"{}", &order).is_err());
    }

    #[test]
    fn a_guest_or_a_locked_setting_is_read_as_such() {
        let body = json!([{ "Structure": { "Devices": [{
            "DeviceID": 1,
            "DeviceName": "Unit",
            "AccessLevel": 3,
            "Device": { "DeviceType": 0, "ProhibitPower": true },
        }] } }]);
        let found = &units(body.to_string().as_bytes()).unwrap()[0];
        assert!(found.locks.guest);
        assert!(found.locks.power);
        assert!(!found.locks.mode);
    }

    #[test]
    fn every_place_of_a_building_is_read_and_a_device_counts_once() {
        let body = json!([
            { "Structure": {
                "Devices": [device(1, 0)],
                "Areas": [{ "Devices": [device(2, 0)] }],
                "Floors": [{ "Devices": [device(3, 0)], "Areas": [{ "Devices": [device(4, 0)] }] }],
            } },
            { "Structure": { "Devices": [device(1, 0)] } },
        ]);
        let ids: Vec<u64> = units(body.to_string().as_bytes())
            .unwrap()
            .iter()
            .map(|u| u.id)
            .collect();
        assert_eq!(ids, [1, 2, 3, 4]);
    }

    #[test]
    fn heat_pumps_and_ventilation_are_not_air_conditioners() {
        let body = json!([{ "Structure": { "Devices": [device(1, 1), device(2, 3)] } }]);
        assert!(units(body.to_string().as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn a_missing_or_absurd_reading_is_unknown_and_never_invented() {
        let body = json!([{ "Structure": { "Devices": [{
            "DeviceID": 1,
            "DeviceName": "Unit",
            "Device": { "DeviceType": 0, "RoomTemperature": 500.0, "OperationMode": 42 },
        }] } }]);
        let found = &units(body.to_string().as_bytes()).unwrap()[0];
        assert_eq!(found.temperature, None);
        assert_eq!(found.target, None);
        assert_eq!(found.mode, None);
        assert_eq!(found.power, None);
        assert_eq!(found.limits, None);
    }

    #[test]
    fn an_answer_that_is_not_a_list_of_buildings_is_refused() {
        assert!(units(b"<html>").is_err());
        assert!(units(b"{}").is_err());
    }
}
