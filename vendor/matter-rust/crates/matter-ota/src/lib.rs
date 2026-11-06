//! Matter OTA Software Update **Provider** — the controller-appropriate half of
//! OTA (Matter Core Spec §11.20, cluster `OtaSoftwareUpdateProvider` 0x0029).
//!
//! A Matter controller/admin is the actor that pushes firmware to the devices it
//! manages, so this crate implements the **Provider** role (it answers a device's
//! `QueryImage`, authorises the `ApplyUpdateRequest`, and acknowledges
//! `NotifyUpdateApplied`) — not the device-side Requestor (0x002A).
//!
//! ## Scope
//!
//! This crate holds the **pure command-handler logic**: functions that take a
//! decoded request's command-fields TLV and produce the response command-fields
//! TLV. There is deliberately **no networking** here — no socket, no CASE
//! session, no BDX transfer — which keeps the protocol testable without one.
//!
//! The surrounding pieces live elsewhere and are complete:
//!
//! * the image bytes travel over BDX — see the `matter-bdx` crate;
//! * the server that owns the socket, advertises the operational service over
//!   mDNS, accepts CASE, and routes IM vs BDX by protocol ID lives in
//!   `matter-controller` (`serve_ota` / `serve_provider_once`).
//!
//! That server wires these handlers as: `parse_invoke_request` → handler →
//! `build_invoke_response_command`/`_status` (see
//! [`matter_interaction::invoke_server`]).
//!
//! The server direction of the 0x0029 codec (decode the inbound `QueryImage`
//! request, encode the outbound `QueryImageResponse`) is **hand-rolled here** over
//! [`matter_codec`]: the `matter-clusters` emitter only generated the *client*
//! direction (encode the request, decode the response), which this crate's tests
//! reuse as oracles.

#![forbid(unsafe_code)]

pub mod provider;

pub use provider::{
    handle_apply_update_request, handle_query_image, parse_notify_update_applied, ImageOffer,
    OtaError,
};

/// Compile-checks the Rust examples in this crate's `README.md`.
///
/// `#[cfg(doctest)]` means the item exists only while rustdoc is collecting
/// doctests, so the README is compiled by `cargo test --doc` without being
/// duplicated into the rendered crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
