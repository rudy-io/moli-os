// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! The energy-management clusters on a live all-clusters-app (endpoint
//! 1). Which of them all-clusters serves depends on the chip release:
//! - EnergyPreference: master 5cd2917a and v1.4.2.0 (the nightly's pin), with
//!   the same delegate (`all-clusters-common/src/energy-preference-delegate.cpp`;
//!   only `TEMPORARY_RETURN_IGNORED` differs) and the same server
//!   (`src/app/clusters/energy-preference-server/energy-preference-server.cpp`).
//!   Runs locally and in the nightly.
//! - WaterHeaterManagement, DeviceEnergyManagement, EnergyEvse and
//!   PowerTopology: v1.4.2.0 only (master's all-clusters dropped them).
//!   These tests ask the checkout (`sweep::all_clusters_serves`) and skip with
//!   a log on master: only the nightly runs them. v1.4.2.0's all-clusters
//!   takes them from the energy-management-app sources
//!   (`examples/all-clusters-app/linux/BUILD.gn`): WaterHeaterManagement with
//!   EM and TP (`water-heater/src/WhmMain.cpp`), DeviceEnergyManagement with
//!   PA, PFR, SFR, STA and PAU (`device-energy-management-stub.cpp`),
//!   EnergyEvse with PREF and RFID (`energy-evse-stub.cpp`), PowerTopology with
//!   SET and DYPF and no endpoints (`power-topology-stub.cpp`).
//!
//! The energy-management event stimuli live in evse-app's tests
//! (`clusters_evse.rs`): v1.4.2.0's all-clusters enables only the
//! water-heater trigger, and these tests need none (Boost and CancelBoost
//! emit WaterHeaterManagement's events).

use std::time::{Duration, Instant};

use integration_tests::dut::DutConfig;
use integration_tests::events::{payload_tlv, read_event_items};
use integration_tests::sweep::{
    all_clusters_serves, assert_exact_attribute_ids, attribute_tlv, decode_every_attribute,
    invoke_for_response, invoke_for_status, newer_than_codegen, ok, read_cluster_attributes,
    write_attribute,
};
use matter_clusters::clusters::{
    device_energy_management as dem, energy_evse as evse, energy_preference as pref,
    power_topology as topo, water_heater_management as whm,
};
use matter_controller::{AttributePath, CommandPath, EventPath, ImStatus, MatterController, Node};

/// Every energy-management cluster all-clusters serves lives on endpoint 1.
const EP: u16 = 1;

/// IM status codes the servers answer with (Matter Core spec 8.10.1).
const INVALID_COMMAND: ImStatus = ImStatus::Failure(0x85);
const CONSTRAINT_ERROR: ImStatus = ImStatus::Failure(0x87);

/// The DUT config, controller and node id, or `None` (skip) unless the DUT is
/// all-clusters.
async fn connect_all_clusters() -> Option<(DutConfig, MatterController, u64)> {
    let cfg = DutConfig::from_env()?;
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: energy sweep needs the all-clusters DUT (`just integration`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((cfg, controller, node_id))
}

/// Whether this checkout's all-clusters serves `cluster` on [`EP`]; logs the
/// skip when it does not (master: only v1.4.2.0 serves the energy-management
/// clusters other than EnergyPreference).
fn served(cfg: &DutConfig, cluster: &str) -> bool {
    let yes = all_clusters_serves(cfg, EP, cluster).unwrap();
    if !yes {
        eprintln!(
            "skipped: this checkout's all-clusters does not serve {cluster} (v1.4.2.0 does; the nightly runs it)"
        );
    }
    yes
}

/// A command path on [`EP`].
fn command(cluster: u32, command: u32) -> CommandPath {
    CommandPath {
        endpoint: EP,
        cluster,
        command,
    }
}

/// Invoke a command on [`EP`] and return the bare status it answers.
async fn status_of(node: &Node, cluster: u32, id: u32, fields: Vec<u8>) -> ImStatus {
    invoke_for_status(node, command(cluster, id), fields)
        .await
        .unwrap()
}

/// One attribute of `cluster` on [`EP`], read fresh, as its value TLV.
async fn read_tlv(node: &Node, cluster: u32, attribute: u32) -> Vec<u8> {
    let attrs = read_cluster_attributes(node, EP, cluster).await.unwrap();
    attribute_tlv(&attrs, attribute).to_vec()
}

// ---- EnergyPreference (both refs) -------------------------------------------

/// Every EnergyPreference attribute decodes, with all-clusters' values: three
/// energy balances (the middle one unlabelled), priorities Efficiency then
/// Comfort, and three low-power sensitivities (the delegate counts the
/// sensitivities with the balances' array size, so only the first three of its
/// ten are served). Both current levels are written, refused at the list
/// length (ConstraintError: the server's pre-change callback checks the index
/// against the delegate's count) and restored.
#[tokio::test]
async fn energy_preference_decodes_every_attribute_and_writes_its_levels() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use pref::{attribute_id as a, EnergyPriorityEnum};
    let attrs = decode_every_attribute(&node, EP, pref::CLUSTER_ID, |id, t| match id {
        a::ENERGY_BALANCES => ok(pref::decode_energy_balances(t)),
        a::CURRENT_ENERGY_BALANCE => ok(pref::decode_current_energy_balance(t)),
        a::ENERGY_PRIORITIES => ok(pref::decode_energy_priorities(t)),
        a::LOW_POWER_MODE_SENSITIVITIES => ok(pref::decode_low_power_mode_sensitivities(t)),
        a::CURRENT_LOW_POWER_MODE_SENSITIVITY => {
            ok(pref::decode_current_low_power_mode_sensitivity(t))
        }
        other => newer_than_codegen("EnergyPreference", other),
    })
    .await;
    assert_exact_attribute_ids("EnergyPreference", &attrs, &[0, 1, 2, 3, 4]);
    let tlv = |id| attribute_tlv(&attrs, id);
    let labels = |list: Vec<pref::BalanceStruct>| -> Vec<(u8, Option<String>)> {
        list.into_iter().map(|b| (b.step, b.label)).collect()
    };
    assert_eq!(
        labels(pref::decode_energy_balances(tlv(a::ENERGY_BALANCES)).unwrap()),
        [
            (0, Some("Efficient".to_string())),
            (50, None),
            (100, Some("Comfort".to_string()))
        ]
    );
    assert_eq!(
        pref::decode_energy_priorities(tlv(a::ENERGY_PRIORITIES)).unwrap(),
        [EnergyPriorityEnum::Efficiency, EnergyPriorityEnum::Comfort]
    );
    assert_eq!(
        labels(
            pref::decode_low_power_mode_sensitivities(tlv(a::LOW_POWER_MODE_SENSITIVITIES))
                .unwrap()
        ),
        [
            (0, Some("1 Minute".to_string())),
            (12, Some("5 Minutes".to_string())),
            (24, Some("10 Minutes".to_string()))
        ]
    );
    write_level(
        &node,
        a::CURRENT_ENERGY_BALANCE,
        pref::encode_current_energy_balance,
    )
    .await;
    write_level(
        &node,
        a::CURRENT_LOW_POWER_MODE_SENSITIVITY,
        pref::encode_current_low_power_mode_sensitivity,
    )
    .await;
}

/// Write index 1 (accepted), index 3 (one past the three entries: refused,
/// nothing changes), then the original index.
async fn write_level(node: &Node, attribute: u32, encode: fn(u8) -> Vec<u8>) {
    let at = AttributePath {
        endpoint: EP,
        cluster: pref::CLUSTER_ID,
        attribute,
    };
    let read = || async {
        // Both levels are uint8; the balance decoder reads either.
        pref::decode_current_energy_balance(&read_tlv(node, pref::CLUSTER_ID, attribute).await)
            .unwrap()
    };
    let original = read().await;
    assert_eq!(
        write_attribute(node, at, encode(1)).await.unwrap(),
        ImStatus::Success
    );
    assert_eq!(read().await, 1);
    assert_eq!(
        write_attribute(node, at, encode(3)).await.unwrap(),
        CONSTRAINT_ERROR
    );
    assert_eq!(read().await, 1, "a refused write changes nothing");
    assert_eq!(
        write_attribute(node, at, encode(original)).await.unwrap(),
        ImStatus::Success
    );
    assert_eq!(read().await, original, "restored");
}

// ---- WaterHeaterManagement (v1.4.2.0) ---------------------------------------

/// Every WaterHeaterManagement event numbered above `baseline`, newest last.
/// Read through the cluster-wide event path: v1.4.2.0's all-clusters-app.matter
/// declares no `emits event` for the cluster, so a concrete event path is not
/// relied on.
async fn whm_events_after(node: &Node, baseline: Option<u64>) -> Vec<(u32, Vec<u8>)> {
    let mut items = read_event_items(node, EventPath::cluster(EP, whm::CLUSTER_ID))
        .await
        .unwrap();
    items.retain(|i| baseline.is_none_or(|b| i.event_number > b));
    items.sort_by_key(|i| i.event_number);
    items
        .iter()
        .map(|i| (i.path.event.unwrap(), payload_tlv(&i.value)))
        .collect()
}

/// The newest WaterHeaterManagement event number, if any.
async fn whm_baseline(node: &Node) -> Option<u64> {
    read_event_items(node, EventPath::cluster(EP, whm::CLUSTER_ID))
        .await
        .unwrap()
        .iter()
        .map(|i| i.event_number)
        .max()
}

/// Poll until a WaterHeaterManagement `event` numbered above `baseline`
/// arrives (10 s), and return its payload.
async fn wait_for_whm_event(node: &Node, event: u32, baseline: Option<u64>) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some((_, tlv)) = whm_events_after(node, baseline)
            .await
            .into_iter()
            .rev()
            .find(|(id, _)| *id == event)
        {
            return tlv;
        }
        assert!(
            Instant::now() < deadline,
            "no WaterHeaterManagement event {event:#04x}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn boost_state(tlv: &[u8]) -> whm::BoostStateEnum {
    whm::decode_boost_state(tlv).unwrap()
}

/// Every WaterHeaterManagement attribute decodes (exact ids 0..=5 with EM and
/// TP; HeaterTypes ImmersionElement1, `WhmManufacturer::Init`); a Boost to
/// 80 % with a 20 % reheat turns BoostState Active and emits BoostStarted
/// carrying the request's BoostInfo; CancelBoost turns it Inactive and emits
/// BoostEnded. v1.4.2.0 only.
#[tokio::test]
async fn water_heater_management_boosts_and_cancels() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    if !served(&cfg, "WaterHeaterManagement") {
        return;
    }
    let node = controller.node(node_id);
    use whm::{attribute_id as a, command_id as c, event_id as e};
    let cl = whm::CLUSTER_ID;
    assert_eq!(
        status_of(&node, cl, c::CANCEL_BOOST, whm::encode_cancel_boost()).await,
        ImStatus::Success
    );
    let attrs = decode_every_attribute(&node, EP, cl, |id, t| match id {
        a::HEATER_TYPES => ok(whm::decode_heater_types(t)),
        a::HEAT_DEMAND => ok(whm::decode_heat_demand(t)),
        a::TANK_VOLUME => ok(whm::decode_tank_volume(t)),
        a::ESTIMATED_HEAT_REQUIRED => ok(whm::decode_estimated_heat_required(t)),
        a::TANK_PERCENTAGE => ok(whm::decode_tank_percentage(t)),
        a::BOOST_STATE => ok(whm::decode_boost_state(t)),
        other => newer_than_codegen("WaterHeaterManagement", other),
    })
    .await;
    assert_exact_attribute_ids("WaterHeaterManagement", &attrs, &[0, 1, 2, 3, 4, 5]);
    assert_eq!(
        whm::decode_heater_types(attribute_tlv(&attrs, a::HEATER_TYPES)).unwrap(),
        whm::WaterHeaterHeatSourceBitmap::IMMERSION_ELEMENT1
    );
    assert_eq!(
        boost_state(attribute_tlv(&attrs, a::BOOST_STATE)),
        whm::BoostStateEnum::Inactive
    );

    let info = whm::WaterHeaterBoostInfoStruct {
        duration: 60,
        one_shot: None,
        emergency_boost: None,
        temporary_setpoint: None,
        target_percentage: Some(80),
        target_reheat: Some(20),
    };
    let base = whm_baseline(&node).await;
    assert_eq!(
        status_of(&node, cl, c::BOOST, whm::encode_boost(info.clone())).await,
        ImStatus::Success
    );
    let started =
        whm::BoostStartedEvent::decode(&wait_for_whm_event(&node, e::BOOST_STARTED, base).await)
            .unwrap();
    assert_eq!(started.boost_info, info);
    assert_eq!(
        boost_state(&read_tlv(&node, cl, a::BOOST_STATE).await),
        whm::BoostStateEnum::Active
    );

    let base = whm_baseline(&node).await;
    assert_eq!(
        status_of(&node, cl, c::CANCEL_BOOST, whm::encode_cancel_boost()).await,
        ImStatus::Success
    );
    assert_eq!(
        wait_for_whm_event(&node, e::BOOST_ENDED, base).await,
        [0x15, 0x18]
    );
    assert_eq!(
        boost_state(&read_tlv(&node, cl, a::BOOST_STATE).await),
        whm::BoostStateEnum::Inactive
    );
}

/// chip's server checks WaterHeaterBoostInfoStruct's two rules before its
/// delegate runs (water-heater-management-server.cpp `HandleBoost`, v1.4.2.0;
/// the same in master's WaterHeaterManagementCluster.cpp), with the
/// TankPercent feature (TP is set on all-clusters v1.4.2.0: WhmMain.cpp; without
/// TP it refuses any TargetPercentage or TargetReheat): a TargetReheat
/// without TargetPercentage, and a TargetReheat with OneShot, are each
/// refused with InvalidCommand, and BoostState stays Inactive. The generated
/// encoder sends both as asked (the rules are documented, not enforced).
/// v1.4.2.0 only.
#[tokio::test]
async fn water_heater_boost_refuses_a_reheat_that_breaks_its_rules() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    if !served(&cfg, "WaterHeaterManagement") {
        return;
    }
    let node = controller.node(node_id);
    let cl = whm::CLUSTER_ID;
    let boost = |one_shot, target_percentage| {
        whm::encode_boost(whm::WaterHeaterBoostInfoStruct {
            duration: 60,
            one_shot,
            emergency_boost: None,
            temporary_setpoint: None,
            target_percentage,
            target_reheat: Some(20),
        })
    };
    for (fields, why) in [
        (boost(None, None), "TargetReheat without TargetPercentage"),
        (boost(Some(true), Some(80)), "TargetReheat with OneShot"),
    ] {
        assert_eq!(
            status_of(&node, cl, whm::command_id::BOOST, fields).await,
            INVALID_COMMAND,
            "{why}"
        );
    }
    assert_eq!(
        boost_state(&read_tlv(&node, cl, whm::attribute_id::BOOST_STATE).await),
        whm::BoostStateEnum::Inactive
    );
}

// ---- DeviceEnergyManagement, EnergyEvse, PowerTopology (v1.4.2.0) -------------

/// v1.4.2.0's all-clusters serves DeviceEnergyManagement (all eight
/// attributes), EnergyEvse (the 17 attributes its `.matter` lists for PREF and
/// RFID; GetTargets answers GetTargetsResponse) and PowerTopology (both lists,
/// empty: the stub delegate has no endpoints). Every attribute decodes.
/// v1.4.2.0's server answers GetTargets with GetTargetsResponse followed by a
/// Success status for the same path (energy-evse-server.cpp:487-488; master
/// sends only the response); `invoke_for_response` reads the first
/// InvokeResponseIB. v1.4.2.0 only.
#[tokio::test]
async fn energy_clusters_decode_on_all_clusters() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    if !served(&cfg, "DeviceEnergyManagement") {
        return;
    }
    let node = controller.node(node_id);
    decode_device_energy_management(&node).await;
    decode_energy_evse(&node).await;
    let attrs = decode_every_attribute(&node, EP, topo::CLUSTER_ID, |id, t| match id {
        topo::attribute_id::AVAILABLE_ENDPOINTS => ok(topo::decode_available_endpoints(t)),
        topo::attribute_id::ACTIVE_ENDPOINTS => ok(topo::decode_active_endpoints(t)),
        other => newer_than_codegen("PowerTopology", other),
    })
    .await;
    assert_exact_attribute_ids("PowerTopology", &attrs, &[0, 1]);
    assert_eq!(
        topo::decode_available_endpoints(attribute_tlv(
            &attrs,
            topo::attribute_id::AVAILABLE_ENDPOINTS
        ))
        .unwrap(),
        Vec::<u16>::new()
    );
    assert_eq!(
        topo::decode_active_endpoints(attribute_tlv(&attrs, topo::attribute_id::ACTIVE_ENDPOINTS))
            .unwrap(),
        Vec::<u16>::new()
    );
}

async fn decode_device_energy_management(node: &Node) {
    use dem::attribute_id as a;
    let attrs = decode_every_attribute(node, EP, dem::CLUSTER_ID, |id, t| match id {
        a::ESA_TYPE => ok(dem::decode_esa_type(t)),
        a::ESA_CAN_GENERATE => ok(dem::decode_esa_can_generate(t)),
        a::ESA_STATE => ok(dem::decode_esa_state(t)),
        a::ABS_MIN_POWER => ok(dem::decode_abs_min_power(t)),
        a::ABS_MAX_POWER => ok(dem::decode_abs_max_power(t)),
        a::POWER_ADJUSTMENT_CAPABILITY => ok(dem::decode_power_adjustment_capability(t)),
        a::FORECAST => ok(dem::decode_forecast(t)),
        a::OPT_OUT_STATE => ok(dem::decode_opt_out_state(t)),
        other => newer_than_codegen("DeviceEnergyManagement", other),
    })
    .await;
    assert_exact_attribute_ids(
        "DeviceEnergyManagement",
        &attrs,
        &(0..=7).collect::<Vec<_>>(),
    );
}

async fn decode_energy_evse(node: &Node) {
    use evse::attribute_id as a;
    let attrs = decode_every_attribute(node, EP, evse::CLUSTER_ID, |id, t| match id {
        a::STATE => ok(evse::decode_state(t)),
        a::SUPPLY_STATE => ok(evse::decode_supply_state(t)),
        a::FAULT_STATE => ok(evse::decode_fault_state(t)),
        a::CHARGING_ENABLED_UNTIL => ok(evse::decode_charging_enabled_until(t)),
        a::CIRCUIT_CAPACITY => ok(evse::decode_circuit_capacity(t)),
        a::MINIMUM_CHARGE_CURRENT => ok(evse::decode_minimum_charge_current(t)),
        a::MAXIMUM_CHARGE_CURRENT => ok(evse::decode_maximum_charge_current(t)),
        a::USER_MAXIMUM_CHARGE_CURRENT => ok(evse::decode_user_maximum_charge_current(t)),
        a::RANDOMIZATION_DELAY_WINDOW => ok(evse::decode_randomization_delay_window(t)),
        a::NEXT_CHARGE_START_TIME => ok(evse::decode_next_charge_start_time(t)),
        a::NEXT_CHARGE_TARGET_TIME => ok(evse::decode_next_charge_target_time(t)),
        a::NEXT_CHARGE_REQUIRED_ENERGY => ok(evse::decode_next_charge_required_energy(t)),
        a::NEXT_CHARGE_TARGET_SOC => ok(evse::decode_next_charge_target_soc(t)),
        a::APPROXIMATE_EV_EFFICIENCY => ok(evse::decode_approximate_ev_efficiency(t)),
        a::SESSION_ID => ok(evse::decode_session_id(t)),
        a::SESSION_DURATION => ok(evse::decode_session_duration(t)),
        a::SESSION_ENERGY_CHARGED => ok(evse::decode_session_energy_charged(t)),
        other => newer_than_codegen("EnergyEvse", other),
    })
    .await;
    let want: Vec<u32> = [0x00, 0x01, 0x02, 0x03, 0x05, 0x06, 0x07, 0x09, 0x0A]
        .into_iter()
        .chain(0x23..=0x27)
        .chain(0x40..=0x42)
        .collect();
    assert_exact_attribute_ids("EnergyEvse", &attrs, &want);
    let tlv = invoke_for_response(
        node,
        command(evse::CLUSTER_ID, evse::command_id::GET_TARGETS),
        evse::encode_get_targets(),
        evse::command_id::GET_TARGETS_RESPONSE,
    )
    .await
    .unwrap();
    evse::GetTargetsResponse::decode(&tlv).unwrap();
}
