//! Error type for `matter-transport`.
//!
//! M5.1 ships the subset of variants the framing + session-manager layers
//! produce. M5.2 adds MRP variants. M5.3 adds UDP and mDNS variants behind
//! the relevant Cargo features.

use thiserror::Error;

/// Convenience `Result` alias for this crate.
pub type Result<T> = core::result::Result<T, Error>;

/// All failures that can surface from the `matter-transport` framing and
/// session layers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The secured-message header bytes could not be parsed. The `usize` is
    /// the byte offset at which parsing failed.
    #[error("malformed secured-message header at byte {0}")]
    MalformedHeader(usize),

    /// AES-CCM tag verification failed. Indistinguishable from "ciphertext
    /// was tampered with" by spec design — do NOT branch on this for
    /// detection logic.
    #[error("AES-CCM decryption failed (wrong key or tag mismatch)")]
    DecryptionFailed,

    /// Inbound message counter has already been seen and is inside the
    /// replay window.
    #[error("inbound counter {counter} already seen in replay window")]
    ReplayedCounter {
        /// Counter value that was rejected.
        counter: u32,
    },

    /// Inbound message counter is older than the lowest counter the replay
    /// window still tracks.
    #[error("inbound counter {counter} too old (window: {window_low}..={window_high})")]
    CounterTooOld {
        /// Counter value that was rejected.
        counter: u32,
        /// Low end of the still-tracked window.
        window_low: u32,
        /// High end of the still-tracked window.
        window_high: u32,
    },

    /// Inbound packet referenced a session ID we have no record of.
    #[error("unknown session ID {0}")]
    UnknownSession(u16),

    /// Outbound message counter would wrap past `u32::MAX`. The session must
    /// be torn down and re-established (a new CASE handshake) to continue.
    #[error("session counter overflow — session must be re-keyed")]
    CounterOverflow,

    /// A payload was too large for the secured-message format
    /// (`AES-CCM-128` has a hard cap; we additionally cap to keep MTU sane).
    #[error("payload too large ({len} bytes; max {max})")]
    PayloadTooLarge {
        /// Length attempted.
        len: usize,
        /// Maximum allowed (constant set in `framing.rs`).
        max: usize,
    },

    /// Decrypted protocol header bytes could not be parsed. The `usize` is
    /// the byte offset within the decrypted payload (not the wire packet)
    /// at which parsing failed. Surfaces only after successful AES-CCM
    /// authentication, so a `MalformedProtocolHeader` necessarily
    /// originates from an authenticated peer.
    #[error("malformed protocol header at byte {0}")]
    MalformedProtocolHeader(usize),

    /// MRP retransmit attempts exhausted for a specific outbound message.
    /// `MrpEvent::Expired` is emitted alongside via `handle_timeout`;
    /// this variant exists for callers who prefer `Result`-style error
    /// handling over event-loop consumption.
    #[error("MRP retransmit exhausted for exchange {exchange_id} counter {counter}")]
    MrpRetransmitExhausted {
        /// Exchange ID whose pending message expired.
        exchange_id: u16,
        /// Outbound counter of the expired message.
        counter: u32,
    },

    /// An inbound message referenced a previously-unseen exchange ID while
    /// the per-session exchange table was already at its cap
    /// (`MAX_EXCHANGES_PER_SESSION`). The new exchange is dropped to bound
    /// memory; messages on already-tracked exchanges are unaffected. A
    /// well-behaved peer never reaches this limit (the cap sits far above
    /// any realistic concurrent-exchange count), so this surfaces only an
    /// an abusive or buggy peer.
    #[error("per-session MRP exchange table is full; dropping new exchange")]
    ExchangeTableFull,

    /// UDP I/O failure from the Tokio adapter. Feature-gated so embedded
    /// builds without `tokio` don't carry an `io::Error` dependency on
    /// this variant.
    #[cfg(feature = "tokio")]
    #[error("UDP I/O: {0}")]
    Io(#[from] std::io::Error),

    /// mDNS daemon failure. Stored as `String` because `mdns_sd::Error`
    /// is not `Eq` and we want a uniform variant shape. Feature-gated.
    #[cfg(feature = "mdns-sd")]
    #[error("mDNS: {0}")]
    Mdns(String),

    /// Bridge from the matter-crypto AEAD module. Currently surfaces as
    /// `DecryptionFailed` in practice — this variant exists so the From impl
    /// is total.
    #[error("crypto error: {0}")]
    Crypto(#[from] matter_crypto::Error),
}
