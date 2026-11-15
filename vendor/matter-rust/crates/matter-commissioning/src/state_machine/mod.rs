//! Commissioning state machine.
//!
//! Cursor + switch-on-enum design modeled on
//! `project-chip/connectedhomeip`'s `AutoCommissioner`. Transport-agnostic
//! and sans-IO — emits cluster-command-level [`Action`]s that a driver wraps
//! in Invoke envelopes and routes via `matter-transport`. The crate's own
//! async driver (the `driver` feature) is one such caller; you can supply
//! your own instead.

#![forbid(unsafe_code)]

mod action;
mod commissioner;
mod error;
mod stage;

pub use action::{Action, CommissionedFabric, Expectation, SessionContext};
#[cfg(feature = "__test_shortcuts")]
pub use commissioner::TestStateSeeds;
pub use commissioner::{Commissioner, CommissionerConfig, NetworkCredentials, WiFiCredentials};
// Re-exported so the driver (crate::driver::commission) can floor the
// BLE-path ConnectNetwork response deadline (when the device reported
// ConnectMaxTimeSeconds) at the same value the state-machine failsafe
// extension uses, so that deadline never fires before the extension would
// expire. `commissioner` itself stays a private module. The driver is the
// only consumer of the re-export, hence the feature gate — the constant
// itself stays ungated for `Commissioner::network_enable_failsafe_seconds`.
#[cfg(feature = "driver")]
pub(crate) use commissioner::DEFAULT_CONNECT_MAX_TIME_SECONDS;
pub use error::{CommissioningError, NetworkKind, RemediationHint};
// Re-exported for crate-internal use; only `stage`'s tests call it today.
#[allow(unused_imports)]
pub(crate) use stage::next_stage;
pub use stage::Stage;
