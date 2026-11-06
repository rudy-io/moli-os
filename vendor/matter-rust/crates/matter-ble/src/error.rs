use thiserror::Error;

/// Errors from BTP parsing, session state, and BLE advertisement handling.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum BtpError {
    /// Service data shorter than the 8-byte commissionable advertisement.
    #[error("advertisement service data too short: {0} bytes")]
    AdvertTooShort(usize),
    /// Advertisement opcode byte was not 0x00 (commissionable).
    #[error("unsupported advertisement opcode {0:#04x}")]
    UnsupportedOpcode(u8),
    /// Handshake packet failed structural validation.
    #[error("malformed handshake packet")]
    MalformedHandshake,
    /// Peer selected a BTP version other than 4.
    #[error("unsupported BTP version {0}")]
    UnsupportedVersion(u8),
    /// Negotiated fragment size below the 6-byte minimum.
    #[error("fragment size {0} below minimum 6")]
    FragmentTooSmall(u16),
    /// Received sequence number != expected `RxNext`.
    #[error("invalid sequence: expected {expected}, got {got}")]
    InvalidSequence {
        /// The expected `RxNext` sequence number.
        expected: u8,
        /// The sequence number actually received.
        got: u8,
    },
    /// Ack for a sequence number that is not outstanding.
    #[error("invalid ack {0}")]
    InvalidAck(u8),
    /// A fragment arrived that the reassembler cannot accept in its current
    /// state (chip `BLE_ERROR_REASSEMBLER_INCORRECT_STATE`): a new `Begin`
    /// while a message is already in progress or completed-but-unacknowledged,
    /// or a `Continue`/`End` with no message in progress.
    #[error("reassembler in incorrect state")]
    ReassemblerInvalidState,
    /// `queue_message` called while a message is still in flight.
    #[error("a message is already in flight")]
    MessageInFlight,
    /// Reassembled message would exceed the 2048-byte cap.
    #[error("reassembly overflow")]
    ReassemblyOverflow,
    /// Packet too short for its declared flags.
    #[error("packet too short")]
    PacketTooShort,
    /// No ack received for a sent fragment within 15 s.
    #[error("ack timed out")]
    AckTimedOut,
}
