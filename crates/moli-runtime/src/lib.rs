//! Moli OS runtime.
//!
//! The [`Hub`] owns the live model of the home (devices, current values,
//! availability, labels), broadcasts every change as an [`Event`], routes
//! validated commands to the driver that owns the target device and keeps an
//! auditable journal of every write. Drivers run under a [`supervisor`] that
//! isolates their failures from the core.
//!
//! [`Event`]: moli_core::Event

mod driver;
pub mod guard;
mod hub;
mod journal_log;
mod labels;
pub mod media;
mod secrets;
mod state_cache;
mod stats;
pub mod supervisor;

pub use driver::{BoxFuture, CommandRequest, Driver, DriverCtx, MediaPublisher};
pub use hub::{
    ApprovalError, CORE_NAMESPACE, CommandError, DeviceView, DriverView, Hub, HubOptions,
    LabelError, LabelPatch, MAX_LABEL, Snapshot,
};
pub use secrets::{MasterKey, forget_secret, set_secret};
pub use stats::Stats;
pub use supervisor::spawn_driver;
