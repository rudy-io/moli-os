// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! ScenesManagement on a live all-clusters-app (endpoint 1, which
//! serves it at master and v1.4.2.0 with SceneNames, featureMap 1): both
//! attributes decoded, then AddScene / ViewScene / RecallScene / StoreScene /
//! GetSceneMembership / CopyScene / RemoveScene / RemoveAllScenes with every
//! response decoded and its status, group and scene checked.
//!
//! Scenes use group 0 (no group): chip checks group membership only for a
//! non-zero group (`0 != req.groupID && !HasEndpoint(...)`), so no Groups
//! setup is needed. The scene table is fabric-scoped and persisted (KVS), so
//! every test that creates scenes starts and ends with RemoveAllScenes(0)
//! (the attribute-decode test, which creates none, only starts with it), and
//! the scene's OnOff
//! extension field set is restored to the light's original state. Chip
//! sources: `ScenesManagementCluster.cpp` on master, `scenes-server.cpp` at
//! v1.4.2.0 (the same statuses, response fields and FabricSceneInfo updates
//! here); the OnOff scene handler (`OnOffCluster.cpp` /
//! `on-off-server/codegen/scenes-integration.cpp` on master, `on-off-server.cpp`
//! `DefaultOnOffSceneHandler` at v1.4.2.0) stores and applies OnOff as
//! `ValueUnsigned8`.

use std::time::{Duration, Instant};

use integration_tests::dut::DutConfig;
use integration_tests::sweep::{
    assert_exact_attribute_ids, attribute_tlv, decode_every_attribute, invoke_for_response,
    invoke_for_status, newer_than_codegen, ok, read_cluster_attributes,
};
use matter_clusters::clusters::scenes_management as scenes;
use matter_clusters::clusters::scenes_management::{
    AttributeValuePairStruct, ExtensionFieldSetStruct, SceneInfoStruct,
};
use matter_clusters::types::Nullable;
use matter_controller::{CommandPath, ImStatus, MatterController, Node, ReadPath, Value};

/// ScenesManagement and OnOff on all-clusters' endpoint 1.
const EP: u16 = 1;
const ON_OFF: u32 = 0x0006;

/// Scene group 0 (no group) and the scene ids the tests use.
const GROUP: u16 = 0;
const SCENE: u8 = 1;
const STORED: u8 = 2;
const COPY: u8 = 3;

/// The IM status chip puts in a response for a scene that does not exist
/// (`CHIP_ERROR_NOT_FOUND` → NotFound, `ResponseStatus`).
const NOT_FOUND: u8 = 0x8B;

/// chip's `kUndefinedSceneId`: never a valid scene id (SceneID is
/// `0..=0xFE`); the scene id of a free slot in chip's scene table.
const UNDEFINED_SCENE: u8 = 0xFF;

async fn connect_all_clusters() -> Option<(DutConfig, MatterController, u64)> {
    let cfg = DutConfig::from_env()?;
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: scenes tests need the all-clusters DUT (`just integration`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((cfg, controller, node_id))
}

fn scenes_path(command: u32) -> CommandPath {
    CommandPath {
        endpoint: EP,
        cluster: scenes::CLUSTER_ID,
        command,
    }
}

/// Invoke a scenes command that answers with response `response` and return
/// the response payload TLV (the helper checks endpoint, cluster and id).
async fn scenes_response(node: &Node, command: u32, fields: Vec<u8>, response: u32) -> Vec<u8> {
    invoke_for_response(node, scenes_path(command), fields, response)
        .await
        .unwrap()
}

/// RemoveAllScenes(group 0): Success, for this fabric's scenes only.
async fn remove_all_scenes(node: &Node) {
    use scenes::command_id as c;
    let tlv = scenes_response(
        node,
        c::REMOVE_ALL_SCENES,
        scenes::encode_remove_all_scenes(GROUP),
        c::REMOVE_ALL_SCENES_RESPONSE,
    )
    .await;
    let r = scenes::RemoveAllScenesResponse::decode(&tlv).unwrap();
    assert_eq!((r.status, r.group_id), (0, GROUP));
}

/// The OnOff extension field set chip stores and returns for a scene that
/// turns the light `on`.
fn on_off_set(on: bool) -> ExtensionFieldSetStruct {
    ExtensionFieldSetStruct {
        cluster_id: ON_OFF,
        attribute_value_list: vec![AttributeValuePairStruct {
            attribute_id: 0x0000,
            value_unsigned8: Some(u8::from(on)),
            value_signed8: None,
            value_unsigned16: None,
            value_signed16: None,
            value_unsigned32: None,
            value_signed32: None,
            value_unsigned64: None,
            value_signed64: None,
        }],
    }
}

async fn on_off(node: &Node) -> bool {
    let r = node
        .read(&[ReadPath::concrete(EP, ON_OFF, 0x0000)])
        .await
        .unwrap();
    match r.into_iter().find(|(p, _)| p.attribute == 0x0000) {
        Some((_, Value::Bool(b))) => b,
        other => panic!("OnOff read: {other:?}"),
    }
}

/// Switch the light with OnOff `On` (0x01) / `Off` (0x00), polled until it
/// reads back.
async fn set_on_off(node: &Node, on: bool) {
    let path = CommandPath {
        endpoint: EP,
        cluster: ON_OFF,
        command: u32::from(on),
    };
    let st = invoke_for_status(node, path, vec![0x15, 0x18])
        .await
        .unwrap();
    assert_eq!(st, ImStatus::Success);
    wait_for_on_off(node, on).await;
}

async fn wait_for_on_off(node: &Node, want: bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while on_off(node).await != want {
        assert!(Instant::now() < deadline, "OnOff never became {want}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The scene ids of a group-0 GetSceneMembership `SceneList`, sorted, without
/// chip's free-slot entries.
///
/// Chip's server lists every FREE slot of this fabric's scene map as scene
/// 0xFF in a group-0 reply. The map has a fixed size per fabric
/// (`kMaxScenesPerFabric`); a free slot holds `SceneStorageId`'s defaults,
/// group `kGlobalGroupSceneId` (0) and scene `kUndefinedSceneId` (0xFF); and
/// `DefaultSceneTableImpl::GetAllSceneIdsInGroup` matches a slot on its group
/// alone. `SceneTable.h` and `SceneTableImpl.cpp` are identical here at
/// v1.4.2.0 and master. Seen live (master): `[1, 2, 3, 255, 255, 255, 255]` for three
/// scenes in a 7-slot map. 0xFF names no scene, so those entries are dropped
/// and the real ids are asserted exactly.
fn scene_ids_in_group_0(list: Vec<u8>) -> Vec<u8> {
    let mut ids: Vec<u8> = list
        .into_iter()
        .filter(|id| *id != UNDEFINED_SCENE)
        .collect();
    ids.sort_unstable();
    ids
}

/// This fabric's FabricSceneInfo entry (an unfiltered read returns every
/// fabric's; ours is the one whose sensitive fields are present).
async fn own_scene_info(node: &Node) -> SceneInfoStruct {
    let attrs = read_cluster_attributes(node, EP, scenes::CLUSTER_ID)
        .await
        .unwrap();
    let list = scenes::decode_fabric_scene_info(attribute_tlv(
        &attrs,
        scenes::attribute_id::FABRIC_SCENE_INFO,
    ))
    .unwrap();
    let own: Vec<_> = list
        .into_iter()
        .filter(|e| e.current_scene.is_some())
        .collect();
    assert_eq!(
        own.len(),
        1,
        "exactly one entry with our sensitive fields: {own:?}"
    );
    own.into_iter().next().unwrap()
}

/// Both attributes decode and are the only ones served (SceneTableSize,
/// FabricSceneInfo; the DoNotUse id 0x0000 is not). After RemoveAllScenes our
/// fabric has an entry (chip creates it on the first scenes command,
/// `UpdateFabricSceneInfo`) with no scenes and SceneValid false.
#[tokio::test]
async fn scenes_management_decodes_both_attributes() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use scenes::attribute_id as a;
    remove_all_scenes(&node).await;
    let attrs = decode_every_attribute(&node, EP, scenes::CLUSTER_ID, |id, t| match id {
        a::SCENE_TABLE_SIZE => ok(scenes::decode_scene_table_size(t)),
        a::FABRIC_SCENE_INFO => ok(scenes::decode_fabric_scene_info(t)),
        other => newer_than_codegen("ScenesManagement", other),
    })
    .await;
    assert_exact_attribute_ids(
        "ScenesManagement",
        &attrs,
        &[a::SCENE_TABLE_SIZE, a::FABRIC_SCENE_INFO],
    );
    let size = scenes::decode_scene_table_size(attribute_tlv(&attrs, a::SCENE_TABLE_SIZE)).unwrap();
    let own = own_scene_info(&node).await;
    assert_eq!((own.scene_count, own.scene_valid), (0, Some(false)));
    assert!(
        own.remaining_capacity > 0 && u16::from(own.remaining_capacity) <= size,
        "RemainingCapacity {} of SceneTableSize {size}",
        own.remaining_capacity
    );
}

/// AddScene, then ViewScene returns exactly what was added (transition time,
/// name: SceneNames is on, and the OnOff extension field set).
async fn add_and_view_scene(node: &Node) {
    use scenes::command_id as c;
    let add = scenes_response(
        node,
        c::ADD_SCENE,
        scenes::encode_add_scene(
            GROUP,
            SCENE,
            0,
            &"Test".to_string(),
            &vec![on_off_set(true)],
        ),
        c::ADD_SCENE_RESPONSE,
    )
    .await;
    let add = scenes::AddSceneResponse::decode(&add).unwrap();
    assert_eq!((add.status, add.group_id, add.scene_id), (0, GROUP, SCENE));
    let view = scenes_response(
        node,
        c::VIEW_SCENE,
        scenes::encode_view_scene(GROUP, SCENE),
        c::VIEW_SCENE_RESPONSE,
    )
    .await;
    let view = scenes::ViewSceneResponse::decode(&view).unwrap();
    assert_eq!(
        (view.status, view.group_id, view.scene_id),
        (0, GROUP, SCENE)
    );
    assert_eq!(view.transition_time, Some(0));
    assert_eq!(view.scene_name.as_deref(), Some("Test"));
    assert_eq!(
        view.extension_field_set_structs,
        Some(vec![on_off_set(true)])
    );
}

/// RecallScene with the light off applies the scene (the light turns on) and
/// FabricSceneInfo records it: CurrentScene / CurrentGroup set, one scene.
///
/// SceneValid then reads false, not true. Chip's server sets it true while
/// handling RecallScene, but the OnOff scene handler applies the value later,
/// from a timer (`ApplyScene` → `scheduleTimerCallbackMs`, even for a 0 ms
/// transition), and `setOnOffValue` marks every fabric's scene invalid
/// whenever it changes OnOff (`MakeSceneInvalidForAllFabrics`): the recall
/// invalidates itself. Both run on chip's event loop, so once OnOff reads on
/// the invalidation has happened. (`on-off-server.cpp` at v1.4.2.0,
/// `on-off-server/codegen/on-off-server.cpp` and `scenes-integration.cpp` on
/// master: the same code.) Recalling again with the light already on changes
/// nothing (`setOnOffValue` returns early, "already set to new value"), so
/// SceneValid stays true.
async fn recall_scene(node: &Node) {
    set_on_off(node, false).await;
    let recall = || scenes::encode_recall_scene(GROUP, SCENE, None);
    let path = scenes_path(scenes::command_id::RECALL_SCENE);
    let st = invoke_for_status(node, path, recall()).await.unwrap();
    assert_eq!(st, ImStatus::Success);
    wait_for_on_off(node, true).await;
    let fields = |e: &SceneInfoStruct| {
        (
            e.scene_count,
            e.current_scene,
            e.current_group,
            e.scene_valid,
        )
    };
    assert_eq!(
        fields(&own_scene_info(node).await),
        (1, Some(SCENE), Some(GROUP), Some(false)),
        "the recall's own OnOff change marks the scene invalid"
    );
    let st = invoke_for_status(node, path, recall()).await.unwrap();
    assert_eq!(st, ImStatus::Success);
    assert!(on_off(node).await, "the light stays on");
    assert_eq!(
        fields(&own_scene_info(node).await),
        (1, Some(SCENE), Some(GROUP), Some(true)),
        "a recall that changes nothing leaves the scene valid"
    );
}

/// StoreScene captures the current state as a second scene; CopyScene copies
/// the first to a third; GetSceneMembership lists all three with a non-null
/// capacity; RemoveScene removes the copy, after which ViewScene of it is
/// NotFound with none of the success-only fields, and RecallScene of it is
/// NotFound.
async fn store_copy_list_and_remove(node: &Node) {
    use scenes::command_id as c;
    let store = scenes_response(
        node,
        c::STORE_SCENE,
        scenes::encode_store_scene(GROUP, STORED),
        c::STORE_SCENE_RESPONSE,
    )
    .await;
    let store = scenes::StoreSceneResponse::decode(&store).unwrap();
    assert_eq!(
        (store.status, store.group_id, store.scene_id),
        (0, GROUP, STORED)
    );
    let copy =
        scenes::encode_copy_scene(scenes::CopyModeBitmap::empty(), GROUP, SCENE, GROUP, COPY);
    let copy = scenes::CopySceneResponse::decode(
        &scenes_response(node, c::COPY_SCENE, copy, c::COPY_SCENE_RESPONSE).await,
    )
    .unwrap();
    assert_eq!(
        (
            copy.status,
            copy.group_identifier_from,
            copy.scene_identifier_from
        ),
        (0, GROUP, SCENE)
    );
    let members = scenes_response(
        node,
        c::GET_SCENE_MEMBERSHIP,
        scenes::encode_get_scene_membership(GROUP),
        c::GET_SCENE_MEMBERSHIP_RESPONSE,
    )
    .await;
    let members = scenes::GetSceneMembershipResponse::decode(&members).unwrap();
    assert_eq!((members.status, members.group_id), (0, GROUP));
    assert!(
        matches!(members.capacity, Nullable::Value(_)),
        "{members:?}"
    );
    let ids = scene_ids_in_group_0(members.scene_list.unwrap());
    assert_eq!(ids, [SCENE, STORED, COPY]);

    let removed = scenes_response(
        node,
        c::REMOVE_SCENE,
        scenes::encode_remove_scene(GROUP, COPY),
        c::REMOVE_SCENE_RESPONSE,
    )
    .await;
    let removed = scenes::RemoveSceneResponse::decode(&removed).unwrap();
    assert_eq!(
        (removed.status, removed.group_id, removed.scene_id),
        (0, GROUP, COPY)
    );
    let gone = scenes_response(
        node,
        c::VIEW_SCENE,
        scenes::encode_view_scene(GROUP, COPY),
        c::VIEW_SCENE_RESPONSE,
    )
    .await;
    let gone = scenes::ViewSceneResponse::decode(&gone).unwrap();
    assert_eq!(
        (
            gone.status,
            gone.scene_id,
            gone.transition_time,
            gone.scene_name,
            gone.extension_field_set_structs
        ),
        (NOT_FOUND, COPY, None, None, None)
    );
    let recall = scenes::encode_recall_scene(GROUP, COPY, Some(Nullable::Null));
    let st = invoke_for_status(node, scenes_path(c::RECALL_SCENE), recall)
        .await
        .unwrap();
    assert_eq!(st, ImStatus::Failure(NOT_FOUND));
}

/// The scene commands end to end on one fabric, from and back to an empty
/// scene table and the light's original state.
#[tokio::test]
async fn scenes_management_commands_round_trip() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    let was_on = on_off(&node).await;
    remove_all_scenes(&node).await;
    add_and_view_scene(&node).await;
    recall_scene(&node).await;
    store_copy_list_and_remove(&node).await;
    remove_all_scenes(&node).await;
    use scenes::command_id as c;
    let members = scenes_response(
        &node,
        c::GET_SCENE_MEMBERSHIP,
        scenes::encode_get_scene_membership(GROUP),
        c::GET_SCENE_MEMBERSHIP_RESPONSE,
    )
    .await;
    let members = scenes::GetSceneMembershipResponse::decode(&members).unwrap();
    assert_eq!(
        scene_ids_in_group_0(members.scene_list.unwrap()),
        Vec::<u8>::new(),
        "the table is empty again"
    );
    set_on_off(&node, was_on).await;
}

/// AddScene with an OnOff pair that breaks choice group `a` (exactly one value
/// field): the generated encoder does not check it (the struct documents the
/// rule), chip does (`IsExactlyOneValuePopulated` in the scene handler's
/// validation, both refs) and answers Failure (CHIP_ERROR_INVALID_ARGUMENT,
/// which `ResponseStatus` maps through StatusIB to 0x01), storing nothing.
#[tokio::test]
async fn scenes_add_scene_refuses_a_pair_without_exactly_one_value() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use scenes::command_id as c;
    remove_all_scenes(&node).await;
    let mut no_value = on_off_set(true);
    no_value.attribute_value_list[0].value_unsigned8 = None;
    let mut two_values = on_off_set(true);
    two_values.attribute_value_list[0].value_unsigned16 = Some(1);
    for set in [no_value, two_values] {
        let add = scenes::encode_add_scene(GROUP, SCENE, 0, &"Bad".to_string(), &vec![set]);
        let r = scenes_response(&node, c::ADD_SCENE, add, c::ADD_SCENE_RESPONSE).await;
        let r = scenes::AddSceneResponse::decode(&r).unwrap();
        assert_eq!((r.status, r.group_id, r.scene_id), (0x01, GROUP, SCENE));
        let view = scenes_response(
            &node,
            c::VIEW_SCENE,
            scenes::encode_view_scene(GROUP, SCENE),
            c::VIEW_SCENE_RESPONSE,
        )
        .await;
        assert_eq!(
            scenes::ViewSceneResponse::decode(&view).unwrap().status,
            NOT_FOUND
        );
    }
    remove_all_scenes(&node).await;
}
