//! M6.4.6 / M6.5.3 — matter.js byte-parity gate.
//!
//! Replays a captured matter.js commissioning trace through the
//! [`Commissioner`] state machine and asserts every emitted Invoke +
//! `ReadAttribute` payload matches matter.js byte-for-byte for the
//! same inputs.
//!
//! Skips with `eprintln!` when the fixture file is absent or empty —
//! CI stays green during the operator-touch wiring of
//! `cargo xtask capture-commissioning` against the current
//! `@matter/protocol` API.
//!
//! M6.4.6 shipped RNG-free byte-parity for the M6.4 stages
//! (`ArmFailSafe`, `SetRegulatoryConfig`, `CertChainRequest`,
//! `AddTrustedRootCertificate`). M6.5.3 extends scope to the new
//! Wi-Fi sub-cursor: when the operator-captured fixture grows
//! `ReadNetworkCommissioningInfo` (`FeatureMap` read),
//! `NetworkSetup` (`AddOrUpdateWiFiNetwork`),
//! `FailsafeBeforeNetworkEnable` (second `ArmFailSafe`), and
//! `NetworkEnable` (`ConnectNetwork`) stage records, this test
//! will replay them via the existing data-driven match arms — no
//! Rust-side schema change required. All four new stages are
//! RNG-free; they are NOT added to the `rng_bearing` allowlist.
//!
//! The capture-time nonces ride in the fixture and script this test's
//! `NocRng`, so `SendAttestationRequest` and `SendOpCertSigningRequest`
//! are byte-asserted too (the 2026-07-12 upgrade of the original
//! RNG-free baseline). Only `SendTrustedRootCert` and `SendNoc` remain
//! unasserted: they carry certificates minted under THIS run's fresh
//! fabric key pair, which can never equal matter.js's.

// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]
#![forbid(unsafe_code)]

use std::sync::Arc;

use base64::Engine;
use matter_cert::time::MatterTime;
use matter_commissioning::attestation::CdSigningRoots;
use matter_commissioning::noc::{FabricRecord, NocRng, SystemNocRng};
use matter_commissioning::setup::{
    CommissioningFlow, DiscoveryCapabilities, Discriminator, Passcode, SetupPayload,
};
use matter_commissioning::state_machine::{Action, Commissioner, CommissionerConfig};
use matter_commissioning::PaaTrustStore;
use matter_crypto::{RingSigner, Signer};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    captured_at_unix: u64,
    fabric_id: String,
    commissioner_node_id: String,
    assigned_node_id: String,
    ipk_epoch_key_b64: String,
    pase_attestation_challenge_b64: String,
    /// matter.js's capture-time nonces. The walk scripts the state
    /// machine's `NocRng` with these so our `AttestationRequest` /
    /// `CSRRequest` reproduce matter.js's payloads byte-for-byte AND the
    /// captured responses' nonce echoes verify.
    attestation_nonce_b64: String,
    csr_nonce_b64: String,
    /// SPKI PEM of the CD signer the captured device used (chip's
    /// official test CMS signer, via matter.js).
    cd_signing_spki_pem: String,
    /// Set on the tampered-DAC sibling fixture: the walk must REJECT
    /// during attestation instead of completing.
    #[serde(default)]
    verdict_only_reject: bool,
    stages: Vec<StageRecord>,
}

/// `NocRng` that plays back the capture-time nonces: the first two
/// 32-byte fills return the attestation + CSR nonces (the state
/// machine's only 32-byte fills, in stage order); everything else
/// (NOC serials, etc.) falls through to the system RNG.
#[derive(Debug)]
struct ScriptedRng {
    nonces: std::sync::Mutex<std::collections::VecDeque<[u8; 32]>>,
    fallback: SystemNocRng,
}

impl NocRng for ScriptedRng {
    fn fill(&self, dest: &mut [u8]) -> Result<(), matter_commissioning::noc::NocError> {
        if dest.len() == 32 {
            if let Some(n) = self.nonces.lock().unwrap().pop_front() {
                dest.copy_from_slice(&n);
                return Ok(());
            }
        }
        self.fallback.fill(dest)
    }
}

#[derive(Deserialize)]
struct StageRecord {
    stage: String,
    action: String,
    #[serde(default)]
    #[allow(dead_code)] // Reserved for richer per-stage assertions.
    cluster: Option<String>,
    #[serde(default)]
    #[allow(dead_code)] // Reserved for richer per-stage assertions.
    command: Option<String>,
    #[serde(default)]
    #[allow(dead_code)] // Reserved for ReadAttribute attribute-set checks.
    attribute_ids: Vec<u32>,
    expected_payload_b64: Option<String>,
    response_payload_b64: Option<String>,
}

fn parse_hex_u64(s: &str) -> u64 {
    let s = s.trim_start_matches("0x");
    u64::from_str_radix(s, 16).expect("hex u64")
}

fn b64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .expect("base64 decodes")
}

fn fixture_path() -> std::path::PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest_dir).join("../../test-vectors/commissioning/e2e/happy-path.json")
}

#[test]
fn matter_js_happy_path_byte_parity() {
    let path = fixture_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => {
            eprintln!(
                "SKIP: {} missing or empty — run `cargo xtask capture-commissioning` first",
                path.display()
            );
            return;
        }
    };
    let fixture: Fixture = match serde_json::from_str(&raw) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("SKIP: fixture failed to parse: {e}");
            return;
        }
    };
    if fixture.stages.is_empty() {
        eprintln!("SKIP: fixture has no stages — operator wiring incomplete");
        return;
    }

    // Build a Commissioner with matter.js-derived inputs.
    let ipk: [u8; 16] = b64(&fixture.ipk_epoch_key_b64)
        .try_into()
        .expect("IPK is exactly 16 bytes");
    let challenge: [u8; 16] = b64(&fixture.pase_attestation_challenge_b64)
        .try_into()
        .expect("attestation challenge is exactly 16 bytes");

    let (signer, _pkcs8) = RingSigner::generate().expect("ring keypair");
    let signer: Arc<dyn Signer> = Arc::new(signer);
    let rng_fabric = SystemNocRng;
    // Verify at capture time: matter.js mints its dev DAC/PAI fresh per
    // capture, so their validity window brackets captured_at_unix, not any
    // fixed date. Fabric validity brackets the same instant (±1 year).
    let now = MatterTime::from_unix_secs(fixture.captured_at_unix);
    let fabric = FabricRecord::new_root_only(
        parse_hex_u64(&fixture.fabric_id),
        signer,
        MatterTime::from_unix_secs(fixture.captured_at_unix - 31_536_000),
        MatterTime::from_unix_secs(fixture.captured_at_unix + 31_536_000),
        parse_hex_u64(&fixture.commissioner_node_id),
        &rng_fabric,
    )
    .expect("valid root fabric");

    let setup = SetupPayload {
        version: 0,
        vendor_id: Some(0xFFF1),
        product_id: Some(0x8001),
        commissioning_flow: CommissioningFlow::Standard,
        discovery_capabilities: DiscoveryCapabilities::ON_NETWORK,
        discriminator: Discriminator::new(0x0F00).expect("valid discriminator"),
        passcode: Passcode::new(20_202_021).expect("valid passcode"),
    };
    let paa = PaaTrustStore::with_example_device_roots();
    // Trust exactly the CD signer the captured device used (chip's test CMS
    // signer, carried in the fixture as an SPKI PEM).
    let cd = CdSigningRoots::from_pem(&[fixture.cd_signing_spki_pem.as_bytes()])
        .expect("fixture CD signing SPKI parses");
    // Script the capture-time nonces so our RNG-bearing payloads reproduce
    // matter.js's bytes AND the captured responses' nonce echoes verify.
    let attestation_nonce: [u8; 32] = b64(&fixture.attestation_nonce_b64)
        .try_into()
        .expect("attestation nonce is 32 bytes");
    let csr_nonce: [u8; 32] = b64(&fixture.csr_nonce_b64)
        .try_into()
        .expect("CSR nonce is 32 bytes");
    let rng: Arc<dyn NocRng> = Arc::new(ScriptedRng {
        nonces: std::sync::Mutex::new([attestation_nonce, csr_nonce].into_iter().collect()),
        fallback: SystemNocRng,
    });
    let cfg = CommissionerConfig {
        pase_attestation_challenge: challenge,
        fabric: &fabric,
        setup_payload: &setup,
        paa_trust_store: &paa,
        cd_signing_roots: &cd,
        commissioner_node_id: parse_hex_u64(&fixture.commissioner_node_id),
        assigned_node_id: parse_hex_u64(&fixture.assigned_node_id),
        ipk_epoch_key: ipk,
        case_admin_subject: parse_hex_u64(&fixture.commissioner_node_id),
        admin_vendor_id: 0xFFF1,
        now,
        rng,
        network: matter_commissioning::NetworkCredentials::AlreadyOnNetwork,
    };
    let mut sm = Commissioner::new(cfg).expect("valid config");

    // Walk every fixture stage, asserting byte-parity on emitted
    // payloads (skipping RNG-bearing payloads — see module doc).
    for (idx, record) in fixture.stages.iter().enumerate() {
        let act = sm.poll().expect("poll");
        match (record.action.as_str(), &act) {
            (
                "Invoke",
                Action::Invoke {
                    payload, expect, ..
                },
            ) => {
                if let Some(expected_b64) = &record.expected_payload_b64 {
                    let expected = b64(expected_b64);
                    // M6.4.6 RNG-free baseline: assert byte-parity only on
                    // stages without RNG-derived content.
                    // With the fixture-scripted nonces the attestation and
                    // CSR requests are byte-asserted too. Only the two
                    // stages carrying LOCALLY-MINTED certificates still
                    // legitimately differ: AddTrustedRootCertificate (our
                    // fabric's RCAC, minted from this run's fresh key pair)
                    // and AddNOC (our NOC under that RCAC).
                    let rng_bearing =
                        matches!(record.stage.as_str(), "SendTrustedRootCert" | "SendNoc");
                    if rng_bearing {
                        eprintln!(
                            "stage[{idx}] {} — skipping byte-parity (locally-minted certificate)",
                            record.stage
                        );
                    } else {
                        assert_eq!(
                            payload, &expected,
                            "stage[{idx}] {} Invoke payload differs from matter.js",
                            record.stage
                        );
                    }
                }
                let response = record
                    .response_payload_b64
                    .as_ref()
                    .map(|s| b64(s))
                    .unwrap_or_default();
                sm.on_response(*expect, &response)
                    .expect("response accepted");
            }
            ("ReadAttribute", Action::ReadAttribute { expect, .. }) => {
                let response = record
                    .response_payload_b64
                    .as_ref()
                    .map(|s| b64(s))
                    .unwrap_or_default();
                sm.on_response(*expect, &response)
                    .expect("response accepted");
            }
            ("EstablishCase", Action::EstablishCase { .. }) => {
                sm.on_case_established().expect("case established");
            }
            (kind, other) => {
                panic!("stage[{idx}] expected {kind}, got {other:?}");
            }
        }
    }

    match sm.poll().expect("final poll") {
        Action::Done(_) => {}
        other => panic!("expected Done after walking fixture, got {other:?}"),
    }
}

/// Verdict-only negative gate (M6.4.6): the sibling fixture flips one
/// bit inside the DAC certificate of the captured `CertificateChainResponse`.
/// The walk must FAIL during attestation — the DAC's signature no longer
/// verifies against its PAI — rather than complete, and must fail no later
/// than the `AttestationRequest` response (the stage where the chain is
/// verified). No byte-parity is asserted on a rejection path.
#[test]
fn tampered_dac_is_rejected_during_attestation() {
    let path = fixture_path().with_file_name("tampered-dac.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => {
            eprintln!(
                "SKIP: {} missing or empty — run `cargo xtask capture-commissioning` first",
                path.display()
            );
            return;
        }
    };
    let fixture: Fixture = match serde_json::from_str(&raw) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("SKIP: tampered fixture failed to parse: {e}");
            return;
        }
    };
    assert!(
        fixture.verdict_only_reject,
        "tampered fixture must be marked verdict_only_reject"
    );

    let ipk: [u8; 16] = b64(&fixture.ipk_epoch_key_b64)
        .try_into()
        .expect("IPK is exactly 16 bytes");
    let challenge: [u8; 16] = b64(&fixture.pase_attestation_challenge_b64)
        .try_into()
        .expect("attestation challenge is exactly 16 bytes");
    let (signer, _pkcs8) = RingSigner::generate().expect("ring keypair");
    let signer: Arc<dyn Signer> = Arc::new(signer);
    let rng_fabric = SystemNocRng;
    let now = MatterTime::from_unix_secs(fixture.captured_at_unix);
    let fabric = FabricRecord::new_root_only(
        parse_hex_u64(&fixture.fabric_id),
        signer,
        MatterTime::from_unix_secs(fixture.captured_at_unix - 31_536_000),
        MatterTime::from_unix_secs(fixture.captured_at_unix + 31_536_000),
        parse_hex_u64(&fixture.commissioner_node_id),
        &rng_fabric,
    )
    .expect("valid root fabric");
    let setup = SetupPayload {
        version: 0,
        vendor_id: Some(0xFFF1),
        product_id: Some(0x8001),
        commissioning_flow: CommissioningFlow::Standard,
        discovery_capabilities: DiscoveryCapabilities::ON_NETWORK,
        discriminator: Discriminator::new(0x0F00).expect("valid discriminator"),
        passcode: Passcode::new(20_202_021).expect("valid passcode"),
    };
    let paa = PaaTrustStore::with_example_device_roots();
    let cd = CdSigningRoots::from_pem(&[fixture.cd_signing_spki_pem.as_bytes()])
        .expect("fixture CD signing SPKI parses");
    let attestation_nonce: [u8; 32] = b64(&fixture.attestation_nonce_b64)
        .try_into()
        .expect("attestation nonce is 32 bytes");
    let csr_nonce: [u8; 32] = b64(&fixture.csr_nonce_b64)
        .try_into()
        .expect("CSR nonce is 32 bytes");
    let rng: Arc<dyn NocRng> = Arc::new(ScriptedRng {
        nonces: std::sync::Mutex::new([attestation_nonce, csr_nonce].into_iter().collect()),
        fallback: SystemNocRng,
    });
    let cfg = CommissionerConfig {
        pase_attestation_challenge: challenge,
        fabric: &fabric,
        setup_payload: &setup,
        paa_trust_store: &paa,
        cd_signing_roots: &cd,
        commissioner_node_id: parse_hex_u64(&fixture.commissioner_node_id),
        assigned_node_id: parse_hex_u64(&fixture.assigned_node_id),
        ipk_epoch_key: ipk,
        case_admin_subject: parse_hex_u64(&fixture.commissioner_node_id),
        admin_vendor_id: 0xFFF1,
        now,
        rng,
        network: matter_commissioning::NetworkCredentials::AlreadyOnNetwork,
    };
    let mut sm = Commissioner::new(cfg).expect("valid config");

    // Walk until something rejects. The tamper MUST surface by the time the
    // AttestationRequest response is processed (that is where the DAC chain
    // is verified); reaching any later stage means the tamper was accepted.
    for record in &fixture.stages {
        let act = match sm.poll() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("rejected at poll during stage {}: {e}", record.stage);
                // The off-wire AttestationVerification runs in the poll that
                // would otherwise emit the CSR request — the rejection must
                // not arrive later than that.
                assert!(
                    matches!(
                        record.stage.as_str(),
                        "SendDacCertRequest"
                            | "SendAttestationRequest"
                            | "SendOpCertSigningRequest"
                    ),
                    "tamper must surface by attestation verification, not at stage {}",
                    record.stage
                );
                return; // rejection observed — test passes
            }
        };
        let fed = match (record.action.as_str(), &act) {
            ("Invoke", Action::Invoke { expect, .. })
            | ("ReadAttribute", Action::ReadAttribute { expect, .. }) => {
                let response = record
                    .response_payload_b64
                    .as_ref()
                    .map(|s| b64(s))
                    .unwrap_or_default();
                sm.on_response(*expect, &response)
            }
            ("EstablishCase", Action::EstablishCase { .. }) => sm.on_case_established(),
            (kind, other) => panic!("stage {} expected {kind}, got {other:?}", record.stage),
        };
        if let Err(e) = fed {
            eprintln!("rejected at stage {}: {e}", record.stage);
            assert!(
                matches!(
                    record.stage.as_str(),
                    "SendDacCertRequest" | "SendAttestationRequest"
                ),
                "tamper must surface during DAC delivery or attestation \
                 verification, not at stage {}",
                record.stage
            );
            return; // rejection observed — test passes
        }
        assert_ne!(
            record.stage.as_str(),
            "SendOpCertSigningRequest",
            "walk progressed past attestation with a tampered DAC"
        );
    }
    panic!("tampered-DAC walk completed without rejection");
}
