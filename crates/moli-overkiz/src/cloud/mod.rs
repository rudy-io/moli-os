//! Optional independent Somfy cloud observation, with a closed provider protocol.
#[allow(dead_code, reason = "Actor wiring is the next isolated task.")]
pub(crate) mod client;
#[allow(dead_code, reason = "Actor wiring is the next isolated task.")]
mod protocol;
#[allow(dead_code, reason = "Actor wiring is the next isolated task.")]
pub(crate) mod session;
mod types;
pub use session::{SessionBinding, SessionPhase, read_session_binding};
pub use types::*;

pub const SESSION_KEY: &str = "cloud_session_v1";
pub const PENDING_KEY: &str = "cloud_pending_v1";
pub const STATE_KEY: &str = "cloud_state_v1";
pub const MAX_PENDING_BYTES: usize = 16_384;
pub const MAX_STATE_BYTES: usize = 131_072;
pub const MAX_SESSION_BYTES: usize = 196_608;

pub(super) fn bounded_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
#[cfg(test)]
mod tests;
