// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! The RVC clusters on a live rvc-app (`just integration-rvc`), endpoint 1.
//!
//! Live on a Linux arm64 Pi (chip master 5cd2917a; 8 passed): rvc-app needs
//! Rosetta 2 to build on Apple silicon, so it runs there, not on the Mac. Every
//! expected value below is also read from chip source.
//!
//! The mode clusters: RvcRunMode and RvcCleanMode. rvc-app's modes come from
//! examples/rvc-app/rvc-common/include/rvc-mode-delegates.h; its mode-change
//! rules from rvc-common/src/rvc-device.cpp (`HandleRvcRunChangeToMode`,
//! `HandleRvcCleanChangeToMode`). Line numbers cited below are chip master
//! 5cd2917a; v1.4.2.0's rvc-mode-delegates.h is byte-identical and its two
//! change handlers differ only in whitespace.
//!
//! The appliance clusters: RvcOperationalState (and its OperationalError /
//! OperationCompletion events) and ServiceArea, which only rvc-app serves.
//! Their behaviour comes from rvc-common/src/rvc-device.cpp, the service-area
//! delegate (rvc-service-area-delegate.cpp) and chip's servers
//! (OperationalStateCluster.cpp, service-area-server.cpp). At v1.4.2.0 every
//! file cited is the same apart from formatting, with two exceptions: the
//! operational-state server is operational-state-server.cpp (same logic),
//! and rvc-device.cpp's GoHome callback accepts GoHome only from Stopped
//! (master also from Paused and Running). The GoHome test sends it only from
//! Stopped in Idle, where both refs agree.
//!
//! The tests run single-threaded (`--test-threads=1`, xtask `run_tests`)
//! against one rvc-app booted with a fresh KVS. Each test first sends
//! rvc-app's `Reset` pipe command (`reset_rvc`), so none depends on what an
//! earlier test left behind.

use integration_tests::dut::DutConfig;
use integration_tests::events::{
    latest_event_number, payload_tlv, send_app_pipe, wait_for_event_after,
};
use integration_tests::sweep::{
    assert_exact_attribute_ids, attribute_ids, attribute_tlv, decode_every_attribute,
    invoke_for_response, newer_than_codegen, ok, read_cluster_attributes,
};
use integration_tests::{operational_command, sweep_operational_state};
use matter_clusters::clusters::{
    rvc_clean_mode, rvc_operational_state, rvc_run_mode, service_area,
};
use matter_clusters::error::ClusterError;
use matter_clusters::types::Nullable;
use matter_controller::{CommandPath, MatterController, Node};

/// rvc-app's RVC endpoint (`RVC_ENDPOINT` in examples/rvc-app/linux/main.cpp:25).
const EP: u16 = 1;

/// The DUT config, controller and node id, or `None` (skip) unless the DUT is
/// rvc-app.
async fn connect_rvc() -> Option<(DutConfig, MatterController, u64)> {
    let Some(cfg) = DutConfig::from_env() else {
        eprintln!("skipped: no DUT (set MATTER_INTEGRATION_DUT via `just integration-rvc`)");
        return None;
    };
    if !cfg.is_app("rvc") {
        eprintln!("skipped: RVC test needs the rvc-app DUT (`just integration-rvc`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((cfg, controller, node_id))
}

/// rvc-app's `Reset` pipe command (rvc-device.cpp:490-503
/// `HandleResetMessage`): run mode Idle, operational state Stopped, clean
/// mode Quick, no selected areas, no progress, null CurrentArea and
/// EstimatedEndTime, and the two-map topology re-installed. It does not touch
/// the docked / charging flags, which only the Docked, Charging and
/// ChargerFound pipe commands set (no test sends them). rvc-app only logs a
/// pipe command it does not know (RvcAppCommandDelegate.cpp:140-143), unlike
/// all-clusters, which aborts.
async fn reset_rvc(cfg: &DutConfig) {
    send_app_pipe(cfg, r#"{"Name": "Reset"}"#)
        .await
        .expect("rvc-app Reset over the app pipe");
}

/// Both mode clusters: every attribute decodes, ChangeToMode(CurrentMode)
/// answers Success with no StatusText, and the served attributes and
/// supported modes are exactly rvc-app's.
#[tokio::test]
async fn rvc_mode_clusters_decode_and_change_to_current_mode() {
    let Some((cfg, controller, node_id)) = connect_rvc().await else {
        return;
    };
    reset_rvc(&cfg).await;
    let node = controller.node(node_id);
    integration_tests::sweep_mode_base!(&node, EP, rvc_run_mode);
    integration_tests::sweep_mode_base!(&node, EP, rvc_clean_mode);
    assert_run_modes(&node).await;
    assert_clean_modes(&node).await;
}

/// RvcRunMode: rvc-app.matter:1956-1966 serves exactly SupportedModes and
/// CurrentMode (no OnMode/StartUpMode), and rvc-mode-delegates.h:36-57 lists
/// three modes, one tag each.
async fn assert_run_modes(node: &Node) {
    use rvc_run_mode::attribute_id::{CURRENT_MODE, SUPPORTED_MODES};
    use rvc_run_mode::ModeTag as Run;
    let attrs = read_cluster_attributes(node, EP, rvc_run_mode::CLUSTER_ID)
        .await
        .unwrap();
    assert_eq!(attribute_ids(&attrs), [SUPPORTED_MODES, CURRENT_MODE]);
    let run = rvc_run_mode::decode_supported_modes(attribute_tlv(&attrs, SUPPORTED_MODES)).unwrap();
    let got: Vec<(u8, &str, Vec<Run>)> = run
        .iter()
        .map(|m| {
            (
                m.mode,
                m.label.as_str(),
                m.mode_tags.iter().map(|t| t.value).collect(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (0, "Idle", vec![Run::Idle]),
            (1, "Cleaning", vec![Run::Cleaning]),
            (2, "Mapping", vec![Run::Mapping]),
        ]
    );
}

/// RvcCleanMode: rvc-app.matter:1968-1978 serves exactly SupportedModes and
/// CurrentMode, and rvc-mode-delegates.h:85-132 lists six modes whose tags mix
/// RvcCleanMode's own (Vacuum, Mop, DeepClean, VacuumThenMop) with the common
/// ModeBase ones (Quick, Auto, Quiet), in rvc-app's order.
async fn assert_clean_modes(node: &Node) {
    use rvc_clean_mode::attribute_id::{CURRENT_MODE, SUPPORTED_MODES};
    use rvc_clean_mode::ModeTag as Clean;
    let attrs = read_cluster_attributes(node, EP, rvc_clean_mode::CLUSTER_ID)
        .await
        .unwrap();
    assert_eq!(attribute_ids(&attrs), [SUPPORTED_MODES, CURRENT_MODE]);
    let clean =
        rvc_clean_mode::decode_supported_modes(attribute_tlv(&attrs, SUPPORTED_MODES)).unwrap();
    let got: Vec<(u8, &str, Vec<Clean>)> = clean
        .iter()
        .map(|m| {
            (
                m.mode,
                m.label.as_str(),
                m.mode_tags.iter().map(|t| t.value).collect(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (0, "Quick", vec![Clean::Vacuum, Clean::Quick]),
            (1, "Auto", vec![Clean::Auto, Clean::Vacuum]),
            (
                2,
                "Deep Clean",
                vec![Clean::Mop, Clean::DeepClean, Clean::Vacuum]
            ),
            (3, "Quiet", vec![Clean::Quiet, Clean::Vacuum]),
            (4, "Max Vac", vec![Clean::Vacuum, Clean::DeepClean]),
            (
                5,
                "Vacuum Then Mop",
                vec![Clean::Vacuum, Clean::Mop, Clean::VacuumThenMop]
            ),
        ]
    );
}

/// `CurrentMode` of a mode cluster on [`EP`], read fresh.
async fn current_mode(
    node: &Node,
    cluster: u32,
    decode: fn(&[u8]) -> Result<u8, ClusterError>,
) -> u8 {
    // CurrentMode is attribute 0x0001 in every ModeBase derivative.
    let attrs = read_cluster_attributes(node, EP, cluster).await.unwrap();
    decode(attribute_tlv(
        &attrs,
        rvc_run_mode::attribute_id::CURRENT_MODE,
    ))
    .unwrap()
}

/// `RvcRunMode.ChangeToMode(mode)` and its decoded response (the invoke must
/// answer with ChangeToModeResponse, not a bare status).
async fn change_run_mode(node: &Node, mode: u8) -> rvc_run_mode::ChangeToModeResponse {
    let resp = invoke_for_response(
        node,
        CommandPath {
            endpoint: EP,
            cluster: rvc_run_mode::CLUSTER_ID,
            command: rvc_run_mode::command_id::CHANGE_TO_MODE,
        },
        rvc_run_mode::encode_change_to_mode(mode),
        rvc_run_mode::command_id::CHANGE_TO_MODE_RESPONSE,
    )
    .await
    .unwrap_or_else(|e| panic!("RvcRunMode.ChangeToMode({mode}): {e:#}"));
    rvc_run_mode::ChangeToModeResponse::decode(&resp).expect("ChangeToModeResponse decodes")
}

/// `RvcCleanMode.ChangeToMode(mode)` and its decoded response.
async fn change_clean_mode(node: &Node, mode: u8) -> rvc_clean_mode::ChangeToModeResponse {
    let resp = invoke_for_response(
        node,
        CommandPath {
            endpoint: EP,
            cluster: rvc_clean_mode::CLUSTER_ID,
            command: rvc_clean_mode::command_id::CHANGE_TO_MODE,
        },
        rvc_clean_mode::encode_change_to_mode(mode),
        rvc_clean_mode::command_id::CHANGE_TO_MODE_RESPONSE,
    )
    .await
    .unwrap_or_else(|e| panic!("RvcCleanMode.ChangeToMode({mode}): {e:#}"));
    rvc_clean_mode::ChangeToModeResponse::decode(&resp).expect("ChangeToModeResponse decodes")
}

/// A refused mode change carries rvc-app's StatusText, on both clusters.
///
/// `Reset` leaves rvc-app in run mode Idle and operational state Stopped, as
/// at boot (rvc-device.cpp:492-494; rvc-device.h:52-56). From there Idle ->
/// Mapping is accepted (rvc-device.cpp:42-61, state becomes Running). While
/// Running, any run mode but Idle is refused with InvalidInMode and a
/// StatusText (rvc-device.cpp:63-69), and any clean-mode change is refused
/// because the run mode is not Idle (rvc-device.cpp:87-96). Mapping -> Idle
/// is then accepted (rvc-device.cpp:71-77).
///
/// Every new mode differs from CurrentMode: chip answers a same-mode change
/// with Success before consulting the delegate (ModeBaseCluster.cpp:412-417),
/// which would make the refusal vacuous. The run mode is returned to Idle
/// before any value is asserted, so a wrong value cannot strand the robot in
/// Mapping. The operational state is left SeekingCharger (rvc-device.cpp:72-73);
/// the next test's `Reset` clears it.
#[tokio::test]
async fn refused_run_mode_change_decodes_its_status_text() {
    let Some((cfg, controller, node_id)) = connect_rvc().await else {
        return;
    };
    reset_rvc(&cfg).await;
    let node = controller.node(node_id);
    use rvc_clean_mode::ModeChangeStatus as CleanStatus;
    use rvc_run_mode::ModeChangeStatus as RunStatus;
    let run_cluster = rvc_run_mode::CLUSTER_ID;
    let clean_cluster = rvc_clean_mode::CLUSTER_ID;
    let decode_run = rvc_run_mode::decode_current_mode;
    let decode_clean = rvc_clean_mode::decode_current_mode;

    let run_before = current_mode(&node, run_cluster, decode_run).await;
    assert_eq!(run_before, 0, "Reset leaves the run mode Idle");
    let clean_before = current_mode(&node, clean_cluster, decode_clean).await;
    // Auto (1), or Quick (0) if Auto is current: never the current mode.
    let clean_target = match clean_before {
        1 => 0,
        _ => 1,
    };

    let start = change_run_mode(&node, 2).await; // Idle -> Mapping
    let mapping = current_mode(&node, run_cluster, decode_run).await;
    let refused = change_run_mode(&node, 1).await; // Mapping -> Cleaning
    let still_mapping = current_mode(&node, run_cluster, decode_run).await;
    let clean_refused = change_clean_mode(&node, clean_target).await;
    let clean_after = current_mode(&node, clean_cluster, decode_clean).await;
    let back = change_run_mode(&node, 0).await; // Mapping -> Idle
    let run_after = current_mode(&node, run_cluster, decode_run).await;

    assert_eq!(
        (start.status, start.status_text),
        (RunStatus::Success, None)
    );
    assert_eq!(mapping, 2);
    assert_eq!(refused.status, RunStatus::InvalidInMode);
    assert_eq!(
        refused.status_text.as_deref(),
        Some("Change to the mapping or cleaning mode is only allowed from idle")
    );
    assert_eq!(still_mapping, 2, "a refused change leaves CurrentMode");
    assert_eq!(clean_refused.status, CleanStatus::InvalidInMode);
    assert_eq!(
        clean_refused.status_text.as_deref(),
        Some("Change of the cleaning mode is only allowed in Idle.")
    );
    assert_eq!(
        clean_after, clean_before,
        "a refused change leaves CurrentMode"
    );
    assert_eq!((back.status, back.status_text), (RunStatus::Success, None));
    assert_eq!(run_after, 0, "the robot is back in Idle");
}

// ---- M9-A3 B3: RvcOperationalState --------------------------------------------

/// The attribute ids rvc-app serves for RvcOperationalState
/// (rvc-app.matter:1980-1997: no CountdownTime).
const RVC_OP_STATE_IDS: [u32; 5] = [0x0000, 0x0001, 0x0003, 0x0004, 0x0005];

/// `RvcOperationalState.OperationalState`, read fresh.
async fn rvc_state(node: &Node) -> rvc_operational_state::OperationalStateEnum {
    let attrs = read_cluster_attributes(node, EP, rvc_operational_state::CLUSTER_ID)
        .await
        .unwrap();
    rvc_operational_state::decode_operational_state(attribute_tlv(
        &attrs,
        rvc_operational_state::attribute_id::OPERATIONAL_STATE,
    ))
    .unwrap()
}

/// After `Reset`: rvc-app's eleven operational states
/// (rvc-operational-state-delegate.h:40-56), no phases (PhaseList null,
/// OperationalStateCluster.cpp:376-379), Stopped, NoError (`SetOperationalState`
/// clears an earlier error, :110-115). Pause and Resume from Stopped are
/// CommandInvalidInState (OperationalStateCluster.cpp:422-425, :493-496).
/// GoHome from Stopped in Idle succeeds and seeks the charger (rvc-device.cpp:146-170
/// `HandleOpStateGoHomeCallback`); Pause is then allowed (SeekingCharger is
/// pause-compatible, OperationalStateCluster.cpp:522-525) and Resume returns
/// to SeekingCharger, the state before the pause (rvc-device.cpp:101-108,
/// :126-131).
#[tokio::test]
async fn rvc_operational_state_decodes_and_goes_home() {
    let Some((cfg, controller, node_id)) = connect_rvc().await else {
        return;
    };
    reset_rvc(&cfg).await;
    let node = controller.node(node_id);
    use rvc::{attribute_id as op, ErrorStateEnum as E, OperationalStateEnum as S};
    use rvc_operational_state as rvc;
    let attrs = sweep_operational_state!(&node, EP, rvc, &RVC_OP_STATE_IDS);
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        rvc::decode_phase_list(tlv(op::PHASE_LIST)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        rvc::decode_current_phase(tlv(op::CURRENT_PHASE)).unwrap(),
        Nullable::Null
    );
    let states: Vec<S> = rvc::decode_operational_state_list(tlv(op::OPERATIONAL_STATE_LIST))
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
            S::Docked,
            S::EmptyingDustBin,
            S::CleaningMop,
            S::FillingWaterTank,
            S::UpdatingMaps,
        ]
    );
    assert_eq!(
        rvc::decode_operational_state(tlv(op::OPERATIONAL_STATE)).unwrap(),
        S::Stopped
    );
    assert_eq!(
        rvc::decode_operational_error(tlv(op::OPERATIONAL_ERROR))
            .unwrap()
            .error_state_id,
        E::NoError
    );

    let pause = operational_command!(&node, EP, rvc, PAUSE, encode_pause);
    let resume = operational_command!(&node, EP, rvc, RESUME, encode_resume);
    assert_eq!(
        (pause.error_state_id, resume.error_state_id),
        (E::CommandInvalidInState, E::CommandInvalidInState)
    );
    let home = operational_command!(&node, EP, rvc, GO_HOME, encode_go_home);
    assert_eq!(home.error_state_id, E::NoError);
    assert_eq!(rvc_state(&node).await, S::SeekingCharger);
    let pause = operational_command!(&node, EP, rvc, PAUSE, encode_pause);
    assert_eq!(pause.error_state_id, E::NoError);
    assert_eq!(rvc_state(&node).await, S::Paused);
    let resume = operational_command!(&node, EP, rvc, RESUME, encode_resume);
    assert_eq!(resume.error_state_id, E::NoError);
    assert_eq!(
        rvc_state(&node).await,
        S::SeekingCharger,
        "back to the state before the pause"
    );
}

/// rvc-app's `ErrorEvent` pipe command raises OperationalError with an RVC
/// error id and moves the robot to Error (rvc-device.cpp:393-476
/// `HandleErrorEvent`, "Stuck" at :413-416; OperationalStateCluster.cpp:147-175
/// `OnOperationalErrorDetected`, which leaves the label out because rvc-app
/// sets none). `ClearError` returns it to Stopped (rvc-device.cpp:478-488:
/// neither docked nor charging, :15-33).
#[tokio::test]
async fn rvc_operational_error_event_carries_the_rvc_error() {
    let Some((cfg, controller, node_id)) = connect_rvc().await else {
        return;
    };
    reset_rvc(&cfg).await;
    let node = controller.node(node_id);
    use rvc::{attribute_id as op, event_id, ErrorStateEnum as E, OperationalStateEnum as S};
    use rvc_operational_state as rvc;
    let cl = rvc::CLUSTER_ID;
    let base = latest_event_number(&node, EP, cl, event_id::OPERATIONAL_ERROR)
        .await
        .unwrap();
    send_app_pipe(&cfg, r#"{"Name": "ErrorEvent", "Error": "Stuck"}"#)
        .await
        .expect("app pipe");
    let items = wait_for_event_after(&node, EP, cl, event_id::OPERATIONAL_ERROR, base)
        .await
        .unwrap();
    let last = items.iter().max_by_key(|i| i.event_number).unwrap();
    let e = rvc::OperationalErrorEvent::decode(&payload_tlv(&last.value))
        .expect("OperationalError decodes");
    assert_eq!(
        (
            e.error_state.error_state_id,
            e.error_state.error_state_label
        ),
        (E::Stuck, None)
    );
    let attrs = read_cluster_attributes(&node, EP, cl).await.unwrap();
    assert_eq!(
        rvc::decode_operational_state(attribute_tlv(&attrs, op::OPERATIONAL_STATE)).unwrap(),
        S::Error
    );
    assert_eq!(
        rvc::decode_operational_error(attribute_tlv(&attrs, op::OPERATIONAL_ERROR))
            .unwrap()
            .error_state_id,
        E::Stuck
    );
    send_app_pipe(&cfg, r#"{"Name": "ClearError"}"#)
        .await
        .expect("app pipe");
    assert_eq!(rvc_state(&node).await, S::Stopped);
}

/// A cleaning run ends with OperationCompletion: RvcRunMode Cleaning from
/// Idle/Stopped starts it (Running, rvc-device.cpp:42-60), and the
/// `ActivityComplete` pipe command emits OperationCompletion
/// { 0, Some(100), Some(10) }, returns the run mode to Idle and the robot to
/// SeekingCharger (rvc-device.cpp:339-360 `HandleActivityCompleteEvent`).
#[tokio::test]
async fn rvc_operation_completion_event_ends_a_cleaning_run() {
    let Some((cfg, controller, node_id)) = connect_rvc().await else {
        return;
    };
    reset_rvc(&cfg).await;
    let node = controller.node(node_id);
    use rvc::{event_id, OperationalStateEnum as S};
    use rvc_operational_state as rvc;
    let start = change_run_mode(&node, 1).await; // Idle -> Cleaning
    assert_eq!(start.status, rvc_run_mode::ModeChangeStatus::Success);
    assert_eq!(rvc_state(&node).await, S::Running);

    let cl = rvc::CLUSTER_ID;
    let base = latest_event_number(&node, EP, cl, event_id::OPERATION_COMPLETION)
        .await
        .unwrap();
    send_app_pipe(&cfg, r#"{"Name": "ActivityComplete"}"#)
        .await
        .expect("app pipe");
    let items = wait_for_event_after(&node, EP, cl, event_id::OPERATION_COMPLETION, base)
        .await
        .unwrap();
    let last = items.iter().max_by_key(|i| i.event_number).unwrap();
    let c = rvc::OperationCompletionEvent::decode(&payload_tlv(&last.value))
        .expect("OperationCompletion decodes");
    assert_eq!(
        (
            c.completion_error_code,
            c.total_operational_time,
            c.paused_time
        ),
        (0, Some(Nullable::Value(100)), Some(Nullable::Value(10)))
    );
    assert_eq!(rvc_state(&node).await, S::SeekingCharger);
    assert_eq!(
        current_mode(
            &node,
            rvc_run_mode::CLUSTER_ID,
            rvc_run_mode::decode_current_mode
        )
        .await,
        0,
        "the run mode is Idle again"
    );
}

// ---- M9-A3 B3: ServiceArea -------------------------------------------------------

/// `ServiceArea.SelectAreas(areas)` and its decoded response.
async fn select_areas(node: &Node, areas: &[u32]) -> service_area::SelectAreasResponse {
    let resp = invoke_for_response(
        node,
        CommandPath {
            endpoint: EP,
            cluster: service_area::CLUSTER_ID,
            command: service_area::command_id::SELECT_AREAS,
        },
        service_area::encode_select_areas(&areas.to_vec()),
        service_area::command_id::SELECT_AREAS_RESPONSE,
    )
    .await
    .unwrap_or_else(|e| panic!("SelectAreas({areas:?}): {e:#}"));
    service_area::SelectAreasResponse::decode(&resp).expect("SelectAreasResponse decodes")
}

/// `ServiceArea.SkipArea(area)` and its decoded response.
async fn skip_area(node: &Node, area: u32) -> service_area::SkipAreaResponse {
    let resp = invoke_for_response(
        node,
        CommandPath {
            endpoint: EP,
            cluster: service_area::CLUSTER_ID,
            command: service_area::command_id::SKIP_AREA,
        },
        service_area::encode_skip_area(area),
        service_area::command_id::SKIP_AREA_RESPONSE,
    )
    .await
    .unwrap_or_else(|e| panic!("SkipArea({area}): {e:#}"));
    service_area::SkipAreaResponse::decode(&resp).expect("SkipAreaResponse decodes")
}

/// Every ServiceArea attribute, decoded, with the exact ids rvc-app serves
/// (rvc-app.matter:1999-2014: all six; features MAPS and PROG,
/// rvc-device.h:49-50).
async fn sweep_service_area(node: &Node) -> Vec<(u32, Vec<u8>)> {
    use service_area::attribute_id as sa;
    let attrs = decode_every_attribute(node, EP, service_area::CLUSTER_ID, |id, t| match id {
        sa::SUPPORTED_AREAS => ok(service_area::decode_supported_areas(t)),
        sa::SUPPORTED_MAPS => ok(service_area::decode_supported_maps(t)),
        sa::SELECTED_AREAS => ok(service_area::decode_selected_areas(t)),
        sa::CURRENT_AREA => ok(service_area::decode_current_area(t)),
        sa::ESTIMATED_END_TIME => ok(service_area::decode_estimated_end_time(t)),
        sa::PROGRESS => ok(service_area::decode_progress(t)),
        other => newer_than_codegen("ServiceArea", other),
    })
    .await;
    assert_exact_attribute_ids(
        "ServiceArea",
        &attrs,
        &[
            sa::SUPPORTED_AREAS,
            sa::SUPPORTED_MAPS,
            sa::SELECTED_AREAS,
            sa::CURRENT_AREA,
            sa::ESTIMATED_END_TIME,
            sa::PROGRESS,
        ],
    );
    attrs
}

/// After `Reset`: rvc-app's two maps and four areas
/// (rvc-service-area-delegate.cpp:26-64 `SetMapTopology`, ids at
/// rvc-service-area-delegate.h:51-58), each area's LocationDescriptorStruct
/// and landmark as chip encodes them, and nothing selected, current or in
/// progress. Without the `Reset` CurrentArea would be 10050, which rvc-app
/// sets at boot (rvc-service-area-delegate.cpp:103).
#[tokio::test]
async fn service_area_decodes_rvc_app_topology() {
    let Some((cfg, controller, node_id)) = connect_rvc().await else {
        return;
    };
    reset_rvc(&cfg).await;
    let node = controller.node(node_id);
    use service_area::attribute_id as sa;
    let attrs = sweep_service_area(&node).await;
    let tlv = |id| attribute_tlv(&attrs, id);
    let maps: Vec<(u32, String)> = service_area::decode_supported_maps(tlv(sa::SUPPORTED_MAPS))
        .unwrap()
        .into_iter()
        .map(|m| (m.map_id, m.name))
        .collect();
    assert_eq!(
        maps,
        [(3, "My Map XX".to_string()), (245, "My Map YY".to_string())]
    );
    let areas = service_area::decode_supported_areas(tlv(sa::SUPPORTED_AREAS)).unwrap();
    let summary: Vec<_> = areas
        .iter()
        .map(|a| {
            let location = match &a.area_info.location_info {
                Nullable::Value(l) => Some((l.location_name.clone(), l.floor_number, l.area_type)),
                Nullable::Null => None,
            };
            let landmark = match &a.area_info.landmark_info {
                Nullable::Value(l) => Some((l.landmark_tag, l.relative_position_tag)),
                Nullable::Null => None,
            };
            (a.area_id, a.map_id, location, landmark)
        })
        .collect();
    let loc = |n: &str, f: Nullable<i16>, t: Nullable<u8>| Some((n.to_string(), f, t));
    // PlayRoom 0x41, BackDoor 0x02, Couch 0x0D, NextTo 0x01 (chip's global
    // AreaTypeTag / LandmarkTag / RelativePositionTag,
    // zzz_generated/app-common/clusters/shared/Enums.h:219, :274, :285, :406).
    assert_eq!(
        summary,
        [
            (
                7,
                Nullable::Value(3),
                loc("My Location A", Nullable::Value(4), Nullable::Null),
                None
            ),
            (
                1_234_567,
                Nullable::Value(3),
                loc("My Location B", Nullable::Null, Nullable::Null),
                None
            ),
            (
                10_050,
                Nullable::Value(245),
                loc("", Nullable::Value(-1), Nullable::Value(0x41)),
                Some((0x02, Nullable::Value(0x01)))
            ),
            (
                0x8888_8888,
                Nullable::Value(245),
                loc("My Location D", Nullable::Null, Nullable::Null),
                Some((0x0D, Nullable::Value(0x01)))
            ),
        ]
    );
    assert_eq!(
        service_area::decode_selected_areas(tlv(sa::SELECTED_AREAS)).unwrap(),
        Vec::<u32>::new()
    );
    assert_eq!(
        service_area::decode_current_area(tlv(sa::CURRENT_AREA)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        service_area::decode_estimated_end_time(tlv(sa::ESTIMATED_END_TIME)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        service_area::decode_progress(tlv(sa::PROGRESS))
            .unwrap()
            .len(),
        0
    );
}

/// SelectAreas and SkipArea outside a run (service-area-server.cpp:214-366
/// `HandleSelectAreasCmd`, :368-411 `HandleSkipAreaCmd`;
/// rvc-service-area-delegate.cpp:116-162 `IsValidSelectAreasSet`;
/// rvc-device.cpp:188-213 `SaHandleSkipArea`): skipping with nothing selected
/// is InvalidAreaList (server :382-386); two areas of one map are accepted
/// and become SelectedAreas; areas of two maps are InvalidSet (delegate
/// :152-157); an unknown area is UnsupportedArea with an empty StatusText
/// (server :282-286, before the delegate's own UnsupportedArea text); skipping
/// an area that is not the current one (null after `Reset`) is InvalidInMode
/// (rvc-device.cpp:190-195).
#[tokio::test]
async fn service_area_validates_selection_and_skips() {
    let Some((cfg, controller, node_id)) = connect_rvc().await else {
        return;
    };
    reset_rvc(&cfg).await;
    let node = controller.node(node_id);
    use service_area::{attribute_id as sa, SelectAreasStatus as Sel, SkipAreaStatus as Skip};
    let r = skip_area(&node, 7).await;
    assert_eq!(
        (r.status, r.status_text.as_str()),
        (Skip::InvalidAreaList, "")
    );
    let r = select_areas(&node, &[7, 1_234_567]).await;
    assert_eq!((r.status, r.status_text.as_str()), (Sel::Success, ""));
    let selected = |attrs: &[(u32, Vec<u8>)]| {
        service_area::decode_selected_areas(attribute_tlv(attrs, sa::SELECTED_AREAS)).unwrap()
    };
    let attrs = read_cluster_attributes(&node, EP, service_area::CLUSTER_ID)
        .await
        .unwrap();
    assert_eq!(selected(&attrs), [7, 1_234_567]);
    let r = select_areas(&node, &[7, 10_050]).await;
    assert_eq!(
        (r.status, r.status_text.as_str()),
        (
            Sel::InvalidSet,
            "all selected areas must be in the same map"
        )
    );
    let r = select_areas(&node, &[99]).await;
    assert_eq!(
        (r.status, r.status_text.as_str()),
        (Sel::UnsupportedArea, "")
    );
    let attrs = read_cluster_attributes(&node, EP, service_area::CLUSTER_ID)
        .await
        .unwrap();
    assert_eq!(
        selected(&attrs),
        [7, 1_234_567],
        "refused selections change nothing"
    );
    let r = skip_area(&node, 1_234_567).await;
    assert_eq!(
        (r.status, r.status_text.as_str()),
        (
            Skip::InvalidInMode,
            "the skipped area does not match the current area"
        )
    );
}

/// Progress during a run: with areas 7 and 1234567 selected, RvcRunMode
/// Cleaning makes 7 current and Operating (TotalOperationalTime null) and
/// 1234567 Pending (no times; rvc-service-area-delegate.cpp:199-217
/// `SetAttributesAtCleanStart`, service-area-server.cpp:1139-1174
/// `AddPendingProgressElement`, :1176-1210 `SetProgressStatus`). SelectAreas
/// is refused while running (rvc-device.cpp:178-186
/// `SaIsSetSelectedAreasAllowed`). SkipArea(7) moves on: 7 Skipped (its null
/// TotalOperationalTime kept), 1234567 current and Operating
/// (rvc-device.cpp:204-205, rvc-service-area-delegate.cpp:271-297).
#[tokio::test]
async fn service_area_progress_follows_a_cleaning_run() {
    let Some((cfg, controller, node_id)) = connect_rvc().await else {
        return;
    };
    reset_rvc(&cfg).await;
    let node = controller.node(node_id);
    use service_area::{
        attribute_id as sa, OperationalStatusEnum as Op, SelectAreasStatus as Sel,
        SkipAreaStatus as Skip,
    };
    assert_eq!(
        select_areas(&node, &[7, 1_234_567]).await.status,
        Sel::Success
    );
    let start = change_run_mode(&node, 1).await; // Idle -> Cleaning
    assert_eq!(start.status, rvc_run_mode::ModeChangeStatus::Success);
    let progress = |attrs: &[(u32, Vec<u8>)]| -> Vec<_> {
        service_area::decode_progress(attribute_tlv(attrs, sa::PROGRESS))
            .unwrap()
            .into_iter()
            .map(|p| {
                (
                    p.area_id,
                    p.status,
                    p.total_operational_time,
                    p.estimated_time,
                )
            })
            .collect()
    };
    let current = |attrs: &[(u32, Vec<u8>)]| {
        service_area::decode_current_area(attribute_tlv(attrs, sa::CURRENT_AREA)).unwrap()
    };
    let attrs = sweep_service_area(&node).await;
    assert_eq!(current(&attrs), Nullable::Value(7));
    assert_eq!(
        progress(&attrs),
        [
            (7, Op::Operating, Some(Nullable::Null), None),
            (1_234_567, Op::Pending, None, None),
        ]
    );
    let r = select_areas(&node, &[7]).await;
    assert_eq!(
        (r.status, r.status_text.as_str()),
        (
            Sel::InvalidInMode,
            "cannot set the Selected Areas while the device is running"
        )
    );
    let r = skip_area(&node, 7).await;
    assert_eq!((r.status, r.status_text.as_str()), (Skip::Success, ""));
    let attrs = read_cluster_attributes(&node, EP, service_area::CLUSTER_ID)
        .await
        .unwrap();
    assert_eq!(current(&attrs), Nullable::Value(1_234_567));
    assert_eq!(
        progress(&attrs),
        [
            (7, Op::Skipped, Some(Nullable::Null), None),
            (1_234_567, Op::Operating, Some(Nullable::Null), None),
        ]
    );
}
