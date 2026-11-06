//! Matter PASE (Password Authenticated Session Establishment) via SPAKE2+.
//!
//! SPAKE2+ math over P-256 with Matter's M and N constants, the PBKDF2 and
//! HKDF derivations, the wire-format messages, and the sans-IO
//! [`PaseProver`] / [`PaseVerifier`] state machines that drive them.
//! Byte-checked against matter.js fixtures.
//!
//! See Matter Core Specification §3.10.

pub(crate) mod kdf;
pub(crate) mod messages;
pub(crate) mod prover;
pub(crate) mod spake2plus;
pub(crate) mod verifier;

pub use kdf::pake_passcode_verifier;
pub use prover::PaseProver;
pub use verifier::PaseVerifier;

/// Identifies one of the 5 PASE message types. Used by
/// [`crate::Error::UnexpectedMessage`] and `expected_inbound()` accessors
/// on the state machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PaseMessageKind {
    /// Commissioner -> verifier, negotiation path.
    PbkdfParamRequest,
    /// Verifier -> commissioner, negotiation path.
    PbkdfParamResponse,
    /// Commissioner -> verifier, X point.
    Pake1,
    /// Verifier -> commissioner, Y point + cB confirmation.
    Pake2,
    /// Commissioner -> verifier, cA confirmation.
    Pake3,
}

/// Negotiable PASE PBKDF parameters (Matter spec §3.10.3).
///
/// Produced by decoding a `PbkdfParamResponse` and consumed by both
/// `PaseProver` and `PaseVerifier` state machines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasePbkdfParams {
    /// PBKDF2 iteration count. Matter spec §3.10.3 requires ≥ 1000.
    pub iterations: u32,
    /// PBKDF2 salt. Matter spec §3.10.3 requires 16–32 bytes.
    pub salt: Vec<u8>,
}

/// Session keys produced by a completed PASE handshake (spec §3.10.7).
///
/// Contains the 16-byte shared secret `Ke` (`TT_HASH`\[16..32\]) and the
/// three per-session keys derived from it via HKDF `"SessionKeys"`.
///
/// # Key layout (matter.js `NodeSession.ts`, commissioner = initiator)
///
/// ```text
/// blob = HKDF-SHA256(Ke, salt=[], "SessionKeys", 48)
/// i2r_key  = blob[0..16]   (initiator→responder; encrypt for commissioner)
/// r2i_key  = blob[16..32]  (responder→initiator; decrypt for commissioner)
/// attestation_key = blob[32..48]
/// ```
///
/// # Secret hygiene
///
/// This type carries live symmetric key material. It implements
/// [`zeroize::ZeroizeOnDrop`] so the key bytes are wiped from memory when the
/// value is dropped, and its [`Debug`] impl redacts every field (printing
/// `PaseSessionKeys { .. }`) so key bytes never reach logs. Equality is
/// intentionally *not* derived: comparing session keys with the variable-time
/// `==` would be a timing side-channel, and no caller needs it (tests compare
/// individual byte-array fields directly).
#[derive(Clone, zeroize::ZeroizeOnDrop)]
pub struct PaseSessionKeys {
    /// Shared symmetric secret (`Ke`): `TT_HASH`\[16..32\].
    ///
    /// This is the raw SPAKE2+ session secret. Higher layers can re-derive
    /// `i2r_key`, `r2i_key`, and `attestation_key` from this alone.
    pub ke: [u8; 16],
    /// Initiator-to-responder (commissioner → device) encryption key.
    pub i2r_key: [u8; 16],
    /// Responder-to-initiator (device → commissioner) decryption key.
    pub r2i_key: [u8; 16],
    /// Attestation challenge key (used for device attestation in commissioning).
    pub attestation_key: [u8; 16],
}

impl core::fmt::Debug for PaseSessionKeys {
    /// Redacts all key material; never prints key bytes.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PaseSessionKeys").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod secret_hygiene_tests {
    use super::*;

    /// Compile-time proof that `PaseSessionKeys: ZeroizeOnDrop`.
    fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}

    #[test]
    fn pase_session_keys_is_zeroize_on_drop() {
        assert_zeroize_on_drop::<PaseSessionKeys>();
    }

    #[test]
    fn pase_session_keys_debug_redacts_key_bytes() {
        let keys = PaseSessionKeys {
            ke: [0xAA; 16],
            i2r_key: [0xBB; 16],
            r2i_key: [0xCC; 16],
            attestation_key: [0xDD; 16],
        };
        let s = format!("{keys:?}");
        assert!(!s.contains("aa"), "ke bytes leaked: {s}");
        assert!(!s.contains("bb"), "i2r_key bytes leaked: {s}");
        assert!(!s.contains("cc"), "r2i_key bytes leaked: {s}");
        assert!(!s.contains("dd"), "attestation_key bytes leaked: {s}");
        assert!(s.contains("PaseSessionKeys"));
    }
}
