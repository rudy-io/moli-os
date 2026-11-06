// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use matter_controller::ReadPath;

#[tokio::test]
async fn dut_gate_skips_without_env() {
    // With no MATTER_INTEGRATION_DUT this must early-return (skip), not panic.
    let _cfg = integration_tests::dut_or_skip!();
    // Reaching here means a DUT IS configured; nothing to assert in the smoke test.
}

/// Commission the all-clusters-app (PASE -> dev-cert attestation -> NOC -> CASE)
/// and read BasicInformation::VendorName, proving the fixture works end-to-end.
#[tokio::test]
async fn commissions_and_reads_basic_information() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    // BasicInformation (0x0028) VendorName (0x0001) on endpoint 0.
    let reports = node
        .read(&[ReadPath::concrete(0, 0x0028, 0x0001)])
        .await
        .expect("read VendorName");
    assert!(
        !reports.is_empty(),
        "expected a VendorName attribute report"
    );
}
