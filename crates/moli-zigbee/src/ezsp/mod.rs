//! EZSP, the command protocol of a Silicon Labs coordinator (UG100), carried
//! as the payload of ASH data frames.
//!
//! Encoding and decoding (frames, frame IDs and the types the startup
//! sequence needs), and the client that sends commands and awaits them.

mod client;
mod frame;
pub mod ids;
mod types;

pub use client::{Callback, Ezsp, EzspFailure, MAX_PROTOCOL, MIN_PROTOCOL};
pub use frame::{EzspError, Header, Version, decode, encode, legacy_version_request};
pub use types::{
    EmberStatus, NetworkParameters, NetworkStatus, SL_STATUS_VERSION, SlStatus, read_ezsp_status,
    read_status,
};
