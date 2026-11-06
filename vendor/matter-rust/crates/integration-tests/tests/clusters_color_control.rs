// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use std::time::Duration;

use matter_codec::Tag;
use matter_controller::{CommandPath, Node, ReadPath, Value};

const COLOR_CONTROL: u32 = 0x0300;
const CMD_MOVE_TO_COLOR_TEMPERATURE: u32 = 0x0A;
const ATTR_COLOR_TEMPERATURE_MIREDS: u32 = 0x0007;

async fn read_attr(node: &Node, ep: u16, cluster: u32, attr: u32) -> Option<Value> {
    let r = node
        .read(&[ReadPath::concrete(ep, cluster, attr)])
        .await
        .expect("read attribute");
    r.into_iter()
        .find(|(p, _)| p.attribute == attr)
        .map(|(_, v)| v)
}

/// MoveToColorTemperature with ExecuteIfOff forced so it applies regardless of OnOff.
async fn move_to_color_temp(node: &Node, mireds: u64) {
    node.invoke(
        CommandPath {
            endpoint: 1,
            cluster: COLOR_CONTROL,
            command: CMD_MOVE_TO_COLOR_TEMPERATURE,
        },
        Value::Structure(vec![
            (Tag::Context(0), Value::Uint(mireds)), // ColorTemperatureMireds
            (Tag::Context(1), Value::Uint(0)),      // TransitionTime = 0
            (Tag::Context(2), Value::Uint(1)),      // OptionsMask = ExecuteIfOff
            (Tag::Context(3), Value::Uint(1)),      // OptionsOverride = ExecuteIfOff
        ]),
    )
    .await
    .expect("invoke MoveToColorTemperature");
}

/// ColorControl behavioral: MoveToColorTemperature(250) → ColorTemperatureMireds==250;
/// then (400) → ==400.
#[tokio::test]
async fn color_control_move_to_color_temperature() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    move_to_color_temp(&node, 250).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        read_attr(&node, 1, COLOR_CONTROL, ATTR_COLOR_TEMPERATURE_MIREDS).await,
        Some(Value::Uint(250)),
        "ColorTemperatureMireds did not become 250"
    );

    move_to_color_temp(&node, 400).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        read_attr(&node, 1, COLOR_CONTROL, ATTR_COLOR_TEMPERATURE_MIREDS).await,
        Some(Value::Uint(400)),
        "ColorTemperatureMireds did not become 400"
    );
}
