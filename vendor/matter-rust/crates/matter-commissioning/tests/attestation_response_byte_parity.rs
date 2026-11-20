//! Byte-parity integration test — Rust `verify_attestation_response`
//! must agree with matter.js's `NodeJsStyleCrypto.verifyEcdsa` on the
//! accept/reject verdict for every tuple in the captured fixture.
//!
//! Fixture provenance: `xtask/scripts/capture-attestation/index.js`.
//! Fixture path: `test-vectors/attestation/response/happy-path.json`.
//! Regenerate with `cargo xtask capture-attestation` (requires
//! `npm install` in the script dir first).
//!
//! Verdict semantics:
//!
//!   `matter_js_verify` == "accept"  ==>  Rust returns Ok(())
//!   `matter_js_verify` == "reject"  ==>  Rust returns Err(BadResponseSignature)
//!
//! Anything else is a byte-parity violation.

use matter_commissioning::attestation::{
    verify_attestation_response, AttestationError, AttestationResponse,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    scenario: String,
    inputs: Inputs,
    matter_js_verify: Verdict,
    mutations: Vec<Mutation>,
}

#[derive(Debug, Deserialize)]
#[allow(clippy::struct_field_names)] // Field names mirror JSON keys verbatim; renaming would diverge from the fixture schema.
struct Inputs {
    dac_public_key_hex: String,
    attestation_elements_hex: String,
    attestation_challenge_hex: String,
    signature_hex: String,
}

#[derive(Debug, Deserialize)]
struct Mutation {
    name: String,
    patch: Patch,
    matter_js_verify: Verdict,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "field")]
#[allow(non_camel_case_types)]
enum Patch {
    signature_hex { byte_index: usize, xor: u8 },
    attestation_challenge_hex { byte_index: usize, xor: u8 },
    attestation_elements_hex { byte_index: usize, xor: u8 },
    dac_public_key_hex { replace_hex: String },
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Verdict {
    Accept,
    Reject,
}

const FIXTURE: &str = include_str!("../../../test-vectors/attestation/response/happy-path.json");

#[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#[allow(clippy::cast_possible_truncation)] // `to_digit(16)` is always 0..=15; truncation to u8 is safe.
fn hex_to_vec(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char)
            .to_digit(16)
            .expect("fixture hex must be valid lowercase hex") as u8;
        let lo = (bytes[i + 1] as char)
            .to_digit(16)
            .expect("fixture hex must be valid lowercase hex") as u8;
        out.push((hi << 4) | lo);
        i += 2;
    }
    out
}

#[test]
#[allow(clippy::expect_used, clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
fn rust_verifier_agrees_with_matter_js_on_happy_path_and_mutations() {
    let fx: Fixture = serde_json::from_str(FIXTURE).expect("fixture JSON parses");
    assert_eq!(fx.scenario, "happy-path");

    let pubkey = hex_to_vec(&fx.inputs.dac_public_key_hex);
    let elements = hex_to_vec(&fx.inputs.attestation_elements_hex);
    let challenge_vec = hex_to_vec(&fx.inputs.attestation_challenge_hex);
    let signature_vec = hex_to_vec(&fx.inputs.signature_hex);

    assert_eq!(
        pubkey.len(),
        65,
        "DAC public key must be 65 bytes (SEC1 uncompressed)"
    );
    assert_eq!(challenge_vec.len(), 16, "challenge must be 16 bytes");
    assert_eq!(signature_vec.len(), 64, "signature must be 64 bytes (r||s)");

    let mut challenge = [0u8; 16];
    challenge.copy_from_slice(&challenge_vec);
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&signature_vec);

    // ── Happy path ──
    let response = AttestationResponse {
        attestation_elements: elements.clone(),
        signature,
    };
    let result = verify_attestation_response(&response, &challenge, &pubkey);
    match fx.matter_js_verify {
        Verdict::Accept => result.expect("matter.js accepted; Rust must accept"),
        Verdict::Reject => assert!(
            matches!(result, Err(AttestationError::BadResponseSignature)),
            "matter.js rejected; Rust must reject with BadResponseSignature"
        ),
    }

    // ── Mutations ──
    for mutation in &fx.mutations {
        let mut mut_pubkey = pubkey.clone();
        let mut mut_elements = elements.clone();
        let mut mut_challenge = challenge;
        let mut mut_signature = signature;

        match &mutation.patch {
            Patch::signature_hex { byte_index, xor } => {
                mut_signature[*byte_index] ^= xor;
            }
            Patch::attestation_challenge_hex { byte_index, xor } => {
                mut_challenge[*byte_index] ^= xor;
            }
            Patch::attestation_elements_hex { byte_index, xor } => {
                mut_elements[*byte_index] ^= xor;
            }
            Patch::dac_public_key_hex { replace_hex } => {
                mut_pubkey = hex_to_vec(replace_hex);
            }
        }

        let mut_response = AttestationResponse {
            attestation_elements: mut_elements,
            signature: mut_signature,
        };
        let mut_result = verify_attestation_response(&mut_response, &mut_challenge, &mut_pubkey);

        match mutation.matter_js_verify {
            Verdict::Accept => mut_result.unwrap_or_else(|e| {
                panic!(
                    "mutation `{}`: matter.js accepted but Rust rejected with {e:?}",
                    mutation.name
                )
            }),
            Verdict::Reject => assert!(
                matches!(mut_result, Err(AttestationError::BadResponseSignature)),
                "mutation `{}`: matter.js rejected but Rust returned {mut_result:?}",
                mutation.name
            ),
        }
    }
}
