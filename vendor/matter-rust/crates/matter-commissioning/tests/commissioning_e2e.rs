//! End-to-end happy path through the PUBLIC state-machine API, driven by
//! the captured matter.js fixture (`xtask capture-commissioning`): every
//! stage's response is the real captured device's bytes, attestation runs
//! the full verifier chain, and the walk must terminate in `Action::Done`
//! with the commissioned fabric's identity matching the fixture.
//!
//! Byte-parity on the emitted payloads is asserted separately by
//! `commissioning_byte_parity.rs`; this test pins COMPLETION via the same
//! public surface a production driver uses.

// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod e2e_fixture;

use matter_commissioning::state_machine::Action;

#[test]
fn happy_path_reaches_done() {
    let Some(fixture) = e2e_fixture::load("happy-path.json") else {
        return; // SKIP already printed
    };
    let harness = e2e_fixture::Harness::from_fixture(&fixture);
    let mut sm = harness.commissioner();

    e2e_fixture::drive(&mut sm, &fixture.stages, None)
        .unwrap_or_else(|(stage, e)| panic!("walk failed at stage {stage}: {e}"));

    match sm.poll().expect("final poll") {
        Action::Done(cf) => {
            assert_eq!(
                cf.peer_node_id,
                e2e_fixture::parse_hex_u64(&fixture.assigned_node_id),
                "commissioned peer node id must match the fixture"
            );
            assert_eq!(
                cf.fabric.fabric_id,
                e2e_fixture::parse_hex_u64(&fixture.fabric_id),
                "commissioned fabric id must match the fixture"
            );
        }
        other => panic!("expected Done after walking the fixture, got {other:?}"),
    }
}
