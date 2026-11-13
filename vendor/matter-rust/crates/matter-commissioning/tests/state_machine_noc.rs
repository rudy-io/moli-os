//! M6.4.4 — the state machine drives through the CSR + NOC flow on the
//! captured matter.js fixture: the real captured `CSRResponse` passes
//! `ValidateCsr` (self-signature, nonce echo, DAC attestation signature),
//! `GenerateNocChain` mints a NOC under this run's fabric, and after the
//! `AddNOC` response the cursor reaches `ReadNetworkCommissioningInfo`
//! (the next emitted action reads `NetworkCommissioning::FeatureMap`).

// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod e2e_fixture;

use matter_commissioning::state_machine::Action;

#[test]
fn happy_path_drives_through_csr_and_noc_to_read_network_commissioning_info() {
    let Some(fixture) = e2e_fixture::load("happy-path.json") else {
        return; // SKIP already printed
    };
    let harness = e2e_fixture::Harness::from_fixture(&fixture);
    let mut sm = harness.commissioner();

    // Walk through the NOCResponse; the next poll must emit the
    // NetworkCommissioning FeatureMap read.
    e2e_fixture::drive(&mut sm, &fixture.stages, Some("SendNoc"))
        .unwrap_or_else(|(stage, e)| panic!("walk failed at stage {stage}: {e}"));

    match sm.poll().expect("post-NOC poll") {
        Action::ReadAttribute {
            cluster,
            attributes,
            ..
        } => {
            assert_eq!(cluster, 0x0031, "reads NetworkCommissioning");
            assert_eq!(
                attributes,
                // FeatureMap (0xFFFC) + ConnectMaxTimeSeconds (0x0003);
                // the latter sizes the FailsafeBeforeNetworkEnable
                // extension.
                &[0xFFFC, 0x0003],
                "reads FeatureMap + ConnectMaxTimeSeconds"
            );
        }
        other => panic!("expected the FeatureMap read after AddNOC, got {other:?}"),
    }
}
