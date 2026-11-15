//! M6.4.3 — the state machine reaches `AttestationVerification`, runs the
//! full verifier chain (DAC/PAI chain validation against the CSA test PAA,
//! attestation signature over elements ‖ challenge, CD verification, nonce
//! echo) on the CAPTURED matter.js device's real attestation materials, and
//! advances past attestation: the next emitted action must be the
//! `CSRRequest` invoke.

// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod e2e_fixture;

use matter_commissioning::state_machine::Action;

#[test]
fn happy_path_advances_past_attestation_verification() {
    let Some(fixture) = e2e_fixture::load("happy-path.json") else {
        return; // SKIP already printed
    };
    let harness = e2e_fixture::Harness::from_fixture(&fixture);
    let mut sm = harness.commissioner();

    // Walk through the AttestationResponse (fed by `drive`); the NEXT poll
    // performs the off-wire AttestationVerification and, on success, emits
    // the CSRRequest invoke.
    e2e_fixture::drive(&mut sm, &fixture.stages, Some("SendAttestationRequest"))
        .unwrap_or_else(|(stage, e)| panic!("walk failed at stage {stage}: {e}"));

    match sm.poll().expect("attestation verification must pass") {
        Action::Invoke {
            cluster, command, ..
        } => {
            assert_eq!(cluster, 0x003E, "next invoke is OperationalCredentials");
            assert_eq!(command, 0x04, "next invoke is CSRRequest");
        }
        other => panic!("expected the CSRRequest invoke after attestation, got {other:?}"),
    }
}
