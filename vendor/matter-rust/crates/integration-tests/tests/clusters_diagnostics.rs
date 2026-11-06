// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

use matter_clusters::clusters::general_diagnostics;
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

/// GeneralDiagnostics (0x0033, ep0): RebootCount, UpTime, and NetworkInterfaces
/// typed-decode Ok from the live device bytes.
#[tokio::test]
async fn general_diagnostics_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x0033;

    let reboot = read_attr(&node, 0, C, 0x0001).await;
    assert!(
        general_diagnostics::decode_reboot_count(&value_to_tlv(&reboot)).is_ok(),
        "GeneralDiagnostics.RebootCount typed-decode failed: {reboot:?}"
    );
    let uptime = read_attr(&node, 0, C, 0x0002).await;
    assert!(
        general_diagnostics::decode_up_time(&value_to_tlv(&uptime)).is_ok(),
        "GeneralDiagnostics.UpTime typed-decode failed: {uptime:?}"
    );
    let nifs = read_attr(&node, 0, C, 0x0000).await;
    assert!(
        general_diagnostics::decode_network_interfaces(&value_to_tlv(&nifs)).is_ok(),
        "GeneralDiagnostics.NetworkInterfaces typed-decode failed: {nifs:?}"
    );
}
