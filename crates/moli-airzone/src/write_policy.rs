//! Explicit migration boundary: reading new fields never enables new writes.
use crate::{Config, model::Entity};
use serde::Deserialize;
use serde_json::Value as Json;

// Moli point names and their historical Local API parameters. In particular,
// fan_speed is the existing Moli key for the manufacturer's speed parameter.
const LEGACY: [(&str, &str); 4] = [
    ("on", "on"),
    ("target_temperature", "setpoint"),
    ("mode", "mode"),
    ("fan_speed", "speed"),
];

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    #[default]
    Legacy,
    Extended,
}

impl WriteMode {
    pub(crate) fn allows_point(self, key: &str, historical_resource: bool) -> bool {
        self == Self::Extended
            || (historical_resource && LEGACY.iter().any(|(point, _)| *point == key))
    }

    /// Called after structural validation of an addressed operation. A legacy
    /// write has exactly two selectors and one historical parameter; even the
    /// reviewed setpoint + turn-on operation requires the extended opt-in.
    /// Historical shared controls addressed the master zone, never a whole
    /// system without zoneID. System metadata is an opt-in resource.
    pub(crate) fn allows_wire(self, path: &str, body: &Json) -> bool {
        self == Self::Extended
            || (path == "hvac"
                && body.get("zoneID").is_some()
                && body.as_object().is_some_and(|object| object.len() == 3)
                && LEGACY.iter().any(|(_, wire)| body.get(wire).is_some()))
    }
}

impl Config {
    pub(crate) fn restrict_writes(&self, entities: &mut [Entity], historical_resource: bool) {
        for (device, _) in entities {
            for point in &mut device.points {
                point.access.write &=
                    !self.read_only && self.write.allows_point(&point.key, historical_resource);
            }
        }
    }
}
