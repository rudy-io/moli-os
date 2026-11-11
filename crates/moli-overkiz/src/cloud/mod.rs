//! Optional independent Somfy cloud observation, with a closed provider protocol.
pub(crate) mod client;
mod protocol;
pub(crate) mod session;
mod storage;
mod types;
pub(crate) mod worker;
pub use session::{SessionBinding, SessionPhase, read_session_binding};
pub use types::*;

pub(crate) const MAX_RETRY_SECS: u64 = 3600;

pub const SESSION_KEY: &str = "cloud_session_v1";
pub const PENDING_KEY: &str = "cloud_pending_v1";
pub const STATE_KEY: &str = "cloud_state_v1";
pub const CLEANUP_KEY: &str = "private_state_purge_pending_v1";
pub const MAX_PENDING_BYTES: usize = 16_384;
pub const MAX_STATE_BYTES: usize = 131_072;
pub const MAX_SESSION_BYTES: usize = 196_608;

pub(super) fn bounded_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
#[cfg(test)]
mod tests;

/// Reserved native namespace for observation-only cloud publications.
pub const SUMMARY_NATIVE: &str = "cloud:summary";
#[must_use]
pub fn summary_device_id(instance: &moli_core::InstanceId) -> moli_core::DeviceId {
    moli_core::DeviceId::new(instance, SUMMARY_NATIVE)
}
pub(crate) fn owns_device(id: &moli_core::DeviceId) -> bool {
    id.as_str()
        .split_once(':')
        .is_some_and(|(_, native)| native.starts_with("cloud:"))
}
