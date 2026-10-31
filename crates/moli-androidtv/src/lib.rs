//! Android and Google TVs speak two open protocols beside their maker's
//! API, both reused from what Home Assistant already paired:
//!
//! - [`remote`]: Android TV Remote v2 (TLS 6466, the client certificate a
//!   pairing made): the app on screen, launching an app, keys.
//! - [`cast`]: Google Cast (TLS 8009): what an app plays (title, picture,
//!   playing or paused) and play / pause.
//!
//! Each runs one connection until it fails; the driver reconnects.
//!
//! - [`tv`]: one TV through both, kept connected: what any Android TV
//!   driver needs (a maker's, like Philips, runs it beside its own API).

pub mod cast;
pub mod remote;
pub mod tv;

/// The protobuf codec, shared with the other protocols.
pub use moli_net::proto;
