// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! The appliance clusters on a live all-clusters-app (endpoint 1): every
//! attribute of each hosted appliance cluster decoded with the generated
//! decoder, the exact attribute ids served, one or more safe commands per
//! cluster, attribute writes, and the OperationalState events.
//!
//! Hosted here, at master and at v1.4.2.0 alike (both `all-clusters-app.matter`
//! files): OperationalState, OvenCavityOperationalState, RvcOperationalState,
//! TemperatureControl (TL feature), LaundryWasherControls, LaundryDryerControls.
//! Not served by all-clusters at either ref: MicrowaveOvenControl
//! (microwave-oven-app, `clusters_microwave_oven.rs`) and ServiceArea (rvc-app,
//! `clusters_rvc.rs`).
//!
//! Every chip source cited below is the same at v1.4.2.0 (the nightly's pin)
//! as at master 5cd2917a, apart from formatting: the delegates under
//! examples/all-clusters-app/all-clusters-common, the OperationalState server
//! (`OperationalStateCluster.cpp`, `operational-state-server.cpp` at
//! v1.4.2.0), the laundry servers, and the `OperationalStateChange` pipe
//! command (AllClustersCommandDelegate.cpp, both refs).
//!
//! Each test restores what it changes. The OperationalState and
//! OvenCavityOperationalState tests set the state they start from with a
//! `Stop`, which ends in Stopped from any state. The RvcOperationalState test
//! cannot (RVC disallows Stop): it relies on the app booting Stopped and on
//! no other test moving the RVC state.

use integration_tests::dut::DutConfig;
use integration_tests::events::{
    latest_event_number, payload_tlv, read_event_items, send_app_pipe, wait_for_event_after,
};
use integration_tests::sweep::{
    assert_exact_attribute_ids, attribute_tlv, decode_every_attribute, invoke_for_status,
    newer_than_codegen, ok, read_cluster_attributes, write_attribute,
};
use integration_tests::{operational_command, sweep_operational_state};
use matter_clusters::clusters::{
    laundry_dryer_controls, laundry_washer_controls, operational_state,
    oven_cavity_operational_state, rvc_operational_state, temperature_control,
};
use matter_clusters::types::Nullable;
use matter_controller::{AttributePath, CommandPath, EventPath, ImStatus, MatterController, Node};

/// Every appliance cluster all-clusters serves lives on endpoint 1.
const EP: u16 = 1;

/// IM status codes the servers answer with (Matter Core spec 8.10.1).
const INVALID_COMMAND: ImStatus = ImStatus::Failure(0x85);
const CONSTRAINT_ERROR: ImStatus = ImStatus::Failure(0x87);
const INVALID_IN_STATE: ImStatus = ImStatus::Failure(0xCB);

/// The attribute ids all-clusters serves for each OperationalState-family
/// cluster (all-clusters-app.matter, both refs): all six.
const OP_STATE_IDS: [u32; 6] = [0x0000, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005];

/// The DUT config, controller and node id, or `None` (skip) unless the DUT is
/// all-clusters.
async fn connect_all_clusters() -> Option<(DutConfig, MatterController, u64)> {
    let cfg = DutConfig::from_env()?;
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: appliance sweep needs the all-clusters DUT (`just integration`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((cfg, controller, node_id))
}

/// OperationalState: the attributes the generic delegate serves after a Stop
/// (operational-state-delegate-impl.h: four states without labels, no phases,
/// a null countdown), then Stop / Pause / Resume from Stopped
/// (OperationalStateCluster.cpp: Stop is a no-op answering NoError; Pause and
/// Resume are CommandInvalidInState without calling the delegate).
#[tokio::test]
async fn operational_state_decodes_and_answers_commands_when_stopped() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use operational_state::{attribute_id as op, ErrorStateEnum, OperationalStateEnum};
    let first = operational_command!(&node, EP, operational_state, STOP, encode_stop);
    assert_eq!(first.error_state_id, ErrorStateEnum::NoError);

    let attrs = sweep_operational_state!(&node, EP, operational_state, &OP_STATE_IDS);
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        operational_state::decode_phase_list(tlv(op::PHASE_LIST)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        operational_state::decode_current_phase(tlv(op::CURRENT_PHASE)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        operational_state::decode_countdown_time(tlv(op::COUNTDOWN_TIME)).unwrap(),
        Nullable::Null
    );
    let states: Vec<_> =
        operational_state::decode_operational_state_list(tlv(op::OPERATIONAL_STATE_LIST))
            .unwrap()
            .into_iter()
            .map(|s| (s.operational_state_id, s.operational_state_label))
            .collect();
    assert_eq!(
        states,
        [
            (OperationalStateEnum::Stopped, None),
            (OperationalStateEnum::Running, None),
            (OperationalStateEnum::Paused, None),
            (OperationalStateEnum::Error, None),
        ]
    );
    assert_eq!(
        operational_state::decode_operational_state(tlv(op::OPERATIONAL_STATE)).unwrap(),
        OperationalStateEnum::Stopped
    );
    let error = operational_state::decode_operational_error(tlv(op::OPERATIONAL_ERROR)).unwrap();
    assert_eq!(
        (
            error.error_state_id,
            error.error_state_label,
            error.error_state_details
        ),
        (ErrorStateEnum::NoError, None, None)
    );

    let stop = operational_command!(&node, EP, operational_state, STOP, encode_stop);
    let pause = operational_command!(&node, EP, operational_state, PAUSE, encode_pause);
    let resume = operational_command!(&node, EP, operational_state, RESUME, encode_resume);
    assert_eq!(stop.error_state_id, ErrorStateEnum::NoError);
    assert_eq!(pause.error_state_id, ErrorStateEnum::CommandInvalidInState);
    assert_eq!(resume.error_state_id, ErrorStateEnum::CommandInvalidInState);
    assert_eq!(pause.error_state_label, None, "chip sends only the id");
}

/// OperationalState events: the app pipe's `OnFault` (Param 1) raises
/// OperationalError with UnableToStartOrResume and moves the cluster to Error
/// (OperationalStateCluster.cpp OnOperationalErrorDetected); a Stop clears it
/// and the generic delegate emits OperationCompletion with the error code it
/// reads after clearing (NoError) and its run/pause counters, both 0 because
/// nothing in the suite starts this cluster (operational-state-delegate-impl.cpp
/// HandleStopStateCallback).
#[tokio::test]
async fn operational_state_fault_and_stop_emit_their_events() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use operational_state::{
        attribute_id as op, event_id, ErrorStateEnum, OperationCompletionEvent,
        OperationalErrorEvent, OperationalStateEnum,
    };
    let cl = operational_state::CLUSTER_ID;
    operational_command!(&node, EP, operational_state, STOP, encode_stop);

    let error_base = latest_event_number(&node, EP, cl, event_id::OPERATIONAL_ERROR)
        .await
        .unwrap();
    send_app_pipe(
        &cfg,
        r#"{"Name": "OperationalStateChange", "Device": "Generic", "Operation": "OnFault", "Param": 1}"#,
    )
    .await
    .expect("app pipe");
    let items = wait_for_event_after(&node, EP, cl, event_id::OPERATIONAL_ERROR, error_base)
        .await
        .unwrap();
    let last = items.iter().max_by_key(|i| i.event_number).unwrap();
    let e =
        OperationalErrorEvent::decode(&payload_tlv(&last.value)).expect("OperationalError decodes");
    assert_eq!(
        e.error_state.error_state_id,
        ErrorStateEnum::UnableToStartOrResume
    );
    let attrs = read_cluster_attributes(&node, EP, cl).await.unwrap();
    assert_eq!(
        operational_state::decode_operational_state(attribute_tlv(&attrs, op::OPERATIONAL_STATE))
            .unwrap(),
        OperationalStateEnum::Error
    );
    assert_eq!(
        operational_state::decode_operational_error(attribute_tlv(&attrs, op::OPERATIONAL_ERROR))
            .unwrap()
            .error_state_id,
        ErrorStateEnum::UnableToStartOrResume
    );

    let completion_base = latest_event_number(&node, EP, cl, event_id::OPERATION_COMPLETION)
        .await
        .unwrap();
    let stop = operational_command!(&node, EP, operational_state, STOP, encode_stop);
    assert_eq!(stop.error_state_id, ErrorStateEnum::NoError);
    let items = wait_for_event_after(
        &node,
        EP,
        cl,
        event_id::OPERATION_COMPLETION,
        completion_base,
    )
    .await
    .unwrap();
    let last = items.iter().max_by_key(|i| i.event_number).unwrap();
    let c = OperationCompletionEvent::decode(&payload_tlv(&last.value))
        .expect("OperationCompletion decodes");
    assert_eq!(
        (
            c.completion_error_code,
            c.total_operational_time,
            c.paused_time
        ),
        (0, Some(Nullable::Value(0)), Some(Nullable::Value(0)))
    );
    let attrs = read_cluster_attributes(&node, EP, cl).await.unwrap();
    assert_eq!(
        operational_state::decode_operational_state(attribute_tlv(&attrs, op::OPERATIONAL_STATE))
            .unwrap(),
        OperationalStateEnum::Stopped,
        "Stop restored the cluster"
    );
    assert_eq!(
        operational_state::decode_operational_error(attribute_tlv(&attrs, op::OPERATIONAL_ERROR))
            .unwrap()
            .error_state_id,
        ErrorStateEnum::NoError,
        "SetOperationalState clears the error"
    );
}

/// OvenCavityOperationalState: attributes after a Stop (oven-operational-state-
/// delegate.h: four states, no phases, a null countdown), Stop from Stopped,
/// and an OperationalError for a manufacturer error id (pipe `OnFault` Param
/// 128 on the Oven device). chip's GenericOperationalError carries no label,
/// although 1.4 conformance asks for one on 0x80..=0xBF: ErrorStateLabel's
/// conformance is an expression, so it is optional (only an unconditional M is
/// mandatory). A Stop
/// restores Stopped (the oven delegate emits no OperationCompletion).
#[tokio::test]
async fn oven_cavity_operational_state_decodes_and_reports_a_manufacturer_error() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use oven::{
        attribute_id as op, event_id, ErrorStateEnum, OperationalErrorEvent, OperationalStateEnum,
    };
    use oven_cavity_operational_state as oven;
    operational_command!(&node, EP, oven, STOP, encode_stop);
    let attrs = sweep_operational_state!(&node, EP, oven, &OP_STATE_IDS);
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        oven::decode_phase_list(tlv(op::PHASE_LIST)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        oven::decode_countdown_time(tlv(op::COUNTDOWN_TIME)).unwrap(),
        Nullable::Null
    );
    let states: Vec<_> = oven::decode_operational_state_list(tlv(op::OPERATIONAL_STATE_LIST))
        .unwrap()
        .into_iter()
        .map(|s| s.operational_state_id)
        .collect();
    use OperationalStateEnum::{Error, Paused, Running, Stopped};
    assert_eq!(states, [Stopped, Running, Paused, Error]);
    assert_eq!(
        oven::decode_operational_state(tlv(op::OPERATIONAL_STATE)).unwrap(),
        Stopped
    );
    let stop = operational_command!(&node, EP, oven, STOP, encode_stop);
    assert_eq!(stop.error_state_id, ErrorStateEnum::NoError);

    let cl = oven::CLUSTER_ID;
    let base = latest_event_number(&node, EP, cl, event_id::OPERATIONAL_ERROR)
        .await
        .unwrap();
    send_app_pipe(
        &cfg,
        r#"{"Name": "OperationalStateChange", "Device": "Oven", "Operation": "OnFault", "Param": 128}"#,
    )
    .await
    .expect("app pipe");
    let items = wait_for_event_after(&node, EP, cl, event_id::OPERATIONAL_ERROR, base)
        .await
        .unwrap();
    let last = items.iter().max_by_key(|i| i.event_number).unwrap();
    let e =
        OperationalErrorEvent::decode(&payload_tlv(&last.value)).expect("OperationalError decodes");
    assert_eq!(
        (
            e.error_state.error_state_id,
            e.error_state.error_state_label
        ),
        (ErrorStateEnum::Unknown(0x80), None)
    );
    let after_fault = read_cluster_attributes(&node, EP, cl).await.unwrap();
    assert_eq!(
        oven::decode_operational_state(attribute_tlv(&after_fault, op::OPERATIONAL_STATE)).unwrap(),
        Error
    );
    assert_eq!(
        oven::decode_operational_error(attribute_tlv(&after_fault, op::OPERATIONAL_ERROR))
            .unwrap()
            .error_state_id,
        ErrorStateEnum::Unknown(0x80)
    );
    let restore = operational_command!(&node, EP, oven, STOP, encode_stop);
    assert_eq!(restore.error_state_id, ErrorStateEnum::NoError);
    let restored = read_cluster_attributes(&node, EP, cl).await.unwrap();
    assert_eq!(
        oven::decode_operational_state(attribute_tlv(&restored, op::OPERATIONAL_STATE)).unwrap(),
        Stopped
    );
    assert_eq!(
        oven::decode_operational_error(attribute_tlv(&restored, op::OPERATIONAL_ERROR))
            .unwrap()
            .error_state_id,
        ErrorStateEnum::NoError
    );
}

/// RvcOperationalState on all-clusters: attributes (rvc-operational-state-
/// delegate-impl.h: seven states, the three RVC ones included), and Pause /
/// Resume from Stopped, both CommandInvalidInState without a state change
/// (OperationalStateCluster.cpp). GoHome is not sent: it moves the robot to
/// SeekingCharger, and with Stop and Start disallowed nothing on the wire
/// brings it back. all-clusters has no RVC pipe command, so its events are
/// decoded if present, not stimulated (rvc-app stimulates them,
/// `clusters_rvc.rs`).
#[tokio::test]
async fn rvc_operational_state_decodes_and_refuses_pause_and_resume_when_stopped() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use rvc::{attribute_id as op, ErrorStateEnum, OperationalStateEnum as S};
    use rvc_operational_state as rvc;
    let attrs = sweep_operational_state!(&node, EP, rvc, &OP_STATE_IDS);
    let states: Vec<_> =
        rvc::decode_operational_state_list(attribute_tlv(&attrs, op::OPERATIONAL_STATE_LIST))
            .unwrap()
            .into_iter()
            .map(|s| s.operational_state_id)
            .collect();
    assert_eq!(
        states,
        [
            S::Stopped,
            S::Running,
            S::Paused,
            S::Error,
            S::SeekingCharger,
            S::Charging,
            S::Docked
        ]
    );
    assert_eq!(
        rvc::decode_operational_state(attribute_tlv(&attrs, op::OPERATIONAL_STATE)).unwrap(),
        S::Stopped
    );
    let pause = operational_command!(&node, EP, rvc, PAUSE, encode_pause);
    let resume = operational_command!(&node, EP, rvc, RESUME, encode_resume);
    assert_eq!(pause.error_state_id, ErrorStateEnum::CommandInvalidInState);
    assert_eq!(resume.error_state_id, ErrorStateEnum::CommandInvalidInState);
    let after = read_cluster_attributes(&node, EP, rvc::CLUSTER_ID)
        .await
        .unwrap();
    assert_eq!(
        rvc::decode_operational_state(attribute_tlv(&after, op::OPERATIONAL_STATE)).unwrap(),
        S::Stopped
    );
    for event in [
        rvc::event_id::OPERATIONAL_ERROR,
        rvc::event_id::OPERATION_COMPLETION,
    ] {
        let items = read_event_items(&node, EventPath::concrete(EP, rvc::CLUSTER_ID, event))
            .await
            .unwrap();
        for item in &items {
            let tlv = payload_tlv(&item.value);
            let decoded = if event == rvc::event_id::OPERATIONAL_ERROR {
                rvc::OperationalErrorEvent::decode(&tlv).map(|_| ())
            } else {
                rvc::OperationCompletionEvent::decode(&tlv).map(|_| ())
            };
            decoded
                .unwrap_or_else(|e| panic!("RVC event {event:#04x} #{}: {e}", item.event_number));
        }
        eprintln!(
            "[sweep] RvcOperationalState event {event:#04x}: {} present, all decoded",
            items.len()
        );
    }
}

/// TemperatureControl (TL feature only, featureMap 2): SelectedTemperatureLevel
/// and SupportedTemperatureLevels are the only attributes served; the levels
/// are static-supported-temperature-levels.cpp's. SetTemperature with a level
/// in range succeeds and is read back; one past the end is ConstraintError;
/// none at all is InvalidCommand (TemperatureControlCluster.cpp
/// HandleSetTemperature; temperature-control-server.cpp at v1.4.2.0). The
/// original level is restored.
#[tokio::test]
async fn temperature_control_decodes_and_set_temperature_selects_a_level() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use temperature_control::attribute_id as tc;
    let attrs = decode_every_attribute(
        &node,
        EP,
        temperature_control::CLUSTER_ID,
        |id, t| match id {
            tc::SELECTED_TEMPERATURE_LEVEL => {
                ok(temperature_control::decode_selected_temperature_level(t))
            }
            tc::SUPPORTED_TEMPERATURE_LEVELS => {
                ok(temperature_control::decode_supported_temperature_levels(t))
            }
            other => newer_than_codegen("TemperatureControl", other),
        },
    )
    .await;
    assert_exact_attribute_ids(
        "TemperatureControl",
        &attrs,
        &[
            tc::SELECTED_TEMPERATURE_LEVEL,
            tc::SUPPORTED_TEMPERATURE_LEVELS,
        ],
    );
    assert_eq!(
        temperature_control::decode_supported_temperature_levels(attribute_tlv(
            &attrs,
            tc::SUPPORTED_TEMPERATURE_LEVELS
        ))
        .unwrap(),
        ["Hot", "Warm", "Freezing"]
    );
    let before = temperature_control::decode_selected_temperature_level(attribute_tlv(
        &attrs,
        tc::SELECTED_TEMPERATURE_LEVEL,
    ))
    .unwrap();
    let target = if before == 1 { 2 } else { 1 };
    let set = |level: Option<u8>| set_temperature(&node, level);
    assert_eq!(set(Some(target)).await, ImStatus::Success);
    assert_eq!(selected_level(&node).await, target);
    assert_eq!(set(Some(3)).await, CONSTRAINT_ERROR, "three levels: 0..=2");
    assert_eq!(
        set(None).await,
        INVALID_COMMAND,
        "TL needs TargetTemperatureLevel"
    );
    assert_eq!(
        selected_level(&node).await,
        target,
        "refused commands change nothing"
    );
    assert_eq!(set(Some(before)).await, ImStatus::Success);
    assert_eq!(selected_level(&node).await, before, "level restored");
}

/// `TemperatureControl.SetTemperature(None, level)` on [`EP`] and its status.
async fn set_temperature(node: &Node, level: Option<u8>) -> ImStatus {
    invoke_for_status(
        node,
        CommandPath {
            endpoint: EP,
            cluster: temperature_control::CLUSTER_ID,
            command: temperature_control::command_id::SET_TEMPERATURE,
        },
        temperature_control::encode_set_temperature(None, level),
    )
    .await
    .unwrap()
}

/// `TemperatureControl.SelectedTemperatureLevel`, read fresh.
async fn selected_level(node: &Node) -> u8 {
    let attrs = read_cluster_attributes(node, EP, temperature_control::CLUSTER_ID)
        .await
        .unwrap();
    temperature_control::decode_selected_temperature_level(attribute_tlv(
        &attrs,
        temperature_control::attribute_id::SELECTED_TEMPERATURE_LEVEL,
    ))
    .unwrap()
}

/// A path on [`EP`].
fn path(cluster: u32, attribute: u32) -> AttributePath {
    AttributePath {
        endpoint: EP,
        cluster,
        attribute,
    }
}

/// LaundryWasherControls (SPIN and RINSE, featureMap 3): the four attributes,
/// laundry-washer-controls-delegate-impl.cpp's spin speeds and rinses, and the
/// write validation in laundry-washer-controls-server.cpp
/// (MatterLaundryWasherControlsClusterServerPreAttributeChangedCallback): a
/// spin speed index past the list is ConstraintError, a rinse count outside
/// SupportedRinses is InvalidInState. Values are restored where chip accepts
/// them: the RAM default NumberOfRinses (no default in
/// all-clusters-app.matter, so 0, None) is not in SupportedRinses, so the
/// restore is refused by the same check and NumberOfRinses stays Extra.
#[tokio::test]
async fn laundry_washer_controls_decode_and_validate_writes() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use laundry_washer_controls::{attribute_id as lw, NumberOfRinsesEnum as Rinses};
    let attrs = sweep_laundry_washer_controls(&node).await;
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        laundry_washer_controls::decode_spin_speeds(tlv(lw::SPIN_SPEEDS)).unwrap(),
        ["Off", "Low", "Medium", "High"]
    );
    let supported =
        laundry_washer_controls::decode_supported_rinses(tlv(lw::SUPPORTED_RINSES)).unwrap();
    assert_eq!(supported, [Rinses::Normal, Rinses::Extra]);
    let spin_before =
        laundry_washer_controls::decode_spin_speed_current(tlv(lw::SPIN_SPEED_CURRENT)).unwrap();
    let rinses_before =
        laundry_washer_controls::decode_number_of_rinses(tlv(lw::NUMBER_OF_RINSES)).unwrap();

    let cl = laundry_washer_controls::CLUSTER_ID;
    let spin = |v| {
        write_attribute(
            &node,
            path(cl, lw::SPIN_SPEED_CURRENT),
            laundry_washer_controls::encode_spin_speed_current(v),
        )
    };
    let rinses = |v| {
        write_attribute(
            &node,
            path(cl, lw::NUMBER_OF_RINSES),
            laundry_washer_controls::encode_number_of_rinses(v),
        )
    };
    assert_eq!(spin(Nullable::Value(2)).await.unwrap(), ImStatus::Success);
    assert_eq!(
        spin(Nullable::Value(4)).await.unwrap(),
        CONSTRAINT_ERROR,
        "four speeds: 0..=3"
    );
    assert_eq!(rinses(Rinses::Extra).await.unwrap(), ImStatus::Success);
    assert_eq!(
        rinses(Rinses::Max).await.unwrap(),
        INVALID_IN_STATE,
        "Max is not supported"
    );
    let after = read_cluster_attributes(&node, EP, cl).await.unwrap();
    assert_eq!(
        laundry_washer_controls::decode_spin_speed_current(attribute_tlv(
            &after,
            lw::SPIN_SPEED_CURRENT
        ))
        .unwrap(),
        Nullable::Value(2)
    );
    assert_eq!(
        laundry_washer_controls::decode_number_of_rinses(attribute_tlv(
            &after,
            lw::NUMBER_OF_RINSES
        ))
        .unwrap(),
        Rinses::Extra
    );
    assert_eq!(
        spin(Nullable::Null).await.unwrap(),
        ImStatus::Success,
        "SpinSpeedCurrent is nullable"
    );
    assert_eq!(spin(spin_before).await.unwrap(), ImStatus::Success);
    let restore = rinses(rinses_before).await.unwrap();
    let rinses_now = if supported.contains(&rinses_before) {
        assert_eq!(restore, ImStatus::Success);
        rinses_before
    } else {
        assert_eq!(restore, INVALID_IN_STATE, "the same validation refuses it");
        eprintln!(
            "[sweep] NumberOfRinses boot value {rinses_before:?} is not writable; left at Extra"
        );
        Rinses::Extra
    };
    let restored = read_cluster_attributes(&node, EP, cl).await.unwrap();
    let read = |id| attribute_tlv(&restored, id);
    assert_eq!(
        laundry_washer_controls::decode_spin_speed_current(read(lw::SPIN_SPEED_CURRENT)).unwrap(),
        spin_before
    );
    assert_eq!(
        laundry_washer_controls::decode_number_of_rinses(read(lw::NUMBER_OF_RINSES)).unwrap(),
        rinses_now
    );
}

/// LaundryWasherControls on [`EP`]: every attribute decoded, exactly the four
/// ids served.
async fn sweep_laundry_washer_controls(node: &Node) -> Vec<(u32, Vec<u8>)> {
    use laundry_washer_controls::attribute_id as lw;
    let attrs =
        decode_every_attribute(
            node,
            EP,
            laundry_washer_controls::CLUSTER_ID,
            |id, t| match id {
                lw::SPIN_SPEEDS => ok(laundry_washer_controls::decode_spin_speeds(t)),
                lw::SPIN_SPEED_CURRENT => ok(laundry_washer_controls::decode_spin_speed_current(t)),
                lw::NUMBER_OF_RINSES => ok(laundry_washer_controls::decode_number_of_rinses(t)),
                lw::SUPPORTED_RINSES => ok(laundry_washer_controls::decode_supported_rinses(t)),
                other => newer_than_codegen("LaundryWasherControls", other),
            },
        )
        .await;
    assert_exact_attribute_ids(
        "LaundryWasherControls",
        &attrs,
        &[
            lw::SPIN_SPEEDS,
            lw::SPIN_SPEED_CURRENT,
            lw::NUMBER_OF_RINSES,
            lw::SUPPORTED_RINSES,
        ],
    );
    attrs
}

/// LaundryDryerControls: both attributes, laundry-dryer-controls-delegate-
/// impl.cpp's dryness levels, and the write validation in
/// laundry-dryer-controls-server.cpp: a level outside the supported list is
/// ConstraintError, null is accepted. The original value is restored.
#[tokio::test]
async fn laundry_dryer_controls_decode_and_validate_writes() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use laundry_dryer_controls::{attribute_id as ld, DrynessLevelEnum as Dryness};
    let attrs =
        decode_every_attribute(
            &node,
            EP,
            laundry_dryer_controls::CLUSTER_ID,
            |id, t| match id {
                ld::SUPPORTED_DRYNESS_LEVELS => {
                    ok(laundry_dryer_controls::decode_supported_dryness_levels(t))
                }
                ld::SELECTED_DRYNESS_LEVEL => {
                    ok(laundry_dryer_controls::decode_selected_dryness_level(t))
                }
                other => newer_than_codegen("LaundryDryerControls", other),
            },
        )
        .await;
    assert_exact_attribute_ids(
        "LaundryDryerControls",
        &attrs,
        &[ld::SUPPORTED_DRYNESS_LEVELS, ld::SELECTED_DRYNESS_LEVEL],
    );
    assert_eq!(
        laundry_dryer_controls::decode_supported_dryness_levels(attribute_tlv(
            &attrs,
            ld::SUPPORTED_DRYNESS_LEVELS
        ))
        .unwrap(),
        [Dryness::Low, Dryness::Normal, Dryness::Max]
    );
    let before = laundry_dryer_controls::decode_selected_dryness_level(attribute_tlv(
        &attrs,
        ld::SELECTED_DRYNESS_LEVEL,
    ))
    .unwrap();
    let cl = laundry_dryer_controls::CLUSTER_ID;
    let write = |v| {
        write_attribute(
            &node,
            path(cl, ld::SELECTED_DRYNESS_LEVEL),
            laundry_dryer_controls::encode_selected_dryness_level(v),
        )
    };
    assert_eq!(
        write(Nullable::Value(Dryness::Normal)).await.unwrap(),
        ImStatus::Success
    );
    assert_eq!(
        write(Nullable::Value(Dryness::Extra)).await.unwrap(),
        CONSTRAINT_ERROR,
        "Extra is not supported"
    );
    let after = read_cluster_attributes(&node, EP, cl).await.unwrap();
    assert_eq!(
        laundry_dryer_controls::decode_selected_dryness_level(attribute_tlv(
            &after,
            ld::SELECTED_DRYNESS_LEVEL
        ))
        .unwrap(),
        Nullable::Value(Dryness::Normal)
    );
    assert_eq!(write(Nullable::Null).await.unwrap(), ImStatus::Success);
    assert_eq!(write(before).await.unwrap(), ImStatus::Success, "restored");
    let restored = read_cluster_attributes(&node, EP, cl).await.unwrap();
    assert_eq!(
        laundry_dryer_controls::decode_selected_dryness_level(attribute_tlv(
            &restored,
            ld::SELECTED_DRYNESS_LEVEL
        ))
        .unwrap(),
        before
    );
}
