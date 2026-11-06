// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use integration_tests::sweep::invoke_for_response;
use matter_clusters::clusters::{access_control, scenes_management as scenes};
use matter_codec::{Tag, TlvWriter};
use matter_controller::{
    AttestationTrust, AttributePath, CommandPath, FabricConfig, FileStore, ImStatus,
    MatterController, MatterTime, Node, OpenWindowOpts, ReadPath, Value,
};

const ONOFF_CLUSTER: u32 = 0x0006;

/// Controller A's fabric id: the fixture's `FabricConfig::new(1, ..)`.
const FABRIC_A_ID: u64 = 1;

// Controller A is the fixture controller (fabric id 1). Controller B uses a
// distinct fabric id so the device's fabric table holds two unambiguous entries.
const FABRIC_B_ID: u64 = 2;

/// Read back the OnOff attribute (ep1, cluster 0x0006, attr 0x0000) over a node.
async fn read_onoff(node: &Node) -> Option<bool> {
    let r = node
        .read(&[ReadPath::concrete(1, ONOFF_CLUSTER, 0x0000)])
        .await
        .expect("read OnOff");
    r.iter().find_map(|(p, v)| {
        if p.attribute == 0x0000 {
            if let Value::Bool(b) = v {
                return Some(*b);
            }
        }
        None
    })
}

/// Read one attribute of endpoint 0 over `node`: the raw report value.
async fn read_ep0(node: &Node, cluster: u32, attribute: u32) -> Value {
    node.read(&[ReadPath::concrete(0, cluster, attribute)])
        .await
        .expect("read attribute")
        .into_iter()
        .find(|(p, _)| p.attribute == attribute)
        .map(|(_, v)| v)
        .expect("attribute present in report")
}

fn value_to_tlv(value: &Value) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.write_value(Tag::Anonymous, value)
        .expect("infallible: Vec-backed TlvWriter");
    buf
}

/// The field tags of each entry of an unfiltered fabric-scoped list read that
/// belongs to `fabric_index` (its tag 254).
fn tags_of_entries_for(list: &Value, fabric_index: u8) -> Vec<Vec<Tag>> {
    let Value::Array(entries) = list else {
        panic!("expected a list, got {list:?}")
    };
    entries
        .iter()
        .filter_map(|entry| {
            let Value::Structure(fields) = entry else {
                panic!("expected struct entries, got {entry:?}")
            };
            let owner = fields.iter().find_map(|(t, v)| match (t, v) {
                (Tag::Context(254), Value::Uint(i)) => Some(*i),
                _ => None,
            });
            (owner == Some(u64::from(fabric_index)))
                .then(|| fields.iter().map(|(t, _)| *t).collect())
        })
        .collect()
}

/// The fabric-sensitive `Option` rule (another fabric's entries omit those
/// fields) against real chip encoding, AccessControl `Acl` and `Extension`.
/// With B's fabric on the device, A reads both unfiltered (our reads always
/// are): chip returns B's entries carrying only FabricIndex, and the generated
/// decoders must accept them with every fabric-sensitive field `None` while A's
/// own entries keep theirs.
async fn assert_acl_entries_withheld(node_a: &Node, node_b: &Node, a_index: u8, b_index: u8) {
    const ACL: u32 = access_control::attribute_id::ACL;
    const EXTENSION: u32 = access_control::attribute_id::EXTENSION;
    // Give B an Extension entry so A's read holds one of B's (Data = an empty
    // anonymous TLV list, the shape chip's CheckExtensionEntryDataFormat takes).
    let statuses = node_b
        .write(&[(
            AttributePath {
                endpoint: 0,
                cluster: access_control::CLUSTER_ID,
                attribute: EXTENSION,
            },
            Value::Array(vec![Value::Structure(vec![(
                Tag::Context(1),
                Value::Bytes(vec![0x17, 0x18]),
            )])]),
        )])
        .await
        .expect("B writes Extension");
    assert!(
        statuses.iter().all(|(_, s)| matches!(s, ImStatus::Success)),
        "B's Extension write statuses: {statuses:?}"
    );

    // Wire level first: chip really withholds B's sensitive fields.
    let acl_raw = read_ep0(node_a, access_control::CLUSTER_ID, ACL).await;
    let ext_raw = read_ep0(node_a, access_control::CLUSTER_ID, EXTENSION).await;
    for (what, raw) in [("Acl", &acl_raw), ("Extension", &ext_raw)] {
        let b_entries = tags_of_entries_for(raw, b_index);
        assert!(
            !b_entries.is_empty(),
            "A's {what} read holds no entry of B's fabric: {raw:?}"
        );
        for tags in &b_entries {
            assert_eq!(
                tags,
                &vec![Tag::Context(254)],
                "B's {what} entry was not stripped: {raw:?}"
            );
        }
    }

    // Typed: the whole list decodes; B's entries are all-None, A's are full.
    let acl = access_control::decode_acl(&value_to_tlv(&acl_raw))
        .expect("an unfiltered Acl read with two fabrics must decode");
    assert!(
        acl.iter().any(|e| e.fabric_index == a_index),
        "A's unfiltered Acl read holds none of A's own entries: {acl:?}"
    );
    for e in &acl {
        let sensitive = [
            e.privilege.is_some(),
            e.auth_mode.is_some(),
            e.subjects.is_some(),
            e.targets.is_some(),
        ];
        if e.fabric_index == b_index {
            assert_eq!(
                sensitive, [false; 4],
                "B's Acl entry has a sensitive field: {e:?}"
            );
        } else if e.fabric_index == a_index {
            assert_eq!(
                sensitive, [true; 4],
                "A's own Acl entry lost a field: {e:?}"
            );
        }
    }
    let ext = access_control::decode_extension(&value_to_tlv(&ext_raw))
        .expect("an unfiltered Extension read with two fabrics must decode");
    let b_ext: Vec<_> = ext.iter().filter(|e| e.fabric_index == b_index).collect();
    assert!(
        !b_ext.is_empty() && b_ext.iter().all(|e| e.data.is_none()),
        "B's Extension: {ext:?}"
    );
    // And writing B's withheld entry back is refused, never silently encoded.
    assert!(
        b_ext[0].encode().is_err(),
        "a withheld Extension entry must not re-encode"
    );
}

/// matter-controller's hand-written ACL read-modify-write (acl.rs) skips the
/// entries another fabric's read withholds and never writes them back: A's
/// round trip leaves B's admin entry, and so B's access, intact.
async fn assert_acl_round_trip_keeps_other_fabric(node_a: &Node, node_b: &Node, b_index: u8) {
    let own = node_a.read_acl().await.expect("A.read_acl with B present");
    assert!(
        !own.is_empty() && own.iter().all(|e| e.fabric_index != Some(b_index)),
        "read_acl must return A's entries and none of B's: {own:?}"
    );
    // `write_acl` is Ok even when the device rejects the write (the per-path
    // status carries that), so the statuses must be checked explicitly.
    let statuses = node_a
        .write_acl(&own)
        .await
        .expect("A.write_acl round trip");
    assert!(
        !statuses.is_empty() && statuses.iter().all(|(_, s)| matches!(s, ImStatus::Success)),
        "A's write_acl round-trip statuses: {statuses:?}"
    );
    let reread = node_a
        .read_acl()
        .await
        .expect("A.read_acl after the round trip");
    assert_eq!(
        reread, own,
        "A's ACL changed across its own write_acl round trip"
    );
    assert!(
        read_onoff(node_b).await.is_some(),
        "B lost access after A's ACL round trip"
    );
}

/// Invoke one ScenesManagement command on endpoint 1 and return its
/// response payload (the helper checks the response id, endpoint and cluster).
async fn scenes_response(node: &Node, command: u32, fields: Vec<u8>, response: u32) -> Vec<u8> {
    let path = CommandPath {
        endpoint: 1,
        cluster: scenes::CLUSTER_ID,
        command,
    };
    invoke_for_response(node, path, fields, response)
        .await
        .expect("scenes command")
}

/// RemoveAllScenes(group 0) on `node`'s fabric: Success.
async fn remove_all_scenes(node: &Node) {
    use scenes::command_id as c;
    let tlv = scenes_response(
        node,
        c::REMOVE_ALL_SCENES,
        scenes::encode_remove_all_scenes(0),
        c::REMOVE_ALL_SCENES_RESPONSE,
    )
    .await;
    assert_eq!(
        scenes::RemoveAllScenesResponse::decode(&tlv)
            .unwrap()
            .status,
        0
    );
}

/// The fabric-sensitive `Option` rule (another fabric's entries omit those
/// fields) against real chip encoding, ScenesManagement `FabricSceneInfo`
/// (endpoint 1). A empties its group-0 scenes (which gives A an entry) and B
/// adds a scene (which gives B one); A's unfiltered read returns B's entry with
/// only SceneCount, RemainingCapacity and FabricIndex (chip's
/// `SceneInfoStruct::EncodeForRead`), the generated decoder accepts it with
/// CurrentScene, CurrentGroup and SceneValid `None`, and A's own entry keeps
/// all three. (SceneInfoStruct is decode-only: nothing a client sends carries
/// it.) Both scene tables are emptied after.
async fn assert_fabric_scene_info_withheld(node_a: &Node, node_b: &Node, a_index: u8, b_index: u8) {
    use scenes::command_id as c;
    remove_all_scenes(node_a).await;
    let add = scenes::encode_add_scene(0, 1, 0, &"B".to_string(), &vec![]);
    let added = scenes_response(node_b, c::ADD_SCENE, add, c::ADD_SCENE_RESPONSE).await;
    assert_eq!(scenes::AddSceneResponse::decode(&added).unwrap().status, 0);
    let raw = node_a
        .read(&[ReadPath::concrete(
            1,
            scenes::CLUSTER_ID,
            scenes::attribute_id::FABRIC_SCENE_INFO,
        )])
        .await
        .expect("read FabricSceneInfo")
        .into_iter()
        .find(|(p, _)| p.attribute == scenes::attribute_id::FABRIC_SCENE_INFO)
        .map(|(_, v)| v)
        .expect("FabricSceneInfo in the report");
    let b_wire = tags_of_entries_for(&raw, b_index);
    assert_eq!(
        b_wire,
        [vec![Tag::Context(0), Tag::Context(4), Tag::Context(254)]],
        "B's FabricSceneInfo entry on the wire: {raw:?}"
    );
    let list = scenes::decode_fabric_scene_info(&value_to_tlv(&raw))
        .expect("an unfiltered FabricSceneInfo read with two fabrics must decode");
    let b = list
        .iter()
        .find(|e| e.fabric_index == b_index)
        .expect("B's entry");
    assert_eq!(
        (b.current_scene, b.current_group, b.scene_valid),
        (None, None, None)
    );
    assert_eq!(b.scene_count, 1, "B's one scene");
    let a = list
        .iter()
        .find(|e| e.fabric_index == a_index)
        .expect("A's entry");
    assert_eq!(
        (a.scene_count, a.current_group, a.scene_valid),
        (0, Some(0), Some(false)),
        "A's own entry keeps its sensitive fields: {a:?}"
    );
    assert!(a.current_scene.is_some(), "{a:?}");
    remove_all_scenes(node_b).await;
    remove_all_scenes(node_a).await;
}

// ── Multi-admin: open window → 2nd controller → list/remove fabric ───────────

/// Controller B: its own store under the per-run DUT dir, the same
/// development attestation roots, and a fresh fabric ([`FABRIC_B_ID`]).
async fn build_controller_b(cfg: &integration_tests::dut::DutConfig) -> MatterController {
    let trust = AttestationTrust::from_dirs(&cfg.paa_dir(), &cfg.cd_dir())
        .expect("loading development attestation roots (controller B)");
    let store_b = Arc::new(FileStore::new(cfg.dut_dir.join("controller-b-store.bin")));
    let controller_b = MatterController::builder(store_b)
        .attestation_trust(trust)
        .build()
        .await
        .expect("building controller B");
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1_700_000_000, |d| d.as_secs());
    controller_b
        .create_fabric(FabricConfig::new(
            FABRIC_B_ID,
            1,
            FABRIC_B_ID,
            (
                MatterTime::from_unix_secs(now_unix.saturating_sub(3600)),
                MatterTime::NO_EXPIRY,
            ),
        ))
        .await
        .expect("creating controller B fabric");
    controller_b
}

/// B commissions the device through the open window and returns its node id.
/// The window's commissionable advertisement can lag the
/// OpenCommissioningWindow response by a beat (mDNS propagation on a busy
/// DUT), which showed up as a transient commission timeout in the sweep, so
/// ONE transient failure is tolerated after a short pause. The full loop is
/// validated live, so a real regression fails both attempts and panics:
/// never a silent downgrade to a weaker assertion.
async fn commission_through_window(controller_b: &MatterController, manual_code: &str) -> u64 {
    let commissioned = match controller_b.commission(manual_code, None).await {
        Ok(info) => Ok(info),
        Err(first) => {
            eprintln!("[multi_admin] B's first commission attempt failed ({first}); retrying once");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            controller_b.commission(manual_code, None).await
        }
    };
    commissioned
        .unwrap_or_else(|e| {
            panic!("2nd-controller commission via the open-window manual code failed: {e:?}")
        })
        .node_id
}

/// The device's fabric count as A sees it, and A's and B's fabric indices
/// (found by fabric id, so B's index is never A's).
async fn fabric_indices(node_a: &Node) -> (usize, u8, u8) {
    let fabrics = node_a.list_fabrics().await.expect("A.list_fabrics");
    assert!(
        fabrics.len() >= 2,
        "expected ≥ 2 fabrics after B joined, got {}: {fabrics:?}",
        fabrics.len()
    );
    let index_of = |id: u64, who: &str| {
        fabrics
            .iter()
            .find(|f| f.fabric_id == id)
            .unwrap_or_else(|| panic!("{who}'s fabric must be present in A's fabric list"))
            .fabric_index
    };
    // A, then B. The tuple this replaced was already evaluated left to
    // right, so splitting it into two lets is a readability change only: a
    // missing A (the fixture's own fabric, the more fundamental failure) is
    // still reported before a missing B.
    let a_index = index_of(FABRIC_A_ID, "A");
    let b_index = index_of(FABRIC_B_ID, "B");
    assert_ne!(
        a_index, b_index,
        "A and B must hold distinct fabric indices: {fabrics:?}"
    );
    (fabrics.len(), a_index, b_index)
}

/// Drive the multi-admin loop against the live DUT:
///   1. Controller A (fixture) commissions the device (fabric 1).
///   2. A opens an enhanced commissioning window.
///   3. Controller B (its own store + fabric 2, same dev-cert trust) commissions
///      the device via the window's manual pairing code.
///   4. A.list_fabrics() shows ≥ 2 fabrics; both A and B can read OnOff; the
///      fabric-sensitive `Option` regressions run with B present.
///   5. A removes B's fabric by index; the fabric count drops back.
///
/// A risk to rule out: `commission` might not consume an open-window manual
/// code directly. That is now validated live: the full loop runs (B commissions
/// through the window, A removes B's fabric), so a commission failure here is a
/// hard error (see [`commission_through_window`]).
#[tokio::test]
async fn open_window_second_controller_and_remove_fabric() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller_a, node_id_a) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT (controller A)");
    let node_a = controller_a.node(node_id_a);
    assert!(
        read_onoff(&node_a).await.is_some(),
        "controller A could not read OnOff before opening the window"
    );

    let window = node_a
        .open_commissioning_window(OpenWindowOpts::default())
        .await
        .expect("open_commissioning_window");
    assert!(
        !window.manual_code.is_empty(),
        "commissioning window must yield a manual pairing code"
    );
    let controller_b = build_controller_b(&cfg).await;
    let node_b =
        controller_b.node(commission_through_window(&controller_b, &window.manual_code).await);

    assert!(
        read_onoff(&node_a).await.is_some(),
        "controller A lost its OnOff read path after B joined"
    );
    assert!(
        read_onoff(&node_b).await.is_some(),
        "controller B could not read OnOff after commissioning"
    );
    let (count, a_index, b_index) = fabric_indices(&node_a).await;

    // Fabric-sensitive regressions against real chip: unfiltered reads with B
    // present.
    assert_acl_entries_withheld(&node_a, &node_b, a_index, b_index).await;
    assert_acl_round_trip_keeps_other_fabric(&node_a, &node_b, b_index).await;
    assert_fabric_scene_info_withheld(&node_a, &node_b, a_index, b_index).await;

    node_a
        .remove_fabric(b_index)
        .await
        .expect("A.remove_fabric(B)");
    let after = node_a
        .list_fabrics()
        .await
        .expect("A.list_fabrics after removal");
    assert!(
        after.len() < count,
        "fabric count did not drop after removing B: before={count}, after={}",
        after.len()
    );
    assert!(
        after.iter().all(|f| f.fabric_id != FABRIC_B_ID),
        "B's fabric is still present after removal: {after:?}"
    );
}
