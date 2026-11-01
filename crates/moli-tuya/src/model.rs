//! Tuya devices as Moli sees them: the device file (no secret in it) and
//! the translation between data points (DPs) and Moli points.
//!
//! A DP has an id (« 1 »), a code (« switch_1 ») and a type. The code
//! becomes the point key; integers carry a decimal `scale`.

use std::collections::BTreeMap;
use std::sync::Arc;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::light::{self, Hsv, Light};
use crate::protocol::Version;

/// `data/tuya.json`, written by `moli-os tuya import`. Hand-editable: `ip`,
/// `version` and `room` are kept across re-imports.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeviceFile {
    pub devices: Vec<TuyaDevice>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TuyaDevice {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    /// Found by discovery when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Version>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    /// Sub-device of a gateway (Zigbee/BLE behind a Tuya hub): not
    /// reachable directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// A sub-device's address on its gateway (`cid` in local frames).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// A sub-device's gateway (its device id): with `node_id`, the
    /// sub-device is reached locally through the gateway's connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default)]
    pub dps: Vec<Dp>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dp {
    pub id: u32,
    pub code: String,
    #[serde(rename = "type")]
    pub kind: DpKind,
    #[serde(default)]
    pub writable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default)]
    pub scale: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub range: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DpKind {
    Boolean,
    Integer,
    Enum,
    String,
    Json,
    Raw,
    Bitmap,
}

impl Dp {
    /// Unit as Moli knows it, and the factor turning raw (after scale)
    /// into it (Tuya says mA: Moli shows A).
    fn unit(&self) -> (Option<Unit>, f64) {
        let Some(raw) = self
            .unit
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        else {
            return (None, 1.0);
        };
        match raw {
            "℃" | "°C" | "C" => (Some(Unit::Celsius), 1.0),
            "kwh" | "kWh" | "KWH" | "kW·h" | "度" => (Some(Unit::KiloWattHour), 1.0),
            "mA" => (Some(Unit::Ampere), 0.001),
            other => (Unit::parse(other), 1.0),
        }
    }

    /// The lamp's setting this DP holds, on a lamp (`lamp`: its category is
    /// one of [`light::LAMPS`]) only.
    fn setting(&self, lamp: bool) -> Option<Light> {
        lamp.then(|| Light::of(&self.code)).flatten()
    }

    /// The point's key: a lamp's settings take the names every lamp of Moli
    /// has (`brightness`, `color_temp`, `color`), the rest keeps its code.
    #[must_use]
    pub fn key(&self, lamp: bool) -> &str {
        match self.setting(lamp) {
            Some(light) => light.key(),
            None => &self.code,
        }
    }

    /// The bounds the bulb gives, else Tuya's usual ones (`_v2`: 10–1000);
    /// a maximum of zero or less is not one.
    fn bounds(&self) -> (f64, f64) {
        let max = light::scale(self.max.unwrap_or(1000.0));
        (self.min.unwrap_or(10.0), max)
    }

    /// A lamp's setting, in Moli's terms.
    fn light_point(&self, light: Light) -> PointSpec {
        let (kind, unit, semantic, label) = match light {
            Light::Brightness => (
                Kind::Numeric {
                    min: Some(1.0),
                    max: Some(100.0),
                    step: Some(1.0),
                },
                Some(Unit::Percent),
                Semantic::Brightness,
                "pilotes.tuya.luminosite",
            ),
            Light::White => {
                let (cool, warm) = light::mired_bounds();
                (
                    Kind::Numeric {
                        min: Some(cool),
                        max: Some(warm),
                        step: Some(1.0),
                    },
                    Some(Unit::Other("mired".into())),
                    Semantic::ColorTemp,
                    "pilotes.tuya.temperature_couleur",
                )
            }
            Light::Colour => (Kind::Text, None, Semantic::Color, "pilotes.tuya.couleur"),
        };
        PointSpec {
            key: light.key().into(),
            label: moli_i18n::tr(label).into(),
            semantic,
            kind,
            access: Access {
                read: true,
                write: self.writable,
            },
            unit,
        }
    }

    fn divisor(&self) -> f64 {
        10f64.powi(i32::try_from(self.scale.min(9)).unwrap_or(0))
    }

    /// The Moli point for this DP; `None` for opaque types.
    fn point(&self, lamp: bool) -> Option<PointSpec> {
        if let Some(light) = self.setting(lamp) {
            return Some(self.light_point(light));
        }
        let (unit, factor) = self.unit();
        let kind = match self.kind {
            DpKind::Boolean => Kind::Binary,
            DpKind::Integer => {
                let k = |v: Option<f64>| v.map(|v| v / self.divisor() * factor);
                Kind::Numeric {
                    min: k(self.min),
                    max: k(self.max),
                    step: k(self.step),
                }
            }
            DpKind::Enum if !self.range.is_empty() => Kind::Enum {
                values: self.range.iter().map(|v| Arc::from(v.as_str())).collect(),
            },
            DpKind::Enum | DpKind::String => Kind::Text,
            DpKind::Json | DpKind::Raw | DpKind::Bitmap => return None,
        };
        let writable =
            self.writable && matches!(self.kind, DpKind::Boolean | DpKind::Integer | DpKind::Enum);
        Some(PointSpec {
            key: self.code.as_str().into(),
            label: label(&self.code).into(),
            semantic: semantic(&self.code, unit.as_ref()),
            kind,
            access: Access {
                read: true,
                write: writable,
            },
            unit,
        })
    }

    /// A raw DP value → Moli value.
    #[must_use]
    pub fn decode(&self, lamp: bool, raw: &Json) -> Option<Value> {
        let (_, max) = self.bounds();
        match self.setting(lamp) {
            Some(Light::Brightness) => {
                return Some(Value::Float(light::percent(raw.as_f64()?, max)));
            }
            Some(Light::White) => return Some(Value::Float(light::mired(raw.as_f64()?, max))),
            Some(Light::Colour) => return Some(Value::from(Hsv::parse(raw)?.hex().as_str())),
            None => {}
        }
        Some(match (self.kind, raw) {
            (DpKind::Boolean, Json::Bool(b)) => Value::Bool(*b),
            (DpKind::Integer, Json::Number(n)) => {
                let (_, factor) = self.unit();
                let v = n.as_f64()?;
                if self.scale == 0 && (factor - 1.0).abs() < f64::EPSILON {
                    n.as_i64().map_or(Value::Float(v), Value::Int)
                } else {
                    Value::Float(round(v / self.divisor() * factor, self.scale + 3))
                }
            }
            (DpKind::Enum | DpKind::String, Json::String(s)) => Value::Text(s.as_str().into()),
            _ => return None,
        })
    }

    /// A Moli value (already validated against the point) → raw DP value.
    #[must_use]
    pub fn encode(&self, lamp: bool, value: &Value) -> Option<Json> {
        let (min, max) = self.bounds();
        #[allow(clippy::cast_possible_truncation)]
        match self.setting(lamp) {
            Some(Light::Brightness) => {
                return Some(Json::from(
                    light::from_percent(value.as_f64()?, min, max) as i64
                ));
            }
            Some(Light::White) => {
                return Some(Json::from(light::from_mired(value.as_f64()?, max) as i64));
            }
            // At full value; an order takes the lamp's brightness (`order`).
            Some(Light::Colour) => {
                let Value::Text(hex) = value else { return None };
                return Some(Json::from(Hsv::from_hex(hex, 1000.0)?.tuya()));
            }
            None => {}
        }
        match (self.kind, value) {
            (DpKind::Boolean, Value::Bool(b)) => Some(Json::Bool(*b)),
            (DpKind::Integer, v) => {
                let (_, factor) = self.unit();
                let mut raw = (v.as_f64()? / factor * self.divisor()).round();
                // Devices refuse values off their step.
                if let Some(step) = self.step.filter(|s| *s > 1.0) {
                    raw = (raw / step).round() * step;
                }
                #[allow(clippy::cast_possible_truncation)]
                Some(Json::from(raw as i64))
            }
            (DpKind::Enum | DpKind::String, Value::Text(s)) => Some(Json::String(s.to_string())),
            _ => None,
        }
    }
}

fn round(v: f64, decimals: u32) -> f64 {
    let p = 10f64.powi(i32::try_from(decimals.min(9)).unwrap_or(0));
    (v * p).round() / p
}

/// What Tuya's standard codes mean, where the name alone does not say it.
/// `doorcontact_state` stays `Other`: it is true when the door is *open*,
/// the opposite of a `contact`.
fn semantic(code: &str, unit: Option<&Unit>) -> Semantic {
    match code {
        "battery_percentage" | "va_battery" => Semantic::Battery,
        "humidity_value" | "va_humidity" => Semantic::Humidity,
        "temp_current" | "va_temperature" => Semantic::Temperature,
        // A relay (`switch_1`, `switch_2`…) or a light (`switch_led`); a
        // fan's `fan_switch` is not the device's main on/off.
        "switch" | "switch_led" => Semantic::OnOff,
        c if c
            .strip_prefix("switch_")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            Semantic::OnOff
        }
        _ => Semantic::infer(code, unit),
    }
}

/// Tuya's standard codes in words; others: `cur_power` → « Cur power ».
fn label(code: &str) -> String {
    // The outlets of a strip and their timers: `switch_3` → « Marche 3 ».
    for (prefix, key) in [
        ("switch_", "pilotes.tuya.marche_n"),
        ("countdown_", "pilotes.tuya.minuterie_n"),
    ] {
        if let Some(n) = code
            .strip_prefix(prefix)
            .filter(|n| n.parse::<u8>().is_ok_and(|n| n >= 2))
        {
            return moli_i18n::tr!(key, n = n);
        }
    }
    let known = match code {
        "switch" | "switch_1" => "pilotes.tuya.marche",
        "relay_status" => "pilotes.tuya.retour_courant",
        "light_mode" => "pilotes.tuya.voyant",
        "child_lock" => "pilotes.tuya.securite_enfant",
        "cur_power" => "pilotes.tuya.puissance",
        "cur_voltage" => "pilotes.tuya.tension",
        "cur_current" => "pilotes.tuya.courant",
        "add_ele" => "pilotes.tuya.energie",
        "countdown_1" | "countdown" => "pilotes.tuya.minuterie",
        "doorcontact_state" => "pilotes.tuya.porte_ouverte",
        "watersensor_state" => "pilotes.tuya.fuite_eau",
        "battery_percentage" | "va_battery" => "pilotes.tuya.pile",
        "battery_state" => "pilotes.tuya.etat_pile",
        "pir" => "pilotes.tuya.mouvement",
        "temp_current" | "va_temperature" => "pilotes.tuya.temperature",
        "humidity_value" | "va_humidity" => "pilotes.tuya.humidite",
        "co2_value" => "pilotes.tuya.co2",
        "voc_value" => "pilotes.tuya.cov",
        "ch2o_value" => "pilotes.tuya.formaldehyde",
        "tds_in" => "pilotes.tuya.mineraux",
        "percent_control" => "pilotes.tuya.ouverture",
        "control" => "pilotes.tuya.commande",
        _ => "",
    };
    if !known.is_empty() {
        return moli_i18n::tr(known);
    }
    let words = code.replace('_', " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

impl TuyaDevice {
    #[must_use]
    pub fn device(&self, instance: &InstanceId) -> Device {
        Device {
            id: DeviceId::new(instance, &self.id),
            instance: instance.clone(),
            native_name: self.name.trim().into(),
            manufacturer: Some("Tuya".into()),
            model: self.product.as_deref().map(|p| p.trim().into()),
            description: None,
            native_room: self.room.as_deref().map(Into::into),
            members: Vec::new(),
            points: self
                .dps
                .iter()
                .filter_map(|d| d.point(self.is_lamp()))
                .collect(),
        }
    }

    /// A light or a dimmer: its brightness, white and colour take a lamp's
    /// names (see [`light::LAMPS`]).
    #[must_use]
    pub fn is_lamp(&self) -> bool {
        light::LAMPS.contains(&self.category.as_str())
    }

    /// A scene switch's press (`wxkg`, `switch_mode1`…): an occurrence, so
    /// the same press twice is two presses, not one value.
    #[must_use]
    pub fn is_press(&self, code: &str) -> bool {
        self.category == "wxkg" && code.starts_with("switch_mode")
    }

    /// The DP behind a point (`brightness` → `bright_value_v2`…).
    #[must_use]
    pub fn dp_by_key(&self, key: &str) -> Option<&Dp> {
        self.dps.iter().find(|d| d.key(self.is_lamp()) == key)
    }

    /// The DPs to send for a point set to `value` (already validated);
    /// `current` tells what the device shows now, by point key.
    #[must_use]
    pub fn order(
        &self,
        key: &str,
        value: &Value,
        current: &dyn Fn(&str) -> Option<Value>,
    ) -> Option<serde_json::Map<String, Json>> {
        let lamp = self.is_lamp();
        let dp = self.dp_by_key(key)?;
        let raw = dp.encode(lamp, value)?;
        let Some(setting) = dp.setting(lamp) else {
            return Some(serde_json::Map::from_iter([(dp.id.to_string(), raw)]));
        };
        let mode = self.dp_by_code("work_mode").map(|d| d.id);
        let colour = self
            .dps
            .iter()
            .find(|d| d.setting(lamp) == Some(Light::Colour))
            .map(|d| d.id);
        light::order(setting, value, dp.id, raw, mode, colour, current)
    }

    #[must_use]
    pub fn dp_by_code(&self, code: &str) -> Option<&Dp> {
        self.dps.iter().find(|d| d.code == code)
    }

    /// `{"1": true, "18": 120}` → `[(point key, value)]`, unknown DPs
    /// skipped. A bulb in colour shows its colour's own brightness.
    #[must_use]
    pub fn decode_dps(&self, dps: &serde_json::Map<String, Json>) -> Vec<(String, Value)> {
        let by_id: BTreeMap<u32, &Dp> = self.dps.iter().map(|d| (d.id, d)).collect();
        let mut values: Vec<(String, Value)> = dps
            .iter()
            .filter_map(|(id, raw)| {
                let dp = by_id.get(&id.parse().ok()?)?;
                Some((
                    dp.key(self.is_lamp()).to_owned(),
                    dp.decode(self.is_lamp(), raw)?,
                ))
            })
            .collect();
        if let Some(pct) = self.colour_brightness(dps) {
            let key = Light::Brightness.key();
            values.retain(|(k, _)| k != key);
            values.push((key.to_owned(), Value::Float(pct)));
        }
        values
    }

    /// The brightness of a bulb in colour, from its colour's value: when the
    /// message says so, or brings a colour without a white brightness (a
    /// change made in colour).
    fn colour_brightness(&self, dps: &serde_json::Map<String, Json>) -> Option<f64> {
        let raw = |setting: Light| {
            self.dps
                .iter()
                .find(|d| d.setting(self.is_lamp()) == Some(setting))
                .and_then(|d| dps.get(&d.id.to_string()))
        };
        let hsv = Hsv::parse(raw(Light::Colour)?)?;
        let mode = self
            .dp_by_code("work_mode")
            .and_then(|d| dps.get(&d.id.to_string()))
            .and_then(Json::as_str);
        let in_colour = mode.map_or(raw(Light::Brightness).is_none(), |m| m == "colour");
        in_colour.then(|| light::percent(hsv.v, 1000.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An LSC A60 RGB+CCT bulb, its DPs as the import files them.
    fn bulb() -> TuyaDevice {
        serde_json::from_value(serde_json::json!({
            "id": "bulb", "name": "Bureau", "category": "dj",
            "dps": [
                {"id": 20, "code": "switch_led", "type": "Boolean", "writable": true},
                {"id": 21, "code": "work_mode", "type": "Enum", "writable": true,
                 "range": ["white", "colour", "scene", "music"]},
                {"id": 22, "code": "bright_value_v2", "type": "Integer", "writable": true,
                 "min": 10.0, "max": 1000.0, "step": 1.0},
                {"id": 23, "code": "temp_value_v2", "type": "Integer", "writable": true,
                 "min": 0.0, "max": 1000.0, "step": 1.0},
                {"id": 24, "code": "colour_data_v2", "type": "Json", "writable": true},
                {"id": 25, "code": "scene_data_v2", "type": "Json", "writable": true}
            ]
        }))
        .unwrap()
    }

    /// Outside the lights and dimmers, a `bright_value` keeps its code and
    /// meaning: a gate module named « Portail » is still a gate to the
    /// guard, which would take a lamp's brightness for a lamp (#71).
    #[test]
    fn a_gate_with_a_brightness_is_still_a_gate() {
        let gate: TuyaDevice = serde_json::from_value(serde_json::json!({
            "id": "gate", "name": "Portail", "category": "kg",
            "dps": [
                {"id": 1, "code": "switch_1", "type": "Boolean", "writable": true},
                {"id": 2, "code": "bright_value", "type": "Integer", "writable": true,
                 "min": 10.0, "max": 1000.0}
            ]
        }))
        .unwrap();
        let device = gate.device(&InstanceId::from("tuya"));
        let keys: Vec<&str> = device.points.iter().map(|p| &*p.key).collect();
        assert_eq!(keys, ["switch_1", "bright_value"]);
        assert!(
            device
                .points
                .iter()
                .all(|p| p.semantic != Semantic::Brightness)
        );
        assert_eq!(
            moli_runtime::opener(&["Portail"], &device.points),
            moli_runtime::Opener::Gate
        );
        // The same DPs on a bulb are a lamp's.
        let mut bulb = gate;
        bulb.category = "dj".into();
        let device = bulb.device(&InstanceId::from("tuya"));
        assert!(device.points.iter().any(|p| &*p.key == "brightness"));
    }

    #[test]
    fn a_bulb_has_the_settings_of_any_lamp() {
        let device = bulb().device(&InstanceId::from("tuya"));
        let point = |key: &str| device.points.iter().find(|p| &*p.key == key).unwrap();
        assert_eq!(point("switch_led").semantic, Semantic::OnOff);
        let brightness = point("brightness");
        assert_eq!(brightness.semantic, Semantic::Brightness);
        assert_eq!(brightness.unit, Some(Unit::Percent));
        assert!(brightness.access.write);
        assert_eq!(point("color_temp").semantic, Semantic::ColorTemp);
        assert_eq!(point("color").semantic, Semantic::Color);
        assert_eq!(point("color").kind, Kind::Text);
        assert!(device.points.iter().all(|p| &*p.key != "scene_data_v2"));
    }

    #[test]
    fn a_bulb_reads_in_white_and_in_colour() {
        let bulb = bulb();
        let dps = |j: Json| j.as_object().unwrap().clone();
        // White: its brightness and white, as Moli says them.
        let v = bulb.decode_dps(&dps(serde_json::json!(
            {"20": true, "21": "white", "22": 1000, "23": 571, "24": "000003e801f4"}
        )));
        assert!(v.contains(&("brightness".into(), Value::Float(100.0))));
        assert!(v.contains(&("color_temp".into(), Value::Float(247.0))));
        assert!(v.contains(&("color".into(), Value::from("#ff0000"))));
        // Colour: the brightness is the colour's own (500 of 1000).
        let v = bulb.decode_dps(&dps(serde_json::json!(
            {"21": "colour", "22": 1000, "24": "000003e801f4"}
        )));
        assert_eq!(
            v.iter()
                .filter(|(k, _)| k == "brightness")
                .collect::<Vec<_>>(),
            vec![&("brightness".to_owned(), Value::Float(50.0))]
        );
        // A colour changed alone: the bulb is in colour.
        let v = bulb.decode_dps(&dps(serde_json::json!({"24": "00f003e800fa"})));
        assert!(v.contains(&("brightness".into(), Value::Float(25.0))));
        assert!(v.contains(&("color".into(), Value::from("#0000ff"))));
    }

    #[test]
    fn orders_reach_the_bulbs_own_codes() {
        let bulb = bulb();
        let white = |k: &str| match k {
            "work_mode" => Some(Value::from("white")),
            "brightness" => Some(Value::Float(60.0)),
            _ => None,
        };
        let o = bulb
            .order("brightness", &Value::Float(50.0), &white)
            .unwrap();
        assert_eq!(serde_json::Value::Object(o), serde_json::json!({"22": 500}));
        let o = bulb
            .order("color", &Value::from("#00ff00"), &white)
            .unwrap();
        assert_eq!(
            serde_json::Value::Object(o),
            serde_json::json!({"21": "colour", "24": "007803e80258"})
        );
        let o = bulb
            .order("color_temp", &Value::Float(154.0), &white)
            .unwrap();
        assert_eq!(
            serde_json::Value::Object(o),
            serde_json::json!({"21": "white", "23": 1000})
        );
        // Anything else as before.
        let o = bulb
            .order("switch_led", &Value::Bool(false), &white)
            .unwrap();
        assert_eq!(
            serde_json::Value::Object(o),
            serde_json::json!({"20": false})
        );
    }

    #[test]
    fn a_strips_outlets_are_numbered_in_words() {
        assert_eq!(label("switch_1"), "Marche");
        assert_eq!(label("switch_2"), "Marche 2");
        assert_eq!(label("switch_3"), "Marche 3");
        assert_eq!(label("countdown_1"), "Minuterie");
        assert_eq!(label("countdown_3"), "Minuterie 3");
        assert_eq!(label("relay_status"), "État au retour du courant");
        assert_eq!(label("child_lock"), "Sécurité enfant");
        // Not a number: as the code says.
        assert_eq!(label("switch_led"), "Switch led");
        assert_eq!(label("switch_inching"), "Switch inching");
    }

    fn plug() -> TuyaDevice {
        serde_json::from_value(serde_json::json!({
            "id": "bf123", "name": "Prise télé ", "category": "cz", "product": "Smart Plug",
            "dps": [
                { "id": 1, "code": "switch_1", "type": "Boolean", "writable": true },
                { "id": 18, "code": "cur_current", "type": "Integer", "unit": "mA", "scale": 0, "min": 0, "max": 30000 },
                { "id": 19, "code": "cur_power", "type": "Integer", "unit": "W", "scale": 1, "min": 0, "max": 80000 },
                { "id": 20, "code": "cur_voltage", "type": "Integer", "unit": "V", "scale": 1 },
                { "id": 38, "code": "relay_status", "type": "Enum", "writable": true, "range": ["power_off", "power_on", "last"] },
                { "id": 41, "code": "cycle_time", "type": "String", "writable": true },
                { "id": 99, "code": "blob", "type": "Raw" },
            ]
        }))
        .unwrap()
    }

    #[test]
    fn dps_become_points_with_units_and_scale() {
        let d = plug().device(&InstanceId::from("tuya"));
        assert_eq!(d.id.as_str(), "tuya:bf123");
        assert_eq!(&*d.native_name, "Prise télé");
        let keys: Vec<&str> = d.points.iter().map(|p| &*p.key).collect();
        assert_eq!(
            keys,
            [
                "switch_1",
                "cur_current",
                "cur_power",
                "cur_voltage",
                "relay_status",
                "cycle_time"
            ]
        );
        let power = d.point("cur_power").unwrap();
        assert_eq!(power.unit, Some(Unit::Watt));
        assert_eq!(power.semantic, Semantic::Power);
        assert!(
            matches!(power.kind, Kind::Numeric { max: Some(m), .. } if (m - 8000.0).abs() < 1e-9)
        );
        assert!(!power.access.write);
        assert!(d.point("switch_1").unwrap().access.write);
        assert!(
            !d.point("cycle_time").unwrap().access.write,
            "free text is never written"
        );
        assert_eq!(d.point("cur_current").unwrap().unit, Some(Unit::Ampere));
        assert_eq!(&*power.label, "Puissance");
        assert_eq!(&*d.point("cycle_time").unwrap().label, "Cycle time");
    }

    #[test]
    fn standard_sensor_codes_say_what_they_are() {
        let d: TuyaDevice = serde_json::from_value(serde_json::json!({
            "id": "bf9", "name": "Porte", "category": "mcs",
            "dps": [
                { "id": 1, "code": "doorcontact_state", "type": "Boolean" },
                { "id": 2, "code": "battery_percentage", "type": "Integer", "unit": "%" },
                { "id": 3, "code": "humidity_value", "type": "Integer", "unit": "%" },
            ]
        }))
        .unwrap();
        let d = d.device(&InstanceId::from("tuya"));
        let door = d.point("doorcontact_state").unwrap();
        assert_eq!(
            door.semantic,
            Semantic::Other,
            "true means open: not a contact"
        );
        assert_eq!(&*door.label, "Porte ouverte");
        assert_eq!(
            d.point("battery_percentage").unwrap().semantic,
            Semantic::Battery
        );
        assert_eq!(
            d.point("humidity_value").unwrap().semantic,
            Semantic::Humidity
        );
        assert_eq!(semantic("switch_1", None), Semantic::OnOff);
        assert_eq!(semantic("switch_led", None), Semantic::OnOff);
        assert_eq!(
            semantic("fan_switch", None),
            Semantic::Other,
            "the fan, not the device"
        );
        assert_eq!(semantic("switch_inching", None), Semantic::Other);
    }

    #[test]
    fn values_decode_and_encode() {
        let dev = plug();
        let raw = serde_json::json!({ "1": true, "18": 1234, "19": 1205, "38": "last", "77": 5 });
        let decoded = dev.decode_dps(raw.as_object().unwrap());
        assert_eq!(
            decoded,
            vec![
                ("switch_1".into(), Value::Bool(true)),
                ("cur_current".into(), Value::Float(1.234)),
                ("cur_power".into(), Value::Float(120.5)),
                ("relay_status".into(), Value::Text("last".into())),
            ]
        );
        let power = dev.dp_by_code("cur_power").unwrap();
        assert_eq!(
            power.encode(false, &Value::Float(120.5)),
            Some(Json::from(1205))
        );
        assert_eq!(
            dev.dp_by_code("switch_1")
                .unwrap()
                .encode(false, &Value::Bool(false)),
            Some(Json::Bool(false))
        );
        assert_eq!(
            dev.dp_by_code("switch_1")
                .unwrap()
                .encode(false, &Value::Int(1)),
            None
        );
    }
}
