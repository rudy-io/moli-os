//! What travels on the battery's MQTT link: the requests Moli publishes on
//! `…/action` and the answers the battery publishes on `…/status` and
//! `…/event`. Pure: no clock, no network.
//!
//! Every message is `{type, eventId, messageId, ts, body}`. The body's `cmd`
//! says what it holds: 106 the battery's own state and settings, 120 the
//! whole system's flows, 110 its meters and plugs (not read here: the flows
//! already carry the meter's power).

use moli_core::Value;
use serde_json::{Value as Json, json};

/// Operating states (`stat`), in the order of their codes.
pub const STATES: [&str; 6] = [
    "normal",
    "attente",
    "alarme",
    "defaut",
    "veille",
    "charge_faible",
];

/// The reserve's bounds (%), the ones the battery itself accepts.
pub const RESERVE_MIN: f64 = 5.0;
pub const RESERVE_MAX: f64 = 49.0;

fn message(kind: u16, event: u8, body: &Json, token: &str, ts: u64, id: u32) -> String {
    json!({
        "type": kind,
        "eventId": event,
        "messageId": id,
        "ts": ts,
        "token": token,
        "body": body,
    })
    .to_string()
}

/// One round of reads: flows (105), state and settings (2).
#[must_use]
pub fn reads(token: &str, ts: u64, id: u32) -> [String; 2] {
    [
        message(105, 0, &Json::Null, token, ts, id),
        message(2, 0, &Json::Null, token, ts, id.wrapping_add(1)),
    ]
}

/// The order that sets the reserve (the level under which the battery
/// stops feeding the home), in whole percent.
#[must_use]
pub fn set_reserve(percent: u8, token: &str, ts: u64, id: u32) -> String {
    message(
        1,
        3,
        &json!({ "cmd": 5, "rc": 1, "socDischgLimit": percent }),
        token,
        ts,
        id,
    )
}

fn number(body: &Json, key: &str) -> Option<f64> {
    body.get(key).and_then(Json::as_f64)
}

fn push(out: &mut Vec<(&'static str, Value)>, key: &'static str, value: Option<f64>) {
    if let Some(value) = value {
        out.push((key, Value::Float(value)));
    }
}

/// The points an answer gives (`None`: not an answer this driver reads).
/// A field the battery leaves out is left as it was, never set to zero.
#[must_use]
pub fn readings(payload: &[u8]) -> Option<Vec<(&'static str, Value)>> {
    let message: Json = serde_json::from_slice(payload).ok()?;
    let body = message.get("body")?;
    let mut out = Vec::new();
    match body.get("cmd").and_then(Json::as_u64)? {
        // The system: what flows where, in watts.
        120 => {
            push(&mut out, "battery", number(body, "soc"));
            push(&mut out, "charge_power", number(body, "batInPw"));
            push(&mut out, "discharge_power", number(body, "batOutPw"));
            push(&mut out, "solar_power", number(body, "pvPw"));
            push(&mut out, "home_power", number(body, "otherLoadPw"));
            // At the main feed, signed: bought minus sent out. Only when the
            // battery sees its meter, or the zeros would read as « nothing ».
            if body.get("ctStat").and_then(Json::as_u64) == Some(1)
                && let (Some(bought), Some(sent)) =
                    (number(body, "inGridSidePw"), number(body, "outGridSidePw"))
            {
                out.push(("grid_power", Value::Float(bought - sent)));
            }
            if let Some(code) = body.get("stat").and_then(Json::as_u64) {
                let state = usize::try_from(code).ok().and_then(|c| STATES.get(c));
                out.push((
                    "status",
                    state.map_or(Value::Null, |s| Value::Text((*s).into())),
                ));
            }
        }
        // The battery itself: settings, temperature, Wi-Fi.
        106 => {
            push(&mut out, "reserve", number(body, "socDischgLimit"));
            push(&mut out, "charge_limit", number(body, "socChgLimit"));
            push(&mut out, "max_output_power", number(body, "maxOutPw"));
            // Tenths of a degree.
            push(
                &mut out,
                "temperature",
                number(body, "cellTemp").map(|t| t / 10.0),
            );
            push(&mut out, "signal", number(body, "wsig"));
            push(&mut out, "packs", number(body, "batNum"));
        }
        _ => return None,
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(readings: &[(&'static str, Value)], key: &str) -> Option<Value> {
        readings
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn the_system_answer_gives_the_flows() {
        // Captured from a real battery (serial and addresses replaced).
        let r = readings(include_bytes!(
            "../../../integrations/jackery/fixtures/system.json"
        ))
        .unwrap();
        assert_eq!(get(&r, "battery"), Some(Value::Float(21.0)));
        assert_eq!(get(&r, "charge_power"), Some(Value::Float(0.0)));
        assert_eq!(get(&r, "home_power"), Some(Value::Float(1426.0)));
        assert_eq!(get(&r, "grid_power"), Some(Value::Float(1426.0)), "buying");
        assert_eq!(get(&r, "status"), Some(Value::Text("normal".into())));
        assert_eq!(get(&r, "reserve"), None, "settings: the other answer");
    }

    #[test]
    fn the_battery_answer_gives_settings_and_health() {
        let r = readings(include_bytes!(
            "../../../integrations/jackery/fixtures/status.json"
        ))
        .unwrap();
        assert_eq!(get(&r, "reserve"), Some(Value::Float(24.0)));
        assert_eq!(get(&r, "charge_limit"), Some(Value::Float(100.0)));
        assert_eq!(get(&r, "max_output_power"), Some(Value::Float(2500.0)));
        assert_eq!(get(&r, "temperature"), Some(Value::Float(18.4)));
        assert_eq!(get(&r, "signal"), Some(Value::Float(-81.0)));
        assert_eq!(get(&r, "packs"), Some(Value::Float(2.0)));
        assert_eq!(get(&r, "charge_power"), None, "flows: the other answer");
    }

    #[test]
    fn sending_out_is_negative_and_a_lost_meter_gives_no_grid_power() {
        let sending = br#"{"body":{"cmd":120,"ctStat":1,"inGridSidePw":0,"outGridSidePw":766}}"#;
        assert_eq!(
            get(&readings(sending).unwrap(), "grid_power"),
            Some(Value::Float(-766.0))
        );
        let lost =
            br#"{"body":{"cmd":120,"ctStat":0,"inGridSidePw":0,"outGridSidePw":0,"soc":40}}"#;
        let r = readings(lost).unwrap();
        assert_eq!(get(&r, "grid_power"), None);
        assert_eq!(get(&r, "battery"), Some(Value::Float(40.0)));
    }

    #[test]
    fn unknown_states_and_other_messages_are_not_guessed() {
        let odd = br#"{"body":{"cmd":120,"stat":9}}"#;
        assert_eq!(get(&readings(odd).unwrap(), "status"), Some(Value::Null));
        // Meters and plugs, garbage, a body without `cmd`.
        assert!(
            readings(include_bytes!(
                "../../../integrations/jackery/fixtures/meters.json"
            ))
            .is_none()
        );
        assert!(readings(b"not json").is_none());
        assert!(readings(br#"{"body":{"soc":40}}"#).is_none());
        assert!(readings(br#"{"body":null}"#).is_none());
    }

    #[test]
    fn requests_have_the_shape_the_battery_expects() {
        let [flows, state] = reads("t0k", 1_700_000_000, 41);
        let flows: Json = serde_json::from_str(&flows).unwrap();
        assert_eq!(flows["type"], 105);
        assert_eq!(flows["token"], "t0k");
        assert_eq!(flows["body"], Json::Null);
        let state: Json = serde_json::from_str(&state).unwrap();
        assert_eq!(
            (state["type"].as_u64(), state["messageId"].as_u64()),
            (Some(2), Some(42))
        );
        let order: Json = serde_json::from_str(&set_reserve(15, "t0k", 1_700_000_000, 7)).unwrap();
        assert_eq!(
            (order["type"].as_u64(), order["eventId"].as_u64()),
            (Some(1), Some(3))
        );
        assert_eq!(
            order["body"],
            json!({ "cmd": 5, "rc": 1, "socDischgLimit": 15 })
        );
    }
}
