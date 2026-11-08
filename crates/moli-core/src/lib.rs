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
pub use spec::{
    Access, Kind, MAX_TEXT, PRIVATE_POINT_KEYS, PointSpec, Semantic, Unit, ValueError,
    is_private_point, validate,
};
pub use value::Value;

/// Milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Whether two byte slices are equal, compared in constant time so that a
/// mismatch cannot reveal how many leading bytes matched. Lengths must be
/// fixed by the format (hex digests, tokens) — a length difference is
/// itself the answer and is returned without scanning.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len = a.len();
    if len != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_agrees_with_normal_equality() {
        assert!(constant_time_eq(b"ab12", b"ab12"));
        assert!(!constant_time_eq(b"ab12", b"ab13"));
        assert!(!constant_time_eq(b"ab12", b"ab1"));
    }
}
