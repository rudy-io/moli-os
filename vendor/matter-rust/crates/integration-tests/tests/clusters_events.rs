// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! The events of the clusters generated before 0.6.0, read from a live
//! all-clusters-app and decoded with the generated `<Name>Event` decoders.
//!
//! Stimulus per event (chip's all-clusters, master and v1.4.2.0 alike unless
//! noted):
//! - BasicInformation `StartUp`, GeneralDiagnostics `BootReason`: emitted at boot.
//! - GeneralDiagnostics `HardwareFaultChange` / `RadioFaultChange` /
//!   `NetworkFaultChange`: app pipe `{"Name": "<event>"}`
//!   (AllClustersCommandDelegate.cpp `OnGeneralFaultEventHandler`).
//! - OccupancySensing `OccupancyChanged`: app pipe `SetOccupancy` (ep1).
//! - BooleanState `StateChange`: app pipe `SetBooleanState` (ep1) — master
//!   only; v1.4.2.0 has no such command, so its presence is not required.
//! - AccessControl `AccessControlEntryChanged` / `AccessControlExtensionChanged`:
//!   our own ACL / Extension writes (access-control-cluster.cpp `OnEntryChanged`).
//! - TimeSynchronization: `SetTimeZone` may emit `TimeZoneStatus`; whatever is
//!   reported is decoded, nothing is required.
//! - PowerSource, PumpConfigurationAndControl, OtaSoftwareUpdateRequestor: no
//!   stimulus exists in all-clusters; anything reported is decoded.

use matter_clusters::clusters::{
    access_control, basic_information, boolean_state, general_diagnostics, occupancy_sensing,
    ota_software_update_requestor, power_source, pump_configuration_and_control,
    time_synchronization,
};
use matter_clusters::types::Nullable;
use matter_codec::Tag;
use matter_controller::{
    AclAuthMode, AclEntry, AclPrivilege, AttributePath, EventPath, ImStatus, Node, TimeZoneEntry,
    Value,
};

use integration_tests::events::{
    all_clusters_pipe_supports, latest_event_number, payload_tlv, read_event_items, send_app_pipe,
    wait_for_event, wait_for_event_after, EVENT_TIMEOUT,
};
use std::time::{Duration, Instant};

/// Our controller's operational node id: the fixture creates its fabric with
/// `FabricConfig::new(1, 1, 1, ..)` (commissioner node id 1,
/// `crates/integration-tests/src/fixture.rs`), and chip reports the CASE
/// subject of an ACL write as the event's `AdminNodeID`.
const FIXTURE_CONTROLLER_NODE_ID: u64 = 1;

/// Decode every event currently readable on `(endpoint, cluster)` with
/// `decode(event_id, payload_tlv)`, panicking on the first failure. Returns
/// how many were decoded, for the log.
async fn decode_every_event(
    node: &Node,
    endpoint: u16,
    cluster: u32,
    decode: impl Fn(u32, &[u8]) -> Result<(), String>,
) -> usize {
    let items = read_event_items(node, EventPath::cluster(endpoint, cluster))
        .await
        .expect("read cluster events");
    for item in &items {
        let id = item.path.event.expect("concrete event id in report");
        decode(id, &payload_tlv(&item.value)).unwrap_or_else(|e| {
            panic!("cluster {cluster:#06x} event {id:#04x} failed to decode: {e}; {item:?}")
        });
    }
    eprintln!(
        "[events] ep{endpoint} cluster {cluster:#06x}: decoded {} event(s)",
        items.len()
    );
    items.len()
}

/// An event id the 1.4 codegen does not know (a 1.5-era chip may emit one):
/// skipped with a log line, never a failure — the generic `Value` path still
/// carries it.
#[allow(clippy::unnecessary_wraps)] // matches the decode-closure signature
fn newer_than_codegen(cluster: &str, id: u32) -> Result<(), String> {
    eprintln!("[events] {cluster}: skipping event id {id:#04x} unknown to the 1.4 codegen");
    Ok(())
}

/// Map a decode result to the `decode_every_event` shape.
fn ok<T>(r: Result<T, matter_clusters::error::ClusterError>) -> Result<(), String> {
    r.map(|_| ()).map_err(|e| e.to_string())
}

/// A fieldless event carries an empty structure; anything else is a decode bug.
fn fieldless(tlv: &[u8]) -> Result<(), String> {
    if tlv == [0x15, 0x18] {
        Ok(())
    } else {
        Err(format!("expected an empty structure, got {tlv:02x?}"))
    }
}

/// The payload TLV of the most recent event on `(0, cluster, event)`.
async fn last_event_tlv(node: &Node, cluster: u32, event: u32) -> Vec<u8> {
    let items = wait_for_event(node, 0, cluster, event)
        .await
        .unwrap_or_else(|e| panic!("{cluster:#06x}/{event:#04x}: {e}"));
    let last = items
        .iter()
        .max_by_key(|i| i.event_number)
        .expect("wait_for_event returns at least one event");
    payload_tlv(&last.value)
}

/// BasicInformation + GeneralDiagnostics (ep0): boot events and the three
/// fault-change events.
#[tokio::test]
async fn basic_information_and_general_diagnostics_events_decode() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: event test needs the all-clusters DUT (`just integration`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // ── BasicInformation (ep0): StartUp is emitted at boot. ──────────────────
    use basic_information::event_id as bi;
    let startup = wait_for_event(&node, 0, basic_information::CLUSTER_ID, bi::START_UP)
        .await
        .expect("StartUp");
    basic_information::StartUpEvent::decode(&payload_tlv(&startup[0].value))
        .expect("StartUp decodes");
    decode_every_event(&node, 0, basic_information::CLUSTER_ID, |id, t| match id {
        bi::START_UP => ok(basic_information::StartUpEvent::decode(t)),
        bi::SHUT_DOWN => fieldless(t),
        bi::LEAVE => ok(basic_information::LeaveEvent::decode(t)),
        bi::REACHABLE_CHANGED => ok(basic_information::ReachableChangedEvent::decode(t)),
        other => newer_than_codegen("BasicInformation", other),
    })
    .await;

    // ── GeneralDiagnostics (ep0): the three fault-change events via the pipe.
    use general_diagnostics::event_id as gd;
    for (name, id) in [
        ("HardwareFaultChange", gd::HARDWARE_FAULT_CHANGE),
        ("RadioFaultChange", gd::RADIO_FAULT_CHANGE),
        ("NetworkFaultChange", gd::NETWORK_FAULT_CHANGE),
    ] {
        send_app_pipe(&cfg, &format!(r#"{{"Name": "{name}"}}"#))
            .await
            .expect("app pipe");
        wait_for_event(&node, 0, general_diagnostics::CLUSTER_ID, id)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // The exact lists chip injects (AllClustersCommandDelegate.cpp
    // `OnGeneralFaultEventHandler`, identical on master and v1.4.2.0).
    use general_diagnostics::{
        HardwareFaultEnum as Hw, NetworkFaultEnum as Net, RadioFaultEnum as Rf,
    };
    let hw = general_diagnostics::HardwareFaultChangeEvent::decode(
        &last_event_tlv(
            &node,
            general_diagnostics::CLUSTER_ID,
            gd::HARDWARE_FAULT_CHANGE,
        )
        .await,
    )
    .expect("HardwareFaultChange decodes");
    assert_eq!(hw.current, [1, 2, 5, 8].map(Hw::from_raw));
    assert_eq!(hw.previous, [1, 5].map(Hw::from_raw));
    let rf = general_diagnostics::RadioFaultChangeEvent::decode(
        &last_event_tlv(
            &node,
            general_diagnostics::CLUSTER_ID,
            gd::RADIO_FAULT_CHANGE,
        )
        .await,
    )
    .expect("RadioFaultChange decodes");
    assert_eq!(rf.current, [1, 2, 3, 4].map(Rf::from_raw));
    assert_eq!(rf.previous, [1, 3].map(Rf::from_raw));
    let net = general_diagnostics::NetworkFaultChangeEvent::decode(
        &last_event_tlv(
            &node,
            general_diagnostics::CLUSTER_ID,
            gd::NETWORK_FAULT_CHANGE,
        )
        .await,
    )
    .expect("NetworkFaultChange decodes");
    assert_eq!(net.current, [1, 2, 3].map(Net::from_raw));
    assert_eq!(net.previous, [1, 2].map(Net::from_raw));
    decode_every_event(
        &node,
        0,
        general_diagnostics::CLUSTER_ID,
        |id, t| match id {
            gd::HARDWARE_FAULT_CHANGE => {
                ok(general_diagnostics::HardwareFaultChangeEvent::decode(t))
            }
            gd::RADIO_FAULT_CHANGE => ok(general_diagnostics::RadioFaultChangeEvent::decode(t)),
            gd::NETWORK_FAULT_CHANGE => ok(general_diagnostics::NetworkFaultChangeEvent::decode(t)),
            gd::BOOT_REASON => ok(general_diagnostics::BootReasonEvent::decode(t)),
            other => newer_than_codegen("GeneralDiagnostics", other),
        },
    )
    .await;
}

/// OccupancySensing + BooleanState (ep1), stimulated through the app pipe.
#[tokio::test]
async fn occupancy_and_boolean_state_events_decode() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: event test needs the all-clusters DUT (`just integration`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // ── OccupancySensing (ep1): SetOccupancy 0 then 1. chip applies
    //    "occupied" at once but delays "unoccupied" by HoldTime (30 s default;
    //    OccupancySensingCluster::SetOccupancy starts a timer, and only
    //    TimerFired emits the event). So the 0 only arms a timer and the 1
    //    cancels it. The DUT boots unoccupied, so the 1 is a change and emits
    //    OccupancyChanged with the Occupied bit set.
    use occupancy_sensing::event_id::OCCUPANCY_CHANGED;
    let occ_baseline =
        latest_event_number(&node, 1, occupancy_sensing::CLUSTER_ID, OCCUPANCY_CHANGED)
            .await
            .expect("OccupancyChanged baseline");
    for occupancy in [0, 1] {
        send_app_pipe(
            &cfg,
            &format!(r#"{{"Name": "SetOccupancy", "EndpointId": 1, "Occupancy": {occupancy}}}"#),
        )
        .await
        .expect("app pipe");
    }
    let occ = wait_for_event_after(
        &node,
        1,
        occupancy_sensing::CLUSTER_ID,
        OCCUPANCY_CHANGED,
        occ_baseline,
    )
    .await
    .expect("OccupancyChanged");
    let latest = occ
        .iter()
        .max_by_key(|i| i.event_number)
        .expect("wait_for_event_after returns at least one event");
    let latest = occupancy_sensing::OccupancyChangedEvent::decode(&payload_tlv(&latest.value))
        .expect("OccupancyChanged decodes");
    assert!(
        latest
            .occupancy
            .contains(occupancy_sensing::OccupancyBitmap::OCCUPIED),
        "latest OccupancyChanged after SetOccupancy 1 must be occupied: {latest:?}"
    );

    // ── BooleanState (ep1): SetBooleanState false then true, only where the
    //    app has that pipe command (master; not v1.4.2.0, the nightly's pin).
    //    An unknown pipe command aborts the DUT, so it is never sent blind.
    //    BooleanStateCluster::SetStateValue emits StateChange only on a
    //    change, and false-then-true ends on a change whatever the boot state,
    //    so the newest StateChange after the baseline must say true.
    use boolean_state::event_id::STATE_CHANGE;
    if all_clusters_pipe_supports(&cfg, "SetBooleanState").expect("pipe dispatch probe") {
        let baseline = latest_event_number(&node, 1, boolean_state::CLUSTER_ID, STATE_CHANGE)
            .await
            .expect("StateChange baseline");
        for state in [false, true] {
            send_app_pipe(
                &cfg,
                &format!(r#"{{"Name": "SetBooleanState", "EndpointId": 1, "NewState": {state}}}"#),
            )
            .await
            .expect("app pipe");
        }
        wait_for_latest_state_change_true(&node, baseline).await;
    } else {
        eprintln!(
            "[events] BooleanState: this chip's all-clusters app has no SetBooleanState \
             pipe command (v1.4.2.0); decoding only events already present"
        );
    }
    decode_every_event(&node, 1, boolean_state::CLUSTER_ID, |id, t| match id {
        boolean_state::event_id::STATE_CHANGE => ok(boolean_state::StateChangeEvent::decode(t)),
        other => newer_than_codegen("BooleanState", other),
    })
    .await;
}

/// Poll BooleanState `StateChange` on ep1 until the newest event after
/// `baseline` decodes `state_value == true`, failing after [`EVENT_TIMEOUT`].
/// Taking the first new event could catch the `false` StateChange before the
/// `true` one is logged; polling for the final state cannot.
async fn wait_for_latest_state_change_true(node: &Node, baseline: Option<u64>) {
    use boolean_state::event_id::STATE_CHANGE;
    let deadline = Instant::now() + EVENT_TIMEOUT;
    loop {
        let mut items = read_event_items(
            node,
            EventPath::concrete(1, boolean_state::CLUSTER_ID, STATE_CHANGE),
        )
        .await
        .expect("read StateChange");
        items.retain(|i| baseline.is_none_or(|b| i.event_number > b));
        let latest = items.iter().max_by_key(|i| i.event_number).map(|i| {
            boolean_state::StateChangeEvent::decode(&payload_tlv(&i.value))
                .expect("StateChange decodes")
        });
        if latest.as_ref().is_some_and(|e| e.state_value) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no StateChange with state_value true after event number {baseline:?} within \
             {EVENT_TIMEOUT:?}; newest after the baseline: {latest:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Write one AccessControl Extension entry and clear it again, then wait for
/// the AccessControlExtensionChanged event that caused.
async fn write_and_clear_extension(node: &Node) {
    use access_control::event_id as ac;
    // Extension (EXTS): write one entry (Data = an empty anonymous TLV list,
    // the only shape chip's CheckExtensionEntryDataFormat accepts), then clear.
    let extension_path = AttributePath {
        endpoint: 0,
        cluster: access_control::CLUSTER_ID,
        attribute: access_control::attribute_id::EXTENSION,
    };
    let ext_baseline = latest_event_number(
        node,
        0,
        access_control::CLUSTER_ID,
        ac::ACCESS_CONTROL_EXTENSION_CHANGED,
    )
    .await
    .expect("AccessControlExtensionChanged baseline");
    let one_entry = Value::Array(vec![Value::Structure(vec![(
        Tag::Context(1),
        Value::Bytes(vec![0x17, 0x18]),
    )])]);
    for (value, what) in [(one_entry, "write"), (Value::Array(vec![]), "clear")] {
        let statuses = node
            .write(&[(extension_path, value)])
            .await
            .unwrap_or_else(|e| panic!("Extension {what}: {e:?}"));
        assert!(
            statuses.iter().all(|(_, s)| matches!(s, ImStatus::Success)),
            "Extension {what} statuses: {statuses:?}"
        );
    }
    wait_for_event_after(
        node,
        0,
        access_control::CLUSTER_ID,
        ac::ACCESS_CONTROL_EXTENSION_CHANGED,
        ext_baseline,
    )
    .await
    .expect("AccessControlExtensionChanged");
}

/// AccessControl (ep0): our own ACL and Extension writes emit the events.
#[tokio::test]
async fn access_control_events_decode() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: event test needs the all-clusters DUT (`just integration`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // ── AccessControl (ep0): our own ACL write → AccessControlEntryChanged. ─
    // The event log already holds commissioning's Added event: chip's AddNOC
    // creates the admin entry under the PASE subject
    // (OperationalCredentialsCluster.cpp `CreateEntry(&subjectDescriptor, ..)`
    // → access-control-cluster.cpp `OnEntryChanged`), so it carries
    // AdminPasscodeID and a null AdminNodeID. Only events numbered after this
    // baseline are ours.
    use access_control::event_id as ac;
    let baseline = latest_event_number(
        &node,
        0,
        access_control::CLUSTER_ID,
        ac::ACCESS_CONTROL_ENTRY_CHANGED,
    )
    .await
    .expect("AccessControlEntryChanged baseline");
    let acl_before = node.read_acl().await.expect("read_acl");
    let mut acl = acl_before.clone();
    acl.push(AclEntry::new(
        AclPrivilege::View,
        AclAuthMode::Case,
        Some(vec![0x0000_0000_0000_BEEF]),
        None,
    ));
    // `write_acl` is Ok even when the device rejects the write (the per-path
    // status carries that), so the statuses must be checked explicitly.
    let statuses = node.write_acl(&acl).await.expect("write_acl (+View entry)");
    assert!(
        !statuses.is_empty() && statuses.iter().all(|(_, s)| matches!(s, ImStatus::Success)),
        "write_acl (+View entry) statuses: {statuses:?}"
    );
    let changed = wait_for_event_after(
        &node,
        0,
        access_control::CLUSTER_ID,
        ac::ACCESS_CONTROL_ENTRY_CHANGED,
        baseline,
    )
    .await
    .expect("AccessControlEntryChanged");
    let added = changed
        .iter()
        .map(|i| {
            access_control::AccessControlEntryChangedEvent::decode(&payload_tlv(&i.value))
                .expect("AccessControlEntryChanged decodes")
        })
        .find(|e| e.change_type == access_control::ChangeTypeEnum::from_raw(1))
        .expect("an Added AccessControlEntryChanged event from our write");
    // Our fabric's event arrives in full (events are exempt from the
    // fabric-sensitive Option rule): the
    // admin is our CASE node, and LatestValue carries its sensitive fields.
    assert_eq!(
        added.admin_node_id,
        Nullable::Value(FIXTURE_CONTROLLER_NODE_ID)
    );
    match &added.latest_value {
        Nullable::Value(entry) => assert!(
            entry.privilege.is_some() && entry.subjects.is_some(),
            "own-fabric LatestValue must carry its sensitive fields: {entry:?}"
        ),
        Nullable::Null => panic!("Added event without LatestValue"),
    }

    write_and_clear_extension(&node).await;
    decode_every_event(&node, 0, access_control::CLUSTER_ID, |id, t| match id {
        ac::ACCESS_CONTROL_ENTRY_CHANGED => {
            ok(access_control::AccessControlEntryChangedEvent::decode(t))
        }
        ac::ACCESS_CONTROL_EXTENSION_CHANGED => ok(
            access_control::AccessControlExtensionChangedEvent::decode(t),
        ),
        ac::FABRIC_RESTRICTION_REVIEW_UPDATE => ok(
            access_control::FabricRestrictionReviewUpdateEvent::decode(t),
        ),
        other => newer_than_codegen("AccessControl", other),
    })
    .await;
    // Restore the ACL so later tests see the fixture's original entries.
    let statuses = node.write_acl(&acl_before).await.expect("restore ACL");
    assert!(
        !statuses.is_empty() && statuses.iter().all(|(_, s)| matches!(s, ImStatus::Success)),
        "restore ACL statuses: {statuses:?}"
    );
}

/// The clusters all-clusters cannot be made to emit on demand: decode what is there.
#[tokio::test]
async fn time_sync_power_source_pump_and_ota_events_decode() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: event test needs the all-clusters DUT (`just integration`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // ── TimeSynchronization (ep0): SetTimeZone may emit TimeZoneStatus. ────
    let _ = node
        .set_time_zone(&[TimeZoneEntry::new(3600, 0, None)])
        .await;
    use time_synchronization::event_id as ts;
    decode_every_event(
        &node,
        0,
        time_synchronization::CLUSTER_ID,
        |id, t| match id {
            ts::DST_TABLE_EMPTY | ts::TIME_FAILURE | ts::MISSING_TRUSTED_TIME_SOURCE => {
                fieldless(t)
            }
            ts::DST_STATUS => ok(time_synchronization::DstStatusEvent::decode(t)),
            ts::TIME_ZONE_STATUS => ok(time_synchronization::TimeZoneStatusEvent::decode(t)),
            other => newer_than_codegen("TimeSynchronization", other),
        },
    )
    .await;

    // ── No stimulus in all-clusters: decode whatever is there. ─────────────
    use power_source::event_id as ps;
    for ep in [0, 1, 2] {
        decode_every_event(&node, ep, power_source::CLUSTER_ID, |id, t| match id {
            ps::WIRED_FAULT_CHANGE => ok(power_source::WiredFaultChangeEvent::decode(t)),
            ps::BAT_FAULT_CHANGE => ok(power_source::BatFaultChangeEvent::decode(t)),
            ps::BAT_CHARGE_FAULT_CHANGE => ok(power_source::BatChargeFaultChangeEvent::decode(t)),
            other => newer_than_codegen("PowerSource", other),
        })
        .await;
    }
    decode_every_event(
        &node,
        1,
        pump_configuration_and_control::CLUSTER_ID,
        |id, t| {
            if id <= pump_configuration_and_control::event_id::TURBINE_OPERATION {
                fieldless(t)
            } else {
                newer_than_codegen("PumpConfigurationAndControl", id)
            }
        },
    )
    .await;
    use ota_software_update_requestor::event_id as ota;
    decode_every_event(
        &node,
        0,
        ota_software_update_requestor::CLUSTER_ID,
        |id, t| match id {
            ota::STATE_TRANSITION => ok(
                ota_software_update_requestor::StateTransitionEvent::decode(t),
            ),
            ota::VERSION_APPLIED => ok(ota_software_update_requestor::VersionAppliedEvent::decode(
                t,
            )),
            ota::DOWNLOAD_ERROR => ok(ota_software_update_requestor::DownloadErrorEvent::decode(t)),
            other => newer_than_codegen("OtaSoftwareUpdateRequestor", other),
        },
    )
    .await;
}
