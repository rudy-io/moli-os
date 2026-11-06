// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! MicrowaveOvenControl on a live microwave-oven-app
//! (`just integration-microwave-oven`), endpoint 1. No other chip
//! example app serves it, at master or v1.4.2.0.
//!
//! Live on a Linux arm64 Pi (chip master 5cd2917a; 3 passed): microwave-oven-app
//! needs Rosetta 2 to build on Apple silicon, so it runs there, not on the Mac.
//! Every expected value below is also read from chip source.
//! Line numbers are chip master 5cd2917a:
//!
//! - examples/microwave-oven-app/microwave-oven-common/include/
//!   microwave-oven-device.h: features PowerAsNumber and PowerNumberLimits
//!   (:50-51), power 20..=90 in steps of 10 and 90 at boot, at most 86400 s
//!   of cook time (:229-233, :247), four operational states and no phases
//!   (:265-274);
//! - microwave-oven-common/src/microwave-oven-device.cpp: WattRating 1000
//!   without the WATTS feature (:39-49), the cooking callbacks (:55-99),
//!   CountdownTime = CookTime (:112-115), endpoint 1 (:247);
//! - microwave-oven-app.matter:1850-1866: the seven MicrowaveOvenControl
//!   attributes served (no SupportedWatts / SelectedWattIndex) and both
//!   commands; :1868-1887: OperationalState's six attributes;
//! - src/app/clusters/microwave-oven-control-server/
//!   MicrowaveOvenControlCluster.cpp: 30 s of cook time at boot (:37) and
//!   `HandleSetCookingParameters` / `HandleAddMoreTime` (:195-329).
//!
//! At v1.4.2.0 the app files differ only in formatting and ignored-return
//! annotations, and the server is microwave-oven-control-server.cpp: the same
//! defaults (its header :34, :88) and the same checks in the same order. The
//! one difference, a v1.4.2.0 InvalidCommand when SetCookingParameters
//! carries no field at all (dropped at master, which then applies the
//! defaults), is not exercised: every command below carries a field. The
//! app reads no app pipe and the cluster has no events.
//!
//! The tests run single-threaded (`--test-threads=1`, xtask `run_tests`)
//! against one microwave-oven-app booted with a fresh KVS, and restore every
//! value they change. The boot values are held in memory only (CookTime in
//! the cluster server, PowerSetting in the app's device class; neither is
//! written to the KVS), so restarting the app resets them. SetCookingParameters without a CookMode applies the
//! Normal mode (0), which MicrowaveOvenMode boots in (ModeBase `Init` takes
//! the first supported mode; microwave-oven-device.h:257-262), so the mode
//! never changes.

use integration_tests::dut::DutConfig;
use integration_tests::sweep::{
    assert_exact_attribute_ids, attribute_tlv, decode_every_attribute, invoke_for_status,
    newer_than_codegen, ok, read_cluster_attributes,
};
use integration_tests::{operational_command, sweep_operational_state};
use matter_clusters::clusters::{microwave_oven_control, operational_state};
use matter_clusters::types::Nullable;
use matter_controller::{CommandPath, ImStatus, MatterController, Node};

/// microwave-oven-app's endpoint (`kDemoEndpointId`, microwave-oven-device.cpp:247).
const EP: u16 = 1;

/// IM status codes the server answers with (Matter Core spec 8.10.1).
const INVALID_COMMAND: ImStatus = ImStatus::Failure(0x85);
const CONSTRAINT_ERROR: ImStatus = ImStatus::Failure(0x87);

/// The DUT config, controller and node id, or `None` (skip) unless the DUT is
/// microwave-oven-app.
async fn connect_microwave_oven() -> Option<(DutConfig, MatterController, u64)> {
    let Some(cfg) = DutConfig::from_env() else {
        eprintln!(
            "skipped: no DUT (set MATTER_INTEGRATION_DUT via `just integration-microwave-oven`)"
        );
        return None;
    };
    if !cfg.is_app("microwave-oven") {
        eprintln!("skipped: needs the microwave-oven-app DUT (`just integration-microwave-oven`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((cfg, controller, node_id))
}

/// `(CookTime, PowerSetting)`, read fresh.
async fn cook_time_and_power(node: &Node) -> (u32, u8) {
    use microwave_oven_control::attribute_id as mw;
    let attrs = read_cluster_attributes(node, EP, microwave_oven_control::CLUSTER_ID)
        .await
        .unwrap();
    (
        microwave_oven_control::decode_cook_time(attribute_tlv(&attrs, mw::COOK_TIME)).unwrap(),
        microwave_oven_control::decode_power_setting(attribute_tlv(&attrs, mw::POWER_SETTING))
            .unwrap(),
    )
}

/// `SetCookingParameters(None, cook_time, power, watt_index, None)` and the
/// bare status it answers (the command has no response command).
async fn set_cooking(
    node: &Node,
    cook_time: Option<u32>,
    power: Option<u8>,
    watt_index: Option<u8>,
) -> ImStatus {
    invoke_for_status(
        node,
        CommandPath {
            endpoint: EP,
            cluster: microwave_oven_control::CLUSTER_ID,
            command: microwave_oven_control::command_id::SET_COOKING_PARAMETERS,
        },
        microwave_oven_control::encode_set_cooking_parameters(
            None, cook_time, power, watt_index, None,
        ),
    )
    .await
    .unwrap()
}

/// Every attribute microwave-oven-app serves decodes, and they are exactly
/// the seven of microwave-oven-app.matter:1850-1866 (no SupportedWatts or
/// SelectedWattIndex, which need the WATTS feature), with the fixed values of
/// microwave-oven-device.h:229-232 and WattRating 1000
/// (microwave-oven-device.cpp:48). CookTime and PowerSetting hold their boot
/// values: this test sorts first, and the one that changes them restores them.
#[tokio::test]
async fn microwave_oven_control_decodes_its_attributes() {
    let Some((_, controller, node_id)) = connect_microwave_oven().await else {
        return;
    };
    let node = controller.node(node_id);
    use m::attribute_id as mw;
    use microwave_oven_control as m;
    let attrs = decode_every_attribute(&node, EP, m::CLUSTER_ID, |id, t| match id {
        mw::COOK_TIME => ok(m::decode_cook_time(t)),
        mw::MAX_COOK_TIME => ok(m::decode_max_cook_time(t)),
        mw::POWER_SETTING => ok(m::decode_power_setting(t)),
        mw::MIN_POWER => ok(m::decode_min_power(t)),
        mw::MAX_POWER => ok(m::decode_max_power(t)),
        mw::POWER_STEP => ok(m::decode_power_step(t)),
        mw::SUPPORTED_WATTS => ok(m::decode_supported_watts(t)),
        mw::SELECTED_WATT_INDEX => ok(m::decode_selected_watt_index(t)),
        mw::WATT_RATING => ok(m::decode_watt_rating(t)),
        other => newer_than_codegen("MicrowaveOvenControl", other),
    })
    .await;
    assert_exact_attribute_ids(
        "MicrowaveOvenControl",
        &attrs,
        &[
            mw::COOK_TIME,
            mw::MAX_COOK_TIME,
            mw::POWER_SETTING,
            mw::MIN_POWER,
            mw::MAX_POWER,
            mw::POWER_STEP,
            mw::WATT_RATING,
        ],
    );
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        m::decode_max_cook_time(tlv(mw::MAX_COOK_TIME)).unwrap(),
        86_400
    );
    assert_eq!(m::decode_min_power(tlv(mw::MIN_POWER)).unwrap(), 20);
    assert_eq!(m::decode_max_power(tlv(mw::MAX_POWER)).unwrap(), 90);
    assert_eq!(m::decode_power_step(tlv(mw::POWER_STEP)).unwrap(), 10);
    assert_eq!(m::decode_watt_rating(tlv(mw::WATT_RATING)).unwrap(), 1000);
    // Boot values: kDefaultCookTimeSec (MicrowaveOvenControlCluster.cpp:37)
    // and kDefaultPowerSettingNum = kMaxPowerNum (microwave-oven-device.h:233).
    assert_eq!(m::decode_cook_time(tlv(mw::COOK_TIME)).unwrap(), 30);
    assert_eq!(m::decode_power_setting(tlv(mw::POWER_SETTING)).unwrap(), 90);
}

/// SetCookingParameters and AddMoreTime: accepted values are read back; a
/// power off the 20..=90 step-10 grid, a zero cook time and a watt index on a
/// PowerAsNumber oven are refused and change nothing
/// (MicrowaveOvenControlCluster.cpp:228-260; microwave-oven-control-server.cpp
/// at v1.4.2.0 checks the same, in the same order). The oven must be Stopped
/// (:206-207), as it boots. CookTime and PowerSetting are restored.
#[tokio::test]
async fn microwave_oven_control_validates_cooking_parameters() {
    let Some((_, controller, node_id)) = connect_microwave_oven().await else {
        return;
    };
    let node = controller.node(node_id);
    let before = cook_time_and_power(&node).await;
    assert_ne!(before, (45, 60), "the accepted values must be a change");
    assert_eq!(
        set_cooking(&node, Some(45), Some(60), None).await,
        ImStatus::Success
    );
    assert_eq!(cook_time_and_power(&node).await, (45, 60));
    // Each refusal names the check it trips. Without a CookTime field the
    // server checks its 30 s default (:228-230), which passes.
    assert_eq!(
        set_cooking(&node, None, Some(65), None).await,
        CONSTRAINT_ERROR,
        "off the step grid: (65 - 20) % 10 != 0 (:256-260)"
    );
    assert_eq!(
        set_cooking(&node, Some(0), None, None).await,
        CONSTRAINT_ERROR,
        "at least 1 s of cook time (:38, :228-230)"
    );
    assert_eq!(
        set_cooking(&node, None, None, Some(0)).await,
        INVALID_COMMAND,
        "a watt index needs the WATTS feature (:239-242)"
    );
    assert_eq!(
        cook_time_and_power(&node).await,
        (45, 60),
        "refused commands change nothing"
    );
    // AddMoreTime from Stopped (only Error refuses it) stays under
    // MaxCookTime: 45 + 15 (:316-329).
    let add = invoke_for_status(
        &node,
        CommandPath {
            endpoint: EP,
            cluster: microwave_oven_control::CLUSTER_ID,
            command: microwave_oven_control::command_id::ADD_MORE_TIME,
        },
        microwave_oven_control::encode_add_more_time(15),
    )
    .await
    .unwrap();
    assert_eq!(add, ImStatus::Success);
    assert_eq!(cook_time_and_power(&node).await, (60, 60));
    assert_eq!(
        set_cooking(&node, Some(before.0), Some(before.1), None).await,
        ImStatus::Success
    );
    assert_eq!(cook_time_and_power(&node).await, before, "restored");
}

/// The OperationalState cluster microwave-oven-app serves next to it
/// (microwave-oven-app.matter:1868-1887): four states, no phases (PhaseList
/// and CurrentPhase null: OperationalStateCluster.cpp:370-378, and
/// CurrentPhase is never set), no error, Stopped, and CountdownTime equal to
/// CookTime, read through the delegate (:403-407;
/// microwave-oven-device.cpp:112-115). Stop from Stopped answers NoError
/// without calling the delegate (:449-464), so the state stays Stopped. At
/// v1.4.2.0, operational-state-server.cpp has the same logic.
#[tokio::test]
async fn microwave_oven_operational_state_counts_down_the_cook_time() {
    let Some((_, controller, node_id)) = connect_microwave_oven().await else {
        return;
    };
    let node = controller.node(node_id);
    use operational_state::{attribute_id as op, ErrorStateEnum, OperationalStateEnum as S};
    let ids = [0x0000, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005];
    let attrs = sweep_operational_state!(&node, EP, operational_state, &ids);
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        operational_state::decode_phase_list(tlv(op::PHASE_LIST)).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        operational_state::decode_current_phase(tlv(op::CURRENT_PHASE)).unwrap(),
        Nullable::Null
    );
    let states: Vec<S> =
        operational_state::decode_operational_state_list(tlv(op::OPERATIONAL_STATE_LIST))
            .unwrap()
            .into_iter()
            .map(|s| s.operational_state_id)
            .collect();
    assert_eq!(states, [S::Stopped, S::Running, S::Paused, S::Error]);
    assert_eq!(
        operational_state::decode_operational_state(tlv(op::OPERATIONAL_STATE)).unwrap(),
        S::Stopped
    );
    assert_eq!(
        operational_state::decode_operational_error(tlv(op::OPERATIONAL_ERROR))
            .unwrap()
            .error_state_id,
        ErrorStateEnum::NoError
    );
    let (cook_time, _) = cook_time_and_power(&node).await;
    assert_eq!(
        operational_state::decode_countdown_time(tlv(op::COUNTDOWN_TIME)).unwrap(),
        Nullable::Value(cook_time)
    );
    let stop = operational_command!(&node, EP, operational_state, STOP, encode_stop);
    assert_eq!(stop.error_state_id, ErrorStateEnum::NoError);
    let after = read_cluster_attributes(&node, EP, operational_state::CLUSTER_ID)
        .await
        .unwrap();
    assert_eq!(
        operational_state::decode_operational_state(attribute_tlv(&after, op::OPERATIONAL_STATE))
            .unwrap(),
        S::Stopped,
        "Stop from Stopped changes nothing"
    );
}
