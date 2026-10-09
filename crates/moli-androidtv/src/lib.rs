//! Android and Google TVs speak two open protocols beside their maker's
//! API, both reused from what Home Assistant already paired:
//!
//! - [`remote`]: Android TV Remote v2 (TLS 6466, the client certificate a
//!   pairing made): the app on screen, launching an app, keys.
//! - [`cast`]: Google Cast (TLS 8009): what an app plays (title, picture,
//!   playing or paused) and play / pause.
//!
//! Each runs one connection until it fails; the driver reconnects.

pub mod cast;
pub mod proto;
pub mod remote;
