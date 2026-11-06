// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! The TV-platform clusters on a live tv-app: Channel, ApplicationLauncher
//! and ContentControl on the video-player endpoint 1, ApplicationBasic on the
//! Content App endpoints, and AccountLogin on the Vendor3 one. The Content App
//! endpoints are the static endpoint 3 and the four dynamic endpoints tv-app
//! adds at start-up for its sample apps
//! (`ApplicationInit` in linux/main.cpp calls
//! `ContentAppFactoryImpl::InstallContentApp` in tv-common/src/AppTv.cpp).
//! tv-app serves all five at master 5cd2917a and v1.4.2.0, and it is the one
//! chip example app the harness drives that does: all-clusters-minimal, the
//! placeholder apps and chef's video-player devices serve some of them, but
//! the harness drives none of those. Of chip's example apps only
//! tv-casting-app serves ContentAppObserver (both refs; tv-app's static
//! Content App endpoint 3 is its client, `binding cluster
//! ContentAppObserver`); the harness does not drive tv-casting-app, so
//! ContentAppObserver is not exercised live.
//!
//! Values come from chip's source at both refs: tv-app's delegates
//! (`examples/tv-app/tv-common/clusters/*/`, and AppTv.cpp's four Content
//! Apps) and chip's cluster servers (`src/app/clusters/*-server/`, and the
//! Content App platform in `src/app/app-platform/`). Between the refs, the
//! code these tests reach differs only in string-literal spelling, master's
//! `TEMPORARY_RETURN_IGNORED` markers and other error-handling and log-format
//! changes, the ApplicationLauncher delegate's `HandleGetCurrentApp` becoming
//! virtual, and the ContentControl delegate's UpdatePIN / AddBonusTime
//! signatures. chip's ContentControl codegen differs more (see its test).
//! Every test here ran live on a Linux arm64 Pi against tv-app built at both
//! refs.
//!
//! State left changed: none that another test reads. The Channel test ends on
//! ABC, where it started; the ApplicationLauncher test leaves the Vendor1
//! Content App Stopped with no current app, as it found them; tv-app's
//! ContentControl and AccountLogin handlers change no attribute.

use integration_tests::dut::DutConfig;
use integration_tests::events::payload_tlv;
use integration_tests::sweep::{
    assert_exact_attribute_ids, attribute_tlv, newer_than_codegen, ok, read_cluster_attributes,
};
use matter_clusters::clusters::{
    account_login as alog, application_basic as ab, application_launcher as al, channel as ch,
    content_control as cc,
};
use matter_clusters::types::{Nullable, Secret};
use matter_controller::{CommandPath, ImStatus, InvokeResult, MatterController, Node, ReadPath};

/// tv-app's video-player endpoint.
const EP: u16 = 1;

/// IM status codes the servers answer with (Matter Core spec 8.10.1).
const FAILURE: ImStatus = ImStatus::Failure(0x01);
const UNSUPPORTED_ACCESS: ImStatus = ImStatus::Failure(0x7E);
const UNSUPPORTED_COMMAND: ImStatus = ImStatus::Failure(0x81);
const INVALID_COMMAND: ImStatus = ImStatus::Failure(0x85);

/// The controller and node id, or `None` (skip) unless the DUT is tv-app.
async fn connect_tv() -> Option<(MatterController, u64)> {
    let Some(cfg) = DutConfig::from_env() else {
        eprintln!("skipped: no DUT (set MATTER_INTEGRATION_DUT via `just integration-tv`)");
        return None;
    };
    if !cfg.is_app("tv") {
        eprintln!("skipped: TV-platform tests need the tv-app DUT (`just integration-tv`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((controller, node_id))
}

/// Every attribute of `cluster` on `endpoint`, decoded by `decode`.
async fn sweep(
    node: &Node,
    endpoint: u16,
    cluster: u32,
    decode: impl Fn(u32, &[u8]) -> Result<(), String>,
) -> Vec<(u32, Vec<u8>)> {
    integration_tests::sweep::decode_every_attribute(node, endpoint, cluster, decode).await
}

/// One attribute of `cluster` on `endpoint`, read fresh.
async fn read_attr(node: &Node, endpoint: u16, cluster: u32, attribute: u32) -> Vec<u8> {
    let attrs = read_cluster_attributes(node, endpoint, cluster)
        .await
        .unwrap();
    attribute_tlv(&attrs, attribute).to_vec()
}

/// Invoke a command on `endpoint`.
async fn invoke(
    node: &Node,
    endpoint: u16,
    cluster: u32,
    command: u32,
    fields: Vec<u8>,
) -> InvokeResult {
    let path = CommandPath {
        endpoint,
        cluster,
        command,
    };
    node.invoke_tlv(path, fields).await.unwrap()
}

/// Invoke a command answered by a bare status.
async fn status(
    node: &Node,
    endpoint: u16,
    cluster: u32,
    command: u32,
    fields: Vec<u8>,
) -> ImStatus {
    match invoke(node, endpoint, cluster, command, fields).await {
        InvokeResult::Status(s) => s,
        // The path only, never the fields: `InvokeResult`'s Debug hides
        // strings only in the responses it knows carry a PIN, so an
        // unexpected response's strings would print.
        InvokeResult::Data { path, .. } => panic!(
            "{cluster:#06x}/{command:#04x}: expected a status, got response {:#04x} ({path:?})",
            path.command
        ),
        _ => panic!("{cluster:#06x}/{command:#04x}: expected a status, got another result"),
    }
}

/// Invoke a command answered by response command `response`; its payload.
async fn response(
    node: &Node,
    endpoint: u16,
    cluster: u32,
    command: u32,
    fields: Vec<u8>,
    response: u32,
) -> Vec<u8> {
    match invoke(node, endpoint, cluster, command, fields).await {
        InvokeResult::Data { path, fields } if path.command == response => payload_tlv(&fields),
        // The path only, never the fields: `InvokeResult`'s Debug hides
        // strings only in the responses it knows carry a PIN, so an
        // unexpected response's strings would print.
        InvokeResult::Data { path, .. } => panic!(
            "{cluster:#06x}/{command:#04x}: expected response {response:#04x}, got response {:#04x} ({path:?})",
            path.command
        ),
        InvokeResult::Status(s) => panic!(
            "{cluster:#06x}/{command:#04x}: expected response {response:#04x}, got status {s:?}"
        ),
        _ => panic!(
            "{cluster:#06x}/{command:#04x}: expected response {response:#04x}, got another result"
        ),
    }
}

/// `(major, minor, name, call sign, affiliate call sign)` of a channel.
fn channel_key(c: &ch::ChannelInfoStruct) -> (u16, u16, Option<&str>, Option<&str>, Option<&str>) {
    (
        c.major_number,
        c.minor_number,
        c.name.as_deref(),
        c.call_sign.as_deref(),
        c.affiliate_call_sign.as_deref(),
    )
}

/// The Channel sweep holds tv-app's four channels and its Comcast lineup
/// (ChannelManager's constructor and HandleGetLineup, both refs).
fn assert_tv_app_channels(attrs: &[(u32, Vec<u8>)]) {
    use ch::attribute_id as a;
    assert_exact_attribute_ids("Channel", attrs, &[0, 1, 2]);
    let list = ch::decode_channel_list(attribute_tlv(attrs, a::CHANNEL_LIST)).unwrap();
    let got: Vec<_> = list.iter().map(channel_key).collect();
    let kcts = (Some("KCTS-TV"), Some("KCTS"));
    assert_eq!(
        got,
        [
            (6, 0, Some("ABC"), Some("KAAL-TV"), Some("KAAL")),
            (9, 1, Some("PBS"), kcts.0, kcts.1),
            (9, 2, Some("PBS Kids"), kcts.0, kcts.1),
            (9, 3, Some("World Channel"), kcts.0, kcts.1),
        ]
    );
    let Nullable::Value(lineup) = ch::decode_lineup(attribute_tlv(attrs, a::LINEUP)).unwrap()
    else {
        panic!("Lineup is null");
    };
    assert_eq!(
        (
            lineup.operator_name.as_str(),
            lineup.lineup_name.as_deref(),
            lineup.postal_code.as_deref(),
            lineup.lineup_info_type
        ),
        (
            "Comcast",
            Some("Comcast King County"),
            Some("98052"),
            ch::LineupInfoTypeEnum::Mso
        )
    );
}

/// Channel (CL, LI; FeatureMap 3): tv-app's ChannelManager lists four
/// channels (ABC 6.0, PBS 9.1, PBS Kids 9.2, World Channel 9.3) and the Comcast
/// lineup. ChangeChannel matches a name, call sign, affiliate call sign or
/// "major.minor": "PBS" is one channel, "KCTS" (three affiliates) is
/// MultipleMatches, an unknown match NoMatches. ChangeChannelByNumber moves to
/// an existing channel and answers Failure for an unknown one (the delegate
/// returns false). SkipChannel moves `count` places modulo the list length
/// (ChannelManager `HandleSkipChannel`, both refs): one past World Channel
/// wraps to ABC, and one back from ABC wraps to World Channel. The test ends
/// on ABC, where it started.
#[tokio::test]
async fn channel_lists_and_changes_channels() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    use ch::{attribute_id as a, command_id as c};
    let attrs = sweep(&node, EP, ch::CLUSTER_ID, |id, t| match id {
        a::CHANNEL_LIST => ok(ch::decode_channel_list(t)),
        a::LINEUP => ok(ch::decode_lineup(t)),
        a::CURRENT_CHANNEL => ok(ch::decode_current_channel(t)),
        other => newer_than_codegen("Channel", other),
    })
    .await;
    assert_tv_app_channels(&attrs);
    let current = || async {
        match ch::decode_current_channel(
            &read_attr(&node, EP, ch::CLUSTER_ID, a::CURRENT_CHANNEL).await,
        )
        .unwrap()
        {
            Nullable::Value(c) => (c.major_number, c.minor_number),
            Nullable::Null => panic!("CurrentChannel is null"),
        }
    };
    assert_eq!(current().await, (6, 0));
    let change = |m: &str| {
        let node = &node;
        let m = m.to_string();
        async move {
            let r = ch::ChangeChannelResponse::decode(
                &response(
                    node,
                    EP,
                    ch::CLUSTER_ID,
                    c::CHANGE_CHANNEL,
                    ch::encode_change_channel(&m),
                    c::CHANGE_CHANNEL_RESPONSE,
                )
                .await,
            )
            .unwrap();
            (r.status, r.data)
        }
    };
    assert_eq!(
        change("PBS").await,
        (ch::StatusEnum::Success, Some("data response".to_string()))
    );
    assert_eq!(current().await, (9, 1));
    assert_eq!(
        change("KCTS").await,
        (ch::StatusEnum::MultipleMatches, None)
    );
    assert_eq!(
        change("no such channel").await,
        (ch::StatusEnum::NoMatches, None)
    );
    let st = |command, fields| status(&node, EP, ch::CLUSTER_ID, command, fields);
    assert_eq!(
        st(
            c::CHANGE_CHANNEL_BY_NUMBER,
            ch::encode_change_channel_by_number(9, 2)
        )
        .await,
        ImStatus::Success
    );
    assert_eq!(current().await, (9, 2));
    assert_eq!(
        st(
            c::CHANGE_CHANNEL_BY_NUMBER,
            ch::encode_change_channel_by_number(99, 0)
        )
        .await,
        FAILURE
    );
    assert_eq!(
        st(c::SKIP_CHANNEL, ch::encode_skip_channel(1)).await,
        ImStatus::Success
    );
    assert_eq!(current().await, (9, 3));
    // (3 + 1) % 4 = 0: past the end of the list, back to ABC.
    assert_eq!(
        st(c::SKIP_CHANNEL, ch::encode_skip_channel(1)).await,
        ImStatus::Success
    );
    assert_eq!(current().await, (6, 0));
    // (0 - 1) % 4 = -1, and the delegate adds 4: back past the start.
    assert_eq!(
        st(c::SKIP_CHANNEL, ch::encode_skip_channel(-1)).await,
        ImStatus::Success
    );
    assert_eq!(current().await, (9, 3));
    assert_eq!(
        st(c::SKIP_CHANNEL, ch::encode_skip_channel(1)).await,
        ImStatus::Success
    );
    assert_eq!(current().await, (6, 0));
}

/// GetProgramGuide: tv-app's four programs (ChannelManager), all in the window
/// 0-3600 and filtered by a narrower start or end time and by a channel list;
/// the response carries an empty paging struct and nested ChannelInfoStruct
/// per program. chip's server does not check the EPG
/// feature, which tv-app's FeatureMap (3) lacks. chip numbers the request's
/// RecordingFlag, ExternalIDList and Data 4, 5 and 6 where the specification
/// (and matter.js) has 5, 6 and 7 (controller-clusters.matter, both refs), so
/// a RecordingFlag at the specification's tag 5 reaches chip as ExternalIDList,
/// fails to decode ("Schema mismatch" in tv-app's log), and is refused with
/// InvalidCommand. The requests expected to succeed therefore leave out
/// RecordingFlag and ExternalIDList, and PageToken too: chip's codegen makes
/// it non-nullable, so chip refuses a null one.
#[tokio::test]
async fn channel_program_guide_filters_and_chips_field_numbering() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    use ch::command_id as c;
    let guide = |fields| {
        let node = &node;
        async move {
            match invoke(node, EP, ch::CLUSTER_ID, c::GET_PROGRAM_GUIDE, fields).await {
                InvokeResult::Data { path, fields }
                    if path.command == c::PROGRAM_GUIDE_RESPONSE =>
                {
                    Ok(ch::ProgramGuideResponse::decode(&payload_tlv(&fields)).unwrap())
                }
                InvokeResult::Status(s) => Err(s),
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    let all = guide(ch::encode_get_program_guide(
        0, 3600, None, None, None, None, None,
    ))
    .await
    .unwrap();
    let ids: Vec<&str> = all
        .program_list
        .iter()
        .map(|p| p.identifier.as_str())
        .collect();
    assert_eq!(
        ids,
        ["progid-abc1", "progid-pbs1", "progid-abc2", "progid-abc3"]
    );
    assert_eq!(
        (all.paging.previous_token, all.paging.next_token),
        (None, None)
    );
    let first = &all.program_list[0];
    assert_eq!(
        (
            first.channel.major_number,
            first.start_time,
            first.end_time,
            first.title.as_str()
        ),
        (6, 0, 1800, "ABC Title1")
    );
    let pbs = ch::ChannelInfoStruct {
        major_number: 9,
        minor_number: 1,
        name: None,
        call_sign: None,
        affiliate_call_sign: None,
        identifier: None,
        r#type: None,
    };
    let one = guide(ch::encode_get_program_guide(
        0,
        3600,
        Some(vec![pbs]),
        None,
        None,
        None,
        None,
    ))
    .await
    .unwrap();
    let ids: Vec<&str> = one
        .program_list
        .iter()
        .map(|p| p.identifier.as_str())
        .collect();
    assert_eq!(ids, ["progid-pbs1"]);
    // The delegate keeps a program only if it starts at or after StartTime
    // and ends at or before EndTime: abc1 and pbs1 run 0-1800, abc2
    // 1800-3600, abc3 0-3600.
    let window = |start, end| {
        let guide = &guide;
        async move {
            let r = guide(ch::encode_get_program_guide(
                start, end, None, None, None, None, None,
            ))
            .await
            .unwrap();
            r.program_list
                .into_iter()
                .map(|p| p.identifier)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(window(1800, 3600).await, ["progid-abc2"]);
    assert_eq!(window(0, 1800).await, ["progid-abc1", "progid-pbs1"]);
    let flag = Some(Nullable::Value(ch::RecordingFlagBitmap::SCHEDULED));
    assert_eq!(
        guide(ch::encode_get_program_guide(
            0, 3600, None, None, flag, None, None
        ))
        .await
        .map(|_| ()),
        Err(INVALID_COMMAND)
    );
}

/// ApplicationLauncher (AP; FeatureMap 1) on endpoint 1: the CatalogList of
/// ApplicationLauncherManager (123, 456) and a null CurrentApp. With the AP
/// feature, chip's server hands LaunchApp, HideApp and StopApp to the Content
/// App they name: tv-app maps catalog 123's "exampleid" to its Vendor1 app
/// (AppTv.cpp `ConvertToPlatformCatalogVendorApp`), whose
/// ApplicationLauncherManager answers each with Success and the bytes
/// "data". LaunchApp makes Vendor1 the current app, HideApp clears that and
/// StopApp sets its Status to Stopped (ContentAppPlatform.cpp). chip's server
/// refuses a request without an Application with InvalidCommand, whatever
/// the endpoint's features (application-launcher-server.cpp, both refs).
#[tokio::test]
async fn application_launcher_launches_hides_and_stops() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    use al::{attribute_id as a, command_id as c};
    let attrs = sweep(&node, EP, al::CLUSTER_ID, |id, t| match id {
        a::CATALOG_LIST => ok(al::decode_catalog_list(t)),
        a::CURRENT_APP => ok(al::decode_current_app(t)),
        other => newer_than_codegen("ApplicationLauncher", other),
    })
    .await;
    assert_exact_attribute_ids("ApplicationLauncher", &attrs, &[0, 1]);
    assert_eq!(
        al::decode_catalog_list(attribute_tlv(&attrs, a::CATALOG_LIST)).unwrap(),
        [123, 456]
    );
    assert_eq!(
        al::decode_current_app(attribute_tlv(&attrs, a::CURRENT_APP)).unwrap(),
        Nullable::Null
    );
    let app = || {
        Some(al::ApplicationStruct {
            catalog_vendor_id: 123,
            application_id: "exampleid".to_string(),
        })
    };
    let launcher = |command, fields| {
        let node = &node;
        async move {
            match invoke(node, EP, al::CLUSTER_ID, command, fields).await {
                InvokeResult::Data { path, fields } if path.command == c::LAUNCHER_RESPONSE => {
                    let r = al::LauncherResponse::decode(&payload_tlv(&fields)).unwrap();
                    Ok((r.status, r.data))
                }
                InvokeResult::Status(s) => Err(s),
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    let done = Ok((al::StatusEnum::Success, Some(b"data".to_vec())));
    assert_eq!(
        launcher(c::LAUNCH_APP, al::encode_launch_app(app(), None)).await,
        done
    );
    assert_eq!(
        launcher(c::HIDE_APP, al::encode_hide_app(app())).await,
        done
    );
    assert_eq!(
        launcher(c::STOP_APP, al::encode_stop_app(app())).await,
        done
    );
    assert_eq!(
        launcher(c::LAUNCH_APP, al::encode_launch_app(None, None)).await,
        Err(INVALID_COMMAND)
    );
}

/// ContentControl (provisional; BU, OCR, SCR: FeatureMap 0x1C) on endpoint 1.
/// tv-app.matter declares attributes 0 to 7 at both refs (none of the block
/// lists). The six `ram` ones read back their storage defaults (Enabled
/// false, an empty OnDemandRatingThreshold, ScreenDailyTime 0).
/// OnDemandRatings and ScheduledContentRatings are `callback` attributes, but
/// chip's ContentControl server registers no attribute-access handler (both
/// refs), so a read falls through to the Content App platform's
/// external-attribute callback, which answers FAILURE for endpoint 1
/// (ContentAppPlatform.cpp; seen in tv-app's log at both refs). The
/// wildcard read leaves the two out, and the delegate's rating getters are
/// never called. The commands sent, and their answers:
/// - SetScreenDailyTime, BlockUnratedContent, UnblockUnratedContent,
///   SetOnDemandRatingThreshold, AddBonusTime, Enable and Disable: Success.
///   chip's server passes each to the delegate, whose handler is empty, and
///   answers Success itself. Enable and Disable are timed at master only
///   (`timed command` in master's controller-clusters.matter), so there the
///   controller retries them as timed invokes.
/// - ResetPIN: Failure. Its handler never calls its responder, so chip's
///   server answers Failure instead of a ResetPinResponse (timed at master
///   too).
/// - AddBlockChannels: UnsupportedCommand. tv-app does not handle it, and
///   v1.4.2.0's codegen has no such command.
///
/// UpdatePIN and SetScheduledContentRatingThreshold are not sent (chip's
/// server would answer both with Success, like the first group).
#[tokio::test]
async fn content_control_stub_answers_its_commands() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    use cc::{attribute_id as a, command_id as c};
    let attrs = sweep(&node, EP, cc::CLUSTER_ID, |id, t| match id {
        a::ENABLED => ok(cc::decode_enabled(t)),
        a::ON_DEMAND_RATINGS => ok(cc::decode_on_demand_ratings(t)),
        a::ON_DEMAND_RATING_THRESHOLD => ok(cc::decode_on_demand_rating_threshold(t)),
        a::SCHEDULED_CONTENT_RATINGS => ok(cc::decode_scheduled_content_ratings(t)),
        a::SCHEDULED_CONTENT_RATING_THRESHOLD => {
            ok(cc::decode_scheduled_content_rating_threshold(t))
        }
        a::SCREEN_DAILY_TIME => ok(cc::decode_screen_daily_time(t)),
        a::REMAINING_SCREEN_TIME => ok(cc::decode_remaining_screen_time(t)),
        a::BLOCK_UNRATED => ok(cc::decode_block_unrated(t)),
        other => newer_than_codegen("ContentControl", other),
    })
    .await;
    assert_exact_attribute_ids("ContentControl", &attrs, &[0, 2, 4, 5, 6, 7]);
    let tlv = |id| attribute_tlv(&attrs, id);
    assert!(!cc::decode_enabled(tlv(a::ENABLED)).unwrap());
    assert_eq!(
        cc::decode_on_demand_rating_threshold(tlv(a::ON_DEMAND_RATING_THRESHOLD)).unwrap(),
        ""
    );
    assert_eq!(
        cc::decode_screen_daily_time(tlv(a::SCREEN_DAILY_TIME)).unwrap(),
        0
    );
    let st = |command, fields| status(&node, EP, cc::CLUSTER_ID, command, fields);
    for (command, fields) in [
        (
            c::SET_SCREEN_DAILY_TIME,
            cc::encode_set_screen_daily_time(3600),
        ),
        (c::BLOCK_UNRATED_CONTENT, cc::encode_block_unrated_content()),
        (
            c::UNBLOCK_UNRATED_CONTENT,
            cc::encode_unblock_unrated_content(),
        ),
        (
            c::SET_ON_DEMAND_RATING_THRESHOLD,
            cc::encode_set_on_demand_rating_threshold(&"G".to_string()),
        ),
        (c::ADD_BONUS_TIME, cc::encode_add_bonus_time(None, 60)),
        (c::ENABLE, cc::encode_enable()),
        (c::DISABLE, cc::encode_disable()),
    ] {
        assert_eq!(
            st(command, fields).await,
            ImStatus::Success,
            "command {command:#04x}"
        );
    }
    let channel = cc::BlockChannelStruct {
        block_channel_index: Nullable::Null,
        major_number: 9,
        minor_number: 1,
        identifier: None,
    };
    assert_eq!(st(c::RESET_PIN, cc::encode_reset_pin()).await, FAILURE);
    assert_eq!(
        st(
            c::ADD_BLOCK_CHANNELS,
            cc::encode_add_block_channels(&vec![channel])
        )
        .await,
        UNSUPPORTED_COMMAND
    );
}

/// ApplicationBasic of every Content App endpoint, found by a wildcard read
/// of VendorID: the static endpoint 3 (ZCLCallbacks.cpp gives it the default
/// ApplicationBasicManager) and the four apps tv-app installs at start-up
/// (AppTv.cpp `InstallContentApp`; AllowedVendorList is each app's VendorID
/// and 456). The dynamic endpoints leave Application out of their attribute
/// list (AppTv.cpp `applicationBasicAttrs` declares no Application), so their
/// sweep has seven attributes. Every app's Status is Stopped, the delegate's
/// initial value (the ApplicationLauncher test leaves Vendor1 Stopped too).
#[tokio::test]
async fn content_apps_identify_themselves() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    let mut endpoints: Vec<u16> = node
        .read(&[ReadPath::new(
            None,
            Some(ab::CLUSTER_ID),
            Some(ab::attribute_id::VENDOR_ID),
        )])
        .await
        .unwrap()
        .into_iter()
        .map(|(p, _)| p.endpoint)
        .collect();
    endpoints.sort_unstable();
    assert_eq!(endpoints.len(), 5, "{endpoints:?}");
    let mut apps = Vec::new();
    for ep in endpoints {
        apps.push((ep, application_basic(&node, ep).await));
    }
    let got: Vec<_> = apps.iter().map(|(_, a)| a.clone()).collect();
    let sample = |vendor: &str, id, name: &str, pid, version: &str| {
        (
            vendor.to_string(),
            id,
            name.to_string(),
            pid,
            version.to_string(),
            vec![id, 456],
        )
    };
    let mut want = vec![
        (
            "exampleVendorName1".to_string(),
            1,
            "exampleName1".to_string(),
            1,
            "exampleVersion".to_string(),
            vec![1, 456],
        ),
        sample("Vendor2", 65521, "exampleString", 32769, "Version2"),
        sample("Vendor1", 1, "exampleid", 11, "Version1"),
        sample("Vendor3", 9050, "App3", 22, "Version3"),
        sample("TestSuiteVendor", 1111, "applicationId", 22, "v2"),
    ];
    let mut got_sorted = got.clone();
    got_sorted.sort();
    want.sort();
    assert_eq!(got_sorted, want);
    assert_eq!(apps[0].0, 3, "the static Content App endpoint comes first");
}

/// `(VendorName, VendorID, ApplicationName, ProductID, ApplicationVersion,
/// AllowedVendorList)` of the Content App on `ep`, after a sweep that decodes
/// every attribute and checks the attribute set and the Status (Stopped).
async fn application_basic(node: &Node, ep: u16) -> (String, u16, String, u16, String, Vec<u16>) {
    use ab::attribute_id as a;
    let attrs = sweep(node, ep, ab::CLUSTER_ID, |id, t| match id {
        a::VENDOR_NAME => ok(ab::decode_vendor_name(t)),
        a::VENDOR_ID => ok(ab::decode_vendor_id(t)),
        a::APPLICATION_NAME => ok(ab::decode_application_name(t)),
        a::PRODUCT_ID => ok(ab::decode_product_id(t)),
        a::APPLICATION => ok(ab::decode_application(t)),
        a::STATUS => ok(ab::decode_status(t)),
        a::APPLICATION_VERSION => ok(ab::decode_application_version(t)),
        a::ALLOWED_VENDOR_LIST => ok(ab::decode_allowed_vendor_list(t)),
        other => newer_than_codegen("ApplicationBasic", other),
    })
    .await;
    if ep == 3 {
        assert_exact_attribute_ids("ApplicationBasic", &attrs, &[0, 1, 2, 3, 4, 5, 6, 7]);
        let app = ab::decode_application(attribute_tlv(&attrs, a::APPLICATION)).unwrap();
        assert_eq!(
            (app.catalog_vendor_id, app.application_id.as_str()),
            (123, "applicationId")
        );
    } else {
        assert_exact_attribute_ids("ApplicationBasic", &attrs, &[0, 1, 2, 3, 5, 6, 7]);
    }
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        ab::decode_status(tlv(a::STATUS)).unwrap(),
        ab::ApplicationStatusEnum::Stopped
    );
    (
        ab::decode_vendor_name(tlv(a::VENDOR_NAME)).unwrap(),
        ab::decode_vendor_id(tlv(a::VENDOR_ID)).unwrap(),
        ab::decode_application_name(tlv(a::APPLICATION_NAME)).unwrap(),
        ab::decode_product_id(tlv(a::PRODUCT_ID)).unwrap(),
        ab::decode_application_version(tlv(a::APPLICATION_VERSION)).unwrap(),
        ab::decode_allowed_vendor_list(tlv(a::ALLOWED_VENDOR_LIST)).unwrap(),
    )
}

/// AccountLogin on the "Vendor3" Content App (VendorID 9050, setup PIN
/// "20202021" in AppTv.cpp): GetSetupPIN returns the PIN, which the
/// response's `Debug` output redacts; Login with another PIN is refused with
/// UnsupportedAccess (chip's server, when the delegate returns false) and with
/// the right one succeeds; Logout with a Node succeeds. All three are timed
/// commands (`fabric timed command` in controller-clusters.matter, both refs):
/// the controller retries each as a timed invoke when the device asks.
#[tokio::test]
async fn account_login_issues_and_checks_a_setup_pin() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    use alog::command_id as c;
    let ep = node
        .read(&[ReadPath::new(
            None,
            Some(ab::CLUSTER_ID),
            Some(ab::attribute_id::VENDOR_ID),
        )])
        .await
        .unwrap()
        .into_iter()
        .find(|(_, v)| ab::decode_vendor_id(&payload_tlv(v)).unwrap() == 9050)
        .expect("tv-app installs the Vendor3 Content App (VendorID 9050)")
        .0
        .endpoint;
    let account = "AAAABBBBCCCCDDDD".to_string();
    let pin = alog::GetSetupPinResponse::decode(
        &response(
            &node,
            ep,
            alog::CLUSTER_ID,
            c::GET_SETUP_PIN,
            alog::encode_get_setup_pin(&account),
            c::GET_SETUP_PIN_RESPONSE,
        )
        .await,
    )
    .unwrap();
    // Neither check may print the PIN when it fails: no assert_eq!, and no
    // failure message that holds the PIN or the Debug text.
    #[allow(
        clippy::manual_assert_eq,
        reason = "assert_eq! would print the setup PIN on failure"
    )]
    {
        assert!(pin.setup_pin == "20202021", "Vendor3's setup PIN differs");
    }
    let shown = format!("{pin:?}");
    assert!(
        shown.contains("<redacted>") && !shown.contains(pin.setup_pin.as_str()),
        "GetSetupPinResponse's Debug output does not redact the setup PIN"
    );
    let st = |command, fields| status(&node, ep, alog::CLUSTER_ID, command, fields);
    assert_eq!(
        st(
            c::LOGIN,
            alog::encode_login(&account, &Secret::new("00000000".to_string()), None)
        )
        .await,
        UNSUPPORTED_ACCESS
    );
    assert_eq!(
        st(
            c::LOGIN,
            alog::encode_login(&account, &Secret::new(pin.setup_pin.clone()), None)
        )
        .await,
        ImStatus::Success
    );
    // chip's server answers Success, then fails to log LoggedOut: it builds
    // the event without the FabricIndex a fabric-sensitive event needs, and
    // LogEvent refuses it ("Invalid Fabric Index", account-login-server.cpp,
    // both refs). So the event never reaches the log; decode smoke covers it.
    assert_eq!(
        st(c::LOGOUT, alog::encode_logout(Some(0x0102_0304_0506_0708))).await,
        ImStatus::Success
    );
}
