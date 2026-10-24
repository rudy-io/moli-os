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
    /// Its address inside its gateway (the node id). With `gateway` naming
    /// that gateway's device id, the gateway's local link reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
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

    /// The Moli point for this DP, under `key`; `None` for opaque types.
    fn point(&self, key: &str) -> Option<PointSpec> {
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
            key: key.into(),
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

/// Tuya codes in Moli's own words, per category, where the dashboard and
/// Moli look for those words: thermostats and heaters (`wk` thermostat,
/// `wkf` radiator valve, `qn` heater) are heating devices, not lights; a
/// temperature and humidity sensor (`wsdcg`) gives the room its readings.
fn moli_words(category: &str) -> &'static [(&'static str, &'static str)] {
    match category {
        "wk" | "wkf" | "qn" => &[
            ("switch", "on"),
            ("temp_set", "target_temperature"),
            ("temp_current", "temperature"),
        ],
        "wsdcg" => &[
            ("va_temperature", "temperature"),
            ("temp_current", "temperature"),
            ("va_humidity", "humidity"),
            ("humidity_value", "humidity"),
        ],
        _ => &[],
    }
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
        "temp_set" => "pilotes.tuya.consigne",
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
                .filter_map(|dp| dp.point(self.key(dp)))
                .chain(crate::program::kept_by(&self.dps).then(|| PointSpec {
                    key: crate::program::POINT.into(),
                    label: moli_i18n::tr("pilotes.tuya.programme").into(),
                    kind: Kind::Text,
                    // Written through the cloud (`cloud::write_programs`).
                    access: Access {
                        read: true,
                        write: true,
                    },
                    unit: None,
                    semantic: Semantic::Other,
                }))
                .collect(),
        }
    }

    /// The Moli point key of a DP: its code, or Moli's word for it in its
    /// category (unless another DP already has that code, or an earlier DP
    /// already took that word).
    #[must_use]
    pub fn key<'a>(&self, dp: &'a Dp) -> &'a str {
        let word = |dp: &Dp| {
            moli_words(&self.category)
                .iter()
                .find(|(code, word)| dp.code == *code && self.dp_by_code(word).is_none())
                .map(|(_, word)| *word)
        };
        match word(dp) {
            Some(w)
                if !self
                    .dps
                    .iter()
                    .take_while(|d| !std::ptr::eq(*d, dp))
                    .any(|d| word(d) == Some(w)) =>
            {
                w
            }
            _ => &dp.code,
        }
    }

    #[must_use]
    pub fn dp_by_code(&self, code: &str) -> Option<&Dp> {
        self.dps.iter().find(|d| d.code == code)
    }

    /// The DP behind a Moli point key.
    #[must_use]
    pub fn dp_by_key(&self, key: &str) -> Option<&Dp> {
        self.dps.iter().find(|d| self.key(d) == key)
    }

    /// `{"1": true, "18": 120}` → `[(key, value)]`, unknown DPs skipped.
    #[must_use]
    pub fn decode_dps(&self, dps: &serde_json::Map<String, Json>) -> Vec<(String, Value)> {
        let by_id: BTreeMap<u32, &Dp> = self.dps.iter().map(|d| (d.id, d)).collect();
        dps.iter()
            .filter_map(|(id, raw)| {
                let dp = by_id.get(&id.parse().ok()?)?;
                Some((self.key(dp).to_owned(), dp.decode(raw)?))
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

    fn thermostat(category: &str) -> TuyaDevice {
        serde_json::from_value(serde_json::json!({
            "id": "bf42", "name": "Radiateur", "category": category, "product": "FP Thermostat",
            "gateway": "tuya",
            "dps": [
                { "id": 1, "code": "switch", "type": "Boolean", "writable": true },
                { "id": 2, "code": "mode", "type": "Enum", "writable": true, "range": ["comfort", "eco"] },
                { "id": 16, "code": "temp_set", "type": "Integer", "writable": true, "unit": "℃", "scale": 1, "min": 50, "max": 400, "step": 5 },
                { "id": 24, "code": "temp_current", "type": "Integer", "unit": "℃", "scale": 1 },
                { "id": 40, "code": "child_lock", "type": "Boolean", "writable": true },
            ]
        }))
        .unwrap()
    }

    #[test]
    fn a_thermostat_speaks_moli_climate_words() {
        let dev = thermostat("wk");
        let d = dev.device(&InstanceId::from("tuya"));
        let keys: Vec<&str> = d.points.iter().map(|p| &*p.key).collect();
        assert_eq!(
            keys,
            [
                "on",
                "mode",
                "target_temperature",
                "temperature",
                "child_lock"
            ]
        );
        let target = d.point("target_temperature").unwrap();
        assert!(target.access.write);
        assert_eq!(target.unit, Some(Unit::Celsius));
        assert_eq!(&*target.label, "Consigne");
        assert_eq!(d.point("on").unwrap().semantic, Semantic::OnOff);

        // States, local or cloud, arrive under the same keys…
        let raw = serde_json::json!({ "1": true, "16": 175, "24": 190 });
        assert_eq!(
            dev.decode_dps(raw.as_object().unwrap()),
            vec![
                ("on".into(), Value::Bool(true)),
                ("target_temperature".into(), Value::Float(17.5)),
                ("temperature".into(), Value::Float(19.0)),
            ]
        );
        // …and an order on a climate word reaches the right DP.
        let set = dev.dp_by_key("target_temperature").unwrap();
        assert_eq!(set.id, 16);
        assert_eq!(set.encode(&Value::Float(17.0)), Some(Json::from(170)));
        assert!(dev.dp_by_key("temp_set").is_none());
        assert_eq!(dev.dp_by_key("child_lock").unwrap().id, 40);
    }

    #[test]
    fn a_temperature_sensor_gives_the_room_its_readings() {
        let mut dev: TuyaDevice = serde_json::from_value(serde_json::json!({
            "id": "bf77", "name": "Capteur", "category": "wsdcg", "gateway": "tuya",
            "dps": [
                { "id": 101, "code": "va_humidity", "type": "Integer", "unit": "%", "min": 0, "max": 100 },
                { "id": 102, "code": "va_battery", "type": "Integer", "min": 0, "max": 500 },
                { "id": 103, "code": "va_temperature", "type": "Integer", "unit": "℃", "min": -20, "max": 60 },
            ]
        }))
        .unwrap();
        let d = dev.device(&InstanceId::from("tuya"));
        let keys: Vec<&str> = d.points.iter().map(|p| &*p.key).collect();
        assert_eq!(keys, ["humidity", "va_battery", "temperature"]);
        assert_eq!(
            d.point("temperature").unwrap().semantic,
            Semantic::Temperature
        );
        assert_eq!(d.point("humidity").unwrap().semantic, Semantic::Humidity);

        // The cloud reports by code: the readings land under the same words.
        let raw = serde_json::json!({ "101": 47, "103": 19 });
        assert_eq!(
            dev.decode_dps(raw.as_object().unwrap()),
            vec![
                ("humidity".into(), Value::Int(47)),
                ("temperature".into(), Value::Int(19)),
            ]
        );

        // Two temperatures on one device: the first keeps the word, never both.
        dev.dps.push(Dp {
            id: 104,
            code: "temp_current".into(),
            kind: DpKind::Integer,
            writable: false,
            unit: None,
            scale: 0,
            min: None,
            max: None,
            step: None,
            range: Vec::new(),
        });
        let d = dev.device(&InstanceId::from("tuya"));
        assert!(d.point("temp_current").is_some());
        assert_eq!(dev.dp_by_key("temperature").unwrap().id, 103);
    }

    #[test]
    fn a_heater_that_keeps_a_program_shows_it() {
        let mut dev = thermostat("wk");
        assert!(
            dev.device(&InstanceId::from("tuya"))
                .point("week_program")
                .is_none()
        );
        dev.dps.push(Dp {
            id: 125,
            code: "week_program_18_1_1".into(),
            kind: DpKind::Raw,
            writable: true,
            unit: None,
            scale: 0,
            min: None,
            max: None,
            step: None,
            range: Vec::new(),
        });
        let d = dev.device(&InstanceId::from("tuya"));
        let program = d.point("week_program").unwrap();
        assert_eq!(program.kind, Kind::Text);
        assert!(program.access.write, "set through the cloud");
        assert!(
            d.point("week_program_18_1_1").is_none(),
            "its raw parts are not points"
        );
    }

    #[test]
    fn only_heating_categories_are_renamed() {
        let d = thermostat("cz").device(&InstanceId::from("tuya"));
        assert!(d.point("temp_set").is_some(), "a plug keeps Tuya's codes");
        assert!(d.point("target_temperature").is_none());
        assert!(
            plug()
                .device(&InstanceId::from("tuya"))
                .point("on")
                .is_none()
        );

        // A DP already coded with the climate word keeps it: no two points share a key.
        let mut dev = thermostat("wk");
        dev.dps.push(Dp {
            id: 50,
            code: "temperature".into(),
            kind: DpKind::Integer,
            writable: false,
            unit: None,
            scale: 0,
            min: None,
            max: None,
            step: None,
            range: Vec::new(),
        });
        let d = dev.device(&InstanceId::from("tuya"));
        assert!(d.point("temp_current").is_some());
        assert_eq!(dev.dp_by_key("temperature").unwrap().id, 50);
    }
}
