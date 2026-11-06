// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! The safety clusters on a live all-clusters-app (endpoint 1):
//! SmokeCoAlarm, BooleanStateConfiguration and ValveConfigurationAndControl.
//! Every attribute is decoded with the generated decoder and the exact
//! attribute ids served are asserted; the writable attributes are written,
//! refused where chip refuses them, and restored; every event chip's
//! all-clusters app can be made to emit is stimulated and decoded.
//!
//! Stimuli, all from chip source and checked at v1.4.2.0 (the nightly's pin)
//! as well as master 5cd2917a:
//! - SmokeCoAlarm: `GeneralDiagnostics.TestEventTrigger` with the codes of
//!   `src/app/clusters/smoke-co-alarm-server/SmokeCOTestEventTriggerHandler.h`
//!   (the same at both refs, apart from master's Unmounted codes, unused
//!   here), handled by all-clusters' `smco-stub.cpp`, and the
//!   `SelfTestRequest` command (its 10 s timer, `kSelfTestingTimeoutSec`,
//!   emits SelfTestComplete). The server's setters and their events
//!   (`smoke-co-alarm-server.cpp`) are identical at both refs. all-clusters'
//!   `args.gni` enables the smoke_co trigger at both refs.
//! - BooleanStateConfiguration: TestEventTrigger `kSensorTrigger` /
//!   `kSensorUntrigger` (`BooleanStateConfigurationTestEventTriggerHandler.h`,
//!   all-clusters `boolcfg-stub.cpp`; both refs), SuppressAlarm and
//!   EnableDisableAlarm; SensorFault through the app-pipe command
//!   `SetBooleanStateSensorFault`, which only master has (probed with
//!   `all_clusters_pipe_supports`: an unknown pipe command aborts
//!   all-clusters).
//! - ValveConfigurationAndControl: Open and Close (no trigger exists). The
//!   server differs between the refs (`valve-configuration-and-control-
//!   server.cpp` at v1.4.2.0, `ValveConfigurationAndControlCluster.cpp` on
//!   master); where that changes what goes on the wire the test asks the
//!   checkout which one it has ([`legacy_valve_server`]).
//!
//! The SmokeCoAlarm and BooleanStateConfiguration state chip keeps is
//! persisted (`persist` in all-clusters-app.matter), so every test sets the
//! state it starts from and clears what it raised.

use std::time::{Duration, Instant};

use integration_tests::dut::DutConfig;
use integration_tests::events::{
    all_clusters_pipe_supports, latest_event_number, payload_tlv, read_event_items, send_app_pipe,
    test_event_trigger, wait_for_event_after, wait_for_event_within,
};
use integration_tests::sweep::{
    all_clusters_serves_attribute, assert_exact_attribute_ids, attribute_tlv,
    decode_every_attribute, invoke_for_status, newer_than_codegen, ok, read_cluster_attributes,
    write_attribute,
};
use matter_clusters::clusters::{
    boolean_state_configuration as bsc, smoke_co_alarm as smoke,
    valve_configuration_and_control as valve,
};
use matter_clusters::types::Nullable;
use matter_codec::Value;
use matter_controller::{AttributePath, CommandPath, EventPath, ImStatus, MatterController, Node};

/// Every safety cluster all-clusters serves lives on endpoint 1.
const EP: u16 = 1;

/// IM status codes the servers answer with (Matter Core spec 8.10.1).
const CONSTRAINT_ERROR: ImStatus = ImStatus::Failure(0x87);
const BUSY: ImStatus = ImStatus::Failure(0x9C);
const INVALID_IN_STATE: ImStatus = ImStatus::Failure(0xCB);

/// The DUT config, controller and node id, or `None` (skip) unless the DUT is
/// all-clusters.
async fn connect_all_clusters() -> Option<(DutConfig, MatterController, u64)> {
    let cfg = DutConfig::from_env()?;
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: safety sweep needs the all-clusters DUT (`just integration`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((cfg, controller, node_id))
}

/// A path on [`EP`].
fn path(cluster: u32, attribute: u32) -> AttributePath {
    AttributePath {
        endpoint: EP,
        cluster,
        attribute,
    }
}

/// Invoke a command on [`EP`] and return the bare status it answers.
async fn status_of(node: &Node, cluster: u32, command: u32, fields: Vec<u8>) -> ImStatus {
    invoke_for_status(
        node,
        CommandPath {
            endpoint: EP,
            cluster,
            command,
        },
        fields,
    )
    .await
    .unwrap()
}

/// One attribute of `cluster` on [`EP`], read fresh, as its value TLV.
async fn read_tlv(node: &Node, cluster: u32, attribute: u32) -> Vec<u8> {
    let attrs = read_cluster_attributes(node, EP, cluster).await.unwrap();
    attribute_tlv(&attrs, attribute).to_vec()
}

/// The newest event on `(EP, cluster, event)` after `baseline`.
async fn newest_event_after(node: &Node, cluster: u32, event: u32, baseline: Option<u64>) -> Value {
    let items = wait_for_event_after(node, EP, cluster, event, baseline)
        .await
        .unwrap();
    items
        .into_iter()
        .max_by_key(|i| i.event_number)
        .unwrap()
        .value
}

// ---- SmokeCoAlarm ---------------------------------------------------------------

/// TestEventTrigger codes (`SmokeCOTrigger`, SmokeCOTestEventTriggerHandler.h).
mod smoke_trigger {
    pub(super) const FORCE_SMOKE_WARNING: u64 = 0x005c_0000_0000_0090;
    pub(super) const FORCE_CO_WARNING: u64 = 0x005c_0000_0000_0091;
    pub(super) const FORCE_SMOKE_INTERCONNECT: u64 = 0x005c_0000_0000_0092;
    pub(super) const FORCE_MALFUNCTION: u64 = 0x005c_0000_0000_0093;
    pub(super) const FORCE_CO_INTERCONNECT: u64 = 0x005c_0000_0000_0094;
    pub(super) const FORCE_LOW_BATTERY_WARNING: u64 = 0x005c_0000_0000_0095;
    pub(super) const FORCE_END_OF_LIFE: u64 = 0x005c_0000_0000_009a;
    pub(super) const FORCE_SILENCE: u64 = 0x005c_0000_0000_009b;
    pub(super) const FORCE_SMOKE_CRITICAL: u64 = 0x005c_0000_0000_009c;
    pub(super) const CLEAR_SMOKE: u64 = 0x005c_0000_0000_00a0;
    pub(super) const CLEAR_CO: u64 = 0x005c_0000_0000_00a1;
    pub(super) const CLEAR_SMOKE_INTERCONNECT: u64 = 0x005c_0000_0000_00a2;
    pub(super) const CLEAR_MALFUNCTION: u64 = 0x005c_0000_0000_00a3;
    pub(super) const CLEAR_CO_INTERCONNECT: u64 = 0x005c_0000_0000_00a4;
    pub(super) const CLEAR_BATTERY_LEVEL_LOW: u64 = 0x005c_0000_0000_00a5;
    pub(super) const CLEAR_CONTAMINATION: u64 = 0x005c_0000_0000_00a6;
    pub(super) const CLEAR_SENSITIVITY: u64 = 0x005c_0000_0000_00a8;
    pub(super) const CLEAR_END_OF_LIFE: u64 = 0x005c_0000_0000_00aa;
    pub(super) const CLEAR_SILENCE: u64 = 0x005c_0000_0000_00ab;
}

/// Every clear trigger: smco-stub.cpp sets each condition back to normal
/// (sensitivity to Standard), and re-derives ExpressedState, so the alarm
/// ends in its quiet state whatever an earlier run left behind.
const SMOKE_CLEAR_ALL: [u64; 10] = [
    smoke_trigger::CLEAR_SMOKE,
    smoke_trigger::CLEAR_CO,
    smoke_trigger::CLEAR_SMOKE_INTERCONNECT,
    smoke_trigger::CLEAR_MALFUNCTION,
    smoke_trigger::CLEAR_CO_INTERCONNECT,
    smoke_trigger::CLEAR_BATTERY_LEVEL_LOW,
    smoke_trigger::CLEAR_CONTAMINATION,
    smoke_trigger::CLEAR_SENSITIVITY,
    smoke_trigger::CLEAR_END_OF_LIFE,
    smoke_trigger::CLEAR_SILENCE,
];

async fn reset_smoke_co_alarm(node: &Node) {
    for trigger in SMOKE_CLEAR_ALL {
        test_event_trigger(node, trigger).await.unwrap();
    }
}

async fn expressed_state(node: &Node) -> smoke::ExpressedStateEnum {
    let tlv = read_tlv(
        node,
        smoke::CLUSTER_ID,
        smoke::attribute_id::EXPRESSED_STATE,
    )
    .await;
    smoke::decode_expressed_state(&tlv).unwrap()
}

/// Every SmokeCoAlarm attribute decodes; the exact ids served (0x0000..=0x000C
/// at both refs, plus master's `unmounted` 0x000D, cluster revision 2, which
/// the 1.4 codegen does not know); the quiet state after every clear trigger;
/// and SmokeSensitivityLevel written and restored.
#[tokio::test]
async fn smoke_co_alarm_decodes_every_attribute_and_writes_its_sensitivity() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use smoke::{attribute_id as a, AlarmStateEnum, ExpressedStateEnum, SensitivityEnum};
    reset_smoke_co_alarm(&node).await;
    let attrs = decode_every_attribute(&node, EP, smoke::CLUSTER_ID, |id, t| match id {
        a::EXPRESSED_STATE => ok(smoke::decode_expressed_state(t)),
        a::SMOKE_STATE => ok(smoke::decode_smoke_state(t)),
        a::CO_STATE => ok(smoke::decode_co_state(t)),
        a::BATTERY_ALERT => ok(smoke::decode_battery_alert(t)),
        a::DEVICE_MUTED => ok(smoke::decode_device_muted(t)),
        a::TEST_IN_PROGRESS => ok(smoke::decode_test_in_progress(t)),
        a::HARDWARE_FAULT_ALERT => ok(smoke::decode_hardware_fault_alert(t)),
        a::END_OF_SERVICE_ALERT => ok(smoke::decode_end_of_service_alert(t)),
        a::INTERCONNECT_SMOKE_ALARM => ok(smoke::decode_interconnect_smoke_alarm(t)),
        a::INTERCONNECT_CO_ALARM => ok(smoke::decode_interconnect_co_alarm(t)),
        a::CONTAMINATION_STATE => ok(smoke::decode_contamination_state(t)),
        a::SMOKE_SENSITIVITY_LEVEL => ok(smoke::decode_smoke_sensitivity_level(t)),
        a::EXPIRY_DATE => ok(smoke::decode_expiry_date(t)),
        other => newer_than_codegen("SmokeCoAlarm", other),
    })
    .await;
    let mut want: Vec<u32> = (0x0000..=0x000C).collect();
    if all_clusters_serves_attribute(&cfg, EP, "SmokeCoAlarm", "unmounted").unwrap() {
        want.push(0x000D);
    }
    assert_exact_attribute_ids("SmokeCoAlarm", &attrs, &want);
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        smoke::decode_expressed_state(tlv(a::EXPRESSED_STATE)).unwrap(),
        ExpressedStateEnum::Normal
    );
    for id in [a::SMOKE_STATE, a::CO_STATE, a::BATTERY_ALERT] {
        assert_eq!(
            smoke::decode_smoke_state(tlv(id)).unwrap(),
            AlarmStateEnum::Normal,
            "{id:#06x}"
        );
    }
    assert!(!smoke::decode_test_in_progress(tlv(a::TEST_IN_PROGRESS)).unwrap());
    assert!(!smoke::decode_hardware_fault_alert(tlv(a::HARDWARE_FAULT_ALERT)).unwrap());
    assert_eq!(
        smoke::decode_smoke_sensitivity_level(tlv(a::SMOKE_SENSITIVITY_LEVEL)).unwrap(),
        SensitivityEnum::Standard,
        "kClearSensitivity sets Standard"
    );

    let sensitivity = path(smoke::CLUSTER_ID, a::SMOKE_SENSITIVITY_LEVEL);
    let write = |level| {
        write_attribute(
            &node,
            sensitivity,
            smoke::encode_smoke_sensitivity_level(level),
        )
    };
    let read_level = || async {
        let t = read_tlv(&node, smoke::CLUSTER_ID, a::SMOKE_SENSITIVITY_LEVEL).await;
        smoke::decode_smoke_sensitivity_level(&t).unwrap()
    };
    assert_eq!(
        write(SensitivityEnum::Low).await.unwrap(),
        ImStatus::Success
    );
    assert_eq!(read_level().await, SensitivityEnum::Low);
    assert_eq!(
        write(SensitivityEnum::Standard).await.unwrap(),
        ImStatus::Success
    );
    assert_eq!(read_level().await, SensitivityEnum::Standard, "restored");
}

/// One forced alarm: the trigger that raises it, the trigger that clears it,
/// the event it emits, the ExpressedState it leads to (smco-stub.cpp's
/// priority order) and the AlarmSeverityLevel the event carries (`None`: a
/// fieldless event).
struct ForcedAlarm {
    force: u64,
    clear: u64,
    event: u32,
    expressed: smoke::ExpressedStateEnum,
    severity: Option<smoke::AlarmStateEnum>,
}

/// Decode the AlarmSeverityLevel of a SmokeAlarm / CoAlarm / LowBattery /
/// InterconnectSmokeAlarm / InterconnectCoAlarm payload (one shape, five
/// generated structs).
fn severity_of(event: u32, value: &Value) -> smoke::AlarmStateEnum {
    use smoke::event_id as e;
    let tlv = payload_tlv(value);
    match event {
        e::SMOKE_ALARM => {
            smoke::SmokeAlarmEvent::decode(&tlv)
                .unwrap()
                .alarm_severity_level
        }
        e::CO_ALARM => {
            smoke::CoAlarmEvent::decode(&tlv)
                .unwrap()
                .alarm_severity_level
        }
        e::LOW_BATTERY => {
            smoke::LowBatteryEvent::decode(&tlv)
                .unwrap()
                .alarm_severity_level
        }
        e::INTERCONNECT_SMOKE_ALARM => {
            smoke::InterconnectSmokeAlarmEvent::decode(&tlv)
                .unwrap()
                .alarm_severity_level
        }
        e::INTERCONNECT_CO_ALARM => {
            smoke::InterconnectCoAlarmEvent::decode(&tlv)
                .unwrap()
                .alarm_severity_level
        }
        other => panic!("event {other:#04x} carries no AlarmSeverityLevel"),
    }
}

/// Raise `alarm`, check its event, payload and ExpressedState, then clear it
/// and check the AllClear event chip emits when ExpressedState returns to
/// Normal (`SetExpressedState`).
async fn force_and_clear(node: &Node, alarm: &ForcedAlarm) {
    let cl = smoke::CLUSTER_ID;
    let base = latest_event_number(node, EP, cl, alarm.event)
        .await
        .unwrap();
    test_event_trigger(node, alarm.force).await.unwrap();
    let value = newest_event_after(node, cl, alarm.event, base).await;
    match alarm.severity {
        Some(want) => assert_eq!(
            severity_of(alarm.event, &value),
            want,
            "{:#04x}",
            alarm.event
        ),
        None => assert_eq!(
            value,
            Value::Structure(vec![]),
            "{:#04x} is fieldless",
            alarm.event
        ),
    }
    assert_eq!(
        expressed_state(node).await,
        alarm.expressed,
        "{:#04x}",
        alarm.event
    );
    let clear_base = latest_event_number(node, EP, cl, smoke::event_id::ALL_CLEAR)
        .await
        .unwrap();
    test_event_trigger(node, alarm.clear).await.unwrap();
    let all_clear = newest_event_after(node, cl, smoke::event_id::ALL_CLEAR, clear_base).await;
    assert_eq!(all_clear, Value::Structure(vec![]));
    assert_eq!(
        expressed_state(node).await,
        smoke::ExpressedStateEnum::Normal
    );
}

/// Eight forced alarms, one at a time from the quiet state: each event
/// decodes with the severity smco-stub.cpp forces (smoke both Critical and
/// Warning, Warning for the rest; HardwareFault and EndOfService carry
/// nothing), ExpressedState follows, and each clear emits AllClear.
#[tokio::test]
async fn smoke_co_alarm_forced_alarms_emit_their_events_and_clear() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use smoke::{event_id as e, AlarmStateEnum as A, ExpressedStateEnum as X};
    use smoke_trigger as t;
    reset_smoke_co_alarm(&node).await;
    let alarms = [
        ForcedAlarm {
            force: t::FORCE_SMOKE_CRITICAL,
            clear: t::CLEAR_SMOKE,
            event: e::SMOKE_ALARM,
            expressed: X::SmokeAlarm,
            severity: Some(A::Critical),
        },
        ForcedAlarm {
            force: t::FORCE_SMOKE_WARNING,
            clear: t::CLEAR_SMOKE,
            event: e::SMOKE_ALARM,
            expressed: X::SmokeAlarm,
            severity: Some(A::Warning),
        },
        ForcedAlarm {
            force: t::FORCE_CO_WARNING,
            clear: t::CLEAR_CO,
            event: e::CO_ALARM,
            expressed: X::CoAlarm,
            severity: Some(A::Warning),
        },
        ForcedAlarm {
            force: t::FORCE_LOW_BATTERY_WARNING,
            clear: t::CLEAR_BATTERY_LEVEL_LOW,
            event: e::LOW_BATTERY,
            expressed: X::BatteryAlert,
            severity: Some(A::Warning),
        },
        ForcedAlarm {
            force: t::FORCE_SMOKE_INTERCONNECT,
            clear: t::CLEAR_SMOKE_INTERCONNECT,
            event: e::INTERCONNECT_SMOKE_ALARM,
            expressed: X::InterconnectSmoke,
            severity: Some(A::Warning),
        },
        ForcedAlarm {
            force: t::FORCE_CO_INTERCONNECT,
            clear: t::CLEAR_CO_INTERCONNECT,
            event: e::INTERCONNECT_CO_ALARM,
            expressed: X::InterconnectCo,
            severity: Some(A::Warning),
        },
        ForcedAlarm {
            force: t::FORCE_MALFUNCTION,
            clear: t::CLEAR_MALFUNCTION,
            event: e::HARDWARE_FAULT,
            expressed: X::HardwareFault,
            severity: None,
        },
        ForcedAlarm {
            force: t::FORCE_END_OF_LIFE,
            clear: t::CLEAR_END_OF_LIFE,
            event: e::END_OF_SERVICE,
            expressed: X::EndOfService,
            severity: None,
        },
    ];
    for alarm in &alarms {
        force_and_clear(&node, alarm).await;
    }
    reset_smoke_co_alarm(&node).await;
}

/// kForceSilence mutes (AlarmMuted; allowed because nothing is Critical,
/// `SetDeviceMuted`) and kClearSilence unmutes (MuteEnded); DeviceMuted
/// follows.
#[tokio::test]
async fn smoke_co_alarm_mute_and_unmute_emit_their_events() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use smoke::{attribute_id as a, event_id as e, MuteStateEnum};
    let cl = smoke::CLUSTER_ID;
    reset_smoke_co_alarm(&node).await;
    let muted = || async {
        smoke::decode_device_muted(&read_tlv(&node, cl, a::DEVICE_MUTED).await).unwrap()
    };
    assert_eq!(muted().await, MuteStateEnum::NotMuted);
    let base = latest_event_number(&node, EP, cl, e::ALARM_MUTED)
        .await
        .unwrap();
    test_event_trigger(&node, smoke_trigger::FORCE_SILENCE)
        .await
        .unwrap();
    assert_eq!(
        newest_event_after(&node, cl, e::ALARM_MUTED, base).await,
        Value::Structure(vec![])
    );
    assert_eq!(muted().await, MuteStateEnum::Muted);
    let base = latest_event_number(&node, EP, cl, e::MUTE_ENDED)
        .await
        .unwrap();
    test_event_trigger(&node, smoke_trigger::CLEAR_SILENCE)
        .await
        .unwrap();
    assert_eq!(
        newest_event_after(&node, cl, e::MUTE_ENDED, base).await,
        Value::Structure(vec![])
    );
    assert_eq!(muted().await, MuteStateEnum::NotMuted, "restored");
}

/// SelfTestRequest from the quiet state: Success, TestInProgress and
/// ExpressedState Testing at once (`HandleRemoteSelfTestRequest`); a second
/// request while testing is Busy; 10 s later (`kSelfTestingTimeoutSec`)
/// SelfTestComplete, TestInProgress false, and AllClear as ExpressedState
/// returns to Normal (`EndSelfTestingEventHandler`).
#[tokio::test]
async fn smoke_co_alarm_self_test_reports_testing_then_completes() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use smoke::{attribute_id as a, event_id as e, ExpressedStateEnum};
    let cl = smoke::CLUSTER_ID;
    reset_smoke_co_alarm(&node).await;
    let complete_base = latest_event_number(&node, EP, cl, e::SELF_TEST_COMPLETE)
        .await
        .unwrap();
    let clear_base = latest_event_number(&node, EP, cl, e::ALL_CLEAR)
        .await
        .unwrap();
    let self_test = || {
        status_of(
            &node,
            cl,
            smoke::command_id::SELF_TEST_REQUEST,
            smoke::encode_self_test_request(),
        )
    };
    assert_eq!(self_test().await, ImStatus::Success);
    let in_progress = || async {
        smoke::decode_test_in_progress(&read_tlv(&node, cl, a::TEST_IN_PROGRESS).await).unwrap()
    };
    assert!(in_progress().await);
    assert_eq!(expressed_state(&node).await, ExpressedStateEnum::Testing);
    assert_eq!(self_test().await, BUSY, "a self-test is already running");
    let items = wait_for_event_within(
        &node,
        EP,
        cl,
        e::SELF_TEST_COMPLETE,
        complete_base,
        Duration::from_secs(20),
    )
    .await
    .unwrap();
    assert_eq!(items.len(), 1, "one self-test, one completion: {items:?}");
    assert!(!in_progress().await);
    assert_eq!(expressed_state(&node).await, ExpressedStateEnum::Normal);
    let all_clear = newest_event_after(&node, cl, e::ALL_CLEAR, clear_base).await;
    assert_eq!(all_clear, Value::Structure(vec![]));
}

// ---- BooleanStateConfiguration -------------------------------------------------

/// TestEventTrigger codes (`BooleanStateConfigurationTrigger`).
const BSC_SENSOR_TRIGGER: u64 = 0x0080_0000_0000_0000;
const BSC_SENSOR_UNTRIGGER: u64 = 0x0080_0000_0000_0001;

/// All eight attributes at both refs; SupportedSensitivityLevels 3,
/// DefaultSensitivityLevel 2 and AlarmsSupported Visual|Audible
/// (all-clusters-app.matter); CurrentSensitivityLevel written, refused at
/// SupportedSensitivityLevels (ConstraintError: `SetCurrentSensitivityLevel`
/// on master, `StoreCurrentSensitivityLevel` at v1.4.2.0) and restored.
#[tokio::test]
async fn boolean_state_configuration_decodes_and_validates_its_sensitivity() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use bsc::{attribute_id as a, AlarmModeBitmap};
    let attrs = decode_every_attribute(&node, EP, bsc::CLUSTER_ID, |id, t| match id {
        a::CURRENT_SENSITIVITY_LEVEL => ok(bsc::decode_current_sensitivity_level(t)),
        a::SUPPORTED_SENSITIVITY_LEVELS => ok(bsc::decode_supported_sensitivity_levels(t)),
        a::DEFAULT_SENSITIVITY_LEVEL => ok(bsc::decode_default_sensitivity_level(t)),
        a::ALARMS_ACTIVE => ok(bsc::decode_alarms_active(t)),
        a::ALARMS_SUPPRESSED => ok(bsc::decode_alarms_suppressed(t)),
        a::ALARMS_ENABLED => ok(bsc::decode_alarms_enabled(t)),
        a::ALARMS_SUPPORTED => ok(bsc::decode_alarms_supported(t)),
        a::SENSOR_FAULT => ok(bsc::decode_sensor_fault(t)),
        other => newer_than_codegen("BooleanStateConfiguration", other),
    })
    .await;
    assert_exact_attribute_ids(
        "BooleanStateConfiguration",
        &attrs,
        &(0x0000..=0x0007).collect::<Vec<_>>(),
    );
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        bsc::decode_supported_sensitivity_levels(tlv(a::SUPPORTED_SENSITIVITY_LEVELS)).unwrap(),
        3
    );
    assert_eq!(
        bsc::decode_default_sensitivity_level(tlv(a::DEFAULT_SENSITIVITY_LEVEL)).unwrap(),
        2
    );
    assert_eq!(
        bsc::decode_alarms_supported(tlv(a::ALARMS_SUPPORTED)).unwrap(),
        AlarmModeBitmap::all()
    );
    let before = bsc::decode_current_sensitivity_level(tlv(a::CURRENT_SENSITIVITY_LEVEL)).unwrap();
    let target = u8::from(before == 0);
    let level = path(bsc::CLUSTER_ID, a::CURRENT_SENSITIVITY_LEVEL);
    let write = |v| write_attribute(&node, level, bsc::encode_current_sensitivity_level(v));
    let read_level = || async {
        bsc::decode_current_sensitivity_level(
            &read_tlv(&node, bsc::CLUSTER_ID, a::CURRENT_SENSITIVITY_LEVEL).await,
        )
        .unwrap()
    };
    assert_eq!(write(target).await.unwrap(), ImStatus::Success);
    assert_eq!(read_level().await, target);
    assert_eq!(
        write(3).await.unwrap(),
        CONSTRAINT_ERROR,
        "three levels: 0..=2"
    );
    assert_eq!(
        read_level().await,
        target,
        "a refused write changes nothing"
    );
    assert_eq!(write(before).await.unwrap(), ImStatus::Success);
    assert_eq!(read_level().await, before, "restored");
}

/// The newest AlarmsStateChanged after `baseline`, decoded, as
/// `(AlarmsActive, AlarmsSuppressed)`.
async fn alarms_state_changed_after(
    node: &Node,
    baseline: Option<u64>,
) -> (bsc::AlarmModeBitmap, Option<bsc::AlarmModeBitmap>) {
    let value = newest_event_after(
        node,
        bsc::CLUSTER_ID,
        bsc::event_id::ALARMS_STATE_CHANGED,
        baseline,
    )
    .await;
    let e = bsc::AlarmsStateChangedEvent::decode(&payload_tlv(&value)).unwrap();
    (e.alarms_active, e.alarms_suppressed)
}

async fn alarms_baseline(node: &Node) -> Option<u64> {
    latest_event_number(
        node,
        EP,
        bsc::CLUSTER_ID,
        bsc::event_id::ALARMS_STATE_CHANGED,
    )
    .await
    .unwrap()
}

async fn bsc_command(node: &Node, command: u32, alarms: bsc::AlarmModeBitmap) -> ImStatus {
    let fields = match command {
        bsc::command_id::SUPPRESS_ALARM => bsc::encode_suppress_alarm(alarms),
        bsc::command_id::ENABLE_DISABLE_ALARM => bsc::encode_enable_disable_alarm(alarms),
        other => panic!("bsc_command: command {other:#04x} takes no AlarmModeBitmap"),
    };
    status_of(node, bsc::CLUSTER_ID, command, fields).await
}

/// With both alarms enabled: SuppressAlarm is InvalidInState while nothing is
/// active; kSensorTrigger activates every enabled alarm (AlarmsStateChanged,
/// AlarmsSuppressed present because of SPRS); SuppressAlarm(Visual) then
/// succeeds (another event); kSensorUntrigger clears both (another event);
/// EnableDisableAlarm with an unsupported bit is ConstraintError; AlarmsEnabled
/// is restored. Server logic: `BooleanStateConfigurationCluster.cpp` on
/// master, `boolean-state-configuration-server.cpp` at v1.4.2.0 (the same
/// statuses and events here).
#[tokio::test]
async fn boolean_state_configuration_alarms_follow_triggers_and_commands() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = &controller.node(node_id);
    use bsc::{attribute_id as a, command_id as c, AlarmModeBitmap as M};
    let read = |id| async move { read_tlv(node, bsc::CLUSTER_ID, id).await };
    let enabled_before = bsc::decode_alarms_enabled(&read(a::ALARMS_ENABLED).await).unwrap();
    assert_eq!(
        bsc_command(node, c::ENABLE_DISABLE_ALARM, M::all()).await,
        ImStatus::Success
    );
    test_event_trigger(node, BSC_SENSOR_UNTRIGGER)
        .await
        .unwrap();
    assert_eq!(
        bsc::decode_alarms_active(&read(a::ALARMS_ACTIVE).await).unwrap(),
        M::empty()
    );

    assert_eq!(
        bsc_command(node, c::SUPPRESS_ALARM, M::VISUAL).await,
        INVALID_IN_STATE
    );
    let base = alarms_baseline(node).await;
    test_event_trigger(node, BSC_SENSOR_TRIGGER).await.unwrap();
    assert_eq!(
        alarms_state_changed_after(node, base).await,
        (M::all(), Some(M::empty()))
    );
    assert_eq!(
        bsc::decode_alarms_active(&read(a::ALARMS_ACTIVE).await).unwrap(),
        M::all()
    );

    let base = alarms_baseline(node).await;
    assert_eq!(
        bsc_command(node, c::SUPPRESS_ALARM, M::VISUAL).await,
        ImStatus::Success
    );
    assert_eq!(
        alarms_state_changed_after(node, base).await,
        (M::all(), Some(M::VISUAL))
    );
    assert_eq!(
        bsc::decode_alarms_suppressed(&read(a::ALARMS_SUPPRESSED).await).unwrap(),
        M::VISUAL
    );

    let base = alarms_baseline(node).await;
    test_event_trigger(node, BSC_SENSOR_UNTRIGGER)
        .await
        .unwrap();
    assert_eq!(
        alarms_state_changed_after(node, base).await,
        (M::empty(), Some(M::empty()))
    );
    assert_eq!(
        bsc::decode_alarms_suppressed(&read(a::ALARMS_SUPPRESSED).await).unwrap(),
        M::empty()
    );

    let unsupported = M::from_bits_retain(0x04);
    assert_eq!(
        bsc_command(node, c::ENABLE_DISABLE_ALARM, unsupported).await,
        CONSTRAINT_ERROR
    );
    assert_eq!(
        bsc_command(node, c::ENABLE_DISABLE_ALARM, enabled_before).await,
        ImStatus::Success
    );
    assert_eq!(
        bsc::decode_alarms_enabled(&read(a::ALARMS_ENABLED).await).unwrap(),
        enabled_before,
        "AlarmsEnabled restored"
    );
}

/// SensorFault through the app pipe, where the checkout's all-clusters has
/// the `SetBooleanStateSensorFault` command (master; v1.4.2.0 has none, and
/// an unknown pipe command aborts the DUT): the event carries the fault, the
/// SensorFault attribute follows (`GenerateSensorFault`), and a zero fault
/// restores it.
#[tokio::test]
async fn boolean_state_configuration_sensor_fault_via_app_pipe() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    if !all_clusters_pipe_supports(&cfg, "SetBooleanStateSensorFault").unwrap() {
        eprintln!(
            "[safety] SensorFault not stimulated: this chip checkout has no \
             SetBooleanStateSensorFault pipe command (v1.4.2.0); decode smoke covers the payload"
        );
        return;
    }
    let node = controller.node(node_id);
    use bsc::{attribute_id as a, event_id as e, SensorFaultBitmap as F};
    let cl = bsc::CLUSTER_ID;
    for (raw, want) in [(1, F::GENERAL_FAULT), (0, F::empty())] {
        let base = latest_event_number(&node, EP, cl, e::SENSOR_FAULT)
            .await
            .unwrap();
        send_app_pipe(
            &cfg,
            &format!(
                r#"{{"Name": "SetBooleanStateSensorFault", "EndpointId": 1, "SensorFault": {raw}}}"#
            ),
        )
        .await
        .expect("app pipe");
        let value = newest_event_after(&node, cl, e::SENSOR_FAULT, base).await;
        let event = bsc::SensorFaultEvent::decode(&payload_tlv(&value)).unwrap();
        assert_eq!(event.sensor_fault, want);
        let attr = bsc::decode_sensor_fault(&read_tlv(&node, cl, a::SENSOR_FAULT).await).unwrap();
        assert_eq!(attr, want);
    }
}

// ---- ValveConfigurationAndControl ---------------------------------------------

/// True when the checkout's valve server is v1.4.2.0's
/// `valve-configuration-and-control-server.cpp` (master replaced it with
/// `ValveConfigurationAndControlCluster.cpp`). Two wire differences follow:
/// v1.4.2.0's ValveStateChanged carries no ValveLevel
/// (`emitValveStateChangedEvent`), master's does with LVL
/// (`EmitValveChangeEvent`); and v1.4.2.0 emits ValveFault{GeneralFault} when
/// it refuses an Open (`emberAfValveConfigurationAndControlClusterOpenCallback`,
/// `exit:`), master emits nothing.
fn legacy_valve_server(cfg: &DutConfig) -> bool {
    cfg.chip_root
        .join("src/app/clusters/valve-configuration-and-control-server/valve-configuration-and-control-server.cpp")
        .exists()
}

async fn valve_state(node: &Node) -> Nullable<valve::ValveStateEnum> {
    let tlv = read_tlv(node, valve::CLUSTER_ID, valve::attribute_id::CURRENT_STATE).await;
    valve::decode_current_state(&tlv).unwrap()
}

/// Poll CurrentState until it is `want` or `timeout` passes (Open and Close
/// complete through the delegate, and a timed Open closes on a 1 s tick).
async fn wait_for_valve_state(node: &Node, want: valve::ValveStateEnum, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let got = valve_state(node).await;
        if got == Nullable::Value(want) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "CurrentState {got:?}, waiting for {want:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Close the valve (the state every valve test starts from) and wait for
/// CurrentState Closed.
async fn close_valve(node: &Node) {
    let st = status_of(
        node,
        valve::CLUSTER_ID,
        valve::command_id::CLOSE,
        valve::encode_close(),
    )
    .await;
    assert_eq!(st, ImStatus::Success);
    wait_for_valve_state(node, valve::ValveStateEnum::Closed, Duration::from_secs(5)).await;
}

/// Every Valve attribute decodes (all eleven served at both refs); the closed
/// state (`CloseValve`: durations and targets null, CurrentLevel 0); LevelStep
/// as all-clusters-app.matter sets it (1 at v1.4.2.0, 2 on master); and both
/// writable defaults written and restored.
#[tokio::test]
async fn valve_decodes_every_attribute_and_writes_its_defaults() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use valve::attribute_id as a;
    close_valve(&node).await;
    let attrs = decode_every_attribute(&node, EP, valve::CLUSTER_ID, |id, t| match id {
        a::OPEN_DURATION => ok(valve::decode_open_duration(t)),
        a::DEFAULT_OPEN_DURATION => ok(valve::decode_default_open_duration(t)),
        a::AUTO_CLOSE_TIME => ok(valve::decode_auto_close_time(t)),
        a::REMAINING_DURATION => ok(valve::decode_remaining_duration(t)),
        a::CURRENT_STATE => ok(valve::decode_current_state(t)),
        a::TARGET_STATE => ok(valve::decode_target_state(t)),
        a::CURRENT_LEVEL => ok(valve::decode_current_level(t)),
        a::TARGET_LEVEL => ok(valve::decode_target_level(t)),
        a::DEFAULT_OPEN_LEVEL => ok(valve::decode_default_open_level(t)),
        a::VALVE_FAULT => ok(valve::decode_valve_fault(t)),
        a::LEVEL_STEP => ok(valve::decode_level_step(t)),
        other => newer_than_codegen("ValveConfigurationAndControl", other),
    })
    .await;
    assert_exact_attribute_ids(
        "ValveConfigurationAndControl",
        &attrs,
        &(0x0000..=0x000A).collect::<Vec<_>>(),
    );
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        valve::decode_open_duration(tlv(a::OPEN_DURATION)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        valve::decode_remaining_duration(tlv(a::REMAINING_DURATION)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        valve::decode_auto_close_time(tlv(a::AUTO_CLOSE_TIME)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        valve::decode_target_state(tlv(a::TARGET_STATE)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        valve::decode_current_level(tlv(a::CURRENT_LEVEL)).unwrap(),
        Nullable::Value(0)
    );
    assert_eq!(
        valve::decode_valve_fault(tlv(a::VALVE_FAULT)).unwrap(),
        valve::ValveFaultBitmap::empty()
    );
    let step = if legacy_valve_server(&cfg) { 1 } else { 2 };
    assert_eq!(valve::decode_level_step(tlv(a::LEVEL_STEP)).unwrap(), step);

    let duration_before =
        valve::decode_default_open_duration(tlv(a::DEFAULT_OPEN_DURATION)).unwrap();
    let duration = path(valve::CLUSTER_ID, a::DEFAULT_OPEN_DURATION);
    for v in [Nullable::Value(30), duration_before] {
        let st = write_attribute(&node, duration, valve::encode_default_open_duration(v)).await;
        assert_eq!(st.unwrap(), ImStatus::Success);
        let back = read_tlv(&node, valve::CLUSTER_ID, a::DEFAULT_OPEN_DURATION).await;
        assert_eq!(valve::decode_default_open_duration(&back).unwrap(), v);
    }
    let level_before = valve::decode_default_open_level(tlv(a::DEFAULT_OPEN_LEVEL)).unwrap();
    let level = path(valve::CLUSTER_ID, a::DEFAULT_OPEN_LEVEL);
    for v in [50, level_before] {
        let st = write_attribute(&node, level, valve::encode_default_open_level(v)).await;
        assert_eq!(st.unwrap(), ImStatus::Success);
        let back = read_tlv(&node, valve::CLUSTER_ID, a::DEFAULT_OPEN_LEVEL).await;
        assert_eq!(valve::decode_default_open_level(&back).unwrap(), v);
    }
}

/// Every ValveStateChanged after `baseline`, decoded, oldest first, once one
/// of them reports `last`; panics if none does within 10 s.
async fn valve_states_until(
    node: &Node,
    baseline: Option<u64>,
    last: valve::ValveStateEnum,
) -> Vec<(valve::ValveStateEnum, Option<u8>)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut items = read_event_items(
            node,
            EventPath::concrete(EP, valve::CLUSTER_ID, valve::event_id::VALVE_STATE_CHANGED),
        )
        .await
        .unwrap();
        items.retain(|i| baseline.is_none_or(|b| i.event_number > b));
        items.sort_by_key(|i| i.event_number);
        let states: Vec<_> = items
            .iter()
            .map(|i| {
                let e = valve::ValveStateChangedEvent::decode(&payload_tlv(&i.value)).unwrap();
                (e.valve_state, e.valve_level)
            })
            .collect();
        if states.iter().any(|(s, _)| *s == last) {
            return states;
        }
        assert!(
            Instant::now() < deadline,
            "no ValveStateChanged {last:?}: {states:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Open(OpenDuration = null, TargetLevel = 50) from Closed: Success,
/// ValveStateChanged Transitioning then Open (the all-clusters delegate opens
/// at once, `ValveControlDelegate::HandleOpenValve`), CurrentLevel 50 and no
/// countdown; Close: Transitioning then Closed, CurrentLevel 0. ValveLevel is
/// absent at v1.4.2.0 and the current level on master ([`legacy_valve_server`]).
#[tokio::test]
async fn valve_open_and_close_emit_state_changes() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use valve::{attribute_id as a, ValveStateEnum as S};
    let cl = valve::CLUSTER_ID;
    let legacy = legacy_valve_server(&cfg);
    let level = |l: u8| (!legacy).then_some(l);
    close_valve(&node).await;
    let base = latest_event_number(&node, EP, cl, valve::event_id::VALVE_STATE_CHANGED)
        .await
        .unwrap();
    let open = valve::encode_open(Some(Nullable::Null), Some(50));
    assert_eq!(
        status_of(&node, cl, valve::command_id::OPEN, open).await,
        ImStatus::Success
    );
    assert_eq!(
        valve_states_until(&node, base, S::Open).await,
        [(S::Transitioning, level(0)), (S::Open, level(50))]
    );
    assert_eq!(valve_state(&node).await, Nullable::Value(S::Open));
    let attrs = read_cluster_attributes(&node, EP, cl).await.unwrap();
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        valve::decode_current_level(tlv(a::CURRENT_LEVEL)).unwrap(),
        Nullable::Value(50)
    );
    assert_eq!(
        valve::decode_open_duration(tlv(a::OPEN_DURATION)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        valve::decode_remaining_duration(tlv(a::REMAINING_DURATION)).unwrap(),
        Nullable::Null
    );

    let base = latest_event_number(&node, EP, cl, valve::event_id::VALVE_STATE_CHANGED)
        .await
        .unwrap();
    close_valve(&node).await;
    assert_eq!(
        valve_states_until(&node, base, S::Closed).await,
        [(S::Transitioning, level(50)), (S::Closed, level(0))]
    );
    let back = read_tlv(&node, cl, a::CURRENT_LEVEL).await;
    assert_eq!(
        valve::decode_current_level(&back).unwrap(),
        Nullable::Value(0)
    );
}

/// Open(OpenDuration = 2 s): OpenDuration 2 and a RemainingDuration of at
/// most 2 while open, then the server's 1 s tick closes it by itself
/// (`HandleUpdateRemainingDurationInternal` / `startRemainingDurationTick`),
/// leaving both durations null. Polled, never slept.
#[tokio::test]
async fn valve_open_duration_counts_down_and_closes() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use valve::{attribute_id as a, ValveStateEnum as S};
    let cl = valve::CLUSTER_ID;
    close_valve(&node).await;
    let open = valve::encode_open(Some(Nullable::Value(2)), None);
    assert_eq!(
        status_of(&node, cl, valve::command_id::OPEN, open).await,
        ImStatus::Success
    );
    let attrs = read_cluster_attributes(&node, EP, cl).await.unwrap();
    assert_eq!(
        valve::decode_open_duration(attribute_tlv(&attrs, a::OPEN_DURATION)).unwrap(),
        Nullable::Value(2)
    );
    let remaining =
        valve::decode_remaining_duration(attribute_tlv(&attrs, a::REMAINING_DURATION)).unwrap();
    assert!(
        matches!(remaining, Nullable::Value(r) if r <= 2),
        "RemainingDuration {remaining:?}"
    );
    wait_for_valve_state(&node, S::Closed, Duration::from_secs(10)).await;
    let attrs = read_cluster_attributes(&node, EP, cl).await.unwrap();
    assert_eq!(
        valve::decode_open_duration(attribute_tlv(&attrs, a::OPEN_DURATION)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        valve::decode_remaining_duration(attribute_tlv(&attrs, a::REMAINING_DURATION)).unwrap(),
        Nullable::Null
    );
}

/// Open(TargetLevel = 0) is ConstraintError at both refs (TargetLevel has a
/// minimum of 1) and leaves the valve Closed with no fault. v1.4.2.0 also
/// emits ValveFault{GeneralFault} before it answers (read once after the
/// answer: it is already logged); master emits nothing.
#[tokio::test]
async fn valve_refuses_a_zero_target_level() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    let cl = valve::CLUSTER_ID;
    close_valve(&node).await;
    let base = latest_event_number(&node, EP, cl, valve::event_id::VALVE_FAULT)
        .await
        .unwrap();
    let open = valve::encode_open(None, Some(0));
    assert_eq!(
        status_of(&node, cl, valve::command_id::OPEN, open).await,
        CONSTRAINT_ERROR
    );
    let mut faults = read_event_items(
        &node,
        EventPath::concrete(EP, cl, valve::event_id::VALVE_FAULT),
    )
    .await
    .unwrap();
    faults.retain(|i| base.is_none_or(|b| i.event_number > b));
    if legacy_valve_server(&cfg) {
        assert_eq!(faults.len(), 1, "{faults:?}");
        let f = valve::ValveFaultEvent::decode(&payload_tlv(&faults[0].value)).unwrap();
        assert_eq!(f.valve_fault, valve::ValveFaultBitmap::GENERAL_FAULT);
    } else {
        assert_eq!(
            faults.len(),
            0,
            "master emits no fault for a refused Open: {faults:?}"
        );
    }
    assert_eq!(
        valve_state(&node).await,
        Nullable::Value(valve::ValveStateEnum::Closed)
    );
    let fault = read_tlv(&node, cl, valve::attribute_id::VALVE_FAULT).await;
    assert_eq!(
        valve::decode_valve_fault(&fault).unwrap(),
        valve::ValveFaultBitmap::empty()
    );
}
