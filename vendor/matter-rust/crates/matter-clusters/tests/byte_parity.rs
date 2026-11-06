//! Byte-parity of generated cluster codecs vs the matter.js 0.16.11 vectors
//! captured in `test-vectors/clusters/` (M7.4a).
//!
//! Writable attributes: decode the vector bytes, re-encode, assert equal —
//! proving our decode accepts matter.js bytes and our encode reproduces them.
//! Read-only attributes: decode succeeds (+ spot checks). Commands: build the
//! typed args and assert `encode_<cmd>` equals the captured payload.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use base64::Engine as _;
use matter_clusters::clusters;
use matter_clusters::types::{Nullable, Secret};
use std::fs;
use std::path::PathBuf;

fn vec_bytes(rel: &str, key: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("test-vectors/clusters")
        .join(rel);
    let v: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    base64::engine::general_purpose::STANDARD
        .decode(v[key].as_str().unwrap())
        .unwrap()
}

fn attr(rel: &str) -> Vec<u8> {
    vec_bytes(rel, "value_tlv_b64")
}
fn cmd(rel: &str) -> Vec<u8> {
    vec_bytes(rel, "payload_tlv_b64")
}

// ---- writable attributes: decode -> re-encode -> equal -------------------

#[test]
fn on_time_roundtrips() {
    let bytes = attr("on_off/attr_on_time.json");
    let v = clusters::on_off::decode_on_time(&bytes).unwrap();
    assert_eq!(clusters::on_off::encode_on_time(v), bytes);
}

#[test]
fn start_up_on_off_present_roundtrips() {
    let bytes = attr("on_off/attr_start_up_on_off_present.json");
    let v = clusters::on_off::decode_start_up_on_off(&bytes).unwrap();
    assert_eq!(clusters::on_off::encode_start_up_on_off(v), bytes);
}

#[test]
fn start_up_on_off_null_roundtrips() {
    let bytes = attr("on_off/attr_start_up_on_off_null.json");
    let v = clusters::on_off::decode_start_up_on_off(&bytes).unwrap();
    assert!(matches!(v, Nullable::Null));
    assert_eq!(clusters::on_off::encode_start_up_on_off(v), bytes);
}

#[test]
fn node_label_roundtrips() {
    let bytes = attr("basic_information/attr_node_label.json");
    let v = clusters::basic_information::decode_node_label(&bytes).unwrap();
    assert_eq!(v, "matter-rust");
    assert_eq!(clusters::basic_information::encode_node_label(&v), bytes);
}

// ---- read-only attributes: decode succeeds (+ spot checks) ---------------

#[test]
fn bool_and_nullable_uint_decode() {
    assert!(clusters::on_off::decode_on_off(&attr("on_off/attr_on_off.json")).unwrap());
    let lvl = clusters::level_control::decode_current_level(&attr(
        "level_control/attr_current_level_present.json",
    ))
    .unwrap();
    assert_eq!(lvl, Nullable::Value(254));
}

#[test]
fn temperature_signed_decode() {
    let t = clusters::temperature_measurement::decode_measured_value(&attr(
        "temperature_measurement/attr_measured_value.json",
    ))
    .unwrap();
    assert_eq!(t, Nullable::Value(-1234));
}

#[test]
fn bitmap_u16_decode() {
    // ColorCapabilities is a map16 bitmap — proves the u16-backing fix.
    let caps = clusters::color_control::decode_color_capabilities(&attr(
        "color_control/attr_color_capabilities.json",
    ))
    .unwrap();
    assert_eq!(caps.bits(), 0b10101);
}

#[test]
fn struct_attribute_decode() {
    let m = clusters::basic_information::decode_capability_minima(&attr(
        "basic_information/attr_capability_minima.json",
    ))
    .unwrap();
    assert_eq!(m.case_sessions_per_fabric, 3);
    assert_eq!(m.subscriptions_per_fabric, 4);
}

#[test]
fn list_of_scalars_and_structs_decode() {
    let server =
        clusters::descriptor::decode_server_list(&attr("descriptor/attr_server_list.json"))
            .unwrap();
    assert_eq!(server, vec![0x06, 0x1d, 0x28]);

    let dts = clusters::descriptor::decode_device_type_list(&attr(
        "descriptor/attr_device_type_list.json",
    ))
    .unwrap();
    assert_eq!(dts.len(), 1);
    assert_eq!(dts[0].device_type, 256);
    assert_eq!(dts[0].revision, 1);
}

#[test]
fn measurement_accuracy_struct_decodes() {
    // M9-A2.2: EEM.Accuracy is a MeasurementAccuracyStruct whose AccuracyRanges
    // is a list-of-struct with optional fields present/absent — the genuinely-new
    // nested wire shape. Decode the matter.js-captured bytes and assert structure.
    let acc = clusters::electrical_energy_measurement::decode_accuracy(&attr(
        "electrical_energy_measurement/attr_accuracy.json",
    ))
    .unwrap();
    assert_eq!(acc.measurement_type.to_raw(), 0); // ActivePower
    assert!(acc.measured);
    assert_eq!(acc.min_measured_value, 1000);
    assert_eq!(acc.max_measured_value, 50000);
    assert_eq!(acc.accuracy_ranges.len(), 2);

    let r0 = &acc.accuracy_ranges[0];
    assert_eq!(r0.range_min, 0);
    assert_eq!(r0.range_max, 10000);
    assert_eq!(r0.percent_max, Some(500));
    assert_eq!(r0.fixed_max, None);

    let r1 = &acc.accuracy_ranges[1];
    assert_eq!(r1.range_min, 10001);
    assert_eq!(r1.fixed_max, Some(100));
    assert_eq!(r1.percent_max, None);
}

// ---- commands: build typed args -> encode -> equal -----------------------

#[test]
fn toggle_command() {
    assert_eq!(
        clusters::on_off::encode_toggle(),
        cmd("on_off/cmd_toggle.json")
    );
}

#[test]
fn on_with_timed_off_command() {
    let got = clusters::on_off::encode_on_with_timed_off(
        clusters::on_off::OnOffControlBitmap::from_bits_truncate(1),
        60,
        0,
    );
    assert_eq!(got, cmd("on_off/cmd_on_with_timed_off.json"));
}

#[test]
fn move_to_level_command() {
    let got = clusters::level_control::encode_move_to_level(
        128,
        Nullable::Value(10),
        clusters::level_control::OptionsBitmap::from_bits_truncate(0),
        clusters::level_control::OptionsBitmap::from_bits_truncate(0),
    );
    assert_eq!(got, cmd("level_control/cmd_move_to_level.json"));
}

#[test]
fn move_to_level_with_on_off_command() {
    // M9-A3 B2: the four *WithOnOff commands inherit their fields from
    // MoveToLevel/Move/Step/Stop. The encoder used to take no arguments and
    // send an empty structure. matter.js rejects that (Level is mandatory);
    // chip decodes the missing fields as zero/null and executes it, so an
    // empty MoveToLevelWithOnOff dims to the minimum level and switches off.
    // The vector is encoded with matter.js's own LevelControl requestSchema.
    let got = clusters::level_control::encode_move_to_level_with_on_off(
        128,
        Nullable::Value(10),
        clusters::level_control::OptionsBitmap::from_bits_truncate(0),
        clusters::level_control::OptionsBitmap::from_bits_truncate(0),
    );
    assert_eq!(got, cmd("level_control/cmd_move_to_level_with_on_off.json"));
}

#[test]
fn set_temperature_command() {
    // M9-A3 B3: the first byte-parity vector for a signed request field
    // (TargetTemperature, int16 0.01 °C, put_int) next to an omitted optional
    // one (TargetTemperatureLevel). Encoded with matter.js's own
    // TemperatureControl TlvSetTemperatureRequest.
    let got = clusters::temperature_control::encode_set_temperature(Some(-1250), None);
    assert_eq!(got, cmd("temperature_control/cmd_set_temperature.json"));
}

#[test]
fn lock_door_optional_field() {
    let with = clusters::door_lock::encode_lock_door(Some(Secret::new(vec![1, 2, 3, 4])));
    assert_eq!(with, cmd("door_lock/cmd_lock_door_with_pin.json"));
    let without = clusters::door_lock::encode_lock_door(None);
    assert_eq!(without, cmd("door_lock/cmd_lock_door_no_pin.json"));
}

#[test]
fn atomic_request_command_encodes() {
    // Thermostat.AtomicRequest with a populated list<attrib-id> — proves the
    // list-typed-command-field encode matches matter.js byte-for-byte.
    assert_eq!(
        clusters::thermostat::encode_atomic_request(0, &vec![5, 6], Some(1000)),
        cmd("thermostat/cmd_atomic_request.json")
    );
}

#[test]
fn set_weekly_schedule_command() {
    // M9-A3 B4: a Matter 1.4 command the 1.5.1 model removed, generated from
    // supplement-1.4.json, against matter.js 0.16.11's own
    // TlvSetWeeklyScheduleRequest. The first request list whose entries carry a
    // TLV null (CoolSetpoint).
    use clusters::thermostat::{
        encode_set_weekly_schedule, ScheduleDayOfWeekBitmap, ScheduleModeBitmap,
        WeeklyScheduleTransitionStruct,
    };
    let transition = |time, heat| WeeklyScheduleTransitionStruct {
        transition_time: time,
        heat_setpoint: Nullable::Value(heat),
        cool_setpoint: Nullable::Null,
    };
    let got = encode_set_weekly_schedule(
        2,
        ScheduleDayOfWeekBitmap::MONDAY | ScheduleDayOfWeekBitmap::WEDNESDAY,
        ScheduleModeBitmap::HEAT_SETPOINT_PRESENT,
        &vec![transition(360, 2000), transition(1410, 1600)],
    );
    assert_eq!(got, cmd("thermostat/cmd_set_weekly_schedule.json"));
}

#[test]
fn open_with_null_duration_command() {
    // M9-A3 B4: the first request field that is optional and nullable
    // (`Option<Nullable<u32>>`), sent as TLV null, against matter.js's own
    // ValveConfigurationAndControl TlvOpenRequest.
    let got =
        clusters::valve_configuration_and_control::encode_open(Some(Nullable::Null), Some(50));
    assert_eq!(
        got,
        cmd("valve_configuration_and_control/cmd_open_null_duration.json")
    );
}

#[test]
fn add_scene_command() {
    // M9-A3 B4: nested request lists (ExtensionFieldSetStruct ->
    // AttributeValuePairStruct) whose entries leave out all but one of eight
    // optional value fields, with an unsigned and a signed value, against
    // matter.js's own ScenesManagement TlvAddSceneRequest.
    use clusters::scenes_management::{
        encode_add_scene, AttributeValuePairStruct, ExtensionFieldSetStruct,
    };
    let pair = |attribute_id| AttributeValuePairStruct {
        attribute_id,
        value_unsigned8: None,
        value_signed8: None,
        value_unsigned16: None,
        value_signed16: None,
        value_unsigned32: None,
        value_signed32: None,
        value_unsigned64: None,
        value_signed64: None,
    };
    let set = |cluster_id, pair| ExtensionFieldSetStruct {
        cluster_id,
        attribute_value_list: vec![pair],
    };
    let mut on = pair(0x0000);
    on.value_unsigned8 = Some(1);
    let mut level = pair(0x0000);
    level.value_unsigned8 = Some(128);
    let mut heat = pair(0x0012);
    heat.value_signed16 = Some(-250);
    let got = encode_add_scene(
        0,
        1,
        1000,
        &"Evening".to_string(),
        &vec![set(0x0006, on), set(0x0008, level), set(0x0201, heat)],
    );
    assert_eq!(got, cmd("scenes_management/cmd_add_scene.json"));
}

#[test]
fn network_interface_struct_decodes() {
    // GeneralDiagnostics.NetworkInterfaces: a NetworkInterface with a hwadr
    // bytes field, a keyword `Type` field, and byte-string-element lists
    // (the new A2.4 shape). Field names are the generated snake form (IPv4 ->
    // i_pv4).
    let ifaces = clusters::general_diagnostics::decode_network_interfaces(&attr(
        "general_diagnostics/attr_network_interfaces.json",
    ))
    .unwrap();
    assert_eq!(ifaces.len(), 1);
    let ni = &ifaces[0];
    assert_eq!(ni.name, "eth0");
    assert!(ni.is_operational);
    assert_eq!(
        ni.hardware_address,
        vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0x01]
    );
    assert_eq!(ni.ipv4_addresses, vec![vec![192, 168, 1, 5]]);
    assert_eq!(ni.ipv6_addresses.len(), 1);
    assert_eq!(ni.ipv6_addresses[0].len(), 16);
    assert_eq!(ni.r#type.to_raw(), 1); // Wi-Fi
}

#[test]
fn float_attribute_decodes_matter_js_bytes() {
    // #112: the concentration-measurement family is the first cluster shape in
    // this crate with a float (`single`) on the wire, so it earns a matter.js
    // vector rather than a hand-built one. The bytes must be TLV FLOAT32
    // (control byte 0x0A) — a FLOAT64 or an integer encoding would decode to
    // the wrong value or not at all. Both values are exactly representable in
    // binary32, so these are exact equalities. All 10 clusters share this
    // shape, so CarbonDioxide stands in for the family.
    let present = attr("carbon_dioxide_concentration_measurement/attr_measured_value_present.json");
    assert_eq!(present[0], 0x0a, "matter.js must emit anonymous FLOAT32");
    assert_eq!(
        clusters::carbon_dioxide_concentration_measurement::decode_measured_value(&present)
            .unwrap(),
        Nullable::Value(415.5)
    );

    let null = attr("carbon_dioxide_concentration_measurement/attr_measured_value_null.json");
    assert_eq!(
        clusters::carbon_dioxide_concentration_measurement::decode_measured_value(&null).unwrap(),
        Nullable::Null
    );

    // Uncertainty is the non-nullable float in the same cluster. Compared by
    // bits (stricter than `==`, which would accept `-0.0` for `0.0`).
    let unc = attr("carbon_dioxide_concentration_measurement/attr_uncertainty.json");
    assert_eq!(
        clusters::carbon_dioxide_concentration_measurement::decode_uncertainty(&unc)
            .unwrap()
            .to_bits(),
        0.25_f32.to_bits()
    );
}

#[test]
fn review_fabric_restrictions_recursive_list_encodes() {
    // AccessControl.ReviewFabricRestrictions — Arl is list<struct{.., list<struct>}>;
    // the recursive list-of-struct command encode (M9-A2.5 gap 2) must match
    // matter.js byte-for-byte. AccessRestrictionStruct.Id is Nullable<u32>.
    let arl = vec![
        clusters::access_control::CommissioningAccessRestrictionEntryStruct {
            endpoint: 1,
            cluster: 0x0006,
            restrictions: vec![clusters::access_control::AccessRestrictionStruct {
                r#type: clusters::access_control::AccessRestrictionTypeEnum::from_raw(0),
                id: Nullable::Value(0x1234),
            }],
        },
    ];
    assert_eq!(
        clusters::access_control::encode_review_fabric_restrictions(&arl),
        cmd("access_control/cmd_review_fabric_restrictions.json")
    );
}

#[test]
fn cancel_messages_request_encodes_a_byte_string_list() {
    // M9-A3 B5: CancelMessagesRequest.MessageIDs is a list<octstr>, the first
    // request list whose elements are byte strings (a wire shape the emitter
    // had not generated before), against matter.js's own
    // TlvCancelMessagesRequest.
    let a: Vec<u8> = (0x00..0x10).collect();
    let b: Vec<u8> = (0xf0..=0xff).collect();
    assert_eq!(
        clusters::messages::encode_cancel_messages_request(&vec![a, b]),
        cmd("messages/cmd_cancel_messages_request.json")
    );
}

#[test]
fn present_messages_request_encodes_an_optional_list() {
    // M9-A3 B5: Responses is the first optional request list (here present,
    // two structs), next to a 16-byte octstr, a map16 bitmap value that fits
    // in one byte, a null epoch-s and a nullable uint64 with a value, against
    // matter.js's own TlvPresentMessagesRequest.
    use clusters::messages::{
        encode_present_messages_request, MessageControlBitmap, MessagePriorityEnum,
        MessageResponseOptionStruct,
    };
    let id: Vec<u8> = (0x00..0x10).collect();
    let responses = vec![
        MessageResponseOptionStruct {
            message_response_id: 1,
            label: "Coming".to_string(),
        },
        MessageResponseOptionStruct {
            message_response_id: 2,
            label: "Later".to_string(),
        },
    ];
    assert_eq!(
        encode_present_messages_request(
            &id,
            MessagePriorityEnum::High,
            MessageControlBitmap::CONFIRMATION_REQUIRED | MessageControlBitmap::RESPONSE_REQUIRED,
            Nullable::Null,
            Nullable::Value(60_000),
            &"Dinner is ready".to_string(),
            Some(responses),
        ),
        cmd("messages/cmd_present_messages_request.json")
    );
}

#[test]
fn launch_url_encodes_nested_structs_and_doubles() {
    // M9-A3 B7: BrandingInformationStruct nests StyleInformationStruct, which
    // nests DimensionStruct: the first request struct with struct fields (a new
    // wire shape for the emitter), and its Width / Height the first `double`
    // (TLV FLOAT64), against matter.js's own TlvLaunchUrlRequest.
    use clusters::content_launcher::{
        encode_launch_url, BrandingInformationStruct, DimensionStruct, MetricTypeEnum,
        StyleInformationStruct,
    };
    let style = |image_url: Option<&str>, color: Option<&str>, size| StyleInformationStruct {
        image_url: image_url.map(str::to_string),
        color: color.map(str::to_string),
        size: Some(size),
    };
    let branding = BrandingInformationStruct {
        provider_name: "Example".to_string(),
        background: Some(style(
            Some("https://example.com/bg.png"),
            Some("#000000"),
            DimensionStruct {
                width: 1920.5,
                height: 1080.25,
                metric: MetricTypeEnum::Pixels,
            },
        )),
        logo: Some(style(
            None,
            None,
            DimensionStruct {
                width: 12.5,
                height: 7.25,
                metric: MetricTypeEnum::Percentage,
            },
        )),
        progress_bar: None,
        splash: None,
        watermark: None,
    };
    assert_eq!(
        encode_launch_url(
            &"https://example.com/watch".to_string(),
            Some("Now playing".to_string()),
            Some(branding),
            None,
        ),
        cmd("content_launcher/cmd_launch_url.json")
    );
}

#[test]
fn launch_content_encodes_optional_and_nullable_lists_in_structs() {
    // M9-A3 B7: ParameterStruct.ExternalIdList (an optional list, present and
    // absent), PlaybackPreferencesStruct's null PlaybackPosition, its TextTrack
    // with a null Characteristics list, and an AudioTracks list whose entry
    // carries a Characteristics list of two enums: optional and nullable lists
    // inside request structs (a new wire shape for the emitter), against
    // matter.js's own TlvLaunchContentRequest.
    use clusters::content_launcher::{
        encode_launch_content, AdditionalInfoStruct, CharacteristicEnum, ContentSearchStruct,
        ParameterEnum, ParameterStruct, PlaybackPreferencesStruct, TrackPreferenceStruct,
    };
    let info = |name: &str, value: &str| AdditionalInfoStruct {
        name: name.to_string(),
        value: value.to_string(),
    };
    let search = ContentSearchStruct {
        parameter_list: vec![
            ParameterStruct {
                r#type: ParameterEnum::Actor,
                value: "Gaby sHoffman".to_string(),
                external_id_list: Some(vec![info("imdb", "nm0001"), info("tmdb", "42")]),
            },
            ParameterStruct {
                r#type: ParameterEnum::Genre,
                value: "Horror".to_string(),
                external_id_list: None,
            },
        ],
    };
    let prefs = PlaybackPreferencesStruct {
        playback_position: Some(Nullable::Null),
        text_track: Some(Nullable::Value(TrackPreferenceStruct {
            language_code: "en".to_string(),
            characteristics: Some(Nullable::Null),
            audio_output_index: None,
        })),
        audio_tracks: Some(Nullable::Value(vec![TrackPreferenceStruct {
            language_code: "fr".to_string(),
            characteristics: Some(Nullable::Value(vec![
                CharacteristicEnum::ForcedSubtitles,
                CharacteristicEnum::Karaoke,
            ])),
            audio_output_index: Some(Nullable::Value(1)),
        }])),
    };
    assert_eq!(
        encode_launch_content(search, true, None, Some(prefs), Some(false)),
        cmd("content_launcher/cmd_launch_content.json")
    );
}

#[test]
fn launch_url_encodes_playback_preferences_with_null_track_and_list() {
    // M9-A3 B7: LaunchURL's PlaybackPreferences (field 3), with a non-null
    // PlaybackPosition, a null nested struct (TextTrack) and a null whole
    // list (AudioTracks), against matter.js's own TlvLaunchUrlRequest.
    use clusters::content_launcher::{encode_launch_url, PlaybackPreferencesStruct};
    let prefs = PlaybackPreferencesStruct {
        playback_position: Some(Nullable::Value(42)),
        text_track: Some(Nullable::Null),
        audio_tracks: Some(Nullable::Null),
    };
    assert_eq!(
        encode_launch_url(&"u".to_string(), None, None, Some(prefs)),
        cmd("content_launcher/cmd_launch_url_preferences.json")
    );
}

#[test]
fn launch_content_encodes_data_empty_lists_and_null_output_index() {
    // M9-A3 B7: LaunchContent's Data, an empty ParameterList, and an
    // AudioTracks entry with an empty Characteristics list and a null
    // AudioOutputIndex, against matter.js's own TlvLaunchContentRequest.
    use clusters::content_launcher::{
        encode_launch_content, ContentSearchStruct, PlaybackPreferencesStruct,
        TrackPreferenceStruct,
    };
    let prefs = PlaybackPreferencesStruct {
        playback_position: None,
        text_track: None,
        audio_tracks: Some(Nullable::Value(vec![TrackPreferenceStruct {
            language_code: "de".to_string(),
            characteristics: Some(Nullable::Value(Vec::new())),
            audio_output_index: Some(Nullable::Null),
        }])),
    };
    assert_eq!(
        encode_launch_content(
            ContentSearchStruct {
                parameter_list: Vec::new(),
            },
            false,
            Some("d".to_string()),
            Some(prefs),
            None,
        ),
        cmd("content_launcher/cmd_launch_content_data.json")
    );
}

#[test]
fn get_program_guide_uses_the_specifications_field_ids() {
    // M9-A3 B7: every GetProgramGuide field, against matter.js's own
    // TlvGetProgramGuideRequest. RecordingFlag, ExternalIDList and Data are
    // tags 5, 6 and 7 in the specification and matter.js; chip's codegen
    // numbers them 4, 5 and 6 (both refs). ExternalIDList's entries are
    // Channel's copy of ContentLauncher's AdditionalInfoStruct.
    use clusters::channel::{
        encode_get_program_guide, AdditionalInfoStruct, ChannelInfoStruct, PageTokenStruct,
        RecordingFlagBitmap,
    };
    let abc = ChannelInfoStruct {
        major_number: 6,
        minor_number: 0,
        name: Some("ABC".to_string()),
        call_sign: None,
        affiliate_call_sign: None,
        identifier: None,
        r#type: None,
    };
    let page = PageTokenStruct {
        limit: Some(10),
        after: Some("a".to_string()),
        before: Some("b".to_string()),
    };
    let ids = vec![AdditionalInfoStruct {
        name: "imdb".to_string(),
        value: "tt0001".to_string(),
    }];
    assert_eq!(
        encode_get_program_guide(
            1000,
            4600,
            Some(vec![abc]),
            Some(Nullable::Value(page)),
            Some(Nullable::Value(
                RecordingFlagBitmap::SCHEDULED | RecordingFlagBitmap::RECORD_SERIES
            )),
            Some(ids),
            Some(vec![0x01, 0x02]),
        ),
        cmd("channel/cmd_get_program_guide.json")
    );
}

#[test]
fn set_block_content_time_window_matches_matter_js() {
    // M9-A3 B7: ContentControl's deepest request (a struct at tag 0 holding a
    // nullable index, a map8 and a list of structs), against matter.js's own
    // TlvSetBlockContentTimeWindowRequest, with the index null and set.
    use clusters::content_control::{
        encode_set_block_content_time_window, DayOfWeekBitmap, TimePeriodStruct, TimeWindowStruct,
    };
    let period = |start_hour, start_minute, end_hour, end_minute| TimePeriodStruct {
        start_hour,
        start_minute,
        end_hour,
        end_minute,
    };
    assert_eq!(
        encode_set_block_content_time_window(TimeWindowStruct {
            time_window_index: Nullable::Null,
            day_of_week: DayOfWeekBitmap::MONDAY | DayOfWeekBitmap::SATURDAY,
            time_period: vec![period(8, 30, 9, 0), period(22, 0, 23, 59)],
        }),
        cmd("content_control/cmd_set_block_content_time_window_null_index.json")
    );
    assert_eq!(
        encode_set_block_content_time_window(TimeWindowStruct {
            time_window_index: Nullable::Value(7),
            day_of_week: DayOfWeekBitmap::SUNDAY,
            time_period: vec![period(6, 15, 7, 45)],
        }),
        cmd("content_control/cmd_set_block_content_time_window_index.json")
    );
}

// ---- fabric-scoped list entries: the write form leaves FabricIndex out ----
//
// matter.js `forWriteInteraction` encodings of an entry without `fabricIndex`,
// which is what chip's EncodeForWrite always sends. `fabric_index` is set to a
// value the server would overwrite, to show it is not written.

#[test]
fn binding_target_write_form_matches_matter_js() {
    let t = clusters::binding::TargetStruct {
        node: Some(0x1122),
        group: None,
        endpoint: Some(1),
        cluster: Some(0x0006),
        fabric_index: 9,
    };
    assert_eq!(t.encode(), attr("binding/attr_binding_entry_write.json"));
}

#[test]
fn group_key_map_write_form_matches_matter_js() {
    let m = clusters::group_key_management::GroupKeyMapStruct {
        group_id: 0x0101,
        group_key_set_id: 0x01a1,
        fabric_index: 9,
    };
    assert_eq!(
        m.encode(),
        attr("group_key_management/attr_group_key_map_entry_write.json")
    );
}

#[test]
fn provider_location_write_form_matches_matter_js() {
    let p = clusters::ota_software_update_requestor::ProviderLocation {
        provider_node_id: 0x0102_0304,
        endpoint: 0,
        fabric_index: 9,
    };
    assert_eq!(
        p.encode(),
        attr("ota_software_update_requestor/attr_default_ota_providers_entry_write.json")
    );
}

#[test]
fn access_control_extension_write_form_matches_matter_js() {
    let e = clusters::access_control::AccessControlExtensionStruct {
        data: Some(vec![0x15, 0x18]),
        fabric_index: 9,
    };
    assert_eq!(
        e.encode().unwrap(),
        attr("access_control/attr_extension_entry_write.json")
    );
}
