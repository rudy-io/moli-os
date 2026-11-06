//! Local PASE roundtrip tests — drive [`PaseProver`] and [`PaseVerifier`]
//! against each other and confirm shared session keys.
//!
//! This file is the M3.2 correctness gate. If it passes, the two
//! state machines agree on:
//!   - The full 5-message handshake (negotiation path).
//!   - The 3-message handshake (known-params path).
//!   - Derived session keys (`ke`, `i2r_key`, `r2i_key`, `attestation_key`).
//!
//! matter.js byte-parity is M3.3's job.

// Test-code carve-out: unwrap/expect are acceptable in integration tests.
// See CONTRIBUTING.md for the policy.
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use matter_crypto::{Error, PasePbkdfParams, PaseProver, PaseVerifier};

fn default_params() -> PasePbkdfParams {
    PasePbkdfParams {
        iterations: 1_000,
        salt: vec![0x42u8; 16],
    }
}

#[test]
fn pase_roundtrip_with_negotiation_default_params() {
    let pin = 20_202_021_u32;
    let params = default_params();

    let mut prover = PaseProver::new_with_negotiation(pin, 0x0001).unwrap();
    let mut verifier = PaseVerifier::new_from_pin(pin, params, 0x0002).unwrap();

    let m = prover.start().expect("start produces PBKDFParamRequest");
    verifier
        .handle_pbkdf_request(&m)
        .expect("verifier accepts request");

    let m = verifier.next_message().expect("verifier produces response");
    prover
        .handle_pbkdf_response(&m)
        .expect("prover accepts response");

    let m = prover.next_message().expect("prover produces Pake1");
    verifier.handle_pake1(&m).expect("verifier accepts Pake1");

    let m = verifier.next_message().expect("verifier produces Pake2");
    prover.handle_pake2(&m).expect("prover accepts Pake2");

    let m = prover.next_message().expect("prover produces Pake3");
    verifier.handle_pake3(&m).expect("verifier accepts Pake3");

    let prover_keys = prover.finish().expect("prover finishes");
    let verifier_keys = verifier.finish().expect("verifier finishes");

    assert_eq!(prover_keys.ke, verifier_keys.ke, "ke must match");
    assert_eq!(
        prover_keys.i2r_key, verifier_keys.i2r_key,
        "i2r_key must match"
    );
    assert_eq!(
        prover_keys.r2i_key, verifier_keys.r2i_key,
        "r2i_key must match"
    );
    assert_eq!(
        prover_keys.attestation_key, verifier_keys.attestation_key,
        "attestation_key must match"
    );
}

#[test]
fn pase_roundtrip_with_known_params_skips_pbkdf_negotiation() {
    let pin = 20_202_021_u32;
    let params = default_params();

    let mut prover = PaseProver::new_with_known_params(pin, params.clone(), 0x0001).unwrap();
    let mut verifier = PaseVerifier::new_from_pin(pin, params, 0x0002).unwrap();

    let m = prover.start().expect("start produces Pake1");
    verifier
        .handle_pake1(&m)
        .expect("verifier accepts Pake1 directly");

    let m = verifier.next_message().expect("verifier produces Pake2");
    prover.handle_pake2(&m).expect("prover accepts Pake2");

    let m = prover.next_message().expect("prover produces Pake3");
    verifier.handle_pake3(&m).expect("verifier accepts Pake3");

    let prover_keys = prover.finish().unwrap();
    let verifier_keys = verifier.finish().unwrap();

    assert_eq!(prover_keys.ke, verifier_keys.ke);
    assert_eq!(prover_keys.attestation_key, verifier_keys.attestation_key);
}

#[test]
fn pase_roundtrip_with_max_iterations() {
    let pin = 12_345_678_u32;
    let params = PasePbkdfParams {
        iterations: 100_000,
        salt: vec![0xABu8; 32],
    };

    let mut prover = PaseProver::new_with_negotiation(pin, 0x0001).unwrap();
    let mut verifier = PaseVerifier::new_from_pin(pin, params, 0x0002).unwrap();

    let m = prover.start().unwrap();
    verifier.handle_pbkdf_request(&m).unwrap();
    let m = verifier.next_message().unwrap();
    prover.handle_pbkdf_response(&m).unwrap();
    let m = prover.next_message().unwrap();
    verifier.handle_pake1(&m).unwrap();
    let m = verifier.next_message().unwrap();
    prover.handle_pake2(&m).unwrap();
    let m = prover.next_message().unwrap();
    verifier.handle_pake3(&m).unwrap();

    assert_eq!(prover.finish().unwrap().ke, verifier.finish().unwrap().ke);
}

#[test]
fn pase_roundtrip_with_wrong_pin_returns_tag_mismatch() {
    // Verifier uses the correct PIN; prover uses a wrong PIN.
    // Pake2's cB must not verify on the prover side.
    let params = default_params();

    let mut prover = PaseProver::new_with_known_params(99_999_999, params.clone(), 0x0001).unwrap();
    let mut verifier = PaseVerifier::new_from_pin(20_202_021, params, 0x0002).unwrap();

    let m = prover.start().unwrap();
    verifier.handle_pake1(&m).unwrap();
    let m = verifier.next_message().unwrap();

    let err = prover.handle_pake2(&m).unwrap_err();
    assert!(
        matches!(err, Error::ConfirmationTagMismatch),
        "expected ConfirmationTagMismatch, got {err:?}",
    );
}

#[test]
fn pase_roundtrip_out_of_order_call_returns_unexpected_message() {
    let pin = 20_202_021_u32;
    let params = default_params();

    let mut prover = PaseProver::new_with_known_params(pin, params, 0x0001).unwrap();
    // Skip start; try to feed Pake2 directly — must error.
    let dummy_pake2 = vec![0u8; 100]; // any bytes; the state check fires first
    let err = prover.handle_pake2(&dummy_pake2).unwrap_err();
    assert!(matches!(err, Error::UnexpectedMessage { .. }));
}

// ── Property-based tests ──────────────────────────────────────────────────────
//
// proptest is already in [dev-dependencies] (workspace = true).
// We cap `cases` at 16 because each case runs PBKDF2 (1 000 iterations) plus
// four P-256 scalar multiplications — a full SPAKE2+ exchange.  That keeps the
// per-PR wall-clock cost reasonable while still exercising a broad PIN space.

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 16,
        ..ProptestConfig::default()
    })]

    /// Any valid Matter setup PIN in [1, 99_999_998] must drive a clean
    /// known-params PASE handshake with both sides deriving the same `ke`.
    ///
    /// 99_999_999 is reserved by the Matter spec (§5.1.6 — the all-9s passcode
    /// pattern is explicitly invalid), so the range stops at 99_999_998.
    #[test]
    fn random_pin_roundtrips(pin in 1u32..=99_999_998) {
        let params = PasePbkdfParams {
            iterations: 1_000,
            salt: vec![0x42u8; 16],
        };
        let mut prover = PaseProver::new_with_known_params(pin, params.clone(), 0x0001)
            .expect("prover construction");
        let mut verifier = PaseVerifier::new_from_pin(pin, params, 0x0002)
            .expect("verifier construction");

        let pake1 = prover.start().expect("prover.start");
        verifier.handle_pake1(&pake1).expect("verifier handles Pake1");
        let pake2 = verifier.next_message().expect("verifier sends Pake2");
        prover.handle_pake2(&pake2).expect("prover handles Pake2");
        let pake3 = prover.next_message().expect("prover sends Pake3");
        verifier.handle_pake3(&pake3).expect("verifier handles Pake3");

        let pk = prover.finish().expect("prover finishes");
        let vk = verifier.finish().expect("verifier finishes");
        prop_assert_eq!(pk.ke, vk.ke);
    }

    /// Flipping a single bit in a serialised Pake2 message must return an
    /// error from `handle_pake2(...)`, never cause a panic.
    ///
    /// This covers the same state-machine surface that a fuzz target would
    /// exercise, without requiring the `cargo-fuzz` toolchain on every PR
    /// (fuzzing runs on a weekly CI schedule).
    #[test]
    fn random_byte_flip_in_pake2_never_panics(
        pin in 1u32..=99_999_998,
        flip_offset in any::<usize>(),
        flip_bit in 0u8..8,
    ) {
        let params = PasePbkdfParams {
            iterations: 1_000,
            salt: vec![0x42u8; 16],
        };
        let mut prover = PaseProver::new_with_known_params(pin, params.clone(), 0x0001)
            .expect("prover construction");
        let mut verifier = PaseVerifier::new_from_pin(pin, params, 0x0002)
            .expect("verifier construction");

        let pake1 = prover.start().expect("prover.start");
        verifier.handle_pake1(&pake1).expect("verifier handles Pake1");
        let mut pake2 = verifier.next_message().expect("verifier sends Pake2");

        // Only flip if the message is non-empty (it should never be for a
        // well-formed verifier, but guard defensively).
        if !pake2.is_empty() {
            let off = flip_offset % pake2.len();
            pake2[off] ^= 1 << flip_bit;
        }

        // Either returns Ok(()) (astronomically unlikely but not impossible for
        // a lucky flip) or returns an Err — it must never panic.
        let _ = prover.handle_pake2(&pake2);
    }
}

#[test]
fn pase_roundtrip_threads_session_ids_end_to_end() {
    let pin = 20_202_021;
    let params = PasePbkdfParams {
        iterations: 1000,
        salt: vec![0x42; 16],
    };

    let mut prover = PaseProver::new_with_negotiation(pin, 0x00AA).unwrap();
    let mut verifier = PaseVerifier::new_from_pin(pin, params, 0x00BB).unwrap();

    let req = prover.start().unwrap();
    verifier.handle_pbkdf_request(&req).unwrap();
    let resp = verifier.next_message().unwrap();
    prover.handle_pbkdf_response(&resp).unwrap();

    // The prover learned the responder's id; it is what the device advertised.
    assert_eq!(prover.responder_session_id(), Some(0x00BB));

    let pake1 = prover.next_message().unwrap();
    verifier.handle_pake1(&pake1).unwrap();
    let pake2 = verifier.next_message().unwrap();
    prover.handle_pake2(&pake2).unwrap();
    let pake3 = prover.next_message().unwrap();
    verifier.handle_pake3(&pake3).unwrap();

    let prover_keys = prover.finish().unwrap();
    let verifier_keys = verifier.finish().unwrap();
    assert_eq!(prover_keys.i2r_key, verifier_keys.i2r_key);
    assert_eq!(prover_keys.r2i_key, verifier_keys.r2i_key);
}
