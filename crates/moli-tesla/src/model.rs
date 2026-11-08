//! What Tesla's API says, in Moli's words: the account's vehicles, what a car
//! reports (normalized to SI units), the orders it takes, and the points it
//! offers (the demo fixture mirrors them).

use moli_core::{Access, Kind, PointSpec, Semantic, Unit};
use serde_json::Value as Json;

/// One vehicle of the account (`GET /api/1/vehicles`).
#[derive(Clone, Debug, PartialEq)]
pub struct Vehicle {
    /// Tesla's stable id: the number in every command path.
    pub id: i64,
    /// What the owner named the car.
    pub name: String,
    /// `"model3"`, `"modely"`, … (the badge, when the API knows it).
    pub car_type: Option<String>,
    /// `false` when the car is asleep: its readings then stay at their last ones.
    pub awake: bool,
}

impl Vehicle {
    /// The badge for the API's `car_type`, when it is a known one.
    #[must_use]
    pub fn badge(&self) -> Option<&'static str> {
        match self.car_type.as_deref() {
            Some("model3") => Some("Model 3"),
            Some("modely") => Some("Model Y"),
            Some("models") => Some("Model S"),
            Some("modelx") => Some("Model X"),
            _ => None,
        }
    }
}

/// `{"state":"online","response":[…]}` (a bare list is also accepted).
#[must_use]
pub fn vehicles_of(answer: &Json) -> Vec<Vehicle> {
    let Some(list) = answer.get("response").and_then(Json::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|v| {
            let id = v.get("id").or_else(|| v.get("vehicle_id")).and_then(|n| {
                n.as_i64()
                    .or_else(|| n.as_str().and_then(|s| s.parse().ok()))
            })?;
            Some(Vehicle {
                id,
                name: v
                    .get("display_name")
                    .and_then(Json::as_str)
                    .or_else(|| v.get("nickname").and_then(Json::as_str))
                    .unwrap_or("Tesla")
                    .to_owned(),
                car_type: v.get("car_type").and_then(Json::as_str).map(str::to_owned),
                awake: v
                    .get("vehicle_state")
                    .or_else(|| v.get("state"))
                    .and_then(Json::as_str)
                    .is_some_and(|s| s != "asleep"),
            })
        })
        .collect()
}

/// A car's readings, normalized: kilometers, Celsius, watts, 0–100.
/// What the car does not say stays `None` (its last word is kept).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Readings {
    pub soc: Option<f64>,
    pub range_km: Option<f64>,
    pub charging: Option<bool>,
    pub charge_power_w: Option<f64>,
    pub plugged: Option<bool>,
    pub doors_locked: Option<bool>,
    pub sentry: Option<bool>,
    pub inside_c: Option<f64>,
    pub outside_c: Option<f64>,
}

/// One mile, in kilometers.
const MILE_KM: f64 = 1.609_344;

/// Fahrenheit to Celsius.
fn celsius(v: f64, fahrenheit: bool) -> f64 {
    if fahrenheit {
        (v - 32.0) * 5.0 / 9.0
    } else {
        v
    }
}

/// Whether the car is actively drawing charge. Current Tesla responses report
/// a `charging_state` string ("Charging", "Finishing", …); others report a
/// boolean `is_charging`.
fn charging_of(charge: Option<&Json>) -> Option<bool> {
    charge.and_then(|c| {
        c.get("is_charging").and_then(Json::as_bool).or_else(|| {
            c.get("charging_state")
                .and_then(Json::as_str)
                .map(|s| matches!(s, "Charging" | "Finishing"))
        })
    })
}

/// Whether the doors are locked: `vehicle_state.locked` in current responses,
/// `door_locked` in legacy ones.
fn locked_of(state: Option<&Json>) -> Option<bool> {
    state.and_then(|s| {
        s.get("locked")
            .and_then(Json::as_bool)
            .or_else(|| s.get("door_locked").and_then(Json::as_bool))
    })
}

/// Whether sentry mode is engaged: `vehicle_state.sentry_mode` in current
/// responses, `sentry_mode_engaged` in legacy ones.
fn sentry_of(state: Option<&Json>) -> Option<bool> {
    state.and_then(|s| {
        s.get("sentry_mode")
            .and_then(Json::as_bool)
            .or_else(|| s.get("sentry_mode_engaged").and_then(Json::as_bool))
    })
}

/// Whether a cable is connected. Current Tesla responses report
/// `conn_charge_cable` (a cable type, or "Invalid" when none); others report
/// a boolean `is_charge_plug_connected`.
fn plugged_of(charge: Option<&Json>) -> Option<bool> {
    charge.and_then(|c| {
        c.get("is_charge_plug_connected")
            .and_then(Json::as_bool)
            .or_else(|| {
                c.get("conn_charge_cable")
                    .and_then(Json::as_str)
                    .map(|cable| !cable.is_empty() && !cable.eq_ignore_ascii_case("invalid"))
            })
    })
}

/// `GET /api/1/vehicles/{id}/vehicle_data`, normalized: Tesla reports every
/// `*_range` value in **miles, irrespective of the car's GUI settings**, so
/// they are turned into kilometers on the way in; temperatures follow the
/// car's `gui_settings` (°F on a US car). An asleep car answers
/// `response: null`: every reading stays `None`.
#[must_use]
pub fn readings_of(answer: &Json) -> Readings {
    let data = answer
        .get("response")
        .filter(|r| r.is_object())
        .unwrap_or(answer);
    let charge = data.get("charge_state");
    let state = data.get("vehicle_state");
    let climate = data.get("climate_state");
    let gui = data.get("gui_settings");
    let fahrenheit = gui
        .and_then(|g| g.get("gui_temperature_units"))
        .and_then(Json::as_str)
        == Some("F");
    let num = |o: Option<&Json>| o.and_then(Json::as_f64);
    Readings {
        soc: num(charge.and_then(|c| c.get("battery_level"))),
        range_km: num(charge.and_then(|c| c.get("battery_range"))).map(|v| v * MILE_KM),
        charging: charging_of(charge),
        // The API's `charger_power` is reported in kilowatts; the point is in
        // watts, so the kilowatts are scaled up on the way in.
        charge_power_w: num(charge.and_then(|c| c.get("charger_power"))).map(|kw| kw * 1000.0),
        plugged: plugged_of(charge),
        doors_locked: locked_of(state),
        sentry: sentry_of(state),
        inside_c: num(climate.and_then(|c| c.get("inside_temp"))).map(|v| celsius(v, fahrenheit)),
        outside_c: num(climate.and_then(|c| c.get("outside_temp"))).map(|v| celsius(v, fahrenheit)),
    }
}

/// A car order, as the dashboard and the automations write it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    ChargeStart,
    ChargeStop,
    Lock,
    Unlock,
    SentryOn,
    SentryOff,
}

/// The six orders, whatever their letter case.
#[must_use]
pub fn order_of(text: &str) -> Option<Order> {
    match text.trim().to_ascii_lowercase().as_str() {
        "start_charging" => Some(Order::ChargeStart),
        "stop_charging" => Some(Order::ChargeStop),
        "lock" => Some(Order::Lock),
        "unlock" => Some(Order::Unlock),
        "sentry_on" => Some(Order::SentryOn),
        "sentry_off" => Some(Order::SentryOff),
        _ => None,
    }
}

/// What goes to the API: the command in the path, and a body for
/// `set_sentry_mode` (the only one that takes one).
#[must_use]
pub fn api_command(order: Order) -> (&'static str, Option<&'static str>) {
    match order {
        Order::ChargeStart => ("charge_start", None),
        Order::ChargeStop => ("charge_stop", None),
        Order::Lock => ("door_lock", None),
        Order::Unlock => ("door_unlock", None),
        Order::SentryOn => ("set_sentry_mode", Some(r#"{"on":true}"#)),
        Order::SentryOff => ("set_sentry_mode", Some(r#"{"on":false}"#)),
    }
}

/// What the dashboard pasted back: the sign-in's last address (the app's
/// callback URI, e.g. `https://example.com/moli-tesla?code=…`), or the code
/// itself copied from that page. `None`: something else (the middle pages,
/// a failure…).
#[must_use]
pub fn code_of(pasted: &str) -> Option<String> {
    let pasted = pasted.trim();
    if let Some(pos) = pasted.find("code=") {
        let tail = &pasted[pos + "code=".len()..];
        let code = percent_decode(tail.split(['&', '#']).next().unwrap_or(tail).trim());
        return code_is(&code).then_some(code);
    }
    // The code copied from the page's field.
    code_is(pasted).then(|| pasted.to_owned())
}

/// A sign-in code: a long opaque token, with no whitespace and none of the
/// signs of an address (`/`, `:`) — a page's address is not a code.
fn code_is(code: &str) -> bool {
    code.len() >= 20
        && !code
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b == b'/' || b == b':')
}

/// `%XX` back to its byte (an address bar sometimes encodes).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    if !bytes.contains(&b'%') {
        return text.to_owned();
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(decoded) = u8::from_str_radix(hex, 16) {
                out.push(decoded);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The points a car offers, in this order (the demo fixture mirrors them).
#[must_use]
pub fn points() -> Vec<PointSpec> {
    let mut points = readings();
    points.push(command());
    points
}

/// What the car says (read-only), in the order the dashboard shows it.
fn readings() -> Vec<PointSpec> {
    vec![
        point(
            "soc",
            &moli_i18n::tr!("pilotes.tesla.batterie"),
            Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: Some(1.0),
            },
            Some(Unit::Percent),
            Semantic::Battery,
            false,
        ),
        point(
            "range",
            &moli_i18n::tr!("pilotes.tesla.autonomie"),
            Kind::Numeric {
                min: Some(0.0),
                max: None,
                step: None,
            },
            Some(Unit::Other("km".into())),
            Semantic::Other,
            false,
        ),
        binary("charging", &moli_i18n::tr!("pilotes.tesla.en_recharge")),
        point(
            "charge_power",
            &moli_i18n::tr!("pilotes.tesla.puissance_charge"),
            Kind::Numeric {
                min: Some(0.0),
                max: None,
                step: None,
            },
            Some(Unit::Watt),
            Semantic::Power,
            false,
        ),
        binary("plugged_in", &moli_i18n::tr!("pilotes.tesla.prise")),
        // The lock's own point: `true` is locked, as Tesla reports it. With this
        // semantic the guard holds every order that may open the car — an agent's
        // « unlock » asks a person first.
        point(
            "doors_locked",
            &moli_i18n::tr!("pilotes.tesla.portes"),
            Kind::Binary,
            None,
            Semantic::Lock,
            false,
        ),
        binary("sentry", &moli_i18n::tr!("pilotes.tesla.vigie")),
        binary("awake", &moli_i18n::tr!("pilotes.tesla.reveillee")),
        temp("inside_temp", &moli_i18n::tr!("pilotes.tesla.interieur")),
        temp("outside_temp", &moli_i18n::tr!("pilotes.tesla.exterieur")),
    ]
}

/// A boolean reading, no unit, nothing to say about it.
fn binary(key: &str, label: &str) -> PointSpec {
    point(key, label, Kind::Binary, None, Semantic::Other, false)
}

/// A temperature reading, in Celsius.
fn temp(key: &str, label: &str) -> PointSpec {
    point(
        key,
        label,
        Kind::Numeric {
            min: Some(-60.0),
            max: Some(100.0),
            step: Some(0.1),
        },
        Some(Unit::Celsius),
        Semantic::Temperature,
        false,
    )
}

/// What the house gives the car (write-only): the six orders.
fn command() -> PointSpec {
    point(
        "command",
        &moli_i18n::tr!("pilotes.tesla.commande"),
        Kind::Text,
        None,
        Semantic::Control,
        true,
    )
}

fn point(
    key: &str,
    label: &str,
    kind: Kind,
    unit: Option<Unit>,
    semantic: Semantic,
    write: bool,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: !write,
            write,
        },
        unit,
        semantic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const VEHICLES: &str = r#"{
      "state": "online",
      "response": [
        {
          "id": "123456789",
          "vehicle_id": "123456789",
          "display_name": "Tesla",
          "car_type": "modely",
          "vehicle_state": "online"
        },
        {
          "id": 987654321,
          "vehicle_id": "987654321",
          "nickname": "La petite",
          "car_type": "model3",
          "vehicle_state": "asleep"
        },
        { "vehicle_state": "online" }
      ]
    }"#;

    #[test]
    fn the_vehicles_are_read() {
        let list = vehicles_of(&serde_json::from_str(VEHICLES).unwrap());
        assert_eq!(list.len(), 2, "a vehicle without an id is skipped");
        assert_eq!(list[0].id, 123_456_789);
        assert_eq!(list[0].name, "Tesla");
        assert_eq!(list[0].badge(), Some("Model Y"));
        assert!(list[0].awake);
        assert_eq!(list[1].id, 987_654_321);
        assert_eq!(list[1].name, "La petite");
        assert_eq!(list[1].badge(), Some("Model 3"));
        assert!(!list[1].awake);
    }

    #[test]
    fn an_unknown_car_type_has_no_badge() {
        let answer: Json = json!({"response": [{"id": 1, "car_type": "modelq"}]});
        assert_eq!(vehicles_of(&answer)[0].badge(), None);
    }

    const DATA: &str = r#"{
      "response": {
        "charge_state": {
          "battery_level": 64.0,
          "battery_range": 262.0,
          "charging_state": "Charging",
          "charger_power": 7.2,
          "conn_charge_cable": "IEC"
        },
        "vehicle_state": { "locked": true, "sentry_mode": false },
        "climate_state": { "inside_temp": 21.4, "outside_temp": 14.2 },
        "gui_settings": { "gui_range_units": "km", "gui_temperature_units": "C" }
      },
      "vehicle_id": "123456789"
    }"#;

    #[test]
    fn the_readings_are_normalized() {
        let r = readings_of(&serde_json::from_str(DATA).unwrap());
        assert_eq!(r.soc, Some(64.0));
        // The car's GUI says "km", but the API reports `battery_range` in
        // miles regardless: the 262 miles become kilometers.
        assert_eq!(r.range_km, Some(262.0 * MILE_KM));
        assert_eq!(r.charging, Some(true));
        assert_eq!(r.charge_power_w, Some(7200.0));
        assert_eq!(r.plugged, Some(true));
        assert_eq!(r.doors_locked, Some(true));
        assert_eq!(r.sentry, Some(false));
        assert!((r.inside_c.unwrap() - 21.4).abs() < 1e-9);
        assert!((r.outside_c.unwrap() - 14.2).abs() < 1e-9);
    }

    #[test]
    fn the_legacy_charge_booleans_are_honored_too() {
        let answer: Json = json!({
            "response": {
                "charge_state": {
                    "is_charging": false,
                    "is_charge_plug_connected": true
                }
            }
        });
        let r = readings_of(&answer);
        assert_eq!(r.charging, Some(false));
        assert_eq!(r.plugged, Some(true));
    }

    #[test]
    fn the_legacy_vehicle_state_fields_are_honored_too() {
        let answer: Json = json!({
            "response": {
                "vehicle_state": {
                    "door_locked": false,
                    "sentry_mode_engaged": true
                }
            }
        });
        let r = readings_of(&answer);
        assert_eq!(r.doors_locked, Some(false));
        assert_eq!(r.sentry, Some(true));
    }

    #[test]
    fn finishing_keeps_charging_and_a_missing_cable_unplugs() {
        let finishing: Json = json!({
            "response": {
                "charge_state": { "charging_state": "Finishing", "conn_charge_cable": "IEC" }
            }
        });
        let r = readings_of(&finishing);
        assert_eq!(r.charging, Some(true));
        assert_eq!(r.plugged, Some(true));
        let disconnected: Json = json!({
            "response": {
                "charge_state": { "charging_state": "Disconnected", "conn_charge_cable": "Invalid" }
            }
        });
        let r = readings_of(&disconnected);
        assert_eq!(r.charging, Some(false));
        assert_eq!(r.plugged, Some(false));
    }

    #[test]
    fn the_range_is_miles_and_the_temps_follow_the_gui() {
        // A US car (GUI mi/°F): the API answers in miles and °F.
        let answer: Json = json!({
            "response": {
                "charge_state": { "battery_level": 64, "battery_range": 163.0 },
                "climate_state": { "inside_temp": 70.5, "outside_temp": 57.6 },
                "gui_settings": { "gui_range_units": "mi", "gui_temperature_units": "F" }
            }
        });
        let r = readings_of(&answer);
        assert!(
            (r.range_km.unwrap() - 262.32).abs() < 0.01,
            "miles become kilometers: {}",
            r.range_km.unwrap()
        );
        assert!(
            (r.inside_c.unwrap() - 21.388).abs() < 0.01,
            "°F become °C: {}",
            r.inside_c.unwrap()
        );
        // A metric car (GUI km/°C): the range is STILL in miles (Tesla's API
        // does not follow the GUI), the temperatures are already in °C.
        let answer: Json = json!({
            "response": {
                "charge_state": { "battery_level": 95, "battery_range": 241.54 },
                "climate_state": { "inside_temp": 8.1, "outside_temp": 6.0 },
                "gui_settings": { "gui_range_units": "km", "gui_temperature_units": "C" }
            }
        });
        let r = readings_of(&answer);
        assert!(
            (r.range_km.unwrap() - 388.72).abs() < 0.01,
            "miles become kilometers even in GUI km: {}",
            r.range_km.unwrap()
        );
        assert!((r.inside_c.unwrap() - 8.1).abs() < 1e-9);
        assert!((r.outside_c.unwrap() - 6.0).abs() < 1e-9);
    }

    #[test]
    fn an_asleep_car_says_nothing() {
        let answer: Json = json!({ "response": null, "vehicle_id": "123456789" });
        assert_eq!(readings_of(&answer), Readings::default());
    }

    #[test]
    fn a_field_absent_from_the_answer_is_none() {
        let answer: Json = json!({ "response": { "charge_state": {} } });
        assert_eq!(readings_of(&answer), Readings::default());
    }

    #[test]
    fn the_orders_are_mapped() {
        assert_eq!(order_of("start_charging"), Some(Order::ChargeStart));
        assert_eq!(order_of("Stop_Charging"), Some(Order::ChargeStop));
        assert_eq!(order_of("lock"), Some(Order::Lock));
        assert_eq!(order_of("unlock"), Some(Order::Unlock));
        assert_eq!(order_of("sentry_on"), Some(Order::SentryOn));
        assert_eq!(order_of("sentry_off"), Some(Order::SentryOff));
        assert_eq!(order_of("boost"), None);
        assert_eq!(order_of(""), None);
    }

    #[test]
    fn each_order_has_its_api_command() {
        assert_eq!(api_command(Order::ChargeStart), ("charge_start", None));
        assert_eq!(api_command(Order::ChargeStop), ("charge_stop", None));
        assert_eq!(api_command(Order::Lock), ("door_lock", None));
        assert_eq!(api_command(Order::Unlock), ("door_unlock", None));
        assert_eq!(
            api_command(Order::SentryOn),
            ("set_sentry_mode", Some(r#"{"on":true}"#))
        );
        assert_eq!(
            api_command(Order::SentryOff),
            ("set_sentry_mode", Some(r#"{"on":false}"#))
        );
    }

    #[test]
    fn the_pasted_address_yields_its_code() {
        let url = "https://auth.tesla.com/redirect?code=AQAbCdefGHIJKLMNOPQRSTUVWXYZ-0123456789";
        assert_eq!(
            code_of(url),
            Some("AQAbCdefGHIJKLMNOPQRSTUVWXYZ-0123456789".to_owned())
        );
        let url =
            "https://auth.tesla.com/redirect?code=AbCdefGHIJKLMNOPQRSTUVWXYZ1234567890x&state=xyz";
        assert_eq!(
            code_of(url),
            Some("AbCdefGHIJKLMNOPQRSTUVWXYZ1234567890x".to_owned())
        );
        // A middle page of the sign-in, or garbage: nothing to exchange.
        assert_eq!(code_of("https://auth.tesla.com/authorize"), None);
        assert_eq!(code_of("code="), None);
        assert_eq!(code_of("https://auth.tesla.com/redirect?state=xyz"), None);
    }

    #[test]
    fn a_bare_code_is_accepted_too() {
        let code = "AQAbCdefGHIJKLMNOPQRSTUVWXYZ-0123456789";
        assert_eq!(code_of(code), Some(code.to_owned()));
        assert_eq!(code_of("  "), None);
        assert_eq!(code_of("short"), None);
    }

    #[test]
    fn a_percent_encoded_code_is_decoded() {
        let url = "https://auth.tesla.com/redirect?code=AQAbC%2BdefGHIJKLMNOPQRSTUVWXYZ01234";
        assert_eq!(
            code_of(url),
            Some("AQAbC+defGHIJKLMNOPQRSTUVWXYZ01234".to_owned())
        );
    }

    #[test]
    fn the_points_cover_the_readings() {
        let points = points();
        let keys: Vec<&str> = points.iter().map(|p| p.key.as_ref()).collect();
        assert_eq!(
            keys,
            vec![
                "soc",
                "range",
                "charging",
                "charge_power",
                "plugged_in",
                "doors_locked",
                "sentry",
                "awake",
                "inside_temp",
                "outside_temp",
                "command"
            ]
        );
        assert!(matches!(points[0].semantic, Semantic::Battery));
        assert!(matches!(points[3].semantic, Semantic::Power));
        let doors = &points[5];
        assert_eq!(&*doors.key, "doors_locked");
        assert!(doors.access.read && !doors.access.write);
        assert!(matches!(doors.semantic, Semantic::Lock));
        let command = &points[10];
        assert_eq!(command.kind, Kind::Text);
        assert!(!command.access.read && command.access.write);
        assert!(matches!(command.semantic, Semantic::Control));
    }
}
