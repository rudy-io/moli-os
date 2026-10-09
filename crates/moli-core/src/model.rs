use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{DeviceId, InstanceId, PointSpec, Value};

/// A device as described by its driver. Immutable once published: a change
/// is a new `Device` replacing the previous one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub instance: InstanceId,
    /// Name as known by the underlying system (may change; never an identity).
    pub native_name: Arc<str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<Arc<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Arc<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Arc<str>>,
    /// Room as known by the underlying system (Hue rooms…). A suggestion:
    /// the user's label always wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_room: Option<Arc<str>>,
    /// For groups (a Hue room…): the devices an order on this one reaches.
    /// The guard checks their rooms too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<DeviceId>,
    pub points: Vec<PointSpec>,
}

impl Device {
    #[must_use]
    pub fn point(&self, key: &str) -> Option<&PointSpec> {
        self.points.iter().find(|p| &*p.key == key)
    }
}

/// Human-facing metadata, owned by the user (or an agent acting for them).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
}

impl Label {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.room.is_none()
    }
}

/// A value at an instant (ms since epoch).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub value: Value,
    pub ts: u64,
}
