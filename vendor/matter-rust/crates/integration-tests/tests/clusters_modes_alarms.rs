// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! The mode and alarm clusters on a live all-clusters-app (endpoint 1): every
//! attribute of each hosted mode or alarm cluster decoded with the generated
//! decoder, one safe command per cluster, and the alarm Notify events.
//!
//! Hosted here: ModeSelect, OvenMode, LaundryWasherMode,
//! RefrigeratorAndTemperatureControlledCabinetMode, RvcRunMode, RvcCleanMode,
//! DishwasherMode, MicrowaveOvenMode, DishwasherAlarm, RefrigeratorAlarm,
//! HepaFilterMonitoring, ActivatedCarbonFilterMonitoring. At v1.4.2.0 (the
//! nightly's pin) all-clusters also serves EnergyEvseMode, WaterHeaterMode and
//! DeviceEnergyManagementMode on endpoint 1; master dropped them, so they are
//! required and swept where the checkout's `all-clusters-app.matter` serves
//! them (the nightly is WaterHeaterMode's only live coverage), and skipped
//! with a log line where it does not. evse-app covers EnergyEvseMode and
//! DeviceEnergyManagementMode locally (`clusters_electrical.rs`).
//! WaterTankLevelMonitoring has no chip host.
//!
//! Safe commands, chosen from what is generated:
//! - ModeBase `ChangeToMode(CurrentMode)`: chip answers Success without
//!   calling the delegate (ModeBaseCluster.cpp `HandleChangeToMode`), and
//!   leaves StatusText out of that reply (so it is optional; only a
//!   delegate's reply may carry it). MicrowaveOvenMode has no ChangeToMode.
//! - ModeSelect `ChangeToMode(CurrentMode)`: a bare Success.
//! - DishwasherAlarm `Reset(InflowError)`: clears one State bit and emits
//!   Notify (dishwasher-alarm-server.cpp `ResetLatchedAlarms`).
//! - RefrigeratorAlarm: no command is generated (Reset is RESET-gated and
//!   RESET is disallowed; ModifyEnabledAlarms is disallowed). Its Notify comes
//!   from the app pipe `SetRefrigeratorDoorStatus`.
//! - Hepa / ActivatedCarbon `ResetCondition`: Condition back to 100 and
//!   ChangeIndication Ok (ResourceMonitoringCluster.cpp `OnResetCondition`).

use integration_tests::dut::DutConfig;
use integration_tests::events::{
    latest_event_number, payload_tlv, send_app_pipe, wait_for_event_after,
};
use integration_tests::sweep::{
    all_clusters_serves, attribute_ids, attribute_tlv, decode_every_attribute, invoke_for_response,
    invoke_for_status, newer_than_codegen, ok, read_cluster_attributes, standard_attribute_ids,
};
use matter_clusters::clusters::{
    activated_carbon_filter_monitoring, descriptor, device_energy_management_mode,
    dishwasher_alarm, dishwasher_mode, energy_evse_mode, hepa_filter_monitoring,
    laundry_washer_mode, microwave_oven_mode, mode_select, oven_mode, refrigerator_alarm,
    refrigerator_and_temperature_controlled_cabinet_mode, rvc_clean_mode, rvc_run_mode,
    water_heater_mode,
};
use matter_clusters::types::Nullable;
use matter_controller::{CommandPath, ImStatus, MatterController};

/// Every mode and alarm cluster all-clusters serves lives on endpoint 1.
const EP: u16 = 1;

/// The DUT config, controller and node id, or `None` (skip) unless the DUT is
/// all-clusters.
async fn connect_all_clusters() -> Option<(DutConfig, MatterController, u64)> {
    let cfg = DutConfig::from_env()?;
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: mode and alarm sweep needs the all-clusters DUT (`just integration`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((cfg, controller, node_id))
}

/// The six ModeBase derivatives with ChangeToMode, plus MicrowaveOvenMode
/// (attributes only).
#[tokio::test]
async fn mode_base_clusters_decode_and_change_to_current_mode() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    integration_tests::sweep_mode_base!(&node, EP, oven_mode);
    integration_tests::sweep_mode_base!(&node, EP, laundry_washer_mode);
    integration_tests::sweep_mode_base!(
        &node,
        EP,
        refrigerator_and_temperature_controlled_cabinet_mode
    );
    integration_tests::sweep_mode_base!(&node, EP, rvc_run_mode);
    integration_tests::sweep_mode_base!(&node, EP, rvc_clean_mode);
    integration_tests::sweep_mode_base!(&node, EP, dishwasher_mode);

    use microwave_oven_mode::attribute_id as mw;
    let attrs = decode_every_attribute(
        &node,
        EP,
        microwave_oven_mode::CLUSTER_ID,
        |id, t| match id {
            mw::SUPPORTED_MODES => ok(microwave_oven_mode::decode_supported_modes(t)),
            mw::CURRENT_MODE => ok(microwave_oven_mode::decode_current_mode(t)),
            other => newer_than_codegen("MicrowaveOvenMode", other),
        },
    )
    .await;
    let ids = attribute_ids(&attrs);
    assert!(ids.contains(&mw::SUPPORTED_MODES) && ids.contains(&mw::CURRENT_MODE));
}

/// EnergyEvseMode, WaterHeaterMode and DeviceEnergyManagementMode, where the
/// chip checkout's `all-clusters-app.matter` serves them on endpoint 1
/// (v1.4.2.0: yes; master: no). Each one the source serves must be in
/// endpoint 1's Descriptor ServerList and gets the full ModeBase sweep, so a
/// missing cluster fails instead of passing vacuously.
#[tokio::test]
async fn energy_mode_clusters_decode_where_served() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    let attrs = read_cluster_attributes(&node, EP, descriptor::CLUSTER_ID)
        .await
        .unwrap();
    let servers = descriptor::decode_server_list(attribute_tlv(
        &attrs,
        descriptor::attribute_id::SERVER_LIST,
    ))
    .unwrap();
    let served = |name: &str, id: u32| {
        let listed = all_clusters_serves(&cfg, EP, name).expect("probe all-clusters-app.matter");
        if listed {
            assert!(
                servers.contains(&id),
                "{name} ({id:#06x}) is served on ep{EP} in all-clusters-app.matter but missing \
                 from the Descriptor ServerList {servers:04x?}"
            );
        } else {
            eprintln!(
                "[sweep] {name} ({id:#06x}): not served on ep{EP} by this checkout's \
                 all-clusters-app.matter; skipped"
            );
        }
        listed
    };
    if served("EnergyEvseMode", energy_evse_mode::CLUSTER_ID) {
        integration_tests::sweep_mode_base!(&node, EP, energy_evse_mode);
    }
    if served("WaterHeaterMode", water_heater_mode::CLUSTER_ID) {
        integration_tests::sweep_mode_base!(&node, EP, water_heater_mode);
    }
    if served(
        "DeviceEnergyManagementMode",
        device_energy_management_mode::CLUSTER_ID,
    ) {
        integration_tests::sweep_mode_base!(&node, EP, device_energy_management_mode);
    }
}

/// An unsupported mode: chip answers UnsupportedMode with no StatusText,
/// although 1.4 conformance makes StatusText mandatory for a non-Success
/// status ("[Status == Success], M"). This is why the dump makes it optional:
/// only an unconditional M is mandatory.
#[tokio::test]
async fn change_to_an_unsupported_mode_decodes_without_status_text() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    let attrs = read_cluster_attributes(&node, EP, dishwasher_mode::CLUSTER_ID)
        .await
        .unwrap();
    let modes = dishwasher_mode::decode_supported_modes(attribute_tlv(
        &attrs,
        dishwasher_mode::attribute_id::SUPPORTED_MODES,
    ))
    .unwrap();
    let unsupported = (0..=u8::MAX)
        .rev()
        .find(|n| modes.iter().all(|m| m.mode != *n))
        .expect("some mode number is unused");
    let resp = invoke_for_response(
        &node,
        CommandPath {
            endpoint: EP,
            cluster: dishwasher_mode::CLUSTER_ID,
            command: dishwasher_mode::command_id::CHANGE_TO_MODE,
        },
        dishwasher_mode::encode_change_to_mode(unsupported),
        dishwasher_mode::command_id::CHANGE_TO_MODE_RESPONSE,
    )
    .await
    .unwrap();
    let r =
        dishwasher_mode::ChangeToModeResponse::decode(&resp).expect("decodes without StatusText");
    assert_eq!(r.status, dishwasher_mode::ModeChangeStatus::UnsupportedMode);
    assert_eq!(r.status_text, None);
}

/// ModeSelect: chip all-clusters' "Coffee" instance (all-clusters-app.matter,
/// static-supported-modes-manager.cpp).
#[tokio::test]
async fn mode_select_decodes_and_changes_to_current_mode() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use mode_select::attribute_id as ms;
    let attrs = decode_every_attribute(&node, EP, mode_select::CLUSTER_ID, |id, t| match id {
        ms::DESCRIPTION => ok(mode_select::decode_description(t)),
        ms::STANDARD_NAMESPACE => ok(mode_select::decode_standard_namespace(t)),
        ms::SUPPORTED_MODES => ok(mode_select::decode_supported_modes(t)),
        ms::CURRENT_MODE => ok(mode_select::decode_current_mode(t)),
        ms::START_UP_MODE => ok(mode_select::decode_start_up_mode(t)),
        ms::ON_MODE => ok(mode_select::decode_on_mode(t)),
        other => newer_than_codegen("ModeSelect", other),
    })
    .await;
    // Both refs' all-clusters-app.matter serve all six; v1.4.2.0 adds the
    // vendor attribute manufacturerExtension (0xFFF1_0001), left out here.
    assert_eq!(
        standard_attribute_ids(&attrs),
        [
            ms::DESCRIPTION,
            ms::STANDARD_NAMESPACE,
            ms::SUPPORTED_MODES,
            ms::CURRENT_MODE,
            ms::START_UP_MODE,
            ms::ON_MODE,
        ],
        "ModeSelect"
    );
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        mode_select::decode_description(tlv(ms::DESCRIPTION)).unwrap(),
        "Coffee"
    );
    assert_eq!(
        mode_select::decode_standard_namespace(tlv(ms::STANDARD_NAMESPACE)).unwrap(),
        Nullable::Value(0)
    );
    let labels: Vec<String> = mode_select::decode_supported_modes(tlv(ms::SUPPORTED_MODES))
        .unwrap()
        .into_iter()
        .map(|m| m.label)
        .collect();
    assert_eq!(labels, ["Black", "Cappuccino", "Espresso"]);
    let current = mode_select::decode_current_mode(tlv(ms::CURRENT_MODE)).unwrap();
    let status = invoke_for_status(
        &node,
        CommandPath {
            endpoint: EP,
            cluster: mode_select::CLUSTER_ID,
            command: mode_select::command_id::CHANGE_TO_MODE,
        },
        mode_select::encode_change_to_mode(current),
    )
    .await
    .unwrap();
    assert_eq!(status, ImStatus::Success);
}

/// DishwasherAlarm: attributes, then `Reset(InflowError)` and the Notify it
/// causes, checked against the State read just before.
#[tokio::test]
async fn dishwasher_alarm_reset_emits_notify() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use dishwasher_alarm::{attribute_id as da, AlarmBitmap, NotifyEvent};
    let attrs = decode_every_attribute(&node, EP, dishwasher_alarm::CLUSTER_ID, |id, t| match id {
        da::MASK => ok(dishwasher_alarm::decode_mask(t)),
        da::LATCH => ok(dishwasher_alarm::decode_latch(t)),
        da::STATE => ok(dishwasher_alarm::decode_state(t)),
        da::SUPPORTED => ok(dishwasher_alarm::decode_supported(t)),
        other => newer_than_codegen("DishwasherAlarm", other),
    })
    .await;
    assert_eq!(
        attribute_ids(&attrs),
        [da::MASK, da::LATCH, da::STATE, da::SUPPORTED]
    );
    let state = dishwasher_alarm::decode_state(attribute_tlv(&attrs, da::STATE)).unwrap();
    let mask = dishwasher_alarm::decode_mask(attribute_tlv(&attrs, da::MASK)).unwrap();

    let ev = dishwasher_alarm::event_id::NOTIFY;
    let baseline = latest_event_number(&node, EP, dishwasher_alarm::CLUSTER_ID, ev)
        .await
        .unwrap();
    let status = invoke_for_status(
        &node,
        CommandPath {
            endpoint: EP,
            cluster: dishwasher_alarm::CLUSTER_ID,
            command: dishwasher_alarm::command_id::RESET,
        },
        dishwasher_alarm::encode_reset(AlarmBitmap::INFLOW_ERROR),
    )
    .await
    .unwrap();
    assert_eq!(status, ImStatus::Success);
    let items = wait_for_event_after(&node, EP, dishwasher_alarm::CLUSTER_ID, ev, baseline)
        .await
        .unwrap();
    let last = items.iter().max_by_key(|i| i.event_number).unwrap();
    let n = NotifyEvent::decode(&payload_tlv(&last.value)).expect("Notify decodes");
    assert_eq!(n.active, AlarmBitmap::empty());
    assert_eq!(n.inactive, state & AlarmBitmap::INFLOW_ERROR);
    assert_eq!(n.state, state - AlarmBitmap::INFLOW_ERROR);
    assert_eq!(n.mask, mask);
}

/// RefrigeratorAlarm: attributes, then the Notify chip emits for app-pipe
/// `SetRefrigeratorDoorStatus` DoorOpen=1 (AllClustersCommandDelegate.cpp:
/// Mask and State both become DoorOpen). The door is left open.
#[tokio::test]
async fn refrigerator_alarm_door_open_emits_notify() {
    let Some((cfg, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use refrigerator_alarm::{attribute_id as ra, AlarmBitmap, NotifyEvent};
    let attrs = decode_every_attribute(
        &node,
        EP,
        refrigerator_alarm::CLUSTER_ID,
        |id, t| match id {
            ra::MASK => ok(refrigerator_alarm::decode_mask(t)),
            ra::STATE => ok(refrigerator_alarm::decode_state(t)),
            ra::SUPPORTED => ok(refrigerator_alarm::decode_supported(t)),
            other => newer_than_codegen("RefrigeratorAlarm", other),
        },
    )
    .await;
    assert_eq!(attribute_ids(&attrs), [ra::MASK, ra::STATE, ra::SUPPORTED]);
    let state = refrigerator_alarm::decode_state(attribute_tlv(&attrs, ra::STATE)).unwrap();

    let ev = refrigerator_alarm::event_id::NOTIFY;
    let baseline = latest_event_number(&node, EP, refrigerator_alarm::CLUSTER_ID, ev)
        .await
        .unwrap();
    send_app_pipe(
        &cfg,
        r#"{"Name": "SetRefrigeratorDoorStatus", "EndpointId": 1, "DoorOpen": 1}"#,
    )
    .await
    .expect("app pipe");
    let items = wait_for_event_after(&node, EP, refrigerator_alarm::CLUSTER_ID, ev, baseline)
        .await
        .unwrap();
    let last = items.iter().max_by_key(|i| i.event_number).unwrap();
    let n = NotifyEvent::decode(&payload_tlv(&last.value)).expect("Notify decodes");
    assert_eq!(n.active, AlarmBitmap::DOOR_OPEN - state);
    assert_eq!(n.inactive, state - AlarmBitmap::DOOR_OPEN);
    assert_eq!(
        (n.state, n.mask),
        (AlarmBitmap::DOOR_OPEN, AlarmBitmap::DOOR_OPEN)
    );
}

/// One ResourceMonitoring derivative on `EP`: decode every attribute, check
/// chip's first replacement product, then `ResetCondition` and read back.
macro_rules! sweep_resource_monitoring {
    ($node:expr, $m:ident) => {{
        use $m::attribute_id as rm;
        use $m::{ChangeIndicationEnum, ProductIdentifierTypeEnum};
        let attrs = decode_every_attribute($node, EP, $m::CLUSTER_ID, |id, t| match id {
            rm::CONDITION => ok($m::decode_condition(t)),
            rm::DEGRADATION_DIRECTION => ok($m::decode_degradation_direction(t)),
            rm::CHANGE_INDICATION => ok($m::decode_change_indication(t)),
            rm::IN_PLACE_INDICATOR => ok($m::decode_in_place_indicator(t)),
            rm::LAST_CHANGED_TIME => ok($m::decode_last_changed_time(t)),
            rm::REPLACEMENT_PRODUCT_LIST => ok($m::decode_replacement_product_list(t)),
            other => newer_than_codegen(stringify!($m), other),
        })
        .await;
        // Both refs' all-clusters-app.matter serve all six.
        assert_eq!(
            standard_attribute_ids(&attrs),
            [
                rm::CONDITION,
                rm::DEGRADATION_DIRECTION,
                rm::CHANGE_INDICATION,
                rm::IN_PLACE_INDICATOR,
                rm::LAST_CHANGED_TIME,
                rm::REPLACEMENT_PRODUCT_LIST,
            ],
            "{}",
            stringify!($m)
        );
        let products = $m::decode_replacement_product_list(attribute_tlv(
            &attrs,
            rm::REPLACEMENT_PRODUCT_LIST,
        ))
        .unwrap();
        assert_eq!(
            products[0].product_identifier_type,
            ProductIdentifierTypeEnum::Upc
        );
        assert_eq!(products[0].product_identifier_value, "111112222233");
        let status = invoke_for_status(
            $node,
            CommandPath {
                endpoint: EP,
                cluster: $m::CLUSTER_ID,
                command: $m::command_id::RESET_CONDITION,
            },
            $m::encode_reset_condition(),
        )
        .await
        .unwrap();
        assert_eq!(status, ImStatus::Success, "{}", stringify!($m));
        let after = read_cluster_attributes($node, EP, $m::CLUSTER_ID)
            .await
            .unwrap();
        // DegradationDirection is Down in all-clusters, so a reset is 100.
        assert_eq!(
            $m::decode_condition(attribute_tlv(&after, rm::CONDITION)).unwrap(),
            100
        );
        assert_eq!(
            $m::decode_change_indication(attribute_tlv(&after, rm::CHANGE_INDICATION)).unwrap(),
            ChangeIndicationEnum::Ok
        );
    }};
}

#[tokio::test]
async fn filter_monitoring_clusters_decode_and_reset_condition() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    sweep_resource_monitoring!(&node, hepa_filter_monitoring);
    sweep_resource_monitoring!(&node, activated_carbon_filter_monitoring);
}
