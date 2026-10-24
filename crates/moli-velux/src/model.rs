//! A VELUX ACTIVE home as its cloud describes it: modules (`homesdata`) and
//! their state (`homestatus`). The gateway (NXG) says rain and departure
//! mode; each window, blind or shutter (NXO) its position, 0 closed to 100
//! open, and where it is heading; the indoor climate sensor (NXS) reads its
//! room (temperature in tenths of a degree, humidity, CO₂, light, air
//! quality); the departure switch (NXD) only has a battery.

use std::sync::Arc;

use moli_core::{Access, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::Value as Json;

/// What a module is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Gateway,
    Window,
    /// A blind, an awning or a roller shutter: something in front of a window.
    Cover,
    Sensor,
    Departure,
}

#[derive(Clone, Debug)]
pub struct Module {
    pub id: String,
    pub role: Role,
    pub name: String,
    pub room: Option<String>,
    room_id: Option<String>,
    /// `velux_type` (`window`, `blind`, `shutter`…), else the module type.
    pub model: String,
}

/// The modules of a home, as `homesdata` lists them.
#[derive(Clone, Debug)]
pub struct Home {
    pub id: String,
    pub modules: Vec<Module>,
}

impl Home {
    /// The home of one `homesdata` entry, with the modules Moli knows.
    #[must_use]
    pub fn parse(home: &Json) -> Option<Self> {
        let rooms = home["rooms"].as_array();
        let room_name = |id: &str| {
            rooms?
                .iter()
                .find(|r| r["id"] == id)
                .and_then(|r| r["name"].as_str())
                .map(|n| n.trim().to_owned())
        };
        let modules = home["modules"]
            .as_array()?
            .iter()
            .filter_map(|m| {
                let kind = m["type"].as_str()?;
                let velux = m["velux_type"].as_str();
                let role = match (kind, velux) {
                    ("NXG", _) => Role::Gateway,
                    ("NXO", Some("window")) => Role::Window,
                    ("NXO", _) => Role::Cover,
                    ("NXS", _) => Role::Sensor,
                    ("NXD", _) => Role::Departure,
                    _ => return None,
                };
                let room_id = m["room_id"].as_str().map(str::to_owned);
                Some(Module {
                    id: m["id"].as_str()?.to_owned(),
                    role,
                    name: m["name"].as_str().unwrap_or(kind).trim().to_owned(),
                    room: room_id.as_deref().and_then(room_name),
                    room_id,
                    model: velux.unwrap_or(kind).to_owned(),
                })
            })
            .collect();
        Some(Self {
            id: home["id"].as_str()?.to_owned(),
            modules,
        })
    }
}

/// A module's state in one `homestatus`, `None` when it is not there (the
/// gateway unreachable: the cloud lists it among errors).
fn state<'a>(status: &'a Json, id: &str) -> Option<&'a Json> {
    status["modules"].as_array()?.iter().find(|m| m["id"] == id)
}

fn room<'a>(status: &'a Json, id: Option<&str>) -> Option<&'a Json> {
    status["rooms"]
        .as_array()?
        .iter()
        .find(|r| id.is_some_and(|id| r["id"] == id))
}

/// A measure of the sensor's room.
struct Measure {
    /// Its name in `homestatus`.
    cloud: &'static str,
    key: &'static str,
    label: &'static str,
    unit: Option<Unit>,
    semantic: Semantic,
    /// Sent in tenths (the temperature: 191 is 19.1 °C).
    tenths: bool,
}

const MEASURES: [Measure; 5] = [
    Measure {
        cloud: "temperature",
        key: "temperature",
        label: "pilotes.velux.temperature",
        unit: Some(Unit::Celsius),
        semantic: Semantic::Temperature,
        tenths: true,
    },
    Measure {
        cloud: "humidity",
        key: "humidity",
        label: "pilotes.velux.humidite",
        unit: Some(Unit::Percent),
        semantic: Semantic::Humidity,
        tenths: false,
    },
    Measure {
        cloud: "co2",
        key: "co2",
        label: "pilotes.velux.co2",
        unit: Some(Unit::Ppm),
        semantic: Semantic::Other,
        tenths: false,
    },
    Measure {
        cloud: "lux",
        key: "illuminance",
        label: "pilotes.velux.luminosite",
        unit: Some(Unit::Lux),
        semantic: Semantic::Illuminance,
        tenths: false,
    },
    Measure {
        cloud: "air_quality",
        key: "air_quality",
        label: "pilotes.velux.qualite_air",
        unit: None,
        semantic: Semantic::Other,
        tenths: false,
    },
];

impl Module {
    /// The points this module shows, from what its first state carries.
    #[must_use]
    pub fn points(&self, status: &Json) -> Vec<PointSpec> {
        let own = state(status, &self.id);
        let has = |key: &str| own.is_some_and(|s| !s[key].is_null());
        let mut out = Vec::new();
        match self.role {
            Role::Gateway => {
                out.push(spec(
                    "rain",
                    "pilotes.velux.pluie",
                    Kind::Binary,
                    None,
                    Semantic::Other,
                ));
                out.push(spec(
                    "departure",
                    "pilotes.velux.depart",
                    Kind::Binary,
                    None,
                    Semantic::Other,
                ));
            }
            Role::Window | Role::Cover => {
                out.push(spec(
                    "percent_control",
                    "pilotes.velux.ouverture",
                    Kind::Numeric {
                        min: Some(0.0),
                        max: Some(100.0),
                        step: Some(1.0),
                    },
                    Some(Unit::Percent),
                    Semantic::Other,
                ));
                out.push(spec(
                    "moving",
                    "pilotes.velux.en_mouvement",
                    Kind::Binary,
                    None,
                    Semantic::Other,
                ));
                if has("battery_state") {
                    out.push(spec(
                        "battery_low",
                        "pilotes.velux.pile_faible",
                        Kind::Binary,
                        None,
                        Semantic::BatteryLow,
                    ));
                }
            }
            Role::Sensor => {
                let measured = room(status, self.room_id.as_deref());
                for m in &MEASURES {
                    if measured.is_some_and(|r| r[m.cloud].is_number()) {
                        out.push(spec(m.key, m.label, number(), m.unit.clone(), m.semantic));
                    }
                }
            }
            Role::Departure => {}
        }
        if matches!(self.role, Role::Sensor | Role::Departure) && has("battery_percent") {
            out.push(spec(
                "battery",
                "pilotes.velux.pile",
                number(),
                Some(Unit::Percent),
                Semantic::Battery,
            ));
        }
        out
    }

    /// The module's values in one `homestatus`, and whether it answers.
    /// `None`: it is not in this status at all.
    #[must_use]
    pub fn values(&self, status: &Json) -> Option<(bool, Vec<(&'static str, Value)>)> {
        let own = state(status, &self.id)?;
        let mut out = Vec::new();
        let flag = |v: &Json| v.as_bool().map(Value::from);
        match self.role {
            Role::Gateway => {
                out.extend(flag(&own["is_raining"]).map(|v| ("rain", v)));
                out.extend(flag(&own["locked"]).map(|v| ("departure", v)));
            }
            Role::Window | Role::Cover => {
                let now = own["current_position"].as_i64();
                let target = own["target_position"].as_i64();
                out.extend(now.map(|p| ("percent_control", Value::from(p))));
                if let (Some(now), Some(target)) = (now, target) {
                    out.push(("moving", Value::from(now != target)));
                }
                if let Some(battery) = own["battery_state"].as_str() {
                    let low = matches!(battery, "low" | "very_low");
                    out.push(("battery_low", Value::from(low)));
                }
            }
            Role::Sensor => {
                if let Some(measured) = room(status, self.room_id.as_deref()) {
                    for m in &MEASURES {
                        let v = &measured[m.cloud];
                        let value = match (m.tenths, v.as_i64(), v.as_f64()) {
                            (false, Some(n), _) => Value::from(n),
                            (true, _, Some(t)) => Value::from((t * 10.0).round() / 100.0),
                            (false, None, Some(x)) => Value::from(x),
                            _ => continue,
                        };
                        out.push((m.key, value));
                    }
                }
            }
            Role::Departure => {}
        }
        if matches!(self.role, Role::Sensor | Role::Departure) {
            out.extend(
                own["battery_percent"]
                    .as_i64()
                    .map(|b| ("battery", Value::from(b))),
            );
        }
        // The gateway's own state has no `reachable`: there, it answers.
        let answers = own["reachable"].as_bool().unwrap_or(true);
        Some((answers, out))
    }
}

fn number() -> Kind {
    Kind::Numeric {
        min: None,
        max: None,
        step: None,
    }
}

fn spec(key: &str, label: &str, kind: Kind, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: Arc::from(key),
        label: moli_i18n::tr(label).into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real KIX 300 home (gateway, 7 windows, 8 blinds, a sensor, a
    /// departure switch), ids made up.
    fn kix300() -> (Home, Json) {
        let text = include_str!("../../../integrations/velux/fixtures/kix300.json");
        let file: Json = serde_json::from_str(text).unwrap();
        (
            Home::parse(&file["homesdata"]).unwrap(),
            file["homestatus"].clone(),
        )
    }

    fn module<'a>(home: &'a Home, name: &str) -> &'a Module {
        home.modules.iter().find(|m| m.name == name).unwrap()
    }

    fn value(m: &Module, status: &Json, key: &str) -> Option<Value> {
        m.values(status)?
            .1
            .into_iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    #[test]
    fn every_module_has_its_role() {
        let (home, _) = kix300();
        let count = |r| home.modules.iter().filter(|m| m.role == r).count();
        assert_eq!(home.modules.len(), 18);
        assert_eq!(count(Role::Gateway), 1);
        assert_eq!(count(Role::Window), 7);
        assert_eq!(count(Role::Cover), 8);
        assert_eq!(count(Role::Sensor), 1);
        assert_eq!(count(Role::Departure), 1);
    }

    #[test]
    fn a_module_keeps_its_velux_room() {
        let (home, _) = kix300();
        assert_eq!(
            module(&home, "Store Chambre").room.as_deref(),
            Some("Chambre")
        );
        assert_eq!(
            module(&home, "Fenêtre grenier").room.as_deref(),
            Some("Grenier")
        );
        assert_eq!(module(&home, "Commande de départ").room, None, "in no room");
    }

    #[test]
    fn positions_go_from_closed_to_open() {
        let (home, status) = kix300();
        let open = module(&home, "Mezzanine Droite");
        assert_eq!(
            value(open, &status, "percent_control"),
            Some(Value::Int(61))
        );
        assert_eq!(value(open, &status, "moving"), Some(Value::Bool(false)));
        let blind = module(&home, "Store grenier");
        assert_eq!(
            value(blind, &status, "percent_control"),
            Some(Value::Int(18))
        );
        let closed = module(&home, "Store Chambre");
        assert_eq!(
            value(closed, &status, "percent_control"),
            Some(Value::Int(0))
        );
    }

    #[test]
    fn a_module_on_its_way_is_moving() {
        let (home, mut status) = kix300();
        let blind = module(&home, "Store Chambre");
        for m in status["modules"].as_array_mut().unwrap() {
            if m["id"] == blind.id.as_str() {
                m["target_position"] = 100.into();
            }
        }
        assert_eq!(value(blind, &status, "moving"), Some(Value::Bool(true)));
    }

    #[test]
    fn the_sensor_reads_its_room() {
        let (home, status) = kix300();
        let sensor = home
            .modules
            .iter()
            .find(|m| m.role == Role::Sensor)
            .unwrap();
        let keys: Vec<_> = sensor
            .points(&status)
            .iter()
            .map(|p| p.key.to_string())
            .collect();
        assert_eq!(
            keys,
            [
                "temperature",
                "humidity",
                "co2",
                "illuminance",
                "air_quality",
                "battery"
            ]
        );
        assert_eq!(
            value(sensor, &status, "temperature"),
            Some(Value::Float(19.1))
        );
        assert_eq!(value(sensor, &status, "co2"), Some(Value::Int(694)));
        assert_eq!(value(sensor, &status, "battery"), Some(Value::Int(76)));
    }

    #[test]
    fn the_gateway_says_rain_and_departure() {
        let (home, status) = kix300();
        let gateway = home
            .modules
            .iter()
            .find(|m| m.role == Role::Gateway)
            .unwrap();
        let (answers, values) = gateway.values(&status).unwrap();
        assert!(answers);
        assert_eq!(
            values,
            [
                ("rain", Value::Bool(false)),
                ("departure", Value::Bool(false))
            ]
        );
    }

    #[test]
    fn nothing_is_writable_yet() {
        let (home, status) = kix300();
        for m in &home.modules {
            assert!(
                m.points(&status).iter().all(|p| !p.access.write),
                "{}",
                m.name
            );
        }
    }

    #[test]
    fn a_window_without_battery_has_no_battery_point() {
        let (home, status) = kix300();
        let mains = module(&home, "Mezzanine Droite");
        assert!(
            mains
                .points(&status)
                .iter()
                .all(|p| &*p.key != "battery_low")
        );
        let solar = module(&home, "Salon");
        assert!(
            solar
                .points(&status)
                .iter()
                .any(|p| &*p.key == "battery_low")
        );
        assert_eq!(
            value(solar, &status, "battery_low"),
            Some(Value::Bool(false))
        );
    }

    #[test]
    fn an_unreachable_module_says_so() {
        let (home, mut status) = kix300();
        let blind = module(&home, "Store grenier");
        for m in status["modules"].as_array_mut().unwrap() {
            if m["id"] == blind.id.as_str() {
                m["reachable"] = false.into();
            }
        }
        assert!(!blind.values(&status).unwrap().0);
        status["modules"] = Json::Array(Vec::new());
        assert!(blind.values(&status).is_none(), "missing from the status");
    }
}
