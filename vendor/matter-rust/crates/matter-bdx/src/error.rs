//! Error type for BDX message codecs and the sender state machine.

#![forbid(unsafe_code)]

/// Errors produced while decoding a BDX message.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum BdxError {
    /// The body ended before a fixed-width field could be read.
    #[error("BDX message truncated")]
    Truncated,

    /// The opcode did not name a BDX message this crate decodes.
    #[error("unknown BDX message type: {0:#04x}")]
    UnknownMessageType(u8),
}
