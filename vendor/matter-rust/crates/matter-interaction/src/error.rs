//! Error type for Interaction Model framing.

#![forbid(unsafe_code)]

use thiserror::Error;

/// Errors produced while building or parsing Interaction Model messages.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ImError {
    /// Underlying TLV decode failure.
    #[error("TLV codec error: {0}")]
    Codec(#[from] matter_codec::Error),

    /// The message's outermost element was not the expected anonymous struct.
    #[error("expected anonymous structure at message root")]
    NotAStruct,

    /// A required field was absent from the message.
    #[error("missing required IM field: {0}")]
    MissingField(&'static str),

    /// A field held a TLV value of an unexpected type.
    #[error("unexpected TLV value for IM field: {0}")]
    UnexpectedValue(&'static str),

    /// An `InvokeResponseIB` contained neither a Command nor a Status member.
    #[error("InvokeResponseIB had neither Command nor Status")]
    EmptyInvokeResponse,

    /// A `StatusIB.Status` field was present on the wire but carried a value
    /// outside the valid Matter status-code range (`0x00..=0xFF`, a single
    /// octet per Matter Core Spec §8.10). The raw decoded value is preserved
    /// so callers can log the malformed code. This is deliberately distinct
    /// from [`MissingField`](Self::MissingField): the field *was* present, it
    /// was simply out of range, and conflating the two would mislead a caller
    /// diagnosing a non-conformant device.
    #[error("StatusIB.Status out of range: {code} (valid 0x00..=0xFF)")]
    InvalidStatusCode {
        /// The raw, out-of-range status value as decoded from the wire.
        code: u64,
    },

    /// A [`ReportAccumulator`](crate::ReportAccumulator) exceeded its in-crate
    /// total-size ceiling while merging chunked `ReportData`. This is
    /// defense-in-depth against a peer streaming an unbounded chunked
    /// read/report set: the accumulator caps both the number of distinct
    /// accumulated elements and an estimate of their total in-memory byte
    /// size, returning this error rather than growing without bound.
    #[error(
        "ReportAccumulator ceiling exceeded: {elements} elements / ~{bytes} bytes \
         (max {max_elements} elements / {max_bytes} bytes)"
    )]
    AccumulatorOverflow {
        /// Distinct accumulated elements at the point the cap was hit.
        elements: usize,
        /// Estimated total accumulated byte size at the point the cap was hit.
        bytes: usize,
        /// The configured maximum element count.
        max_elements: usize,
        /// The configured maximum estimated byte size.
        max_bytes: usize,
    },
}
