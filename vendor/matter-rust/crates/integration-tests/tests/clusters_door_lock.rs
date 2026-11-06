// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

use matter_clusters::clusters::door_lock::{self, LockStateEnum};
use matter_clusters::types::{Nullable, Secret};
use matter_codec::{Tag, TlvWriter};
use matter_controller::{CommandPath, Node, ReadPath, Value};

const DOOR_LOCK: u32 = 0x0101;
const CMD_LOCK_DOOR: u32 = 0x00;
const CMD_UNLOCK_DOOR: u32 = 0x01;
const ATTR_LOCK_STATE: u32 = 0x0000;

fn value_to_tlv(value: &Value) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.write_value(Tag::Anonymous, value)
        .expect("infallible: Vec-backed TlvWriter");
    buf
}

async fn read_lock_state(node: &Node) -> Nullable<LockStateEnum> {
    let r = node
        .read(&[ReadPath::concrete(1, DOOR_LOCK, ATTR_LOCK_STATE)])
        .await
        .expect("read LockState");
    let v = r
        .into_iter()
        .find(|(p, _)| p.attribute == ATTR_LOCK_STATE)
        .map(|(_, v)| v)
        .expect("LockState present");
    door_lock::decode_lock_state(&value_to_tlv(&v)).expect("decode LockState")
}

/// DoorLock (0x0101, ep1) on lock-app: UnlockDoor → LockState == Unlocked;
/// LockDoor → LockState == Locked. DoorLock lock/unlock are timed commands; the
/// lock-app default `RequirePINforRemoteOperation = 0`, so no PIN field is sent.
#[tokio::test]
async fn door_lock_lock_unlock() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("lock") {
        eprintln!("skipped: DoorLock test needs the lock-app DUT (`just integration-lock`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // UnlockDoor (timed, no PIN) → Unlocked.
    node.invoke_timed(
        CommandPath {
            endpoint: 1,
            cluster: DOOR_LOCK,
            command: CMD_UNLOCK_DOOR,
        },
        Value::Structure(vec![]),
        Some(3000),
    )
    .await
    .expect("invoke_timed UnlockDoor");
    assert_eq!(
        read_lock_state(&node).await,
        Nullable::Value(LockStateEnum::Unlocked),
        "LockState did not become Unlocked after UnlockDoor"
    );

    // LockDoor (timed, no PIN) → Locked.
    node.invoke_timed(
        CommandPath {
            endpoint: 1,
            cluster: DOOR_LOCK,
            command: CMD_LOCK_DOOR,
        },
        Value::Structure(vec![]),
        Some(3000),
    )
    .await
    .expect("invoke_timed LockDoor");
    assert_eq!(
        read_lock_state(&node).await,
        Nullable::Value(LockStateEnum::Locked),
        "LockState did not become Locked after LockDoor"
    );
}

// ── DoorLock events (M9-A3 B1) ───────────────────────────────────────────────

/// Our controller's operational node id: the fixture creates its fabric with
/// commissioner node id 1 (`crates/integration-tests/src/fixture.rs`), and
/// door-lock-server reports the invoking CASE peer as an event's `SourceNode`.
const FIXTURE_CONTROLLER_NODE_ID: u64 = 1;

/// The fabric index lock-app gives our fabric: the harness launches it on a
/// fresh KVS and the lock suite commissions it exactly once, so ours is the
/// first (and only) fabric.
const FIXTURE_FABRIC_INDEX: u8 = 1;

/// The highest DoorLock ep1 `event` number before a stimulus (`None` if none).
async fn dl_baseline(node: &Node, event: u32) -> Option<u64> {
    integration_tests::events::latest_event_number(node, 1, DOOR_LOCK, event)
        .await
        .unwrap_or_else(|e| panic!("baseline for DoorLock event {event:#04x}: {e}"))
}

/// Exactly `count` DoorLock ep1 `event`s numbered above `baseline` — the ones
/// the preceding stimulus caused — in event-number order, as `(event number,
/// payload TLV)`. Polls until `count` have appeared or `EVENT_TIMEOUT` passes;
/// any other number fails, since a missing, duplicate or stray event would
/// make the field assertions moot.
async fn dl_new_events(
    node: &Node,
    event: u32,
    baseline: Option<u64>,
    count: usize,
) -> Vec<(u64, Vec<u8>)> {
    use integration_tests::events::{payload_tlv, wait_for_event_after, EVENT_TIMEOUT};

    let deadline = std::time::Instant::now() + EVENT_TIMEOUT;
    loop {
        let mut items = wait_for_event_after(node, 1, DOOR_LOCK, event, baseline)
            .await
            .unwrap_or_else(|e| panic!("DoorLock event {event:#04x}: {e}"));
        if items.len() >= count || std::time::Instant::now() >= deadline {
            items.sort_by_key(|i| i.event_number);
            assert_eq!(
                items.len(),
                count,
                "expected exactly {count} new DoorLock event(s) {event:#04x}: {items:?}"
            );
            return items
                .iter()
                .map(|i| (i.event_number, payload_tlv(&i.value)))
                .collect();
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// The single DoorLock ep1 `event` the preceding stimulus caused
/// ([`dl_new_events`] with a count of one).
async fn dl_new_event(node: &Node, event: u32, baseline: Option<u64>) -> (u64, Vec<u8>) {
    let mut events = dl_new_events(node, event, baseline, 1).await;
    events.remove(0)
}

/// DoorLock events on lock-app ep1, each stimulated, then decoded
/// with the generated `<Name>Event` decoder and its fields asserted against
/// what chip's door-lock-server / lock-app emit for that stimulus:
/// - `LockOperation`: a remote UnlockDoor (Unlatch + Unlock), then LockDoor.
/// - `DoorLockAlarm`, `DoorStateChange`: the lock-app pipe commands
///   `SendDoorLockAlarm` / `SetDoorState` (LockAppCommandDelegate.cpp).
/// - `LockUserChange`: a `SetUser` (Add) of user index 3.
/// - `LockOperationError`: an UnlockDoor with a PIN no credential matches.
///
/// Each wait takes its baseline before the stimulus, so an older event on the
/// same path (boot, an earlier test) can never satisfy it.
#[tokio::test]
async fn door_lock_events_from_lock_app() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("lock") {
        eprintln!("skipped: DoorLock event test needs the lock-app DUT (`just integration-lock`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    lock_operation_events(&node).await;
    alarm_and_door_state_events(&cfg, &node).await;
    lock_user_change_event(&node).await;
    lock_operation_error_event(&node).await;

    // Eight events were stimulated above (UnlockDoor is two); every DoorLock
    // event lock-app holds (theirs plus any from boot) must decode.
    let decoded = decode_every_door_lock_event(&node).await;
    assert!(
        decoded >= 8,
        "expected at least 8 DoorLock events, read {decoded}"
    );
}

fn dl_cmd(command: u32) -> CommandPath {
    CommandPath {
        endpoint: 1,
        cluster: DOOR_LOCK,
        command,
    }
}

/// `LockOperation` for a remote UnlockDoor, then LockDoor, no PIN. lock-app
/// completes each action on a 0 s timer and reports it through
/// `DoorLockServer::SetLockState(.., kRemote, userIndex = null,
/// credentials = null, fabricIdx, nodeId)` (LockEndpoint.cpp
/// `OnLockActionCompleteCallback`), so the user index is null and the
/// credentials field is present but null.
///
/// lock-app's FeatureMap `0x7DB3` has Unbolting (bit 12), so UnlockDoor
/// "pulls the latch" (`LockEndpoint::Unlock` → `kUnlatched`) and the
/// completion callback then moves the lock back to `kUnlocked`: one
/// UnlockDoor is two events, Unlatch then Unlock. LockDoor is one, Lock.
async fn lock_operation_events(node: &Node) {
    use door_lock::{event_id as dl, LockOperationTypeEnum as Op};
    use matter_controller::{ImStatus, InvokeResult};

    let mut baseline = dl_baseline(node, dl::LOCK_OPERATION).await;
    for (command, expected) in [
        (CMD_UNLOCK_DOOR, &[Op::Unlatch, Op::Unlock][..]),
        (CMD_LOCK_DOOR, &[Op::Lock][..]),
    ] {
        let r = node
            .invoke_timed(dl_cmd(command), Value::Structure(vec![]), Some(3000))
            .await
            .expect("invoke_timed Unlock/LockDoor");
        assert!(
            matches!(r, InvokeResult::Status(ImStatus::Success)),
            "{expected:?}: {r:?}"
        );
        let events = dl_new_events(node, dl::LOCK_OPERATION, baseline, expected.len()).await;
        for ((number, tlv), expected) in events.into_iter().zip(expected) {
            baseline = Some(number);
            let op = door_lock::LockOperationEvent::decode(&tlv).expect("LockOperation decodes");
            assert_eq!(op.lock_operation_type, *expected);
            assert_eq!(
                op.operation_source,
                door_lock::OperationSourceEnum::Remote,
                "a controller-invoked operation is Remote"
            );
            assert_eq!(op.user_index, Nullable::Null, "no PIN, so no user");
            assert_eq!(op.fabric_index, Nullable::Value(FIXTURE_FABRIC_INDEX));
            assert_eq!(op.source_node, Nullable::Value(FIXTURE_CONTROLLER_NODE_ID));
            assert_eq!(op.credentials, Some(Nullable::Null), "no credential used");
        }
    }
}

/// `DoorLockAlarm` (LockJammed) and `DoorStateChange` (open, then closed)
/// from the lock-app pipe. lock-app starts with the door closed and
/// `LockEndpoint::SetDoorState` only calls into the server (which then emits)
/// on a change, so open-then-closed is two events.
async fn alarm_and_door_state_events(cfg: &integration_tests::dut::DutConfig, node: &Node) {
    use door_lock::{event_id as dl, DoorStateEnum};
    use integration_tests::events::send_app_pipe;

    let baseline = dl_baseline(node, dl::DOOR_LOCK_ALARM).await;
    send_app_pipe(
        cfg,
        r#"{"Cmd": "SendDoorLockAlarm", "Params": {"EndpointId": 1, "AlarmCode": 0}}"#,
    )
    .await
    .expect("app pipe SendDoorLockAlarm");
    let (_, tlv) = dl_new_event(node, dl::DOOR_LOCK_ALARM, baseline).await;
    let alarm = door_lock::DoorLockAlarmEvent::decode(&tlv).expect("DoorLockAlarm decodes");
    assert_eq!(alarm.alarm_code, door_lock::AlarmCodeEnum::LockJammed);

    let mut baseline = dl_baseline(node, dl::DOOR_STATE_CHANGE).await;
    for (raw, expected) in [(0, DoorStateEnum::DoorOpen), (1, DoorStateEnum::DoorClosed)] {
        send_app_pipe(
            cfg,
            &format!(
                r#"{{"Cmd": "SetDoorState", "Params": {{"EndpointId": 1, "DoorState": {raw}}}}}"#
            ),
        )
        .await
        .expect("app pipe SetDoorState");
        let (number, tlv) = dl_new_event(node, dl::DOOR_STATE_CHANGE, baseline).await;
        baseline = Some(number);
        let change =
            door_lock::DoorStateChangeEvent::decode(&tlv).expect("DoorStateChange decodes");
        assert_eq!(change.door_state, expected);
    }
}

/// `LockUserChange` for a remote `SetUser` (Add) of user index 3 (lock-app has
/// 10 user slots, all free on a fresh KVS). door-lock-server's `createUser`
/// reports it via `sendRemoteLockUserChange(kUserIndex, kAdd, nodeId,
/// fabricIdx, userIndex, dataIndex = userIndex)`.
async fn lock_user_change_event(node: &Node) {
    use door_lock::event_id as dl;
    use matter_controller::{ImStatus, InvokeResult};

    let baseline = dl_baseline(node, dl::LOCK_USER_CHANGE).await;
    let set_user = door_lock::encode_set_user(
        door_lock::DataOperationTypeEnum::Add,
        3,
        Nullable::Value("rust".to_string()),
        Nullable::Value(0xBEEF),
        Nullable::Value(door_lock::UserStatusEnum::OccupiedEnabled),
        Nullable::Value(door_lock::UserTypeEnum::UnrestrictedUser),
        Nullable::Value(door_lock::CredentialRuleEnum::Single),
    );
    let r = node
        .invoke_timed_tlv(
            dl_cmd(door_lock::command_id::SET_USER),
            set_user,
            Some(3000),
        )
        .await
        .expect("invoke_timed SetUser");
    assert!(
        matches!(r, InvokeResult::Status(ImStatus::Success)),
        "SetUser: {r:?}"
    );
    let (_, tlv) = dl_new_event(node, dl::LOCK_USER_CHANGE, baseline).await;
    let change = door_lock::LockUserChangeEvent::decode(&tlv).expect("LockUserChange decodes");
    assert_eq!(
        change.lock_data_type,
        door_lock::LockDataTypeEnum::UserIndex
    );
    assert_eq!(
        change.data_operation_type,
        door_lock::DataOperationTypeEnum::Add
    );
    assert_eq!(
        change.operation_source,
        door_lock::OperationSourceEnum::Remote
    );
    assert_eq!(change.user_index, Nullable::Value(3));
    assert_eq!(change.fabric_index, Nullable::Value(FIXTURE_FABRIC_INDEX));
    assert_eq!(
        change.source_node,
        Nullable::Value(FIXTURE_CONTROLLER_NODE_ID)
    );
    assert_eq!(change.data_index, Nullable::Value(3));
}

/// `LockOperationError` for an UnlockDoor whose PIN matches no credential.
/// door-lock-server's remote-operation handler fails the command with IM
/// `FAILURE` (0x01) and reports `kInvalidCredential` with type Unlock (with
/// Unbolting the attempted operation is Unlatch, `lock_op=4` in lock-app's
/// log, but chip reports a failed Unlatch as Unlock); no user or credential
/// was found, so the user index is null and the credentials field is present
/// but null. One wrong PIN stays under lock-app's `WrongCodeEntryLimit` (3),
/// so no lockout follows.
async fn lock_operation_error_event(node: &Node) {
    use door_lock::event_id as dl;
    use matter_controller::{ImStatus, InvokeResult};

    let baseline = dl_baseline(node, dl::LOCK_OPERATION_ERROR).await;
    let r = node
        .invoke_timed_tlv(
            dl_cmd(CMD_UNLOCK_DOOR),
            door_lock::encode_unlock_door(Some(Secret::new(b"000000".to_vec()))),
            Some(3000),
        )
        .await
        .expect("invoke_timed UnlockDoor (wrong PIN)");
    assert!(
        matches!(r, InvokeResult::Status(ImStatus::Failure(0x01))),
        "an unmatched PIN fails the command with FAILURE: {r:?}"
    );
    let (_, tlv) = dl_new_event(node, dl::LOCK_OPERATION_ERROR, baseline).await;
    let err = door_lock::LockOperationErrorEvent::decode(&tlv).expect("LockOperationError decodes");
    assert_eq!(
        err.lock_operation_type,
        door_lock::LockOperationTypeEnum::Unlock
    );
    assert_eq!(err.operation_source, door_lock::OperationSourceEnum::Remote);
    assert_eq!(
        err.operation_error,
        door_lock::OperationErrorEnum::InvalidCredential
    );
    assert_eq!(err.user_index, Nullable::Null);
    assert_eq!(err.fabric_index, Nullable::Value(FIXTURE_FABRIC_INDEX));
    assert_eq!(err.source_node, Nullable::Value(FIXTURE_CONTROLLER_NODE_ID));
    assert_eq!(err.credentials, Some(Nullable::Null));
}

/// Decode every DoorLock event lock-app reports on ep1 with its generated
/// decoder; an event id unknown to the 1.4 codegen is skipped, not failed.
/// Returns how many events were read.
async fn decode_every_door_lock_event(node: &Node) -> usize {
    use door_lock::event_id as dl;
    use integration_tests::events::{payload_tlv, read_event_items};
    use matter_controller::EventPath;

    let all = read_event_items(node, EventPath::cluster(1, DOOR_LOCK))
        .await
        .expect("read DoorLock events");
    for item in &all {
        let tlv = payload_tlv(&item.value);
        let decoded = match item.path.event.expect("event id") {
            dl::DOOR_LOCK_ALARM => door_lock::DoorLockAlarmEvent::decode(&tlv).map(|_| ()),
            dl::DOOR_STATE_CHANGE => door_lock::DoorStateChangeEvent::decode(&tlv).map(|_| ()),
            dl::LOCK_OPERATION => door_lock::LockOperationEvent::decode(&tlv).map(|_| ()),
            dl::LOCK_OPERATION_ERROR => {
                door_lock::LockOperationErrorEvent::decode(&tlv).map(|_| ())
            }
            dl::LOCK_USER_CHANGE => door_lock::LockUserChangeEvent::decode(&tlv).map(|_| ()),
            other => {
                eprintln!(
                    "[events] DoorLock: skipping event id {other:#04x} unknown to the 1.4 codegen"
                );
                Ok(())
            }
        };
        decoded.unwrap_or_else(|e| panic!("DoorLock event failed to decode: {e}; {item:?}"));
    }
    eprintln!("[events] DoorLock: decoded {} event(s)", all.len());
    all.len()
}
