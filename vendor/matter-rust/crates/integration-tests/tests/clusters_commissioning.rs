// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! GeneralCommissioning and NetworkCommissioning, decoded with the
//! generated codecs on an already-commissioned all-clusters-app (endpoint 0;
//! master 5cd2917a and v1.4.2.0, the nightly's pin).
//!
//! These clusters run commissioning, so the tests never send what would change
//! the DUT's commissioned state (integration tests leave it alone): no
//! CommissioningComplete, no network configuration command and no
//! NetworkCommissioning write. What they do send is undone or refused:
//! - ArmFailSafe(10 s, breadcrumb 7) arms the fail-safe over CASE (no
//!   commissioning window is open), and ArmFailSafe(0) disarms it at once
//!   (chip's server forces the timer to expire, which also resets the
//!   breadcrumb: master GeneralCommissioningCluster.cpp, v1.4.2.0
//!   general-commissioning-server.cpp). Nothing is pending (no AddNOC or
//!   UpdateNOC), so chip's expiry cleanup reverts nothing and keeps the
//!   fabric and its CASE sessions (OperationalCredentials FailSafeCleanup,
//!   both refs). The disarm is sent before anything is asserted, so a failed
//!   assertion never leaves the fail-safe armed for the tests after it.
//! - SetRegulatoryConfig with a location above IndoorOutdoor is answered
//!   ValueOutsideRange, and one with a one-character country code is refused
//!   with ConstraintError, both before anything is stored (both refs).
//!
//! NetworkCommissioning on endpoint 0 is all-clusters' Ethernet interface at
//! both refs, on Linux and macOS, when launched without `--wifi` /
//! `--thread` (the harness does not pass them:
//! examples/platform/linux/AppMain.cpp InitNetworkCommissioning; the
//! secondary endpoint 65534 stays disabled). Both refs' code-driven server
//! serves exactly Ethernet's six attributes. Its values are the host's (the
//! interface name differs between macOS and the Linux nightly), so the test
//! checks shapes, not names.

use anyhow::{Context as _, Result};
use integration_tests::dut::DutConfig;
use integration_tests::events::payload_tlv;
use integration_tests::sweep::{
    assert_exact_attribute_ids, attribute_tlv, decode_every_attribute, invoke_for_response,
    newer_than_codegen, ok, read_cluster_attributes, write_attribute,
};
use matter_clusters::clusters::{general_commissioning as gc, network_commissioning as nc};
use matter_controller::{
    AttributePath, CommandPath, ImStatus, InvokeResult, MatterController, Node, ReadPath,
};

/// IM status code for a refused value (Matter Core spec 8.10.1).
const CONSTRAINT_ERROR: ImStatus = ImStatus::Failure(0x87);

/// The DUT config, controller and node id, or `None` (skip) unless the DUT is
/// all-clusters.
async fn connect_all_clusters() -> Option<(DutConfig, MatterController, u64)> {
    let cfg = DutConfig::from_env()?;
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: commissioning sweep needs the all-clusters DUT (`just integration`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((cfg, controller, node_id))
}

/// A GeneralCommissioning command path on endpoint 0.
fn gc_command(command: u32) -> CommandPath {
    CommandPath {
        endpoint: 0,
        cluster: gc::CLUSTER_ID,
        command,
    }
}

/// The cluster's FeatureMap on endpoint 0.
async fn feature_map(node: &Node, cluster: u32) -> u32 {
    use matter_clusters::clusters::globals::{decode_u32, FEATURE_MAP};
    let report = node
        .read(&[ReadPath::concrete(0, cluster, FEATURE_MAP)])
        .await
        .unwrap();
    let (_, value) = report
        .into_iter()
        .find(|(p, _)| p.attribute == FEATURE_MAP)
        .unwrap();
    decode_u32(&payload_tlv(&value)).unwrap()
}

/// The current Breadcrumb, or the error that kept it from being read (the
/// fail-safe test collects results without panicking until it has disarmed).
async fn try_breadcrumb(node: &Node) -> Result<u64> {
    let attrs = read_cluster_attributes(node, 0, gc::CLUSTER_ID).await?;
    // Not `attribute_tlv`, which panics on a missing attribute: this runs
    // between the arm and the disarm.
    let (_, tlv) = attrs
        .iter()
        .find(|(id, _)| *id == gc::attribute_id::BREADCRUMB)
        .context("Breadcrumb not in the wildcard read")?;
    Ok(gc::decode_breadcrumb(tlv)?)
}

/// The current Breadcrumb.
async fn breadcrumb(node: &Node) -> u64 {
    try_breadcrumb(node).await.unwrap()
}

/// ArmFailSafe and its decoded response, or the error. Never panics, so the
/// caller can always send the disarm before asserting anything.
async fn arm_fail_safe(
    node: &Node,
    expiry: u16,
    breadcrumb: u64,
) -> Result<gc::ArmFailSafeResponse> {
    let resp = invoke_for_response(
        node,
        gc_command(gc::command_id::ARM_FAIL_SAFE),
        gc::encode_arm_fail_safe(expiry, breadcrumb),
        gc::command_id::ARM_FAIL_SAFE_RESPONSE,
    )
    .await?;
    Ok(gc::ArmFailSafeResponse::decode(&resp)?)
}

/// An ArmFailSafe response's error code, after checking its DebugText: chip's
/// server always sends DebugText, empty on these paths (both refs).
fn arm_error_code(what: &str, resp: Result<gc::ArmFailSafeResponse>) -> gc::CommissioningErrorEnum {
    let r = resp.unwrap_or_else(|e| panic!("{what}: {e:#}"));
    assert_eq!(r.debug_text, "", "{what}");
    r.error_code
}

/// Every GeneralCommissioning attribute decodes: BasicCommissioningInfo is
/// chip's default 60 s / 900 s (CHIPDeviceConfig.h, both refs; all-clusters
/// does not override it). Breadcrumb is written. ArmFailSafe arms (Breadcrumb
/// follows) and disarms (Breadcrumb reset).
#[tokio::test]
async fn general_commissioning_decodes_and_arms_and_disarms_the_fail_safe() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use gc::{attribute_id as a, CommissioningErrorEnum};
    let attrs = decode_every_attribute(&node, 0, gc::CLUSTER_ID, |id, t| match id {
        a::BREADCRUMB => ok(gc::decode_breadcrumb(t)),
        a::BASIC_COMMISSIONING_INFO => ok(gc::decode_basic_commissioning_info(t)),
        a::REGULATORY_CONFIG => ok(gc::decode_regulatory_config(t)),
        a::LOCATION_CAPABILITY => ok(gc::decode_location_capability(t)),
        a::SUPPORTS_CONCURRENT_CONNECTION => ok(gc::decode_supports_concurrent_connection(t)),
        other => newer_than_codegen("GeneralCommissioning", other),
    })
    .await;
    // No TC or NR feature: the five base attributes only.
    assert_exact_attribute_ids("GeneralCommissioning", &attrs, &[0, 1, 2, 3, 4]);
    let info =
        gc::decode_basic_commissioning_info(attribute_tlv(&attrs, a::BASIC_COMMISSIONING_INFO))
            .unwrap();
    assert_eq!(
        (
            info.fail_safe_expiry_length_seconds,
            info.max_cumulative_failsafe_seconds
        ),
        (60, 900)
    );
    let at = AttributePath {
        endpoint: 0,
        cluster: gc::CLUSTER_ID,
        attribute: a::BREADCRUMB,
    };
    assert_eq!(
        write_attribute(&node, at, gc::encode_breadcrumb(5))
            .await
            .unwrap(),
        ImStatus::Success
    );
    assert_eq!(breadcrumb(&node).await, 5);

    // Arm, read, and disarm before asserting anything: nothing between the
    // arm and the disarm can panic, and the disarm is sent whatever the arm
    // answered, so a failure here never leaves the fail-safe armed (it would
    // refuse the next test's ArmFailSafe or OpenCommissioningWindow until the
    // 10 s expiry).
    let armed = arm_fail_safe(&node, 10, 7).await;
    let while_armed = try_breadcrumb(&node).await;
    let disarmed = arm_fail_safe(&node, 0, 0).await;
    let after_disarm = try_breadcrumb(&node).await;

    assert_eq!(
        arm_error_code("disarm", disarmed),
        CommissioningErrorEnum::Ok
    );
    assert_eq!(arm_error_code("arm", armed), CommissioningErrorEnum::Ok);
    assert_eq!(while_armed.unwrap(), 7, "arming sets the breadcrumb");
    assert_eq!(after_disarm.unwrap(), 0, "disarming resets it");
}

/// SetRegulatoryConfig refusals: a location above IndoorOutdoor answers
/// ValueOutsideRange; a country code that is not two characters is refused
/// with ConstraintError. Neither changes RegulatoryConfig or Breadcrumb.
#[tokio::test]
async fn general_commissioning_refuses_bad_regulatory_configs() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use gc::{attribute_id as a, CommissioningErrorEnum, RegulatoryLocationTypeEnum};
    let regulatory = || async {
        let attrs = read_cluster_attributes(&node, 0, gc::CLUSTER_ID)
            .await
            .unwrap();
        gc::decode_regulatory_config(attribute_tlv(&attrs, a::REGULATORY_CONFIG)).unwrap()
    };
    let before = (regulatory().await, breadcrumb(&node).await);
    let resp = invoke_for_response(
        &node,
        gc_command(gc::command_id::SET_REGULATORY_CONFIG),
        gc::encode_set_regulatory_config(
            RegulatoryLocationTypeEnum::Unknown(3),
            &"XX".to_string(),
            1,
        ),
        gc::command_id::SET_REGULATORY_CONFIG_RESPONSE,
    )
    .await
    .unwrap();
    let r = gc::SetRegulatoryConfigResponse::decode(&resp).unwrap();
    assert_eq!(r.error_code, CommissioningErrorEnum::ValueOutsideRange);
    // chip's server leaves DebugText empty on this path (both refs).
    assert_eq!(r.debug_text, "");
    let short = node
        .invoke_tlv(
            gc_command(gc::command_id::SET_REGULATORY_CONFIG),
            gc::encode_set_regulatory_config(
                RegulatoryLocationTypeEnum::Indoor,
                &"X".to_string(),
                1,
            ),
        )
        .await
        .unwrap();
    assert!(
        matches!(short, InvokeResult::Status(s) if s == CONSTRAINT_ERROR),
        "{short:?}"
    );
    assert_eq!(
        (regulatory().await, breadcrumb(&node).await),
        before,
        "a refused config stores nothing and sets no breadcrumb"
    );
}

/// NetworkCommissioning on endpoint 0: all-clusters' Ethernet interface
/// (FeatureMap ET only). It serves exactly Ethernet's six attributes (both
/// refs' code-driven server), every one decodes; Networks holds the one
/// interface, MaxNetworks is 1.
#[tokio::test]
async fn network_commissioning_decodes_the_ethernet_interface() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    assert_eq!(
        nc::Feature::from_bits_retain(feature_map(&node, nc::CLUSTER_ID).await),
        nc::Feature::ET
    );
    use nc::attribute_id as a;
    let attrs = decode_every_attribute(&node, 0, nc::CLUSTER_ID, |id, t| match id {
        a::MAX_NETWORKS => ok(nc::decode_max_networks(t)),
        a::NETWORKS => ok(nc::decode_networks(t)),
        a::SCAN_MAX_TIME_SECONDS => ok(nc::decode_scan_max_time_seconds(t)),
        a::CONNECT_MAX_TIME_SECONDS => ok(nc::decode_connect_max_time_seconds(t)),
        a::INTERFACE_ENABLED => ok(nc::decode_interface_enabled(t)),
        a::LAST_NETWORKING_STATUS => ok(nc::decode_last_networking_status(t)),
        a::LAST_NETWORK_ID => ok(nc::decode_last_network_id(t)),
        a::LAST_CONNECT_ERROR_VALUE => ok(nc::decode_last_connect_error_value(t)),
        a::SUPPORTED_WIFI_BANDS => ok(nc::decode_supported_wifi_bands(t)),
        a::SUPPORTED_THREAD_FEATURES => ok(nc::decode_supported_thread_features(t)),
        a::THREAD_VERSION => ok(nc::decode_thread_version(t)),
        other => newer_than_codegen("NetworkCommissioning", other),
    })
    .await;
    // Ethernet's mandatory attributes and nothing else: both refs add
    // ScanMaxTimeSeconds, ConnectMaxTimeSeconds and the Wi-Fi / Thread ones
    // only for a WI or TH instance (master NetworkCommissioningCluster.cpp
    // Attributes, v1.4.2.0 the same function).
    assert_exact_attribute_ids("NetworkCommissioning", &attrs, &[0, 1, 4, 5, 6, 7]);
    assert_eq!(
        nc::decode_max_networks(attribute_tlv(&attrs, a::MAX_NETWORKS)).unwrap(),
        1
    );
    let networks = nc::decode_networks(attribute_tlv(&attrs, a::NETWORKS)).unwrap();
    assert_eq!(networks.len(), 1, "{networks:?}");
    assert_ne!(networks[0].network_id.len(), 0, "{networks:?}");
}
