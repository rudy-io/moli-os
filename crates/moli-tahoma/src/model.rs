//! Pure model: the gateway's `/setup/devices` becomes Moli devices, and a
//! Moli command becomes an Overkiz action. No I/O here, so the mapping is
//! tested against recorded answers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde::Deserialize;
use serde_json::Value as Json;

/// Overkiz counts a closure from the open end: 0 = open, 100 = closed.
const FULLY_CLOSED: f64 = 100.0;

/// Overkiz reports these in `core:ClosureState` when it has no position:
/// 108 is « my » (a stored favourite), 124 is unknown.
const NOT_A_POSITION: [i64; 2] = [108, 124];

/// How a device is driven: a closure counts closed, an awning counts
/// deployed. Both end up as Moli's opening percentage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Closure,
    Deployment,
}

/// One device as the gateway describes it. Everything is optional: firmware
/// versions differ, and a missing field must never lose the whole answer.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawDevice {
    #[serde(rename = "deviceURL", default)]
    pub url: String,
    #[serde(default)]
    pub label: String,
    #[serde(rename = "uiClass", default)]
    pub ui_class: String,
    #[serde(rename = "controllableName", default)]
    pub controllable_name: String,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub states: Vec<State>,
    #[serde(default)]
    pub definition: Definition,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Definition {
    #[serde(rename = "uiClass", default)]
    pub ui_class: String,
    #[serde(default)]
    pub commands: Vec<Command>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Command {
    #[serde(rename = "commandName", default)]
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct State {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: Json,
}

impl RawDevice {
    /// Some firmware puts the class only in the definition.
    #[must_use]
    pub fn class(&self) -> &str {
        if self.ui_class.is_empty() {
            &self.definition.ui_class
        } else {
            &self.ui_class
        }
    }

    fn state(&self, name: &str) -> Option<&Json> {
        self.states
            .iter()
            .find(|s| s.name == name)
            .map(|s| &s.value)
    }

    /// The commands this very device accepts.
    fn commands(&self) -> HashSet<&str> {
        self.definition
            .commands
            .iter()
            .map(|c| c.name.as_str())
            .filter(|n| !n.is_empty())
            .collect()
    }

    fn family(&self) -> Option<Family> {
        let class = self.class();
        let deployment = matches!(
            class,
            "Awning"
                | "PositionableHorizontalAwning"
                | "UpDownHorizontalAwning"
                | "Pergola"
                | "PergolaHorizontalAwning"
        );
        let closure = matches!(
            class,
            "RollerShutter"
                | "PositionableRollerShutter"
                | "UpDownRollerShutter"
                | "AdjustableSlatsRollerShutter"
                | "Shutter"
                | "SwingingShutter"
                | "Screen"
                | "ExteriorScreen"
                | "PositionableScreen"
                | "UpDownScreen"
                | "ExteriorVenetianBlind"
                | "VenetianBlind"
                | "Curtain"
                | "Window"
        );
        if deployment {
            Some(Family::Deployment)
        } else if closure {
            Some(Family::Closure)
        } else {
            None
        }
    }
}

/// A device the driver does not handle, remembered to say so once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub class: Arc<str>,
    /// A class the driver knows but chose to leave out (none today).
    pub known: bool,
}

/// Everything a full load produced.
pub struct Load {
    pub devices: Vec<Device>,
    pub index: Index,
    pub skipped: Vec<Skipped>,
}

/// What the driver still needs to talk to a device it published.
#[derive(Clone, Debug)]
struct Entry {
    url: String,
    family: Family,
    /// `setClosure` / `setDeployment` is available.
    position: bool,
    /// The buttons this device declares, in display order.
    controls: Vec<&'static str>,
}

/// Device id → the gateway's own address for it.
#[derive(Clone, Debug, Default)]
pub struct Index {
    by_id: HashMap<DeviceId, Entry>,
    by_url: HashMap<String, DeviceId>,
}

/// One device's fresh values.
pub struct Update {
    pub id: DeviceId,
    pub values: Vec<(Arc<str>, Value)>,
    pub online: bool,
}

impl Load {
    #[must_use]
    pub fn new(instance: &InstanceId, raw: &[RawDevice]) -> Self {
        let mut devices = Vec::new();
        let mut index = Index::default();
        let mut skipped = Vec::new();
        for device in raw {
            let Some(family) = device.family() else {
                skipped.push(Skipped {
                    class: device.class().into(),
                    known: false,
                });
                continue;
            };
            if device.url.is_empty() || device.label.is_empty() {
                continue;
            }
            let id = DeviceId::new(instance, &device.url);
            let commands = device.commands();
            let position = match family {
                Family::Closure => commands.contains("setClosure"),
                Family::Deployment => commands.contains("setDeployment"),
            };
            let controls = controls(&commands);
            let points = points(&controls, position);
            if points.is_empty() {
                skipped.push(Skipped {
                    class: device.class().into(),
                    known: true,
                });
                continue;
            }
            devices.push(Device {
                id: id.clone(),
                instance: instance.clone(),
                native_name: device.label.as_str().into(),
                manufacturer: Some("Somfy".into()),
                model: Some(
                    if device.controllable_name.is_empty() {
                        device.class()
                    } else {
                        &device.controllable_name
                    }
                    .into(),
                ),
                description: None,
                native_room: None,
                members: Vec::new(),
                points,
            });
            index.by_url.insert(device.url.clone(), id.clone());
            index.by_id.insert(
                id,
                Entry {
                    url: device.url.clone(),
                    family,
                    position,
                    controls,
                },
            );
        }
        skipped.sort_unstable_by(|a, b| a.class.cmp(&b.class));
        skipped.dedup_by(|a, b| a.class == b.class);
        Self {
            devices,
            index,
            skipped,
        }
    }
}

/// The buttons a device declares, in the order Moli shows them.
fn controls(commands: &HashSet<&str>) -> Vec<&'static str> {
    ["open", "stop", "close"]
        .into_iter()
        .filter(|c| commands.contains(c))
        .collect()
}

/// What the house can see and do with one cover. A device with neither
/// buttons nor a position (an unpaired motor, say) is left out.
fn points(controls: &[&str], position: bool) -> Vec<PointSpec> {
    let mut points = Vec::new();
    if !controls.is_empty() {
        points.push(PointSpec {
            key: "control".into(),
            label: moli_i18n::tr!("pilotes.tahoma.commande").into(),
            kind: Kind::Enum {
                values: controls.iter().map(|c| Arc::from(*c)).collect(),
            },
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: Semantic::Other,
        });
    }
    if position {
        points.push(PointSpec {
            key: "percent_control".into(),
            label: moli_i18n::tr!("pilotes.tahoma.ouverture").into(),
            kind: Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: Some(1.0),
            },
            access: Access {
                read: true,
                write: true,
            },
            unit: Some(Unit::Percent),
            semantic: Semantic::Other,
        });
    }
    points
}

impl Index {
    /// Ids this index knows, for availability sweeps and removals.
    #[must_use]
    pub fn ids(&self) -> Vec<DeviceId> {
        self.by_id.keys().cloned().collect()
    }

    #[must_use]
    pub fn contains(&self, id: &DeviceId) -> bool {
        self.by_id.contains_key(id)
    }

    /// Two loads describe the same set of devices: only their states differ.
    #[must_use]
    pub fn same_ids(&self, other: &Self) -> bool {
        self.by_id.len() == other.by_id.len()
            && self.by_id.keys().all(|id| other.by_id.contains_key(id))
    }
    /// Fresh values for the devices this index knows.
    #[must_use]
    pub fn decode(&self, raw: &[RawDevice]) -> Vec<Update> {
        let mut updates = Vec::new();
        for device in raw {
            let Some(id) = self.by_url.get(&device.url) else {
                continue;
            };
            let Some(entry) = self.by_id.get(id) else {
                continue;
            };
            let mut values: Vec<(Arc<str>, Value)> = Vec::new();
            if let Some(percent) = position(device, entry.family) {
                values.push(("percent_control".into(), Value::Int(percent)));
            }
            if let Some(state) = device.state("core:OpenClosedState").and_then(Json::as_str) {
                let control = match state {
                    "open" => Some("open"),
                    "closed" => Some("close"),
                    _ => None,
                };
                if let Some(control) = control {
                    values.push(("control".into(), Value::Text(control.into())));
                }
            }
            updates.push(Update {
                id: id.clone(),
                values,
                online: device.available && device.enabled,
            });
        }
        updates
    }

    /// A Moli command → the gateway's device URL and action commands.
    #[must_use]
    pub fn encode(&self, id: &DeviceId, key: &str, value: &Value) -> Option<(String, Vec<Json>)> {
        let entry = self.by_id.get(id)?;
        match (key, value) {
            ("percent_control", v) if entry.position => {
                let percent = v.as_f64()?.clamp(0.0, 100.0).round();
                let raw = match entry.family {
                    Family::Closure => FULLY_CLOSED - percent,
                    Family::Deployment => percent,
                };
                // Clamped to 0..=100 above and rounded: the cast is exact.
                #[allow(clippy::cast_possible_truncation)]
                let raw = raw as i64;
                let name = match entry.family {
                    Family::Closure => "setClosure",
                    Family::Deployment => "setDeployment",
                };
                Some((
                    entry.url.clone(),
                    vec![Json::Object(
                        [
                            ("name".to_owned(), Json::String(name.to_owned())),
                            ("parameters".to_owned(), Json::Array(vec![Json::from(raw)])),
                        ]
                        .into_iter()
                        .collect(),
                    )],
                ))
            }
            ("control", Value::Text(command)) => {
                let command = command.as_ref();
                if !entry.controls.contains(&command) {
                    return None;
                }
                Some((
                    entry.url.clone(),
                    vec![Json::Object(
                        [("name".to_owned(), Json::String(command.to_owned()))]
                            .into_iter()
                            .collect(),
                    )],
                ))
            }
            _ => None,
        }
    }
}

/// The device's opening percentage, 0 = closed, 100 = open.
fn position(device: &RawDevice, family: Family) -> Option<i64> {
    let (name, invert) = match family {
        Family::Closure => ("core:ClosureState", true),
        Family::Deployment => ("core:DeploymentState", false),
    };
    let raw = device.state(name)?.as_i64()?;
    if NOT_A_POSITION.contains(&raw) || !(0..=100).contains(&raw) {
        return None;
    }
    Some(if invert { 100 - raw } else { raw })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance() -> InstanceId {
        InstanceId::from("tahoma")
    }

    fn load() -> Load {
        Load::new(&instance(), &fixture())
    }

    fn fixture() -> Vec<RawDevice> {
        serde_json::from_str(include_str!(
            "../../../integrations/tahoma/fixtures/devices.json"
        ))
        .expect("fixture")
    }

    fn named<'a>(load: &'a Load, name: &str) -> &'a Device {
        load.devices
            .iter()
            .find(|d| &*d.native_name == name)
            .unwrap_or_else(|| panic!("no device named {name}"))
    }

    fn point<'a>(device: &'a Device, key: &str) -> Option<&'a PointSpec> {
        device.points.iter().find(|p| &*p.key == key)
    }

    fn state(load: &Load, raw: &[RawDevice], device: &Device, key: &str) -> Option<Value> {
        load.index
            .decode(raw)
            .into_iter()
            .find(|u| u.id == device.id)
            .and_then(|u| {
                u.values
                    .into_iter()
                    .find(|(k, _)| &**k == key)
                    .map(|(_, v)| v)
            })
    }

    #[test]
    fn a_positionable_shutter_becomes_a_cover() {
        let load = load();
        let shutter = named(&load, "Volet salon");
        assert_eq!(
            point(shutter, "control").expect("control").kind,
            Kind::Enum {
                values: vec!["open".into(), "stop".into(), "close".into()],
            }
        );
        let position = point(shutter, "percent_control").expect("position");
        assert_eq!(position.unit, Some(Unit::Percent));
        assert!(position.access.write);
    }

    #[test]
    fn a_closed_shutter_reports_zero_percent_open() {
        let load = load();
        let shutter = named(&load, "Volet salon");
        assert_eq!(
            state(&load, &fixture(), shutter, "percent_control"),
            Some(Value::Int(0))
        );
    }

    #[test]
    fn an_open_shutter_reports_full_percent() {
        let load = load();
        let shutter = named(&load, "Volet atelier");
        assert_eq!(
            state(&load, &fixture(), shutter, "percent_control"),
            Some(Value::Int(100))
        );
    }

    #[test]
    fn a_shutter_without_position_keeps_its_buttons() {
        let load = load();
        let shutter = named(&load, "Volet chambre");
        assert!(point(shutter, "percent_control").is_none());
        assert!(point(shutter, "control").is_some());
    }

    #[test]
    fn an_awning_counts_deployment_as_opening() {
        let load = load();
        let awning = named(&load, "Store terrasse");
        assert_eq!(
            state(&load, &fixture(), awning, "percent_control"),
            Some(Value::Int(40))
        );
    }

    #[test]
    fn unknown_classes_are_reported_once_and_left_out() {
        let load = load();
        assert!(load.devices.iter().all(|d| &*d.native_name != "Lampe hall"));
        assert!(load.skipped.iter().any(|s| &*s.class == "Light"));
    }

    #[test]
    fn a_percentage_becomes_a_set_closure_from_the_closed_end() {
        let load = load();
        let shutter = named(&load, "Volet salon");
        let (url, commands) = load
            .index
            .encode(&shutter.id, "percent_control", &Value::Int(40))
            .expect("command");
        assert!(url.starts_with("io://"));
        assert_eq!(commands[0]["name"], "setClosure");
        assert_eq!(commands[0]["parameters"][0], 60);
    }

    #[test]
    fn opening_a_closure_sends_zero() {
        let load = load();
        let shutter = named(&load, "Volet salon");
        let (_, commands) = load
            .index
            .encode(&shutter.id, "percent_control", &Value::Int(100))
            .expect("command");
        assert_eq!(commands[0]["parameters"][0], 0);
    }

    #[test]
    fn an_awning_deploys_by_percentage() {
        let load = load();
        let awning = named(&load, "Store terrasse");
        let (_, commands) = load
            .index
            .encode(&awning.id, "percent_control", &Value::Int(40))
            .expect("command");
        assert_eq!(commands[0]["name"], "setDeployment");
        assert_eq!(commands[0]["parameters"][0], 40);
    }

    #[test]
    fn a_button_command_is_only_sent_when_the_device_has_it() {
        let load = load();
        let shutter = named(&load, "Volet chambre");
        assert!(
            load.index
                .encode(&shutter.id, "control", &Value::Text("stop".into()))
                .is_some()
        );
        assert!(
            load.index
                .encode(&shutter.id, "control", &Value::Text("half".into()))
                .is_none()
        );
    }

    #[test]
    fn positions_the_gateway_does_not_know_are_ignored() {
        let mut raw = fixture();
        for device in &mut raw {
            if device.label == "Volet salon" {
                for s in &mut device.states {
                    if s.name == "core:ClosureState" {
                        s.value = Json::from(124);
                    }
                }
            }
        }
        let load = Load::new(&instance(), &raw);
        let shutter = named(&load, "Volet salon");
        assert_eq!(state(&load, &raw, shutter, "percent_control"), None);
    }

    #[test]
    fn another_set_of_devices_asks_for_a_full_load() {
        let load = load();
        assert!(load.index.same_ids(&load.index));
        let mut fewer = fixture();
        fewer.retain(|d| d.label != "Volet atelier");
        let other = Load::new(&instance(), &fewer);
        assert!(!load.index.same_ids(&other.index));
    }
}
