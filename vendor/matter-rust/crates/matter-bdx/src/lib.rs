//! Matter Bulk Data Exchange (BDX) protocol — `ProtocolId::BDX` (0x0002),
//! Matter Core Specification §11.21.
//!
//! BDX is the bulk file-transfer protocol that backs OTA firmware download (and
//! diagnostic-log retrieval). This crate is **standalone** — message codecs plus
//! a sans-I/O **receiver-driven sender** state machine. It performs no
//! networking and has no OTA dependency; a caller (`matter-controller`'s OTA
//! provider server is the one in this workspace) frames each outgoing message
//! over `ProtocolId::BDX` and drives the transfer over a CASE session.
//!
//! ## Scope
//!
//! Receiver-driven **sender** happy-path + basic abort: accept a `ReceiveInit`,
//! answer each `BlockQuery` with the next `Block`/`BlockEOF`, finish on
//! `BlockAckEOF`. Resumption, `BlockQueryWithSkip`, and sender-drive mode are
//! deferred (YAGNI). BDX message `Metadata` is treated as an opaque byte
//! passthrough (empty for OTA), so no TLV dependency is needed.

#![forbid(unsafe_code)]

pub mod error;
pub mod message_type;
pub mod messages;
pub mod sender;

pub use error::BdxError;
pub use message_type::{BdxStatusCode, MessageType};
pub use messages::{
    BdxMessage, CounterMessage, DataBlock, RangeControl, ReceiveAccept, SendAccept,
    TransferControl, TransferInit, BDX_VERSION,
};
pub use sender::{BlockSender, OutgoingMessage, SenderOutcome};

/// Compile-checks the Rust examples in this crate's `README.md`.
///
/// `#[cfg(doctest)]` means the item exists only while rustdoc is collecting
/// doctests, so the README is compiled by `cargo test --doc` without being
/// duplicated into the rendered crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
