// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! WakeOnLan and LowPower on a live all-clusters-app, endpoint 1.
//! all-clusters serves both at master 5cd2917a and v1.4.2.0
//! (`all-clusters-app.matter`: WakeOnLan with only `MACAddress`, a `ram`
//! attribute with no default; LowPower with `Sleep`), so they run in `just
//! integration` locally and in the nightly, not only on tv-app. LowPower's
//! delegate is all-clusters' `LowPowerManager`, whose `HandleSleep` returns
//! true (`examples/all-clusters-app/linux/include/tv-callbacks.cpp`, both
//! refs), so chip's server answers Sleep with Success.

use integration_tests::dut::DutConfig;
use integration_tests::sweep::{
    assert_exact_attribute_ids, attribute_tlv, decode_every_attribute, invoke_for_status,
    newer_than_codegen, ok,
};
use matter_clusters::clusters::{low_power, wake_on_lan};
use matter_controller::{CommandPath, ImStatus};

/// all-clusters serves both clusters on endpoint 1.
const EP: u16 = 1;

#[tokio::test]
async fn wake_on_lan_and_low_power_on_all_clusters() {
    let Some(cfg) = DutConfig::from_env() else {
        eprintln!("skipped: no DUT (set MATTER_INTEGRATION_DUT via `just integration`)");
        return;
    };
    if !cfg.is_app("all-clusters") {
        eprintln!(
            "skipped: WakeOnLan / LowPower sweep needs the all-clusters DUT (`just integration`)"
        );
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);
    use wake_on_lan::attribute_id as a;
    let attrs = decode_every_attribute(&node, EP, wake_on_lan::CLUSTER_ID, |id, t| match id {
        a::MAC_ADDRESS => ok(wake_on_lan::decode_mac_address(t)),
        a::LINK_LOCAL_ADDRESS => ok(wake_on_lan::decode_link_local_address(t)),
        other => newer_than_codegen("WakeOnLan", other),
    })
    .await;
    assert_exact_attribute_ids("WakeOnLan", &attrs, &[0]);
    // A ram attribute with no default, which all-clusters never sets.
    assert_eq!(
        wake_on_lan::decode_mac_address(attribute_tlv(&attrs, a::MAC_ADDRESS)).unwrap(),
        ""
    );
    let attrs = decode_every_attribute(&node, EP, low_power::CLUSTER_ID, |id, _| {
        newer_than_codegen("LowPower", id)
    })
    .await;
    assert_exact_attribute_ids("LowPower", &attrs, &[]);
    let path = CommandPath {
        endpoint: EP,
        cluster: low_power::CLUSTER_ID,
        command: low_power::command_id::SLEEP,
    };
    assert_eq!(
        invoke_for_status(&node, path, low_power::encode_sleep())
            .await
            .unwrap(),
        ImStatus::Success
    );
}
