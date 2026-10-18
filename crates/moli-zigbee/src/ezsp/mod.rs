//! EZSP, the command protocol of a Silicon Labs coordinator (UG100), carried
//! as the payload of ASH data frames.
//!
//! Pure encoding and decoding: frames, frame IDs and the types the startup
//! sequence needs. The client that sends and awaits them sits on top.

mod frame;
pub mod ids;
mod types;

pub use frame::{EzspError, Header, Version, decode, encode, legacy_version_request};
pub use types::{
    EmberStatus, NetworkParameters, NetworkStatus, SL_STATUS_VERSION, SlStatus, read_ezsp_status,
    read_status,
};
