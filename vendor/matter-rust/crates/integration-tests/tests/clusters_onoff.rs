// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use std::time::Duration;

use matter_controller::{CommandPath, Node, ReadPath, Value};

/// Read back the OnOff attribute (ep1, cluster 0x0006, attr 0x0000).
/// Returns `Some(bool)` when the attribute is present, `None` otherwise.
async fn read_onoff(node: &Node) -> Option<bool> {
    let r = node
        .read(&[ReadPath::concrete(1, 0x0006, 0x0000)])
        .await
        .expect("read OnOff");
    r.iter().find_map(|(p, v)| {
        if p.attribute == 0x0000 {
            if let Value::Bool(b) = v {
                return Some(*b);
            }
        }
        None
    })
}

// ── OnOff behavioral sequence (the per-cluster template) ─────────────────────

/// Exercise the OnOff cluster (ep1, cluster 0x0006) against the live DUT:
///   On  → attr must be true
///   Off → attr must be false
///   Toggle → attr must be true
///
/// This is the template for every later cluster behavioral test (H2–H4).
#[tokio::test]
async fn onoff_on_off_toggle() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // ── 1. On → read back true ────────────────────────────────────────────────
    node.invoke(
        CommandPath {
            endpoint: 1,
            cluster: 0x0006,
            command: 0x01,
        },
        Value::Structure(vec![]),
    )
    .await
    .expect("invoke OnOff::On");

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        read_onoff(&node).await,
        Some(true),
        "On did not set OnOff true"
    );

    // ── 2. Off → read back false ──────────────────────────────────────────────
    node.invoke(
        CommandPath {
            endpoint: 1,
            cluster: 0x0006,
            command: 0x00,
        },
        Value::Structure(vec![]),
    )
    .await
    .expect("invoke OnOff::Off");

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        read_onoff(&node).await,
        Some(false),
        "Off did not set OnOff false"
    );

    // ── 3. Toggle → read back true ────────────────────────────────────────────
    node.invoke(
        CommandPath {
            endpoint: 1,
            cluster: 0x0006,
            command: 0x02,
        },
        Value::Structure(vec![]),
    )
    .await
    .expect("invoke OnOff::Toggle");

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        read_onoff(&node).await,
        Some(true),
        "Toggle did not flip OnOff to true"
    );
}
