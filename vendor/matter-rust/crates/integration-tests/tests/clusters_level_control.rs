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

const LEVEL_CONTROL: u32 = 0x0008;
const CMD_MOVE_TO_LEVEL: u32 = 0x00;
const ATTR_CURRENT_LEVEL: u32 = 0x0000;

async fn read_attr(node: &Node, ep: u16, cluster: u32, attr: u32) -> Option<Value> {
    let r = node
        .read(&[ReadPath::concrete(ep, cluster, attr)])
        .await
        .expect("read attribute");
    r.into_iter()
        .find(|(p, _)| p.attribute == attr)
        .map(|(_, v)| v)
}

/// MoveToLevel with ExecuteIfOff forced (OptionsMask=1, OptionsOverride=1) so the
/// command applies regardless of the device's OnOff state.
async fn move_to_level(node: &Node, level: u64) {
    node.invoke(
        CommandPath {
            endpoint: 1,
            cluster: LEVEL_CONTROL,
            command: CMD_MOVE_TO_LEVEL,
        },
        Value::Structure(vec![
            (Tag::Context(0), Value::Uint(level)), // Level
            (Tag::Context(1), Value::Uint(0)),     // TransitionTime = 0 (immediate)
            (Tag::Context(2), Value::Uint(1)),     // OptionsMask = ExecuteIfOff
            (Tag::Context(3), Value::Uint(1)),     // OptionsOverride = ExecuteIfOff
        ]),
    )
    .await
    .expect("invoke MoveToLevel");
}

/// LevelControl behavioral: MoveToLevel(64) → CurrentLevel==64; MoveToLevel(200) → ==200.
#[tokio::test]
async fn level_control_move_to_level() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    move_to_level(&node, 64).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        read_attr(&node, 1, LEVEL_CONTROL, ATTR_CURRENT_LEVEL).await,
        Some(Value::Uint(64)),
        "CurrentLevel did not become 64 after MoveToLevel(64)"
    );

    move_to_level(&node, 200).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        read_attr(&node, 1, LEVEL_CONTROL, ATTR_CURRENT_LEVEL).await,
        Some(Value::Uint(200)),
        "CurrentLevel did not become 200 after MoveToLevel(200)"
    );
}

/// `MoveToLevelWithOnOff(level)` with the regenerated encoder: no transition,
/// no option overrides (the `WithOnOff` variants ignore Options anyway).
async fn move_to_level_with_on_off(node: &Node, level: u8) -> matter_controller::InvokeResult {
    use matter_clusters::clusters::level_control;
    use matter_clusters::types::Nullable;
    let path = CommandPath {
        endpoint: 1,
        cluster: LEVEL_CONTROL,
        command: level_control::command_id::MOVE_TO_LEVEL_WITH_ON_OFF,
    };
    let fields = level_control::encode_move_to_level_with_on_off(
        level,
        Nullable::Value(0),
        level_control::OptionsBitmap::empty(),
        level_control::OptionsBitmap::empty(),
    );
    node.invoke_tlv(path, fields)
        .await
        .expect("invoke MoveToLevelWithOnOff")
}

/// Poll ep1 until `(OnOff, CurrentLevel)` satisfies `done` or 10 s pass;
/// returns the last pair read, so the caller's assertion shows what chip
/// settled on.
async fn wait_for_on_off_and_level(
    node: &Node,
    done: impl Fn(&(Value, Value)) -> bool,
) -> (Value, Value) {
    use matter_clusters::clusters::on_off;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let got = (
            read_attr(node, 1, on_off::CLUSTER_ID, on_off::attribute_id::ON_OFF)
                .await
                .expect("OnOff in read"),
            read_attr(node, 1, LEVEL_CONTROL, ATTR_CURRENT_LEVEL)
                .await
                .expect("CurrentLevel in read"),
        );
        if done(&got) || std::time::Instant::now() >= deadline {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Regression for the dump's `members` fix (`matter-clusters` 0.6.0):
/// MoveToLevelWithOnOff carries MoveToLevel's four fields. Before the fix the
/// generated encoder took no arguments and sent an empty structure; the
/// regenerated encoder's bytes level the light to 90 and turn it on from off.
#[tokio::test]
async fn level_control_move_to_level_with_on_off_turns_on() {
    use matter_clusters::clusters::on_off;
    use matter_controller::{ImStatus, InvokeResult};

    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let off = CommandPath {
        endpoint: 1,
        cluster: on_off::CLUSTER_ID,
        command: on_off::command_id::OFF,
    };
    let r = node
        .invoke_tlv(off, on_off::encode_off())
        .await
        .expect("Off");
    assert_eq!(r, InvokeResult::Status(ImStatus::Success));
    let (on, _) = wait_for_on_off_and_level(&node, |s| s.0 == Value::Bool(false)).await;
    assert_eq!(on, Value::Bool(false), "Off must turn the light off");

    let r = move_to_level_with_on_off(&node, 90).await;
    assert_eq!(r, InvokeResult::Status(ImStatus::Success));
    assert_eq!(
        wait_for_on_off_and_level(&node, |s| *s == (Value::Bool(true), Value::Uint(90))).await,
        (Value::Bool(true), Value::Uint(90)),
        "MoveToLevelWithOnOff(90) must turn the light on at level 90"
    );
}

/// What the released bug did on a chip device (CHANGELOG, matter-clusters
/// 0.6.0 `Fixed (breaking)`): the old encoder's empty structure (`15 18`) is
/// not rejected. chip's generated `MoveToLevelWithOnOff::DecodableType::Decode`
/// does not check mandatory fields, so Level decodes as 0 (clamped to
/// MinLevel) and TransitionTime as null, and the command returns Success,
/// dims the light to MinLevel and switches it off (level-control.cpp: a
/// `WithOnOff` move that ends at MinLevel sets OnOff false). Starts from on
/// at 90 so "off" is the command's doing, and restores on at 90 afterwards.
#[tokio::test]
async fn level_control_move_to_level_with_on_off_empty_payload_turns_off() {
    use matter_clusters::clusters::level_control;
    use matter_controller::{ImStatus, InvokeResult};

    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let on_at_90 = (Value::Bool(true), Value::Uint(90));
    assert_eq!(
        move_to_level_with_on_off(&node, 90).await,
        InvokeResult::Status(ImStatus::Success)
    );
    assert_eq!(
        wait_for_on_off_and_level(&node, |s| *s == on_at_90).await,
        on_at_90
    );
    let min_level = read_attr(
        &node,
        1,
        LEVEL_CONTROL,
        level_control::attribute_id::MIN_LEVEL,
    )
    .await
    .expect("MinLevel in read");

    // The pre-fix payload, through the raw invoke path.
    let path = CommandPath {
        endpoint: 1,
        cluster: LEVEL_CONTROL,
        command: level_control::command_id::MOVE_TO_LEVEL_WITH_ON_OFF,
    };
    let r = node
        .invoke_tlv(path, vec![0x15, 0x18])
        .await
        .expect("invoke empty MoveToLevelWithOnOff");
    assert_eq!(r, InvokeResult::Status(ImStatus::Success));
    let off_at_min = (Value::Bool(false), min_level);
    assert_eq!(
        wait_for_on_off_and_level(&node, |s| *s == off_at_min).await,
        off_at_min,
        "an empty MoveToLevelWithOnOff must leave the light off at MinLevel"
    );

    // Restore: on at 90, as the fixed encoder leaves it.
    assert_eq!(
        move_to_level_with_on_off(&node, 90).await,
        InvokeResult::Status(ImStatus::Success)
    );
    assert_eq!(
        wait_for_on_off_and_level(&node, |s| *s == on_at_90).await,
        on_at_90
    );
}
