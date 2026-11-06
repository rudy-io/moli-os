// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

use matter_clusters::clusters::power_source;
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

/// PowerSource (0x002F): Status (enum), Order (u8), Description (string) all
/// typed-decode Ok from the live device bytes.
#[tokio::test]
async fn power_source_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x002F;

    let status = read_attr(&node, 1, C, 0x0000).await;
    assert!(
        power_source::decode_status(&value_to_tlv(&status)).is_ok(),
        "PowerSource.Status typed-decode failed: {status:?}"
    );
    let order = read_attr(&node, 1, C, 0x0001).await;
    assert!(
        power_source::decode_order(&value_to_tlv(&order)).is_ok(),
        "PowerSource.Order typed-decode failed: {order:?}"
    );
    let desc = read_attr(&node, 1, C, 0x0002).await;
    assert!(
        power_source::decode_description(&value_to_tlv(&desc)).is_ok(),
        "PowerSource.Description typed-decode failed: {desc:?}"
    );
}
