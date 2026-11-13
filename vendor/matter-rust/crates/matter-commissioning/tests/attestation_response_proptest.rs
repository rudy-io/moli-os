//! Property tests for `verify_attestation_response`.
//!
//! Layer 4 of the M6.2 testing strategy: randomly-generated
//! `attestation_elements` (1..=256 bytes), random challenges (16
//! bytes), and freshly-minted P-256 keypairs round-trip through
//! sign -> verify. Single-byte mutations of any of the four inputs
//! (sig, challenge, elements, key) must fail with
//! `BadResponseSignature`.
//!
//! `proptest` shrinks failing inputs to a minimal counterexample —
//! useful if a future refactor of the canonical-input composition
//! ever drifts from matter.js (e.g. someone accidentally inserts a
//! length prefix). The shrinker would then surface a tiny
//! (1-byte-elements, 16-byte-challenge) failure that points right at
//! the framing bug.

use matter_commissioning::attestation::{
    verify_attestation_response, AttestationError, AttestationResponse,
};
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use proptest::prelude::*;

/// Build a (pubkey, sig) pair signing `tbs` with the keypair derived
/// from `seed`. `seed` is a single byte placed in the high (MSB)
/// position of the 32-byte big-endian scalar — any non-zero value
/// yields a valid P-256 scalar (well below curve order).
fn sign_with_seed(seed: u8, tbs: &[u8]) -> Result<([u8; 65], [u8; 64]), String> {
    let mut scalar = [0u8; 32];
    scalar[0] = seed.max(1); // avoid the zero scalar
    let signing_key =
        SigningKey::from_slice(&scalar).map_err(|e| format!("scalar->SigningKey: {e}"))?;
    let pk_point = signing_key.verifying_key().to_sec1_point(false);
    let pk_bytes = pk_point.as_bytes();
    let mut pk_arr = [0u8; 65];
    pk_arr.copy_from_slice(pk_bytes);

    let sig: Signature = signing_key.sign(tbs);
    let sig_bytes = sig.to_bytes();
    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(&sig_bytes);

    Ok((pk_arr, sig_arr))
}

proptest! {
    /// Round-trip: sign then verify must always succeed.
    #[test]
    fn sign_then_verify_round_trip(
        elements in proptest::collection::vec(any::<u8>(), 0..=256),
        challenge_bytes: [u8; 16],
        seed in 1u8..=255,
    ) {
        let mut tbs = Vec::with_capacity(elements.len() + 16);
        tbs.extend_from_slice(&elements);
        tbs.extend_from_slice(&challenge_bytes);
        let (pubkey, sig) = sign_with_seed(seed, &tbs).map_err(TestCaseError::fail)?;
        let response = AttestationResponse {
            attestation_elements: elements,
            signature: sig,
        };
        prop_assert!(verify_attestation_response(&response, &challenge_bytes, &pubkey).is_ok());
    }

    /// Any single-bit flip in the signature must invalidate it.
    #[test]
    fn single_bit_signature_flip_fails(
        elements in proptest::collection::vec(any::<u8>(), 1..=128),
        challenge_bytes: [u8; 16],
        seed in 1u8..=255,
        flip_byte in 0usize..64,
        flip_bit in 0u8..8,
    ) {
        let mut tbs = Vec::with_capacity(elements.len() + 16);
        tbs.extend_from_slice(&elements);
        tbs.extend_from_slice(&challenge_bytes);
        let (pubkey, mut sig) = sign_with_seed(seed, &tbs).map_err(TestCaseError::fail)?;
        sig[flip_byte] ^= 1 << flip_bit;
        let response = AttestationResponse {
            attestation_elements: elements,
            signature: sig,
        };
        let result = verify_attestation_response(&response, &challenge_bytes, &pubkey);
        prop_assert!(matches!(result, Err(AttestationError::BadResponseSignature)));
    }

    /// Any single-bit flip in the challenge must invalidate.
    #[test]
    fn single_bit_challenge_flip_fails(
        elements in proptest::collection::vec(any::<u8>(), 1..=128),
        challenge_bytes: [u8; 16],
        seed in 1u8..=255,
        flip_byte in 0usize..16,
        flip_bit in 0u8..8,
    ) {
        let mut tbs = Vec::with_capacity(elements.len() + 16);
        tbs.extend_from_slice(&elements);
        tbs.extend_from_slice(&challenge_bytes);
        let (pubkey, sig) = sign_with_seed(seed, &tbs).map_err(TestCaseError::fail)?;
        let mut tampered = challenge_bytes;
        tampered[flip_byte] ^= 1 << flip_bit;
        let response = AttestationResponse {
            attestation_elements: elements,
            signature: sig,
        };
        let result = verify_attestation_response(&response, &tampered, &pubkey);
        prop_assert!(matches!(result, Err(AttestationError::BadResponseSignature)));
    }

    /// Any single-byte mutation of the elements must invalidate.
    #[test]
    fn single_byte_elements_mutation_fails(
        elements in proptest::collection::vec(any::<u8>(), 1..=128),
        challenge_bytes: [u8; 16],
        seed in 1u8..=255,
        flip_idx in any::<proptest::sample::Index>(),
        xor_mask in 1u8..=255,
    ) {
        let mut tbs = Vec::with_capacity(elements.len() + 16);
        tbs.extend_from_slice(&elements);
        tbs.extend_from_slice(&challenge_bytes);
        let (pubkey, sig) = sign_with_seed(seed, &tbs).map_err(TestCaseError::fail)?;
        let idx = flip_idx.index(elements.len());
        let mut tampered_elements = elements.clone();
        tampered_elements[idx] ^= xor_mask;
        let response = AttestationResponse {
            attestation_elements: tampered_elements,
            signature: sig,
        };
        let result = verify_attestation_response(&response, &challenge_bytes, &pubkey);
        prop_assert!(matches!(result, Err(AttestationError::BadResponseSignature)));
    }
}
