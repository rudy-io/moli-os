use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{Device, DeviceId, InstanceId, Label, PointId, Value};

/// Everything that happens in the system, as broadcast on the bus and
/// streamed to the surfaces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    DeviceUpserted {
        device: Arc<Device>,
    },
    DeviceRemoved {
        device: DeviceId,
    },
    State {
        point: PointId,
        value: Value,
        ts: u64,
    },
    Availability {
        device: DeviceId,
        online: bool,
    },
    DriverStatus {
        instance: InstanceId,
        status: DriverStatus,
    },
    LabelChanged {
        device: DeviceId,
        label: Label,
    },
    Journal {
        entry: Arc<JournalEntry>,
    },
    /// An order held by the guard, waiting for a human.
    ApprovalRequested {
        request: Arc<ApprovalRequest>,
    },
    ApprovalResolved {
        id: u64,
        decision: Decision,
    },
    /// A message for the people at home (an automation's notification).
    Notice {
        notice: Notice,
    },
}

/// A message shown in the dashboard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub message: String,
    /// Who says it (an automation's name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub ts: u64,
}

/// An order an agent (or script) gave that only a human may release.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: u64,
    pub ts: u64,
    pub expires: u64,
    pub point: PointId,
    pub value: Value,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Why the guard held it (« pièce protégée (Petite chambre) »…).
    pub reason: String,
    /// Device name and rooms *when the order was held*: what the human
    /// approves cannot be disguised by renaming the device afterwards.
    pub device_name: String,
    /// The name the source system gives it (Hue, Zigbee…), which agents
    /// cannot change: shown beside the label on approval cards.
    pub native_name: String,
    pub rooms: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approved,
    Denied,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DriverStatus {
    Starting,
    Running,
    Backoff {
        error: String,
        retry_in_ms: u64,
    },
    /// Blocked on something only a human can do (press a pairing button…).
    Waiting {
        reason: String,
    },
    Stopped,
}

/// Which surface an action came through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Ui,
    Api,
    Mcp,
    Cli,
    System,
    /// Moli's own assistant (the dashboard's conversation): an agent like
    /// any other, under the same guard.
    Assistant,
    /// An automation a human approved (that exact version): their intent,
    /// carried out by the house.
    Automation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Command {
        point: PointId,
        value: Value,
    },
    Label {
        device: DeviceId,
        label: Label,
    },
    /// A human's decision on a held order (and, when approved, its execution).
    Approval {
        request: u64,
        point: PointId,
        value: Value,
        decision: Decision,
    },
    /// A change to an automation: who did what, to which version.
    Automation {
        id: String,
        name: String,
        change: AutomationChange,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fingerprint: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationChange {
    Created,
    Updated,
    /// Approved by a person, and so switched on.
    Approved,
    SwitchedOff,
    /// Back to the version a person approved.
    Restored,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "error", rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Err(String),
    /// Held by the guard; the text says why and which request to approve.
    Pending(String),
}

/// One auditable write: who, through what, did what, with which result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: u64,
    pub ts: u64,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    pub action: Action,
    pub outcome: Outcome,
    pub duration_ms: u64,
}
