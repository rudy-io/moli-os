//! Shared harness for the captured-fixture e2e tests: loads
//! `test-vectors/commissioning/e2e/happy-path.json` (produced by
//! `cargo xtask capture-commissioning` from a live matter.js
//! commissioner ↔ virtual-device run), builds a [`Commissioner`] whose
//! out-of-wire inputs (IPK, PASE attestation challenge, nonces, CD trust)
//! come from the fixture, and walks the captured stage records through the
//! public API.
//!
//! Declared from each consuming test file via `mod e2e_fixture;`.
//! Production code must never depend on anything here.

// Shared across several integration-test binaries; each compiles this module
// independently and uses a different subset (same carve-outs as
// tests/support/mod.rs).
#![allow(dead_code, unreachable_pub)]
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use base64::Engine;
use matter_cert::time::MatterTime;
use matter_commissioning::attestation::CdSigningRoots;
use matter_commissioning::noc::{FabricRecord, NocError, NocRng, SystemNocRng};
use matter_commissioning::setup::{
    CommissioningFlow, DiscoveryCapabilities, Discriminator, Passcode, SetupPayload,
};
use matter_commissioning::state_machine::{
    Action, Commissioner, CommissionerConfig, CommissioningError,
};
use matter_commissioning::PaaTrustStore;
use matter_crypto::{RingSigner, Signer};
use serde::Deserialize;

/// The captured fixture schema — mirrors what
/// `xtask/src/capture_commissioning.rs` writes.
#[derive(Deserialize)]
pub struct Fixture {
    pub captured_at_unix: u64,
    pub fabric_id: String,
    pub commissioner_node_id: String,
    pub assigned_node_id: String,
    pub ipk_epoch_key_b64: String,
    pub pase_attestation_challenge_b64: String,
    pub attestation_nonce_b64: String,
    pub csr_nonce_b64: String,
    pub cd_signing_spki_pem: String,
    #[serde(default)]
    pub verdict_only_reject: bool,
    pub stages: Vec<StageRecord>,
}

#[derive(Deserialize)]
pub struct StageRecord {
    pub stage: String,
    pub action: String,
    #[serde(default)]
    pub cluster: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub attribute_ids: Vec<u32>,
    pub expected_payload_b64: Option<String>,
    pub response_payload_b64: Option<String>,
}

pub fn parse_hex_u64(s: &str) -> u64 {
    let s = s.trim_start_matches("0x");
    u64::from_str_radix(s, 16).expect("hex u64")
}

pub fn b64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .expect("base64 decodes")
}

/// `NocRng` that plays back the capture-time nonces: the first two 32-byte
/// fills return the attestation + CSR nonces (the state machine's only
/// 32-byte fills, in stage order); everything else (NOC serials, etc.)
/// falls through to the system RNG.
#[derive(Debug)]
pub struct ScriptedRng {
    pub nonces: std::sync::Mutex<std::collections::VecDeque<[u8; 32]>>,
    pub fallback: SystemNocRng,
}

impl NocRng for ScriptedRng {
    fn fill(&self, dest: &mut [u8]) -> Result<(), NocError> {
        if dest.len() == 32 {
            if let Some(n) = self.nonces.lock().unwrap().pop_front() {
                dest.copy_from_slice(&n);
                return Ok(());
            }
        }
        self.fallback.fill(dest)
    }
}

/// Load a fixture by file name from `test-vectors/commissioning/e2e/`.
/// Returns `None` (so callers can SKIP gracefully) when absent, empty, or
/// unparseable — CI stays green on a tree without captured vectors.
pub fn load(name: &str) -> Option<Fixture> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-vectors/commissioning/e2e")
        .join(name);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => {
            eprintln!(
                "SKIP: {} missing or empty — run `cargo xtask capture-commissioning` first",
                path.display()
            );
            return None;
        }
    };
    match serde_json::from_str::<Fixture>(&raw) {
        Ok(f) if !f.stages.is_empty() => Some(f),
        Ok(_) => {
            eprintln!("SKIP: fixture {name} has no stages");
            None
        }
        Err(e) => {
            eprintln!("SKIP: fixture {name} failed to parse: {e}");
            None
        }
    }
}

/// Owns everything `CommissionerConfig` borrows, so tests can build a
/// [`Commissioner`] in one call.
pub struct Harness {
    pub fabric: FabricRecord,
    pub setup: SetupPayload,
    pub paa: PaaTrustStore,
    pub cd: CdSigningRoots,
    pub challenge: [u8; 16],
    pub ipk: [u8; 16],
    pub commissioner_node_id: u64,
    pub assigned_node_id: u64,
    pub now: MatterTime,
    pub rng: Arc<dyn NocRng>,
}

impl Harness {
    /// Build the harness from a fixture: fabric under a fresh key pair,
    /// capture-time clock, fixture-scripted nonces, fixture-pinned CD trust.
    pub fn from_fixture(fixture: &Fixture) -> Self {
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
        // capture, so their validity window brackets captured_at_unix, not
        // any fixed date. Fabric validity brackets the same instant (±1y).
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
        let attestation_nonce: [u8; 32] = b64(&fixture.attestation_nonce_b64)
            .try_into()
            .expect("attestation nonce is 32 bytes");
        let csr_nonce: [u8; 32] = b64(&fixture.csr_nonce_b64)
            .try_into()
            .expect("CSR nonce is 32 bytes");
        Self {
            fabric,
            setup,
            paa: PaaTrustStore::with_example_device_roots(),
            cd: CdSigningRoots::from_pem(&[fixture.cd_signing_spki_pem.as_bytes()])
                .expect("fixture CD signing SPKI parses"),
            challenge,
            ipk,
            commissioner_node_id: parse_hex_u64(&fixture.commissioner_node_id),
            assigned_node_id: parse_hex_u64(&fixture.assigned_node_id),
            now,
            rng: Arc::new(ScriptedRng {
                nonces: std::sync::Mutex::new([attestation_nonce, csr_nonce].into_iter().collect()),
                fallback: SystemNocRng,
            }),
        }
    }

    /// A fresh [`Commissioner`] over this harness's inputs.
    pub fn commissioner(&self) -> Commissioner {
        let cfg = CommissionerConfig {
            pase_attestation_challenge: self.challenge,
            fabric: &self.fabric,
            setup_payload: &self.setup,
            paa_trust_store: &self.paa,
            cd_signing_roots: &self.cd,
            commissioner_node_id: self.commissioner_node_id,
            assigned_node_id: self.assigned_node_id,
            ipk_epoch_key: self.ipk,
            case_admin_subject: self.commissioner_node_id,
            admin_vendor_id: 0xFFF1,
            now: self.now,
            rng: self.rng.clone(),
            network: matter_commissioning::NetworkCredentials::AlreadyOnNetwork,
        };
        Commissioner::new(cfg).expect("valid config")
    }
}

/// Poll + feed captured records through `sm` in order, WITHOUT byte-parity
/// assertions (the parity test does those), stopping after the record named
/// `stop_after` has been fed. Returns the first error surfaced by `poll` /
/// `on_response`, tagged with the stage it arose at.
pub fn drive(
    sm: &mut Commissioner,
    stages: &[StageRecord],
    stop_after: Option<&str>,
) -> Result<(), (String, CommissioningError)> {
    for record in stages {
        let act = sm.poll().map_err(|e| (record.stage.clone(), e))?;
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
        fed.map_err(|e| (record.stage.clone(), e))?;
        if stop_after == Some(record.stage.as_str()) {
            return Ok(());
        }
    }
    Ok(())
}
