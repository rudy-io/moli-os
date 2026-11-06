//! Android and Google TVs speak open protocols beside their maker's
//! API (the first two reuse what Home Assistant already paired):
//!
//! - [`remote`]: Android TV Remote v2 (TLS 6466, the client certificate a
//!   pairing made): the app on screen, launching an app, keys.
//! - [`cast`]: Google Cast (TLS 8009): what an app plays (title, picture,
//!   playing or paused) and play / pause.
//! - [`adb`]: ADB over the network (TCP 5555, the TV's developer options):
//!   what any app plays, from the TV's media sessions.
//! - [`overlay`]: TvOverlay, an app that draws notifications (a picture, a
//!   live video) over whatever plays.
//!
//! Each runs one connection until it fails; the driver reconnects.

pub mod adb;
pub mod cast;
pub mod overlay;
pub mod remote;

/// The protobuf codec, shared with the other protocols.
pub use moli_net::proto;
