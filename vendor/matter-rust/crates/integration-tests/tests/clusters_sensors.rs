// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

use matter_clusters::clusters::{air_quality, boolean_state, occupancy_sensing};
use matter_codec::{Tag, TlvWriter};
use matter_controller::{Node, ReadPath, Value};

fn value_to_tlv(value: &Value) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.write_value(Tag::Anonymous, value)
        .expect("infallible: Vec-backed TlvWriter");
    buf
}

async fn read_attr(node: &Node, ep: u16, cluster: u32, attr: u32) -> Value {
    let r = node
        .read(&[ReadPath::concrete(ep, cluster, attr)])
        .await
        .expect("read attribute");
    r.into_iter()
        .find(|(p, _)| p.attribute == attr)
        .map(|(_, v)| v)
        .expect("attribute present in report")
}

/// OccupancySensing (0x0406): Occupancy (bitmap) and OccupancySensorType (enum)
/// typed-decode Ok from the live device bytes.
#[tokio::test]
async fn occupancy_sensing_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x0406;

    let occ = read_attr(&node, 1, C, 0x0000).await;
    assert!(
        occupancy_sensing::decode_occupancy(&value_to_tlv(&occ)).is_ok(),
        "OccupancySensing.Occupancy typed-decode failed: {occ:?}"
    );
    let ty = read_attr(&node, 1, C, 0x0001).await;
    assert!(
        occupancy_sensing::decode_occupancy_sensor_type(&value_to_tlv(&ty)).is_ok(),
        "OccupancySensing.OccupancySensorType typed-decode failed: {ty:?}"
    );
}

/// BooleanState (0x0045): StateValue (bool) typed-decodes Ok.
#[tokio::test]
async fn boolean_state_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let v = read_attr(&node, 1, 0x0045, 0x0000).await;
    assert!(
        boolean_state::decode_state_value(&value_to_tlv(&v)).is_ok(),
        "BooleanState.StateValue typed-decode failed: {v:?}"
    );
}

/// AirQuality (0x005B): AirQuality (enum) typed-decodes Ok.
#[tokio::test]
async fn air_quality_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let v = read_attr(&node, 1, 0x005B, 0x0000).await;
    assert!(
        air_quality::decode_air_quality(&value_to_tlv(&v)).is_ok(),
        "AirQuality.AirQuality typed-decode failed: {v:?}"
    );
}
