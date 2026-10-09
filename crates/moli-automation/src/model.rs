//! What an automation is: a graph of steps joined by edges, like n8n.

use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Automation {
    /// Given by Moli on creation (from the name): may be left out.
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// Off: never runs. On: runs if a human approved this very version.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: Mode,
    pub graph: Graph,
    #[serde(default)]
    pub author: Author,
    /// Fingerprint of the version a human approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved: Option<String>,
    /// That version itself: what « back to the approved version » restores
    /// after someone else changed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_version: Option<Version>,
    /// Why it stopped by itself, what an import could not translate…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub updated: u64,
}

impl Automation {
    /// Approved as it is now.
    pub fn is_approved(&self) -> bool {
        self.approved.as_deref() == Some(&*self.fingerprint())
    }

    /// Will run when triggered.
    pub fn is_live(&self) -> bool {
        self.enabled && self.is_approved()
    }

    /// What a human approves: the graph and the mode (not the name, not the
    /// layout on the canvas).
    pub fn fingerprint(&self) -> String {
        let nodes: Vec<Json> = self
            .graph
            .nodes
            .iter()
            .map(|n| serde_json::json!({ "id": n.id, "step": n.step }))
            .collect();
        let canonical = serde_json::json!({
            "mode": self.mode,
            "nodes": nodes,
            "edges": self.graph.edges,
        });
        let hash = digest(&SHA256, canonical.to_string().as_bytes());
        // All 256 bits: an agent writes both graphs, a short hash could be
        // made to collide.
        hash.as_ref().iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
    }
}

/// A version a person approved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub mode: Mode,
    pub graph: Graph,
}

/// What happens when it triggers while a run is still going.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The new run replaces the old one (« motion → light for 5 min »).
    #[default]
    Restart,
    /// The new trigger is ignored.
    Single,
    /// The new run waits for the old one.
    Queued,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    #[default]
    Human,
    /// Moli, the assistant.
    Assistant,
    /// An agent through MCP or the API.
    Agent,
    /// Imported from Home Assistant.
    Import,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl Graph {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Where a port leads, in the order of the edges: the order is part of
    /// what a person approves (the canvas layout is not).
    pub fn next(&self, from: &str, port: &str) -> Vec<&Node> {
        self.edges
            .iter()
            .filter(|e| e.from == from && e.port == port)
            .filter_map(|e| self.node(&e.to))
            .collect()
    }

    pub fn triggers(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(|n| n.step.is_trigger())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(flatten)]
    pub step: Step,
    /// Position on the canvas.
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    #[serde(default = "out")]
    pub port: String,
    pub to: String,
}

fn out() -> String {
    "out".into()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    // ---- triggers ----
    /// A point changes (to a value, from a value), maybe held for a while.
    WhenState {
        point: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<Json>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Json>,
        #[serde(default)]
        for_s: u64,
    },
    /// A number goes above or below a threshold (on the crossing).
    WhenThreshold {
        point: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        above: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        below: Option<f64>,
        #[serde(default)]
        for_s: u64,
    },
    /// Every day (or some days) at a time.
    AtTime {
        at: String,
        #[serde(default)]
        days: Vec<u8>,
    },
    /// At sunrise or sunset, give or take some minutes.
    AtSun {
        event: SunEvent,
        #[serde(default)]
        offset_min: i32,
        #[serde(default)]
        days: Vec<u8>,
    },
    /// Every N minutes (aligned on the hour).
    Every {
        minutes: u32,
    },
    /// When Moli OS starts.
    OnStart,
    /// Only when someone runs it.
    Manual,

    // ---- logic ----
    /// Yes or no: all rules (`all`) or any of them.
    If {
        rules: Vec<Rule>,
        #[serde(default = "yes")]
        all: bool,
    },

    // ---- actions ----
    Set {
        point: String,
        value: Json,
    },
    Toggle {
        point: String,
    },
    Wait {
        seconds: u64,
    },
    /// Until a rule holds (`ok`) or the time runs out (`timeout`).
    WaitFor {
        rule: Rule,
        timeout_s: u64,
    },
    Notify {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        message: String,
        #[serde(default = "home_channel")]
        channels: Vec<Channel>,
    },
    /// Moli writes a text (`{{texte}}` afterwards). Words only: never a
    /// decision.
    Write {
        prompt: String,
    },
}

fn yes() -> bool {
    true
}

fn home_channel() -> Vec<Channel> {
    vec![Channel::Maison]
}

impl Step {
    pub fn is_trigger(&self) -> bool {
        matches!(
            self,
            Self::WhenState { .. }
                | Self::WhenThreshold { .. }
                | Self::AtTime { .. }
                | Self::AtSun { .. }
                | Self::Every { .. }
                | Self::OnStart
                | Self::Manual
        )
    }

    /// The ports a step leads on.
    pub fn ports(&self) -> &'static [&'static str] {
        match self {
            Self::If { .. } => &["yes", "no"],
            Self::WaitFor { .. } => &["ok", "timeout"],
            _ => &["out"],
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::WhenState { .. } => "when_state",
            Self::WhenThreshold { .. } => "when_threshold",
            Self::AtTime { .. } => "at_time",
            Self::AtSun { .. } => "at_sun",
            Self::Every { .. } => "every",
            Self::OnStart => "on_start",
            Self::Manual => "manual",
            Self::If { .. } => "if",
            Self::Set { .. } => "set",
            Self::Toggle { .. } => "toggle",
            Self::Wait { .. } => "wait",
            Self::WaitFor { .. } => "wait_for",
            Self::Notify { .. } => "notify",
            Self::Write { .. } => "write",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SunEvent {
    Rise,
    Set,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// The dashboards at home.
    Maison,
    /// Telegram (the `telegram` driver's bot).
    Telegram,
    /// Said aloud by every speaker that can announce (Sonos).
    Voix,
    /// A notification on the household's phones (the Moli app).
    Telephone,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rule {
    /// A point compared to a value.
    State {
        point: String,
        #[serde(default)]
        op: Op,
        value: Json,
    },
    /// Between two times (overnight works: 22:00 → 06:00), some days.
    Time {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<String>,
        #[serde(default)]
        days: Vec<u8>,
    },
    /// Day (sun up) or night.
    Sun { is: DayNight },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    #[default]
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DayNight {
    Day,
    Night,
}

/// « HH:MM » → minutes since midnight.
pub fn minutes_of(at: &str) -> Option<u32> {
    let (h, m) = at.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Automation {
        serde_json::from_value(serde_json::json!({
            "id": "a", "name": "Entrée", "enabled": true,
            "graph": {
                "nodes": [
                    { "id": "t", "type": "when_state", "point": "z:1/contact", "to": false, "x": 0, "y": 0 },
                    { "id": "c", "type": "if", "rules": [{ "kind": "sun", "is": "night" }] },
                    { "id": "s", "type": "set", "point": "hue:2/on", "value": true }
                ],
                "edges": [{ "from": "t", "to": "c" }, { "from": "c", "port": "yes", "to": "s" }]
            }
        }))
        .unwrap()
    }

    #[test]
    fn reads_the_graph_format() {
        let a = sample();
        assert_eq!(a.mode, Mode::Restart);
        assert_eq!(a.graph.triggers().count(), 1);
        assert_eq!(a.graph.next("c", "yes")[0].id, "s");
        assert!(a.graph.next("c", "no").is_empty());
        assert_eq!(a.graph.edges[0].port, "out");
    }

    #[test]
    fn approval_follows_the_logic_not_the_layout() {
        let mut a = sample();
        assert!(!a.is_live());
        a.approved = Some(a.fingerprint());
        assert!(a.is_live());
        a.graph.nodes[0].x = 400.0;
        a.name = "Autre nom".into();
        assert!(
            a.is_approved(),
            "moving a node or renaming keeps the approval"
        );
        a.graph.nodes[2].step = Step::Set {
            point: "hue:2/on".into(),
            value: Json::Bool(false),
        };
        assert!(
            !a.is_approved(),
            "changing what it does needs a new approval"
        );
    }

    #[test]
    fn times_parse() {
        assert_eq!(minutes_of("07:30"), Some(450));
        assert_eq!(minutes_of("24:00"), None);
        assert_eq!(minutes_of("7h"), None);
    }
}
