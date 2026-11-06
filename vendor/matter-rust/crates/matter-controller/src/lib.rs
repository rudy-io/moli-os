//! The high-level Matter controller API — the single crate a consumer depends
//! on to commission and control Matter devices from pure Rust.
//!
//! [`MatterController`] is the entry point. It persists a fabric and a stable
//! commissioner identity through a pluggable [`ControllerStore`] (a default
//! [`FileStore`] ships), commissions devices over IP, and exposes each device
//! through a cheap [`Node`] handle that transparently establishes, caches, and
//! reuses the operational CASE session.
//!
//! # Capabilities
//!
//! - **Fabric & identity** — [`MatterController::create_fabric`] mints and
//!   persists the controller's stable operational identity once per fabric.
//! - **Commissioning** — [`MatterController::commission`] brings a device onto
//!   the fabric from a QR (`MT:…`) or manual pairing code, verifying device
//!   attestation against the configured [`AttestationTrust`].
//! - **Interaction** — [`Node::read`] / [`Node::write`] / [`Node::invoke`] work
//!   over raw [`Value`]s and support **wildcard reads** ([`ReadPath::cluster`],
//!   [`ReadPath::all`]) for reading every attribute off a device.
//! - **Subscriptions** — [`Node::subscribe`] returns a [`Subscription`] stream
//!   of [`SubscriptionEvent`]s (`Report` / `Established` / `Resubscribing` /
//!   `Lagged`; `next().await` + `cancel()`).
//! - **Multi-admin / commissioning windows** — [`Node::open_commissioning_window`]
//!   opens an enhanced commissioning window (generates secrets, computes the PAKE
//!   verifier, returns a [`CommissioningWindow`] with `manual_code`/`qr_code`);
//!   [`Node::open_basic_commissioning_window`] opens a basic window; and
//!   [`Node::revoke_commissioning`] closes any open window.
//!   [`Node::commissioning_window_status`] reads the current window state.
//! - **Fabric management** — [`Node::list_fabrics`] reads the device's full
//!   fabric table as [`Vec<FabricDescriptor>`]; [`Node::remove_fabric`] removes a
//!   fabric by index (self-protected: returns [`Error::WouldRemoveSelf`] for our
//!   own fabric); [`Node::update_fabric_label`] relabels the accessing fabric.
//! - **ACL management** — [`Node::read_acl`] returns the device's
//!   `AccessControl.Acl` list as [`Vec<AclEntry>`]; [`Node::write_acl`] replaces it
//!   atomically (single-chunk) or via a multi-chunk `MoreChunkedMessages` sequence
//!   (large lists), with a lockout guard that returns [`Error::AclWouldLockOut`]
//!   before sending any bytes if the new list would drop our own Administer/CASE
//!   access.
//! - **Group provisioning** — [`Node::write_group_key_set`] provisions a key set
//!   on the device (`KeySetWrite`, `GroupKeyManagement` cluster 0x003F);
//!   [`Node::write_group_key_map`] writes the `GroupKeyMap` attribute via the
//!   chunked list-write mechanism; [`Node::add_group`] / [`Node::remove_group`]
//!   add and remove an endpoint from a group (`Groups` cluster 0x0004). Public
//!   types: [`GroupKeySet`] and [`GroupKeyMapEntry`].
//! - **Group multicast send** — [`MatterController::create_group`] generates and
//!   persists a group epoch key (returns a [`GroupKeySet`] ready to program onto
//!   member devices); [`MatterController::invoke_group`] sends a fire-and-forget
//!   group command over IPv6 multicast (`ff35:…`), encrypted with the operational
//!   group key derived from the persisted epoch key. Returns `Ok` on datagram
//!   send; there is no acknowledgement. [`Error::GroupNotProvisioned`] when the
//!   key set has not been created via `create_group`.
//!
//! # Quickstart
//!
//! ```no_run
//! use std::sync::Arc;
//! use matter_controller::{
//!     AttestationTrust, FabricConfig, FileStore, MatterController, MatterTime, ReadPath,
//!     SubscriptionEvent,
//! };
//!
//! # async fn run() -> Result<(), matter_controller::Error> {
//! // Persisted store + attestation trust (test roots shown; use
//! // `AttestationTrust::from_dirs(..)` with production PAA/CD roots for
//! // certified devices).
//! let store = Arc::new(FileStore::new("controller-state.bin"));
//! let controller = MatterController::builder(store)
//!     .attestation_trust(AttestationTrust::example_device_roots())
//!     .build()
//!     .await?;
//!
//! // One-time: create the fabric. `create_fabric` is NOT idempotent — a
//! // second call with an existing `fabric_id` returns
//! // `Error::FabricAlreadyExists` — so gate it on `fabrics()` (a run that
//! // loaded an existing store already has its fabric and identity).
//! if controller.fabrics().await?.is_empty() {
//!     // `not_before` must be a real wall-clock time, backdated a little (an
//!     // hour is plenty) to tolerate device clock skew. `MatterTime(0)` /
//!     // `from_unix_secs(0)` — the Matter epoch — is rejected; so is a time
//!     // far in the future. See `FabricConfig::validity`.
//!     let now_unix = std::time::SystemTime::now()
//!         .duration_since(std::time::UNIX_EPOCH)
//!         .map_err(|e| matter_controller::Error::Operational(e.to_string()))?
//!         .as_secs();
//!     controller.create_fabric(FabricConfig::new(
//!         1,
//!         1,
//!         1,
//!         (
//!             MatterTime::from_unix_secs(now_unix.saturating_sub(3600)),
//!             MatterTime::NO_EXPIRY,
//!         ),
//!     )).await?;
//! }
//!
//! // Commission a device, then control it. `label` is an opaque
//! // caller-supplied string persisted on the device entry; pass `None` if
//! // you have nothing to attach.
//! let info = controller
//!     .commission("MT:Y.K90AFN00KA0648G00", Some("kitchen plug".into()))
//!     .await?;
//! let node = controller.node(info.node_id);
//!
//! // Read all attributes of the OnOff cluster (0x0006) on endpoint 1.
//! let report = node.read(&[ReadPath::cluster(1, 0x0006)]).await?;
//! for (path, value) in report {
//!     println!("{path:?} = {value:?}");
//! }
//!
//! // Subscribe to live changes.
//! let mut sub = node.subscribe(&[ReadPath::cluster(1, 0x0006)], &[], 1, 30).await?;
//! while let Some(event) = sub.next().await {
//!     if let SubscriptionEvent::Report(change) = event {
//!         println!("changed: {:?} = {:?}", change.path, change.value);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Migrating from matter.js? See `docs/matter-js-migration-guide.md`.

#![forbid(unsafe_code)]

pub(crate) mod acl;
pub(crate) mod actor;
pub(crate) mod admin;
pub(crate) mod binding;
#[cfg(feature = "ble")]
pub(crate) mod ble_commission;
pub mod builder;
pub(crate) mod commission;
pub mod controller;
pub(crate) mod credentials;
#[cfg(test)]
mod debug_redaction_tests;
pub mod error;
pub mod fabric;
pub(crate) mod fabric_info;
pub(crate) mod group;
pub(crate) mod handshake_socket;
pub(crate) mod icd;
pub(crate) mod icd_listener;
pub(crate) mod instance_name;
pub mod node;
pub(crate) mod node_info;
pub(crate) mod opcreds;
pub(crate) mod provider_server;
pub(crate) mod resumption;
pub(crate) mod snapshot;
pub mod state;
pub mod store;
pub mod subscription;
pub mod trust;

pub use acl::{AclAuthMode, AclEntry, AclPrivilege, AclTarget};
pub use admin::{
    CommissioningWindow, CommissioningWindowStatus, OpenWindowOpts, WindowStatus,
    DEFAULT_WINDOW_ITERATIONS, DEFAULT_WINDOW_TIMEOUT_S,
};
pub use binding::BindingTarget;
pub use builder::MatterControllerBuilder;
pub use controller::MatterController;
pub use error::Error;
pub use fabric::FabricConfig;
pub use fabric_info::FabricInfo;
pub use group::{GroupKeyMapEntry, GroupKeySet};
pub use icd::{IcdClientType, IcdRegistration};
pub use icd_listener::CheckIn;
pub use instance_name::{operational_instance_name, parse_operational_instance_name};
pub use matter_cert::MatterTime;
pub use matter_codec::Value;
/// Network (Wi-Fi/Thread) credentials for `MatterController::commission_ble`
/// (feature `ble`) and the network-type witness for
/// [`Error::network_feature_unsupported`], re-exported from
/// `matter-commissioning`.
pub use matter_commissioning::{NetworkCredentials, NetworkKind, ThreadDataset, WiFiCredentials};
pub use matter_interaction::{
    AttributePath, CommandPath, EventFilter, EventPath, EventPriority, EventReport,
    EventReportItem, EventTimestamp, ImStatus, ReadPath,
};
pub use node::{DstOffsetEntry, InvokeResult, Node, TimeGranularity, TimeZoneEntry};
pub use node_info::NodeInfo;
pub use opcreds::FabricDescriptor;
/// Low-level provider-server building blocks. **Unstable** — gated behind the
/// `unstable-provider` feature and not covered by semver. The stable OTA path is
/// `MatterController::serve_ota`.
#[cfg(feature = "unstable-provider")]
pub use provider_server::{build_operational_service, ProviderServer};
pub use state::{
    CommissionerIdentity, ControllerState, DeviceEntry, FabricEntry, GroupKeySetConfig,
};
pub use store::{ControllerStore, FileStore, StoreError};
pub use subscription::{AttributeReport, Subscription, SubscriptionEvent};
pub use trust::AttestationTrust;

/// Compile-checks the Rust examples in this crate's `README.md`.
///
/// `#[cfg(doctest)]` means the item exists only while rustdoc is collecting
/// doctests, so the README is compiled by `cargo test --doc` without being
/// duplicated into the rendered crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
