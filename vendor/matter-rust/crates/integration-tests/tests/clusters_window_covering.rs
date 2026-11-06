// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use std::time::Duration;

use integration_tests::sweep::{attribute_tlv, invoke_for_status, read_cluster_attributes};
use matter_clusters::clusters::window_covering::{self as wc, attribute_id as a};
use matter_codec::Tag;
use matter_controller::{CommandPath, ImStatus, Node, ReadPath, Value};

const WINDOW_COVERING: u32 = 0x0102;
const CMD_GO_TO_LIFT_PERCENTAGE: u32 = 0x05;
const ATTR_TARGET_POSITION_LIFT_PERCENT100THS: u32 = 0x000B;

async fn read_attr(node: &Node, ep: u16, cluster: u32, attr: u32) -> Option<Value> {
    let r = node
        .read(&[ReadPath::concrete(ep, cluster, attr)])
        .await
        .expect("read attribute");
    r.into_iter()
        .find(|(p, _)| p.attribute == attr)
        .map(|(_, v)| v)
}

async fn go_to_lift_percentage(node: &Node, percent100ths: u64) {
    node.invoke(
        CommandPath {
            endpoint: 1,
            cluster: WINDOW_COVERING,
            command: CMD_GO_TO_LIFT_PERCENTAGE,
        },
        Value::Structure(vec![(Tag::Context(0), Value::Uint(percent100ths))]),
    )
    .await
    .expect("invoke GoToLiftPercentage");
}

/// WindowCovering behavioral: GoToLiftPercentage sets the TARGET position
/// immediately. We assert TargetPositionLiftPercent100ths (deterministic) rather
/// than CurrentPosition (which simulates movement over time on all-clusters-app).
#[tokio::test]
async fn window_covering_go_to_lift_percentage() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    go_to_lift_percentage(&node, 5000).await; // 50.00 %
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        read_attr(
            &node,
            1,
            WINDOW_COVERING,
            ATTR_TARGET_POSITION_LIFT_PERCENT100THS
        )
        .await,
        Some(Value::Uint(5000)),
        "TargetPositionLiftPercent100ths did not become 5000"
    );

    go_to_lift_percentage(&node, 2500).await; // 25.00 %
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        read_attr(
            &node,
            1,
            WINDOW_COVERING,
            ATTR_TARGET_POSITION_LIFT_PERCENT100THS
        )
        .await,
        Some(Value::Uint(2500)),
        "TargetPositionLiftPercent100ths did not become 2500"
    );
}

/// The Matter 1.4 absolute-position (ABS) elements, generated from
/// the dump's 1.4 supplement. all-clusters serves the eight ABS attributes on
/// endpoint 1 at master and v1.4.2.0 (all-clusters-app.matter) although its
/// featureMap (0x17: LF, TL, PA_LF, PA_TL) leaves ABS out; each decodes with
/// the generated decoder, the physical closed and installed limits at their
/// .matter defaults (nothing writes them). GoToLiftValue / GoToTiltValue reach chip's handler
/// (a malformed payload would be InvalidCommand) and are refused with Failure
/// because ABS is not set (`emberAfWindowCoveringClusterGoToLiftValueCallback`,
/// the same at both refs), leaving the lift and tilt target positions
/// unchanged.
#[tokio::test]
async fn window_covering_absolute_position_decodes_and_go_to_value_is_refused() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: needs the all-clusters DUT");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    let attrs = read_cluster_attributes(&node, 1, WINDOW_COVERING)
        .await
        .unwrap();
    let tlv = |id| attribute_tlv(&attrs, id);
    let limits = [
        wc::decode_physical_closed_limit_lift(tlv(a::PHYSICAL_CLOSED_LIMIT_LIFT)).unwrap(),
        wc::decode_physical_closed_limit_tilt(tlv(a::PHYSICAL_CLOSED_LIMIT_TILT)).unwrap(),
        wc::decode_installed_open_limit_lift(tlv(a::INSTALLED_OPEN_LIMIT_LIFT)).unwrap(),
        wc::decode_installed_closed_limit_lift(tlv(a::INSTALLED_CLOSED_LIMIT_LIFT)).unwrap(),
        wc::decode_installed_open_limit_tilt(tlv(a::INSTALLED_OPEN_LIMIT_TILT)).unwrap(),
        wc::decode_installed_closed_limit_tilt(tlv(a::INSTALLED_CLOSED_LIMIT_TILT)).unwrap(),
    ];
    let current = (
        wc::decode_current_position_lift(tlv(a::CURRENT_POSITION_LIFT)).unwrap(),
        wc::decode_current_position_tilt(tlv(a::CURRENT_POSITION_TILT)).unwrap(),
    );
    // The values chip served, for the log: the commit quotes them.
    eprintln!(
        "[abs] PhysicalClosedLimit Lift/Tilt, InstalledOpen/ClosedLimit Lift, \
         InstalledOpen/ClosedLimit Tilt = {limits:04x?}; CurrentPosition Lift/Tilt = {current:?}"
    );
    // Physical closed limits and installed limits at their .matter defaults.
    assert_eq!(limits, [0xFFFF, 0xFFFF, 0, 0xFFFF, 0, 0xFFFF]);

    let target_before = read_attr(
        &node,
        1,
        WINDOW_COVERING,
        ATTR_TARGET_POSITION_LIFT_PERCENT100THS,
    )
    .await;
    let tilt_target_before = read_attr(
        &node,
        1,
        WINDOW_COVERING,
        a::TARGET_POSITION_TILT_PERCENT100THS,
    )
    .await;
    for (command, fields) in [
        (
            wc::command_id::GO_TO_LIFT_VALUE,
            wc::encode_go_to_lift_value(100),
        ),
        (
            wc::command_id::GO_TO_TILT_VALUE,
            wc::encode_go_to_tilt_value(100),
        ),
    ] {
        let path = CommandPath {
            endpoint: 1,
            cluster: WINDOW_COVERING,
            command,
        };
        assert_eq!(
            invoke_for_status(&node, path, fields).await.unwrap(),
            ImStatus::Failure(0x01),
            "{command:#04x} without ABS"
        );
    }
    assert_eq!(
        read_attr(
            &node,
            1,
            WINDOW_COVERING,
            ATTR_TARGET_POSITION_LIFT_PERCENT100THS
        )
        .await,
        target_before,
        "a refused GoToLiftValue moves nothing"
    );
    assert_eq!(
        read_attr(
            &node,
            1,
            WINDOW_COVERING,
            a::TARGET_POSITION_TILT_PERCENT100THS
        )
        .await,
        tilt_target_before,
        "a refused GoToTiltValue moves nothing"
    );
}
