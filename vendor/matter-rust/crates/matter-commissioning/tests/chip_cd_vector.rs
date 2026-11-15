//! Cross-verification of the CD verifier against real connectedhomeip CDs
//! (chip is the reference implementation; our output must match it).
//!
//! These vectors pin down *which key signs what*, a distinction that cost a
//! live commissioning session to discover: chip's example CDs are not all
//! signed by chip's test CD signing authority. The VID=0xFFF1 CD that every
//! `CONFIG_EXAMPLE_DAC_PROVIDER` device serves — including the esp-matter
//! ESP32-C6 — is signed by the CSA's **production** "CD Signing Key 001".
//! A commissioner trusting only the test authority rejects it, which is exactly
//! what happened on hardware (chip's own `DefaultDeviceAttestationVerifier`
//! trusts the test key *and* the production keys, so chip-tool saw no problem).
//!
//! Vectors, all vendored from a connectedhomeip checkout:
//! * `Chip-Test-CD-FFF2-8001.der` — `credentials/test/certification-declaration/`
//! * `Chip-Test-CD-Signing-Cert.der` — same dir, PEM converted to DER
//! * `c6-cd-fff1.der` — the VID=0xFFF1 branch of `kCdForAllExamples` in
//!   `src/credentials/examples/DeviceAttestationCredsExample.cpp`
//! * `CSA-CD-Signing-Key-001.der` — `credentials/production/cd-certs/`

#![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests with justification (CONTRIBUTING.md).

use matter_commissioning::attestation::cd::{verify_certification_declaration, CdSigningRoots};
use matter_commissioning::attestation::{ProductId, VendorId};

/// chip's real CD for VID 0xFFF2 / PID 0x8001, signed by the test authority.
const CHIP_CD_FFF2_8001: &[u8] = include_bytes!("vectors/Chip-Test-CD-FFF2-8001.der");
/// chip's test CD signing cert (X.509 DER), SKID `62:FA:82:33:…:71:60`.
const CHIP_CD_SIGNING_CERT: &[u8] = include_bytes!("vectors/Chip-Test-CD-Signing-Cert.der");
/// The CD every `ExampleDACProvider` VID=0xFFF1 device serves (the ESP32-C6).
const C6_CD_FFF1: &[u8] = include_bytes!("vectors/c6-cd-fff1.der");
/// CSA production "CD Signing Key 001", SKID `FE:34:3F:95:…:7D:8E`.
const CSA_CD_SIGNING_KEY_001: &[u8] = include_bytes!("vectors/CSA-CD-Signing-Key-001.der");

#[test]
fn verifies_real_chip_cd_against_chip_test_signing_root() {
    let trust = CdSigningRoots::from_cert_der(&[CHIP_CD_SIGNING_CERT])
        .expect("chip test CD signing cert must parse");
    assert_eq!(trust.len(), 1, "trust root must load");

    verify_certification_declaration(
        CHIP_CD_FFF2_8001,
        VendorId::new(0xFFF2),
        ProductId::new(0x8001),
        &trust,
    )
    .expect("a real chip CD must verify against chip's own test CD signing root");
}

/// The exact blob that failed the first live BLE→Thread commission: it verifies
/// only under the CSA production root, not the test authority.
#[test]
fn verifies_esp_matter_c6_cd_against_csa_production_key_001() {
    let trust = CdSigningRoots::from_cert_der(&[CSA_CD_SIGNING_KEY_001])
        .expect("CSA CD signing key 001 cert must parse");

    verify_certification_declaration(
        C6_CD_FFF1,
        VendorId::new(0xFFF1),
        ProductId::new(0x8000),
        &trust,
    )
    .expect("the C6's CD must verify against CSA production CD signing key 001");
}

/// The bundled `with_example_device_roots()` set MUST verify the C6's real
/// CD. It used to bundle only the synthetic root and reject the C6 —
/// the trap `example_device_roots()`'s "suitable for CSA-test devices" doc papered
/// over. This is the check that would have caught it.
#[test]
fn c6_cd_verifies_against_bundled_example_device_roots() {
    let trust = CdSigningRoots::with_example_device_roots();
    assert!(
        trust.len() >= 3,
        "bundled roots must include synthetic + chip-test + CSA-prod-001"
    );

    verify_certification_declaration(
        C6_CD_FFF1,
        VendorId::new(0xFFF1),
        ProductId::new(0x8000),
        &trust,
    )
    .expect("the C6's real CD must verify against the bundled example_device_roots set");
}

/// Guards the trap itself: trusting only the test authority rejects the C6's CD.
/// If this ever starts passing, the example-CD signer changed upstream and the
/// runbook's `--cd-dir` guidance needs revisiting.
#[test]
fn c6_cd_is_rejected_by_test_authority_alone() {
    let trust = CdSigningRoots::from_cert_der(&[CHIP_CD_SIGNING_CERT])
        .expect("chip test CD signing cert must parse");

    let err = verify_certification_declaration(
        C6_CD_FFF1,
        VendorId::new(0xFFF1),
        ProductId::new(0x8000),
        &trust,
    )
    .expect_err("the C6's CD is not signed by the test authority");
    assert!(
        matches!(
            err,
            matter_commissioning::attestation::AttestationError::
                CertificationDeclarationSignatureInvalid
        ),
        "expected a signature-invalid rejection, got {err:?}"
    );
}
