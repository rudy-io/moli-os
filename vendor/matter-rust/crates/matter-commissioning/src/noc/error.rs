//! Error type, RNG abstraction, and `ring`-backed default RNG for the
//! `noc` module.

#![forbid(unsafe_code)]

use thiserror::Error;

/// Errors produced by `matter-commissioning::noc`.
///
/// Variants are coarse-grained — each maps to a distinct caller-side
/// remediation path, and no finer: a misbehaving device must not be able to
/// use the errors it provokes to probe which check failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NocError {
    /// The NOCSR TLV outer envelope was malformed.
    #[error("NOCSR TLV parse failure")]
    NocsrParse(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// The embedded PKCS#10 CSR could not be parsed.
    #[error("embedded PKCS#10 CSR parse failure")]
    CsrParse(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// The PKCS#10 CSR's self-signature did not verify.
    #[error("CSR self-signature verification failed")]
    BadCsrSelfSignature,

    /// The `CSRNonce` echoed by the device did not match the value the
    /// commissioner sent in `CSRRequest`.
    #[error("CSRNonce did not match commissioner-issued value")]
    NonceMismatch,

    /// The DAC's signature over `NOCSR_elements || attestation_challenge`
    /// did not verify. Coarse by design: it does not say why.
    #[error("DAC attestation signature over NOCSR failed")]
    BadCsrAttestationSignature,

    /// The CSR's public key was not a valid P-256 SEC1 uncompressed point.
    #[error("CSR public key is not a valid P-256 point")]
    InvalidCsrPublicKey,

    /// A Matter DN attribute (e.g., a `CaseAuthenticatedTag`) could not be
    /// constructed from the caller-supplied values.
    #[error("Matter DN attribute construction failed")]
    DnAttributeOverflow,

    /// NOC certificate construction via the matter-cert builder failed.
    #[error("NOC certificate construction failed")]
    CertBuild(#[source] matter_cert::Error),

    /// The fabric's root signer rejected the signing operation.
    #[error("NOC signing failed")]
    SigningFailed(#[source] matter_crypto::SignerError),

    /// The system RNG (or a caller-supplied stub) failed.
    #[error("RNG failure")]
    Rng,

    /// An `OpCreds` cluster command codec failed.
    #[error("OpCreds cluster payload codec error")]
    ClusterCodec(#[source] matter_codec::Error),

    /// An `OpCreds` cluster response was *structurally* malformed — the TLV
    /// itself decoded, but its shape violated the expected schema (wrong
    /// container kind / tag, duplicate field, a fixed-width field with the
    /// wrong length, or a required field absent).
    ///
    /// Distinct from [`NocError::ClusterCodec`], which wraps a low-level codec
    /// failure (e.g. a truncated buffer). A structural mismatch must not be
    /// reported as a codec EOF — the carried `&'static str` names what was
    /// expected so callers and logs get an accurate label.
    #[error("OpCreds cluster response is structurally malformed: {0}")]
    MalformedResponse(&'static str),
}

impl From<matter_codec::Error> for NocError {
    fn from(e: matter_codec::Error) -> Self {
        Self::ClusterCodec(e)
    }
}

/// Pluggable random-byte source used by `noc` for `CSRNonce`, NOC serial,
/// IPK, and any future secret-material draw. Caller-supplied so tests
/// can pass deterministic stubs.
pub trait NocRng: Send + Sync + std::fmt::Debug {
    /// Fill `dest` with cryptographically secure random bytes.
    ///
    /// # Errors
    ///
    /// Returns [`NocError::Rng`] if the underlying RNG fails. For
    /// `SystemNocRng` this is effectively never; for caller-supplied
    /// implementations it may surface IO or device errors.
    fn fill(&self, dest: &mut [u8]) -> Result<(), NocError>;
}

/// Production default: wraps [`ring::rand::SystemRandom`].
#[derive(Debug, Default)]
pub struct SystemNocRng;

impl NocRng for SystemNocRng {
    fn fill(&self, dest: &mut [u8]) -> Result<(), NocError> {
        use ring::rand::SecureRandom;
        ring::rand::SystemRandom::new()
            .fill(dest)
            .map_err(|_| NocError::Rng)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    #[test]
    fn system_rng_fills_distinct_bytes() {
        // Two draws of 32 bytes each must (with overwhelming probability)
        // differ — not a strict guarantee but a useful smoke test that
        // SystemNocRng is wired to a real entropy source.
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        SystemNocRng.fill(&mut a).unwrap();
        SystemNocRng.fill(&mut b).unwrap();
        assert_ne!(a, b, "two random draws collided — RNG is not wired");
        assert_ne!(a, [0u8; 32], "RNG returned all zeros");
    }
}
