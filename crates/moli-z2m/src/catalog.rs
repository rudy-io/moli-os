//! Pure translation between Zigbee2MQTT's model and Moli's. No IO.
//!
//! Z2M describes every device it supports through `exposes` in the retained
//! `bridge/devices` message. Flattening those gives us typed points for all
//! of the ~4 000 Zigbee devices Z2M knows, without one line of
//! device-specific code.

use std::collections::HashMap;
use std::sync::Arc;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde::Deserialize;
use serde_json::{Map, Value as Json};

/// Z2M `access` bitmask.
const ACCESS_STATE: u64 = 1;
const ACCESS_SET: u64 = 2;
const ACCESS_GET: u64 = 4;

/// How a point travels on the wire.
#[derive(Clone, Debug, PartialEq)]
struct Codec {
    /// Booleans travel as device-specific values (`"ON"`/`"OFF"`, `"LOCK"`…).
    binary: Option<(Json, Json)>,
    /// The device answers `{name}/get` for this point.
    gettable: bool,
    /// Enum whose values are numbers on the wire (`[0, 1, 2]`): exposed as
    /// text like every enum, converted back to numbers for the device.
    numeric_enum: bool,
}

/// One Z2M device with everything needed to decode its messages.
#[derive(Clone, Debug)]
pub struct Entry {
    pub ieee: String,
    pub friendly_name: String,
    pub device: Device,
    /// Mains-powered (router): always listening, can be read on demand.
    pub mains: bool,
    codecs: HashMap<Arc<str>, Codec>,
}

/// All devices of one Z2M bridge, indexed by IEEE address (identity) and by
/// friendly name (topic routing, may change at any time).
#[derive(Debug, Default)]
pub struct Catalog {
    entries: HashMap<String, Entry>,
    by_name: HashMap<String, String>,
}

impl Catalog {
    /// Rebuilds the catalog from a `bridge/devices` payload.
    pub fn from_bridge_devices(instance: &InstanceId, payload: &[u8]) -> serde_json::Result<Self> {
        let raw: Vec<RawDevice> = serde_json::from_slice(payload)?;
        let entries: HashMap<String, Entry> = raw
            .into_iter()
            .filter(|d| d.kind != "Coordinator" && !d.disabled)
            .map(|d| {
                let entry = Entry::from_raw(instance, d);
                (entry.ieee.clone(), entry)
            })
            .collect();
        let by_name = entries
            .values()
            .map(|e| (e.friendly_name.clone(), e.ieee.clone()))
            .collect();
        Ok(Self { entries, by_name })
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    #[must_use]
    pub fn by_name(&self, friendly_name: &str) -> Option<&Entry> {
        self.entries.get(self.by_name.get(friendly_name)?)
    }

    #[must_use]
    pub fn by_id(&self, id: &DeviceId) -> Option<&Entry> {
        self.entries.values().find(|e| &e.device.id == id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Entry {
    fn from_raw(instance: &InstanceId, raw: RawDevice) -> Self {
        let mut points = Vec::new();
        let mut codecs = HashMap::new();
        let definition = raw.definition.unwrap_or_default();
        for expose in &definition.exposes {
            flatten(expose, "", &mut points, &mut codecs);
        }
        let device = Device {
            id: DeviceId::new(instance, &raw.ieee_address),
            instance: instance.clone(),
            native_name: raw.friendly_name.as_str().into(),
            manufacturer: definition.vendor.or(raw.manufacturer).map(Into::into),
            model: definition.model.or(raw.model_id).map(Into::into),
            description: definition.description.map(Into::into),
            native_room: None,
            members: Vec::new(),
            points,
        };
        Self {
            ieee: raw.ieee_address,
            friendly_name: raw.friendly_name,
            device,
            mains: raw.kind == "Router",
            codecs,
        }
    }

    /// `{name}/get` payload asking a listening device for its current
    /// values (a Zigbee read: no effect on the device). `None` for sleepy
    /// devices or when nothing is readable.
    #[must_use]
    pub fn read_request(&self) -> Option<Json> {
        if !self.mains {
            return None;
        }
        let mut request = Map::new();
        for (key, codec) in &self.codecs {
            if codec.gettable {
                let top = key.split('.').next().unwrap_or(key);
                request.insert(top.to_owned(), Json::String(String::new()));
            }
        }
        (!request.is_empty()).then_some(Json::Object(request))
    }

    /// Decodes a state message into `(key, value)` pairs for known points.
    /// Unknown properties (`update`, `last_seen`…) are ignored.
    #[must_use]
    pub fn decode(&self, payload: &Map<String, Json>) -> Vec<(Arc<str>, Value)> {
        self.device
            .points
            .iter()
            .filter_map(|point| {
                let json = lookup(payload, &point.key)?;
                let codec = self.codecs.get(&point.key);
                let value = match codec.and_then(|c| c.binary.as_ref()) {
                    Some((on, _)) if json == on => Value::Bool(true),
                    Some((_, off)) if json == off => Value::Bool(false),
                    _ if codec.is_some_and(|c| c.numeric_enum) && json.is_number() => {
                        Value::Text(json.to_string().into())
                    }
                    _ => Value::from_json(json),
                };
                Some((point.key.clone(), value))
            })
            .collect()
    }

    /// Encodes a validated write as the `{friendly_name}/set` payload.
    #[must_use]
    pub fn encode(&self, key: &str, value: &Value) -> Json {
        let codec = self.codecs.get(key);
        let wire = match (codec.and_then(|c| c.binary.as_ref()), value) {
            (Some((on, _)), Value::Bool(true)) => on.clone(),
            (Some((_, off)), Value::Bool(false)) => off.clone(),
            (_, Value::Text(t)) if codec.is_some_and(|c| c.numeric_enum) => {
                serde_json::from_str::<Json>(t)
                    .ok()
                    .filter(Json::is_number)
                    .unwrap_or_else(|| Json::String(t.to_string()))
            }
            // Z2M wants integers where the spec has an integer step.
            (_, Value::Float(f)) if f.fract() == 0.0 && f.abs() < 9e15 =>
            {
                #[allow(clippy::cast_possible_truncation)]
                Json::from(*f as i64)
            }
            (_, v) => v.to_json(),
        };
        // `color.x` → {"color": {"x": …}}
        key.rsplit('.').fold(wire, |inner, part| {
            let mut map = Map::new();
            map.insert(part.to_owned(), inner);
            Json::Object(map)
        })
    }
}

fn lookup<'a>(payload: &'a Map<String, Json>, key: &str) -> Option<&'a Json> {
    let mut parts = key.split('.');
    let mut current = payload.get(parts.next()?)?;
    for part in parts {
        current = current.as_object()?.get(part)?;
    }
    Some(current)
}

fn flatten(
    expose: &RawExpose,
    prefix: &str,
    points: &mut Vec<PointSpec>,
    codecs: &mut HashMap<Arc<str>, Codec>,
) {
    if !expose.features.is_empty() {
        // Composite values nest under their property; specific types
        // (light, switch, climate…) are plain groups.
        let prefix = match (&*expose.kind, &expose.property) {
            ("composite", Some(p)) => format!("{prefix}{p}."),
            _ => prefix.to_owned(),
        };
        for feature in &expose.features {
            flatten(feature, &prefix, points, codecs);
        }
        return;
    }
    let Some(property) = &expose.property else {
        return;
    };
    let key: Arc<str> = format!("{prefix}{property}").into();
    if points.iter().any(|p| p.key == key) {
        return;
    }
    let (kind, binary) = match &*expose.kind {
        "binary" => (
            Kind::Binary,
            Some((
                expose.value_on.clone().unwrap_or(Json::Bool(true)),
                expose.value_off.clone().unwrap_or(Json::Bool(false)),
            )),
        ),
        "numeric" => (
            Kind::Numeric {
                min: expose.value_min,
                max: expose.value_max,
                step: expose.value_step,
            },
            None,
        ),
        "enum" => (
            Kind::Enum {
                values: expose
                    .values
                    .iter()
                    .map(|v| v.as_str().map_or_else(|| v.to_string().into(), Into::into))
                    .collect(),
            },
            None,
        ),
        "text" => (Kind::Text, None),
        _ => return,
    };
    let codec = Codec {
        binary,
        gettable: expose.access & ACCESS_GET != 0,
        numeric_enum: !expose.values.is_empty() && expose.values.iter().all(Json::is_number),
    };
    let unit = expose.unit.as_deref().and_then(Unit::parse);
    // A calibration offset in °C is configuration, not a temperature reading.
    let semantic = if expose.category.as_deref() == Some("config") {
        Semantic::Config
    } else {
        Semantic::infer(&key, unit.as_ref())
    };
    // Z2M converters expect composites (color {x, y}…) as a whole: writing
    // one member alone is rejected. Members are read-only until a composite
    // command exists.
    let in_composite = !prefix.is_empty();
    points.push(PointSpec {
        label: expose.label.as_deref().unwrap_or(property).into(),
        kind,
        access: Access {
            read: expose.access & ACCESS_STATE != 0,
            write: expose.access & ACCESS_SET != 0 && !in_composite,
        },
        unit,
        semantic,
        key: key.clone(),
    });
    codecs.insert(key, codec);
}

#[derive(Debug, Deserialize)]
struct RawDevice {
    ieee_address: String,
    friendly_name: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    manufacturer: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    definition: Option<RawDefinition>,
}

#[derive(Debug, Default, Deserialize)]
struct RawDefinition {
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    exposes: Vec<RawExpose>,
}

#[derive(Debug, Deserialize)]
struct RawExpose {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    property: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    access: u64,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    value_min: Option<f64>,
    #[serde(default)]
    value_max: Option<f64>,
    #[serde(default)]
    value_step: Option<f64>,
    #[serde(default)]
    value_on: Option<Json>,
    #[serde(default)]
    value_off: Option<Json>,
    #[serde(default)]
    values: Vec<Json>,
    #[serde(default)]
    features: Vec<RawExpose>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Catalog {
        Catalog::from_bridge_devices(
            &InstanceId::from("z2m"),
            include_bytes!("../../../integrations/z2m/fixtures/bridge_devices.json"),
        )
        .unwrap()
    }

    fn state(ieee: &str) -> Map<String, Json> {
        let all: Map<String, Json> = serde_json::from_slice(include_bytes!(
            "../../../integrations/z2m/fixtures/state.json"
        ))
        .unwrap();
        all[ieee].as_object().unwrap().clone()
    }

    #[test]
    fn real_bridge_is_fully_mapped() {
        let catalog = catalog();
        assert_eq!(catalog.len(), 7, "coordinator excluded, 7 real devices");
        for entry in catalog.entries() {
            assert!(
                !entry.device.points.is_empty(),
                "{} has no points",
                entry.ieee
            );
            assert!(entry.device.id.as_str().starts_with("z2m:0x"));
        }
    }

    #[test]
    fn plug_points_are_typed() {
        let catalog = catalog();
        let plug = catalog.by_name("0x00124b0000000003").unwrap();
        let state = plug.device.point("state").unwrap();
        assert_eq!(state.kind, Kind::Binary);
        assert!(state.access.write);
        assert_eq!(state.semantic, Semantic::OnOff);

        let power = plug.device.point("power").unwrap();
        assert_eq!(power.unit, Some(Unit::Watt));
        assert_eq!(power.semantic, Semantic::Power);
        assert!(!power.access.write);

        let child_lock = plug.device.point("child_lock").unwrap();
        assert_eq!(child_lock.kind, Kind::Binary);
        assert!(child_lock.access.write);
    }

    #[test]
    fn real_state_messages_decode() {
        let catalog = catalog();
        let plug = catalog.by_name("0x00124b0000000009").unwrap();
        let values: HashMap<_, _> = plug
            .decode(&state("0x00124b0000000009"))
            .into_iter()
            .collect();
        assert_eq!(values["state"], Value::Bool(true));
        assert_eq!(
            values["child_lock"],
            Value::Bool(false),
            "UNLOCK is the off value"
        );
        assert_eq!(values["power"], Value::Int(1538));
        assert_eq!(values["indicator_mode"], Value::from("on"));

        let linky = catalog.by_name("0x00124b0000000001").unwrap();
        let values: HashMap<_, _> = linky
            .decode(&state("0x00124b0000000001"))
            .into_iter()
            .collect();
        assert_eq!(values["apparent_power"], Value::Int(1070));
        assert!(
            !values.contains_key("update"),
            "non-exposed objects are ignored"
        );

        let leak = catalog.by_name("0x00124b0000000007").unwrap();
        let values: HashMap<_, _> = leak
            .decode(&state("0x00124b0000000007"))
            .into_iter()
            .collect();
        assert_eq!(values["water_leak"], Value::Bool(false));
    }

    #[test]
    fn commands_encode_to_native_values() {
        let catalog = catalog();
        let plug = catalog.by_name("0x00124b0000000003").unwrap();
        assert_eq!(
            plug.encode("state", &Value::Bool(true)),
            json!({"state": "ON"})
        );
        assert_eq!(
            plug.encode("child_lock", &Value::Bool(true)),
            json!({"child_lock": "LOCK"})
        );
        assert_eq!(
            plug.encode("indicator_mode", &Value::from("off")),
            json!({"indicator_mode": "off"})
        );

        let pir = catalog.by_name("0x00124b0000000008").unwrap();
        assert_eq!(
            pir.encode("illuminance_interval", &Value::Float(60.0)),
            json!({"illuminance_interval": 60})
        );
        assert_eq!(
            pir.device.point("illuminance_interval").unwrap().unit,
            Some(Unit::Minute)
        );
    }

    #[test]
    fn config_category_wins_over_units_and_numeric_enums_round_trip() {
        let expose: RawExpose = serde_json::from_value(json!({
            "type": "climate",
            "features": [
                {"type": "numeric", "property": "local_temperature_calibration", "access": 7,
                 "unit": "°C", "category": "config"},
                {"type": "numeric", "property": "local_temperature", "access": 5, "unit": "°C"},
                {"type": "enum", "property": "mode", "access": 7, "values": [0, 1, 2]}
            ]
        }))
        .unwrap();
        let mut points = Vec::new();
        let mut codecs = HashMap::new();
        flatten(&expose, "", &mut points, &mut codecs);
        assert_eq!(points[0].semantic, Semantic::Config);
        assert_eq!(points[1].semantic, Semantic::Temperature);

        let entry = Entry {
            ieee: "0x2".into(),
            friendly_name: "trv".into(),
            mains: false,
            device: Device {
                id: DeviceId::from("z2m:0x2"),
                instance: InstanceId::from("z2m"),
                native_name: "trv".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points,
            },
            codecs,
        };
        let decoded: HashMap<_, _> = entry
            .decode(json!({"mode": 1}).as_object().unwrap())
            .into_iter()
            .collect();
        assert_eq!(
            decoded["mode"],
            Value::from("1"),
            "matches the enum's text values"
        );
        assert_eq!(entry.encode("mode", &Value::from("2")), json!({"mode": 2}));
    }

    #[test]
    fn only_listening_devices_are_read_on_demand() {
        let catalog = catalog();
        let plug = catalog.by_name("0x00124b0000000003").unwrap();
        let request = plug.read_request().unwrap();
        assert_eq!(request["state"], "");
        assert!(
            request.get("power").is_none(),
            "power is not gettable on this plug"
        );

        let smoke = catalog.by_name("0x00124b0000000004").unwrap();
        assert!(
            smoke.read_request().is_none(),
            "sleepy devices are never polled"
        );
    }

    #[test]
    fn composite_keys_round_trip() {
        let expose: RawExpose = serde_json::from_value(json!({
            "type": "light",
            "features": [
                {"type": "binary", "property": "state", "access": 7, "value_on": "ON", "value_off": "OFF"},
                {"type": "composite", "property": "color", "features": [
                    {"type": "numeric", "property": "x", "access": 7},
                    {"type": "numeric", "property": "y", "access": 7}
                ]}
            ]
        }))
        .unwrap();
        let mut points = Vec::new();
        let mut codecs = HashMap::new();
        flatten(&expose, "", &mut points, &mut codecs);
        let keys: Vec<_> = points.iter().map(|p| &*p.key).collect();
        assert_eq!(keys, ["state", "color.x", "color.y"]);
        assert!(points[0].access.write);
        assert!(
            !points[1].access.write,
            "composite members are read-only: Z2M rejects partial writes"
        );

        let entry = Entry {
            ieee: "0x1".into(),
            friendly_name: "lamp".into(),
            mains: true,
            device: Device {
                id: DeviceId::from("z2m:0x1"),
                instance: InstanceId::from("z2m"),
                native_name: "lamp".into(),
                manufacturer: None,
                model: None,
                description: None,
                native_room: None,
                members: Vec::new(),
                points,
            },
            codecs,
        };
        assert_eq!(
            entry.encode("color.x", &Value::Float(0.3)),
            json!({"color": {"x": 0.3}})
        );
        let payload = json!({"state": "OFF", "color": {"x": 0.31, "y": 0.32}});
        let decoded: HashMap<_, _> = entry
            .decode(payload.as_object().unwrap())
            .into_iter()
            .collect();
        assert_eq!(decoded["state"], Value::Bool(false));
        assert_eq!(decoded["color.y"], Value::Float(0.32));
    }
}
