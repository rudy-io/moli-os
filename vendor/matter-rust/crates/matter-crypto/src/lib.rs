//! Matter session-establishment protocols.
//!
//! # Scope
//!
//! - [`pase`]: Password Authenticated Session Establishment via SPAKE2+
//!   (spec §3.10). Sans-IO [`PaseProver`] / [`PaseVerifier`] state machines,
//!   PBKDF2 setup-PIN derivation, HKDF session-key derivation, and
//!   constant-time confirmation-tag comparison.
//! - [`case`]: Certificate Authenticated Session Establishment via SIGMA-I
//!   (spec §4.13). Sans-IO [`CaseInitiator`] / [`CaseResponder`] state
//!   machines, NOC chain validation via `matter-cert`, and session
//!   resumption (Sigma1 + `Sigma2_Resume`). Signing goes through the
//!   [`CaseSigner`] trait, so an HSM, TPM, or secure element can hold the
//!   operational key instead of this process.
//! - [`operational`]: operational identity derivations (spec §4.3) — the
//!   Compressed Fabric Identifier, the operational IPK, and the group
//!   session/privacy keys and multicast address.
//! - [`checkin`]: the ICD Check-In message codec (spec §4.18.2), the payload
//!   an intermittently-connected device sends a registered client when it
//!   briefly wakes.
//! - [`aead`]: AES-128-CCM-128 AEAD helpers, used by CASE here and by
//!   `matter-transport`'s secured-message framing. Prefer [`SessionAead`]
//!   over the free functions on any path that encrypts/decrypts more than
//!   once per key, to avoid repeating AES key expansion per call.
//! - [`error`]: the crate error type.
//!
//! Both handshakes are sans-IO: they consume and produce message bytes, and
//! the caller owns the transport. PASE and CASE are byte-checked against
//! matter.js fixtures.
//!
//! # Cryptographic discipline
//!
//! This crate never implements primitives. AES, ECDH, ECDSA, SHA, HKDF, and
//! HMAC come from `ring`. EC scalar/point arithmetic (which ring deliberately
//! doesn't expose) comes from `p256`. We implement only the Matter-defined
//! protocols on top of those primitives.

#![forbid(unsafe_code)]

pub mod aead;
pub mod case;
pub mod checkin;
pub mod error;
pub mod operational;
pub mod pase;
pub mod session_params;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use aead::SessionAead;
pub use case::initiator::CaseInitiator;
pub use case::responder::CaseResponder;
pub use case::signer::{CaseSigner, RingSigner, SignerError};

/// Canonical name for the ECDSA-P256-SHA256 signer trait outside CASE.
///
/// `CaseSigner` is the original name. Outside the CASE handshake, callers
/// should import this re-export — the trait itself is identical.
pub use case::signer::CaseSigner as Signer;
pub use case::{
    CaseCredentials, CaseMessageKind, CaseSessionKeys, CaseSessionOutput, LocalInfo, PeerInfo,
    ResumptionId, ResumptionRecord, Sigma1Outcome,
};
pub use error::{Error, Result};
pub use operational::{
    derive_compressed_fabric_id, derive_group_privacy_key, derive_group_session_id,
    derive_operational_ipk, group_multicast_ipv6,
};
pub use pase::{
    pake_passcode_verifier, PaseMessageKind, PasePbkdfParams, PaseProver, PaseSessionKeys,
    PaseVerifier,
};
pub use session_params::SessionParameters;

/// Fill `buf` with cryptographically secure random bytes (ring `SystemRandom`).
///
/// # Errors
/// Returns [`Error::Rng`] if the system RNG fails.
pub fn random_bytes(buf: &mut [u8]) -> Result<()> {
    use ring::rand::SecureRandom;
    ring::rand::SystemRandom::new()
        .fill(buf)
        .map_err(|_| Error::Rng)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    #[test]
    fn random_bytes_fills_and_varies() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        crate::random_bytes(&mut a).unwrap();
        crate::random_bytes(&mut b).unwrap();
        assert_ne!(a, [0u8; 32]);
        assert_ne!(a, b); // collision probability ~2^-256
    }
}

/// Compile-checks the Rust examples in this crate's `README.md`.
///
/// `#[cfg(doctest)]` means the item exists only while rustdoc is collecting
/// doctests, so the README is compiled by `cargo test --doc` without being
/// duplicated into the rendered crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
