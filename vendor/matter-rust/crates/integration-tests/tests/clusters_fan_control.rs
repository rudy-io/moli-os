// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use std::time::Duration;

use matter_controller::{AttributePath, ImStatus, Node, ReadPath, Value};

const FAN_CONTROL: u32 = 0x0202;
const ATTR_FAN_MODE: u32 = 0x0000;
const ATTR_PERCENT_SETTING: u32 = 0x0002;
const FAN_MODE_HIGH: u64 = 3;

async fn read_attr(node: &Node, ep: u16, cluster: u32, attr: u32) -> Option<Value> {
    let r = node
        .read(&[ReadPath::concrete(ep, cluster, attr)])
        .await
        .expect("read attribute");
    r.into_iter()
        .find(|(p, _)| p.attribute == attr)
        .map(|(_, v)| v)
}

async fn write_attr(node: &Node, ep: u16, cluster: u32, attr: u32, value: Value) {
    let statuses = node
        .write(&[(
            AttributePath {
                endpoint: ep,
                cluster,
                attribute: attr,
            },
            value,
        )])
        .await
        .expect("write attribute");
    for (_, s) in &statuses {
        assert!(
            matches!(s, ImStatus::Success),
            "attribute write status not Success: {s:?}"
        );
    }
}

/// FanControl behavioral: write FanMode=High and PercentSetting=50, read each back.
#[tokio::test]
async fn fan_control_mode_and_percent() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    write_attr(
        &node,
        1,
        FAN_CONTROL,
        ATTR_FAN_MODE,
        Value::Uint(FAN_MODE_HIGH),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        read_attr(&node, 1, FAN_CONTROL, ATTR_FAN_MODE).await,
        Some(Value::Uint(FAN_MODE_HIGH)),
        "FanMode did not round-trip to High(3)"
    );

    write_attr(&node, 1, FAN_CONTROL, ATTR_PERCENT_SETTING, Value::Uint(50)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        read_attr(&node, 1, FAN_CONTROL, ATTR_PERCENT_SETTING).await,
        Some(Value::Uint(50)),
        "PercentSetting did not round-trip to 50"
    );
}
