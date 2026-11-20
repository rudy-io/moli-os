//! Byte parity: the commissioning flow's hand-written `GeneralCommissioning` and
//! `NetworkCommissioning` codecs against the generated ones.
//!
//! `matter-commissioning` drives commissioning with its own hand-written
//! subset of these two clusters (`matter_commissioning::clusters`), and keeps
//! it (the generated clusters are codecs only; consolidating the two is
//! deferred). `matter-clusters` now also generates both clusters in full.
//! The two must never disagree on the wire, so this test pins every command
//! the commissioning flow sends: the same inputs give the same bytes from
//! both encoders. Those commands are
//! `ArmFailSafe` (arm, extend, and the `(0, 0)` disarm), `SetRegulatoryConfig`,
//! `AddOrUpdateWiFiNetwork`, `AddOrUpdateThreadNetwork`, `ConnectNetwork` (SSID
//! or Extended PAN ID) and `CommissioningComplete`. The flow does not send
//! `ScanNetworks`, so there is no hand-written encoder to compare it with.
//!
//! The decoders deliberately differ in strictness, and the test pins that
//! too, so a change to either is a decision rather than an accident: the
//! hand-written ones accept a response without `DebugText` (or, for
//! `ConnectNetworkResponse`, without `ErrorValue`), and a
//! `BasicCommissioningInfo` without `MaxCumulativeFailsafeSeconds`; the
//! generated ones follow the specification, chip and matter.js, where all
//! three are mandatory. chip's server always sends them (its generated
//! response and struct encoders write every non-optional field, at v1.4.2.0
//! and at master `5cd2917a`).
//!
//! `matter-clusters` is a dev-dependency here only (`Cargo.toml`), so this
//! test adds nothing to the commissioning crate's runtime graph.

// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![forbid(unsafe_code)]

use matter_clusters::clusters::{general_commissioning as gen_gc, network_commissioning as gen_nc};
use matter_clusters::error::ClusterError;
use matter_clusters::types::{Nullable, Secret};
use matter_codec::{Tag, TlvWriter};
use matter_commissioning::clusters::{
    general_commissioning as hand_gc, network_commissioning as hand_nc,
};
use matter_commissioning::state_machine::Stage;

/// One anonymous struct written by `write_fields`.
fn struct_of(write_fields: &dyn Fn(&mut TlvWriter<'_>)) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        write_fields(&mut w);
        w.end_container().unwrap();
    }
    buf
}

#[test]
fn ids_agree() {
    assert_eq!(hand_gc::CLUSTER_ID, gen_gc::CLUSTER_ID);
    assert_eq!(hand_nc::CLUSTER_ID, gen_nc::CLUSTER_ID);
    assert_eq!(
        [
            hand_gc::command_id::ARM_FAIL_SAFE,
            hand_gc::command_id::SET_REGULATORY_CONFIG,
            hand_gc::command_id::COMMISSIONING_COMPLETE
        ],
        [
            gen_gc::command_id::ARM_FAIL_SAFE,
            gen_gc::command_id::SET_REGULATORY_CONFIG,
            gen_gc::command_id::COMMISSIONING_COMPLETE
        ]
    );
    assert_eq!(
        [
            hand_nc::command_id::ADD_OR_UPDATE_WIFI_NETWORK,
            hand_nc::command_id::ADD_OR_UPDATE_THREAD_NETWORK,
            hand_nc::command_id::CONNECT_NETWORK,
            hand_nc::response_id::NETWORK_CONFIG_RESPONSE,
            hand_nc::response_id::CONNECT_NETWORK_RESPONSE,
            hand_nc::attribute_id::CONNECT_MAX_TIME_SECONDS
        ],
        [
            gen_nc::command_id::ADD_OR_UPDATE_WIFI_NETWORK,
            gen_nc::command_id::ADD_OR_UPDATE_THREAD_NETWORK,
            gen_nc::command_id::CONNECT_NETWORK,
            gen_nc::command_id::NETWORK_CONFIG_RESPONSE,
            gen_nc::command_id::CONNECT_NETWORK_RESPONSE,
            gen_nc::attribute_id::CONNECT_MAX_TIME_SECONDS
        ]
    );
}

#[test]
fn general_commissioning_requests_encode_identically() {
    for (expiry, breadcrumb) in [(0, 0), (60, 1), (900, u64::MAX)] {
        assert_eq!(
            hand_gc::encode_arm_fail_safe(expiry, breadcrumb),
            gen_gc::encode_arm_fail_safe(expiry, breadcrumb),
            "ArmFailSafe({expiry}, {breadcrumb})"
        );
    }
    let locations = [
        (
            hand_gc::RegulatoryLocation::Indoor,
            gen_gc::RegulatoryLocationTypeEnum::Indoor,
        ),
        (
            hand_gc::RegulatoryLocation::Outdoor,
            gen_gc::RegulatoryLocationTypeEnum::Outdoor,
        ),
        (
            hand_gc::RegulatoryLocation::IndoorOutdoor,
            gen_gc::RegulatoryLocationTypeEnum::IndoorOutdoor,
        ),
    ];
    for (hand, generated) in locations {
        assert_eq!(
            hand_gc::encode_set_regulatory_config(hand, "XX", 2),
            gen_gc::encode_set_regulatory_config(generated, &"XX".to_string(), 2),
            "SetRegulatoryConfig({hand:?})"
        );
    }
    assert_eq!(
        hand_gc::encode_commissioning_complete(),
        gen_gc::encode_commissioning_complete()
    );
}

#[test]
fn network_commissioning_requests_encode_identically() {
    // The commissioning flow always sends Breadcrumb; the generated encoder
    // takes it as an Option (optional in the specification).
    let ssid = b"matter-test".to_vec();
    let credentials = b"correct horse".to_vec();
    assert_eq!(
        hand_nc::encode_add_or_update_wifi_network(&ssid, &credentials, 3),
        gen_nc::encode_add_or_update_wifi_network(
            &ssid,
            &Secret::new(credentials.clone()),
            Some(3)
        )
    );
    let dataset = vec![0x0e, 0x08, 0, 0, 0, 0, 0, 1, 0, 0];
    assert_eq!(
        hand_nc::encode_add_or_update_thread_network(&dataset, 4),
        gen_nc::encode_add_or_update_thread_network(&Secret::new(dataset.clone()), Some(4))
    );
    assert_eq!(
        hand_nc::encode_connect_network(&ssid, 5),
        gen_nc::encode_connect_network(&ssid, Some(5))
    );
}

#[test]
fn network_commissioning_requests_encode_identically_across_the_flows_inputs() {
    // The commissioner (state_machine/commissioner.rs) sends ConnectNetwork
    // with the SSID for Wi-Fi and the 8-byte Extended PAN ID for Thread, and
    // its breadcrumb counter can take any u64. An open Wi-Fi network has empty
    // credentials. The assertions compare bytes without printing them on
    // success; the inputs are synthetic, not real network secrets.
    let ssid = b"matter-test".to_vec();
    let ext_pan_id = vec![0x78, 0x96, 0x21, 0x7f, 0x78, 0x7f, 0x6e, 0xbe];
    let dataset = vec![0x0e, 0x08, 0, 0, 0, 0, 0, 1, 0, 0];
    for breadcrumb in [0, 1, 0xff, 0x1_0000, u64::MAX] {
        for credentials in [Vec::new(), b"correct horse".to_vec()] {
            assert!(
                hand_nc::encode_add_or_update_wifi_network(&ssid, &credentials, breadcrumb)
                    == gen_nc::encode_add_or_update_wifi_network(
                        &ssid,
                        &Secret::new(credentials.clone()),
                        Some(breadcrumb)
                    ),
                "AddOrUpdateWiFiNetwork, breadcrumb {breadcrumb}, {} credential bytes",
                credentials.len()
            );
        }
        assert!(
            hand_nc::encode_add_or_update_thread_network(&dataset, breadcrumb)
                == gen_nc::encode_add_or_update_thread_network(
                    &Secret::new(dataset.clone()),
                    Some(breadcrumb)
                ),
            "AddOrUpdateThreadNetwork, breadcrumb {breadcrumb}"
        );
        for network_id in [&ssid, &ext_pan_id] {
            assert_eq!(
                hand_nc::encode_connect_network(network_id, breadcrumb),
                gen_nc::encode_connect_network(network_id, Some(breadcrumb)),
                "ConnectNetwork, breadcrumb {breadcrumb}"
            );
        }
    }
}

#[test]
fn chip_shaped_responses_decode_alike() {
    // chip's server sends ErrorCode and DebugText (always empty) in
    // ArmFailSafeResponse, SetRegulatoryConfigResponse and
    // CommissioningCompleteResponse. The NetworkCommissioning responses below
    // carry synthetic values.
    let arm = struct_of(&|w| {
        w.put_uint(Tag::Context(0), 4).unwrap();
        w.put_utf8(Tag::Context(1), "").unwrap();
    });
    let hand = hand_gc::decode_arm_fail_safe_response(&arm).unwrap();
    let generated = gen_gc::ArmFailSafeResponse::decode(&arm).unwrap();
    assert_eq!(hand.error_code, generated.error_code.to_raw());
    assert_eq!(
        generated.error_code,
        gen_gc::CommissioningErrorEnum::BusyWithOtherAdmin
    );
    assert_eq!(
        hand.debug_text.as_deref(),
        Some(generated.debug_text.as_str())
    );

    let regulatory = struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_utf8(Tag::Context(1), "").unwrap();
    });
    let hand = hand_gc::decode_set_regulatory_config_response(&regulatory).unwrap();
    let generated = gen_gc::SetRegulatoryConfigResponse::decode(&regulatory).unwrap();
    assert_eq!(hand.error_code, generated.error_code.to_raw());
    assert_eq!(
        generated.error_code,
        gen_gc::CommissioningErrorEnum::ValueOutsideRange
    );
    assert_eq!(
        hand.debug_text.as_deref(),
        Some(generated.debug_text.as_str())
    );

    let config = struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_utf8(Tag::Context(1), "added").unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
    });
    let hand = hand_nc::decode_network_config_response(Stage::NetworkSetup, &config).unwrap();
    let generated = gen_nc::NetworkConfigResponse::decode(&config).unwrap();
    assert_eq!(hand.networking_status, generated.networking_status.to_raw());
    assert_eq!(hand.debug_text, generated.debug_text);
    assert_eq!(generated.network_index, Some(1));

    let connect = struct_of(&|w| {
        w.put_uint(Tag::Context(0), 7).unwrap();
        w.put_int(Tag::Context(2), -3).unwrap();
    });
    let hand = hand_nc::decode_connect_network_response(Stage::NetworkEnable, &connect).unwrap();
    let generated = gen_nc::ConnectNetworkResponse::decode(&connect).unwrap();
    assert_eq!(hand.networking_status, generated.networking_status.to_raw());
    assert_eq!(
        generated.networking_status,
        gen_nc::NetworkCommissioningStatusEnum::AuthFailure
    );
    assert_eq!(hand.error_value, Some(-3));
    assert_eq!(generated.error_value, Nullable::Value(-3));

    let info = struct_of(&|w| {
        w.put_uint(Tag::Context(0), 60).unwrap();
        w.put_uint(Tag::Context(1), 900).unwrap();
    });
    let hand = hand_gc::decode_basic_commissioning_info(&info).unwrap();
    let generated = gen_gc::decode_basic_commissioning_info(&info).unwrap();
    assert_eq!(
        (
            hand.failsafe_expiry_length_seconds,
            hand.max_cumulative_failsafe_seconds
        ),
        (
            generated.fail_safe_expiry_length_seconds,
            generated.max_cumulative_failsafe_seconds
        )
    );
}

#[test]
fn the_commissioning_decoders_stay_more_lenient_than_the_generated_ones() {
    // DebugText missing: the commissioning flow carries on; the generated
    // decoder reports the mandatory field.
    let bare = struct_of(&|w| w.put_uint(Tag::Context(0), 0).unwrap());
    assert_eq!(
        hand_gc::decode_arm_fail_safe_response(&bare)
            .unwrap()
            .debug_text,
        None
    );
    assert!(matches!(
        gen_gc::ArmFailSafeResponse::decode(&bare),
        Err(ClusterError::MissingField("DebugText"))
    ));
    assert_eq!(
        hand_gc::decode_set_regulatory_config_response(&bare)
            .unwrap()
            .debug_text,
        None
    );
    assert!(matches!(
        gen_gc::SetRegulatoryConfigResponse::decode(&bare),
        Err(ClusterError::MissingField("DebugText"))
    ));
    // ErrorValue missing: likewise (mandatory and nullable in the
    // specification).
    let hand = hand_nc::decode_connect_network_response(Stage::NetworkEnable, &bare).unwrap();
    assert_eq!(hand.error_value, None);
    assert!(matches!(
        gen_nc::ConnectNetworkResponse::decode(&bare),
        Err(ClusterError::MissingField("ErrorValue"))
    ));
    // BasicCommissioningInfo without MaxCumulativeFailsafeSeconds (mandatory
    // since Matter 1.0; chip's server always sends it): the commissioning flow
    // reads it as 0, the generated decoder reports the mandatory field.
    let expiry_only = struct_of(&|w| w.put_uint(Tag::Context(0), 60).unwrap());
    let hand = hand_gc::decode_basic_commissioning_info(&expiry_only).unwrap();
    assert_eq!(
        (
            hand.failsafe_expiry_length_seconds,
            hand.max_cumulative_failsafe_seconds
        ),
        (60, 0)
    );
    assert!(matches!(
        gen_gc::decode_basic_commissioning_info(&expiry_only),
        Err(ClusterError::MissingField("MaxCumulativeFailsafeSeconds"))
    ));
}
