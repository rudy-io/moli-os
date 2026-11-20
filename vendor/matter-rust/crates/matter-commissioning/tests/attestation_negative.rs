//! Integration test: 8-row negative-path matrix.
//!
//! Each row loads a (PAA, PAI, DAC) triple from
//! `test-vectors/certs/attestation/negative/<name>/`, builds a
//! `PaaTrustStore` that's either anchor-matched or deliberately not,
//! and asserts `verify_chain` returns the `AttestationError` variant
//! that failure maps to.
//!
//! The 8 fixtures form a negative-path matrix: one per chain-validation
//! failure mode, each paired with the variant it must yield.

// dac / pai / paa / paa_bytes / pai_bytes / dac_bytes are
// spec-canonical role names from Matter §6.2.3 (Device Attestation
// Certificate / Product Attestation Intermediate / Product
// Attestation Authority). They are the entire vocabulary of this
// test file; renaming for clippy's similar-names heuristic would
// obscure the role-based pattern that makes each test
// straightforward to read against the spec.
#![allow(clippy::similar_names)]

use matter_cert::time::MatterTime;
use matter_commissioning::{
    verify_chain, AttestationError, Dac, Paa, PaaTrustStore, Pai, VendorId,
};

/// Matches `AT_UNIX` in `scripts/gen-negative-fixtures.py`. Changing
/// one requires changing the other AND regenerating the fixtures.
const AT_UNIX: u64 = 1_800_000_000;

#[allow(clippy::expect_used, clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
fn load_fixture(name: &str) -> (Dac, Pai, Paa) {
    let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-vectors")
        .join("certs")
        .join("attestation")
        .join("negative")
        .join(name);
    let paa_bytes = std::fs::read(base.join("paa.der")).expect("paa.der readable");
    let pai_bytes = std::fs::read(base.join("pai.der")).expect("pai.der readable");
    let dac_bytes = std::fs::read(base.join("dac.der")).expect("dac.der readable");
    (
        Dac::from_der(&dac_bytes).expect("dac parses"),
        Pai::from_der(&pai_bytes).expect("pai parses"),
        Paa::from_der(&paa_bytes).expect("paa parses"),
    )
}

fn store_with(paa: Paa) -> PaaTrustStore {
    let mut s = PaaTrustStore::empty();
    s.add(paa);
    s
}

fn at() -> MatterTime {
    MatterTime::from_unix_secs(AT_UNIX)
}

// ---- the eight rows -------------------------------------------------

#[test]
fn expired_dac_yields_time_bounds_violation() {
    let (dac, pai, paa) = load_fixture("expired-dac");
    let result = verify_chain(&dac, &pai, &store_with(paa), at());
    assert!(
        matches!(result, Err(AttestationError::TimeBoundsViolation)),
        "expected TimeBoundsViolation, got {result:?}"
    );
}

#[test]
fn not_yet_valid_dac_yields_time_bounds_violation() {
    let (dac, pai, paa) = load_fixture("not-yet-valid-dac");
    let result = verify_chain(&dac, &pai, &store_with(paa), at());
    assert!(
        matches!(result, Err(AttestationError::TimeBoundsViolation)),
        "expected TimeBoundsViolation, got {result:?}"
    );
}

#[test]
fn broken_dac_sig_yields_invalid_chain() {
    let (dac, pai, paa) = load_fixture("broken-dac-sig");
    let result = verify_chain(&dac, &pai, &store_with(paa), at());
    assert!(
        matches!(result, Err(AttestationError::InvalidChain(_))),
        "expected InvalidChain, got {result:?}"
    );
}

#[test]
fn broken_pai_sig_yields_invalid_chain() {
    let (dac, pai, paa) = load_fixture("broken-pai-sig");
    let result = verify_chain(&dac, &pai, &store_with(paa), at());
    assert!(
        matches!(result, Err(AttestationError::InvalidChain(_))),
        "expected InvalidChain, got {result:?}"
    );
}

#[test]
fn wrong_vid_dac_yields_vid_mismatch() {
    let (dac, pai, paa) = load_fixture("wrong-vid-dac");
    let result = verify_chain(&dac, &pai, &store_with(paa), at());
    match result {
        Err(AttestationError::VidMismatch { dac, pai }) => {
            assert_eq!(dac, VendorId::new(0xFFF2));
            assert_eq!(pai, VendorId::new(0xFFF1));
        }
        other => panic!("expected VidMismatch, got {other:?}"),
    }
}

#[test]
fn untrusted_paa_yields_untrusted_root() {
    let (dac, pai, _fixture_paa) = load_fixture("untrusted-paa");
    // Trust store has a DIFFERENT synthetic PAA — the one from the
    // expired-dac fixture (independently generated, so its public
    // key differs from the untrusted-paa fixture's PAA).
    let (_, _, other_paa) = load_fixture("expired-dac");
    let result = verify_chain(&dac, &pai, &store_with(other_paa), at());
    assert!(
        matches!(result, Err(AttestationError::UntrustedRoot)),
        "expected UntrustedRoot, got {result:?}"
    );
}

#[test]
fn dac_with_ca_bit_yields_basic_constraints_violation() {
    let (dac, pai, paa) = load_fixture("dac-with-ca-bit");
    let result = verify_chain(&dac, &pai, &store_with(paa), at());
    assert!(
        matches!(result, Err(AttestationError::BasicConstraintsViolation)),
        "expected BasicConstraintsViolation, got {result:?}"
    );
}

#[test]
fn wrong_eku_yields_invalid_chain() {
    // Note: a "missing-eku" fixture would not test this. webpki (correctly,
    // per RFC 5280 §4.2.1.12) treats an *absent* EKU extension as
    // unconstrained — no required-EKU check triggers. So this fixture
    // instead has an EKU extension whose contents are wrong:
    // id-kp-serverAuth rather than id-kp-clientAuth. webpki's
    // `KeyUsage::client_auth()` rejects with `RequiredEkuNotFound`,
    // which our map_webpki_error long-tails into `InvalidChain`.
    let (dac, pai, paa) = load_fixture("wrong-eku");
    let result = verify_chain(&dac, &pai, &store_with(paa), at());
    assert!(
        matches!(result, Err(AttestationError::InvalidChain(_))),
        "expected InvalidChain, got {result:?}"
    );
}
