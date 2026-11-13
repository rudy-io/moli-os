//! Per-cluster command + attribute codecs for clusters used during
//! commissioning.
//!
//! The `OperationalCredentials` cluster's NOC-issuance subset lives in
//! `noc/commands.rs` rather than here, alongside the certificate work it
//! serves.

#![forbid(unsafe_code)]

pub mod general_commissioning;
pub mod network_commissioning;
