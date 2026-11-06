// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

use matter_clusters::clusters::{binding, fixed_label, user_label};
use matter_codec::{Tag, TlvWriter};
use matter_controller::{AttributePath, ImStatus, Node, ReadPath, Value};

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

/// FixedLabel (0x0040, ep1): LabelList typed-decodes Ok.
#[tokio::test]
async fn fixed_label_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let v = read_attr(&node, 1, 0x0040, 0x0000).await;
    assert!(
        fixed_label::decode_label_list(&value_to_tlv(&v)).is_ok(),
        "FixedLabel.LabelList typed-decode failed: {v:?}"
    );
}

/// Binding (0x001E, ep1): Binding list typed-decodes Ok.
#[tokio::test]
async fn binding_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let v = read_attr(&node, 1, 0x001E, 0x0000).await;
    assert!(
        binding::decode_binding(&value_to_tlv(&v)).is_ok(),
        "Binding.Binding typed-decode failed: {v:?}"
    );
}

/// UserLabel (0x0041, ep1): write a LabelList entry, read it back, typed-decode,
/// and assert the entry round-trips (exercises a writable list-of-struct attr
/// end-to-end). LabelStruct: label = ctx0 Utf8, value = ctx1 Utf8.
#[tokio::test]
async fn user_label_write_read_back() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x0041;

    let entry = Value::Array(vec![Value::Structure(vec![
        (Tag::Context(0), Value::Utf8("room".to_string())),
        (Tag::Context(1), Value::Utf8("kitchen".to_string())),
    ])]);
    let statuses = node
        .write(&[(
            AttributePath {
                endpoint: 1,
                cluster: C,
                attribute: 0x0000,
            },
            entry,
        )])
        .await
        .expect("write UserLabel.LabelList");
    for (_, s) in &statuses {
        assert!(
            matches!(s, ImStatus::Success),
            "UserLabel write status not Success: {s:?}"
        );
    }

    let v = read_attr(&node, 1, C, 0x0000).await;
    let labels =
        user_label::decode_label_list(&value_to_tlv(&v)).expect("decode UserLabel.LabelList");
    assert!(
        labels
            .iter()
            .any(|l| l.label == "room" && l.value == "kitchen"),
        "UserLabel.LabelList did not round-trip the written entry: {labels:?}"
    );
}
