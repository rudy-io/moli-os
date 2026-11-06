// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! Messages on a live tv-app (`just integration-tv`), endpoint 1.
//! No other chip example app serves it, at master or v1.4.2.0. Both
//! attributes decode with the generated decoders; PresentMessagesRequest and
//! CancelMessagesRequest (whose MessageIDs is the first request
//! `list<octstr>`) run through tv-app's message list; chip's server refuses
//! malformed requests.
//!
//! Run live on a Linux arm64 Pi against tv-app built at chip master 5cd2917a
//! and at v1.4.2.0: tv-app needs Rosetta 2 to build on Apple silicon (chip's
//! ZAP ships x86_64-only for macOS). Every expected value below is also read
//! from chip source. Line numbers are chip master 5cd2917a:
//!
//! - examples/tv-app/tv-common/tv-app.matter:4100, :4123-4137: Messages on
//!   endpoint 1 only, attributes 0 and 1 (plus globals), both commands;
//! - examples/tv-app/tv-common/src/ZCLCallbacks.cpp:175-178: tv-app's
//!   `MessagesManager` is the delegate on every Messages endpoint;
//! - src/app/clusters/messages-server/messages-server.cpp: chip's server
//!   reads FeatureMap from the delegate (:136-155); PresentMessagesRequest
//!   answers ConstraintError for an id that is not 16 bytes (:188), more than
//!   four responses (:210) and a response id below 1 (:224), all before the
//!   delegate (:237); CancelMessagesRequest answers ConstraintError for an id
//!   shorter than 16 bytes (:272), before the delegate (:281). The limits are
//!   messages-delegate.h:31-35;
//! - examples/tv-app/tv-common/clusters/messages/MessagesManager.cpp:
//!   tv-app's delegate appends to an in-memory list (:54), a cancel of an
//!   unknown id succeeds (:61-72), both attributes list it in arrival order
//!   (:75-95), FeatureMap is forced to all four features, 0x0F (:98-111);
//!   MessagesManager.h:83-106 sends Responses only when the message has some.
//!
//! chip does not fabric-scope MessageStruct (zap messages-cluster.xml:52-53,
//! "TODO: enable isFabricScoped"; zzz_generated/app-common/clusters/Messages/
//! Structs.h:86 `kIsFabricScoped = false`), so tv-app sends every field of
//! every entry with no FabricIndex: `fabric_index` decodes as `None`. chip's
//! Messages server logs no events.
//!
//! At v1.4.2.0 messages-server.cpp, messages-delegate.h, MessagesManager.cpp,
//! MessagesManager.h and ZCLCallbacks.cpp are identical to master, Structs.h
//! has the same `kIsFabricScoped = false`, and tv-app.matter serves the same
//! Messages cluster on endpoint 1 (:3970, :3993-4007).
//!
//! The tests run single-threaded (`--test-threads=1`, xtask `run_tests`)
//! against one tv-app booted with a fresh KVS. tv-app's message list lives in
//! memory only, and each test first cancels whatever is in it.
//!
//! The tests do not pace their exchanges. chip master's tv-app has only 15
//! packet buffers (chip c37efec9db) and holds each response until its ACK
//! arrives; before the controller ACKed responses as their exchange closed,
//! unACKed responses ran it dry and it ACKed a request in
//! `messages_refuse_malformed_requests` but never answered it, unless every
//! exchange was paced. v1.4.2.0's tv-app allocates buffers from the heap.

use integration_tests::dut::DutConfig;
use integration_tests::sweep::{
    assert_exact_attribute_ids, attribute_tlv, decode_every_attribute, invoke_for_status,
    newer_than_codegen, ok, read_cluster_attributes,
};
use matter_clusters::clusters::messages::{
    self as msg, MessageControlBitmap, MessagePriorityEnum, MessageResponseOptionStruct,
};
use matter_clusters::types::Nullable;
use matter_controller::{CommandPath, ImStatus, MatterController, Node, ReadPath};

/// tv-app serves Messages on endpoint 1 (tv-app.matter:4100, :4123).
const EP: u16 = 1;

/// IM status code chip's server answers malformed requests with (Matter Core
/// spec 8.10.1).
const CONSTRAINT_ERROR: ImStatus = ImStatus::Failure(0x87);

/// Two message ids, 16 bytes each (`kMessageIdLength`, messages-delegate.h:31).
const ID_A: [u8; 16] = [0xA0; 16];
const ID_B: [u8; 16] = [0xB0; 16];

/// The controller and node id, or `None` (skip) unless the DUT is tv-app.
async fn connect_tv() -> Option<(MatterController, u64)> {
    let Some(cfg) = DutConfig::from_env() else {
        eprintln!("skipped: no DUT (set MATTER_INTEGRATION_DUT via `just integration-tv`)");
        return None;
    };
    if !cfg.is_app("tv") {
        eprintln!("skipped: Messages test needs the tv-app DUT (`just integration-tv`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((controller, node_id))
}

/// Invoke a Messages command on [`EP`] and return the bare status it answers
/// (neither command has a response command).
async fn status_of(node: &Node, command: u32, fields: Vec<u8>) -> ImStatus {
    invoke_for_status(
        node,
        CommandPath {
            endpoint: EP,
            cluster: msg::CLUSTER_ID,
            command,
        },
        fields,
    )
    .await
    .unwrap()
}

/// `CancelMessagesRequest(ids)`.
async fn cancel(node: &Node, ids: &[&[u8]]) -> ImStatus {
    let ids: Vec<Vec<u8>> = ids.iter().map(|id| id.to_vec()).collect();
    status_of(
        node,
        msg::command_id::CANCEL_MESSAGES_REQUEST,
        msg::encode_cancel_messages_request(&ids),
    )
    .await
}

/// The Messages list and the ActiveMessageIDs, read fresh.
async fn messages(node: &Node) -> (Vec<msg::MessageStruct>, Vec<Vec<u8>>) {
    let attrs = read_cluster_attributes(node, EP, msg::CLUSTER_ID)
        .await
        .unwrap();
    (
        msg::decode_messages(attribute_tlv(&attrs, msg::attribute_id::MESSAGES)).unwrap(),
        msg::decode_active_message_ids(attribute_tlv(
            &attrs,
            msg::attribute_id::ACTIVE_MESSAGE_IDS,
        ))
        .unwrap(),
    )
}

/// Cancel whatever an earlier test left in tv-app's in-memory list.
async fn clear_messages(node: &Node) {
    let (_, active) = messages(node).await;
    if !active.is_empty() {
        let ids: Vec<&[u8]> = active.iter().map(Vec::as_slice).collect();
        assert_eq!(cancel(node, &ids).await, ImStatus::Success);
    }
    assert_eq!(messages(node).await.1.len(), 0);
}

/// `PresentMessagesRequest` with a null StartTime, the field these tests do
/// not vary.
fn present(
    id: &[u8],
    priority: MessagePriorityEnum,
    control: MessageControlBitmap,
    duration: Nullable<u64>,
    text: &str,
    responses: Option<Vec<MessageResponseOptionStruct>>,
) -> Vec<u8> {
    msg::encode_present_messages_request(
        &id.to_vec(),
        priority,
        control,
        Nullable::Null,
        duration,
        &text.to_string(),
        responses,
    )
}

fn response(id: u32, label: &str) -> MessageResponseOptionStruct {
    MessageResponseOptionStruct {
        message_response_id: id,
        label: label.to_string(),
    }
}

/// Both attributes decode (exact ids 0 and 1; FeatureMap 0x0F); two messages
/// presented arrive in order with every field and no FabricIndex; a cancel
/// of both and of an id tv-app never had succeeds and empties both lists.
#[tokio::test]
async fn messages_present_and_cancel_round_trip() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    clear_messages(&node).await;
    // The sweep runs on empty lists (just cleared); populated entries are
    // decoded by `messages()` below.
    let attrs = decode_every_attribute(&node, EP, msg::CLUSTER_ID, |id, t| match id {
        msg::attribute_id::MESSAGES => ok(msg::decode_messages(t)),
        msg::attribute_id::ACTIVE_MESSAGE_IDS => ok(msg::decode_active_message_ids(t)),
        other => newer_than_codegen("Messages", other),
    })
    .await;
    assert_exact_attribute_ids("Messages", &attrs, &[0, 1]);
    assert_eq!(
        feature_map(&node).await,
        0x0F,
        "MessagesManager::GetFeatureMap"
    );

    let a = present(
        &ID_A,
        MessagePriorityEnum::Low,
        MessageControlBitmap::empty(),
        Nullable::Null,
        "Hello",
        None,
    );
    assert_eq!(
        status_of(&node, msg::command_id::PRESENT_MESSAGES_REQUEST, a).await,
        ImStatus::Success
    );
    let control =
        MessageControlBitmap::CONFIRMATION_REQUIRED | MessageControlBitmap::RESPONSE_REQUIRED;
    let b = present(
        &ID_B,
        MessagePriorityEnum::Critical,
        control,
        Nullable::Value(60_000),
        "Storm warning",
        Some(vec![response(1, "OK"), response(2, "Snooze")]),
    );
    assert_eq!(
        status_of(&node, msg::command_id::PRESENT_MESSAGES_REQUEST, b).await,
        ImStatus::Success
    );

    let (list, active) = messages(&node).await;
    assert_eq!(active, [ID_A.to_vec(), ID_B.to_vec()]);
    assert_eq!(list.len(), 2);
    let first = &list[0];
    assert_eq!(first.message_id.as_deref(), Some(&ID_A[..]));
    assert_eq!(
        (
            first.priority,
            first.message_control,
            first.start_time,
            first.duration
        ),
        (
            Some(MessagePriorityEnum::Low),
            Some(MessageControlBitmap::empty()),
            Some(Nullable::Null),
            Some(Nullable::Null)
        )
    );
    assert_eq!(first.message_text.as_deref(), Some("Hello"));
    assert_eq!(first.responses, None);
    assert_eq!(
        first.fabric_index, None,
        "chip's MessageStruct carries no FabricIndex"
    );
    let second = &list[1];
    assert_eq!(second.message_id.as_deref(), Some(&ID_B[..]));
    assert_eq!(
        (
            second.priority,
            second.message_control,
            second.start_time,
            second.duration
        ),
        (
            Some(MessagePriorityEnum::Critical),
            Some(control),
            Some(Nullable::Null),
            Some(Nullable::Value(60_000))
        )
    );
    assert_eq!(second.message_text.as_deref(), Some("Storm warning"));
    assert_eq!(
        second.responses,
        Some(vec![response(1, "OK"), response(2, "Snooze")])
    );
    assert_eq!(second.fabric_index, None);

    let unknown = [0xEE; 16];
    assert_eq!(
        cancel(&node, &[&ID_A, &ID_B, &unknown]).await,
        ImStatus::Success
    );
    let (list, active) = messages(&node).await;
    assert_eq!((list.len(), active.len()), (0, 0));
}

/// chip's server refuses, before tv-app's delegate sees them: a 15-byte
/// message id, a response id of 0, five responses (at most four), and a
/// 15-byte id in a cancel. Nothing is stored.
#[tokio::test]
async fn messages_refuse_malformed_requests() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    clear_messages(&node).await;
    let short = [0xC0; 15];
    let no_control = MessageControlBitmap::empty();
    let low = MessagePriorityEnum::Low;
    let five: Vec<MessageResponseOptionStruct> = (1..=5).map(|i| response(i, "Option")).collect();
    for (fields, why) in [
        (
            present(&short, low, no_control, Nullable::Null, "Short id", None),
            "a 15-byte id",
        ),
        (
            present(
                &ID_A,
                low,
                no_control,
                Nullable::Null,
                "Zero",
                Some(vec![response(0, "Zero")]),
            ),
            "response id 0",
        ),
        (
            present(&ID_A, low, no_control, Nullable::Null, "Five", Some(five)),
            "five responses",
        ),
    ] {
        assert_eq!(
            status_of(&node, msg::command_id::PRESENT_MESSAGES_REQUEST, fields).await,
            CONSTRAINT_ERROR,
            "{why}"
        );
    }
    assert_eq!(
        cancel(&node, &[&short]).await,
        CONSTRAINT_ERROR,
        "a 15-byte id in a cancel"
    );
    let (list, active) = messages(&node).await;
    assert_eq!((list.len(), active.len()), (0, 0), "nothing stored");
}

/// Messages' FeatureMap on [`EP`]: chip's server answers it from the
/// delegate (messages-server.cpp:136-155), not from the ember RAM default 0.
async fn feature_map(node: &Node) -> u32 {
    use integration_tests::events::payload_tlv;
    use matter_clusters::clusters::globals::{decode_u32, FEATURE_MAP};
    let report = node
        .read(&[ReadPath::concrete(EP, msg::CLUSTER_ID, FEATURE_MAP)])
        .await
        .unwrap();
    let (_, value) = report
        .into_iter()
        .find(|(p, _)| p.attribute == FEATURE_MAP)
        .unwrap();
    decode_u32(&payload_tlv(&value)).unwrap()
}
