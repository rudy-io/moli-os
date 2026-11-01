//! Bounded pure device snapshots; display names never grant actuation.
mod binding;
use anyhow::{Context as _, bail};
use binding::{Binding, Family};
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::{Value as Json, json};
use std::collections::HashSet;
use std::fmt::Write as _;

const COVER_CLASSES: [&str; 18] = [
    "AdjustableSlatsRollerShutter",
    "Awning",
    "Curtain",
    "ExteriorScreen",
    "ExteriorVenetianBlind",
    "Pergola",
    "PergolaHorizontalAwning",
    "PositionableHorizontalAwning",
    "PositionableRollerShutter",
    "PositionableScreen",
    "RollerShutter",
    "Screen",
    "Shutter",
    "SwingingShutter",
    "UpDownHorizontalAwning",
    "UpDownRollerShutter",
    "UpDownScreen",
    "VenetianBlind",
];
const CONTROLS: [&str; 5] = ["open", "close", "stop", "cycle", "favourite"];
const MEMORIZED: &str = "core:Memorized1PositionState";

#[derive(Debug)]
pub(crate) struct Entry {
    raw: Json,
    pub url: String,
    pub online: bool,
    reviewed_class: Option<String>,
    read_only: bool,
}

#[cfg(test)]
pub(crate) fn parse(json: &Json) -> anyhow::Result<Vec<Entry>> {
    parse_policy(json, &[], false)
}
#[cfg(test)]
pub(crate) fn parse_reviewed(
    json: &Json,
    reviewed: &[crate::ReviewedOpener],
) -> anyhow::Result<Vec<Entry>> {
    parse_policy(json, reviewed, false)
}
pub(crate) fn parse_policy(
    json: &Json,
    reviewed: &[crate::ReviewedOpener],
    read_only: bool,
) -> anyhow::Result<Vec<Entry>> {
    let items = json
        .as_array()
        .context(moli_i18n::tr!("overkiz.response"))?;
    if items.len() > 2048 {
        bail!(moli_i18n::tr!("overkiz.response"));
    }
    let mut seen = HashSet::new();
    let mut identities = HashSet::new();
    items
        .iter()
        .map(|raw| {
            let url = raw["deviceURL"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
                .context(moli_i18n::tr!("overkiz.response"))?;
            if !seen.insert(url) {
                bail!(moli_i18n::tr!("overkiz.response"));
            }
            let entry = Entry {
                raw: raw.clone(),
                url: url.to_owned(),
                online: raw["available"] == true && raw["enabled"] == true,
                reviewed_class: reviewed
                    .iter()
                    .find(|p| p.device_url == url)
                    .map(|p| p.ui_class.clone()),
                read_only,
            };
            if !identities.insert(entry.native()) {
                bail!(moli_i18n::tr!("overkiz.response"));
            }
            Ok(entry)
        })
        .collect()
}
impl Entry {
    pub fn native(&self) -> String {
        let hash = ring::digest::digest(&ring::digest::SHA256, self.url.as_bytes());
        let mut id = String::with_capacity(64);
        for b in hash.as_ref() {
            let _ = write!(id, "{b:02x}");
        }
        id
    }
    fn io(&self) -> bool {
        self.url.starts_with("io://")
    }
    // Missing classes may use the other location; contradictory or malformed classes cannot authorize writes.
    fn class(&self) -> Option<&str> {
        let root = self.raw.get("uiClass");
        let nested = self.raw["definition"].get("uiClass");
        match (root, nested) {
            (Some(Json::String(a)), Some(Json::String(b))) if a == b => Some(a),
            (None, Some(Json::String(class))) | (Some(Json::String(class)), None) => Some(class),
            _ => None,
        }
        .filter(|class| !class.is_empty())
        .map(String::as_str)
    }
    fn known_class(&self) -> Option<&str> {
        self.class()
            .filter(|class| COVER_CLASSES.contains(class) || matches!(*class, "Gate" | "Window"))
    }
    fn safe(&self) -> bool {
        if !(self.io() || self.url.starts_with("rts://")) {
            return false;
        }
        match (self.reviewed_class.as_deref(), self.class()) {
            (Some(reviewed @ ("Gate" | "Window")), Some(class)) => reviewed == class,
            (None, Some(class)) => COVER_CLASSES.contains(&class),
            _ => false,
        }
    }
    fn arity(&self, name: &str) -> Option<u64> {
        let commands = self.raw["definition"]["commands"].as_array()?;
        let mut matching = commands.iter().filter(|c| c["commandName"] == name);
        let command = matching.next()?;
        if matching.next().is_some() {
            return None;
        }
        command["nparams"].as_u64()
    }
    fn state(&self, name: &str) -> Option<&Json> {
        let mut states = self.raw["states"]
            .as_array()?
            .iter()
            .filter(|s| s["name"] == name);
        let value = &states.next()?["value"];
        if states.next().is_some() {
            None
        } else {
            Some(value)
        }
    }
    fn has_state(&self, name: &str) -> bool {
        self.raw["states"]
            .as_array()
            .is_some_and(|states| states.iter().any(|state| state["name"] == name))
    }
    fn family(&self) -> Option<Family> {
        // State names identify the reported axis even when its value is unknown.
        // Preserve Deployment priority when both axes are explicitly reported (#25).
        if self.has_state("core:DeploymentState") {
            return Some(Family::Deployment);
        }
        if self.has_state("core:ClosureState") {
            return Some(Family::Closure);
        }
        match (
            self.arity("setClosure") == Some(1),
            self.arity("setDeployment") == Some(1),
        ) {
            (true, false) => Some(Family::Closure),
            (false, true) => Some(Family::Deployment),
            _ => None,
        }
    }
    fn reading(&self, name: &str) -> Value {
        self.family()
            .and_then(|family| {
                self.state(name)
                    .and_then(Json::as_f64)
                    .and_then(|v| family.convert(v))
            })
            .map_or(Value::Null, Value::Float)
    }
    pub fn position(&self) -> Value {
        if !self.io() {
            return Value::Null;
        }
        self.family()
            .map_or(Value::Null, |family| self.reading(family.state()))
    }
    pub fn favourite(&self) -> Value {
        // Memorized configuration is meaningful on RTS too; it is never measured motor feedback.
        self.reading(MEMORIZED)
    }
    pub fn opening(&self) -> Value {
        self.state("core:OpenClosedState")
            .and_then(Json::as_str)
            .filter(|v| matches!(*v, "open" | "closed"))
            .map_or(Value::Null, Value::from)
    }
    // This is the only native command selection path, shared by publication and encoding.
    fn binding(&self, key: &str, control: &str) -> Option<Binding> {
        if !self.safe() {
            return None;
        }
        match key {
            "percent_control" if self.io() => {
                let family = self.family()?;
                (self.arity(family.setter()) == Some(1))
                    .then(|| Binding::percent(family.setter(), family))
            }
            "favourite_position" => {
                let family = self.family()?;
                (self.arity("setMemorized1Position") == Some(1))
                    .then(|| Binding::percent("setMemorized1Position", family))
            }
            "control" => {
                let names: &[&'static str] = match control {
                    "open" if self.reviewed_class.is_some() => &["open"],
                    "close" if self.reviewed_class.is_some() => &["close"],
                    "open" => &["open", "up"],
                    "close" => &["close", "down"],
                    "stop" => &["stop"],
                    "cycle" if self.reviewed_class.is_some() => &["cycle"],
                    "favourite" => &["goToMemorized1Position", "my"],
                    _ => &[],
                };
                names.iter().find_map(|name| match self.arity(name) {
                    Some(0) => Some(Binding::empty(name)),
                    // PyOverkiz test_rts_injection.py at 5f318585 explicitly qualifies RTS my(0-duration).
                    Some(1)
                        if control == "favourite"
                            && *name == "my"
                            && self.url.starts_with("rts://") =>
                    {
                        Some(Binding::rts(name))
                    }
                    Some(1)
                        if matches!(control, "open" | "close" | "stop")
                            && self.url.starts_with("rts://") =>
                    {
                        Some(Binding::rts(name))
                    }
                    _ => None,
                })
            }
            _ => None,
        }
    }
    pub fn apply(&self, key: &str, value: &Value) -> anyhow::Result<Json> {
        if self.read_only || !self.online {
            bail!(moli_i18n::tr!("overkiz.refused"));
        }
        let control = if key == "control" {
            let Value::Text(control) = value else {
                bail!(moli_i18n::tr!("overkiz.refused"));
            };
            control.as_ref()
        } else {
            ""
        };
        let command = self
            .binding(key, control)
            .and_then(|binding| binding.encode(value))
            .context(moli_i18n::tr!("overkiz.refused"))?;
        Ok(json!({"label":"Moli","actions":[{"deviceURL":self.url,"commands":[command]}]}))
    }
    fn write_semantic(&self) -> Semantic {
        if self.reviewed_class.is_some() {
            Semantic::Control
        } else {
            Semantic::Other
        }
    }
    fn numeric_point(&self, key: &str, label: String) -> PointSpec {
        let supported = self.binding(key, "").is_some();
        PointSpec {
            key: key.into(),
            label: label.into(),
            kind: Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: Some(1.0),
            },
            access: Access {
                read: true,
                write: supported && !self.read_only,
            },
            unit: Some(Unit::Percent),
            semantic: if supported {
                self.write_semantic()
            } else {
                Semantic::Other
            },
        }
    }
    pub fn device(&self, instance: &InstanceId) -> Device {
        let mut points = vec![];
        if self.io()
            && self.family().is_some_and(|family| {
                self.has_state(family.state()) || self.binding("percent_control", "").is_some()
            })
        {
            points.push(self.numeric_point("percent_control", moli_i18n::tr!("overkiz.position")));
        }
        if self.has_state(MEMORIZED) || self.binding("favourite_position", "").is_some() {
            let mut point = self.numeric_point(
                "favourite_position",
                moli_i18n::tr!("overkiz.favourite_position"),
            );
            point.access.read = self.io() || self.has_state(MEMORIZED);
            points.push(point);
        }
        if self.known_class().is_some() || self.has_state("core:OpenClosedState") {
            points.push(PointSpec {
                key: "opening_state".into(),
                label: moli_i18n::tr!("overkiz.state").into(),
                kind: Kind::Enum {
                    values: vec!["open".into(), "closed".into()],
                },
                access: Access {
                    read: true,
                    write: false,
                },
                unit: None,
                semantic: Semantic::Other,
            });
        }
        let movements: Vec<_> = CONTROLS
            .into_iter()
            .filter(|k| self.binding("control", k).is_some())
            .map(Into::into)
            .collect();
        if !movements.is_empty() {
            points.push(PointSpec {
                key: "control".into(),
                label: moli_i18n::tr!("overkiz.command").into(),
                kind: Kind::Enum { values: movements },
                access: Access {
                    read: false,
                    write: !self.read_only,
                },
                unit: None,
                semantic: self.write_semantic(),
            });
        }
        Device {
            id: DeviceId::new(instance, &self.native()),
            instance: instance.clone(),
            native_name: self.raw["label"]
                .as_str()
                .filter(|s| s.len() <= 256)
                .unwrap_or("Overkiz")
                .into(),
            manufacturer: Some("Somfy / Overkiz".into()),
            model: self
                .known_class()
                .map(|class| format!("Overkiz {class}").into()),
            description: None,
            native_room: None,
            members: vec![],
            points,
        }
    }
}
#[cfg(test)]
#[path = "model_convergence_tests.rs"]
mod convergence_tests;
#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
