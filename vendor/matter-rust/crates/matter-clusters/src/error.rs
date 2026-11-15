//! Errors surfaced by generated cluster codecs.

use matter_codec::Error as TlvError;

/// Error returned by a generated attribute/command/struct decoder.
///
/// Deliberately has **no** `InvalidEnumValue` variant: unknown enum
/// discriminants decode to the enum's `Unknown(n)` variant, never an error
/// (forward compatibility — a device on a newer spec revision must not break
/// our decode).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClusterError {
    /// The underlying TLV codec rejected the bytes.
    #[error("TLV codec error: {0}")]
    Tlv(#[from] TlvError),

    /// A TLV element had a type we did not expect for this field.
    #[error("unexpected TLV type for {context}")]
    UnexpectedType {
        /// Where the mismatch occurred (e.g. `"OnOff::OnTime"`).
        context: &'static str,
    },

    /// A required struct/command field was absent.
    #[error("missing required field: {0}")]
    MissingField(&'static str),

    /// An integer or list length did not fit its declared Rust width.
    #[error("value out of range for {0}")]
    InvalidLength(&'static str),
}
