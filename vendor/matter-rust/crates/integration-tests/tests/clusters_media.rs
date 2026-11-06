// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

//! The media clusters on a live tv-app, its video-player endpoint 1:
//! WakeOnLan, LowPower, KeypadInput, MediaInput, AudioOutput, TargetNavigator,
//! MediaPlayback and ContentLauncher.
//! tv-app serves all of them on endpoint 1 at master 5cd2917a and v1.4.2.0
//! (`examples/tv-app/tv-common/tv-app.matter`, the same cluster set at both
//! refs). It is the one chip example app the harness drives that does:
//! all-clusters also serves WakeOnLan and LowPower (clusters_wake_and_power.rs),
//! and all-clusters-minimal, chef's video-player devices (one at v1.4.2.0, two
//! at master) and the placeholder apps serve most of them, but the harness
//! drives none of those.
//!
//! Every value asserted comes from chip's source at both refs: tv-app's
//! delegates (`examples/tv-app/tv-common/clusters/*/`, identical between the
//! refs except for string-literal spelling, master's `TEMPORARY_RETURN_IGNORED`
//! markers and the ContentControl delegate's UpdatePIN / AddBonusTime
//! signatures) and chip's cluster servers
//! (`src/app/clusters/*-server/`, identical except ApplicationLauncher's,
//! ContentAppObserver's and the ContentControl delegate interface). It ran
//! live on a Linux arm64 Pi against tv-app built at both refs.
//!
//! No pacing between exchanges: chip master's tv-app has only 15 packet
//! buffers (chip commit c37efec9db's `#ifndef CHIP_DEVICE_LAYER_TARGET_DARWIN`
//! leaves it on the default pool; v1.4.2.0 allocates from the heap), and it
//! holds every response until its ACK arrives. The controller sends that ACK
//! as the exchange closes, so at most one response per exchange is ever
//! unACKed, and only for the time the exchange is open.
//!
//! State left changed: the tests restore MediaInput, AudioOutput,
//! ActiveTextTrack and CurrentState, but leave ActiveAudioTrack on
//! `activeAudioTrackId_1`, PlaybackSpeed 1, SampledPosition 1000 and
//! CurrentTarget 2 (tv-app refuses NavigateTarget(0), so CurrentTarget's
//! initial 0 cannot be restored). No test asserts any of the four, so the
//! tests still pass in any order.

use integration_tests::dut::DutConfig;
use integration_tests::sweep::{
    assert_exact_attribute_ids, attribute_tlv, newer_than_codegen, ok, read_cluster_attributes,
};
use matter_clusters::clusters::{
    audio_output as ao, content_launcher as cl, keypad_input as ki, low_power as lp,
    media_input as mi, media_playback as mp, target_navigator as tn, wake_on_lan as wol,
};
use matter_clusters::types::Nullable;
use matter_controller::{CommandPath, ImStatus, InvokeResult, MatterController, Node};

/// tv-app's video-player endpoint (tv-app.matter `endpoint 1`, device type
/// `ma_casting_videoplayer`).
const EP: u16 = 1;

/// IM status codes the servers answer with (Matter Core spec 8.10.1).
const FAILURE: ImStatus = ImStatus::Failure(0x01);
const INVALID_COMMAND: ImStatus = ImStatus::Failure(0x85);
const INVALID_IN_STATE: ImStatus = ImStatus::Failure(0xCB);

/// The controller and node id, or `None` (skip) unless the DUT is tv-app.
async fn connect_tv() -> Option<(MatterController, u64)> {
    let Some(cfg) = DutConfig::from_env() else {
        eprintln!("skipped: no DUT (set MATTER_INTEGRATION_DUT via `just integration-tv`)");
        return None;
    };
    if !cfg.is_app("tv") {
        eprintln!("skipped: media tests need the tv-app DUT (`just integration-tv`)");
        return None;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    Some((controller, node_id))
}

/// Every attribute of `cluster` on [`EP`], decoded by `decode`.
async fn sweep(
    node: &Node,
    cluster: u32,
    decode: impl Fn(u32, &[u8]) -> Result<(), String>,
) -> Vec<(u32, Vec<u8>)> {
    integration_tests::sweep::decode_every_attribute(node, EP, cluster, decode).await
}

/// One attribute of `cluster` on [`EP`], read fresh (a cluster read).
async fn read_attr(node: &Node, cluster: u32, attribute: u32) -> Vec<u8> {
    let attrs = read_cluster_attributes(node, EP, cluster).await.unwrap();
    attribute_tlv(&attrs, attribute).to_vec()
}

/// Invoke a command on [`EP`].
async fn invoke(node: &Node, cluster: u32, command: u32, fields: Vec<u8>) -> InvokeResult {
    let path = CommandPath {
        endpoint: EP,
        cluster,
        command,
    };
    node.invoke_tlv(path, fields).await.unwrap()
}

/// Invoke a command answered by a bare status.
async fn status(node: &Node, cluster: u32, command: u32, fields: Vec<u8>) -> ImStatus {
    match invoke(node, cluster, command, fields).await {
        InvokeResult::Status(s) => s,
        other => panic!("{cluster:#06x}/{command:#04x}: expected a status, got {other:?}"),
    }
}

/// Invoke a command answered by response command `response`; its payload.
async fn response(
    node: &Node,
    cluster: u32,
    command: u32,
    fields: Vec<u8>,
    response: u32,
) -> Vec<u8> {
    match invoke(node, cluster, command, fields).await {
        InvokeResult::Data { path, fields } if path.command == response => {
            integration_tests::events::payload_tlv(&fields)
        }
        other => panic!(
            "{cluster:#06x}/{command:#04x}: expected response {response:#04x}, got {other:?}"
        ),
    }
}

/// WakeOnLan serves only MACAddress (tv-app.matter: `ram attribute
/// MACAddress`); LinkLocalAddress is not served. chip's `WakeOnLanAttrAccess`
/// answers each read of MACAddress from tv-app's
/// `WakeOnLanManager::HandleGetMacAddress`: the host's primary MAC as 12
/// upper-case hex digits, or `"000000000000"` when there is none (both refs). LowPower's Sleep
/// succeeds (LowPowerManager::HandleSleep returns true).
#[tokio::test]
async fn wake_on_lan_mac_and_low_power_sleep() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    let attrs = sweep(&node, wol::CLUSTER_ID, |id, t| match id {
        wol::attribute_id::MAC_ADDRESS => ok(wol::decode_mac_address(t)),
        wol::attribute_id::LINK_LOCAL_ADDRESS => ok(wol::decode_link_local_address(t)),
        other => newer_than_codegen("WakeOnLan", other),
    })
    .await;
    assert_exact_attribute_ids("WakeOnLan", &attrs, &[0]);
    let mac = wol::decode_mac_address(attribute_tlv(&attrs, 0)).unwrap();
    assert_eq!(mac.len(), 12, "{mac}");
    assert!(
        mac.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()),
        "{mac}"
    );
    let attrs = sweep(&node, lp::CLUSTER_ID, |id, _| {
        newer_than_codegen("LowPower", id)
    })
    .await;
    assert_exact_attribute_ids("LowPower", &attrs, &[]);
    assert_eq!(
        status(
            &node,
            lp::CLUSTER_ID,
            lp::command_id::SLEEP,
            lp::encode_sleep()
        )
        .await,
        ImStatus::Success
    );
}

/// KeypadInput (no attributes; FeatureMap 7, KeypadInputManager::GetFeatureMap):
/// Select is a supported key; a key code outside CecKeyCodeEnum answers
/// UnsupportedKey (the delegate's `default` arm).
#[tokio::test]
async fn keypad_input_sends_a_key_and_refuses_an_unknown_one() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    let attrs = sweep(&node, ki::CLUSTER_ID, |id, _| {
        newer_than_codegen("KeypadInput", id)
    })
    .await;
    assert_exact_attribute_ids("KeypadInput", &attrs, &[]);
    let send = |key| {
        let node = &node;
        async move {
            ki::SendKeyResponse::decode(
                &response(
                    node,
                    ki::CLUSTER_ID,
                    ki::command_id::SEND_KEY,
                    ki::encode_send_key(key),
                    ki::command_id::SEND_KEY_RESPONSE,
                )
                .await,
            )
            .unwrap()
            .status
        }
    };
    assert_eq!(
        send(ki::CecKeyCodeEnum::Select).await,
        ki::StatusEnum::Success
    );
    assert_eq!(
        send(ki::CecKeyCodeEnum::Unknown(0xFE)).await,
        ki::StatusEnum::UnsupportedKey
    );
}

/// MediaInput (NU) and AudioOutput (NU): the delegates' lists (two and three
/// HDMI entries from index 1); selecting another entry and renaming it change
/// the attributes, an index outside the list is refused with Failure (the
/// delegate returns false), and both are put back.
#[tokio::test]
async fn media_input_and_audio_output_select_and_rename() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    use mi::attribute_id as ma;
    let attrs = sweep(&node, mi::CLUSTER_ID, |id, t| match id {
        ma::INPUT_LIST => ok(mi::decode_input_list(t)),
        ma::CURRENT_INPUT => ok(mi::decode_current_input(t)),
        other => newer_than_codegen("MediaInput", other),
    })
    .await;
    assert_exact_attribute_ids("MediaInput", &attrs, &[0, 1]);
    let inputs = mi::decode_input_list(attribute_tlv(&attrs, ma::INPUT_LIST)).unwrap();
    let got: Vec<(u8, mi::InputTypeEnum, &str, &str)> = inputs
        .iter()
        .map(|i| {
            (
                i.index,
                i.input_type,
                i.name.as_str(),
                i.description.as_str(),
            )
        })
        .collect();
    let hdmi = "High-Definition Multimedia Interface";
    assert_eq!(
        got,
        [
            (1, mi::InputTypeEnum::Hdmi, "HDMI", hdmi),
            (2, mi::InputTypeEnum::Hdmi, "HDMI", hdmi)
        ]
    );
    assert_eq!(
        mi::decode_current_input(attribute_tlv(&attrs, ma::CURRENT_INPUT)).unwrap(),
        1
    );
    let cmd = |c: u32, f: Vec<u8>| status(&node, mi::CLUSTER_ID, c, f);
    assert_eq!(
        cmd(mi::command_id::SELECT_INPUT, mi::encode_select_input(2)).await,
        ImStatus::Success
    );
    assert_eq!(
        mi::decode_current_input(&read_attr(&node, mi::CLUSTER_ID, ma::CURRENT_INPUT).await)
            .unwrap(),
        2
    );
    assert_eq!(
        cmd(mi::command_id::SELECT_INPUT, mi::encode_select_input(9)).await,
        FAILURE
    );
    let renamed = "Console".to_string();
    assert_eq!(
        cmd(
            mi::command_id::RENAME_INPUT,
            mi::encode_rename_input(2, &renamed)
        )
        .await,
        ImStatus::Success
    );
    let inputs =
        mi::decode_input_list(&read_attr(&node, mi::CLUSTER_ID, ma::INPUT_LIST).await).unwrap();
    assert_eq!(inputs[1].name, renamed);
    assert_eq!(
        cmd(
            mi::command_id::SHOW_INPUT_STATUS,
            mi::encode_show_input_status()
        )
        .await,
        ImStatus::Success
    );
    assert_eq!(
        cmd(
            mi::command_id::HIDE_INPUT_STATUS,
            mi::encode_hide_input_status()
        )
        .await,
        ImStatus::Success
    );
    assert_eq!(
        cmd(
            mi::command_id::RENAME_INPUT,
            mi::encode_rename_input(2, &"HDMI".to_string())
        )
        .await,
        ImStatus::Success
    );
    assert_eq!(
        cmd(mi::command_id::SELECT_INPUT, mi::encode_select_input(1)).await,
        ImStatus::Success
    );
    audio_output_selects_and_renames(&node).await;
}

async fn audio_output_selects_and_renames(node: &Node) {
    use ao::attribute_id as oa;
    let attrs = sweep(node, ao::CLUSTER_ID, |id, t| match id {
        oa::OUTPUT_LIST => ok(ao::decode_output_list(t)),
        oa::CURRENT_OUTPUT => ok(ao::decode_current_output(t)),
        other => newer_than_codegen("AudioOutput", other),
    })
    .await;
    assert_exact_attribute_ids("AudioOutput", &attrs, &[0, 1]);
    let outputs = ao::decode_output_list(attribute_tlv(&attrs, oa::OUTPUT_LIST)).unwrap();
    let got: Vec<(u8, ao::OutputTypeEnum, &str)> = outputs
        .iter()
        .map(|o| (o.index, o.output_type, o.name.as_str()))
        .collect();
    let hdmi = ao::OutputTypeEnum::Hdmi;
    assert_eq!(
        got,
        [(1, hdmi, "HDMI"), (2, hdmi, "HDMI"), (3, hdmi, "HDMI")]
    );
    assert_eq!(
        ao::decode_current_output(attribute_tlv(&attrs, oa::CURRENT_OUTPUT)).unwrap(),
        1
    );
    let cmd = |c: u32, f: Vec<u8>| status(node, ao::CLUSTER_ID, c, f);
    assert_eq!(
        cmd(ao::command_id::SELECT_OUTPUT, ao::encode_select_output(3)).await,
        ImStatus::Success
    );
    assert_eq!(
        ao::decode_current_output(&read_attr(node, ao::CLUSTER_ID, oa::CURRENT_OUTPUT).await)
            .unwrap(),
        3
    );
    assert_eq!(
        cmd(ao::command_id::SELECT_OUTPUT, ao::encode_select_output(9)).await,
        FAILURE
    );
    let renamed = "Soundbar".to_string();
    assert_eq!(
        cmd(
            ao::command_id::RENAME_OUTPUT,
            ao::encode_rename_output(3, &renamed)
        )
        .await,
        ImStatus::Success
    );
    let outputs =
        ao::decode_output_list(&read_attr(node, ao::CLUSTER_ID, oa::OUTPUT_LIST).await).unwrap();
    assert_eq!(outputs[2].name, renamed);
    assert_eq!(
        cmd(
            ao::command_id::RENAME_OUTPUT,
            ao::encode_rename_output(3, &"HDMI".to_string())
        )
        .await,
        ImStatus::Success
    );
    assert_eq!(
        cmd(ao::command_id::SELECT_OUTPUT, ao::encode_select_output(1)).await,
        ImStatus::Success
    );
}

/// TargetNavigator: the delegate's two targets (ids from 1, both named
/// "exampleName"; 0 is "no current target"); NavigateTarget(2) succeeds with
/// the delegate's "data response" and moves CurrentTarget; target 0 and an id
/// past the list answer TargetNotFound with "error".
#[tokio::test]
async fn target_navigator_navigates_and_refuses_unknown_targets() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    use tn::attribute_id as a;
    let attrs = sweep(&node, tn::CLUSTER_ID, |id, t| match id {
        a::TARGET_LIST => ok(tn::decode_target_list(t)),
        a::CURRENT_TARGET => ok(tn::decode_current_target(t)),
        other => newer_than_codegen("TargetNavigator", other),
    })
    .await;
    assert_exact_attribute_ids("TargetNavigator", &attrs, &[0, 1]);
    let targets = tn::decode_target_list(attribute_tlv(&attrs, a::TARGET_LIST)).unwrap();
    let got: Vec<(u8, &str)> = targets
        .iter()
        .map(|t| (t.identifier, t.name.as_str()))
        .collect();
    assert_eq!(got, [(1, "exampleName"), (2, "exampleName")]);
    let navigate = |target| {
        let node = &node;
        async move {
            tn::NavigateTargetResponse::decode(
                &response(
                    node,
                    tn::CLUSTER_ID,
                    tn::command_id::NAVIGATE_TARGET,
                    tn::encode_navigate_target(target, None),
                    tn::command_id::NAVIGATE_TARGET_RESPONSE,
                )
                .await,
            )
            .unwrap()
        }
    };
    let ok_response = navigate(2).await;
    assert_eq!(
        (ok_response.status, ok_response.data.as_deref()),
        (tn::StatusEnum::Success, Some("data response"))
    );
    assert_eq!(
        tn::decode_current_target(&read_attr(&node, tn::CLUSTER_ID, a::CURRENT_TARGET).await)
            .unwrap(),
        2
    );
    for target in [0, 3] {
        let refused = navigate(target).await;
        assert_eq!(
            (refused.status, refused.data.as_deref()),
            (tn::StatusEnum::TargetNotFound, Some("error")),
            "target {target}"
        );
    }
}

/// MediaPlayback (AS, VS; FeatureMap 3): every attribute decodes, including
/// ActiveTextTrack, which tv-app sends as a TrackStruct with an empty id and a
/// null TrackAttributes until a text track is activated (the class-N
/// relaxation). chip numbers TrackAttributesStruct's DisplayName at tag 1,
/// where the specification has Characteristics (controller-clusters.matter,
/// both refs), so tv-app's display names arrive at a tag our decoder skips:
/// both decode as `None`.
#[tokio::test]
async fn media_playback_decodes_tv_apps_tracks() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    use mp::attribute_id as a;
    let attrs = sweep(&node, mp::CLUSTER_ID, |id, t| match id {
        a::CURRENT_STATE => ok(mp::decode_current_state(t)),
        a::START_TIME => ok(mp::decode_start_time(t)),
        a::DURATION => ok(mp::decode_duration(t)),
        a::SAMPLED_POSITION => ok(mp::decode_sampled_position(t)),
        a::PLAYBACK_SPEED => ok(mp::decode_playback_speed(t)),
        a::SEEK_RANGE_END => ok(mp::decode_seek_range_end(t)),
        a::SEEK_RANGE_START => ok(mp::decode_seek_range_start(t)),
        a::ACTIVE_AUDIO_TRACK => ok(mp::decode_active_audio_track(t)),
        a::AVAILABLE_AUDIO_TRACKS => ok(mp::decode_available_audio_tracks(t)),
        a::ACTIVE_TEXT_TRACK => ok(mp::decode_active_text_track(t)),
        a::AVAILABLE_TEXT_TRACKS => ok(mp::decode_available_text_tracks(t)),
        other => newer_than_codegen("MediaPlayback", other),
    })
    .await;
    assert_exact_attribute_ids("MediaPlayback", &attrs, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    let tlv = |id| attribute_tlv(&attrs, id);
    assert_eq!(
        mp::decode_duration(tlv(a::DURATION)).unwrap(),
        Nullable::Value(80_000)
    );
    assert_eq!(
        mp::decode_seek_range_end(tlv(a::SEEK_RANGE_END)).unwrap(),
        Nullable::Value(80_000)
    );
    assert_eq!(
        mp::decode_seek_range_start(tlv(a::SEEK_RANGE_START)).unwrap(),
        Nullable::Value(0)
    );
    let Nullable::Value(text) = mp::decode_active_text_track(tlv(a::ACTIVE_TEXT_TRACK)).unwrap()
    else {
        panic!("ActiveTextTrack is null");
    };
    assert_eq!(
        (text.id.as_str(), text.track_attributes),
        ("", Nullable::Null)
    );
    let Nullable::Value(audio) =
        mp::decode_available_audio_tracks(tlv(a::AVAILABLE_AUDIO_TRACKS)).unwrap()
    else {
        panic!("AvailableAudioTracks is null");
    };
    let got: Vec<(&str, Option<&str>, bool, bool)> = audio
        .iter()
        .map(|t| match &t.track_attributes {
            Nullable::Value(attrs) => (
                t.id.as_str(),
                Some(attrs.language_code.as_str()),
                attrs.characteristics.is_none(),
                attrs.display_name.is_none(),
            ),
            Nullable::Null => (t.id.as_str(), None, true, true),
        })
        .collect();
    assert_eq!(
        got,
        [
            ("activeAudioTrackId_0", Some("languageCode1"), true, true),
            ("activeAudioTrackId_1", Some("languageCode2"), true, true)
        ]
    );
}

/// MediaPlayback commands: Play and Pause change CurrentState; Seek inside
/// the seek range succeeds and past it answers SeekOutOfRange (chip's server
/// passes Seek straight to the delegate; tv-app's
/// `MediaPlaybackManager::HandleSeek` answers SeekOutOfRange when
/// `positionMilliseconds > mDuration`, the same `mDuration` that
/// `HandleGetSeekRangeEnd` reports, both refs); activating a text track fills
/// ActiveTextTrack and an unknown track answers InvalidInState (chip's server,
/// when the delegate returns false); an audio track with AudioOutputIndex 0
/// succeeds, while a null AudioOutputIndex (nullable in the specification,
/// not in chip's codegen) is refused with InvalidCommand.
#[tokio::test]
async fn media_playback_plays_seeks_and_switches_tracks() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    use mp::{attribute_id as a, command_id as c};
    let playback = |command, fields| {
        let node = &node;
        async move {
            mp::PlaybackResponse::decode(
                &response(node, mp::CLUSTER_ID, command, fields, c::PLAYBACK_RESPONSE).await,
            )
            .unwrap()
        }
    };
    let state = || async {
        mp::decode_current_state(&read_attr(&node, mp::CLUSTER_ID, a::CURRENT_STATE).await).unwrap()
    };
    let played = playback(c::PLAY, mp::encode_play()).await;
    assert_eq!(
        (played.status, played.data.as_deref()),
        (mp::StatusEnum::Success, Some("data response"))
    );
    assert_eq!(state().await, mp::PlaybackStateEnum::Playing);
    assert_eq!(
        playback(c::PAUSE, mp::encode_pause()).await.status,
        mp::StatusEnum::Success
    );
    assert_eq!(state().await, mp::PlaybackStateEnum::Paused);
    assert_eq!(
        playback(c::SEEK, mp::encode_seek(1_000)).await.status,
        mp::StatusEnum::Success
    );
    assert_eq!(
        playback(c::SEEK, mp::encode_seek(80_001)).await.status,
        mp::StatusEnum::SeekOutOfRange
    );
    let track = "activeTextTrackId_1".to_string();
    let st = |command, fields| status(&node, mp::CLUSTER_ID, command, fields);
    assert_eq!(
        st(
            c::ACTIVATE_TEXT_TRACK,
            mp::encode_activate_text_track(&track)
        )
        .await,
        ImStatus::Success
    );
    let Nullable::Value(text) =
        mp::decode_active_text_track(&read_attr(&node, mp::CLUSTER_ID, a::ACTIVE_TEXT_TRACK).await)
            .unwrap()
    else {
        panic!("ActiveTextTrack is null");
    };
    assert_eq!(text.id, track);
    assert!(
        matches!(&text.track_attributes, Nullable::Value(t) if t.language_code == "languageCode2")
    );
    assert_eq!(
        st(
            c::ACTIVATE_TEXT_TRACK,
            mp::encode_activate_text_track(&"nope".to_string())
        )
        .await,
        INVALID_IN_STATE
    );
    assert_eq!(
        st(c::DEACTIVATE_TEXT_TRACK, mp::encode_deactivate_text_track()).await,
        ImStatus::Success
    );
    let audio = "activeAudioTrackId_1".to_string();
    assert_eq!(
        st(
            c::ACTIVATE_AUDIO_TRACK,
            mp::encode_activate_audio_track(&audio, Some(Nullable::Value(0)))
        )
        .await,
        ImStatus::Success
    );
    assert_eq!(
        st(
            c::ACTIVATE_AUDIO_TRACK,
            mp::encode_activate_audio_track(&audio, Some(Nullable::Null))
        )
        .await,
        INVALID_COMMAND
    );
    assert_eq!(
        playback(c::PLAY, mp::encode_play()).await.status,
        mp::StatusEnum::Success
    );
}

/// A LaunchContent search: an actor with two external ids, and a genre.
fn search() -> cl::ContentSearchStruct {
    cl::ContentSearchStruct {
        parameter_list: vec![
            cl::ParameterStruct {
                r#type: cl::ParameterEnum::Actor,
                value: "Gaby sHoffman".to_string(),
                external_id_list: Some(vec![
                    cl::AdditionalInfoStruct {
                        name: "imdb".to_string(),
                        value: "nm0001".to_string(),
                    },
                    cl::AdditionalInfoStruct {
                        name: "tmdb".to_string(),
                        value: "42".to_string(),
                    },
                ]),
            },
            cl::ParameterStruct {
                r#type: cl::ParameterEnum::Genre,
                value: "Horror".to_string(),
                external_id_list: None,
            },
        ],
    }
}

/// A track preference with every field present and non-null.
fn track(language: &str) -> cl::TrackPreferenceStruct {
    cl::TrackPreferenceStruct {
        language_code: language.to_string(),
        characteristics: Some(Nullable::Value(vec![cl::CharacteristicEnum::MainProgram])),
        audio_output_index: Some(Nullable::Value(0)),
    }
}

/// ContentLauncher (CS, UP; FeatureMap 3): AcceptHeader is the two "example"
/// entries and SupportedStreamingProtocols the 0 of ContentLauncherManager's
/// default constructor (ContentLauncherManager.h, both refs).
/// LaunchContent with the nested search and with every PlaybackPreferences
/// field non-null succeeds ("exampleData"); a null TextTrack (nullable in the
/// specification, not in chip's codegen) is refused with InvalidCommand.
/// LaunchURL with BrandingInformation (StyleInformationStruct, DimensionStruct
/// doubles) succeeds; tv-app's two test URLs answer UrlNotAvailable and
/// AuthFailed.
#[tokio::test]
async fn content_launcher_launches_content_and_urls() {
    let Some((controller, node_id)) = connect_tv().await else {
        return;
    };
    let node = controller.node(node_id);
    use cl::{attribute_id as a, command_id as c};
    let attrs = sweep(&node, cl::CLUSTER_ID, |id, t| match id {
        a::ACCEPT_HEADER => ok(cl::decode_accept_header(t)),
        a::SUPPORTED_STREAMING_PROTOCOLS => ok(cl::decode_supported_streaming_protocols(t)),
        other => newer_than_codegen("ContentLauncher", other),
    })
    .await;
    assert_exact_attribute_ids("ContentLauncher", &attrs, &[0, 1]);
    assert_eq!(
        cl::decode_accept_header(attribute_tlv(&attrs, a::ACCEPT_HEADER)).unwrap(),
        ["example", "example"]
    );
    assert_eq!(
        cl::decode_supported_streaming_protocols(attribute_tlv(
            &attrs,
            a::SUPPORTED_STREAMING_PROTOCOLS
        ))
        .unwrap()
        .bits(),
        0
    );
    let launch = |command, fields| {
        let node = &node;
        async move {
            match invoke(node, cl::CLUSTER_ID, command, fields).await {
                InvokeResult::Data { path, fields } if path.command == c::LAUNCHER_RESPONSE => {
                    let r = cl::LauncherResponse::decode(&integration_tests::events::payload_tlv(
                        &fields,
                    ))
                    .unwrap();
                    Ok((r.status, r.data))
                }
                InvokeResult::Status(s) => Err(s),
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    let launched = Ok((cl::StatusEnum::Success, Some("exampleData".to_string())));
    assert_eq!(
        launch(
            c::LAUNCH_CONTENT,
            cl::encode_launch_content(search(), true, None, None, None)
        )
        .await,
        launched
    );
    let prefs = |text_track| cl::PlaybackPreferencesStruct {
        playback_position: Some(Nullable::Value(0)),
        text_track: Some(text_track),
        audio_tracks: Some(Nullable::Value(vec![track("fr")])),
    };
    let full = cl::encode_launch_content(
        search(),
        true,
        Some("d".to_string()),
        Some(prefs(Nullable::Value(track("en")))),
        Some(false),
    );
    assert_eq!(launch(c::LAUNCH_CONTENT, full).await, launched);
    let null_text =
        cl::encode_launch_content(search(), true, None, Some(prefs(Nullable::Null)), None);
    assert_eq!(
        launch(c::LAUNCH_CONTENT, null_text).await,
        Err(INVALID_COMMAND)
    );
    content_launcher_launches_urls(&node, launch).await;
}

async fn content_launcher_launches_urls<F, Fut>(_node: &Node, launch: F)
where
    F: Fn(u32, Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = Result<(cl::StatusEnum, Option<String>), ImStatus>>,
{
    let size = |width, height, metric| cl::DimensionStruct {
        width,
        height,
        metric,
    };
    let branding = cl::BrandingInformationStruct {
        provider_name: "Example".to_string(),
        background: Some(cl::StyleInformationStruct {
            image_url: Some("https://example.com/bg.png".to_string()),
            color: Some("#000000".to_string()),
            size: Some(size(1920.5, 1080.25, cl::MetricTypeEnum::Pixels)),
        }),
        logo: Some(cl::StyleInformationStruct {
            image_url: None,
            color: None,
            size: Some(size(12.5, 7.25, cl::MetricTypeEnum::Percentage)),
        }),
        progress_bar: None,
        splash: None,
        watermark: None,
    };
    let url = |u: &str, branding| {
        cl::encode_launch_url(
            &u.to_string(),
            Some("Now playing".to_string()),
            branding,
            None,
        )
    };
    let data = Some("exampleData".to_string());
    use cl::command_id::LAUNCH_URL;
    assert_eq!(
        launch(LAUNCH_URL, url("https://example.com/watch", Some(branding))).await,
        Ok((cl::StatusEnum::Success, data.clone()))
    );
    assert_eq!(
        launch(LAUNCH_URL, url("https://badurl", None)).await,
        Ok((cl::StatusEnum::UrlNotAvailable, data.clone()))
    );
    assert_eq!(
        launch(LAUNCH_URL, url("https://csa-iot.org/badauth", None)).await,
        Ok((cl::StatusEnum::AuthFailed, data))
    );
}
