//! Async Tokio commissioning driver — the IO layer beneath the sans-IO
//! [`Commissioner`](crate::Commissioner). Gated behind the `driver` feature.
//!
//! [`commission()`] is the entry point: it discovers the device, runs PASE,
//! then drives the state machine's [`Action`](crate::Action)s over the wire —
//! Invoke and Read round-trips, the CASE handshake, and the operational
//! mDNS resolve — through to a `CommissionedFabric`. [`commission_ble`] does
//! the same over a BLE (BTP) transport.
//!
//! The layers underneath are public too, so you can assemble your own flow:
//! [`AsyncDatagram`] is the transport seam, [`secured_round_trip`] and
//! [`secured_read`] are the exchange helpers, [`UnsecuredExchange`] is the
//! session-id-0 framing the PASE handshake runs over, and [`run_pase`] /
//! [`run_case`] are the handshake bridges.

mod case;
mod commission;
mod datagram;
mod error;
mod exchange;
mod pase;
mod unsecured;

pub use case::{
    operational_instance_name, preferred_address, resolve_operational,
    resolve_operational_with_attempts, resolve_operational_with_mrp, run_case, run_case_establish,
    run_case_establish_with, CaseEstablishOptions, CaseEstablished, BLE_RESOLVE_POLL_ATTEMPTS,
};
pub use commission::{
    commission, commission_ble, resolve_commissionable, BleDriverConfig, DriverConfig, STREAM_PEER,
};
pub use datagram::{AsyncDatagram, InMemoryDatagram};
pub use error::DriverError;
pub use exchange::{
    secured_read, secured_round_trip, SecuredResponse, MAX_READ_BYTES, MAX_READ_CHUNKS,
    MAX_READ_CHUNK_BYTES,
};
pub use pase::{run_pase, run_pase_with};
pub use unsecured::{
    decode_unsecured, encode_unsecured, encode_unsecured_reply, parse_status_report,
    random_exchange_id, require_handshake_opcode, require_handshake_opcode_any,
    SecureChannelStatus, UnsecuredExchange, UnsecuredMessage, MAX_HANDSHAKE_RETRANSMIT_WINDOW,
};

/// How reliability is provided under an unsecured (PASE handshake) exchange.
///
/// Matter offers a message reliability at the exchange layer (MRP) *and* the
/// possibility of running over an already-reliable transport. Which one is in
/// force changes what the unsecured PASE path puts on the wire.
///
/// Spec §4.12: MRP is **off** over BLE/BTP, whose transport is already
/// reliable and ordered — so the R-flag, retransmits, and standalone acks are
/// all suppressed there. Over plain UDP, MRP is the reliability mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportReliability {
    /// UDP: MRP provides reliability (R-flag set, stop-and-wait retransmits,
    /// standalone acks). This is the historical default for the UDP path.
    Mrp,
    /// BTP or in-memory: the transport is itself reliable and ordered, so the
    /// exchange never sets the R-flag, never retransmits, and never sends
    /// standalone acks (Matter spec §4.12: MRP off over BLE).
    TransportProvides,
}
