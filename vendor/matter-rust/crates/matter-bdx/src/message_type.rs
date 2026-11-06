//! BDX message types (the opcode carried in the Matter protocol header for
//! `ProtocolId::BDX`) and BDX status codes (Matter Core §11.21).

#![forbid(unsafe_code)]

/// A BDX message type — its value is the protocol-header opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    /// `SendInit` (0x01).
    SendInit,
    /// `SendAccept` (0x02).
    SendAccept,
    /// `ReceiveInit` (0x04).
    ReceiveInit,
    /// `ReceiveAccept` (0x05).
    ReceiveAccept,
    /// `BlockQuery` (0x10).
    BlockQuery,
    /// `Block` (0x11).
    Block,
    /// `BlockEOF` (0x12).
    BlockEof,
    /// `BlockAck` (0x13).
    BlockAck,
    /// `BlockAckEOF` (0x14).
    BlockAckEof,
}

impl MessageType {
    /// The protocol-header opcode for this message type.
    #[must_use]
    pub fn to_u8(self) -> u8 {
        match self {
            Self::SendInit => 0x01,
            Self::SendAccept => 0x02,
            Self::ReceiveInit => 0x04,
            Self::ReceiveAccept => 0x05,
            Self::BlockQuery => 0x10,
            Self::Block => 0x11,
            Self::BlockEof => 0x12,
            Self::BlockAck => 0x13,
            Self::BlockAckEof => 0x14,
        }
    }

    /// Decode an opcode into a [`MessageType`] (`None` for unknown/deferred,
    /// e.g. `BlockQueryWithSkip` 0x15).
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x01 => Self::SendInit,
            0x02 => Self::SendAccept,
            0x04 => Self::ReceiveInit,
            0x05 => Self::ReceiveAccept,
            0x10 => Self::BlockQuery,
            0x11 => Self::Block,
            0x12 => Self::BlockEof,
            0x13 => Self::BlockAck,
            0x14 => Self::BlockAckEof,
            _ => return None,
        })
    }
}

/// BDX status codes (Matter Core §11.21.x), carried in a Secure-Channel
/// `StatusReport` when a transfer aborts. The state machine surfaces these
/// as the abort reason; F3 encodes the actual `StatusReport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BdxStatusCode {
    /// `0x0012` — proposed length exceeds the sender's limit.
    LengthTooLarge,
    /// `0x0016` — message contents could not be parsed.
    BadMessageContents,
    /// `0x0017` — a block counter did not match the expected value.
    BadBlockCounter,
    /// `0x0018` — a message arrived in a state that does not expect it.
    UnexpectedMessage,
    /// `0x0050` — the proposed transfer control (drive mode) is not supported.
    TransferMethodNotSupported,
    /// `0x005F` — unknown / catch-all failure.
    Unknown,
}

impl BdxStatusCode {
    /// The 16-bit BDX status code.
    #[must_use]
    pub fn to_u16(self) -> u16 {
        match self {
            Self::LengthTooLarge => 0x0012,
            Self::BadMessageContents => 0x0016,
            Self::BadBlockCounter => 0x0017,
            Self::UnexpectedMessage => 0x0018,
            Self::TransferMethodNotSupported => 0x0050,
            Self::Unknown => 0x005F,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;

    #[test]
    fn message_type_roundtrips_all_known_opcodes() {
        for op in [0x01u8, 0x02, 0x04, 0x05, 0x10, 0x11, 0x12, 0x13, 0x14] {
            let mt = MessageType::from_u8(op).expect("known opcode");
            assert_eq!(mt.to_u8(), op);
        }
    }

    #[test]
    fn message_type_rejects_unknown_and_deferred() {
        assert_eq!(MessageType::from_u8(0x00), None);
        assert_eq!(MessageType::from_u8(0x15), None); // BlockQueryWithSkip (deferred)
        assert_eq!(MessageType::from_u8(0x03), None); // reserved gap
    }

    #[test]
    fn status_codes_match_spec_values() {
        assert_eq!(BdxStatusCode::BadBlockCounter.to_u16(), 0x0017);
        assert_eq!(BdxStatusCode::UnexpectedMessage.to_u16(), 0x0018);
        assert_eq!(BdxStatusCode::TransferMethodNotSupported.to_u16(), 0x0050);
    }
}
