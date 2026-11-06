// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! Energy management on a live evse-app (endpoint 1): EnergyEvse,
//! DeviceEnergyManagement and PowerTopology. Every attribute is decoded with
//! the generated decoder and the exact attribute ids served are asserted;
//! EnergyEvse's writable settings are written, refused where chip refuses
//! them, and restored; a charging session, a fault and an RFID tap are
//! stimulated and their events decoded; charging targets are set, read back
//! and cleared; DeviceEnergyManagement's power adjustment and pause run
//! through their events, and are refused where chip refuses them.
//!
//! evse-app is local only (not in the nightly), built from chip master
//! 5cd2917a: `evse-common/src/EnergyEvseMain.cpp` sets all five features
//! (PREF, SOC, PNC, RFID, V2X) and the three optional attributes, so every
//! attribute is served. The server is master's code-driven
//! `src/app/clusters/energy-evse-server/EnergyEvseCluster.cpp`; the delegate
//! and its state machine are `evse-common/src/EnergyEvseDelegateImpl.cpp`;
//! the stimuli are `GeneralDiagnostics.TestEventTrigger` codes from
//! `EnergyEvseTestEventTriggerHandler.h`, handled by
//! `evse-common/src/EnergyEvseEventTriggers.cpp` (evse-app's `args.gni`
//! enables the EVSE, DEM and energy-reporting triggers).
//!
//! DeviceEnergyManagement: `evse-app/linux/main.cpp` sets PA, PFR, STA, PAU,
//! FA and CON; the server is master's
//! `src/app/clusters/device-energy-management-server/DeviceEnergyManagementCluster.cpp`,
//! the delegate `examples/energy-management/device-energy-management/src/
//! DeviceEnergyManagementDelegateImpl.cpp` (EVSEManufacturerImpl.cpp sets it
//! Online, ESAType Evse, 1.2 kW to 7.6 kW), the triggers
//! `DeviceEnergyManagementTestEventTriggerHandler.h`, handled by
//! `energy-management-triggers/DEMTestEventTriggers.cpp`. PowerTopology:
//! NODE only (`EnergyEvseMain.cpp`), so no attribute beyond the globals.
//!
//! Every EnergyEvse command is timed (`timed=true` in the 1.4.2 XML);
//! `Node::invoke_tlv` retries a command as a timed interaction when the device
//! answers NEEDS_TIMED_INTERACTION. The tests share one evse-app process (the
//! harness runs them one at a time against it), so what one test leaves
//! behind carries over to the next: the EVSE state machine and the ESA state
//! live in the app's memory. (evse-app persists only a few values, such as
//! SessionID and the charging targets, and the harness wipes the KVS at every
//! launch, so nothing carries over between runs.) Each test therefore first
//! puts the EVSE (no fault, supply disabled, nothing plugged in) or the ESA
//! (Online, no opt-out) back where it expects it.

use integration_tests::events::{
    latest_event_number, payload_tlv, test_event_trigger, wait_for_event_after,
};
use integration_tests::sweep::{
    assert_exact_attribute_ids, attribute_tlv, decode_every_attribute, invoke_for_response,
    invoke_for_status, newer_than_codegen, ok, read_cluster_attributes, write_attribute,
};
use matter_clusters::clusters::{
    device_energy_management as dem, energy_evse as evse, power_topology,
};
use matter_clusters::types::Nullable;
use matter_controller::{AttributePath, CommandPath, ImStatus, MatterController, Node, ReadPath};

/// evse-app serves EnergyEvse, DeviceEnergyManagement and PowerTopology on
/// endpoint 1.
const EP: u16 = 1;

/// IM status codes the server answers with (Matter Core spec 8.10.1).
const FAILURE: ImStatus = ImStatus::Failure(0x01);
const INVALID_COMMAND: ImStatus = ImStatus::Failure(0x85);
const CONSTRAINT_ERROR: ImStatus = ImStatus::Failure(0x87);
const INVALID_IN_STATE: ImStatus = ImStatus::Failure(0xCB);

/// The controller and node id, or `None` (skip) unless the DUT is evse-app.
async fn connect_evse() -> Option<(MatterController, u64)> {
    let cfg = integration_tests::dut::DutConfig::from_env()?;
    if !cfg.is_app("evse") {
        eprintln!(
            "skipped: energy-management test needs the evse-app DUT (`just integration-energy`)"
        );
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((controller, node_id))
}

/// A path on [`EP`].
fn path(cluster: u32, attribute: u32) -> AttributePath {
    AttributePath {
        endpoint: EP,
        cluster,
        attribute,
    }
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

/// The cluster's FeatureMap on [`EP`].
async fn feature_map(node: &Node, cluster: u32) -> u32 {
    use matter_clusters::clusters::globals::{decode_u32, FEATURE_MAP};
    let report = node
        .read(&[ReadPath::concrete(EP, cluster, FEATURE_MAP)])
        .await
        .unwrap();
    let (_, value) = report
        .into_iter()
        .find(|(p, _)| p.attribute == FEATURE_MAP)
        .unwrap();
    decode_u32(&payload_tlv(&value)).unwrap()
}

/// The newest `(EP, cluster, event)` payload after `baseline`, as TLV.
async fn newest_event_after(
    node: &Node,
    cluster: u32,
    event: u32,
    baseline: Option<u64>,
) -> Vec<u8> {
    let items = wait_for_event_after(node, EP, cluster, event, baseline)
        .await
        .unwrap();
    let newest = items.into_iter().max_by_key(|i| i.event_number).unwrap();
    payload_tlv(&newest.value)
}

// ---- EnergyEvse -------------------------------------------------------------

/// TestEventTrigger codes (`EnergyEvseTrigger`,
/// EnergyEvseTestEventTriggerHandler.h at master 5cd2917a). v1.4.2.0's enum
/// has the same values up to kEVTimeOfUseModeClear (0x…21) and stops there:
/// RFID (0x…50), like the SoC (0x…30–32) and VehicleID (0x…40) codes, exists
/// at master only.
mod evse_trigger {
    pub(super) const BASIC: u64 = 0x0099_0000_0000_0000;
    pub(super) const BASIC_CLEAR: u64 = 0x0099_0000_0000_0001;
    pub(super) const PLUGGED_IN: u64 = 0x0099_0000_0000_0002;
    pub(super) const PLUGGED_IN_CLEAR: u64 = 0x0099_0000_0000_0003;
    pub(super) const CHARGE_DEMAND: u64 = 0x0099_0000_0000_0004;
    pub(super) const CHARGE_DEMAND_CLEAR: u64 = 0x0099_0000_0000_0005;
    pub(super) const GROUND_FAULT: u64 = 0x0099_0000_0000_0010;
    pub(super) const FAULT_CLEAR: u64 = 0x0099_0000_0000_0012;
    pub(super) const RFID: u64 = 0x0099_0000_0000_0050;
}

/// Put the EVSE where every test starts: no fault (kEVSEFaultClear; a no-op
/// when there is none), supply disabled (Disable, which refuses with Failure
/// while a fault is present), and the basic-functionality limits installed
/// (kBasicFunctionality: 32 A hardware, circuit and user limits, state
/// NotPluggedIn).
async fn reset_evse(node: &Node) {
    test_event_trigger(node, evse_trigger::FAULT_CLEAR)
        .await
        .unwrap();
    assert_eq!(
        status_of(
            node,
            evse::CLUSTER_ID,
            evse::command_id::DISABLE,
            evse::encode_disable()
        )
        .await,
        ImStatus::Success
    );
    test_event_trigger(node, evse_trigger::BASIC).await.unwrap();
}

/// Every EnergyEvse attribute decodes and evse-app serves all 23 (all five
/// features, all three optional attributes); RandomizationDelayWindow and
/// ApproximateEvEfficiency are written and restored, and
/// RandomizationDelayWindow (only it) is refused above its 86400 s limit.
#[tokio::test]
async fn energy_evse_decodes_every_attribute_and_writes_its_settings() {
    let Some((controller, node_id)) = connect_evse().await else {
        return;
    };
    let node = controller.node(node_id);
    use evse::attribute_id as a;
    reset_evse(&node).await;
    assert_eq!(feature_map(&node, evse::CLUSTER_ID).await, 0x1F);
    let attrs = decode_every_attribute(&node, EP, evse::CLUSTER_ID, |id, t| match id {
        a::STATE => ok(evse::decode_state(t)),
        a::SUPPLY_STATE => ok(evse::decode_supply_state(t)),
        a::FAULT_STATE => ok(evse::decode_fault_state(t)),
        a::CHARGING_ENABLED_UNTIL => ok(evse::decode_charging_enabled_until(t)),
        a::DISCHARGING_ENABLED_UNTIL => ok(evse::decode_discharging_enabled_until(t)),
        a::CIRCUIT_CAPACITY => ok(evse::decode_circuit_capacity(t)),
        a::MINIMUM_CHARGE_CURRENT => ok(evse::decode_minimum_charge_current(t)),
        a::MAXIMUM_CHARGE_CURRENT => ok(evse::decode_maximum_charge_current(t)),
        a::MAXIMUM_DISCHARGE_CURRENT => ok(evse::decode_maximum_discharge_current(t)),
        a::USER_MAXIMUM_CHARGE_CURRENT => ok(evse::decode_user_maximum_charge_current(t)),
        a::RANDOMIZATION_DELAY_WINDOW => ok(evse::decode_randomization_delay_window(t)),
        a::NEXT_CHARGE_START_TIME => ok(evse::decode_next_charge_start_time(t)),
        a::NEXT_CHARGE_TARGET_TIME => ok(evse::decode_next_charge_target_time(t)),
        a::NEXT_CHARGE_REQUIRED_ENERGY => ok(evse::decode_next_charge_required_energy(t)),
        a::NEXT_CHARGE_TARGET_SOC => ok(evse::decode_next_charge_target_soc(t)),
        a::APPROXIMATE_EV_EFFICIENCY => ok(evse::decode_approximate_ev_efficiency(t)),
        a::STATE_OF_CHARGE => ok(evse::decode_state_of_charge(t)),
        a::BATTERY_CAPACITY => ok(evse::decode_battery_capacity(t)),
        a::VEHICLE_ID => ok(evse::decode_vehicle_id(t)),
        a::SESSION_ID => ok(evse::decode_session_id(t)),
        a::SESSION_DURATION => ok(evse::decode_session_duration(t)),
        a::SESSION_ENERGY_CHARGED => ok(evse::decode_session_energy_charged(t)),
        a::SESSION_ENERGY_DISCHARGED => ok(evse::decode_session_energy_discharged(t)),
        other => newer_than_codegen("EnergyEvse", other),
    })
    .await;
    let want: Vec<u32> = (0x0000..=0x000A)
        .chain(0x0023..=0x0027)
        .chain(0x0030..=0x0032)
        .chain(0x0040..=0x0043)
        .collect();
    assert_exact_attribute_ids("EnergyEvse", &attrs, &want);
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        evse::decode_state(tlv(a::STATE)).unwrap(),
        Nullable::Value(evse::StateEnum::NotPluggedIn)
    );
    assert_eq!(
        evse::decode_supply_state(tlv(a::SUPPLY_STATE)).unwrap(),
        evse::SupplyStateEnum::Disabled
    );
    assert_eq!(
        evse::decode_fault_state(tlv(a::FAULT_STATE)).unwrap(),
        evse::FaultStateEnum::NoError
    );
    assert_eq!(
        evse::decode_circuit_capacity(tlv(a::CIRCUIT_CAPACITY)).unwrap(),
        32_000,
        "kBasicFunctionality's circuit capacity"
    );
    write_randomization_delay_window(&node).await;
    write_approximate_ev_efficiency(&node).await;
    test_event_trigger(&node, evse_trigger::BASIC_CLEAR)
        .await
        .unwrap();
}

/// RandomizationDelayWindow: 600 s accepted, 86401 s refused
/// (`kMaxRandomizationDelayWindowSec`), the original value restored.
async fn write_randomization_delay_window(node: &Node) {
    use evse::attribute_id::RANDOMIZATION_DELAY_WINDOW as RDW;
    let read = || async {
        evse::decode_randomization_delay_window(&read_tlv(node, evse::CLUSTER_ID, RDW).await)
            .unwrap()
    };
    let at = path(evse::CLUSTER_ID, RDW);
    let original = read().await;
    let write = |v| write_attribute(node, at, evse::encode_randomization_delay_window(v));
    assert_eq!(write(600).await.unwrap(), ImStatus::Success);
    assert_eq!(read().await, 600);
    assert_eq!(write(86_401).await.unwrap(), CONSTRAINT_ERROR);
    assert_eq!(read().await, 600, "a refused write changes nothing");
    assert_eq!(write(original).await.unwrap(), ImStatus::Success);
    assert_eq!(read().await, original, "restored");
}

/// ApproximateEvEfficiency (nullable): a value, then null, then the original.
async fn write_approximate_ev_efficiency(node: &Node) {
    use evse::attribute_id::APPROXIMATE_EV_EFFICIENCY as EFF;
    let read = || async {
        evse::decode_approximate_ev_efficiency(&read_tlv(node, evse::CLUSTER_ID, EFF).await)
            .unwrap()
    };
    let at = path(evse::CLUSTER_ID, EFF);
    let original = read().await;
    let write = |v| write_attribute(node, at, evse::encode_approximate_ev_efficiency(v));
    assert_eq!(
        write(Nullable::Value(3500)).await.unwrap(),
        ImStatus::Success
    );
    assert_eq!(read().await, Nullable::Value(3500));
    assert_eq!(write(Nullable::Null).await.unwrap(), ImStatus::Success);
    assert_eq!(read().await, Nullable::Null);
    assert_eq!(write(original).await.unwrap(), ImStatus::Success);
    assert_eq!(read().await, original, "restored");
}

/// The baseline of every EnergyEvse event this module stimulates.
async fn evse_baseline(node: &Node, event: u32) -> Option<u64> {
    latest_event_number(node, EP, evse::CLUSTER_ID, event)
        .await
        .unwrap()
}

/// A charging session, as EnergyEvseDelegateImpl.cpp's state machine runs it:
/// plug in (EvConnected, a new session: SessionID incremented, or 0 from
/// null), ask for charge while the supply is disabled (PluggedInDemand, no
/// event), EnableCharging at 40 A (PluggedInCharging, EnergyTransferStarted
/// clamped to 32 A: every other limit is 32 A), Disable
/// (EnergyTransferStopped, EvseStopped), stop asking and unplug
/// (EvNotDetected). Every event carries the same session id.
#[tokio::test]
async fn energy_evse_charging_session_emits_its_events() {
    let Some((controller, node_id)) = connect_evse().await else {
        return;
    };
    let node = controller.node(node_id);
    use evse::{event_id as e, StateEnum};
    use evse_trigger as t;
    let cl = evse::CLUSTER_ID;
    reset_evse(&node).await;

    let session_id_attr = || async {
        evse::decode_session_id(&read_tlv(&node, cl, evse::attribute_id::SESSION_ID).await).unwrap()
    };
    let before = session_id_attr().await;
    let base = evse_baseline(&node, e::EV_CONNECTED).await;
    test_event_trigger(&node, t::PLUGGED_IN).await.unwrap();
    let connected =
        evse::EvConnectedEvent::decode(&newest_event_after(&node, cl, e::EV_CONNECTED, base).await)
            .unwrap();
    let session = connected.session_id;
    // `EvseSession::StartSession`: a null SessionID becomes 0, any other
    // value is incremented (a uint32_t, so it wraps).
    let next = match before {
        Nullable::Null => 0,
        Nullable::Value(old) => old.wrapping_add(1),
    };
    assert_eq!(session, next, "a new session after {before:?}");
    assert_eq!(session_id_attr().await, Nullable::Value(session));
    test_event_trigger(&node, t::CHARGE_DEMAND).await.unwrap();
    assert_eq!(
        evse::decode_state(&read_tlv(&node, cl, evse::attribute_id::STATE).await).unwrap(),
        Nullable::Value(StateEnum::PluggedInDemand),
        "demand with the supply disabled starts nothing"
    );

    // 40 A asked; `ComputeMaxChargeCurrentLimit` takes the minimum of it and
    // kBasicFunctionality's 32 A hardware, circuit and user limits (and the
    // 63 A cable kEVPluggedIn installs), so the transfer starts at 32 A.
    let base = evse_baseline(&node, e::ENERGY_TRANSFER_STARTED).await;
    let enable = evse::encode_enable_charging(Nullable::Null, 6_000, 40_000);
    assert_eq!(
        status_of(&node, cl, evse::command_id::ENABLE_CHARGING, enable).await,
        ImStatus::Success
    );
    let started = evse::EnergyTransferStartedEvent::decode(
        &newest_event_after(&node, cl, e::ENERGY_TRANSFER_STARTED, base).await,
    )
    .unwrap();
    assert_eq!(
        (started.session_id, started.state, started.maximum_current),
        (session, StateEnum::PluggedInCharging, 32_000),
        "clamped to the 32 A limits, not the 40 A asked"
    );
    assert!(
        started.maximum_discharge_current.is_some(),
        "V2X reports it"
    );

    let base = evse_baseline(&node, e::ENERGY_TRANSFER_STOPPED).await;
    assert_eq!(
        status_of(&node, cl, evse::command_id::DISABLE, evse::encode_disable()).await,
        ImStatus::Success
    );
    let stopped = evse::EnergyTransferStoppedEvent::decode(
        &newest_event_after(&node, cl, e::ENERGY_TRANSFER_STOPPED, base).await,
    )
    .unwrap();
    assert_eq!(
        (stopped.session_id, stopped.reason),
        (session, evse::EnergyTransferStoppedReasonEnum::EvseStopped)
    );

    let base = evse_baseline(&node, e::EV_NOT_DETECTED).await;
    test_event_trigger(&node, t::CHARGE_DEMAND_CLEAR)
        .await
        .unwrap();
    test_event_trigger(&node, t::PLUGGED_IN_CLEAR)
        .await
        .unwrap();
    let gone = evse::EvNotDetectedEvent::decode(
        &newest_event_after(&node, cl, e::EV_NOT_DETECTED, base).await,
    )
    .unwrap();
    assert_eq!(
        (gone.session_id, gone.state),
        (session, StateEnum::PluggedInNoDemand)
    );
    assert!(gone.session_energy_discharged.is_some(), "V2X reports it");
    test_event_trigger(&node, t::BASIC_CLEAR).await.unwrap();
}

/// A ground fault: the Fault event (NoError → GroundFault), State Fault and
/// SupplyState DisabledError; EnableCharging refused with Failure while it
/// lasts (`CheckFaultOrDiagnostic`); clearing it emits Fault again
/// (GroundFault → NoError). Then an RFID tap: kEVSETriggerRFID's ten-byte
/// UID in an Rfid event.
#[tokio::test]
async fn energy_evse_fault_and_rfid_emit_their_events() {
    let Some((controller, node_id)) = connect_evse().await else {
        return;
    };
    let node = controller.node(node_id);
    use evse::{event_id as e, FaultStateEnum as F};
    let cl = evse::CLUSTER_ID;
    reset_evse(&node).await;

    let base = evse_baseline(&node, e::FAULT).await;
    test_event_trigger(&node, evse_trigger::GROUND_FAULT)
        .await
        .unwrap();
    let raised =
        evse::FaultEvent::decode(&newest_event_after(&node, cl, e::FAULT, base).await).unwrap();
    assert_eq!(
        (
            raised.state,
            raised.fault_state_previous_state,
            raised.fault_state_current_state
        ),
        (evse::StateEnum::NotPluggedIn, F::NoError, F::GroundFault)
    );
    let attrs = read_cluster_attributes(&node, EP, cl).await.unwrap();
    use evse::attribute_id as a;
    assert_eq!(
        evse::decode_state(attribute_tlv(&attrs, a::STATE)).unwrap(),
        Nullable::Value(evse::StateEnum::Fault)
    );
    assert_eq!(
        evse::decode_supply_state(attribute_tlv(&attrs, a::SUPPLY_STATE)).unwrap(),
        evse::SupplyStateEnum::DisabledError
    );
    let enable = evse::encode_enable_charging(Nullable::Null, 6_000, 32_000);
    assert_eq!(
        status_of(&node, cl, evse::command_id::ENABLE_CHARGING, enable).await,
        FAILURE
    );

    let base = evse_baseline(&node, e::FAULT).await;
    test_event_trigger(&node, evse_trigger::FAULT_CLEAR)
        .await
        .unwrap();
    let cleared =
        evse::FaultEvent::decode(&newest_event_after(&node, cl, e::FAULT, base).await).unwrap();
    assert_eq!(
        (
            cleared.state,
            cleared.fault_state_previous_state,
            cleared.fault_state_current_state
        ),
        (evse::StateEnum::Fault, F::GroundFault, F::NoError)
    );

    let base = evse_baseline(&node, e::RFID).await;
    test_event_trigger(&node, evse_trigger::RFID).await.unwrap();
    let rfid =
        evse::RfidEvent::decode(&newest_event_after(&node, cl, e::RFID, base).await).unwrap();
    assert_eq!(
        rfid.uid,
        [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99]
    );
    test_event_trigger(&node, evse_trigger::BASIC_CLEAR)
        .await
        .unwrap();
}

/// GetTargets on [`EP`], decoded.
async fn get_targets(node: &Node) -> Vec<evse::ChargingTargetScheduleStruct> {
    let tlv = invoke_for_response(
        node,
        command(evse::CLUSTER_ID, evse::command_id::GET_TARGETS),
        evse::encode_get_targets(),
        evse::command_id::GET_TARGETS_RESPONSE,
    )
    .await
    .unwrap();
    evse::GetTargetsResponse::decode(&tlv)
        .unwrap()
        .charging_target_schedules
}

/// SetTargets stores a weekday schedule and GetTargets returns it as sent
/// (EnergyEvseTargetsStore.cpp keeps the order and the optional fields);
/// ClearTargets empties it. With SOC every target must carry TargetSoC:
/// master's `EnergyEvseCluster::ValidateTargets` refuses one that does not
/// with InvalidCommand (v1.4.2.0's server answers Failure).
#[tokio::test]
async fn energy_evse_targets_round_trip_and_clear() {
    let Some((controller, node_id)) = connect_evse().await else {
        return;
    };
    let node = controller.node(node_id);
    use evse::{
        command_id as c, ChargingTargetScheduleStruct, ChargingTargetStruct,
        TargetDayOfWeekBitmap as D,
    };
    let cl = evse::CLUSTER_ID;
    assert_eq!(
        status_of(&node, cl, c::CLEAR_TARGETS, evse::encode_clear_targets()).await,
        ImStatus::Success
    );
    assert_eq!(get_targets(&node).await, []);

    let weekdays = vec![ChargingTargetScheduleStruct {
        day_of_week_for_sequence: D::MONDAY | D::TUESDAY | D::WEDNESDAY | D::THURSDAY | D::FRIDAY,
        charging_targets: vec![
            ChargingTargetStruct {
                target_time_minutes_past_midnight: 450,
                target_soc: Some(80),
                added_energy: None,
            },
            ChargingTargetStruct {
                target_time_minutes_past_midnight: 1200,
                target_soc: Some(100),
                added_energy: Some(25_000_000),
            },
        ],
    }];
    assert_eq!(
        status_of(
            &node,
            cl,
            c::SET_TARGETS,
            evse::encode_set_targets(&weekdays)
        )
        .await,
        ImStatus::Success
    );
    assert_eq!(get_targets(&node).await, weekdays);

    let no_soc = vec![ChargingTargetScheduleStruct {
        day_of_week_for_sequence: D::SATURDAY,
        charging_targets: vec![ChargingTargetStruct {
            target_time_minutes_past_midnight: 600,
            target_soc: None,
            added_energy: Some(10_000_000),
        }],
    }];
    assert_eq!(
        status_of(&node, cl, c::SET_TARGETS, evse::encode_set_targets(&no_soc)).await,
        INVALID_COMMAND
    );
    assert_eq!(
        get_targets(&node).await,
        weekdays,
        "a refused SetTargets changes nothing"
    );

    assert_eq!(
        status_of(&node, cl, c::CLEAR_TARGETS, evse::encode_clear_targets()).await,
        ImStatus::Success
    );
    assert_eq!(get_targets(&node).await, []);
}

// ---- DeviceEnergyManagement --------------------------------------------------

/// TestEventTrigger codes (`DeviceEnergyManagementTrigger`,
/// DeviceEnergyManagementTestEventTriggerHandler.h; the same values at
/// master 5cd2917a and v1.4.2.0).
mod dem_trigger {
    /// A PowerAdjustmentCapability of 5 kW to 30 kW for 10 s to 60 s.
    pub(super) const POWER_ADJUSTMENT: u64 = 0x0098_0000_0000_0000;
    /// The capability's range set to zero (`SetTestEventTrigger_ClearForecast`).
    pub(super) const POWER_ADJUSTMENT_CLEAR: u64 = 0x0098_0000_0000_0001;
    pub(super) const USER_OPT_OUT_LOCAL: u64 = 0x0098_0000_0000_0002;
    pub(super) const USER_OPT_OUT_CLEAR_ALL: u64 = 0x0098_0000_0000_0004;
    /// A two-slot forecast whose active slot 0 is pausable for 10 s to 60 s.
    pub(super) const PAUSABLE: u64 = 0x0098_0000_0000_0007;
    /// DEMTestEventTriggers.cpp handles it with
    /// `SetTestEventTrigger_ClearForecast` too: it zeroes the power
    /// adjustment capability and leaves the pausable forecast in place.
    pub(super) const PAUSABLE_CLEAR: u64 = 0x0098_0000_0000_0009;
}

/// DeviceEnergyManagement's ESAState.
async fn esa_state(node: &Node) -> dem::ESAStateEnum {
    let tlv = read_tlv(node, dem::CLUSTER_ID, dem::attribute_id::ESA_STATE).await;
    dem::decode_esa_state(&tlv).unwrap()
}

/// Put the ESA back Online with no opt-out: end an adjustment or a pause an
/// earlier run left behind.
async fn reset_dem(node: &Node) {
    use dem::command_id as c;
    test_event_trigger(node, dem_trigger::USER_OPT_OUT_CLEAR_ALL)
        .await
        .unwrap();
    match esa_state(node).await {
        dem::ESAStateEnum::PowerAdjustActive => assert_eq!(
            status_of(
                node,
                dem::CLUSTER_ID,
                c::CANCEL_POWER_ADJUST_REQUEST,
                dem::encode_cancel_power_adjust_request()
            )
            .await,
            ImStatus::Success
        ),
        dem::ESAStateEnum::Paused => assert_eq!(
            status_of(
                node,
                dem::CLUSTER_ID,
                c::RESUME_REQUEST,
                dem::encode_resume_request()
            )
            .await,
            ImStatus::Success
        ),
        _ => {}
    }
    assert_eq!(esa_state(node).await, dem::ESAStateEnum::Online);
}

/// Every DeviceEnergyManagement attribute decodes; evse-app serves all eight
/// (features 0x7B) with EVSEManufacturerImpl.cpp's values.
#[tokio::test]
async fn device_energy_management_decodes_every_attribute() {
    let Some((controller, node_id)) = connect_evse().await else {
        return;
    };
    let node = controller.node(node_id);
    use dem::attribute_id as a;
    reset_dem(&node).await;
    assert_eq!(feature_map(&node, dem::CLUSTER_ID).await, 0x7B);
    let attrs = decode_every_attribute(&node, EP, dem::CLUSTER_ID, |id, t| match id {
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
        &(0x0000..=0x0007).collect::<Vec<_>>(),
    );
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        dem::decode_esa_type(tlv(a::ESA_TYPE)).unwrap(),
        dem::ESATypeEnum::Evse
    );
    assert!(!dem::decode_esa_can_generate(tlv(a::ESA_CAN_GENERATE)).unwrap());
    assert_eq!(
        (
            dem::decode_abs_min_power(tlv(a::ABS_MIN_POWER)).unwrap(),
            dem::decode_abs_max_power(tlv(a::ABS_MAX_POWER)).unwrap()
        ),
        (1_200_000, 7_600_000)
    );
    assert_eq!(
        dem::decode_opt_out_state(tlv(a::OPT_OUT_STATE)).unwrap(),
        dem::OptOutStateEnum::NoOptOut
    );
}

/// A power adjustment within the trigger's capability starts
/// (PowerAdjustStart; ESAState PowerAdjustActive; the capability's cause
/// LocalOptimizationAdjustment) and CancelPowerAdjustRequest ends it
/// (PowerAdjustEnd, Cancelled). Refused: a power outside the capability
/// (ConstraintError), a local adjustment after a local opt-out
/// (ConstraintError, `CheckOptOutAllowsRequest`), a cancel with nothing to
/// cancel (InvalidInState).
#[tokio::test]
async fn device_energy_management_power_adjustment_starts_and_cancels() {
    let Some((controller, node_id)) = connect_evse().await else {
        return;
    };
    let node = controller.node(node_id);
    use dem::{command_id as c, event_id as e, AdjustmentCauseEnum as Cause};
    let cl = dem::CLUSTER_ID;
    reset_dem(&node).await;
    test_event_trigger(&node, dem_trigger::POWER_ADJUSTMENT)
        .await
        .unwrap();
    let adjust = |power| dem::encode_power_adjust_request(power, 30, Cause::LocalOptimization);
    assert_eq!(
        status_of(&node, cl, c::POWER_ADJUST_REQUEST, adjust(1_000_000)).await,
        CONSTRAINT_ERROR,
        "1 kW is below the 5 kW minimum"
    );
    test_event_trigger(&node, dem_trigger::USER_OPT_OUT_LOCAL)
        .await
        .unwrap();
    assert_eq!(
        status_of(&node, cl, c::POWER_ADJUST_REQUEST, adjust(10_000_000)).await,
        CONSTRAINT_ERROR,
        "the user opted out of local optimization"
    );
    test_event_trigger(&node, dem_trigger::USER_OPT_OUT_CLEAR_ALL)
        .await
        .unwrap();

    let base = latest_event_number(&node, EP, cl, e::POWER_ADJUST_START)
        .await
        .unwrap();
    assert_eq!(
        status_of(&node, cl, c::POWER_ADJUST_REQUEST, adjust(10_000_000)).await,
        ImStatus::Success
    );
    assert_eq!(
        newest_event_after(&node, cl, e::POWER_ADJUST_START, base).await,
        [0x15, 0x18]
    );
    assert_eq!(esa_state(&node).await, dem::ESAStateEnum::PowerAdjustActive);
    let cap = dem::decode_power_adjustment_capability(
        &read_tlv(&node, cl, dem::attribute_id::POWER_ADJUSTMENT_CAPABILITY).await,
    )
    .unwrap();
    let Nullable::Value(cap) = cap else {
        panic!("the trigger set a capability")
    };
    assert_eq!(
        cap.cause,
        dem::PowerAdjustReasonEnum::LocalOptimizationAdjustment
    );

    let base = latest_event_number(&node, EP, cl, e::POWER_ADJUST_END)
        .await
        .unwrap();
    assert_eq!(
        status_of(
            &node,
            cl,
            c::CANCEL_POWER_ADJUST_REQUEST,
            dem::encode_cancel_power_adjust_request()
        )
        .await,
        ImStatus::Success
    );
    let end = dem::PowerAdjustEndEvent::decode(
        &newest_event_after(&node, cl, e::POWER_ADJUST_END, base).await,
    )
    .unwrap();
    assert_eq!(end.cause, dem::CauseEnum::Cancelled);
    assert!(end.duration <= 30, "it ran at most its 30 s: {end:?}");
    assert_eq!(esa_state(&node).await, dem::ESAStateEnum::Online);
    assert_eq!(
        status_of(
            &node,
            cl,
            c::CANCEL_POWER_ADJUST_REQUEST,
            dem::encode_cancel_power_adjust_request()
        )
        .await,
        INVALID_IN_STATE,
        "nothing left to cancel"
    );
    test_event_trigger(&node, dem_trigger::POWER_ADJUSTMENT_CLEAR)
        .await
        .unwrap();
}

/// A pause within the active slot's 10 s to 60 s range pauses the ESA
/// (Paused; ESAState Paused; the forecast's update reason LocalOptimization)
/// and ResumeRequest resumes it (Resumed, Cancelled; ESAState Online; reason
/// InternalOptimization). Refused: a 5 s pause (ConstraintError), a resume
/// with nothing paused (InvalidInState).
#[tokio::test]
async fn device_energy_management_pause_and_resume() {
    let Some((controller, node_id)) = connect_evse().await else {
        return;
    };
    let node = controller.node(node_id);
    use dem::{command_id as c, event_id as e, AdjustmentCauseEnum as Cause};
    let cl = dem::CLUSTER_ID;
    reset_dem(&node).await;
    test_event_trigger(&node, dem_trigger::PAUSABLE)
        .await
        .unwrap();
    assert_eq!(
        status_of(
            &node,
            cl,
            c::PAUSE_REQUEST,
            dem::encode_pause_request(5, Cause::LocalOptimization)
        )
        .await,
        CONSTRAINT_ERROR,
        "below the slot's 10 s MinPauseDuration"
    );

    let base = latest_event_number(&node, EP, cl, e::PAUSED).await.unwrap();
    assert_eq!(
        status_of(
            &node,
            cl,
            c::PAUSE_REQUEST,
            dem::encode_pause_request(30, Cause::LocalOptimization)
        )
        .await,
        ImStatus::Success
    );
    assert_eq!(
        newest_event_after(&node, cl, e::PAUSED, base).await,
        [0x15, 0x18]
    );
    assert_eq!(esa_state(&node).await, dem::ESAStateEnum::Paused);
    assert_eq!(
        forecast_update_reason(&node).await,
        dem::ForecastUpdateReasonEnum::LocalOptimization
    );

    let base = latest_event_number(&node, EP, cl, e::RESUMED)
        .await
        .unwrap();
    assert_eq!(
        status_of(&node, cl, c::RESUME_REQUEST, dem::encode_resume_request()).await,
        ImStatus::Success
    );
    let resumed =
        dem::ResumedEvent::decode(&newest_event_after(&node, cl, e::RESUMED, base).await).unwrap();
    assert_eq!(resumed.cause, dem::CauseEnum::Cancelled);
    assert_eq!(esa_state(&node).await, dem::ESAStateEnum::Online);
    assert_eq!(
        forecast_update_reason(&node).await,
        dem::ForecastUpdateReasonEnum::InternalOptimization
    );
    assert_eq!(
        status_of(&node, cl, c::RESUME_REQUEST, dem::encode_resume_request()).await,
        INVALID_IN_STATE,
        "nothing paused"
    );
    test_event_trigger(&node, dem_trigger::PAUSABLE_CLEAR)
        .await
        .unwrap();
}

/// The Forecast's ForecastUpdateReason (the pausable trigger set a forecast).
async fn forecast_update_reason(node: &Node) -> dem::ForecastUpdateReasonEnum {
    let tlv = read_tlv(node, dem::CLUSTER_ID, dem::attribute_id::FORECAST).await;
    match dem::decode_forecast(&tlv).unwrap() {
        Nullable::Value(f) => f.forecast_update_reason,
        Nullable::Null => panic!("no forecast"),
    }
}

// ---- PowerTopology -----------------------------------------------------------

/// evse-app's PowerTopology has the NODE feature only, so it serves neither
/// AvailableEndpoints (SET) nor ActiveEndpoints (DYPF): a wildcard read
/// returns no attribute beyond the globals, and AttributeList names exactly
/// the five globals evse-app.matter lists for it (no EventList).
#[tokio::test]
async fn power_topology_serves_node_topology_only() {
    let Some((controller, node_id)) = connect_evse().await else {
        return;
    };
    let node = controller.node(node_id);
    assert_eq!(
        feature_map(&node, power_topology::CLUSTER_ID).await,
        power_topology::Feature::NODE.bits()
    );
    let attrs = read_cluster_attributes(&node, EP, power_topology::CLUSTER_ID)
        .await
        .unwrap();
    assert_exact_attribute_ids("PowerTopology", &attrs, &[]);

    use matter_clusters::clusters::globals::ATTRIBUTE_LIST;
    let report = node
        .read(&[ReadPath::concrete(
            EP,
            power_topology::CLUSTER_ID,
            ATTRIBUTE_LIST,
        )])
        .await
        .unwrap();
    let (_, list) = report
        .into_iter()
        .find(|(p, _)| p.attribute == ATTRIBUTE_LIST)
        .unwrap();
    let matter_codec::Value::Array(entries) = list else {
        panic!("AttributeList is not a list: {list:?}");
    };
    let mut ids: Vec<u64> = entries
        .iter()
        .map(|e| match e {
            matter_codec::Value::Uint(id) => *id,
            other => panic!("AttributeList entry is not an unsigned int: {other:?}"),
        })
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        [0xFFF8, 0xFFF9, 0xFFFB, 0xFFFC, 0xFFFD],
        "PowerTopology AttributeList"
    );
}
