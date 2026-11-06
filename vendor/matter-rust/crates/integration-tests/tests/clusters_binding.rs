// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use matter_controller::{BindingTarget, ImStatus};

/// Binding (ep1, 0x001E) against the live DUT (G-b): write a unicast binding,
/// read it back, then restore the empty list (leave the DUT as found).
#[tokio::test]
async fn binding_write_read_restore() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // Write one unicast binding: node 0x1122, endpoint 1, cluster OnOff (0x0006).
    let target = BindingTarget::new(Some(0x1122), None, Some(1), Some(0x0006));
    let statuses = node
        .write_binding(1, std::slice::from_ref(&target))
        .await
        .expect("write_binding");
    assert!(
        statuses.iter().all(|(_, s)| matches!(s, ImStatus::Success)),
        "device rejected the binding write: {statuses:?}"
    );

    // Read it back.
    let read = node.read_binding(1).await.expect("read_binding");
    assert!(
        read.contains(&target),
        "written binding {target:?} not found in {read:?}"
    );

    // Restore: empty list.
    node.write_binding(1, &[])
        .await
        .expect("restore empty binding");
}
