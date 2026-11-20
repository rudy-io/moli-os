//! matter.js byte-parity tests for PASE.
//!
//! Each test loads a captured handshake JSON fixture from
//! `test-vectors/pase/` and replays it through our [`PaseProver`] and
//! [`PaseVerifier`] state machines, asserting byte-identical TLV output
//! at every step.
//!
//! Fixtures are produced by `cargo xtask capture-pase`. All three scenarios
//! drive the full 5-message negotiation path (`PbkdfParamRequest` →
//! `PbkdfParamResponse` → `Pake1` → `Pake2` → `Pake3`).
//!
//! # Why all five messages?
//!
//! The capture script always performs the negotiation path because matter.js's
//! `PaseClient` always requests PBKDF params even when they could be cached.
//! So all three fixtures include `pbkdf_param_request_hex` and
//! `pbkdf_param_response_hex`.
//!
//! # Failure diagnosis
//!
//! If a test fails, the assertion output shows both the expected (matter.js)
//! and actual (our) hex strings so you can compare byte-by-byte. Likely
//! culprits are TLV field ordering, session ID injection, or `responder_random`
//! mismatch in `PBKDFParamResponse`.

#![allow(clippy::expect_used)] // Test code: expect is allowed in tests (CONTRIBUTING.md).
#![allow(clippy::doc_markdown)] // test module docs need not link every identifier
#![allow(clippy::struct_field_names)] // fixture field names must match JSON keys verbatim

use std::fs;
use std::path::PathBuf;

use matter_crypto::test_support::{
    prover_with_scalar_random_and_session_id, verifier_with_scalar_and_random,
};
use serde::Deserialize;

// =============================================================================
// Fixture types — field names match the JSON produced by index.js
// =============================================================================

#[derive(Debug, Deserialize)]
struct Fixture {
    #[allow(dead_code)]
    scenario: String,
    inputs: FixtureInputs,
    intermediates: FixtureIntermediates,
    messages: FixtureMessages,
}

#[derive(Debug, Deserialize)]
struct FixtureInputs {
    pin: u32,
    iterations: u32,
    salt_hex: String,
    initiator_session_id: u16,
    responder_session_id: u16,
    initiator_random_hex: String,
    responder_random_hex: String,
    x_scalar_hex: String,
    y_scalar_hex: String,
}

#[derive(Debug, Deserialize)]
struct FixtureIntermediates {
    w0_hex: String,
    #[serde(rename = "L_hex")]
    l_hex: String,
}

#[derive(Debug, Deserialize)]
struct FixtureMessages {
    pbkdf_param_request_hex: String,
    pbkdf_param_response_hex: String,
    pake1_hex: String,
    pake2_hex: String,
    pake3_hex: String,
}

// =============================================================================
// Helpers
// =============================================================================

fn fixture_path(scenario: &str) -> PathBuf {
    // Integration tests run with cwd = workspace root (for `cargo test`) or
    // the crate directory (for `cargo test -p`). Resolve relative to the
    // manifest directory so we always find the fixtures.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..") // crates/matter-crypto -> crates
        .join("..") // crates -> repo root
        .join("test-vectors")
        .join("pase")
        .join(format!("{scenario}.json"))
}

fn load_fixture(scenario: &str) -> Fixture {
    let path = fixture_path(scenario);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read fixture {}: {} — run `cargo xtask capture-pase` to regenerate",
            path.display(),
            e,
        )
    });
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("malformed fixture {}: {}", path.display(), e))
}

fn decode_hex_32(hex: &str) -> [u8; 32] {
    let bytes = hex::decode(hex).expect("valid hex string");
    bytes
        .try_into()
        .unwrap_or_else(|v: Vec<u8>| panic!("expected 32 bytes, got {}", v.len()))
}

fn decode_hex_65(hex: &str) -> [u8; 65] {
    let bytes = hex::decode(hex).expect("valid hex string");
    bytes
        .try_into()
        .unwrap_or_else(|v: Vec<u8>| panic!("expected 65 bytes, got {}", v.len()))
}

fn decode_hex_vec(hex: &str) -> Vec<u8> {
    hex::decode(hex).expect("valid hex string")
}

/// Assert byte equality and print a diff if the assertion fails.
fn assert_bytes_eq(label: &str, ours: &[u8], expected_hex: &str) {
    let ours_hex = hex::encode(ours);
    if ours_hex != expected_hex {
        // Print both sides for easy diff; show the first differing nibble.
        eprintln!("MISMATCH [{label}]");
        eprintln!("  ours:   {ours_hex}");
        eprintln!("  matter: {expected_hex}");
        for (i, (a, b)) in ours_hex
            .as_bytes()
            .iter()
            .zip(expected_hex.as_bytes().iter())
            .enumerate()
        {
            if a != b {
                eprintln!(
                    "  first diff at hex-char index {i} (byte {}, nibble {})",
                    i / 2,
                    i % 2
                );
                break;
            }
        }
        if ours_hex.len() != expected_hex.len() {
            eprintln!(
                "  length diff: ours {} chars ({} bytes), matter {} chars ({} bytes)",
                ours_hex.len(),
                ours_hex.len() / 2,
                expected_hex.len(),
                expected_hex.len() / 2,
            );
        }
        panic!("byte-parity assertion failed for {label}");
    }
}

// =============================================================================
// Full 5-message handshake driver
// =============================================================================

/// Drive a complete negotiation-path handshake through our state machines and
/// assert byte-identical output for every message.
fn run_full_handshake(fx: &Fixture) {
    use matter_crypto::PasePbkdfParams;

    let params = PasePbkdfParams {
        iterations: fx.inputs.iterations,
        salt: decode_hex_vec(&fx.inputs.salt_hex),
    };

    let x_scalar = decode_hex_32(&fx.inputs.x_scalar_hex);
    let y_scalar = decode_hex_32(&fx.inputs.y_scalar_hex);
    let initiator_random = decode_hex_32(&fx.inputs.initiator_random_hex);
    let responder_random = decode_hex_32(&fx.inputs.responder_random_hex);
    let w0 = decode_hex_32(&fx.intermediates.w0_hex);
    let l = decode_hex_65(&fx.intermediates.l_hex);

    // --- Prover construction (negotiation path) ---
    let mut prover = prover_with_scalar_random_and_session_id(
        fx.inputs.pin,
        x_scalar,
        initiator_random,
        fx.inputs.initiator_session_id,
    )
    .expect("prover construction must succeed");

    // --- Verifier construction (w0/L already known) ---
    let mut verifier = verifier_with_scalar_and_random(
        w0,
        l,
        params,
        y_scalar,
        responder_random,
        fx.inputs.responder_session_id,
    )
    .expect("verifier construction must succeed");

    // Step 1: prover emits PbkdfParamRequest.
    let req = prover.start().expect("prover.start()");
    assert_bytes_eq(
        "PbkdfParamRequest",
        &req,
        &fx.messages.pbkdf_param_request_hex,
    );

    // Step 2: verifier receives request, emits PbkdfParamResponse.
    verifier
        .handle_pbkdf_request(&req)
        .expect("verifier.handle_pbkdf_request()");
    let resp = verifier
        .next_message()
        .expect("verifier.next_message() [PbkdfParamResponse]");
    assert_bytes_eq(
        "PbkdfParamResponse",
        &resp,
        &fx.messages.pbkdf_param_response_hex,
    );

    // Step 3: prover receives response, emits Pake1.
    prover
        .handle_pbkdf_response(&resp)
        .expect("prover.handle_pbkdf_response()");
    let pake1 = prover
        .next_message()
        .expect("prover.next_message() [Pake1]");
    assert_bytes_eq("Pake1", &pake1, &fx.messages.pake1_hex);

    // Step 4: verifier receives Pake1, emits Pake2.
    verifier
        .handle_pake1(&pake1)
        .expect("verifier.handle_pake1()");
    let pake2 = verifier
        .next_message()
        .expect("verifier.next_message() [Pake2]");
    assert_bytes_eq("Pake2", &pake2, &fx.messages.pake2_hex);

    // Step 5: prover receives Pake2, emits Pake3.
    prover.handle_pake2(&pake2).expect("prover.handle_pake2()");
    let pake3 = prover
        .next_message()
        .expect("prover.next_message() [Pake3]");
    assert_bytes_eq("Pake3", &pake3, &fx.messages.pake3_hex);

    // Step 6: verifier receives Pake3, verifies cA.
    verifier
        .handle_pake3(&pake3)
        .expect("verifier.handle_pake3()");

    // Final: both sides derive the same session Ke.
    let pk = prover.finish().expect("prover.finish()");
    let vk = verifier.finish().expect("verifier.finish()");
    assert_eq!(pk.ke, vk.ke, "prover and verifier must agree on session Ke");
}

// =============================================================================
// Tests
// =============================================================================

/// Scenario: PIN 20202021, 1000 iterations, 16-byte all-0x42 salt.
/// Baseline: smallest valid parameters, session IDs 1/2.
#[test]
fn matter_js_byte_parity_negotiation() {
    let fx = load_fixture("handshake-negotiation");
    run_full_handshake(&fx);
}

/// Scenario: PIN 123456, 2000 iterations, 24-byte alternating salt.
/// Exercises different PIN and non-minimum salt length.
#[test]
fn matter_js_byte_parity_known_params() {
    let fx = load_fixture("handshake-known-params");
    run_full_handshake(&fx);
}

/// Scenario: PIN 20202021, 100 000 iterations, 32-byte all-0x55 salt.
/// Exercises PBKDF2 under maximum iteration count.
#[test]
fn matter_js_byte_parity_max_iter() {
    let fx = load_fixture("handshake-max-iter");
    run_full_handshake(&fx);
}
