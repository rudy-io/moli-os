//! M6.3.3 matter.js byte-parity gate.
//!
//! Drives `issue_noc` against the captured matter.js fixtures and
//! asserts our output bytes equal matter.js's output bytes.
//!
//! Fixtures must be captured via `cargo xtask capture-noc` first.
//! Tests skip (log a warning) when fixtures are absent or carry empty
//! `expected_*_b64` fields, so local development isn't blocked while
//! the matter.js capture is being wired.

#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::redundant_closure_for_method_calls,
    clippy::single_match_else,
    clippy::doc_markdown
)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(unreachable_pub)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use matter_cert::{MatterCertificate, MatterTime, PublicKey};
use matter_commissioning::{
    encode_add_noc, encode_add_trusted_root, encode_csr_request, issue_noc, FabricRecord, NocRng,
    VerifiedCsr,
};
use matter_crypto::{RingSigner, Signer};
use serde::Deserialize;

fn fixtures_root() -> PathBuf {
    let mut p: PathBuf = env!("CARGO_MANIFEST_DIR").into();
    p.push("..");
    p.push("..");
    p.push("test-vectors");
    p.push("commissioning");
    p.push("noc");
    p
}

fn list_jsons(sub: &str) -> Vec<PathBuf> {
    let dir = fixtures_root().join(sub);
    if !dir.exists() {
        return Vec::new();
    }
    let mut out: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    out.sort();
    out
}

#[derive(Deserialize)]
struct CsrRequestFixture {
    nonce_hex: String,
    is_for_update_noc: bool,
    expected_tlv_b64: String,
}

#[test]
fn csr_request_encoder_matches_matter_js() {
    let paths = list_jsons("csr_request");
    if paths.is_empty() {
        eprintln!("skipping: no CSRRequest fixtures (run `cargo xtask capture-noc`)");
        return;
    }
    for path in paths {
        let bytes = fs::read(&path).unwrap();
        let f: CsrRequestFixture = serde_json::from_slice(&bytes).unwrap();
        if f.expected_tlv_b64.is_empty() {
            eprintln!("{path:?}: empty expected_tlv_b64 — fixture not captured yet, skipping");
            continue;
        }
        let nonce_bytes = hex::decode(&f.nonce_hex).unwrap();
        let mut nonce = [0u8; 32];
        nonce.copy_from_slice(&nonce_bytes);
        let ours = encode_csr_request(&nonce, f.is_for_update_noc);
        let theirs = B64.decode(&f.expected_tlv_b64).unwrap();
        assert_eq!(ours, theirs, "CSRRequest mismatch for {path:?}");
    }
}

#[derive(Deserialize)]
struct NocChainFixture {
    rcac_pkcs8_b64: String,
    rcac_matter_tlv_b64: String,
    csr_public_key_b64: String,
    node_id: u64,
    fabric_id: u64,
    cats: Vec<u32>,
    validity: ValidityField,
    serial_hex: String,
    expected_noc_matter_tlv_b64: String,
}

#[derive(Deserialize)]
struct ValidityField {
    not_before_unix: u32,
    not_after_unix: serde_json::Value,
}

/// Stub RNG that returns pre-baked bytes (the serial number) when
/// `fill` is called with a 19-byte buffer. Falls back to a deterministic
/// fill for any other size (used here only if FabricRecord::new_root_only
/// were called — but the byte-parity test bypasses it by constructing
/// FabricRecord directly).
#[derive(Debug)]
struct StubRng {
    serial: [u8; 19],
}
impl NocRng for StubRng {
    fn fill(&self, dest: &mut [u8]) -> Result<(), matter_commissioning::NocError> {
        if dest.len() == 19 {
            dest.copy_from_slice(&self.serial);
            return Ok(());
        }
        for (i, b) in dest.iter_mut().enumerate() {
            *b = (i & 0xff) as u8;
        }
        Ok(())
    }
}

#[test]
fn issued_noc_matches_matter_js_bytes() {
    let paths = list_jsons("noc_chains");
    if paths.is_empty() {
        eprintln!("skipping: no NOC chain fixtures (run `cargo xtask capture-noc`)");
        return;
    }

    for path in paths {
        let bytes = fs::read(&path).unwrap();
        let f: NocChainFixture = serde_json::from_slice(&bytes).unwrap();
        if f.expected_noc_matter_tlv_b64.is_empty() {
            eprintln!(
                "{path:?}: empty expected_noc_matter_tlv_b64 — fixture not captured yet, skipping"
            );
            continue;
        }

        // Load RCAC keypair from PKCS#8.
        let pkcs8 = B64.decode(&f.rcac_pkcs8_b64).unwrap();
        let root_signer: Arc<dyn Signer> = Arc::new(RingSigner::from_pkcs8(&pkcs8).unwrap());

        // Load RCAC certificate from Matter TLV.
        let rcac_tlv = B64.decode(&f.rcac_matter_tlv_b64).unwrap();
        let rcac = MatterCertificate::from_tlv(&rcac_tlv).unwrap();

        // Construct FabricRecord directly (skip new_root_only so we
        // use matter.js's RCAC, not a freshly-generated one).
        let fabric = FabricRecord {
            fabric_id: f.fabric_id,
            root_public_key: root_signer.public_key().clone(),
            root_signer,
            root_cert: rcac,
            icac_signer: None,
            icac_cert: None,
            identity_protection_key: [0u8; 16],
        };

        // Build VerifiedCsr from the CSR public key.
        let pk_bytes = B64.decode(&f.csr_public_key_b64).unwrap();
        let mut pk_arr = [0u8; 65];
        pk_arr.copy_from_slice(&pk_bytes);
        let verified = VerifiedCsr {
            public_key: PublicKey::new(pk_arr).unwrap(),
        };

        // Stable serial.
        let serial_bytes = hex::decode(&f.serial_hex).unwrap();
        let mut serial = [0u8; 19];
        serial.copy_from_slice(&serial_bytes);
        let rng = StubRng { serial };

        let not_after = match &f.validity.not_after_unix {
            serde_json::Value::String(s) if s == "NO_EXPIRY" => MatterTime::NO_EXPIRY,
            serde_json::Value::Number(n) => {
                let secs = n.as_u64().unwrap();
                MatterTime::from_unix_secs(secs)
            }
            other => panic!("unexpected not_after_unix shape: {other:?}"),
        };
        let validity = (
            MatterTime::from_unix_secs(u64::from(f.validity.not_before_unix)),
            not_after,
        );

        let noc = issue_noc(&fabric, &verified, f.node_id, &f.cats, validity, &rng).unwrap();
        let ours = noc.to_tlv().unwrap();
        let theirs = B64.decode(&f.expected_noc_matter_tlv_b64).unwrap();
        assert_eq!(ours, theirs, "NOC byte-parity mismatch for {path:?}");
    }
}

#[derive(Deserialize)]
struct AddNocFixture {
    noc_matter_tlv_b64: String,
    icac_matter_tlv_b64: Option<String>,
    ipk_hex: String,
    case_admin_subject: u64,
    admin_vendor_id: u16,
    expected_payload_b64: String,
}

#[test]
fn add_noc_payload_matches_matter_js() {
    let paths = list_jsons("add_noc");
    if paths.is_empty() {
        eprintln!("skipping: no AddNOC fixtures (run `cargo xtask capture-noc`)");
        return;
    }
    for path in paths {
        // Skip files reserved for the AddTrustedRoot test below.
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.ends_with("-add_trusted_root.json"))
        {
            continue;
        }
        let bytes = fs::read(&path).unwrap();
        let f: AddNocFixture = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("{path:?}: not an AddNoc fixture — skipping");
                continue;
            }
        };
        if f.expected_payload_b64.is_empty() {
            continue;
        }
        let noc = B64.decode(&f.noc_matter_tlv_b64).unwrap();
        let icac = f
            .icac_matter_tlv_b64
            .as_deref()
            .map(|s| B64.decode(s).unwrap());
        let icac_ref = icac.as_deref();
        let ipk_bytes = hex::decode(&f.ipk_hex).unwrap();
        let mut ipk = [0u8; 16];
        ipk.copy_from_slice(&ipk_bytes);
        let ours = encode_add_noc(
            &noc,
            icac_ref,
            &ipk,
            f.case_admin_subject,
            f.admin_vendor_id,
        );
        let theirs = B64.decode(&f.expected_payload_b64).unwrap();
        assert_eq!(ours, theirs, "AddNOC mismatch for {path:?}");
    }
}

#[derive(Deserialize)]
struct AddTrustedRootFixture {
    rcac_matter_tlv_b64: String,
    expected_payload_b64: String,
}

#[test]
fn add_trusted_root_payload_matches_matter_js() {
    let dir = fixtures_root().join("add_noc");
    if !dir.exists() {
        eprintln!("skipping: no AddNOC fixtures (run `cargo xtask capture-noc`)");
        return;
    }
    let entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.ends_with("-add_trusted_root.json"))
        })
        .collect();
    for path in entries {
        let bytes = fs::read(&path).unwrap();
        let f: AddTrustedRootFixture = serde_json::from_slice(&bytes).unwrap();
        let rcac = B64.decode(&f.rcac_matter_tlv_b64).unwrap();
        let ours = encode_add_trusted_root(&rcac);
        let theirs = B64.decode(&f.expected_payload_b64).unwrap();
        assert_eq!(ours, theirs, "AddTrustedRoot mismatch for {path:?}");
    }
}
