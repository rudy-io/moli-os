//! Matter network transport: secured-message framing, session management,
//! UDP, mDNS, and MRP reliability.
//!
//! Consumes session keys produced by completed PASE handshakes
//! (`matter_crypto::pase`) or CASE handshakes (`matter_crypto::case`) and
//! ships messages over the wire in Matter's secured-message format.
//!
//! # What's here
//!
//! - [`framing`] — secured-message header encode/decode, AES-CCM-128 payload
//!   encryption, sliding-window replay protection, and the group (multicast)
//!   variants with message privacy.
//! - [`session`] — [`SessionManager`] owns per-session counters, replay
//!   windows, and MRP state, and is the seam through which messages are
//!   encoded outbound and decoded inbound.
//! - [`mrp`] — the Message Reliability Protocol as a sans-IO state machine:
//!   pending acks, piggybacking, the exchange table, retransmit scheduling.
//!   Retransmit timing is sized to the *peer* from its advertised
//!   `SII`/`SAI`/`SAT` ([`MrpConfig::for_peer`]), so a sleepy device is not
//!   hammered with active-interval spacing.
//! - [`protocol_header`] — the Matter application protocol header codec.
//! - [`secure_channel`] — the Secure Channel `CloseSession` status report:
//!   encode one for a session, and recognise one in a decrypted message.
//! - [`transport`] and [`discovery`] — the sans-IO [`Transport`] and
//!   [`Discovery`] traits, plus the service-record types for Matter's
//!   commissionable and operational mDNS records.
//! - Default adapters behind Cargo features: Tokio UDP and `mdns-sd`.
//!
//! Framing, MRP, and the protocol header are byte-checked against matter.js
//! fixtures.
//!
//! # Cargo features
//!
#![cfg_attr(
    feature = "tokio",
    doc = "- `tokio` (default): enables [`tokio_udp::TokioUdpTransport`] and the `Error::Io` variant."
)]
#![cfg_attr(
    not(feature = "tokio"),
    doc = "- `tokio` (default): enables `tokio_udp::TokioUdpTransport` and the `Error::Io` variant."
)]
#![cfg_attr(
    feature = "mdns-sd",
    doc = "- `mdns-sd` (default): enables [`mdns_sd_discovery::MdnsSdDiscovery`] and the `Error::Mdns` variant."
)]
#![cfg_attr(
    not(feature = "mdns-sd"),
    doc = "- `mdns-sd` (default): enables `mdns_sd_discovery::MdnsSdDiscovery` and the `Error::Mdns` variant."
)]
//!
//! Embedded callers disable defaults: the sans-IO core (framing, MRP,
//! protocol header, session manager, `Transport`/`Discovery` traits)
//! is always available.

#![forbid(unsafe_code)]

#[cfg(test)]
mod debug_redaction_tests;
pub mod discovery;
pub mod error;
pub mod framing;
pub mod local_addr;
pub mod mrp;
pub mod protocol_header;
pub mod secure_channel;
pub mod session;
pub mod transport;

#[cfg(feature = "tokio")]
pub mod tokio_udp;

#[cfg(feature = "mdns-sd")]
pub mod mdns_sd_discovery;

pub use error::{Error, Result};
pub use framing::{
    build_group_privacy_nonce, decode_group_secured, decode_group_secured_with_privacy_key,
    decode_header, decode_secured, encode_group_secured, encode_group_secured_with_privacy_key,
    encode_header, encode_secured, DestNodeId, MessageCounter, NodeId, ReplayWindow,
    SecuredMessageFlags, SecuredMessageHeader, SecurityFlags, SessionId,
};
pub use mrp::{
    InboundOutcome, MrpConfig, MrpEvent, MrpFlags, MrpProvenance, MrpState, MrpTimerEvent,
    PreparedOutbound, RecentInboundView,
};
pub use protocol_header::{
    build_standalone_ack_header, decode_protocol_header, encode_protocol_header, ExchangeFlags,
    ProtocolHeader, ProtocolId,
};
pub use session::{
    DecodeInboundOutput, EncodeOutboundOutput, PeerHint, Session, SessionKeys, SessionManager,
    SessionRole,
};

pub use discovery::{
    operational_fabric_subtype, Discovery, MatterService, QueryHandle, ServiceKind,
};
pub use local_addr::local_advertise_addrs;
pub use transport::{PeerAddress, Transport};

#[cfg(feature = "tokio")]
pub use tokio_udp::TokioUdpTransport;

#[cfg(feature = "mdns-sd")]
pub use mdns_sd_discovery::MdnsSdDiscovery;

/// Compile-checks the Rust examples in this crate's `README.md`.
///
/// `#[cfg(doctest)]` means the item exists only while rustdoc is collecting
/// doctests, so the README is compiled by `cargo test --doc` without being
/// duplicated into the rendered crate docs.
///
/// Additionally gated on `feature = "tokio"`: the README's minimal example
/// uses `TokioUdpTransport`, which that feature gates. `just embedded` runs
/// `cargo test --no-default-features --doc` for this crate, so without the
/// gate the sans-IO build would fail on an example it cannot compile.
#[cfg(all(doctest, feature = "tokio"))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
