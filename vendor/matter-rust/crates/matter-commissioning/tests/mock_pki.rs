//! Gate test for the mock-device PKI builder.
//!
//! This file's only job is to:
//!
//! 1. Compile `tests/support/mod.rs` by declaring it as a module.
//! 2. Prove that `build_mock_device_pki` produces a chain that the
//!    Commissioner's real `verify_chain` accepts.
//!
//! Everything in `support/mod.rs` is reused by the
//! `commission_loopback.rs` integration test, which declares `mod support;`
//! the same way.
//!
//! If this test fails, fix the chain builder (`tests/support/mod.rs`),
//! NOT `verify_chain` — weakening the verifier defeats the point of
//! attestation.

// `support` uses driver types, so this only compiles with the `driver` feature
// (CI runs `--all-features`; plain `cargo test` skips it cleanly).
#![cfg(feature = "driver")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
// Domain acronyms in comments are prose, not code items.
#![allow(clippy::doc_markdown)]

mod support;

use matter_cert::MatterTime;
use matter_commissioning::attestation::{verify_chain, Dac, Pai, ProductId, VendorId};
use matter_crypto::CaseSigner as _; // bring public_key() into scope for RingSigner

/// Unix timestamp used as "now" for this test. 2027-01-15T08:00:00Z
/// (matches `x509_builder_gate.rs` so the two test files use the same anchor).
const AT_UNIX: u64 = 1_800_000_000;

/// The synthetic chain built by `build_mock_device_pki` must satisfy the real
/// `verify_chain` — the same path used during live commissioning. VID and PID
/// in the returned `ChainVerification` must match the constants in `support`.
#[test]
fn mock_pki_chain_passes_real_verify_chain() {
    let now = MatterTime::from_unix_secs(AT_UNIX);
    let pki = support::build_mock_device_pki(now);

    let dac = Dac::from_der(&pki.dac_der).expect("DAC parses");
    let pai = Pai::from_der(&pki.pai_der).expect("PAI parses");

    // THE GATE: the real verifier, unmodified, must accept the synthetic chain.
    let result = verify_chain(&dac, &pai, &pki.paa_trust_store, now).expect(
        "verify_chain MUST accept the mock-device chain — fix the builder, not the verifier",
    );

    assert_eq!(
        result.vendor_id,
        VendorId::new(support::VID),
        "VID must be 0xFFF1"
    );
    assert_eq!(
        result.product_id,
        ProductId::new(support::PID),
        "PID must be 0x8001"
    );
    assert_eq!(
        result.dac_public_key.len(),
        65,
        "DAC public key must be 65-byte SEC1 uncompressed"
    );
    assert!(
        !result.paa_subject.is_empty(),
        "PAA subject must be non-empty"
    );

    // Verify the DAC signer's public key matches the public key the verifier
    // extracted from the DAC certificate. This proves the signer we returned
    // is the one whose public key is in the chain.
    assert_eq!(
        pki.dac_signer.public_key().as_bytes().as_ref(),
        result.dac_public_key.as_slice(),
        "dac_signer.public_key() must match the key extracted by verify_chain"
    );
}
