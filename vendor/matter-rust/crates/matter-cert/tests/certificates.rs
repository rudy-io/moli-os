//! Integration test: every certificate in `test-vectors/certs/manifest.toml`
//! parses successfully and round-trips through `from_tlv` → `to_tlv`
//! byte-for-byte.
//!
//! The manifest carries no `expected` field-assertion annotations for the
//! committed snapshot — those can be added later. The test
//! therefore exercises: (1) every captured cert parses without error,
//! (2) re-serialisation produces byte-for-byte identical output, and
//! (3) the manifest is non-empty (regression guard against an empty capture).

// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md), provided
// there is a documented justification. Here they are used to fail
// the test with a clear message that names the offending cert ID.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

use matter_cert::MatterCertificate;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Manifest schema — must match `test-vectors/certs/manifest.toml` exactly.
// The `kind` field ("rcac" | "icac" | "noc") is present but only used for
// the `#[allow(dead_code)]` display; the round-trip test is kind-agnostic.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Manifest {
    certificate: Vec<CertificateEntry>,
}

#[derive(Debug, Deserialize)]
struct CertificateEntry {
    id: String,
    #[allow(dead_code)]
    description: String,
    #[allow(dead_code)]
    source: String,
    file: String,
    #[allow(dead_code)]
    kind: String,
    /// Whether this cert is self-signed (root). Used by the byte-parity and
    /// signature-verification integration tests to identify which key to
    /// verify against.
    #[serde(default)]
    is_self_signed: bool,
    /// `id` of the cert whose public key signed this one. Used by
    /// `every_certificate_signature_verifies_against_its_issuer` to chain
    /// through the manifest.
    #[serde(default)]
    signed_by_id: Option<String>,
    /// Path (relative to test-vectors/certs/) of matter.js's
    /// `asUnsignedDer()` output for this cert. The bytes whose
    /// signature should verify against this cert's signature.
    /// Consumed by the byte-parity integration test.
    #[serde(default)]
    tbs_file: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR resolves to `crates/matter-cert` at test time.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ directory must exist")
        .parent()
        .expect("workspace root must exist")
        .to_path_buf()
}

/// The captured fixture sets: matter.js (`@matter/protocol`) and the CSA
/// C++ reference (`connectedhomeip`'s chip-cert, via
/// `cargo xtask capture-cert-chip`). Every test below runs against BOTH —
/// matter.js has diverged from the C++ canonical implementation before, so
/// the byte-parity gate holds against each independently.
const FIXTURE_DIRS: [&str; 2] = ["test-vectors/certs", "test-vectors/certs/connectedhomeip"];

fn load_manifest(dir: &str) -> Manifest {
    let path = workspace_root().join(dir).join("manifest.toml");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read manifest at {}: {e}", path.display()));
    toml::from_str(&text).expect("manifest.toml must be valid TOML matching the Manifest schema")
}

fn load_bin(dir: &str, file: &str) -> Vec<u8> {
    let path = workspace_root().join(dir).join(file);
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read cert file {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn every_certificate_parses_and_round_trips() {
    for dir in FIXTURE_DIRS {
        let manifest = load_manifest(dir);
        assert!(
            !manifest.certificate.is_empty(),
            "manifest contains no certificates — capture must have failed or the file is empty",
        );

        let mut processed = 0usize;
        for entry in &manifest.certificate {
            let bytes = load_bin(dir, &entry.file);

            // Step 1: parse.
            let cert = MatterCertificate::from_tlv(&bytes).unwrap_or_else(|e| {
                panic!(
                    "MatterCertificate::from_tlv failed for cert '{}' (file: {}): {e}",
                    entry.id, entry.file,
                );
            });

            // Step 2: re-serialise and assert byte-for-byte equality.
            let re_serialised = cert.to_tlv().unwrap_or_else(|e| {
                panic!(
                    "MatterCertificate::to_tlv failed for cert '{}': {e}",
                    entry.id,
                );
            });
            assert_eq!(
                re_serialised, bytes,
                "cert '{}' parsed successfully but re-serialised to different bytes \
             (check for missed fields or wrong emission order in to_tlv)",
                entry.id,
            );

            processed += 1;
        }

        eprintln!("[{dir}] processed {processed} certificate(s) — all parsed and round-tripped");
    }
}

// ---------------------------------------------------------------------------
// byte-parity + signature verification
// ---------------------------------------------------------------------------

/// Assert that `to_x509_tbs_der()` produces byte-identical output to
/// matter.js's `asUnsignedDer()` for every cert in the manifest.
///
/// This is the strict correctness gate for the X.509 encoder.  If it fails,
/// the first differing byte identifies which encoder function to fix.
#[test]
fn every_certificate_x509_tbs_matches_reference() {
    for dir in FIXTURE_DIRS {
        let entries = load_manifest(dir).certificate;

        for entry in &entries {
            let cert_bytes = load_bin(dir, &entry.file);
            let cert = MatterCertificate::from_tlv(&cert_bytes).unwrap_or_else(|e| {
                panic!("parse {}: {e}", entry.id);
            });

            let our_tbs = cert.to_x509_tbs_der().unwrap_or_else(|e| {
                panic!("to_x509_tbs_der for {}: {e}", entry.id);
            });

            let tbs_file = entry
                .tbs_file
                .as_ref()
                .unwrap_or_else(|| panic!("manifest entry {} missing tbs_file", entry.id));
            let captured_tbs = load_bin(dir, tbs_file);

            assert_eq!(
            our_tbs, captured_tbs,
            "byte-parity mismatch for {} in {dir}: our TBS differs from the reference's signed TBS",
            entry.id
        );
        }
    }
}

/// For each cert in the manifest, verify its signature using the issuer's
/// public key (or the cert's own key if self-signed).
#[test]
fn every_certificate_signature_verifies_against_its_issuer() {
    for dir in FIXTURE_DIRS {
        let entries = load_manifest(dir).certificate;

        for entry in &entries {
            let cert_bytes = load_bin(dir, &entry.file);
            let cert = MatterCertificate::from_tlv(&cert_bytes).unwrap_or_else(|e| {
                panic!("parse {}: {e}", entry.id);
            });

            let issuer_key = if entry.is_self_signed {
                cert.public_key().clone()
            } else {
                let issuer_id = entry.signed_by_id.as_ref().unwrap_or_else(|| {
                    panic!("non-self-signed entry {} missing signed_by_id", entry.id)
                });
                let issuer_entry = entries
                    .iter()
                    .find(|e| &e.id == issuer_id)
                    .unwrap_or_else(|| panic!("issuer {issuer_id} not in manifest"));
                let issuer_bytes = load_bin(dir, &issuer_entry.file);
                let issuer_cert = MatterCertificate::from_tlv(&issuer_bytes).unwrap();
                issuer_cert.public_key().clone()
            };

            cert.verify_signed_by(&issuer_key).unwrap_or_else(|e| {
                panic!(
                    "signature verification failed for {} in {dir}: {e}",
                    entry.id
                )
            });
        }
    }
}

/// Verify that `verify_signed_by` correctly rejects a signature when the
/// wrong issuer key is provided (NOC signed by ICAC, verified against RCAC).
#[test]
fn signature_verification_rejects_wrong_issuer() {
    for dir in FIXTURE_DIRS {
        let entries = load_manifest(dir).certificate;
        let noc_entry = entries
            .iter()
            .find(|e| e.id == "noc")
            .expect("noc in manifest");
        let rcac_entry = entries
            .iter()
            .find(|e| e.id == "rcac")
            .expect("rcac in manifest");

        let noc_bytes = load_bin(dir, &noc_entry.file);
        let noc = MatterCertificate::from_tlv(&noc_bytes).unwrap();
        let rcac_bytes = load_bin(dir, &rcac_entry.file);
        let rcac = MatterCertificate::from_tlv(&rcac_bytes).unwrap();

        let err = noc.verify_signed_by(rcac.public_key()).unwrap_err();
        assert!(
            matches!(err, matter_cert::Error::SignatureVerificationFailed),
            "expected SignatureVerificationFailed, got {err:?}"
        );
    }
}
