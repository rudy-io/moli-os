// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

use matter_clusters::clusters::descriptor;
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

/// Descriptor (0x001D): ServerList on ep1 typed-decodes and contains OnOff
/// (0x0006); DeviceTypeList on ep1 decodes non-empty; PartsList on ep0 contains
/// child endpoint 1.
#[tokio::test]
async fn descriptor_lists_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x001D;

    // ep1 ServerList contains OnOff.
    let server_list = read_attr(&node, 1, C, 0x0001).await;
    let servers =
        descriptor::decode_server_list(&value_to_tlv(&server_list)).expect("decode ep1 ServerList");
    assert!(
        servers.contains(&0x0006),
        "ep1 Descriptor.ServerList must contain OnOff (0x0006): {servers:?}"
    );

    // ep1 DeviceTypeList decodes non-empty.
    let dtl = read_attr(&node, 1, C, 0x0000).await;
    let device_types = descriptor::decode_device_type_list(&value_to_tlv(&dtl))
        .expect("decode ep1 DeviceTypeList");
    assert!(
        !device_types.is_empty(),
        "ep1 Descriptor.DeviceTypeList must be non-empty"
    );

    // ep0 PartsList lists the child endpoints, so it contains endpoint 1.
    let parts = read_attr(&node, 0, C, 0x0003).await;
    let parts_list =
        descriptor::decode_parts_list(&value_to_tlv(&parts)).expect("decode ep0 PartsList");
    assert!(
        parts_list.contains(&1),
        "ep0 Descriptor.PartsList must contain child endpoint 1: {parts_list:?}"
    );
}
