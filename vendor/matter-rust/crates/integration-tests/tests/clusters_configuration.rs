// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! The node-configuration clusters on a live all-clusters-app: the
//! three localization clusters and PowerSourceConfiguration (endpoint 0), and
//! Actions (endpoint 1). all-clusters serves all five at master 5cd2917a and
//! v1.4.2.0 (the nightly's pin), with the same attribute lists.
//!
//! The servers differ between the refs (master's are code-driven clusters,
//! v1.4.2.0's ember callbacks and an AttributeAccessInterface). The values
//! they start at are an app's choice (all-clusters' `ApplicationInit` sets
//! UnitLocalization to Fahrenheit at both refs, over the server's Celsius
//! default), so the writes pick their values from what the device reports (a
//! supported value other than the current one; a value outside the supported
//! list) and restore the original. Both refs refuse an unsupported value with
//! ConstraintError (v1.4.2.0's `Status::InvalidValue` is an alias of
//! ConstraintError).

use integration_tests::dut::DutConfig;
use integration_tests::sweep::{
    assert_exact_attribute_ids, attribute_tlv, decode_every_attribute, invoke_for_status,
    newer_than_codegen, ok, read_cluster_attributes, write_attribute,
};
use matter_clusters::clusters::{
    actions, localization_configuration as locale, power_source_configuration as sources,
    time_format_localization as time_format, unit_localization as units,
};
use matter_controller::{AttributePath, CommandPath, ImStatus, MatterController, Node};

/// IM status codes the servers answer with (Matter Core spec 8.10.1).
const INVALID_COMMAND: ImStatus = ImStatus::Failure(0x85);
const CONSTRAINT_ERROR: ImStatus = ImStatus::Failure(0x87);
const NOT_FOUND: ImStatus = ImStatus::Failure(0x8B);

/// The DUT config, controller and node id, or `None` (skip) unless the DUT is
/// all-clusters.
async fn connect_all_clusters() -> Option<(DutConfig, MatterController, u64)> {
    let cfg = DutConfig::from_env()?;
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: configuration sweep needs the all-clusters DUT (`just integration`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((cfg, controller, node_id))
}

/// Write one endpoint-0 attribute and return the status.
async fn write0(node: &Node, cluster: u32, attribute: u32, tlv: Vec<u8>) -> ImStatus {
    let at = AttributePath {
        endpoint: 0,
        cluster,
        attribute,
    };
    write_attribute(node, at, tlv).await.unwrap()
}

/// One endpoint-0 attribute, read fresh, as its value TLV.
async fn read0(node: &Node, cluster: u32, attribute: u32) -> Vec<u8> {
    let attrs = read_cluster_attributes(node, 0, cluster).await.unwrap();
    attribute_tlv(&attrs, attribute).to_vec()
}

/// LocalizationConfiguration: both attributes decode; ActiveLocale is one of
/// SupportedLocales. Writing another supported locale succeeds, a locale
/// outside the list is refused (ConstraintError) and changes nothing, and the
/// original is restored.
#[tokio::test]
async fn localization_configuration_writes_only_a_supported_locale() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use locale::attribute_id as a;
    let attrs = decode_every_attribute(&node, 0, locale::CLUSTER_ID, |id, t| match id {
        a::ACTIVE_LOCALE => ok(locale::decode_active_locale(t)),
        a::SUPPORTED_LOCALES => ok(locale::decode_supported_locales(t)),
        other => newer_than_codegen("LocalizationConfiguration", other),
    })
    .await;
    assert_exact_attribute_ids("LocalizationConfiguration", &attrs, &[0, 1]);
    let supported =
        locale::decode_supported_locales(attribute_tlv(&attrs, a::SUPPORTED_LOCALES)).unwrap();
    let original = locale::decode_active_locale(attribute_tlv(&attrs, a::ACTIVE_LOCALE)).unwrap();
    assert!(
        supported.contains(&original),
        "{original} not in {supported:?}"
    );
    let other = supported
        .iter()
        .find(|l| **l != original)
        .expect("all-clusters supports more than one locale")
        .clone();
    let active = || async {
        locale::decode_active_locale(&read0(&node, locale::CLUSTER_ID, a::ACTIVE_LOCALE).await)
            .unwrap()
    };
    let write = |value: &str| {
        write0(
            &node,
            locale::CLUSTER_ID,
            a::ACTIVE_LOCALE,
            locale::encode_active_locale(&value.to_string()),
        )
    };
    assert_eq!(write(&other).await, ImStatus::Success);
    assert_eq!(active().await, other);
    assert_eq!(write("xx-YY").await, CONSTRAINT_ERROR);
    assert_eq!(active().await, other, "a refused write changes nothing");
    assert_eq!(write(&original).await, ImStatus::Success);
    assert_eq!(active().await, original);
}

/// TimeFormatLocalization (CALFMT): every attribute decodes. HourFormat
/// toggles between 12 h and 24 h and back; an unknown hour format is refused.
/// A calendar type outside SupportedCalendarTypes is refused; the active one
/// is accepted.
#[tokio::test]
async fn time_format_localization_writes_hour_format_and_calendar() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use time_format::{attribute_id as a, CalendarTypeEnum, HourFormatEnum};
    let attrs = decode_every_attribute(&node, 0, time_format::CLUSTER_ID, |id, t| match id {
        a::HOUR_FORMAT => ok(time_format::decode_hour_format(t)),
        a::ACTIVE_CALENDAR_TYPE => ok(time_format::decode_active_calendar_type(t)),
        a::SUPPORTED_CALENDAR_TYPES => ok(time_format::decode_supported_calendar_types(t)),
        other => newer_than_codegen("TimeFormatLocalization", other),
    })
    .await;
    assert_exact_attribute_ids("TimeFormatLocalization", &attrs, &[0, 1, 2]);
    let tlv = |id| attribute_tlv(&attrs, id);
    let hour = time_format::decode_hour_format(tlv(a::HOUR_FORMAT)).unwrap();
    let toggled = if hour == HourFormatEnum::Hr24 {
        HourFormatEnum::Hr12
    } else {
        HourFormatEnum::Hr24
    };
    let write_hour = |h| {
        write0(
            &node,
            time_format::CLUSTER_ID,
            a::HOUR_FORMAT,
            time_format::encode_hour_format(h),
        )
    };
    assert_eq!(write_hour(toggled).await, ImStatus::Success);
    let read_hour = || async {
        time_format::decode_hour_format(
            &read0(&node, time_format::CLUSTER_ID, a::HOUR_FORMAT).await,
        )
        .unwrap()
    };
    assert_eq!(read_hour().await, toggled);
    assert_eq!(
        write_hour(HourFormatEnum::Unknown(7)).await,
        CONSTRAINT_ERROR
    );
    assert_eq!(
        read_hour().await,
        toggled,
        "a refused write changes nothing"
    );
    assert_eq!(write_hour(hour).await, ImStatus::Success);
    assert_eq!(read_hour().await, hour);

    let supported =
        time_format::decode_supported_calendar_types(tlv(a::SUPPORTED_CALENDAR_TYPES)).unwrap();
    let active = time_format::decode_active_calendar_type(tlv(a::ACTIVE_CALENDAR_TYPE)).unwrap();
    assert!(
        supported.contains(&active),
        "{active:?} not in {supported:?}"
    );
    let unsupported = (0..=11)
        .map(CalendarTypeEnum::from_raw)
        .find(|c| !supported.contains(c))
        .expect("all-clusters does not support every calendar");
    let write_calendar = |c| {
        write0(
            &node,
            time_format::CLUSTER_ID,
            a::ACTIVE_CALENDAR_TYPE,
            time_format::encode_active_calendar_type(c),
        )
    };
    let read_calendar = || async {
        time_format::decode_active_calendar_type(
            &read0(&node, time_format::CLUSTER_ID, a::ACTIVE_CALENDAR_TYPE).await,
        )
        .unwrap()
    };
    assert_eq!(write_calendar(unsupported).await, CONSTRAINT_ERROR);
    assert_eq!(
        read_calendar().await,
        active,
        "a refused write changes nothing"
    );
    assert_eq!(write_calendar(active).await, ImStatus::Success);
    assert_eq!(read_calendar().await, active);
}

/// UnitLocalization (TEMP): both attributes decode; TemperatureUnit is one of
/// SupportedTemperatureUnits. Another supported unit is accepted and the
/// original restored; an unknown unit is refused.
#[tokio::test]
async fn unit_localization_writes_only_a_supported_unit() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use units::{attribute_id as a, TempUnitEnum};
    let attrs = decode_every_attribute(&node, 0, units::CLUSTER_ID, |id, t| match id {
        a::TEMPERATURE_UNIT => ok(units::decode_temperature_unit(t)),
        a::SUPPORTED_TEMPERATURE_UNITS => ok(units::decode_supported_temperature_units(t)),
        other => newer_than_codegen("UnitLocalization", other),
    })
    .await;
    assert_exact_attribute_ids("UnitLocalization", &attrs, &[0, 1]);
    let supported = units::decode_supported_temperature_units(attribute_tlv(
        &attrs,
        a::SUPPORTED_TEMPERATURE_UNITS,
    ))
    .unwrap();
    let original =
        units::decode_temperature_unit(attribute_tlv(&attrs, a::TEMPERATURE_UNIT)).unwrap();
    assert!(
        supported.contains(&original),
        "{original:?} not in {supported:?}"
    );
    let other = *supported
        .iter()
        .find(|u| **u != original)
        .expect("all-clusters supports at least two units");
    let write = |u| {
        write0(
            &node,
            units::CLUSTER_ID,
            a::TEMPERATURE_UNIT,
            units::encode_temperature_unit(u),
        )
    };
    let read = || async {
        units::decode_temperature_unit(&read0(&node, units::CLUSTER_ID, a::TEMPERATURE_UNIT).await)
            .unwrap()
    };
    assert_eq!(write(other).await, ImStatus::Success);
    assert_eq!(read().await, other);
    assert_eq!(write(TempUnitEnum::Unknown(9)).await, CONSTRAINT_ERROR);
    assert_eq!(read().await, other, "a refused write changes nothing");
    assert_eq!(write(original).await, ImStatus::Success);
    assert_eq!(read().await, original);
}

/// PowerSourceConfiguration: Sources lists the endpoints that carry
/// PowerSource, sorted by their Order (chip's
/// power-source-configuration-server.cpp, unchanged between the refs).
/// all-clusters' Orders are 3 on endpoint 0, 2 on endpoint 1 and 1 on
/// endpoint 2 at both refs (all-clusters-app.matter defaults).
#[tokio::test]
async fn power_source_configuration_lists_sources_by_order() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    let attrs = decode_every_attribute(&node, 0, sources::CLUSTER_ID, |id, t| match id {
        sources::attribute_id::SOURCES => ok(sources::decode_sources(t)),
        other => newer_than_codegen("PowerSourceConfiguration", other),
    })
    .await;
    assert_exact_attribute_ids("PowerSourceConfiguration", &attrs, &[0]);
    assert_eq!(
        sources::decode_sources(attribute_tlv(&attrs, sources::attribute_id::SOURCES)).unwrap(),
        [2, 1, 0]
    );
}

/// Actions on endpoint 1: all-clusters' stub delegate
/// (all-clusters-common/include/bridged-actions-stub.h, the same at both refs)
/// serves three scene actions over three endpoint lists, none with a
/// supported command. chip's server refuses a known action whose
/// SupportedCommands lacks the command with InvalidCommand, and an unknown
/// action with NotFound, before the delegate runs (master ActionsCluster.cpp,
/// v1.4.2.0 actions-server.cpp).
#[tokio::test]
async fn actions_decode_the_stub_and_refuse_its_commands() {
    let Some((_, controller, node_id)) = connect_all_clusters().await else {
        return;
    };
    let node = controller.node(node_id);
    use actions::{attribute_id as a, command_id as c, ActionStateEnum, ActionTypeEnum};
    let attrs = decode_every_attribute(&node, 1, actions::CLUSTER_ID, |id, t| match id {
        a::ACTION_LIST => ok(actions::decode_action_list(t)),
        a::ENDPOINT_LISTS => ok(actions::decode_endpoint_lists(t)),
        a::SETUP_URL => ok(actions::decode_setup_url(t)),
        other => newer_than_codegen("Actions", other),
    })
    .await;
    assert_exact_attribute_ids("Actions", &attrs, &[0, 1, 2]);
    let list = actions::decode_action_list(attribute_tlv(&attrs, a::ACTION_LIST)).unwrap();
    let got: Vec<(u16, &str, u16)> = list
        .iter()
        .map(|x| (x.action_id, x.name.as_str(), x.endpoint_list_id))
        .collect();
    assert_eq!(
        got,
        [
            (0, "TurnOnLight", 0),
            (1, "TurnOffLight", 1),
            (2, "ToggleLight", 2)
        ]
    );
    for x in &list {
        assert_eq!(
            (x.r#type, x.supported_commands.bits(), x.state),
            (ActionTypeEnum::Scene, 0, ActionStateEnum::Inactive)
        );
    }
    let lists = actions::decode_endpoint_lists(attribute_tlv(&attrs, a::ENDPOINT_LISTS)).unwrap();
    let got: Vec<(u16, &str, Vec<u16>)> = lists
        .iter()
        .map(|l| (l.endpoint_list_id, l.name.as_str(), l.endpoints.clone()))
        .collect();
    assert_eq!(
        got,
        [
            (0, "On", vec![1]),
            (1, "Off", vec![1, 2]),
            (2, "Toggle", vec![1, 2, 3])
        ]
    );
    assert_eq!(
        actions::decode_setup_url(attribute_tlv(&attrs, a::SETUP_URL)).unwrap(),
        "https://example.com"
    );
    let invoke = |command, fields| {
        let path = CommandPath {
            endpoint: 1,
            cluster: actions::CLUSTER_ID,
            command,
        };
        let node = &node;
        async move { invoke_for_status(node, path, fields).await.unwrap() }
    };
    assert_eq!(
        invoke(c::INSTANT_ACTION, actions::encode_instant_action(0, None)).await,
        INVALID_COMMAND
    );
    assert_eq!(
        invoke(
            c::INSTANT_ACTION,
            actions::encode_instant_action(7, Some(9))
        )
        .await,
        NOT_FOUND
    );
    assert_eq!(
        invoke(
            c::START_ACTION_WITH_DURATION,
            actions::encode_start_action_with_duration(1, None, 30)
        )
        .await,
        INVALID_COMMAND
    );
}
