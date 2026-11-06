// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! The diagnostics clusters on a live all-clusters-app (endpoint 0;
//! master 5cd2917a and v1.4.2.0, the nightly's pin): SoftwareDiagnostics,
//! ThreadNetworkDiagnostics, WiFiNetworkDiagnostics,
//! EthernetNetworkDiagnostics and DiagnosticLogs. GeneralDiagnostics lives in
//! `clusters_diagnostics.rs` and `clusters_events.rs`.
//!
//! The counters and table contents are the host's (null and zero on macOS,
//! whatever the Linux runner reports in the nightly), so the tests decode
//! every value and check shapes, not numbers. The events come from
//! all-clusters' test-event triggers, built in at both refs
//! (all-clusters-app/linux/args.gni: `chip_enable_software_diagnostics_trigger`,
//! `chip_enable_wifi_diagnostics_trigger`; registered in
//! examples/platform/linux/AppMain.cpp) and handled by
//! all-clusters-common/src/{software,wifi}-diagnostics-stub.cpp, whose
//! payloads are the same at both refs. ThreadNetworkDiagnostics' events have
//! no trigger or pipe command in any chip app at either ref (decode smoke
//! only).

use std::path::Path;

use integration_tests::dut::DutConfig;
use integration_tests::events::{
    latest_event_number, payload_tlv, test_event_trigger, wait_for_event_after,
};
use integration_tests::sweep::{
    assert_exact_attribute_ids, decode_every_attribute, invoke_for_response, invoke_for_status,
    newer_than_codegen, ok,
};
use matter_clusters::clusters::{
    diagnostic_logs as logs, ethernet_network_diagnostics as eth, software_diagnostics as sw,
    thread_network_diagnostics as thread, wifi_network_diagnostics as wifi,
};
use matter_controller::{CommandPath, ImStatus, MatterController, Node, ReadPath};

/// IM status code for a refused request (Matter Core spec 8.10.1).
const INVALID_COMMAND: ImStatus = ImStatus::Failure(0x85);

/// TestEventTrigger codes (chip src/app/clusters/*/…TestEventTriggerHandler.h,
/// the same at both refs).
const SOFTWARE_FAULT_TRIGGER: u64 = 0x0034_0000_0000_0000;
const WIFI_DISCONNECTION_TRIGGER: u64 = 0x0036_0000_0000_0000;
const WIFI_ASSOCIATION_FAILURE_TRIGGER: u64 = 0x0036_0000_0000_0001;
const WIFI_CONNECTION_STATUS_TRIGGER: u64 = 0x0036_0000_0000_0002;

/// all-clusters' endpoint configuration (ZAP's `.matter` IDL), relative to
/// the connectedhomeip checkout.
const ALL_CLUSTERS_MATTER: &str =
    "examples/all-clusters-app/all-clusters-common/all-clusters-app.matter";

/// The DUT config, controller and node id, or `None` (skip) unless the DUT is
/// all-clusters.
async fn connect_all_clusters() -> Option<(DutConfig, MatterController, u64)> {
    let cfg = DutConfig::from_env()?;
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: diagnostics sweep needs the all-clusters DUT (`just integration`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((cfg, controller, node_id))
}

/// Invoke an endpoint-0 command and return the bare status it answers.
async fn status0(node: &Node, cluster: u32, command: u32, fields: Vec<u8>) -> ImStatus {
    let path = CommandPath {
        endpoint: 0,
        cluster,
        command,
    };
    invoke_for_status(node, path, fields).await.unwrap()
}

/// Fire `trigger` and return the payload TLV of the newest `(0, cluster,
/// event)` event it caused.
async fn triggered_event(node: &Node, cluster: u32, event: u32, trigger: u64) -> Vec<u8> {
    let baseline = latest_event_number(node, 0, cluster, event).await.unwrap();
    test_event_trigger(node, trigger).await.unwrap();
    let items = wait_for_event_after(node, 0, cluster, event, baseline)
        .await
        .unwrap();
    payload_tlv(&items.last().unwrap().value)
}

/// SoftwareDiagnostics (WTRMRK): every attribute decodes, ThreadMetrics
/// included; ResetWatermarks succeeds; the SoftwareFault trigger emits a
/// fault whose Name is its Id (the stub records the process id as both) and
/// whose FaultRecording is the time (`strftime("%c")`, so only its presence
/// is checked).
#[tokio::test]
async fn software_diagnostics_decodes_resets_and_reports_a_fault() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use sw::attribute_id as a;
    let attrs = decode_every_attribute(&node, 0, sw::CLUSTER_ID, |id, t| match id {
        a::THREAD_METRICS => ok(sw::decode_thread_metrics(t)),
        a::CURRENT_HEAP_FREE => ok(sw::decode_current_heap_free(t)),
        a::CURRENT_HEAP_USED => ok(sw::decode_current_heap_used(t)),
        a::CURRENT_HEAP_HIGH_WATERMARK => ok(sw::decode_current_heap_high_watermark(t)),
        other => newer_than_codegen("SoftwareDiagnostics", other),
    })
    .await;
    assert_exact_attribute_ids("SoftwareDiagnostics", &attrs, &[0, 1, 2, 3]);
    assert_eq!(
        status0(
            &node,
            sw::CLUSTER_ID,
            sw::command_id::RESET_WATERMARKS,
            sw::encode_reset_watermarks()
        )
        .await,
        ImStatus::Success
    );
    let fault = sw::SoftwareFaultEvent::decode(
        &triggered_event(
            &node,
            sw::CLUSTER_ID,
            sw::event_id::SOFTWARE_FAULT,
            SOFTWARE_FAULT_TRIGGER,
        )
        .await,
    )
    .unwrap();
    assert_eq!(fault.name, Some(fault.id.to_string()));
    assert_ne!(fault.fault_recording.unwrap_or_default().len(), 0);
}

/// WiFiNetworkDiagnostics (PKTCNT, ERRCNT): all 13 attributes decode;
/// ResetCounts succeeds; the three triggers emit the stub's events:
/// Disconnection with reason 3, AssociationFailure (AuthenticationFailed,
/// status 4), ConnectionStatus NotConnected. At master the stub reaches the
/// cluster through `GetWiFiDiagnosticsDelegate()`, which the cluster sets
/// when it is created; at v1.4.2.0 it calls `WiFiDiagnosticsServer::Instance()`.
/// Same payloads.
#[tokio::test]
async fn wifi_diagnostics_decode_reset_and_report_their_events() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use wifi::{attribute_id as a, event_id as e};
    let attrs = decode_every_attribute(&node, 0, wifi::CLUSTER_ID, |id, t| match id {
        a::BSSID => ok(wifi::decode_bssid(t)),
        a::SECURITY_TYPE => ok(wifi::decode_security_type(t)),
        a::WIFI_VERSION => ok(wifi::decode_wifi_version(t)),
        a::CHANNEL_NUMBER => ok(wifi::decode_channel_number(t)),
        a::RSSI => ok(wifi::decode_rssi(t)),
        a::BEACON_LOST_COUNT => ok(wifi::decode_beacon_lost_count(t)),
        a::BEACON_RX_COUNT => ok(wifi::decode_beacon_rx_count(t)),
        a::PACKET_MULTICAST_RX_COUNT => ok(wifi::decode_packet_multicast_rx_count(t)),
        a::PACKET_MULTICAST_TX_COUNT => ok(wifi::decode_packet_multicast_tx_count(t)),
        a::PACKET_UNICAST_RX_COUNT => ok(wifi::decode_packet_unicast_rx_count(t)),
        a::PACKET_UNICAST_TX_COUNT => ok(wifi::decode_packet_unicast_tx_count(t)),
        a::CURRENT_MAX_RATE => ok(wifi::decode_current_max_rate(t)),
        a::OVERRUN_COUNT => ok(wifi::decode_overrun_count(t)),
        other => newer_than_codegen("WiFiNetworkDiagnostics", other),
    })
    .await;
    let all: Vec<u32> = (0..=12).collect();
    assert_exact_attribute_ids("WiFiNetworkDiagnostics", &attrs, &all);
    assert_eq!(
        status0(
            &node,
            wifi::CLUSTER_ID,
            wifi::command_id::RESET_COUNTS,
            wifi::encode_reset_counts()
        )
        .await,
        ImStatus::Success
    );
    let tlv = triggered_event(
        &node,
        wifi::CLUSTER_ID,
        e::DISCONNECTION,
        WIFI_DISCONNECTION_TRIGGER,
    )
    .await;
    assert_eq!(
        wifi::DisconnectionEvent::decode(&tlv).unwrap().reason_code,
        3
    );
    let tlv = triggered_event(
        &node,
        wifi::CLUSTER_ID,
        e::ASSOCIATION_FAILURE,
        WIFI_ASSOCIATION_FAILURE_TRIGGER,
    )
    .await;
    let failure = wifi::AssociationFailureEvent::decode(&tlv).unwrap();
    assert_eq!(
        (failure.association_failure_cause, failure.status),
        (wifi::AssociationFailureCauseEnum::AuthenticationFailed, 4)
    );
    let tlv = triggered_event(
        &node,
        wifi::CLUSTER_ID,
        e::CONNECTION_STATUS,
        WIFI_CONNECTION_STATUS_TRIGGER,
    )
    .await;
    assert_eq!(
        wifi::ConnectionStatusEvent::decode(&tlv)
            .unwrap()
            .connection_status,
        wifi::ConnectionStatusEnum::NotConnected
    );
}

/// The ThreadNetworkDiagnostics revision the checkout's all-clusters serves on
/// endpoint 0, from its `all-clusters-app.matter`.
///
/// Neither ref's endpoint-0 block lists `extAddress` or `rloc16`, so the
/// attribute list cannot tell the refs apart; the revision can. v1.4.2.0's
/// ember server takes it from the block's `ram attribute clusterRevision
/// default = 2;` and serves only the listed attributes. master's code-driven
/// server (ThreadNetworkDiagnosticsCluster.cpp) declares the attribute
/// `callback`, reports the generated `ThreadNetworkDiagnostics::kRevision`
/// (3, zzz_generated/.../ThreadNetworkDiagnostics/Metadata.h) and serves the
/// whole 0x0000..=0x0040 range whatever the block lists. With no `ram`
/// revision in the block, this reads the `revision 3;` of the `cluster
/// ThreadNetworkDiagnostics = 53 {` definition, generated from the same XML.
/// A reshaped block also falls back to the definition's revision; the test's
/// live ClusterRevision assert catches a wrong pick.
///
/// # Panics
///
/// If the file cannot be read or holds neither declaration: a moved or
/// reshaped file must fail the test, not quietly pick a list.
fn thread_revision(chip_root: &Path) -> u32 {
    let path = chip_root.join(ALL_CLUSTERS_MATTER);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read all-clusters endpoint config {}: {e}", path.display()));
    let pinned = source
        .lines()
        .skip_while(|l| l.trim_end() != "endpoint 0 {")
        .take_while(|l| !l.starts_with('}'))
        .skip_while(|l| l.trim() != "server cluster ThreadNetworkDiagnostics {")
        .take_while(|l| l.trim() != "}")
        .find_map(|l| match l.split_whitespace().collect::<Vec<_>>()[..] {
            [storage, "attribute", "clusterRevision", "default", "=", value]
                if storage != "callback" =>
            {
                value.strip_suffix(';')?.parse().ok()
            }
            _ => None,
        });
    let defined = || {
        source
            .lines()
            .skip_while(|l| l.trim_end() != "cluster ThreadNetworkDiagnostics = 53 {")
            .take_while(|l| !l.starts_with('}'))
            .find_map(|l| {
                l.trim()
                    .strip_prefix("revision ")?
                    .strip_suffix(';')?
                    .parse()
                    .ok()
            })
    };
    pinned
        .or_else(defined)
        .unwrap_or_else(|| panic!("no ThreadNetworkDiagnostics revision in {}", path.display()))
}

/// The cluster's ClusterRevision on endpoint 0.
async fn cluster_revision(node: &Node, cluster: u32) -> u32 {
    use matter_clusters::clusters::globals::{decode_u32, CLUSTER_REVISION};
    let report = node
        .read(&[ReadPath::concrete(0, cluster, CLUSTER_REVISION)])
        .await
        .unwrap();
    let (_, value) = report
        .into_iter()
        .find(|(p, _)| p.attribute == CLUSTER_REVISION)
        .unwrap();
    decode_u32(&payload_tlv(&value)).unwrap()
}

/// ThreadNetworkDiagnostics (all four features): every attribute it serves
/// decodes with its own decoder, and the served ids are exactly those of its
/// revision (spec revision history: revision 3 adds ExtAddress and Rloc16).
/// The revision comes from the checkout's `.matter` ([`thread_revision`]: 3 at
/// master, 0x0000..=0x0040; 2 at v1.4.2.0, 0x0000..=0x003E) and must match
/// what the DUT reports. ResetCounts succeeds. EthernetNetworkDiagnostics
/// (PKTCNT, ERRCNT): all nine attributes decode; ResetCounts succeeds.
#[tokio::test]
async fn thread_and_ethernet_diagnostics_decode_and_reset() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    let revision = thread_revision(&cfg.chip_root);
    assert_eq!(
        cluster_revision(&node, thread::CLUSTER_ID).await,
        revision,
        "ThreadNetworkDiagnostics ClusterRevision vs {ALL_CLUSTERS_MATTER}"
    );
    let attrs = decode_every_attribute(&node, 0, thread::CLUSTER_ID, decode_thread).await;
    use thread::attribute_id as ta;
    let mut want: Vec<u32> = (ta::CHANNEL..=ta::ACTIVE_NETWORK_FAULTS_LIST).collect();
    if revision >= 3 {
        want.extend([ta::EXT_ADDRESS, ta::RLOC16]);
    }
    assert_exact_attribute_ids("ThreadNetworkDiagnostics", &attrs, &want);
    assert_eq!(
        status0(
            &node,
            thread::CLUSTER_ID,
            thread::command_id::RESET_COUNTS,
            thread::encode_reset_counts()
        )
        .await,
        ImStatus::Success
    );
    use eth::attribute_id as a;
    let attrs = decode_every_attribute(&node, 0, eth::CLUSTER_ID, |id, t| match id {
        a::PHY_RATE => ok(eth::decode_phy_rate(t)),
        a::FULL_DUPLEX => ok(eth::decode_full_duplex(t)),
        a::PACKET_RX_COUNT => ok(eth::decode_packet_rx_count(t)),
        a::PACKET_TX_COUNT => ok(eth::decode_packet_tx_count(t)),
        a::TX_ERR_COUNT => ok(eth::decode_tx_err_count(t)),
        a::COLLISION_COUNT => ok(eth::decode_collision_count(t)),
        a::OVERRUN_COUNT => ok(eth::decode_overrun_count(t)),
        a::CARRIER_DETECT => ok(eth::decode_carrier_detect(t)),
        a::TIME_SINCE_RESET => ok(eth::decode_time_since_reset(t)),
        other => newer_than_codegen("EthernetNetworkDiagnostics", other),
    })
    .await;
    let all: Vec<u32> = (0..=8).collect();
    assert_exact_attribute_ids("EthernetNetworkDiagnostics", &attrs, &all);
    assert_eq!(
        status0(
            &node,
            eth::CLUSTER_ID,
            eth::command_id::RESET_COUNTS,
            eth::encode_reset_counts()
        )
        .await,
        ImStatus::Success
    );
}

/// The generated decoder for ThreadNetworkDiagnostics attribute `id`, one
/// per attribute.
fn decode_thread(id: u32, t: &[u8]) -> Result<(), String> {
    use thread::attribute_id as a;
    match id {
        a::CHANNEL => ok(thread::decode_channel(t)),
        a::ROUTING_ROLE => ok(thread::decode_routing_role(t)),
        a::NETWORK_NAME => ok(thread::decode_network_name(t)),
        a::PAN_ID => ok(thread::decode_pan_id(t)),
        a::EXTENDED_PAN_ID => ok(thread::decode_extended_pan_id(t)),
        a::MESH_LOCAL_PREFIX => ok(thread::decode_mesh_local_prefix(t)),
        a::OVERRUN_COUNT => ok(thread::decode_overrun_count(t)),
        a::NEIGHBOR_TABLE => ok(thread::decode_neighbor_table(t)),
        a::ROUTE_TABLE => ok(thread::decode_route_table(t)),
        a::PARTITION_ID => ok(thread::decode_partition_id(t)),
        a::WEIGHTING => ok(thread::decode_weighting(t)),
        a::DATA_VERSION => ok(thread::decode_data_version(t)),
        a::STABLE_DATA_VERSION => ok(thread::decode_stable_data_version(t)),
        a::LEADER_ROUTER_ID => ok(thread::decode_leader_router_id(t)),
        a::DETACHED_ROLE_COUNT => ok(thread::decode_detached_role_count(t)),
        a::CHILD_ROLE_COUNT => ok(thread::decode_child_role_count(t)),
        a::ROUTER_ROLE_COUNT => ok(thread::decode_router_role_count(t)),
        a::LEADER_ROLE_COUNT => ok(thread::decode_leader_role_count(t)),
        a::ATTACH_ATTEMPT_COUNT => ok(thread::decode_attach_attempt_count(t)),
        a::PARTITION_ID_CHANGE_COUNT => ok(thread::decode_partition_id_change_count(t)),
        a::BETTER_PARTITION_ATTACH_ATTEMPT_COUNT => {
            ok(thread::decode_better_partition_attach_attempt_count(t))
        }
        a::PARENT_CHANGE_COUNT => ok(thread::decode_parent_change_count(t)),
        a::TX_TOTAL_COUNT => ok(thread::decode_tx_total_count(t)),
        a::TX_UNICAST_COUNT => ok(thread::decode_tx_unicast_count(t)),
        a::TX_BROADCAST_COUNT => ok(thread::decode_tx_broadcast_count(t)),
        a::TX_ACK_REQUESTED_COUNT => ok(thread::decode_tx_ack_requested_count(t)),
        a::TX_ACKED_COUNT => ok(thread::decode_tx_acked_count(t)),
        a::TX_NO_ACK_REQUESTED_COUNT => ok(thread::decode_tx_no_ack_requested_count(t)),
        a::TX_DATA_COUNT => ok(thread::decode_tx_data_count(t)),
        a::TX_DATA_POLL_COUNT => ok(thread::decode_tx_data_poll_count(t)),
        a::TX_BEACON_COUNT => ok(thread::decode_tx_beacon_count(t)),
        a::TX_BEACON_REQUEST_COUNT => ok(thread::decode_tx_beacon_request_count(t)),
        a::TX_OTHER_COUNT => ok(thread::decode_tx_other_count(t)),
        a::TX_RETRY_COUNT => ok(thread::decode_tx_retry_count(t)),
        a::TX_DIRECT_MAX_RETRY_EXPIRY_COUNT => {
            ok(thread::decode_tx_direct_max_retry_expiry_count(t))
        }
        a::TX_INDIRECT_MAX_RETRY_EXPIRY_COUNT => {
            ok(thread::decode_tx_indirect_max_retry_expiry_count(t))
        }
        a::TX_ERR_CCA_COUNT => ok(thread::decode_tx_err_cca_count(t)),
        a::TX_ERR_ABORT_COUNT => ok(thread::decode_tx_err_abort_count(t)),
        a::TX_ERR_BUSY_CHANNEL_COUNT => ok(thread::decode_tx_err_busy_channel_count(t)),
        a::RX_TOTAL_COUNT => ok(thread::decode_rx_total_count(t)),
        a::RX_UNICAST_COUNT => ok(thread::decode_rx_unicast_count(t)),
        a::RX_BROADCAST_COUNT => ok(thread::decode_rx_broadcast_count(t)),
        a::RX_DATA_COUNT => ok(thread::decode_rx_data_count(t)),
        a::RX_DATA_POLL_COUNT => ok(thread::decode_rx_data_poll_count(t)),
        a::RX_BEACON_COUNT => ok(thread::decode_rx_beacon_count(t)),
        a::RX_BEACON_REQUEST_COUNT => ok(thread::decode_rx_beacon_request_count(t)),
        a::RX_OTHER_COUNT => ok(thread::decode_rx_other_count(t)),
        a::RX_ADDRESS_FILTERED_COUNT => ok(thread::decode_rx_address_filtered_count(t)),
        a::RX_DEST_ADDR_FILTERED_COUNT => ok(thread::decode_rx_dest_addr_filtered_count(t)),
        a::RX_DUPLICATED_COUNT => ok(thread::decode_rx_duplicated_count(t)),
        a::RX_ERR_NO_FRAME_COUNT => ok(thread::decode_rx_err_no_frame_count(t)),
        a::RX_ERR_UNKNOWN_NEIGHBOR_COUNT => ok(thread::decode_rx_err_unknown_neighbor_count(t)),
        a::RX_ERR_INVALID_SRC_ADDR_COUNT => ok(thread::decode_rx_err_invalid_src_addr_count(t)),
        a::RX_ERR_SEC_COUNT => ok(thread::decode_rx_err_sec_count(t)),
        a::RX_ERR_FCS_COUNT => ok(thread::decode_rx_err_fcs_count(t)),
        a::RX_ERR_OTHER_COUNT => ok(thread::decode_rx_err_other_count(t)),
        a::ACTIVE_TIMESTAMP => ok(thread::decode_active_timestamp(t)),
        a::PENDING_TIMESTAMP => ok(thread::decode_pending_timestamp(t)),
        a::DELAY => ok(thread::decode_delay(t)),
        a::SECURITY_POLICY => ok(thread::decode_security_policy(t)),
        a::CHANNEL_PAGE0_MASK => ok(thread::decode_channel_page0_mask(t)),
        a::OPERATIONAL_DATASET_COMPONENTS => ok(thread::decode_operational_dataset_components(t)),
        a::ACTIVE_NETWORK_FAULTS_LIST => ok(thread::decode_active_network_faults_list(t)),
        a::EXT_ADDRESS => ok(thread::decode_ext_address(t)),
        a::RLOC16 => ok(thread::decode_rloc16(t)),
        other => newer_than_codegen("ThreadNetworkDiagnostics", other),
    }
}

/// DiagnosticLogs: all-clusters is launched without log files, so chip's
/// server answers each intent's ResponsePayload request with NoLogs, an
/// empty LogContent and no timestamps (the delegate's GetSizeForIntent is 0
/// without a file: master DiagnosticLogsCluster.cpp
/// `HandleLogRequestForResponsePayload`, v1.4.2.0 diagnostic-logs-server.cpp
/// `DiagnosticLogsServer::HandleLogRequestForResponsePayload`). A BDX
/// request without a TransferFileDesignator, and an unknown intent, are
/// refused with InvalidCommand (both refs).
#[tokio::test]
async fn diagnostic_logs_answer_no_logs_and_refuse_bad_requests() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use logs::{IntentEnum, StatusEnum, TransferProtocolEnum};
    let path = CommandPath {
        endpoint: 0,
        cluster: logs::CLUSTER_ID,
        command: logs::command_id::RETRIEVE_LOGS_REQUEST,
    };
    for intent in [
        IntentEnum::EndUserSupport,
        IntentEnum::NetworkDiag,
        IntentEnum::CrashLogs,
    ] {
        let resp = invoke_for_response(
            &node,
            path,
            logs::encode_retrieve_logs_request(intent, TransferProtocolEnum::ResponsePayload, None),
            logs::command_id::RETRIEVE_LOGS_RESPONSE,
        )
        .await
        .unwrap();
        let r = logs::RetrieveLogsResponse::decode(&resp).unwrap();
        assert_eq!(
            (
                r.status,
                r.log_content.len(),
                r.utc_time_stamp,
                r.time_since_boot
            ),
            (StatusEnum::NoLogs, 0, None, None),
            "{intent:?}"
        );
    }
    let refused = |intent, protocol| {
        invoke_for_status(
            &node,
            path,
            logs::encode_retrieve_logs_request(intent, protocol, None),
        )
    };
    assert_eq!(
        refused(IntentEnum::EndUserSupport, TransferProtocolEnum::Bdx)
            .await
            .unwrap(),
        INVALID_COMMAND
    );
    assert_eq!(
        refused(
            IntentEnum::Unknown(3),
            TransferProtocolEnum::ResponsePayload
        )
        .await
        .unwrap(),
        INVALID_COMMAND
    );
}
