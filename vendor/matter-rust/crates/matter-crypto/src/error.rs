//! Error type for `matter-crypto`.

use thiserror::Error;

use crate::pase::PaseMessageKind;

/// All errors `matter-crypto` can produce in M3 (PASE). M4 (CASE) will
/// extend this enum; `#[non_exhaustive]` keeps that addition non-breaking.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    // --- Spec-defined PASE status codes — transmissible on the wire. ---
    /// Spec §3.10.5 status code: peer's parameter was malformed or out of range.
    #[error("PASE: invalid parameter")]
    InvalidParameter,

    /// Spec §3.10.5 status code: peer's signature or confirmation tag did not validate.
    #[error("PASE: invalid signature or tag")]
    InvalidSignatureOrTag,

    /// Spec §3.10.5 status code: referenced session does not exist.
    #[error("PASE: session not found")]
    SessionNotFound,

    /// Spec §3.10.5 status code: referenced session has expired.
    #[error("PASE: session expired")]
    SessionExpired,

    /// Spec §3.10.5 status code: peer is busy.
    #[error("PASE: busy")]
    Busy,

    // --- Internal errors — never on the wire. ---
    /// TLV codec error propagated from `matter-codec`.
    #[error("TLV codec error: {0}")]
    Codec(#[from] matter_codec::Error),

    /// SPAKE2+ scalar was zero or out of range after sampling/reduction.
    #[error("invalid SPAKE2+ scalar (out of range or all-zero)")]
    InvalidScalar,

    /// Confirmation tag did not match in constant-time compare.
    ///
    /// We never tell the peer which tag failed — that itself would be
    /// a side-channel. Local-only abort.
    #[error("confirmation tag did not match (constant-time compare)")]
    ConfirmationTagMismatch,

    /// Caller invoked a state-machine method that doesn't match the
    /// current expected message (e.g., `handle_pake3` before `handle_pake2`).
    #[error("unexpected PASE message: expected {expected:?}, got {got:?}")]
    UnexpectedMessage {
        /// The next message kind the state machine was expecting.
        expected: PaseMessageKind,
        /// The kind the caller tried to feed.
        got: PaseMessageKind,
    },

    /// PIN-to-w0/w1 derivation failed.
    #[error("PIN derivation failed")]
    PinDerivationFailed,

    /// Generic HKDF/KDF operation failed (e.g., ring returned an error for an
    /// output-length or expand call that should succeed for all valid inputs).
    ///
    /// Used by operational identity derivations such as
    /// [`crate::derive_compressed_fabric_id`].
    #[error("key derivation failed")]
    KeyDerivationFailed,

    /// PBKDF iteration count below the Matter spec minimum (1000).
    #[error("PBKDF iteration count {0} below spec minimum 1000")]
    PbkdfIterationsTooLow(u32),

    /// PBKDF iteration count above the accepted ceiling.
    ///
    /// The iteration count is peer-controlled and is fed directly into
    /// PBKDF2-HMAC-SHA256 (one HMAC pass per iteration). A malicious or
    /// spoofed PASE responder could advertise an inflated count
    /// (up to `u32::MAX`) to force the commissioner into billions of HMAC
    /// rounds — a single-threaded CPU denial-of-service per handshake. We
    /// reject anything above the Matter spec's published maximum (100000)
    /// *before* any key derivation runs.
    #[error("PBKDF iteration count {iterations} above accepted maximum {max}")]
    PbkdfIterationsTooHigh {
        /// The (rejected) iteration count advertised by the peer.
        iterations: u32,
        /// The maximum iteration count we accept.
        max: u32,
    },

    /// PBKDF salt length outside [16, 32] bytes per Matter spec §3.10.3.
    #[error("PBKDF salt length {0} not in [16, 32]")]
    PbkdfSaltLengthInvalid(usize),

    /// `finish()` called before the handshake completed all phases.
    #[error("`finish` called before handshake complete")]
    HandshakeIncomplete,

    // --- CASE-specific internal errors (M4) ---
    /// Peer's NOC chain failed validation against the trusted RCAC roots.
    #[error("CASE: invalid peer NOC chain — {0}")]
    InvalidPeerNocChain(#[source] matter_cert::Error),

    /// Peer's NOC carried a `FabricId` attribute that does not match the
    /// `FabricId` we expected (i.e., the peer is on a different fabric).
    #[error("CASE: peer NOC's fabric_id ({peer}) doesn't match local fabric_id ({local})")]
    FabricIdMismatch {
        /// `FabricId` carried by the peer's NOC.
        peer: u64,
        /// `FabricId` we expected (from our own credentials).
        local: u64,
    },

    /// Peer's NOC carried a `NodeId` attribute that does not match the
    /// `NodeId` we expected.
    #[error("CASE: peer NOC's node_id ({0}) doesn't match expected ({1})")]
    PeerNodeIdMismatch(u64, u64),

    /// Ephemeral P-256 keypair generation failed (RNG failure or
    /// repeated zero scalars).
    #[error("CASE: ephemeral key generation failed")]
    EphemeralKeyGenerationFailed,

    /// AEAD decryption of an encrypted blob (Sigma2 or Sigma3 ciphertext)
    /// failed — corrupt ciphertext, wrong key, or wrong nonce.
    #[error("CASE: AEAD decryption of encrypted blob failed")]
    EncryptedBlobDecryptionFailed,

    /// AEAD encryption failed — cipher initialisation rejected the key, or
    /// the underlying AES-CCM encrypt call failed. Not expected in practice
    /// for the spec-bounded key and message sizes.
    #[error("AEAD encryption failed")]
    EncryptionFailed,

    /// Peer's ECDSA signature over the SIGMA transcript did not verify.
    #[error("CASE: peer signature did not verify")]
    PeerSignatureInvalid,

    /// Resumption MAC tag (`sigma2_resume_mic` / `sigma3_resume_mic`) did
    /// not verify in constant time.
    #[error("CASE: resumption MAC tag did not verify")]
    ResumptionMacMismatch,

    /// Secure random generation failed.
    #[error("secure random generation failed")]
    Rng,

    /// `CaseSigner` returned a `SignerError`.
    #[error("CASE: signing failed — {0}")]
    SigningFailed(#[source] crate::case::signer::SignerError),

    /// Caller fed a CASE state machine a message that doesn't match its
    /// current expected-inbound kind (e.g., calling `handle_sigma2` before
    /// `start` has been invoked).
    ///
    /// A separate variant from [`Error::UnexpectedMessage`] keeps M3 PASE
    /// callers unchanged while surfacing the correct
    /// [`crate::case::CaseMessageKind`].
    #[error("CASE: unexpected message: expected {expected:?}, got {got:?}")]
    UnexpectedCaseMessage {
        /// The message kind the state machine was waiting for.
        expected: crate::case::CaseMessageKind,
        /// The kind the caller tried to supply.
        got: crate::case::CaseMessageKind,
    },
}

/// `Result<T, Error>` for convenience.
pub type Result<T> = core::result::Result<T, Error>;
