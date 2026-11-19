//! Pure translation between the rfxcom2mqtt gateway model and Moli's. No IO.
//!
//! A 433 MHz receiver hears every sensor around, the neighbours' included:
//! rfxcom2mqtt keeps a state topic for each of them (`{base}/devices/{id}`,
//! `{base}/devices/{id}/{unit}` for a switch), hundreds in a dense street.
//! The devices the bridge actually knows are the ones it announces through
//! its Home Assistant discovery: one retained config per entity
//! (`{discovery}/sensor/{id}/temperature/config`,
//! `{discovery}/switch/{id}/{unit}/config`…). The catalog is built from those
//! configs only, so a stray state topic never creates a device.
//!
//! Identity: the state topic below `{base}/devices/` (`0x5A01`, or
//! `0x00ABCDE1/12` for one unit of a switch, `/` becoming `_`). Names come
//! from the config's `device.name` and are suggestions only.

use std::collections::BTreeMap;
use std::sync::Arc;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde::Deserialize;
use serde_json::Value as Json;

/// What a state field becomes, read from a discovery config.
#[derive(Clone, Debug, PartialEq)]
pub struct Prop {
    /// The field of the state payload (`temperature`, `batteryLevel`).
    pub field: String,
    /// The point key (`temperature`, `battery`).
    pub key: String,
    pub conv: Conv,
}

/// How a field's raw value becomes a point value.
#[derive(Clone, Debug, PartialEq)]
pub enum Conv {
    /// The number as it is.
    Number,
    /// The RFXtrx battery level, 0 (empty) to 9 (full), as a percent.
    BatteryLevel,
    /// A text compared to the config's on/off payloads.
    Binary { on: String, off: String },
    /// Anything else, as the JSON gives it: a scalar only (a number, a
    /// text, a boolean), an object or an array is dropped.
    Raw,
}

/// The write target of a switch, as its discovery config gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// The full command topic (`{base}/cmd/{type}/{subtype}/{id}/{unit}`).
    pub topic: String,
    pub payload_on: String,
    pub payload_off: String,
}

/// One rfxcom2mqtt device with everything needed to decode its state topic
/// and encode its commands.
#[derive(Clone, Debug)]
pub struct Entry {
    pub device: Device,
    pub props: Vec<Prop>,
    pub command: Option<Command>,
    /// The config topics this entry was built from: an emptied config
    /// removes its point, the last one removes the device.
    configs: BTreeMap<String, String>,
    /// Published to the hub: once it has a point besides the signal.
    published: bool,
}

/// What a discovery config changed.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// A device appeared or gained a point: publish it again.
    Upsert(Device),
    /// The last config of a device was emptied.
    Remove(DeviceId),
    /// Nothing for this driver (another bridge, a component not covered,
    /// a config already known).
    None,
}

/// The devices of one rfxcom2mqtt bridge, keyed by state topic suffix.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    entries: BTreeMap<String, Entry>,
    /// Config topic → state topic suffix, to forget an emptied config.
    config_owner: BTreeMap<String, String>,
}

/// The fields of a Home Assistant discovery config this driver reads.
#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default)]
    state_topic: Option<String>,
    #[serde(default)]
    value_template: Option<String>,
    #[serde(default)]
    command_topic: Option<String>,
    #[serde(default)]
    payload_on: Option<Json>,
    #[serde(default)]
    payload_off: Option<Json>,
    #[serde(default)]
    unit_of_measurement: Option<String>,
    #[serde(default)]
    device: Option<ConfigDevice>,
}

#[derive(Debug, Deserialize)]
struct ConfigDevice {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    manufacturer: Option<String>,
}

/// The highest RFXtrx battery level (full).
const BATTERY_LEVEL_MAX: f64 = 9.0;

/// The highest RFXtrx signal level.
const RSSI_MAX: f64 = 15.0;

/// The most devices one bridge may announce: a house has tens, a busy
/// street a few hundred; anything beyond is ignored.
pub const MAX_DEVICES: usize = 2_000;

/// The most points one device may carry.
pub const MAX_POINTS: usize = 64;

impl Catalog {
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries().count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The published devices.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values().filter(|e| e.published)
    }

    #[must_use]
    pub fn by_id(&self, id: &DeviceId) -> Option<&Entry> {
        self.entries().find(|e| &e.device.id == id)
    }

    /// The entry of a state topic suffix (`0x5A01`, `0x00ABCDE1/12`).
    #[must_use]
    pub fn by_state(&self, rest: &str) -> Option<&Entry> {
        self.entries.get(rest).filter(|e| e.published)
    }

    /// Applies one retained discovery config. `component` is the discovery
    /// component (`sensor`, `switch`, `binary_sensor`); `base` is the
    /// bridge's base topic. A config whose state topic is not one of this
    /// bridge's devices is ignored, so the same broker's Zigbee2MQTT configs
    /// never reach the catalog.
    pub fn apply_config(
        &mut self,
        instance: &InstanceId,
        base: &str,
        topic: &str,
        component: &str,
        payload: &[u8],
    ) -> Change {
        if payload.iter().all(u8::is_ascii_whitespace) {
            return self.forget_config(topic);
        }
        let Ok(config) = serde_json::from_slice::<Config>(payload) else {
            return Change::None;
        };
        let Some(state_topic) = config.state_topic.as_deref() else {
            return Change::None;
        };
        let Some(rest) = state_rest(state_topic, base) else {
            return Change::None;
        };
        let Some(field) = config.value_template.as_deref().and_then(template_field) else {
            return Change::None;
        };
        let Some((prop, spec, command)) = point_for(component, &field, &config, base) else {
            return Change::None;
        };
        // `0x01/12` and `0x01_12` would share one `DeviceId`: the first
        // one announced keeps it.
        if !self.entries.contains_key(rest) {
            let id = DeviceId::new(instance, rest);
            if self.entries.values().any(|e| e.device.id == id) || self.entries.len() >= MAX_DEVICES
            {
                return Change::None;
            }
        }
        let rest = rest.to_owned();
        let entry = self
            .entries
            .entry(rest.clone())
            .or_insert_with(|| new_entry(instance, &rest, &config));
        if entry.device.points.len() >= MAX_POINTS && entry.device.point(&spec.key).is_none() {
            return Change::None;
        }
        let before = (entry.device.points.clone(), entry.command.clone());
        entry.configs.insert(topic.to_owned(), prop.key.clone());
        // In place when the key is known: a repeated config changes nothing.
        match entry.props.iter_mut().find(|p| p.key == prop.key) {
            Some(known) => *known = prop,
            None => entry.props.push(prop),
        }
        match entry.device.points.iter_mut().find(|p| p.key == spec.key) {
            Some(known) => *known = spec,
            None => entry.device.points.push(spec),
        }
        if command.is_some() {
            entry.command = command;
        }
        self.config_owner.insert(topic.to_owned(), rest);
        let changed = (entry.device.points.clone(), entry.command.clone()) != before;
        entry.publication(changed)
    }

    /// An emptied config: its point goes, and the device with its last one.
    fn forget_config(&mut self, topic: &str) -> Change {
        let Some(rest) = self.config_owner.remove(topic) else {
            return Change::None;
        };
        let Some(entry) = self.entries.get_mut(&rest) else {
            return Change::None;
        };
        let Some(key) = entry.configs.remove(topic) else {
            return Change::None;
        };
        if !entry.configs.values().any(|k| *k == key) {
            entry.props.retain(|p| p.key != key);
            entry.device.points.retain(|p| *p.key != *key);
            if key == "on" {
                entry.command = None;
            }
        }
        if entry.configs.is_empty() {
            let change = if entry.published {
                Change::Remove(entry.device.id.clone())
            } else {
                Change::None
            };
            self.entries.remove(&rest);
            return change;
        }
        entry.publication(true)
    }
}

impl Entry {
    /// What the hub must learn after a change. rfxcom2mqtt announces a
    /// signal sensor on a switch's bare id (`devices/{id}`), where no state
    /// ever arrives (they go to `devices/{id}/{unit}`): a device with
    /// nothing but its signal stays unpublished.
    fn publication(&mut self, changed: bool) -> Change {
        let visible = self.device.points.iter().any(|p| &*p.key != "rssi");
        match (visible, self.published) {
            (true, false) => {
                self.published = true;
                Change::Upsert(self.device.clone())
            }
            (true, true) if changed => Change::Upsert(self.device.clone()),
            (false, true) => {
                self.published = false;
                Change::Remove(self.device.id.clone())
            }
            _ => Change::None,
        }
    }

    /// Decodes a state payload into point values. Only the fields the
    /// discovery configs declared are read; a missing field is skipped (the
    /// echo of a sent command carries `command` alone).
    #[must_use]
    pub fn decode(&self, payload: &[u8]) -> Vec<(String, Value)> {
        let Ok(Json::Object(state)) = serde_json::from_slice::<Json>(payload) else {
            return Vec::new();
        };
        self.props
            .iter()
            .filter_map(|prop| {
                let raw = state.get(&prop.field)?;
                convert(&prop.conv, raw).map(|v| (prop.key.clone(), v))
            })
            .collect()
    }

    /// Encodes a validated write as `(full topic, payload)`. Only a switch's
    /// `on` is writable.
    pub fn encode(&self, key: &str, value: &Value) -> anyhow::Result<(String, String)> {
        let (Some(command), "on") = (&self.command, key) else {
            return Err(anyhow::anyhow!(moli_i18n::tr!(
                "pilotes.rfxcom2mqtt.point_lu_seul",
                point = key
            )));
        };
        let Value::Bool(on) = value else {
            return Err(anyhow::anyhow!(moli_i18n::tr!(
                "pilotes.rfxcom2mqtt.on_attend_booleen"
            )));
        };
        let payload = if *on {
            &command.payload_on
        } else {
            &command.payload_off
        };
        Ok((command.topic.clone(), payload.clone()))
    }
}

/// The suffix of a state topic below `{base}/devices/`, if it is one of
/// this bridge's devices.
fn state_rest<'a>(state_topic: &'a str, base: &str) -> Option<&'a str> {
    let rest = state_topic.strip_prefix(base)?.strip_prefix("/devices/")?;
    (!rest.is_empty() && rest.split('/').all(|s| !s.is_empty())).then_some(rest)
}

/// The state field a template reads: `{{ value_json.temperature }}`.
fn template_field(template: &str) -> Option<String> {
    let after = template.split("value_json.").nth(1)?;
    let field: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!field.is_empty()).then_some(field)
}

/// A JSON payload as the text rfxcom2mqtt compares (`"On"`, or `1`).
fn payload_text(json: Option<&Json>) -> Option<String> {
    match json? {
        Json::String(s) => Some(s.clone()),
        Json::Null => None,
        other => Some(other.to_string()),
    }
}

/// The point one config gives.
fn point_for(
    component: &str,
    field: &str,
    config: &Config,
    base: &str,
) -> Option<(Prop, PointSpec, Option<Command>)> {
    match component {
        "switch" => switch_point(field, config, base),
        "binary_sensor" => Some(binary_point(field, config)),
        "sensor" => Some(sensor_point(field, config)),
        _ => None,
    }
}

const READ_ONLY: Access = Access {
    read: true,
    write: false,
};

/// A switch's `on`, written through the command topic its config names.
fn switch_point(
    field: &str,
    config: &Config,
    base: &str,
) -> Option<(Prop, PointSpec, Option<Command>)> {
    let topic = config.command_topic.clone()?;
    // A command goes to this bridge's own command topics, nowhere else.
    if !topic.starts_with(&format!("{base}/cmd/")) {
        return None;
    }
    let on = payload_text(config.payload_on.as_ref()).unwrap_or_else(|| "On".into());
    let off = payload_text(config.payload_off.as_ref()).unwrap_or_else(|| "Off".into());
    let prop = Prop {
        field: field.to_owned(),
        key: "on".into(),
        conv: Conv::Binary {
            on: on.clone(),
            off: off.clone(),
        },
    };
    let spec = PointSpec {
        key: "on".into(),
        label: "On".into(),
        kind: Kind::Binary,
        access: Access {
            read: true,
            write: true,
        },
        unit: None,
        semantic: Semantic::OnOff,
    };
    let command = Command {
        topic,
        payload_on: on,
        payload_off: off,
    };
    Some((prop, spec, Some(command)))
}

/// A binary sensor, read as it is.
fn binary_point(field: &str, config: &Config) -> (Prop, PointSpec, Option<Command>) {
    let on = payload_text(config.payload_on.as_ref()).unwrap_or_else(|| "On".into());
    let off = payload_text(config.payload_off.as_ref()).unwrap_or_else(|| "Off".into());
    let key = snake(field);
    let prop = Prop {
        field: field.to_owned(),
        key: key.clone(),
        conv: Conv::Binary { on, off },
    };
    let spec = PointSpec {
        key: key.into(),
        label: field.into(),
        kind: Kind::Binary,
        access: READ_ONLY,
        unit: None,
        // A door sensor's « on » is open, Moli's contact is closed:
        // nothing is deduced, the meaning stays neutral.
        semantic: Semantic::Other,
    };
    (prop, spec, None)
}

/// A sensor field, typed when it is a known one.
fn sensor_point(field: &str, config: &Config) -> (Prop, PointSpec, Option<Command>) {
    let (key, label, unit, semantic, conv, kind) = match field {
        "temperature" => (
            "temperature".to_owned(),
            "Temperature",
            Some(Unit::Celsius),
            Semantic::Temperature,
            Conv::Number,
            numeric(None, None),
        ),
        "humidity" => (
            "humidity".to_owned(),
            "Humidity",
            Some(Unit::Percent),
            Semantic::Humidity,
            Conv::Number,
            numeric(Some(0.0), Some(100.0)),
        ),
        "barometer" => (
            "pressure".to_owned(),
            "Pressure",
            Some(Unit::HectoPascal),
            Semantic::Pressure,
            Conv::Number,
            numeric(None, None),
        ),
        "batteryLevel" => (
            "battery".to_owned(),
            "Battery level",
            Some(Unit::Percent),
            Semantic::Battery,
            Conv::BatteryLevel,
            numeric(Some(0.0), Some(100.0)),
        ),
        "rssi" => (
            "rssi".to_owned(),
            "Signal",
            None,
            Semantic::SignalStrength,
            Conv::Number,
            numeric(Some(0.0), Some(RSSI_MAX)),
        ),
        other => {
            let key = snake(other);
            let unit = config.unit_of_measurement.as_deref().and_then(Unit::parse);
            let semantic = Semantic::infer(&key, unit.as_ref());
            (key, other, unit, semantic, Conv::Raw, Kind::Text)
        }
    };
    let kind = if matches!(conv, Conv::Raw) && config.unit_of_measurement.is_some() {
        numeric(None, None)
    } else {
        kind
    };
    let conv = if matches!(conv, Conv::Raw) && matches!(kind, Kind::Numeric { .. }) {
        Conv::Number
    } else {
        conv
    };
    let prop = Prop {
        field: field.to_owned(),
        key: key.clone(),
        conv,
    };
    let spec = PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: READ_ONLY,
        unit,
        semantic,
    };
    (prop, spec, None)
}

fn numeric(min: Option<f64>, max: Option<f64>) -> Kind {
    Kind::Numeric {
        min,
        max,
        step: None,
    }
}

/// A new, empty entry for a state topic suffix.
fn new_entry(instance: &InstanceId, rest: &str, config: &Config) -> Entry {
    let device_name = config
        .device
        .as_ref()
        .and_then(|d| d.name.clone())
        .filter(|n| !n.trim().is_empty());
    // One unit of a multi-unit switch: the unit tells them apart.
    let unit = rest.split_once('/').map(|(_, unit)| unit);
    let native_name = match (device_name, unit) {
        (Some(name), Some(unit)) => format!("{name} {unit}"),
        (Some(name), None) => name,
        (None, _) => rest.replace('/', " "),
    };
    Entry {
        device: Device {
            id: DeviceId::new(instance, rest),
            instance: instance.clone(),
            native_name: native_name.into(),
            manufacturer: config
                .device
                .as_ref()
                .and_then(|d| d.manufacturer.clone())
                .filter(|m| !m.is_empty())
                .map(Arc::from),
            model: None,
            description: None,
            native_room: None,
            members: Vec::new(),
            points: Vec::new(),
        },
        props: Vec::new(),
        command: None,
        configs: BTreeMap::new(),
        published: false,
    }
}

/// `batteryLevel` → `battery_level`.
fn snake(field: &str) -> String {
    let mut out = String::with_capacity(field.len() + 4);
    for c in field.chars() {
        if c.is_ascii_uppercase() {
            if !out.is_empty() {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// One raw state value through its conversion.
fn convert(conv: &Conv, raw: &Json) -> Option<Value> {
    match conv {
        Conv::Number => raw.as_f64().map(number),
        Conv::BatteryLevel => {
            let level = raw.as_f64()?.clamp(0.0, BATTERY_LEVEL_MAX);
            Some(number((level * 100.0 / BATTERY_LEVEL_MAX).round()))
        }
        Conv::Binary { on, off } => {
            let text = payload_text(Some(raw))?;
            if text.eq_ignore_ascii_case(on) {
                Some(Value::Bool(true))
            } else if text.eq_ignore_ascii_case(off) {
                Some(Value::Bool(false))
            } else {
                None
            }
        }
        Conv::Raw => match raw {
            Json::Bool(_) | Json::Number(_) | Json::String(_) => Some(Value::from_json(raw)),
            Json::Null | Json::Array(_) | Json::Object(_) => None,
        },
    }
}

/// An exact integer stays an integer (`9`, not `9.0`).
fn number(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        #[allow(clippy::cast_possible_truncation)]
        Value::Int(f as i64)
    } else {
        Value::Float(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISCOVERY: &[u8] =
        include_bytes!("../../../integrations/rfxcom2mqtt/fixtures/discovery.json");
    const STATES: &[u8] = include_bytes!("../../../integrations/rfxcom2mqtt/fixtures/states.json");

    #[derive(Deserialize)]
    struct Message {
        topic: String,
        payload: Json,
    }

    fn instance() -> InstanceId {
        InstanceId::from("rfxcom")
    }

    fn messages(bytes: &[u8]) -> Vec<Message> {
        serde_json::from_slice(bytes).unwrap()
    }

    fn payload_bytes(payload: &Json) -> Vec<u8> {
        match payload {
            Json::String(s) => s.clone().into_bytes(),
            other => other.to_string().into_bytes(),
        }
    }

    /// Discovery topic → component (`homeassistant/sensor/…/config`).
    fn component(topic: &str) -> &str {
        topic.split('/').nth(1).unwrap()
    }

    fn catalog() -> Catalog {
        let mut catalog = Catalog::default();
        for m in messages(DISCOVERY) {
            catalog.apply_config(
                &instance(),
                "rfxcom2mqtt",
                &m.topic,
                component(&m.topic),
                &payload_bytes(&m.payload),
            );
        }
        catalog
    }

    #[test]
    fn devices_come_from_the_bridges_discovery_only() {
        let catalog = catalog();
        let ids: Vec<&str> = catalog.entries().map(|e| e.device.id.as_str()).collect();
        // Two sensors and one switch unit; the bridge's own entities and the
        // Zigbee2MQTT config on the same broker are not devices.
        assert_eq!(
            ids,
            ["rfxcom:0x00ABCDE1_12", "rfxcom:0x5A01", "rfxcom:0x6B02"]
        );
    }

    #[test]
    fn a_sensor_gets_typed_points() {
        let catalog = catalog();
        let sensor = catalog.by_state("0x5A01").unwrap();
        assert_eq!(&*sensor.device.native_name, "Sonde salon");
        let temperature = sensor.device.point("temperature").unwrap();
        assert_eq!(temperature.unit, Some(Unit::Celsius));
        assert_eq!(temperature.semantic, Semantic::Temperature);
        assert!(!temperature.access.write);
        assert_eq!(
            sensor.device.point("battery").unwrap().semantic,
            Semantic::Battery
        );
        assert_eq!(
            sensor.device.point("rssi").unwrap().semantic,
            Semantic::SignalStrength
        );
        assert_eq!(
            sensor.device.point("humidity").unwrap().unit,
            Some(Unit::Percent)
        );
    }

    #[test]
    fn a_switch_unit_is_writable_through_its_command_topic() {
        let catalog = catalog();
        let switch = catalog.by_state("0x00ABCDE1/12").unwrap();
        assert_eq!(&*switch.device.native_name, "AC_00ABCDE1 12");
        let on = switch.device.point("on").unwrap();
        assert!(on.access.write);
        assert_eq!(on.semantic, Semantic::OnOff);
        assert_eq!(
            switch.encode("on", &Value::Bool(true)).unwrap(),
            (
                "rfxcom2mqtt/cmd/lighting2/0/0x00ABCDE1/12".to_owned(),
                "On".to_owned()
            )
        );
        assert_eq!(switch.encode("on", &Value::Bool(false)).unwrap().1, "Off");
        assert!(switch.encode("on", &Value::Int(1)).is_err());
        assert!(switch.encode("rssi", &Value::Int(1)).is_err());
        let sensor = catalog.by_state("0x5A01").unwrap();
        assert!(sensor.encode("on", &Value::Bool(true)).is_err());
    }

    #[test]
    fn states_decode_received_frames_and_command_echoes() {
        let catalog = catalog();
        let mut decoded = Vec::new();
        for m in messages(STATES) {
            let Some(rest) = m.topic.strip_prefix("rfxcom2mqtt/devices/") else {
                continue;
            };
            if let Some(entry) = catalog.by_state(rest) {
                decoded.push((rest.to_owned(), entry.decode(&payload_bytes(&m.payload))));
            }
        }
        let sensor = &decoded.iter().find(|(r, _)| r == "0x5A01").unwrap().1;
        assert!(sensor.contains(&("temperature".into(), Value::Float(21.4))));
        assert!(sensor.contains(&("humidity".into(), Value::Int(52))));
        assert!(sensor.contains(&("battery".into(), Value::Int(100))));
        assert!(sensor.contains(&("rssi".into(), Value::Int(5))));
        let flat = &decoded.iter().find(|(r, _)| r == "0x6B02").unwrap().1;
        assert!(flat.contains(&("battery".into(), Value::Int(0))));
        // The received frame and the echo of a sent command both carry `command`.
        let switch: Vec<&Vec<(String, Value)>> = decoded
            .iter()
            .filter(|(r, _)| r == "0x00ABCDE1/12")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(switch.len(), 3);
        assert_eq!(switch[0], &vec![("on".to_owned(), Value::Bool(false))]);
        assert_eq!(switch[1], &vec![("on".to_owned(), Value::Bool(true))]);
        assert_eq!(switch[2], &vec![("on".to_owned(), Value::Bool(false))]);
        // A stray neighbour's sensor has no device.
        assert!(catalog.by_state("0x7C03").is_none());
    }

    #[test]
    fn an_emptied_config_removes_its_point_then_the_device() {
        let mut catalog = catalog();
        let topic = "homeassistant/switch/0x00ABCDE1/12/config";
        let change = catalog.apply_config(&instance(), "rfxcom2mqtt", topic, "switch", b"");
        assert_eq!(
            change,
            Change::Remove(DeviceId::new(&instance(), "0x00ABCDE1/12"))
        );
        assert!(catalog.by_state("0x00ABCDE1/12").is_none());
        let change = catalog.apply_config(
            &instance(),
            "rfxcom2mqtt",
            "homeassistant/sensor/0x5A01/linkquality/config",
            "sensor",
            b"",
        );
        let Change::Upsert(device) = change else {
            panic!("the sensor keeps its other points");
        };
        assert!(device.point("rssi").is_none());
        assert!(device.point("temperature").is_some());
    }

    #[test]
    fn a_signal_alone_makes_no_device() {
        let mut catalog = Catalog::default();
        let signal = br#"{"state_topic":"rfxcom2mqtt/devices/0x00ABCDE1","value_template":"{{ value_json.rssi }}","device":{"name":"AC_00ABCDE1"}}"#;
        let change = catalog.apply_config(
            &instance(),
            "rfxcom2mqtt",
            "homeassistant/sensor/0x00ABCDE1/linkquality/config",
            "sensor",
            signal,
        );
        assert_eq!(change, Change::None);
        assert!(catalog.is_empty());
        assert!(catalog.by_state("0x00ABCDE1").is_none());
        // A sensor whose signal config comes first appears with its
        // temperature, signal included.
        let mut catalog = Catalog::default();
        let mut sorted = messages(DISCOVERY);
        sorted.sort_by_key(|m| !m.topic.ends_with("linkquality/config"));
        for m in &sorted {
            catalog.apply_config(
                &instance(),
                "rfxcom2mqtt",
                &m.topic,
                component(&m.topic),
                &payload_bytes(&m.payload),
            );
        }
        let sensor = catalog.by_state("0x5A01").unwrap();
        assert!(sensor.device.point("rssi").is_some());
        assert!(sensor.device.point("temperature").is_some());
    }

    #[test]
    fn a_repeated_config_changes_nothing() {
        let mut catalog = catalog();
        let m = &messages(DISCOVERY)[0];
        let change = catalog.apply_config(
            &instance(),
            "rfxcom2mqtt",
            &m.topic,
            component(&m.topic),
            &payload_bytes(&m.payload),
        );
        assert_eq!(change, Change::None);
    }

    #[test]
    fn a_command_topic_outside_the_bridge_is_refused() {
        let mut catalog = Catalog::default();
        let config = br#"{"state_topic":"rfxcom2mqtt/devices/0x01/1","value_template":"{{ value_json.command }}","command_topic":"zigbee2mqtt/porte/set","payload_on":"On","payload_off":"Off","device":{"name":"x"}}"#;
        let change = catalog.apply_config(
            &instance(),
            "rfxcom2mqtt",
            "homeassistant/switch/0x01/1/config",
            "switch",
            config,
        );
        assert_eq!(change, Change::None);
        assert!(catalog.is_empty());
    }

    #[test]
    fn two_state_topics_never_share_a_device_id() {
        let mut catalog = Catalog::default();
        let unit = br#"{"state_topic":"rfxcom2mqtt/devices/0x01/12","value_template":"{{ value_json.command }}","command_topic":"rfxcom2mqtt/cmd/lighting2/0/0x01/12","payload_on":"On","payload_off":"Off","device":{"name":"x"}}"#;
        let flat = br#"{"state_topic":"rfxcom2mqtt/devices/0x01_12","value_template":"{{ value_json.temperature }}","device":{"name":"y"}}"#;
        let first = catalog.apply_config(
            &instance(),
            "rfxcom2mqtt",
            "homeassistant/switch/0x01/12/config",
            "switch",
            unit,
        );
        assert!(matches!(first, Change::Upsert(_)));
        let second = catalog.apply_config(
            &instance(),
            "rfxcom2mqtt",
            "homeassistant/sensor/0x01_12/temperature/config",
            "sensor",
            flat,
        );
        assert_eq!(second, Change::None);
        assert_eq!(catalog.len(), 1);
        assert!(catalog.by_state("0x01_12").is_none());
        let kept = catalog.by_state("0x01/12").unwrap();
        assert!(kept.device.point("temperature").is_none());
    }

    #[test]
    fn devices_and_points_are_capped() {
        let sensor = |id: &str, field: &str| {
            format!(
                r#"{{"state_topic":"rfxcom2mqtt/devices/{id}","value_template":"{{{{ value_json.{field} }}}}","device":{{"name":"x"}}}}"#
            )
        };
        let mut catalog = Catalog::default();
        for n in 0..=MAX_DEVICES {
            let id = format!("0x{n:04X}");
            catalog.apply_config(
                &instance(),
                "rfxcom2mqtt",
                &format!("homeassistant/sensor/{id}/temperature/config"),
                "sensor",
                sensor(&id, "temperature").as_bytes(),
            );
        }
        assert_eq!(catalog.len(), MAX_DEVICES);
        let mut catalog = Catalog::default();
        for n in 0..=MAX_POINTS {
            catalog.apply_config(
                &instance(),
                "rfxcom2mqtt",
                &format!("homeassistant/sensor/0x01/field{n}/config"),
                "sensor",
                sensor("0x01", &format!("field{n}")).as_bytes(),
            );
        }
        let device = &catalog.by_state("0x01").unwrap().device;
        assert_eq!(device.points.len(), MAX_POINTS);
        // A point already known still updates at the cap.
        let change = catalog.apply_config(
            &instance(),
            "rfxcom2mqtt",
            "homeassistant/sensor/0x01/field0/config",
            "sensor",
            sensor("0x01", "field0").as_bytes(),
        );
        assert_eq!(change, Change::None);
        assert!(
            catalog
                .by_state("0x01")
                .unwrap()
                .device
                .point("field0")
                .is_some()
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(
            template_field("{{ value_json.batteryLevel }}").as_deref(),
            Some("batteryLevel")
        );
        assert_eq!(template_field("{{ value }}"), None);
        assert_eq!(snake("batteryVoltage"), "battery_voltage");
        assert_eq!(
            state_rest("rfxcom2mqtt/devices/0x01/2", "rfxcom2mqtt"),
            Some("0x01/2")
        );
        assert_eq!(state_rest("rfxcom2mqtt/devices", "rfxcom2mqtt"), None);
        assert_eq!(state_rest("rfxcom2mqtt/devices//2", "rfxcom2mqtt"), None);
        assert_eq!(state_rest("zigbee2mqtt/devices/0x01", "rfxcom2mqtt"), None);
        assert_eq!(
            convert(&Conv::BatteryLevel, &serde_json::json!(5)),
            Some(Value::Int(56))
        );
    }

    #[test]
    fn a_raw_field_keeps_scalars_only() {
        assert!(convert(&Conv::Raw, &serde_json::json!("RAIN")).is_some());
        assert!(convert(&Conv::Raw, &serde_json::json!(3)).is_some());
        assert!(convert(&Conv::Raw, &serde_json::json!(true)).is_some());
        assert_eq!(convert(&Conv::Raw, &serde_json::json!({"a": 1})), None);
        assert_eq!(convert(&Conv::Raw, &serde_json::json!([1, 2])), None);
        assert_eq!(convert(&Conv::Raw, &serde_json::json!(null)), None);
    }
}
