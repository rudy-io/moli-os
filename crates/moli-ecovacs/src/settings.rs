//! Settings and station alerts reported by the X8 PRO OMNI.

use moli_core::{Access, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_i18n::tr;
use moli_runtime::DriverCtx;
use serde_json::{Value as Json, json};

const SPEEDS: [(&str, i64); 4] = [("quiet", 1000), ("normal", 0), ("max", 1), ("max_plus", 2)];
const MODES: [(&str, i64); 4] = [
    ("vacuum", 1),
    ("mop", 2),
    ("vacuum_and_mop", 0),
    ("mop_after_vacuum", 3),
];
const WATER_MAX: u8 = 50;

pub(super) fn points(writable: bool) -> Vec<PointSpec> {
    let enumeration = |entries: &[(&str, i64)]| Kind::Enum {
        values: entries.iter().map(|(name, _)| (*name).into()).collect(),
    };
    let point = |key: &'static str, label: String, kind, write| PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read: true, write },
        unit: None,
        semantic: if write {
            Semantic::Control
        } else {
            Semantic::Other
        },
    };
    vec![
        point(
            "fan_speed",
            tr!("ecovacs.fan_speed"),
            enumeration(&SPEEDS),
            writable,
        ),
        point(
            "work_mode",
            tr!("ecovacs.work_mode"),
            enumeration(&MODES),
            writable,
        ),
        point(
            "water_level",
            tr!("ecovacs.water_level"),
            Kind::Numeric {
                min: Some(0.0),
                max: Some(f64::from(WATER_MAX)),
                step: Some(1.0),
            },
            writable,
        ),
        point(
            "mop_attached",
            tr!("ecovacs.mop_attached"),
            Kind::Binary,
            false,
        ),
        point(
            "clean_water_empty",
            tr!("ecovacs.clean_water_empty"),
            Kind::Binary,
            false,
        ),
        point(
            "dirty_water_full",
            tr!("ecovacs.dirty_water_full"),
            Kind::Binary,
            false,
        ),
        point("fault", tr!("ecovacs.fault"), Kind::Binary, false),
        point("detail", tr!("ecovacs.error_detail"), Kind::Text, false),
    ]
}

fn integer(value: &Json) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}

fn choice(entries: &[(&'static str, i64)], value: &Json) -> Option<&'static str> {
    let code = integer(value)?;
    entries
        .iter()
        .find(|(_, id)| *id == code)
        .map(|(name, _)| *name)
}

pub(super) fn order(key: &str, value: &Value) -> Option<(&'static str, Json)> {
    if key == "water_level" {
        let level = value.as_f64()?;
        if level.fract() != 0.0 || !(0.0..=f64::from(WATER_MAX)).contains(&level) {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let level = level as u8;
        return Some(("setWaterInfo", json!({"customAmount": level})));
    }
    let (entries, command, field) = match key {
        "fan_speed" => (&SPEEDS, "setSpeed", "speed"),
        "work_mode" => (&MODES, "setWorkMode", "mode"),
        _ => return None,
    };
    let Value::Text(setting) = value else {
        return None;
    };
    let (_, code) = entries.iter().find(|(name, _)| *name == setting.as_ref())?;
    Some((command, json!({(field): code})))
}

pub(super) fn publish(
    ctx: &DriverCtx,
    id: &DeviceId,
    speed: Option<&Json>,
    mode: Option<&Json>,
    water: Option<&Json>,
    errors: Option<&Json>,
) {
    if let Some(name) = speed.and_then(|body| choice(&SPEEDS, &body["data"]["speed"])) {
        ctx.set_state(id, "fan_speed", Value::Text(name.into()));
    }
    if let Some(name) = mode.and_then(|body| choice(&MODES, &body["data"]["mode"])) {
        ctx.set_state(id, "work_mode", Value::Text(name.into()));
    }
    if let Some(body) = water {
        let data = &body["data"];
        if let Some(amount) =
            integer(&data["customAmount"]).filter(|n| (0..=i64::from(WATER_MAX)).contains(n))
        {
            ctx.set_state(id, "water_level", Value::Int(amount));
        }
        let enable = &data["enable"];
        if let Some(attached) = enable.as_bool().or_else(|| match integer(enable) {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        }) {
            ctx.set_state(id, "mop_attached", Value::Bool(attached));
        }
    }
    if let Some(alerts) = errors.and_then(parse_alerts) {
        ctx.set_state(id, "clean_water_empty", Value::Bool(alerts.clean_empty));
        ctx.set_state(id, "dirty_water_full", Value::Bool(alerts.dirty_full));
        ctx.set_state(id, "fault", Value::Bool(alerts.last.is_some()));
        ctx.set_state(id, "detail", Value::Text(alerts.detail().into()));
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Alerts {
    clean_empty: bool,
    dirty_full: bool,
    last: Option<i64>,
}

impl Alerts {
    fn detail(&self) -> String {
        match self.last {
            None => String::new(),
            Some(301) => tr!("ecovacs.clean_water_empty"),
            Some(302) => tr!("ecovacs.dirty_water_full"),
            Some(303) => tr!("ecovacs.clean_water_missing"),
            Some(304) => tr!("ecovacs.dirty_water_missing"),
            Some(code) => tr!("ecovacs.error_code", code = code),
        }
    }
}

fn parse_alerts(body: &Json) -> Option<Alerts> {
    let mut alerts = Alerts {
        clean_empty: false,
        dirty_full: false,
        last: None,
    };
    for value in body["data"]["code"].as_array()? {
        let code = integer(value)?;
        alerts.clean_empty |= code == 301;
        alerts.dirty_full |= code == 302;
        if code != 0 && code != 100 {
            alerts.last = Some(code);
        }
    }
    Some(alerts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_use_the_x8_protocol_codes() {
        let observed: Json = serde_json::from_str(include_str!(
            "../../../integrations/ecovacs/fixtures/x8-settings.json"
        ))
        .unwrap();
        assert_eq!(
            choice(&SPEEDS, &observed["getSpeed"]["data"]["speed"]),
            Some("max")
        );
        assert_eq!(
            choice(&MODES, &observed["getWorkMode"]["data"]["mode"]),
            Some("vacuum_and_mop")
        );
        assert_eq!(
            order("fan_speed", &Value::Text("max_plus".into())),
            Some(("setSpeed", json!({"speed":2})))
        );
        assert_eq!(
            order("work_mode", &Value::Text("mop_after_vacuum".into())),
            Some(("setWorkMode", json!({"mode":3})))
        );
        assert_eq!(
            order("water_level", &Value::Float(29.0)),
            Some(("setWaterInfo", json!({"customAmount":29})))
        );
        assert!(order("water_level", &Value::Float(29.5)).is_none());
        assert!(order("water_level", &Value::Float(51.0)).is_none());
    }

    #[test]
    fn station_alerts_keep_both_tank_states_and_only_clear_on_a_valid_read() {
        let alerts = parse_alerts(&json!({"data":{"code":[301, "302", 100]}})).unwrap();
        assert_eq!(
            alerts,
            Alerts {
                clean_empty: true,
                dirty_full: true,
                last: Some(302)
            }
        );
        assert_eq!(
            parse_alerts(&json!({"data":{"code":[]}})).unwrap().last,
            None
        );
        assert!(parse_alerts(&json!({"data":{"code":null}})).is_none());
    }
}
