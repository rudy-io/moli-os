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

    fn divisor(&self) -> f64 {
        10f64.powi(i32::try_from(self.scale.min(9)).unwrap_or(0))
    }

    /// The Moli point for this DP; `None` for opaque types.
    fn point(&self) -> Option<PointSpec> {
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
    pub fn decode(&self, raw: &Json) -> Option<Value> {
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
    pub fn encode(&self, value: &Value) -> Option<Json> {
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
            points: self.dps.iter().filter_map(Dp::point).collect(),
        }
    }

    #[must_use]
    pub fn dp_by_code(&self, code: &str) -> Option<&Dp> {
        self.dps.iter().find(|d| d.code == code)
    }

    /// `{"1": true, "18": 120}` → `[(code, value)]`, unknown DPs skipped.
    #[must_use]
    pub fn decode_dps(&self, dps: &serde_json::Map<String, Json>) -> Vec<(String, Value)> {
        let by_id: BTreeMap<u32, &Dp> = self.dps.iter().map(|d| (d.id, d)).collect();
        dps.iter()
            .filter_map(|(id, raw)| {
                let dp = by_id.get(&id.parse().ok()?)?;
                Some((dp.code.clone(), dp.decode(raw)?))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(power.encode(&Value::Float(120.5)), Some(Json::from(1205)));
        assert_eq!(
            dev.dp_by_code("switch_1")
                .unwrap()
                .encode(&Value::Bool(false)),
            Some(Json::Bool(false))
        );
        assert_eq!(
            dev.dp_by_code("switch_1").unwrap().encode(&Value::Int(1)),
            None
        );
    }
}
