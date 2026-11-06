//! Pluggable ECDSA-P256-SHA256 signer for CASE.
//!
//! M4.1: `SignerError`, `CaseSigner` trait, and `RingSigner` (the in-tree
//! production-default implementation).
//!
//! # Signing algorithm
//!
//! `RingSigner::sign_p256_sha256` uses the `p256` crate's ECDSA implementation,
//! which follows RFC 6979 deterministic nonce generation. This is the same
//! algorithm used by `@noble/curves` (JavaScript) and enables byte-for-byte
//! reproducible test vectors captured by `cargo xtask capture-case`.
//!
//! `ring`'s `EcdsaKeyPair::sign` uses a hedged variant that mixes in random
//! bytes, making signatures non-deterministic. We still use `ring` for key
//! generation and public-key parsing (where it excels), but switch to `p256`
//! for the signing step itself so that CASE byte-parity tests are possible.

use matter_cert::PublicKey;
use p256::ecdsa::{signature::Signer as EcdsaSigner, Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;

use crate::error::{Error, Result};

/// Errors returned by a [`CaseSigner`] implementation.
///
/// This type is intentionally `#[non_exhaustive]` so that future backends
/// (HSMs, software key stores, OS keychain) can add variants without breaking
/// callers that only handle the existing arms.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SignerError {
    /// Signer hardware is not available (e.g., HSM disconnected).
    #[error("signer hardware unavailable")]
    Unavailable,
    /// Signer explicitly rejected the operation (e.g., policy violation).
    #[error("signer rejected the operation: {0}")]
    Rejected(&'static str),
    /// Internal signer error (e.g., ring returned an opaque failure).
    #[error("internal signer error")]
    Internal,
}

/// Pluggable ECDSA-P256-SHA256 signer for CASE.
///
/// The in-tree concrete implementation is [`RingSigner`], which wraps
/// a P-256 private key and produces RFC 6979 deterministic signatures.
/// Alternative backends (HSM, OS keychain, software key store) can implement
/// this trait without touching core CASE code.
pub trait CaseSigner: Send + Sync + std::fmt::Debug {
    /// Sign `message` with the NOC's private ECDSA-P256 key.
    /// Returns raw 64-byte r||s signature (Matter wire format).
    ///
    /// # Errors
    ///
    /// Returns [`SignerError`] if the signing operation fails (hardware
    /// unavailable, policy rejection, or internal error).
    fn sign_p256_sha256(&self, message: &[u8]) -> std::result::Result<[u8; 64], SignerError>;

    /// The 65-byte SEC1-uncompressed P-256 public key matching the NOC.
    fn public_key(&self) -> &PublicKey;
}

// ── RingSigner ────────────────────────────────────────────────────────────────

use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

/// [`CaseSigner`] backed by the `p256` crate's RFC 6979 deterministic ECDSA.
///
/// The name `RingSigner` is kept for API stability (this type was introduced in
/// M4.1). Key generation still uses `ring` (`[Self::generate]`), but signing
/// uses `p256::ecdsa::SigningKey` (RFC 6979 deterministic) and the public key
/// is derived from the same `p256` signing key. This enables byte-for-byte
/// test-vector parity with `@noble/curves` (JavaScript) as captured by
/// `cargo xtask capture-case`.
///
/// Use [`Self::from_pkcs8`] to load existing keys (e.g., from a fabric
/// store), or [`Self::generate`] in tests to mint a fresh keypair.
pub struct RingSigner {
    /// RFC 6979 deterministic signing key (p256 crate).
    signing_key: SigningKey,
    public_key: PublicKey,
}

impl std::fmt::Debug for RingSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RingSigner")
            .field("public_key", &self.public_key)
            .field("signing_key", &"<p256::ecdsa::SigningKey>")
            .finish_non_exhaustive()
    }
}

impl RingSigner {
    /// Construct from PKCS#8 v1 encoded private key bytes.
    ///
    /// Parses the PKCS#8 DER bytes using `p256::ecdsa::SigningKey`, which
    /// also derives the matching public key. The 65-byte SEC1-uncompressed
    /// public key is extracted from the `p256` verifying key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SigningFailed`] with [`SignerError::Internal`] if the
    /// bytes are not a valid PKCS#8-encoded P-256 key.
    pub fn from_pkcs8(pkcs8_bytes: &[u8]) -> Result<Self> {
        let signing_key = SigningKey::from_pkcs8_der(pkcs8_bytes)
            .map_err(|_| Error::SigningFailed(SignerError::Internal))?;

        // Derive the 65-byte SEC1-uncompressed public key from the signing key.
        // `to_sec1_point` is the p256 0.14 name for 0.13's `to_encoded_point`;
        // same SEC1 encoding, same `false = uncompressed` flag.
        let encoded = signing_key.verifying_key().to_sec1_point(false);
        let encoded_bytes = encoded.as_bytes();
        if encoded_bytes.len() != 65 {
            return Err(Error::SigningFailed(SignerError::Internal));
        }
        let mut pk_bytes = [0u8; 65];
        pk_bytes.copy_from_slice(encoded_bytes);
        let public_key =
            PublicKey::new(pk_bytes).map_err(|_| Error::SigningFailed(SignerError::Internal))?;

        Ok(Self {
            signing_key,
            public_key,
        })
    }

    /// Generate a fresh ECDSA-P256 keypair. Returns the signer plus the
    /// PKCS#8 bytes so the caller can persist them.
    ///
    /// Key generation uses `ring`'s `EcdsaKeyPair::generate_pkcs8` for its
    /// well-audited RNG plumbing. The resulting PKCS#8 is then loaded via
    /// `from_pkcs8` so that signing uses the deterministic `p256` path.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SigningFailed`] with [`SignerError::Internal`] if the
    /// OS RNG or key-generation step fails (extremely unlikely in practice).
    pub fn generate() -> Result<(Self, Vec<u8>)> {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| Error::SigningFailed(SignerError::Internal))?;
        let pkcs8_vec = pkcs8.as_ref().to_vec();
        let signer = Self::from_pkcs8(&pkcs8_vec)?;
        Ok((signer, pkcs8_vec))
    }
}

impl CaseSigner for RingSigner {
    /// Sign `message` using RFC 6979 deterministic ECDSA-P256-SHA256.
    ///
    /// Uses `p256::ecdsa::SigningKey::sign` which follows RFC 6979 for nonce
    /// generation (same algorithm as `@noble/curves` in JavaScript), enabling
    /// byte-for-byte reproducible test vectors captured by
    /// `cargo xtask capture-case`.
    ///
    /// The signature is low-s normalized (`s ≤ n/2`), matching `@noble/curves`
    /// behavior. Both `s` and `n - s` are valid ECDSA signatures; this ensures
    /// the Rust output is byte-identical with matter.js.
    ///
    /// The returned signature is in IEEE P1363 compact format (raw r||s, 64 bytes),
    /// which is the Matter wire format for ECDSA signatures.
    ///
    /// # Errors
    ///
    /// Returns [`SignerError::Internal`] on signing failure (not expected for
    /// valid keys and any message length).
    fn sign_p256_sha256(&self, message: &[u8]) -> std::result::Result<[u8; 64], SignerError> {
        // `Signer::sign` hashes `message` with SHA-256 internally (the `sha256`
        // feature of the `p256` crate wires this up via `ecdsa::hazmat::SignPrimitive`
        // + `DigestSigner<sha2::Sha256, _>`). The returned `Signature` is in the
        // compact (IEEE P1363) format: 32-byte big-endian r || 32-byte big-endian s.
        let sig: Signature = self.signing_key.sign(message);
        // Apply low-s normalization (s → n - s when s > n/2).
        //
        // `@noble/curves` always produces low-s signatures (s ≤ n/2).
        // `p256::ecdsa::SigningKey::sign` uses RFC 6979 for nonce generation but
        // does NOT guarantee low-s — it may produce s > n/2 for some keys and
        // messages. Both forms are mathematically equivalent and valid per ECDSA,
        // but byte-parity tests require the same representation as matter.js.
        //
        // `Signature::normalize_s()` flips s to n - s when s > n/2 and returns
        // the signature unchanged when s is already low. (In p256 0.13 it
        // returned `Option<Signature>` — `Some` only on the flip — so the call
        // site read `sig.normalize_s().unwrap_or(sig)`. p256 0.14 returns
        // `Signature` directly, folding the "already low" case in. Same result
        // in both branches.)
        let sig = sig.normalize_s();
        // `Signature::to_bytes()` returns the compact 64-byte r||s byte array.
        Ok(sig.to_bytes().into())
    }

    fn public_key(&self) -> &PublicKey {
        &self.public_key
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_round_trip_through_pkcs8() {
        let (signer1, pkcs8) = RingSigner::generate().unwrap();
        let signer2 = RingSigner::from_pkcs8(&pkcs8).unwrap();
        assert_eq!(
            signer1.public_key().as_bytes(),
            signer2.public_key().as_bytes()
        );
    }

    #[test]
    fn signing_produces_64_byte_signature() {
        let (signer, _) = RingSigner::generate().unwrap();
        let sig = signer.sign_p256_sha256(b"hello world").unwrap();
        assert_eq!(sig.len(), 64);
    }

    #[test]
    fn signing_same_message_twice_is_deterministic() {
        // p256's ECDSA uses RFC 6979 deterministic nonce generation, so signing
        // the same message twice produces identical signatures. This property is
        // required for byte-parity test vectors captured by `cargo xtask capture-case`.
        let (signer, _) = RingSigner::generate().unwrap();
        let a = signer.sign_p256_sha256(b"same").unwrap();
        let b = signer.sign_p256_sha256(b"same").unwrap();
        assert_eq!(a, b, "RFC 6979 signing must be deterministic");
    }

    #[test]
    fn signing_produces_verifiable_signature() {
        use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};
        let (signer, _) = RingSigner::generate().unwrap();
        let msg = b"verify me";
        let sig = signer.sign_p256_sha256(msg).unwrap();
        let pk = UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, signer.public_key().as_bytes());
        pk.verify(msg, &sig).expect("signature must verify");
    }

    #[test]
    fn pkcs8_from_invalid_bytes_returns_error() {
        let err = RingSigner::from_pkcs8(b"garbage").unwrap_err();
        assert!(matches!(err, Error::SigningFailed(SignerError::Internal)));
    }

    #[test]
    fn signing_produces_low_s_signature() {
        // Signatures must have s ≤ n/2 (low-s normalized) to match @noble/curves output.
        // P-256 curve order n split into two u128 halves (big-endian):
        // n = 0xFFFFFFFF_00000000_FFFFFFFF_FFFFFFFF_BCE6FAAD_A7179E84_F3B9CAC2_FC632551
        let n_hi = 0xFFFF_FFFF_0000_0000_FFFF_FFFF_FFFF_FFFFu128;
        let n_lo = 0xBCE6_FAAD_A717_9E84_F3B9_CAC2_FC63_2551u128;

        let (signer, _) = RingSigner::generate().unwrap();
        // Sign several messages to exercise different (r, s) pairs.
        for msg in &[b"alpha" as &[u8], b"beta", b"gamma", b"delta", b"epsilon"] {
            let sig = signer.sign_p256_sha256(msg).unwrap();
            let s_hi = u128::from_be_bytes(sig[32..48].try_into().unwrap());
            let s_lo = u128::from_be_bytes(sig[48..64].try_into().unwrap());
            // n/2 = (n - 1) / 2; s ≤ n/2 iff 2*s ≤ n. Perform a 256-bit comparison.
            // n is odd so n/2 = 0x7FFF...DE73... We check s_hi, then s_lo.
            let half_hi = (n_hi >> 1) | ((n_lo >> 127) << 127); // carry bit from lo to hi
            let half_lo = n_lo >> 1;
            let is_low_s = s_hi < half_hi || (s_hi == half_hi && s_lo <= half_lo);
            assert!(
                is_low_s,
                "signature s must be ≤ n/2 (low-s) for message {msg:?}: s = {s_hi:016x}{s_lo:016x}",
            );
        }
    }
}
