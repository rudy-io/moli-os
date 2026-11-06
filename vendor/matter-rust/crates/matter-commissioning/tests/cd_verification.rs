//! M6.4.3 — `verify_certification_declaration` against synthetic CSA
//! test fixtures captured by `cargo xtask capture-cd`.

// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use base64::Engine;
use matter_commissioning::{
    verify_certification_declaration, AttestationError, CdSigningRoots, ProductId, VendorId,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    cd_b64: String,
    expected_vid: u16,
    expected_pid: u16,
}

fn fixture_path(name: &str) -> PathBuf {
    let mut p: PathBuf = env!("CARGO_MANIFEST_DIR").into();
    p.push("..");
    p.push("..");
    p.push("test-vectors");
    p.push("commissioning");
    p.push("cd");
    p.push(name);
    p
}

fn load(name: &str) -> Fixture {
    let path = fixture_path(name);
    let raw = std::fs::read_to_string(&path).expect("fixture present");
    serde_json::from_str(&raw).expect("fixture parses as JSON")
}

fn cd_bytes(f: &Fixture) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(f.cd_b64.as_bytes())
        .expect("base64")
}

#[test]
fn happy_path_accepts() {
    let f = load("happy-path.json");
    let trust = CdSigningRoots::with_example_device_roots();
    verify_certification_declaration(
        &cd_bytes(&f),
        VendorId::new(f.expected_vid),
        ProductId::new(f.expected_pid),
        &trust,
    )
    .expect("happy path should verify");
}

#[test]
fn tampered_signature_rejected() {
    let f = load("tampered-signature.json");
    let trust = CdSigningRoots::with_example_device_roots();
    let err = verify_certification_declaration(
        &cd_bytes(&f),
        VendorId::new(f.expected_vid),
        ProductId::new(f.expected_pid),
        &trust,
    )
    .expect_err("tampered signature should reject");
    // Tampering one byte in the signature region might surface as
    // SignatureInvalid (preferred) or Malformed (if the tampered byte
    // lands inside a length/structure octet). Accept either.
    assert!(
        matches!(
            err,
            AttestationError::CertificationDeclarationSignatureInvalid
                | AttestationError::CertificationDeclarationMalformed
        ),
        "got {err:?}"
    );
}

#[test]
fn vid_mismatch_rejected() {
    let f = load("happy-path.json");
    let trust = CdSigningRoots::with_example_device_roots();
    let wrong_vid = VendorId::new(f.expected_vid.wrapping_add(1));
    let err = verify_certification_declaration(
        &cd_bytes(&f),
        wrong_vid,
        ProductId::new(f.expected_pid),
        &trust,
    )
    .expect_err("wrong VID should reject");
    assert!(
        matches!(
            err,
            AttestationError::CertificationDeclarationVidMismatch { .. }
        ),
        "got {err:?}"
    );
}

#[test]
fn pid_mismatch_rejected() {
    let f = load("happy-path.json");
    let trust = CdSigningRoots::with_example_device_roots();
    let wrong_pid = ProductId::new(f.expected_pid.wrapping_add(0xFF));
    let err = verify_certification_declaration(
        &cd_bytes(&f),
        VendorId::new(f.expected_vid),
        wrong_pid,
        &trust,
    )
    .expect_err("wrong PID should reject");
    assert!(
        matches!(
            err,
            AttestationError::CertificationDeclarationPidMismatch(_)
        ),
        "got {err:?}"
    );
}

#[test]
fn empty_trust_store_rejects_valid_cd() {
    let f = load("happy-path.json");
    let trust = CdSigningRoots::from_pem(&[]).expect("empty trust store ok");
    let err = verify_certification_declaration(
        &cd_bytes(&f),
        VendorId::new(f.expected_vid),
        ProductId::new(f.expected_pid),
        &trust,
    )
    .expect_err("empty trust store should reject");
    assert!(
        matches!(
            err,
            AttestationError::CertificationDeclarationSignatureInvalid
        ),
        "got {err:?}"
    );
}
