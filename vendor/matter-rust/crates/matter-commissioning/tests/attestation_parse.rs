//! Integration test: parse the vendored CSA test attestation chain
//! fixtures via the public `matter_commissioning::attestation` API.
//!
//! This complements the unit tests in `src/attestation/x509.rs` by
//! exercising the public re-exports and the `tests/` integration-test
//! crate boundary. A regression here means the crate's public API has
//! changed in a way that breaks downstream consumers.

use matter_commissioning::{AttestationError, Dac, Paa, PaaTrustStore, Pai, ProductId, VendorId};

const DAC_DER: &[u8] = include_bytes!(
    "../../../test-vectors/certs/attestation/happy-path/Chip-Test-DAC-FFF1-8000-0004-Cert.der"
);
const PAI_DER: &[u8] = include_bytes!(
    "../../../test-vectors/certs/attestation/happy-path/Chip-Test-PAI-FFF1-8000-Cert.der"
);

#[test]
#[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
fn dac_parses_with_expected_identifiers() {
    let dac = Dac::from_der(DAC_DER).expect("happy-path DAC parses");
    assert_eq!(dac.subject_vid(), VendorId::new(0xFFF1));
    assert_eq!(dac.subject_pid(), ProductId::new(0x8000));
    assert_eq!(dac.public_key().len(), 65);
    assert_eq!(dac.public_key()[0], 0x04);
    assert_eq!(dac.der(), DAC_DER);
}

#[test]
#[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
fn pai_parses_with_expected_identifiers() {
    let pai = Pai::from_der(PAI_DER).expect("happy-path PAI parses");
    assert_eq!(pai.subject_vid(), VendorId::new(0xFFF1));
    assert_eq!(pai.subject_pid(), Some(ProductId::new(0x8000)));
}

#[test]
fn with_example_device_roots_contains_pai_issuer_root() {
    // Sanity: the bundled trust store includes a PAA that COULD chain
    // our test PAI (subject_vid matches). Actual chain validation is
    // M6.2.2 — here we just confirm the trust-store wiring lines up.
    let store = PaaTrustStore::with_example_device_roots();
    let has_matching_vid = store
        .iter()
        .any(|paa| paa.subject_vid() == Some(VendorId::new(0xFFF1)));
    assert!(
        has_matching_vid,
        "bundled CSA test roots include a PAA for VID 0xFFF1"
    );
}

#[test]
fn paa_from_der_rejects_a_dac() {
    // A DAC isn't structurally a valid PAA (DAC subject DN contains a
    // ProductId attribute, which is FORBIDDEN on PAA per Matter §6.5).
    // Confirm Paa::from_der surfaces a parse error rather than
    // silently accepting it.
    let err = Paa::from_der(DAC_DER);
    // We deliberately don't assert WHICH error variant inside Parse —
    // the structural-violation kind depends on which check fires
    // first in Paa::from_der. M6.2.2 will tighten this.
    assert!(matches!(err, Err(AttestationError::Parse(_))));
}
