// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

use matter_clusters::clusters::{
    access_control, administrator_commissioning, group_key_management,
    ota_software_update_requestor,
};
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

/// AccessControl (0x001F, ep0): Acl list typed-decodes Ok; AccessControlEntriesPerFabric >= 4.
#[tokio::test]
async fn access_control_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x001F;

    let acl = read_attr(&node, 0, C, 0x0000).await;
    let entries = access_control::decode_acl(&value_to_tlv(&acl))
        .unwrap_or_else(|e| panic!("AccessControl.Acl typed-decode failed: {e}; {acl:?}"));
    // Privilege is fabric-sensitive (Option since M9-A3 B1): our fabric's
    // admin entry carries it.
    assert!(
        entries
            .iter()
            .any(|e| e.privilege
                == Some(access_control::AccessControlEntryPrivilegeEnum::from_raw(5))),
        "no Administer entry with its Privilege present: {entries:?}"
    );
    let per_fabric = read_attr(&node, 0, C, 0x0004).await;
    let n = access_control::decode_access_control_entries_per_fabric(&value_to_tlv(&per_fabric))
        .expect("decode AccessControlEntriesPerFabric");
    assert!(
        n >= 4,
        "AccessControlEntriesPerFabric must be >= 4 (spec minimum): {n}"
    );
}

/// GroupKeyManagement (0x003F, ep0): GroupKeyMap list + MaxGroupsPerFabric typed-decode Ok.
#[tokio::test]
async fn group_key_management_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x003F;

    let map = read_attr(&node, 0, C, 0x0000).await;
    assert!(
        group_key_management::decode_group_key_map(&value_to_tlv(&map)).is_ok(),
        "GroupKeyManagement.GroupKeyMap typed-decode failed: {map:?}"
    );
    let max = read_attr(&node, 0, C, 0x0002).await;
    assert!(
        group_key_management::decode_max_groups_per_fabric(&value_to_tlv(&max)).is_ok(),
        "GroupKeyManagement.MaxGroupsPerFabric typed-decode failed: {max:?}"
    );
}

/// AdministratorCommissioning (0x003C, ep0): WindowStatus + AdminVendorId typed-decode Ok.
#[tokio::test]
async fn administrator_commissioning_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x003C;

    let status = read_attr(&node, 0, C, 0x0000).await;
    assert!(
        administrator_commissioning::decode_window_status(&value_to_tlv(&status)).is_ok(),
        "AdministratorCommissioning.WindowStatus typed-decode failed: {status:?}"
    );
    let vid = read_attr(&node, 0, C, 0x0002).await;
    assert!(
        administrator_commissioning::decode_admin_vendor_id(&value_to_tlv(&vid)).is_ok(),
        "AdministratorCommissioning.AdminVendorId typed-decode failed: {vid:?}"
    );
}

/// OtaSoftwareUpdateRequestor (0x002A, ep0): UpdateState + UpdatePossible typed-decode Ok.
#[tokio::test]
async fn ota_requestor_typed_decode() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    const C: u32 = 0x002A;

    let state = read_attr(&node, 0, C, 0x0002).await;
    assert!(
        ota_software_update_requestor::decode_update_state(&value_to_tlv(&state)).is_ok(),
        "OtaRequestor.UpdateState typed-decode failed: {state:?}"
    );
    let possible = read_attr(&node, 0, C, 0x0001).await;
    assert!(
        ota_software_update_requestor::decode_update_possible(&value_to_tlv(&possible)).is_ok(),
        "OtaRequestor.UpdatePossible typed-decode failed: {possible:?}"
    );
}
