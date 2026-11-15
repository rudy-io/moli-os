//! Matter EC public key (P-256, uncompressed point).
//!
//! M2.1 ships only the byte container; M2.2 adds signature verification
//! via `ring`.

use core::fmt;

use crate::error::{Error, Result};

/// A 65-byte uncompressed P-256 public key: `0x04 || X(32) || Y(32)`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; 65]);

impl PublicKey {
    /// Construct from raw bytes. Validates the `0x04` uncompressed-point
    /// prefix; rejects compressed or hybrid encodings.
    ///
    /// # Errors
    ///
    /// Returns [`Error::BadPublicKeyPrefix`] if `bytes[0] != 0x04`.
    pub fn new(bytes: [u8; 65]) -> Result<Self> {
        if bytes[0] != 0x04 {
            return Err(Error::BadPublicKeyPrefix);
        }
        Ok(Self(bytes))
    }

    /// Construct from a byte slice. Rejects wrong-length input.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongPublicKeyLength`] if `slice.len() != 65`,
    /// or [`Error::BadPublicKeyPrefix`] if the first byte is not `0x04`.
    pub fn from_slice(slice: &[u8]) -> Result<Self> {
        let bytes: [u8; 65] = slice
            .try_into()
            .map_err(|_| Error::WrongPublicKeyLength(slice.len()))?;
        Self::new(bytes)
    }

    /// The raw 65-byte uncompressed point.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 65] {
        &self.0
    }

    /// Verify an ECDSA-P256-SHA256 signature against a message.
    ///
    /// The signature is the 64-byte raw `r || s` form; ring's
    /// `ECDSA_P256_SHA256_FIXED` algorithm consumes exactly this layout
    /// and computes the SHA-256 hash internally.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SignatureVerificationFailed`] if the signature
    /// is not a valid signature over `message` by the private key
    /// corresponding to this public key. The error variant does NOT
    /// distinguish between malformed signatures, wrong-key signatures,
    /// and signatures over different messages — ring deliberately
    /// keeps this opaque to avoid side-channel leaks.
    pub fn verify(&self, message: &[u8], signature: &crate::signature::Signature) -> Result<()> {
        use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};
        let key = UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &self.0[..]);
        key.verify(message, signature.as_bytes())
            .map_err(|_| Error::SignatureVerificationFailed)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PublicKey({:02x}{:02x}{:02x}{:02x}…)",
            self.0[1], self.0[2], self.0[3], self.0[4]
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

    use super::*;
    use crate::signature::Signature;

    /// Generate a fresh test keypair via ring. Returns the public key
    /// (in our `PublicKey` newtype) and the key pair (for signing).
    fn make_keypair() -> (PublicKey, EcdsaKeyPair) {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap();
        let our_pub = PublicKey::from_slice(key_pair.public_key().as_ref()).unwrap();
        (our_pub, key_pair)
    }

    fn sign(key_pair: &EcdsaKeyPair, message: &[u8]) -> Signature {
        let rng = SystemRandom::new();
        let sig = key_pair.sign(&rng, message).unwrap();
        Signature::from_slice(sig.as_ref()).unwrap()
    }

    #[test]
    fn verify_accepts_correct_signature() {
        let (pub_key, key_pair) = make_keypair();
        let message = b"matter-cert phase 2 test message";
        let sig = sign(&key_pair, message);
        assert!(pub_key.verify(message, &sig).is_ok());
    }

    #[test]
    fn verify_rejects_signature_from_different_key() {
        let (_, key_a) = make_keypair();
        let (pub_b, _) = make_keypair();
        let message = b"signed by A, verified against B";
        let sig = sign(&key_a, message);
        let err = pub_b.verify(message, &sig).unwrap_err();
        assert!(matches!(err, Error::SignatureVerificationFailed));
    }

    #[test]
    fn verify_rejects_signature_for_different_message() {
        let (pub_key, key_pair) = make_keypair();
        let sig = sign(&key_pair, b"signed message");
        let err = pub_key.verify(b"different message", &sig).unwrap_err();
        assert!(matches!(err, Error::SignatureVerificationFailed));
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        let (pub_key, key_pair) = make_keypair();
        let message = b"this is the original message";
        let mut sig = sign(&key_pair, message);
        let mut raw = *sig.as_bytes();
        raw[0] ^= 0x01;
        sig = Signature::from_slice(&raw).unwrap();
        let err = pub_key.verify(message, &sig).unwrap_err();
        assert!(matches!(err, Error::SignatureVerificationFailed));
    }

    #[test]
    fn new_rejects_non_0x04_prefix() {
        let mut bytes = [0u8; 65];
        bytes[0] = 0x02;
        assert!(matches!(
            PublicKey::new(bytes),
            Err(Error::BadPublicKeyPrefix)
        ));
    }

    #[test]
    fn new_accepts_0x04_prefix() {
        let mut bytes = [0u8; 65];
        bytes[0] = 0x04;
        bytes[1] = 0xAB;
        let key = PublicKey::new(bytes).unwrap();
        assert_eq!(key.as_bytes(), &bytes);
    }

    #[test]
    fn from_slice_rejects_wrong_length() {
        let short = [0x04u8; 10];
        assert!(matches!(
            PublicKey::from_slice(&short),
            Err(Error::WrongPublicKeyLength(10))
        ));
    }

    #[test]
    fn debug_format_does_not_leak_full_key() {
        let mut bytes = [0u8; 65];
        bytes[0] = 0x04;
        bytes[1] = 0xAB;
        bytes[2] = 0xCD;
        bytes[3] = 0xEF;
        bytes[4] = 0x12;
        let key = PublicKey::new(bytes).unwrap();
        let s = format!("{key:?}");
        assert!(s.contains("abcdef12"));
        assert!(!s.contains("00000000"));
    }
}
