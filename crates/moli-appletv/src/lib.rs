//! The Apple TV, through the protocol Apple's Remote speaks (Companion), as
//! pyatv (MIT) describes it:
//!
//! - [`opack`]: Apple's binary encoding of the messages.
//! - [`tlv8`]: HomeKit's pairing messages.
//! - [`srp`]: SRP-6a, proving the code on the screen.
//! - [`hap`]: pair setup (once, with the code) and pair verify (each
//!   connection), the session's keys.
//! - [`companion`]: the link itself, encrypted after pair verify.
//! - [`driver`]: the `appletv` driver.

pub mod companion;
pub mod driver;
pub mod hap;
pub mod opack;
pub mod srp;
pub mod tlv8;

pub use driver::{AppleTv, Config};
