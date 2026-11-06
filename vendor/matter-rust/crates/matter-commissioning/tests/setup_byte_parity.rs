//! Byte-parity replay of matter.js setup-payload fixtures captured by
//! `cargo xtask capture-setup`. Every fixture in
//! `test-vectors/commissioning/setup/` whose name starts with `qr-` is
//! roundtripped through `encode_qr` / `parse_qr`; every fixture starting
//! with `manual-` through `encode_manual_code` / `parse_manual_code`.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
// Fixture names are ASCII and produced by the capture script — a
// case-sensitive `.json` check is correct and matches the fuzz-target
// convention used elsewhere in the workspace.
#![allow(clippy::case_sensitive_file_extension_comparisons)]

use std::fs;
use std::path::{Path, PathBuf};

use matter_commissioning::setup::{
    encode_manual_code, encode_qr, parse_manual_code, parse_qr, CommissioningFlow,
    DiscoveryCapabilities, Discriminator, Passcode, SetupPayload,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    intent: String,
    input: InputJson,
    expected: ExpectedJson,
}

#[derive(Deserialize)]
struct InputJson {
    version: u8,
    vendor_id: Option<u16>,
    product_id: Option<u16>,
    commissioning_flow: String,
    discovery_capabilities: Vec<String>,
    discriminator: u16,
    passcode: u32,
}

#[derive(Deserialize)]
struct ExpectedJson {
    qr: Option<String>,
    manual: Option<String>,
}

fn fixtures_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/matter-commissioning when
    // `cargo test` runs the integration test target.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-vectors")
        .join("commissioning")
        .join("setup")
        .canonicalize()
        .expect("test-vectors/commissioning/setup must exist")
}

fn input_to_payload(input: &InputJson) -> SetupPayload {
    let mut caps = DiscoveryCapabilities::empty();
    for c in &input.discovery_capabilities {
        match c.as_str() {
            "SoftAp" => caps |= DiscoveryCapabilities::SOFT_AP,
            "Ble" => caps |= DiscoveryCapabilities::BLE,
            "OnNetwork" => caps |= DiscoveryCapabilities::ON_NETWORK,
            other => panic!("unknown discovery capability `{other}`"),
        }
    }
    let flow = match input.commissioning_flow.as_str() {
        "Standard" => CommissioningFlow::Standard,
        "UserIntent" => CommissioningFlow::UserIntent,
        "Custom" => CommissioningFlow::Custom,
        other => panic!("unknown commissioning flow `{other}`"),
    };
    SetupPayload {
        version: input.version,
        vendor_id: input.vendor_id,
        product_id: input.product_id,
        commissioning_flow: flow,
        discovery_capabilities: caps,
        discriminator: Discriminator::new(input.discriminator).unwrap(),
        passcode: Passcode::new(input.passcode).unwrap(),
    }
}

/// Like [`input_to_payload`], but for `manual-*` fixtures, whose payloads
/// carry a **short** discriminator.
///
/// A manual pairing code only ever conveys the upper 4 bits, so a payload
/// decoded from one is short by construction and compares unequal to the same
/// bits built as a long discriminator (#120). Every current manual fixture
/// stores a short-aligned value, which this asserts: a fixture with non-zero
/// low bits could not round-trip at all, and would need the expected *decoded*
/// discriminator recorded separately rather than reusing `input`.
fn manual_input_to_payload(input: &InputJson) -> SetupPayload {
    assert_eq!(
        input.discriminator & 0x00FF,
        0,
        "manual fixture discriminator {:#05x} has non-zero low bits; a manual \
         code cannot carry them, so `input` cannot double as the expected \
         decode — record the decoded value separately",
        input.discriminator
    );
    let mut payload = input_to_payload(input);
    #[allow(clippy::cast_possible_truncation)] // Asserted short-aligned above.
    let short = ((input.discriminator >> 8) & 0x0F) as u8;
    payload.discriminator = Discriminator::from_short(short).unwrap();
    payload
}

fn load_fixtures(prefix: &str) -> Vec<(String, Fixture)> {
    let dir = fixtures_dir();
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).expect("read fixtures dir") {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.starts_with(prefix) || !name.ends_with(".json") {
            continue;
        }
        let bytes = fs::read(&path).expect("read fixture");
        let f: Fixture = serde_json::from_slice(&bytes).expect("fixture JSON shape");
        out.push((name, f));
    }
    assert!(!out.is_empty(), "no fixtures matched prefix `{prefix}`");
    out
}

#[test]
fn qr_encode_matches_matterjs() {
    for (name, fixture) in load_fixtures("qr-") {
        let payload = input_to_payload(&fixture.input);
        let expected_qr = fixture.expected.qr.as_ref().unwrap_or_else(|| {
            panic!("fixture {name} ({}) is missing expected.qr", fixture.intent)
        });
        let got = encode_qr(&payload)
            .unwrap_or_else(|e| panic!("encode_qr failed for {name} ({}): {e}", fixture.intent));
        assert_eq!(
            &got, expected_qr,
            "encode_qr byte-parity mismatch for {name} ({})",
            fixture.intent
        );
    }
}

#[test]
fn qr_decode_matches_matterjs() {
    for (name, fixture) in load_fixtures("qr-") {
        let expected_qr = fixture.expected.qr.as_ref().unwrap();
        let payload = parse_qr(expected_qr)
            .unwrap_or_else(|e| panic!("parse_qr failed for {name} ({}): {e}", fixture.intent));
        let expected = input_to_payload(&fixture.input);
        assert_eq!(payload, expected, "parse_qr value mismatch for {name}");
    }
}

#[test]
fn manual_encode_matches_matterjs() {
    for (name, fixture) in load_fixtures("manual-") {
        let payload = manual_input_to_payload(&fixture.input);
        let expected_manual = fixture
            .expected
            .manual
            .as_ref()
            .unwrap_or_else(|| panic!("fixture {name} is missing expected.manual"));
        let got = encode_manual_code(&payload);
        assert_eq!(
            &got, expected_manual,
            "encode_manual_code byte-parity mismatch for {name} ({})",
            fixture.intent
        );
    }
}

#[test]
fn manual_decode_matches_matterjs() {
    for (name, fixture) in load_fixtures("manual-") {
        let expected_manual = fixture.expected.manual.as_ref().unwrap();
        let payload = parse_manual_code(expected_manual)
            .unwrap_or_else(|e| panic!("parse_manual_code failed for {name}: {e}"));
        let expected = manual_input_to_payload(&fixture.input);
        assert_eq!(
            payload, expected,
            "parse_manual_code value mismatch for {name}"
        );
    }
}
