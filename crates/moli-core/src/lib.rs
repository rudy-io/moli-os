//! Moli OS domain model.
//!
//! Pure types, no IO, no async runtime. Everything the rest of the system
//! exchanges — identities, typed values, point specifications, events and
//! journal entries — lives here so drivers, the runtime and the surfaces
//! agree on one vocabulary.

pub mod color;
mod event;
mod id;
mod model;
mod spec;
mod value;

pub use event::{
    Action, ApprovalRequest, AutomationChange, Decision, DriverStatus, Event, JournalEntry, Notice,
    Origin, Outcome,
};
pub use id::{DeviceId, InstanceId, PointId};
pub use model::{Device, Label, Sample};
pub use spec::{Access, Kind, MAX_TEXT, PointSpec, Semantic, Unit, ValueError, validate};
pub use value::Value;

/// Milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
