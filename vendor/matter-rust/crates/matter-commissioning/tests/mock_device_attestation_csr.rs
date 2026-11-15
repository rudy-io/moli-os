//! Gate test for the mock-device attestation + CSR response builders.
//!
//! Proves that the outputs of `support::build_attestation_response` and
//! `support::build_csr_response` are accepted by the Commissioner's REAL
//! unmodified verifiers (`verify_attestation_response` and
//! `verify_csr_response`).  If either assertion fails, fix the builder in
//! `tests/support/mod.rs`, NOT the verifier.

// `support` uses driver types, so this only compiles with the `driver` feature
// (CI runs `--all-features`; plain `cargo test` skips it cleanly).
#![cfg(feature = "driver")]
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]
// Domain acronyms are prose, not code items.
#![allow(clippy::doc_markdown)]

mod support;

use matter_cert::MatterTime;
use matter_commissioning::attestation::{verify_attestation_response, AttestationResponse};
use matter_commissioning::verify_csr_response;
use matter_crypto::CaseSigner as _; // bring public_key() into scope

/// Same Unix anchor used in `mock_pki.rs` so validity windows bracket "now".
const AT_UNIX: u64 = 1_800_000_000;

/// The REAL `verify_attestation_response` must accept the output of
/// `build_attestation_response` when given the same nonce and challenge.
#[test]
fn real_verifier_accepts_mock_attestation_response() {
    let now = MatterTime::from_unix_secs(AT_UNIX);
    let pki = support::build_mock_device_pki(now);

    let cd_bytes = support::load_cd_fixture();
    let nonce = [0xA1_u8; 32];
    let challenge = [0xB2_u8; 16];

    let response: AttestationResponse =
        support::build_attestation_response(&cd_bytes, nonce, challenge, &pki.dac_signer);

    // The REAL verifier — unmodified — must accept.
    let dac_pub = pki.dac_signer.public_key();
    verify_attestation_response(&response, &challenge, dac_pub.as_bytes().as_ref())
        .expect("REAL verify_attestation_response must accept the mock device's response");
}

/// The REAL `verify_csr_response` must accept the output of
/// `build_csr_response` when given the same nonce and challenge.
#[test]
fn real_verifier_accepts_mock_csr_response() {
    let now = MatterTime::from_unix_secs(AT_UNIX);
    let pki = support::build_mock_device_pki(now);

    let csr_nonce = [0xC3_u8; 32];
    let challenge = [0xD4_u8; 16];

    let csr_resp = support::build_csr_response(csr_nonce, challenge, &pki.dac_signer);

    let dac_pub = pki.dac_signer.public_key();
    let verified = verify_csr_response(
        &csr_resp.nocsr_elements,
        &csr_resp.attestation_signature,
        &csr_nonce,
        &challenge,
        dac_pub.as_bytes().as_ref(),
    )
    .expect("REAL verify_csr_response must accept the mock device's CSR response");

    // Sanity: the returned public key is the CSR's embedded operational key,
    // not the DAC key (they are different key pairs).
    assert_ne!(
        verified.public_key.as_bytes().as_ref(),
        dac_pub.as_bytes().as_ref(),
        "CSR's operational key must differ from the DAC key"
    );
}

/// Cross-check: wrong challenge causes `verify_attestation_response` to reject.
/// Ensures the builder correctly binds the challenge into the signature.
#[test]
fn wrong_challenge_is_rejected_by_real_attestation_verifier() {
    let now = MatterTime::from_unix_secs(AT_UNIX);
    let pki = support::build_mock_device_pki(now);

    let cd_bytes = support::load_cd_fixture();
    let nonce = [0xE5_u8; 32];
    let challenge = [0xF6_u8; 16];

    let response =
        support::build_attestation_response(&cd_bytes, nonce, challenge, &pki.dac_signer);

    let wrong_challenge = [0x00_u8; 16];
    let dac_pub = pki.dac_signer.public_key();
    let err = verify_attestation_response(&response, &wrong_challenge, dac_pub.as_bytes().as_ref())
        .expect_err("wrong challenge must be rejected");
    assert!(
        matches!(
            err,
            matter_commissioning::AttestationError::BadResponseSignature
        ),
        "expected BadResponseSignature, got {err:?}"
    );
}

/// Cross-check: wrong nonce causes `verify_csr_response` to reject.
/// Ensures the builder binds the nonce into nocsr_elements (echoed by the device).
#[test]
fn wrong_nonce_is_rejected_by_real_csr_verifier() {
    let now = MatterTime::from_unix_secs(AT_UNIX);
    let pki = support::build_mock_device_pki(now);

    let csr_nonce = [0x11_u8; 32];
    let challenge = [0x22_u8; 16];

    let csr_resp = support::build_csr_response(csr_nonce, challenge, &pki.dac_signer);

    let wrong_nonce = [0xFF_u8; 32];
    let dac_pub = pki.dac_signer.public_key();
    let err = verify_csr_response(
        &csr_resp.nocsr_elements,
        &csr_resp.attestation_signature,
        &wrong_nonce,
        &challenge,
        dac_pub.as_bytes().as_ref(),
    )
    .expect_err("wrong nonce must be rejected");
    assert!(
        matches!(err, matter_commissioning::NocError::NonceMismatch),
        "expected NonceMismatch, got {err:?}"
    );
}
