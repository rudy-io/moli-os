//! Moli's devices as HomeKit accessories: which ones, with which services,
//! and how a value goes from one to the other.
//!
//! - a `guard_mode` point (a security station) → *Security System*;
//! - `motion` → *Motion Sensor*, `person` → *Occupancy Sensor*;
//! - `temperature` → *Temperature Sensor*;
//! - `doorbell` → *Doorbell* (a press each time it turns true);
//! - `control` + `percent_control` → *Window Covering* (a shutter);
//! - `door_control` + `door_position` → *Garage Door Opener*;
//! - `gate` → a *Switch* that pulses the gate and falls back to off.
//!
//! Accessory ids are kept per device (`aids`): HomeKit remembers rooms and
//! names by them. Instance ids follow from the order below, fixed.

use std::collections::BTreeMap;

use moli_core::{DeviceId, PointId, Value};
use serde_json::{Value as Json, json};

/// HomeKit types (short form of `0000XXXX-0000-1000-8000-0026BB765291`).
mod kind {
    pub const INFORMATION: &str = "3E";
    pub const PROTOCOL: &str = "A2";
    pub const SECURITY: &str = "7E";
    pub const MOTION: &str = "85";
    pub const OCCUPANCY: &str = "86";
    pub const DOORBELL: &str = "121";
    pub const COVERING: &str = "8C";
    pub const GARAGE: &str = "41";
    pub const SWITCH: &str = "49";
    pub const TEMPERATURE: &str = "8A";

    pub const IDENTIFY: &str = "14";
    pub const MANUFACTURER: &str = "20";
    pub const MODEL: &str = "21";
    pub const NAME: &str = "23";
    pub const SERIAL: &str = "30";
    pub const FIRMWARE: &str = "52";
    pub const VERSION: &str = "37";
    pub const CURRENT_SECURITY: &str = "66";
    pub const TARGET_SECURITY: &str = "67";
    pub const MOTION_DETECTED: &str = "22";
    pub const OCCUPANCY_DETECTED: &str = "71";
    pub const SWITCH_EVENT: &str = "73";
    pub const CURRENT_POSITION: &str = "6D";
    pub const TARGET_POSITION: &str = "7C";
    pub const POSITION_STATE: &str = "72";
    pub const CURRENT_DOOR: &str = "E";
    pub const TARGET_DOOR: &str = "32";
    pub const OBSTRUCTION: &str = "24";
    pub const ON: &str = "25";
    pub const CURRENT_TEMPERATURE: &str = "11";
}

/// What a characteristic reflects of Moli.
#[derive(Clone, Debug, PartialEq)]
pub enum Binding {
    /// A constant (names, models…).
    Fixed(Json),
    /// The security system's state, from `guard_mode`; « triggered » while
    /// `alarm` (when the device has one) is anything but `none`.
    Current { mode: PointId, alarm: PointId },
    /// What the person asked; writing it orders `guard_mode`.
    Target(PointId),
    /// A detection, as a boolean (motion) or 0/1 (occupancy).
    Detected(PointId, bool),
    /// A doorbell press: event only, no value.
    Press(PointId),
    /// A position in percent (shutter or door), read, and ordered when written.
    Position(PointId),
    /// Whether a shutter is closing (0), opening (1) or still (2), from its control.
    Moving(PointId),
    /// A garage door's state (0 open, 1 closed, 2 opening, 3 closing, 4 stopped).
    DoorCurrent { control: PointId, position: PointId },
    /// A garage door's target (0 open, 1 closed); writing it orders the control.
    DoorTarget { control: PointId, position: PointId },
    /// A temperature in °C, read only.
    Temperature(PointId),
    /// A pulse: always off when read; writing « on » orders the point.
    Pulse(PointId),
}

impl Binding {
    /// Whether a change of `point` changes what this reflects.
    #[must_use]
    pub fn watches(&self, point: &PointId) -> bool {
        match self {
            Self::Fixed(_) | Self::Press(_) | Self::Pulse(_) => false,
            Self::Current { mode, alarm } => mode == point || alarm == point,
            Self::Target(p)
            | Self::Detected(p, _)
            | Self::Position(p)
            | Self::Moving(p)
            | Self::Temperature(p) => p == point,
            Self::DoorCurrent { control, position } | Self::DoorTarget { control, position } => {
                control == point || position == point
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Characteristic {
    pub json: Json,
    pub binding: Binding,
}

#[derive(Clone, Debug, Default)]
pub struct Accessories {
    /// `(aid, iid)` → characteristic.
    pub chars: BTreeMap<(u64, u64), Characteristic>,
    /// The `/accessories` answer.
    pub json: Json,
}

/// What HomeKit's security system says, from Moli's modes. Modes HomeKit
/// has no name for (Eufy's custom ones, schedule, geofencing) read as
/// « Night », which writes `custom1`.
pub fn security_state(mode: &str) -> Option<u8> {
    Some(match mode {
        "home" => 0,
        "away" => 1,
        "custom1" | "custom2" | "custom3" | "schedule" | "geofencing" => 2,
        "disarmed" => 3,
        _ => return None,
    })
}

pub fn security_mode(state: u64) -> Option<&'static str> {
    Some(match state {
        0 => "home",
        1 => "away",
        2 => "custom1",
        3 => "disarmed",
        _ => return None,
    })
}

/// The value HomeKit reads for a binding, from Moli's current one.
pub fn read(binding: &Binding, current: impl Fn(&PointId) -> Option<Value>) -> Json {
    match binding {
        Binding::Fixed(v) => v.clone(),
        Binding::Current { alarm, .. } if matches!(current(alarm), Some(Value::Text(a)) if &*a != "none") =>
        {
            json!(4)
        }
        Binding::Current { mode: p, .. } | Binding::Target(p) => match current(p) {
            Some(Value::Text(mode)) => security_state(&mode).map_or(Json::Null, Json::from),
            _ => Json::Null,
        },
        Binding::Detected(p, boolean) => {
            let on = matches!(current(p), Some(Value::Bool(true)));
            if *boolean {
                json!(on)
            } else {
                json!(u8::from(on))
            }
        }
        Binding::Press(_) => Json::Null,
        Binding::Position(p) => {
            json!(percent(current(p)).unwrap_or(0))
        }
        Binding::Moving(p) => json!(match current(p) {
            Some(Value::Text(t)) if &*t == "open" => 1,
            Some(Value::Text(t)) if &*t == "close" => 0,
            _ => 2,
        }),
        Binding::DoorCurrent { control, position } => {
            json!(door_state(
                current(control).as_ref(),
                percent(current(position))
            ))
        }
        Binding::DoorTarget { control, position } => {
            json!(door_target(
                current(control).as_ref(),
                percent(current(position))
            ))
        }
        Binding::Temperature(p) => current(p)
            .and_then(|v| v.as_f64())
            .filter(|t| t.is_finite())
            .map_or(Json::Null, |t| json!((t * 10.0).round() / 10.0)),
        Binding::Pulse(_) => json!(false),
    }
}

/// A position as a whole percent, 0 to 100.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=100
fn percent(value: Option<Value>) -> Option<u8> {
    let n = value?.as_f64()?;
    n.is_finite().then(|| n.clamp(0.0, 100.0).round() as u8)
}

/// 0 open, 1 closed, 2 opening, 3 closing, 4 stopped half way.
fn door_state(control: Option<&Value>, position: Option<u8>) -> u8 {
    match control {
        Some(Value::Text(t)) if &**t == "open" => 2,
        Some(Value::Text(t)) if &**t == "close" => 3,
        _ => match position {
            Some(0) => 1,
            Some(100) => 0,
            _ => 4,
        },
    }
}

/// Where the door is heading: 0 open, 1 closed.
fn door_target(control: Option<&Value>, position: Option<u8>) -> u8 {
    match control {
        Some(Value::Text(t)) if &**t == "open" => 0,
        Some(Value::Text(t)) if &**t == "close" => 1,
        _ => u8::from(position == Some(0)),
    }
}

/// A device seen by the bridge: its id, names, and the points it has.
/// Kept (`Layout`): an accessory stays while its device has not come back
/// yet after a restart, or HomeKit would forget it, room and all.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Source {
    pub id: DeviceId,
    pub name: String,
    pub manufacturer: String,
    pub model: String,
    pub keys: Vec<String>,
}

impl Source {
    fn has(&self, key: &str) -> bool {
        self.keys.iter().any(|k| k == key)
    }

    /// Whether the bridge shows it at all.
    pub fn shown(&self) -> bool {
        [
            "guard_mode",
            "motion",
            "person",
            "doorbell",
            "door_control",
            "gate",
            "temperature",
        ]
        .iter()
        .any(|k| self.has(k))
            || (self.has("control") && self.has("percent_control"))
    }
}

struct Builder<'a> {
    aid: u64,
    iid: u64,
    chars: &'a mut BTreeMap<(u64, u64), Characteristic>,
}

impl Builder<'_> {
    fn next(&mut self) -> u64 {
        self.iid += 1;
        self.iid
    }

    fn char(
        &mut self,
        kind: &str,
        perms: &[&str],
        format: &str,
        extra: Json,
        binding: Binding,
    ) -> Json {
        let iid = self.next();
        let mut json = json!({ "iid": iid, "type": kind, "perms": perms, "format": format });
        if let (Json::Object(map), Json::Object(more)) = (&mut json, extra) {
            map.extend(more);
        }
        if let (Json::Object(map), Binding::Fixed(value)) = (&mut json, &binding)
            && !value.is_null()
        {
            map.insert("value".into(), value.clone());
        }
        self.chars.insert(
            (self.aid, iid),
            Characteristic {
                json: json.clone(),
                binding,
            },
        );
        json
    }

    fn fixed(&mut self, kind: &str, value: &str) -> Json {
        self.char(
            kind,
            &["pr"],
            "string",
            json!({}),
            Binding::Fixed(json!(value)),
        )
    }

    fn service(kind: &str, chars: &[Json]) -> Json {
        // The service's own iid comes before its characteristics'.
        json!({ "iid": chars.first().and_then(|c| c["iid"].as_u64()).map_or(0, |i| i - 1), "type": kind, "characteristics": chars })
    }

    fn information(&mut self, name: &str, manufacturer: &str, model: &str, serial: &str) -> Json {
        let _service = self.next();
        let chars = vec![
            self.char(
                kind::IDENTIFY,
                &["pw"],
                "bool",
                json!({}),
                Binding::Fixed(Json::Null),
            ),
            self.fixed(kind::MANUFACTURER, manufacturer),
            self.fixed(kind::MODEL, model),
            self.fixed(kind::NAME, name),
            self.fixed(kind::SERIAL, serial),
            self.fixed(kind::FIRMWARE, env!("CARGO_PKG_VERSION")),
        ];
        Self::service(kind::INFORMATION, &chars)
    }
}

/// HomeKit names: letters, digits, spaces and a little punctuation.
fn clean(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || " '-.,".contains(*c))
        .collect();
    let kept = kept.trim();
    if kept.is_empty() {
        "Moli".into()
    } else {
        kept.chars().take(64).collect()
    }
}

/// The whole bridge: Moli itself (aid 1) and the devices shown, in `aids`
/// order. `labels` gives each detection's service name.
#[allow(clippy::too_many_lines)]
pub fn build(
    bridge_name: &str,
    sources: &[Source],
    aids: &BTreeMap<String, u64>,
    labels: &Labels,
) -> Accessories {
    let mut chars = BTreeMap::new();
    let mut accessories = Vec::new();
    {
        let mut b = Builder {
            aid: 1,
            iid: 0,
            chars: &mut chars,
        };
        let info = b.information(&clean(bridge_name), "Moli", "Moli OS", "moli");
        let _service = b.next();
        let version = vec![b.fixed(kind::VERSION, "1.1.0")];
        let protocol = Builder::service(kind::PROTOCOL, &version);
        accessories.push(json!({ "aid": 1, "services": [info, protocol] }));
    }
    let mut shown: Vec<(&Source, u64)> = sources
        .iter()
        .filter(|s| s.shown())
        .filter_map(|s| aids.get(s.id.as_str()).map(|aid| (s, *aid)))
        .collect();
    shown.sort_by_key(|(_, aid)| *aid);
    for (source, aid) in shown {
        let mut b = Builder {
            aid,
            iid: 0,
            chars: &mut chars,
        };
        let name = clean(&source.name);
        let serial = source.id.as_str().rsplit(':').next().unwrap_or("moli");
        let mut services = vec![b.information(&name, &source.manufacturer, &source.model, serial)];
        let point = |key: &str| PointId::new(&source.id, key);
        if source.has("guard_mode") {
            let _service = b.next();
            let states = json!({ "valid-values": [0, 1, 2, 3, 4] });
            let current = b.char(
                kind::CURRENT_SECURITY,
                &["pr", "ev"],
                "uint8",
                states,
                Binding::Current {
                    mode: point("guard_mode"),
                    alarm: point("alarm"),
                },
            );
            let targets = json!({ "valid-values": [0, 1, 2, 3] });
            let target = b.char(
                kind::TARGET_SECURITY,
                &["pr", "pw", "ev"],
                "uint8",
                targets,
                Binding::Target(point("guard_mode")),
            );
            services.push(Builder::service(kind::SECURITY, &[current, target]));
        }
        if source.has("motion") {
            let _service = b.next();
            let detected = b.char(
                kind::MOTION_DETECTED,
                &["pr", "ev"],
                "bool",
                json!({}),
                Binding::Detected(point("motion"), true),
            );
            let named = b.fixed(kind::NAME, &labels.motion);
            services.push(Builder::service(kind::MOTION, &[detected, named]));
        }
        if source.has("person") {
            let _service = b.next();
            let detected = b.char(
                kind::OCCUPANCY_DETECTED,
                &["pr", "ev"],
                "uint8",
                json!({ "valid-values": [0, 1] }),
                Binding::Detected(point("person"), false),
            );
            let named = b.fixed(kind::NAME, &labels.person);
            services.push(Builder::service(kind::OCCUPANCY, &[detected, named]));
        }
        if source.has("doorbell") {
            let _service = b.next();
            let press = b.char(
                kind::SWITCH_EVENT,
                &["pr", "ev"],
                "uint8",
                json!({ "valid-values": [0] }),
                Binding::Press(point("doorbell")),
            );
            let named = b.fixed(kind::NAME, &labels.doorbell);
            services.push(Builder::service(kind::DOORBELL, &[press, named]));
        }
        if source.has("control") && source.has("percent_control") {
            let _service = b.next();
            let control = point("control");
            let position = point("percent_control");
            let ranged =
                json!({ "minValue": 0, "maxValue": 100, "minStep": 1, "unit": "percentage" });
            let current = b.char(
                kind::CURRENT_POSITION,
                &["pr", "ev"],
                "uint8",
                ranged.clone(),
                Binding::Position(position.clone()),
            );
            let target = b.char(
                kind::TARGET_POSITION,
                &["pr", "pw", "ev"],
                "uint8",
                ranged,
                Binding::Position(position),
            );
            let state = b.char(
                kind::POSITION_STATE,
                &["pr", "ev"],
                "uint8",
                json!({ "valid-values": [0, 1, 2] }),
                Binding::Moving(control),
            );
            services.push(Builder::service(kind::COVERING, &[current, target, state]));
        }
        if source.has("door_control") {
            let _service = b.next();
            let control = point("door_control");
            let position = point("door_position");
            let current = b.char(
                kind::CURRENT_DOOR,
                &["pr", "ev"],
                "uint8",
                json!({ "valid-values": [0, 1, 2, 3, 4] }),
                Binding::DoorCurrent {
                    control: control.clone(),
                    position: position.clone(),
                },
            );
            let target = b.char(
                kind::TARGET_DOOR,
                &["pr", "pw", "ev"],
                "uint8",
                json!({ "valid-values": [0, 1] }),
                Binding::DoorTarget { control, position },
            );
            let obstruction = b.char(
                kind::OBSTRUCTION,
                &["pr", "ev"],
                "bool",
                json!({}),
                Binding::Fixed(json!(false)),
            );
            services.push(Builder::service(
                kind::GARAGE,
                &[current, target, obstruction],
            ));
        }
        if source.has("temperature") {
            let _service = b.next();
            let degrees = b.char(
                kind::CURRENT_TEMPERATURE,
                &["pr", "ev"],
                "float",
                json!({ "minValue": -40, "maxValue": 80, "minStep": 0.1, "unit": "celsius" }),
                Binding::Temperature(point("temperature")),
            );
            let named = b.fixed(kind::NAME, &labels.temperature);
            services.push(Builder::service(kind::TEMPERATURE, &[degrees, named]));
        }
        if source.has("gate") {
            let _service = b.next();
            let on = b.char(
                kind::ON,
                &["pr", "pw", "ev"],
                "bool",
                json!({}),
                Binding::Pulse(point("gate")),
            );
            services.push(Builder::service(kind::SWITCH, &[on]));
        }
        accessories.push(json!({ "aid": aid, "services": services }));
    }
    Accessories {
        chars,
        json: json!({ "accessories": accessories }),
    }
}

/// Service names, in the house's language.
#[derive(Clone, Debug)]
pub struct Labels {
    pub motion: String,
    pub person: String,
    pub doorbell: String,
    pub temperature: String,
}

/// Gives every shown device an accessory id, once and for all (2, 3…).
/// `true` if one was added.
pub fn assign(aids: &mut BTreeMap<String, u64>, sources: &[Source]) -> bool {
    let mut added = false;
    for s in sources.iter().filter(|s| s.shown()) {
        if !aids.contains_key(s.id.as_str()) {
            let next = aids.values().copied().max().unwrap_or(1) + 1;
            aids.insert(s.id.as_str().to_owned(), next);
            added = true;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> Labels {
        Labels {
            motion: "Mouvement".into(),
            person: "Personne".into(),
            doorbell: "Sonnette".into(),
            temperature: "Température".into(),
        }
    }

    fn source(id: &str, keys: &[&str]) -> Source {
        Source {
            id: DeviceId::from(id),
            name: "Porte d'entrée 🔔".into(),
            manufacturer: "Eufy".into(),
            model: "T8210".into(),
            keys: keys.iter().map(|k| (*k).to_owned()).collect(),
        }
    }

    #[test]
    fn devices_become_accessories_with_stable_ids() {
        let sources = vec![
            source(
                "eufy:T8210P0000000000",
                &["battery", "motion", "person", "doorbell"],
            ),
            source("eufy:T8010P0000000000", &["guard_mode"]),
            source("hue:lamp", &["state", "brightness"]),
        ];
        let mut aids = BTreeMap::new();
        assert!(assign(&mut aids, &sources));
        assert!(!assign(&mut aids, &sources));
        assert_eq!(aids.len(), 2);
        assert_eq!(aids["eufy:T8210P0000000000"], 2);
        let built = build("Moli", &sources, &aids, &labels());
        let list = built.json["accessories"].as_array().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0]["aid"], 1);
        let doorbell = &list[1];
        let types: Vec<&str> = doorbell["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, ["3E", "85", "86", "121"]);
        // The name keeps what HomeKit accepts.
        assert_eq!(
            doorbell["services"][0]["characteristics"][3]["value"],
            "Porte d'entrée"
        );
        assert!(
            matches!(built.chars[&(2, 5)].binding, Binding::Fixed(ref v) if v == "Porte d'entrée")
        );
        // Each service's iid is right before its first characteristic's.
        for service in doorbell["services"].as_array().unwrap() {
            let first = service["characteristics"][0]["iid"].as_u64().unwrap();
            assert_eq!(service["iid"].as_u64().unwrap() + 1, first);
        }
        let station = &list[2];
        assert_eq!(station["services"][1]["type"], "7E");
    }

    #[test]
    fn a_temperature_becomes_a_sensor_service() {
        let sources = vec![source("somfy:abc", &["battery", "temperature"])];
        let mut aids = BTreeMap::new();
        assert!(assign(&mut aids, &sources));
        let built = build("Moli", &sources, &aids, &labels());
        let services = built.json["accessories"][1]["services"].as_array().unwrap();
        assert_eq!(services[1]["type"], "8A");
        assert_eq!(services[1]["characteristics"][0]["type"], "11");
        let device = DeviceId::from("somfy:abc");
        let p = PointId::new(&device, "temperature");
        let binding = Binding::Temperature(p.clone());
        let at = |t: Option<Value>| move |_: &PointId| t.clone();
        assert_eq!(read(&binding, at(Some(Value::Float(19.54)))), json!(19.5));
        assert_eq!(read(&binding, at(Some(Value::Int(21)))), json!(21.0));
        assert_eq!(read(&binding, at(None)), Json::Null);
        assert!(binding.watches(&p));
    }

    #[test]
    fn modes_and_detections_read_as_homekit_values() {
        let device = DeviceId::from("eufy:T8010P0000000000");
        let p = PointId::new(&device, "guard_mode");
        let current = Binding::Current {
            mode: p.clone(),
            alarm: PointId::new(&device, "alarm"),
        };
        let mode = |m: &'static str| {
            let p = p.clone();
            move |q: &PointId| (*q == p).then(|| Value::Text(m.into()))
        };
        assert_eq!(read(&current, mode("away")), json!(1));
        let ringing = |q: &PointId| {
            Some(Value::Text(
                if q.split().is_some_and(|(_, k)| k == "alarm") {
                    "trespass"
                } else {
                    "away"
                }
                .into(),
            ))
        };
        assert_eq!(read(&current, ringing), json!(4));
        assert_eq!(
            read(&Binding::Target(p.clone()), mode("disarmed")),
            json!(3)
        );
        assert_eq!(read(&current, mode("custom2")), json!(2));
        assert_eq!(read(&current, |_| None), Json::Null);
        assert_eq!(security_mode(0), Some("home"));
        assert_eq!(security_mode(4), None);
        let on = |_: &PointId| Some(Value::Bool(true));
        assert_eq!(read(&Binding::Detected(p.clone(), true), on), json!(true));
        assert_eq!(read(&Binding::Detected(p, false), on), json!(1));
    }

    #[test]
    fn shutters_doors_and_gates_become_accessories() {
        let sources = [
            source("volet:aabb", &["control", "percent_control", "power"]),
            source("garage:aabb", &["door_control", "door_position"]),
            source("portail:aabb", &["gate", "relay_2"]),
        ];
        let mut aids = BTreeMap::new();
        assign(&mut aids, &sources);
        let built = build("Moli", &sources, &aids, &labels());
        let kinds: Vec<Vec<String>> = built.json["accessories"]
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .map(|a| {
                a["services"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s["type"].as_str().unwrap().to_owned())
                    .collect()
            })
            .collect();
        assert_eq!(
            kinds,
            [["3E", "8C"], ["3E", "41"], ["3E", "49"]].map(|k| k.map(String::from).to_vec())
        );
    }

    #[test]
    fn short_types_have_no_leading_zero() {
        // HomeKit drops the whole bridge over one « 0E » (it wants « E »).
        let sources = [source(
            "tout:aabb",
            &[
                "guard_mode",
                "motion",
                "person",
                "doorbell",
                "control",
                "percent_control",
                "door_control",
                "door_position",
                "gate",
            ],
        )];
        let mut aids = BTreeMap::new();
        assign(&mut aids, &sources);
        let built = build("Moli", &sources, &aids, &labels());
        for accessory in built.json["accessories"].as_array().unwrap() {
            for service in accessory["services"].as_array().unwrap() {
                let chars = service["characteristics"].as_array().unwrap();
                for t in chars
                    .iter()
                    .chain([service])
                    .map(|j| j["type"].as_str().unwrap())
                {
                    assert!(!t.starts_with('0'), "{t}");
                }
            }
        }
    }

    #[test]
    fn a_garage_door_reads_from_its_control_and_position() {
        let device = DeviceId::from("garage:aabb");
        let control = PointId::new(&device, "door_control");
        let position = PointId::new(&device, "door_position");
        let current = Binding::DoorCurrent {
            control: control.clone(),
            position: position.clone(),
        };
        let target = Binding::DoorTarget {
            control: control.clone(),
            position: position.clone(),
        };
        let state = |c: &'static str, pos: i64| {
            let (control, position) = (control.clone(), position.clone());
            move |p: &PointId| {
                if *p == control {
                    Some(Value::Text(c.into()))
                } else if *p == position {
                    Some(Value::Int(pos))
                } else {
                    None
                }
            }
        };
        assert_eq!(read(&current, state("stop", 0)), json!(1));
        assert_eq!(read(&current, state("stop", 100)), json!(0));
        assert_eq!(read(&current, state("stop", 40)), json!(4));
        assert_eq!(read(&current, state("open", 10)), json!(2));
        assert_eq!(read(&current, state("close", 90)), json!(3));
        assert_eq!(read(&target, state("stop", 0)), json!(1));
        assert_eq!(read(&target, state("stop", 100)), json!(0));
        assert_eq!(read(&target, state("close", 90)), json!(1));
        assert!(current.watches(&position) && current.watches(&control));
    }

    #[test]
    fn a_pulse_always_reads_off() {
        let p = PointId::new(&DeviceId::from("portail:aabb"), "gate");
        let on = |_: &PointId| Some(Value::Bool(true));
        assert_eq!(read(&Binding::Pulse(p), on), json!(false));
    }
}
