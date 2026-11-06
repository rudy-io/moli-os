//! Decode-smoke for the M9-A2.1 pilot and M9-A2.2 energy clusters: each
//! generated decoder reads a representative attribute's wire value. These
//! clusters are read-only; A2.1 reuses datatype shapes already byte-parity-proven
//! by the M7 clusters, and A2.2's one genuinely-new nested shape
//! (`MeasurementAccuracyStruct`) gets a dedicated matter.js byte-parity vector in
//! `byte_parity.rs`. Here a synthetic decode (construct TLV → decode → assert) is
//! the gate. (Roundtrip applies to writable attrs in later batches.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use matter_clusters::clusters;
use matter_clusters::types::{Nullable, Secret};
use matter_codec::{Tag, TlvWriter};

/// Encode a single anonymous-tagged unsigned scalar (the wire shape of a
/// read-only scalar attribute value).
fn uint_attr(v: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    TlvWriter::new(&mut buf)
        .put_uint(Tag::Anonymous, v)
        .unwrap();
    buf
}
fn int_attr(v: i64) -> Vec<u8> {
    let mut buf = Vec::new();
    TlvWriter::new(&mut buf).put_int(Tag::Anonymous, v).unwrap();
    buf
}
fn bool_attr(v: bool) -> Vec<u8> {
    let mut buf = Vec::new();
    TlvWriter::new(&mut buf)
        .put_bool(Tag::Anonymous, v)
        .unwrap();
    buf
}
fn null_attr() -> Vec<u8> {
    let mut buf = Vec::new();
    TlvWriter::new(&mut buf).put_null(Tag::Anonymous).unwrap();
    buf
}

#[test]
fn illuminance_measured_value_decodes() {
    // MeasuredValue: nullable uint16.
    assert_eq!(
        clusters::illuminance_measurement::decode_measured_value(&uint_attr(12345)).unwrap(),
        Nullable::Value(12345)
    );
    assert_eq!(
        clusters::illuminance_measurement::decode_measured_value(&null_attr()).unwrap(),
        Nullable::Null
    );
}

#[test]
fn pressure_measured_value_decodes() {
    // MeasuredValue: nullable int16.
    assert_eq!(
        clusters::pressure_measurement::decode_measured_value(&int_attr(-50)).unwrap(),
        Nullable::Value(-50)
    );
}

#[test]
fn flow_measured_value_decodes() {
    // MeasuredValue: nullable uint16.
    assert_eq!(
        clusters::flow_measurement::decode_measured_value(&uint_attr(200)).unwrap(),
        Nullable::Value(200)
    );
}

#[test]
fn boolean_state_state_value_decodes() {
    // StateValue: bool.
    assert!(clusters::boolean_state::decode_state_value(&bool_attr(true)).unwrap());
}

#[test]
fn switch_current_position_decodes() {
    // CurrentPosition: uint8 (not nullable).
    assert_eq!(
        clusters::switch::decode_current_position(&uint_attr(2)).unwrap(),
        2
    );
}

// ---- M9-A2.2 energy batch -------------------------------------------------
// These exercise the new shapes A2.2 added to the emitter: a list of named
// enums (gap 6), a nullable struct-valued attribute (gap 7), a nullable list
// (gap 8), the energy semantic scalars (gap 3), and an `Unknown`-member enum
// with its renamed `Unrecognized` catch-all (gaps 1/5).

/// Encode an anonymous-tagged array of anonymous unsigned scalars (the wire
/// shape of a `list<enum8>` / `list<endpoint-no>` attribute value).
fn uint_array_attr(values: &[u64]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_array(Tag::Anonymous).unwrap();
        for &v in values {
            w.put_uint(Tag::Anonymous, v).unwrap();
        }
        w.end_container().unwrap();
    }
    buf
}

#[test]
fn air_quality_decodes() {
    use clusters::air_quality::AirQualityEnum;
    assert_eq!(
        clusters::air_quality::decode_air_quality(&uint_attr(1)).unwrap(),
        AirQualityEnum::Good
    );
    // The model member named `Unknown` (value 0) is a fieldless variant…
    assert_eq!(
        clusters::air_quality::decode_air_quality(&uint_attr(0)).unwrap(),
        AirQualityEnum::Unknown
    );
    // …and an out-of-range discriminant lands in the renamed catch-all.
    assert_eq!(
        clusters::air_quality::decode_air_quality(&uint_attr(99)).unwrap(),
        AirQualityEnum::Unrecognized(99)
    );
}

#[test]
fn power_source_status_and_lists_decode() {
    use clusters::power_source::{PowerSourceStatusEnum, WiredFaultEnum};
    // Status: mandatory enum8.
    assert_eq!(
        clusters::power_source::decode_status(&uint_attr(1)).unwrap(),
        PowerSourceStatusEnum::Active
    );
    // ActiveWiredFaults: list<WiredFaultEnum> -> Vec<WiredFaultEnum> (gap 6).
    assert_eq!(
        clusters::power_source::decode_active_wired_faults(&uint_array_attr(&[1])).unwrap(),
        vec![WiredFaultEnum::OverVoltage]
    );
    // EndpointList: list<endpoint-no> -> Vec<u16>.
    assert_eq!(
        clusters::power_source::decode_endpoint_list(&uint_array_attr(&[1, 2])).unwrap(),
        vec![1u16, 2u16]
    );
}

#[test]
fn electrical_power_measurement_decodes() {
    use clusters::electrical_power_measurement as epm;
    // PowerMode: mandatory enum8 (model has an `Unknown` member -> renamed catch-all).
    assert_eq!(
        epm::decode_power_mode(&uint_attr(2)).unwrap(),
        epm::PowerModeEnum::Ac
    );
    // Voltage: nullable voltage-mV -> Nullable<i64> (gap 3).
    assert_eq!(
        epm::decode_voltage(&int_attr(230_000)).unwrap(),
        Nullable::Value(230_000)
    );
    // Accuracy: list<MeasurementAccuracyStruct> -> Vec<…>; empty array -> empty Vec.
    assert_eq!(
        epm::decode_accuracy(&uint_array_attr(&[])).unwrap(),
        Vec::<epm::MeasurementAccuracyStruct>::new()
    );
    // HarmonicCurrents: nullable list -> Nullable<Vec<…>> (gap 8); null decodes to Null.
    assert!(matches!(
        epm::decode_harmonic_currents(&null_attr()).unwrap(),
        Nullable::Null
    ));
}

#[test]
fn electrical_energy_measurement_nullable_struct_attr_decodes() {
    // CumulativeEnergyImported: nullable EnergyMeasurementStruct -> Nullable<…>
    // (gap 7); a TLV null decodes to Nullable::Null.
    assert!(matches!(
        clusters::electrical_energy_measurement::decode_cumulative_energy_imported(&null_attr())
            .unwrap(),
        Nullable::Null
    ));
}

// ---- M9-A2.3 actuator batch ----------------------------------------------

#[test]
fn thermostat_system_mode_decodes() {
    use clusters::thermostat::SystemModeEnum;
    // SystemMode: enum8; raw 4 = Heat (spot-check a known member).
    assert_eq!(
        clusters::thermostat::decode_system_mode(&uint_attr(4)).unwrap(),
        SystemModeEnum::Heat
    );
}

#[test]
fn thermostat_atomic_response_decodes_synth_struct() {
    use matter_codec::{ContainerKind, Element, TlvReader};
    // Hand-build an AtomicResponse payload: anon struct {
    //   ctx0 = StatusCode(0),
    //   ctx1 = array[ struct{ ctx0=AttributeId(0x1234), ctx1=StatusCode(0) } ],
    //   ctx2 = Timeout(1000) }.
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.start_array(Tag::Context(1)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 0x1234).unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(2), 1000).unwrap();
        w.end_container().unwrap();
    }
    let mut r = TlvReader::new(&buf);
    // Consume the opening anonymous structure, then decode the fields.
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart {
            kind: ContainerKind::Structure,
            ..
        })
    ));
    let resp = clusters::thermostat::AtomicResponse::decode_from(&mut r).unwrap();
    assert_eq!(resp.status_code, 0);
    assert_eq!(resp.attribute_status.len(), 1);
    assert_eq!(resp.attribute_status[0].attribute_id, 0x1234);
    assert_eq!(resp.attribute_status[0].status_code, 0);
    assert_eq!(resp.timeout, Some(1000));
}

#[test]
fn fan_control_fan_mode_decodes() {
    use clusters::fan_control::FanModeEnum;
    // FanMode: enum8; raw 3 = High.
    assert_eq!(
        clusters::fan_control::decode_fan_mode(&uint_attr(3)).unwrap(),
        FanModeEnum::High
    );
}

#[test]
fn tuic_keypad_lockout_decodes() {
    use clusters::thermostat_user_interface_configuration::KeypadLockoutEnum;
    // KeypadLockout: enum8; raw 0 = NoLockout.
    assert_eq!(
        clusters::thermostat_user_interface_configuration::decode_keypad_lockout(&uint_attr(0))
            .unwrap(),
        KeypadLockoutEnum::NoLockout
    );
}

#[test]
fn pump_operation_mode_decodes() {
    use clusters::pump_configuration_and_control::OperationModeEnum;
    // OperationMode: enum8; raw 0 = Normal.
    assert_eq!(
        clusters::pump_configuration_and_control::decode_operation_mode(&uint_attr(0)).unwrap(),
        OperationModeEnum::Normal
    );
}

#[test]
fn window_covering_mode_decodes() {
    // Mode: map8 bitmap; bit0 = MotorDirectionReversed (raw 1).
    let m = clusters::window_covering::decode_mode(&uint_attr(1)).unwrap();
    assert_eq!(m.bits(), 1);
}

// ---- M9-A2.4 utility batch ------------------------------------------------

#[test]
fn binding_target_struct_decodes_fabric_index() {
    use matter_codec::{ContainerKind, Element, TlvReader};
    // TargetStruct { Cluster(4)=0x0006, FabricIndex(254)=1 } — proves the
    // global FabricIndex typedef de-aliases to u8 (gap 1).
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(4), 0x0006).unwrap();
        w.put_uint(Tag::Context(254), 1).unwrap();
        w.end_container().unwrap();
    }
    let mut r = TlvReader::new(&buf);
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart {
            kind: ContainerKind::Structure,
            ..
        })
    ));
    let t = clusters::binding::TargetStruct::decode_from(&mut r).unwrap();
    assert_eq!(t.cluster, Some(0x0006));
    assert_eq!(t.fabric_index, 1u8);
}

#[test]
fn fixed_label_label_struct_decodes() {
    use matter_codec::{ContainerKind, Element, TlvReader};
    // LabelStruct { Label(0)="room", Value(1)="kitchen" }.
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_utf8(Tag::Context(0), "room").unwrap();
        w.put_utf8(Tag::Context(1), "kitchen").unwrap();
        w.end_container().unwrap();
    }
    let mut r = TlvReader::new(&buf);
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart {
            kind: ContainerKind::Structure,
            ..
        })
    ));
    let l = clusters::fixed_label::LabelStruct::decode_from(&mut r).unwrap();
    assert_eq!(l.label, "room");
    assert_eq!(l.value, "kitchen");
}

#[test]
fn groups_add_group_command_encodes_wellformed() {
    use matter_codec::{Element, TlvReader, Value};
    // encode_add_group(group_id, group_name) -> anon struct { ctx0=uint, ctx1=utf8 }.
    let bytes = clusters::groups::encode_add_group(0x0007, &"den".to_string());
    let mut r = TlvReader::new(&bytes);
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart { .. })
    ));
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::Scalar {
            tag: Tag::Context(0),
            value: Value::Uint(7)
        })
    ));
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::Scalar { tag: Tag::Context(1), value: Value::Utf8(ref s) }) if s == "den"
    ));
}

#[test]
fn groups_get_group_membership_command_encodes_list() {
    use matter_codec::{ContainerKind, Element, TlvReader, Value};
    // encode_get_group_membership(list<group-id>) -> anon struct { ctx0=array[uint,uint] }
    // (reuses the A2.3 list-typed-command-field encode codepath).
    let bytes = clusters::groups::encode_get_group_membership(&vec![1u16, 2u16]);
    let mut r = TlvReader::new(&bytes);
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart { .. })
    ));
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart {
            tag: Tag::Context(0),
            kind: ContainerKind::Array
        })
    ));
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::Scalar {
            value: Value::Uint(1),
            ..
        })
    ));
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::Scalar {
            value: Value::Uint(2),
            ..
        })
    ));
}

// ---- M9-A2.5 management batch ----------------------------------------------

#[test]
fn access_control_entry_decodes_subjects_u64() {
    use matter_codec::{ContainerKind, Element, TlvReader};
    // AccessControlEntryStruct { Privilege(1)=5, AuthMode(2)=2,
    //   Subjects(3)=[0x1122334455667788], Targets(4)=null, FabricIndex(254)=1 }.
    // Proves subject-id -> u64 (gap 1) and nullable list-of-scalar decode.
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(1), 5).unwrap();
        w.put_uint(Tag::Context(2), 2).unwrap();
        w.start_array(Tag::Context(3)).unwrap();
        w.put_uint(Tag::Anonymous, 0x1122_3344_5566_7788).unwrap();
        w.end_container().unwrap();
        w.put_null(Tag::Context(4)).unwrap();
        w.put_uint(Tag::Context(254), 1).unwrap();
        w.end_container().unwrap();
    }
    let mut r = TlvReader::new(&buf);
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart {
            kind: ContainerKind::Structure,
            ..
        })
    ));
    let e = clusters::access_control::AccessControlEntryStruct::decode_from(&mut r).unwrap();
    // Subjects/Targets are fabric-sensitive, so `Option`-wrapped (M9-A3 B1);
    // our own fabric's entry carries them.
    assert_eq!(
        e.subjects,
        Some(Nullable::Value(vec![0x1122_3344_5566_7788u64]))
    );
    assert!(matches!(e.targets, Some(Nullable::Null)));
    assert_eq!(e.fabric_index, 1u8);
}

#[test]
fn group_key_set_write_encodes_wellformed() {
    use matter_codec::{Element, TlvReader};
    // KeySetWrite wraps a GroupKeySetStruct at ctx0 (single struct command field,
    // a shape DoorLock's SetCredential already proves). Smoke: encode is a
    // well-formed anon struct holding a nested struct.
    let gks = clusters::group_key_management::GroupKeySetStruct {
        group_key_set_id: 0x0042,
        group_key_security_policy:
            clusters::group_key_management::GroupKeySecurityPolicyEnum::from_raw(0),
        epoch_key0: Nullable::Value(vec![0xab; 16]),
        epoch_start_time0: Nullable::Value(1234),
        epoch_key1: Nullable::Null,
        epoch_start_time1: Nullable::Null,
        epoch_key2: Nullable::Null,
        epoch_start_time2: Nullable::Null,
        group_key_multicast_policy: None,
        fabric_index: None,
    };
    let bytes = clusters::group_key_management::encode_key_set_write(gks);
    let mut r = TlvReader::new(&bytes);
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart { .. })
    ));
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart {
            tag: Tag::Context(0),
            ..
        })
    ));
}

#[test]
fn admin_open_basic_commissioning_window_encodes() {
    use matter_codec::{Element, TlvReader, Value};
    let bytes = clusters::administrator_commissioning::encode_open_basic_commissioning_window(180);
    let mut r = TlvReader::new(&bytes);
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart { .. })
    ));
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::Scalar {
            tag: Tag::Context(0),
            value: Value::Uint(180)
        })
    ));
}

#[test]
fn ota_announce_provider_encodes_scalars_and_enum() {
    use clusters::ota_software_update_requestor::AnnouncementReasonEnum;
    use matter_codec::{Element, TlvReader, Value};
    // metadata_for_node is optional -> None skips ctx3; ctx0 is the node id.
    let bytes = clusters::ota_software_update_requestor::encode_announce_ota_provider(
        0x0000_0000_0000_1234,
        0xFFF1,
        AnnouncementReasonEnum::SimpleAnnouncement,
        None,
        1,
    );
    let mut r = TlvReader::new(&bytes);
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart { .. })
    ));
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::Scalar {
            tag: Tag::Context(0),
            value: Value::Uint(0x1234)
        })
    ));
}

#[test]
fn ota_provider_query_image_encodes_scalars() {
    use clusters::ota_software_update_provider::{encode_query_image, DownloadProtocolEnum};
    use matter_codec::{Element, TlvReader, Value};
    let bytes = encode_query_image(
        0xFFF1,
        0x8000,
        5,
        &vec![DownloadProtocolEnum::BdxSynchronous],
        None,
        None,
        None,
        None,
    );
    let mut r = TlvReader::new(&bytes);
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart { .. })
    ));
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::Scalar {
            tag: Tag::Context(0),
            value: Value::Uint(0xFFF1)
        })
    ));
}

#[test]
fn ota_provider_query_image_response_decodes() {
    use clusters::ota_software_update_provider::{QueryImageResponse, StatusEnum};
    use matter_codec::{Element, Tag, TlvReader, TlvWriter};
    // Hand-build a minimal QueryImageResponse: ctx0 Status = UpdateAvailable(0).
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous).unwrap();
    w.put_uint(Tag::Context(0), 0).unwrap(); // Status = UpdateAvailable
    w.end_container().unwrap();
    let mut r = TlvReader::new(&buf);
    assert!(matches!(
        r.next().unwrap(),
        Some(Element::ContainerStart { .. })
    ));
    let decoded = QueryImageResponse::decode_from(&mut r).expect("decode QueryImageResponse");
    assert_eq!(decoded.status, StatusEnum::UpdateAvailable);
}

#[test]
fn time_sync_utc_time_and_granularity_decode() {
    use clusters::time_synchronization::{decode_granularity, decode_utc_time, GranularityEnum};
    // UTCTime is nullable epoch_us; a present value decodes to Nullable::Some.
    let decoded = decode_utc_time(&uint_attr(780_000_000_000_000)).unwrap();
    assert_eq!(decoded, Nullable::Value(780_000_000_000_000));
    // Granularity enum8 = SecondsGranularity(2).
    let g = decode_granularity(&uint_attr(2)).unwrap();
    assert_eq!(g, GranularityEnum::SecondsGranularity);
}

#[test]
fn icd_register_client_response_and_operating_mode_decode() {
    use clusters::icd_management::{
        decode_operating_mode, OperatingModeEnum, RegisterClientResponse,
    };
    use matter_codec::{Tag, TlvWriter};
    // RegisterClientResponse: ctx0 ICDCounter = 7.
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous).unwrap();
    w.put_uint(Tag::Context(0), 7).unwrap();
    w.end_container().unwrap();
    let decoded = RegisterClientResponse::decode(&buf).expect("decode RegisterClientResponse");
    assert_eq!(decoded.icd_counter, 7);
    // OperatingMode enum8 = Lit(1).
    let m = decode_operating_mode(&uint_attr(1)).unwrap();
    assert_eq!(m, OperatingModeEnum::Lit);
}

#[test]
fn time_sync_set_time_zone_response_decodes() {
    use clusters::time_synchronization::SetTimeZoneResponse;
    use matter_codec::{Tag, TlvWriter};
    // Hand-build SetTimeZoneResponse: ctx0 DSTOffsetRequired = true.
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous).unwrap();
    w.put_bool(Tag::Context(0), true).unwrap();
    w.end_container().unwrap();
    let decoded = SetTimeZoneResponse::decode(&buf).expect("decode SetTimeZoneResponse");
    assert!(decoded.dst_offset_required);
}

// ---- concentration measurement family (#112) -----------------------------
//
// The 10 concentration-measurement clusters (Matter 1.2) are *derived* from one
// base cluster, so they share a single shape: nullable `single` (float32)
// measurements, two `elapsed-s` windows, and three enums. That shape is the
// first FLOAT on the wire in this crate, so it also gets a matter.js
// byte-parity vector (`byte_parity.rs::float_attribute_decodes_matter_js_bytes`)
// on CarbonDioxide as the family representative; the per-cluster tests below
// are the usual synthetic decode-smoke, proving every generated module is
// wired up and decodes its float.

/// Encode a single anonymous-tagged FLOAT32 (the wire shape of a `single`
/// attribute value). These clusters are read-only, so there is no generated
/// `encode_*` to pair with — the codec writer is the encoder half here.
fn float_attr(v: f32) -> Vec<u8> {
    let mut buf = Vec::new();
    TlvWriter::new(&mut buf)
        .put_float(Tag::Anonymous, v)
        .unwrap();
    buf
}

/// Encode a single anonymous-tagged FLOAT64 — the *wrong* width for every
/// float attribute the crate generates today (all are `single`). Used only to
/// prove those decoders reject it; the control byte is `0x0B`.
fn double_attr(v: f64) -> Vec<u8> {
    let mut buf = Vec::new();
    TlvWriter::new(&mut buf)
        .put_double(Tag::Anonymous, v)
        .unwrap();
    buf
}

/// Assert two `f32`s are the same value **bit for bit**. Stricter than `==`
/// (which accepts `-0.0` for `0.0` and can never match a NaN), and it keeps
/// `clippy::float_cmp` quiet without an allow.
#[track_caller]
fn assert_f32_eq(actual: f32, expected: f32) {
    assert_eq!(
        actual.to_bits(),
        expected.to_bits(),
        "expected {expected}, got {actual}"
    );
}

#[test]
fn carbon_dioxide_concentration_full_attribute_set_decodes() {
    use clusters::carbon_dioxide_concentration_measurement as co2;
    use co2::{LevelValueEnum, MeasurementMediumEnum, MeasurementUnitEnum};

    // Nullable float32 measurements: a value, and TLV null.
    assert_eq!(
        co2::decode_measured_value(&float_attr(415.5)).unwrap(),
        Nullable::Value(415.5)
    );
    assert_eq!(
        co2::decode_measured_value(&null_attr()).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        co2::decode_min_measured_value(&float_attr(0.0)).unwrap(),
        Nullable::Value(0.0)
    );
    assert_eq!(
        co2::decode_max_measured_value(&float_attr(5000.0)).unwrap(),
        Nullable::Value(5000.0)
    );
    assert_eq!(
        co2::decode_peak_measured_value(&float_attr(1200.25)).unwrap(),
        Nullable::Value(1200.25)
    );
    assert_eq!(
        co2::decode_average_measured_value(&float_attr(-12.5)).unwrap(),
        Nullable::Value(-12.5)
    );
    // Non-nullable float32.
    assert_f32_eq(co2::decode_uncertainty(&float_attr(0.25)).unwrap(), 0.25);
    // elapsed-s windows are plain u32, not floats.
    assert_eq!(
        co2::decode_peak_measured_value_window(&uint_attr(3600)).unwrap(),
        3600
    );
    assert_eq!(
        co2::decode_average_measured_value_window(&uint_attr(300)).unwrap(),
        300
    );
    // The three enums.
    assert_eq!(
        co2::decode_measurement_unit(&uint_attr(0)).unwrap(),
        MeasurementUnitEnum::Ppm
    );
    assert_eq!(
        co2::decode_measurement_medium(&uint_attr(0)).unwrap(),
        MeasurementMediumEnum::Air
    );
    assert_eq!(
        co2::decode_level_value(&uint_attr(4)).unwrap(),
        LevelValueEnum::Critical
    );
    // Forward-compat: an unknown enum discriminant is preserved, not rejected.
    assert_eq!(
        co2::decode_level_value(&uint_attr(99)).unwrap(),
        LevelValueEnum::Unrecognized(99)
    );
}

#[test]
fn float_attribute_rejects_non_float_wire_types() {
    use clusters::carbon_dioxide_concentration_measurement as co2;
    // A float attribute encoded as an integer (or any other type) is a type
    // mismatch, not a silent coercion — the pre-#112 emitter fallthrough would
    // have decoded these as integers.
    assert!(co2::decode_measured_value(&uint_attr(415)).is_err());
    assert!(co2::decode_uncertainty(&int_attr(-1)).is_err());
    assert!(co2::decode_uncertainty(&bool_attr(true)).is_err());
    // …and a null in a non-nullable float attribute is still an error.
    assert!(co2::decode_uncertainty(&null_attr()).is_err());
    // A FLOAT64 element in a `single` attribute is rejected too — the case
    // with real interop consequences, so it is pinned rather than assumed.
    // This is deliberate and matches chip: `TLVReader::Get(float&)`
    // (`src/lib/core/TLVReader.cpp`) accepts FLOAT32 only and errors on a
    // FLOAT64, while its `Get(double&)` accepts both. matter.js is lenient in
    // both directions; we follow the stricter reference here. Anyone widening
    // this arm is diverging from chip and must say so.
    assert_eq!(
        double_attr(415.5)[0],
        0x0B,
        "anonymous FLOAT64 control byte"
    );
    assert!(co2::decode_measured_value(&double_attr(415.5)).is_err());
    assert!(co2::decode_uncertainty(&double_attr(0.25)).is_err());
}

#[test]
fn float_wire_roundtrip_including_edge_values() {
    use clusters::carbon_dioxide_concentration_measurement as co2;
    // encode (matter-codec) -> decode (generated) -> equal, across the edges of
    // the binary32 space. Compared by BITS: NaN != NaN and 0.0 == -0.0 under
    // value equality, either of which would make this test lie.
    for v in [
        0.0_f32,
        -0.0,
        1.0,
        -1.0,
        f32::MIN,
        f32::MAX,
        // Smallest positive *normal*, then the smallest subnormal — a distinct
        // encoding class (zero exponent field), and the one the uniform-bits
        // proptest is least likely to draw.
        f32::MIN_POSITIVE,
        f32::from_bits(1),
        -f32::from_bits(1),
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
    ] {
        match co2::decode_measured_value(&float_attr(v)).unwrap() {
            Nullable::Value(d) => assert_f32_eq(d, v),
            Nullable::Null => panic!("float {v} decoded as null"),
        }
    }
}

#[test]
fn every_concentration_cluster_decodes_its_measured_value() {
    // One assertion per generated module: the family was added as a batch
    // (#112) precisely so no member is left out, and this is the guard.
    macro_rules! assert_family_member {
        ($m:ident, $id:expr) => {{
            use clusters::$m as m;
            assert_eq!(m::CLUSTER_ID, $id, concat!(stringify!($m), " cluster id"));
            assert_eq!(
                m::decode_measured_value(&float_attr(1.5)).unwrap(),
                Nullable::Value(1.5)
            );
            assert_eq!(
                m::decode_measured_value(&null_attr()).unwrap(),
                Nullable::Null
            );
            assert_f32_eq(m::decode_uncertainty(&float_attr(0.5)).unwrap(), 0.5);
        }};
    }
    assert_family_member!(carbon_monoxide_concentration_measurement, 0x040C);
    assert_family_member!(carbon_dioxide_concentration_measurement, 0x040D);
    assert_family_member!(nitrogen_dioxide_concentration_measurement, 0x0413);
    assert_family_member!(ozone_concentration_measurement, 0x0415);
    assert_family_member!(pm25_concentration_measurement, 0x042A);
    assert_family_member!(formaldehyde_concentration_measurement, 0x042B);
    assert_family_member!(pm1_concentration_measurement, 0x042C);
    assert_family_member!(pm10_concentration_measurement, 0x042D);
    assert_family_member!(
        total_volatile_organic_compounds_concentration_measurement,
        0x042E
    );
    assert_family_member!(radon_concentration_measurement, 0x042F);
}

// ---- Bridge support: BDBI (0x0039) + Switch events (Phase 0) ---------------
// BridgedDeviceBasicInformation reuses string/bool wire shapes already
// byte-parity-proven via BasicInformation (the two clusters share attribute
// ids per the Matter spec), so synthetic decode is the gate, per the A2.x
// convention above. Switch event payloads are hand-built TLV structures —
// the wire shape of EventDataIB's Data field.

/// Encode a single anonymous-tagged UTF-8 string (the wire shape of a
/// string attribute value).
fn str_attr(v: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    TlvWriter::new(&mut buf)
        .put_utf8(Tag::Anonymous, v)
        .unwrap();
    buf
}

#[test]
fn bridged_device_basic_information_ids_pinned() {
    use clusters::bridged_device_basic_information as bdbi;
    assert_eq!(bdbi::CLUSTER_ID, 0x0039);
    // Same attribute ids as BasicInformation (0x0028), per the Matter spec.
    assert_eq!(bdbi::attribute_id::NODE_LABEL, 0x0005);
    assert_eq!(bdbi::attribute_id::REACHABLE, 0x0011);
    assert_eq!(bdbi::attribute_id::UNIQUE_ID, 0x0012);
}

#[test]
fn bridged_device_basic_information_decodes() {
    use clusters::bridged_device_basic_information as bdbi;
    // NodeLabel / UniqueId: strings.
    assert_eq!(
        bdbi::decode_node_label(&str_attr("Kitchen sensor")).unwrap(),
        "Kitchen sensor"
    );
    assert_eq!(
        bdbi::decode_unique_id(&str_attr("00112233AABB")).unwrap(),
        "00112233AABB"
    );
    // Reachable: bool.
    assert!(bdbi::decode_reachable(&bool_attr(true)).unwrap());
    assert!(!bdbi::decode_reachable(&bool_attr(false)).unwrap());
    // A type mismatch is an error, never a default.
    assert!(bdbi::decode_node_label(&bool_attr(true)).is_err());
    assert!(bdbi::decode_reachable(&str_attr("x")).is_err());
}

#[test]
fn switch_event_ids_pinned() {
    use clusters::switch::event_id as ev;
    assert_eq!(ev::SWITCH_LATCHED, 0x00);
    assert_eq!(ev::INITIAL_PRESS, 0x01);
    assert_eq!(ev::LONG_PRESS, 0x02);
    assert_eq!(ev::SHORT_RELEASE, 0x03);
    assert_eq!(ev::LONG_RELEASE, 0x04);
    assert_eq!(ev::MULTI_PRESS_ONGOING, 0x05);
    assert_eq!(ev::MULTI_PRESS_COMPLETE, 0x06);
}

#[test]
fn switch_multi_press_complete_event_round_trips() {
    // Hand-built MultiPressComplete payload: an anonymous structure with
    // PreviousPosition (ctx tag 0) and TotalNumberOfPressesCounted (ctx tag 1).
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_uint(Tag::Context(1), 2).unwrap();
        w.end_container().unwrap();
    }
    let ev = clusters::switch::MultiPressCompleteEvent::decode(&buf).unwrap();
    assert_eq!(ev.previous_position, 1);
    assert_eq!(ev.total_number_of_presses_counted, 2);
}

#[test]
fn switch_multi_press_complete_event_missing_field_errors() {
    // TotalNumberOfPressesCounted (ctx tag 1) is mandatory — a payload
    // without it must be a decode error, not a silent zero.
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.end_container().unwrap();
    }
    assert!(clusters::switch::MultiPressCompleteEvent::decode(&buf).is_err());
}

// ---- M9-A3 B1: fabric-sensitive fields withheld for other fabrics -----------
//
// Our reads are unfiltered (IsFabricFiltered=false). chip then returns every
// fabric's entries of a fabric-scoped list and, for another fabric's entry,
// encodes ONLY FabricIndex: the sensitive fields are written only when
// `includeSensitive` (connectedhomeip
// zzz_generated/app-common/clusters/AccessControl/Structs.ipp:225-251,
// IcdManagement/Structs.ipp:45-60). matter.js cannot produce this shape, so
// the bytes are hand-built to match chip.

/// A list attribute's wire value: an anonymous array of anonymous structs,
/// each written by one closure.
fn list_of(entries: &[&dyn Fn(&mut TlvWriter<'_>)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_array(Tag::Anonymous).unwrap();
        for write_entry in entries {
            w.start_structure(Tag::Anonymous).unwrap();
            write_entry(&mut w);
            w.end_container().unwrap();
        }
        w.end_container().unwrap();
    }
    buf
}

/// One anonymous struct written by `write_fields` (a single entry's bytes).
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

/// Another fabric's entry as chip sends it: `FabricIndex` only.
fn other_fabric_entry(w: &mut TlvWriter<'_>) {
    w.put_uint(Tag::Context(254), 2).unwrap();
}

/// Our fabric's ACL entry: Administer / CASE / [node 0x1122], Targets null.
fn own_acl_entry(w: &mut TlvWriter<'_>) {
    w.put_uint(Tag::Context(1), 5).unwrap();
    w.put_uint(Tag::Context(2), 2).unwrap();
    w.start_array(Tag::Context(3)).unwrap();
    w.put_uint(Tag::Anonymous, 0x1122).unwrap();
    w.end_container().unwrap();
    w.put_null(Tag::Context(4)).unwrap();
    w.put_uint(Tag::Context(254), 1).unwrap();
}

#[test]
fn acl_list_with_another_fabrics_entry_decodes() {
    // The regression: before fabric-sensitive fields became Option, one
    // other-fabric entry failed the WHOLE list with MissingField("Privilege").
    let acl =
        clusters::access_control::decode_acl(&list_of(&[&own_acl_entry, &other_fabric_entry]))
            .expect("an unfiltered ACL read with a second fabric must decode");
    assert_eq!(acl.len(), 2);
}

#[test]
fn acl_other_fabric_entry_has_every_sensitive_field_none() {
    use clusters::access_control::{
        AccessControlEntryAuthModeEnum, AccessControlEntryPrivilegeEnum,
    };
    let acl =
        clusters::access_control::decode_acl(&list_of(&[&own_acl_entry, &other_fabric_entry]))
            .unwrap();
    let (own, other) = (&acl[0], &acl[1]);
    assert_eq!(
        own.privilege,
        Some(AccessControlEntryPrivilegeEnum::from_raw(5))
    );
    assert_eq!(
        own.auth_mode,
        Some(AccessControlEntryAuthModeEnum::from_raw(2))
    );
    assert_eq!(own.subjects, Some(Nullable::Value(vec![0x1122])));
    assert_eq!(own.targets, Some(Nullable::Null));
    assert_eq!(own.fabric_index, 1);
    assert_eq!(other.privilege, None);
    assert_eq!(other.auth_mode, None);
    assert_eq!(other.subjects, None);
    assert_eq!(other.targets, None);
    assert_eq!(other.fabric_index, 2);
}

#[test]
fn acl_entry_with_some_sensitive_fields_withheld_decodes_field_by_field() {
    // No 1.4 server does this, but nothing in the encoding forbids it: each
    // sensitive field is independently present or absent.
    let partial = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(1), 3).unwrap(); // Privilege: Operate
        w.put_null(Tag::Context(4)).unwrap(); // Targets: null (present)
        w.put_uint(Tag::Context(254), 2).unwrap();
    };
    let acl = clusters::access_control::decode_acl(&list_of(&[&partial])).unwrap();
    assert_eq!(
        acl[0].privilege,
        Some(clusters::access_control::AccessControlEntryPrivilegeEnum::from_raw(3))
    );
    assert_eq!(acl[0].auth_mode, None);
    assert_eq!(acl[0].subjects, None);
    // Present-but-null is distinct from withheld.
    assert_eq!(acl[0].targets, Some(Nullable::Null));
}

#[test]
fn extension_other_fabric_entry_decodes_and_refuses_reencode() {
    let own = |w: &mut TlvWriter<'_>| {
        w.put_bytes(Tag::Context(1), &[0x17, 0x18]).unwrap();
        w.put_uint(Tag::Context(254), 1).unwrap();
    };
    let ext =
        clusters::access_control::decode_extension(&list_of(&[&own, &other_fabric_entry])).unwrap();
    assert_eq!(ext[0].data, Some(vec![0x17, 0x18]));
    assert_eq!(ext[1].data, None);
    assert_eq!(ext[1].fabric_index, 2);
    // Read-modify-write of the unfiltered list must not re-home the other
    // fabric's entry onto ours with its data missing.
    assert!(matches!(
        ext[1].encode(),
        Err(matter_clusters::error::ClusterError::MissingField("Data"))
    ));
    // Our own entry still encodes, in its write form: what was read, less the
    // FabricIndex, which the server sets to the writing fabric.
    assert_eq!(
        ext[0].encode().unwrap(),
        struct_of(&|w| w.put_bytes(Tag::Context(1), &[0x17, 0x18]).unwrap())
    );
}

#[test]
fn arl_other_fabric_entry_has_every_sensitive_field_none() {
    let own = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_uint(Tag::Context(1), 0x0006).unwrap();
        w.start_array(Tag::Context(2)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(254), 1).unwrap();
    };
    let arl = clusters::access_control::decode_arl(&list_of(&[&own, &other_fabric_entry])).unwrap();
    assert_eq!(arl[0].endpoint, Some(1));
    assert_eq!(arl[0].cluster, Some(0x0006));
    assert_eq!(arl[0].restrictions.as_ref().map(Vec::len), Some(1));
    assert_eq!(arl[1].endpoint, None);
    assert_eq!(arl[1].cluster, None);
    assert_eq!(arl[1].restrictions, None);
    assert_eq!(arl[1].fabric_index, 2);
}

/// Our fabric's ICD registration: `CheckInNodeId` 0x1122, `MonitoredSubject`
/// 0x3344, `ClientType` 1 (Ephemeral).
fn own_icd_entry(w: &mut TlvWriter<'_>) {
    w.put_uint(Tag::Context(1), 0x1122).unwrap();
    w.put_uint(Tag::Context(2), 0x3344).unwrap();
    w.put_uint(Tag::Context(4), 1).unwrap();
    w.put_uint(Tag::Context(254), 1).unwrap();
}

#[test]
fn icd_registered_clients_other_fabric_entry_has_every_sensitive_field_none() {
    let clients = clusters::icd_management::decode_registered_clients(&list_of(&[
        &own_icd_entry,
        &other_fabric_entry,
    ]))
    .expect("an unfiltered RegisteredClients read with a second fabric must decode");
    assert_eq!(clients[0].check_in_node_id, Some(0x1122));
    assert_eq!(clients[0].monitored_subject, Some(0x3344));
    assert_eq!(
        clients[0].client_type,
        Some(clusters::icd_management::ClientTypeEnum::from_raw(1))
    );
    assert_eq!(clients[1].check_in_node_id, None);
    assert_eq!(clients[1].monitored_subject, None);
    assert_eq!(clients[1].client_type, None);
    assert_eq!(clients[1].fabric_index, 2);
}

// ---- M9-A3 B1: events, scalar-field shapes ----------------------------------
//
// Each event payload is an anonymous structure of context-tagged fields (the
// same wire shape as a command response), decoded by the generated
// `<Name>Event::decode`. Field tags and types follow the 1.4.2 event tables.

#[test]
fn basic_information_event_ids_pinned() {
    use clusters::basic_information::event_id as ev;
    assert_eq!(ev::START_UP, 0x00);
    assert_eq!(ev::SHUT_DOWN, 0x01);
    assert_eq!(ev::LEAVE, 0x02);
    assert_eq!(ev::REACHABLE_CHANGED, 0x03);
}

#[test]
fn basic_information_events_decode() {
    use clusters::basic_information::{LeaveEvent, ReachableChangedEvent, StartUpEvent};
    let e = StartUpEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0x0102_0304).unwrap();
    }))
    .unwrap();
    assert_eq!(e.software_version, 0x0102_0304);
    let e = LeaveEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 3).unwrap();
    }))
    .unwrap();
    assert_eq!(e.fabric_index, 3);
    let e = ReachableChangedEvent::decode(&struct_of(&|w| {
        w.put_bool(Tag::Context(0), false).unwrap();
    }))
    .unwrap();
    assert!(!e.reachable_new_value);
    // A mandatory field missing is an error, never a default.
    assert!(StartUpEvent::decode(&struct_of(&|_| {})).is_err());
}

#[test]
fn event_payload_with_an_unknown_future_field_still_decodes() {
    // A newer-revision device may add fields; the decoder skips unknown tags.
    let e = clusters::basic_information::StartUpEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 5).unwrap();
        w.put_utf8(Tag::Context(9), "future").unwrap();
        w.start_structure(Tag::Context(10)).unwrap();
        w.put_bool(Tag::Context(0), true).unwrap();
        w.end_container().unwrap();
    }))
    .unwrap();
    assert_eq!(e.software_version, 5);
}

#[test]
fn boolean_state_state_change_event_decodes() {
    use clusters::boolean_state::{event_id, StateChangeEvent};
    assert_eq!(event_id::STATE_CHANGE, 0x00);
    let e = StateChangeEvent::decode(&struct_of(&|w| {
        w.put_bool(Tag::Context(0), true).unwrap();
    }))
    .unwrap();
    assert!(e.state_value);
}

#[test]
fn occupancy_changed_event_decodes() {
    use clusters::occupancy_sensing::{event_id, OccupancyBitmap, OccupancyChangedEvent};
    assert_eq!(event_id::OCCUPANCY_CHANGED, 0x00);
    let e = OccupancyChangedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(e.occupancy, OccupancyBitmap::from_bits_retain(1));
}

#[test]
fn pump_configuration_event_ids_pinned() {
    // All 17 pump events are fieldless: only their ids are generated.
    use clusters::pump_configuration_and_control::event_id as ev;
    assert_eq!(ev::SUPPLY_VOLTAGE_LOW, 0x00);
    assert_eq!(ev::DRY_RUNNING, 0x05);
    assert_eq!(ev::PUMP_BLOCKED, 0x09);
    assert_eq!(ev::TURBINE_OPERATION, 0x10);
}

#[test]
fn time_synchronization_events_decode() {
    use clusters::time_synchronization::{event_id as ev, DstStatusEvent, TimeZoneStatusEvent};
    assert_eq!(ev::DST_TABLE_EMPTY, 0x00);
    assert_eq!(ev::DST_STATUS, 0x01);
    assert_eq!(ev::TIME_ZONE_STATUS, 0x02);
    assert_eq!(ev::TIME_FAILURE, 0x03);
    assert_eq!(ev::MISSING_TRUSTED_TIME_SOURCE, 0x04);
    let e = DstStatusEvent::decode(&struct_of(&|w| {
        w.put_bool(Tag::Context(0), true).unwrap();
    }))
    .unwrap();
    assert!(e.dst_offset_active);
    // Offset is int32 (negative west of UTC); Name is optional.
    let e = TimeZoneStatusEvent::decode(&struct_of(&|w| {
        w.put_int(Tag::Context(0), -18_000).unwrap();
    }))
    .unwrap();
    assert_eq!(e.offset, -18_000);
    assert_eq!(e.name, None);
    let e = TimeZoneStatusEvent::decode(&struct_of(&|w| {
        w.put_int(Tag::Context(0), 3600).unwrap();
        w.put_utf8(Tag::Context(1), "Europe/Paris").unwrap();
    }))
    .unwrap();
    assert_eq!(e.name.as_deref(), Some("Europe/Paris"));
}

#[test]
fn ota_requestor_events_decode() {
    use clusters::ota_software_update_requestor::{
        event_id as ev, ChangeReasonEnum, DownloadErrorEvent, StateTransitionEvent,
        UpdateStateEnum, VersionAppliedEvent,
    };
    assert_eq!(ev::STATE_TRANSITION, 0x00);
    assert_eq!(ev::VERSION_APPLIED, 0x01);
    assert_eq!(ev::DOWNLOAD_ERROR, 0x02);
    let e = StateTransitionEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap(); // Idle
        w.put_uint(Tag::Context(1), 4).unwrap(); // Downloading
        w.put_uint(Tag::Context(2), 1).unwrap(); // Success
        w.put_null(Tag::Context(3)).unwrap();
    }))
    .unwrap();
    assert_eq!(e.previous_state, UpdateStateEnum::from_raw(1));
    assert_eq!(e.new_state, UpdateStateEnum::from_raw(4));
    assert_eq!(e.reason, ChangeReasonEnum::from_raw(1));
    assert_eq!(e.target_software_version, Nullable::Null);
    let e = VersionAppliedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 2).unwrap();
        w.put_uint(Tag::Context(1), 0x8000).unwrap();
    }))
    .unwrap();
    assert_eq!((e.software_version, e.product_id), (2, 0x8000));
    let e = DownloadErrorEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 2).unwrap();
        w.put_uint(Tag::Context(1), 1_048_576).unwrap();
        w.put_uint(Tag::Context(2), 42).unwrap();
        w.put_int(Tag::Context(3), -7).unwrap();
    }))
    .unwrap();
    assert_eq!(e.bytes_downloaded, 1_048_576);
    assert_eq!(e.progress_percent, Nullable::Value(42));
    assert_eq!(e.platform_code, Nullable::Value(-7));
}

// ---- M9-A3 B1: events, list-of-enum shapes ----------------------------------

#[test]
fn general_diagnostics_events_decode() {
    use clusters::general_diagnostics::{
        event_id as ev, BootReasonEnum, BootReasonEvent, HardwareFaultChangeEvent,
        HardwareFaultEnum, NetworkFaultChangeEvent, NetworkFaultEnum, RadioFaultChangeEvent,
        RadioFaultEnum,
    };
    assert_eq!(ev::HARDWARE_FAULT_CHANGE, 0x00);
    assert_eq!(ev::RADIO_FAULT_CHANGE, 0x01);
    assert_eq!(ev::NETWORK_FAULT_CHANGE, 0x02);
    assert_eq!(ev::BOOT_REASON, 0x03);
    // HardwareFault and NetworkFault use the lists chip's all-clusters
    // fault injection sends (`OnGeneralFaultEventHandler`): hardware
    // previous [Radio, PowerSource] → current [Radio, Sensor, PowerSource,
    // UserInterfaceFault] = [1, 5] → [1, 2, 5, 8]. RadioFault adds an empty list.
    let faults = |current: &[u64], previous: &[u64]| {
        let (current, previous) = (current.to_vec(), previous.to_vec());
        struct_of(&move |w| {
            w.start_array(Tag::Context(0)).unwrap();
            for v in &current {
                w.put_uint(Tag::Anonymous, *v).unwrap();
            }
            w.end_container().unwrap();
            w.start_array(Tag::Context(1)).unwrap();
            for v in &previous {
                w.put_uint(Tag::Anonymous, *v).unwrap();
            }
            w.end_container().unwrap();
        })
    };
    let e = HardwareFaultChangeEvent::decode(&faults(&[1, 2, 5, 8], &[1, 5])).unwrap();
    assert_eq!(
        e.current,
        [1, 2, 5, 8].map(HardwareFaultEnum::from_raw).to_vec()
    );
    assert_eq!(e.previous, [1, 5].map(HardwareFaultEnum::from_raw).to_vec());
    let e = RadioFaultChangeEvent::decode(&faults(&[1, 3], &[])).unwrap();
    assert_eq!(e.current, [1, 3].map(RadioFaultEnum::from_raw).to_vec());
    assert_eq!(e.previous, []);
    let e = NetworkFaultChangeEvent::decode(&faults(&[1, 2, 3], &[1, 2])).unwrap();
    assert_eq!(
        e.current,
        [1, 2, 3].map(NetworkFaultEnum::from_raw).to_vec()
    );
    assert_eq!(e.previous, [1, 2].map(NetworkFaultEnum::from_raw).to_vec());
    let e = BootReasonEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap(); // PowerOnReboot
    }))
    .unwrap();
    assert_eq!(e.boot_reason, BootReasonEnum::from_raw(1));
}

#[test]
fn power_source_fault_events_decode() {
    use clusters::power_source::{
        event_id as ev, BatChargeFaultChangeEvent, BatChargeFaultEnum, BatFaultChangeEvent,
        BatFaultEnum, WiredFaultChangeEvent, WiredFaultEnum,
    };
    assert_eq!(ev::WIRED_FAULT_CHANGE, 0x00);
    assert_eq!(ev::BAT_FAULT_CHANGE, 0x01);
    assert_eq!(ev::BAT_CHARGE_FAULT_CHANGE, 0x02);
    let pair = |current: u64, previous: Option<u64>| {
        struct_of(&move |w| {
            w.start_array(Tag::Context(0)).unwrap();
            w.put_uint(Tag::Anonymous, current).unwrap();
            w.end_container().unwrap();
            w.start_array(Tag::Context(1)).unwrap();
            if let Some(p) = previous {
                w.put_uint(Tag::Anonymous, p).unwrap();
            }
            w.end_container().unwrap();
        })
    };
    let e = WiredFaultChangeEvent::decode(&pair(1, None)).unwrap();
    assert_eq!(e.current, vec![WiredFaultEnum::from_raw(1)]);
    assert_eq!(e.previous, []);
    let e = BatFaultChangeEvent::decode(&pair(2, Some(1))).unwrap();
    assert_eq!(e.current, vec![BatFaultEnum::from_raw(2)]);
    assert_eq!(e.previous, vec![BatFaultEnum::from_raw(1)]);
    let e = BatChargeFaultChangeEvent::decode(&pair(3, None)).unwrap();
    assert_eq!(e.current, vec![BatChargeFaultEnum::from_raw(3)]);
}

#[test]
fn list_of_enum_event_missing_list_is_an_error() {
    use matter_clusters::error::ClusterError;
    // Both lists are mandatory in 1.4.2: a missing list is MissingField,
    // never an empty-list default (an empty list is a different statement).
    let only_current = struct_of(&|w| {
        w.start_array(Tag::Context(0)).unwrap();
        w.put_uint(Tag::Anonymous, 1).unwrap();
        w.end_container().unwrap();
    });
    assert!(matches!(
        clusters::general_diagnostics::HardwareFaultChangeEvent::decode(&only_current),
        Err(ClusterError::MissingField("Previous"))
    ));
    assert!(matches!(
        clusters::power_source::BatFaultChangeEvent::decode(&only_current),
        Err(ClusterError::MissingField("Previous"))
    ));
    assert!(matches!(
        clusters::general_diagnostics::BootReasonEvent::decode(&struct_of(&|_| {})),
        Err(ClusterError::MissingField("BootReason"))
    ));
}

// ---- M9-A3 B1: events, composite-field shapes -------------------------------

#[test]
fn access_control_event_ids_pinned() {
    use clusters::access_control::event_id as ev;
    assert_eq!(ev::ACCESS_CONTROL_ENTRY_CHANGED, 0x00);
    assert_eq!(ev::ACCESS_CONTROL_EXTENSION_CHANGED, 0x01);
    assert_eq!(ev::FABRIC_RESTRICTION_REVIEW_UPDATE, 0x02);
}

#[test]
fn access_control_entry_changed_event_decodes_with_unwrapped_fields() {
    use clusters::access_control::{
        AccessControlEntryChangedEvent, AccessControlEntryPrivilegeEnum, ChangeTypeEnum,
    };
    // chip (access-control-cluster.cpp OnEntryChanged): a CASE admin added an
    // entry — AdminNodeID set, AdminPasscodeID null, LatestValue the full
    // entry (our own fabric's event: chip encodes it with includeSensitive).
    let bytes = struct_of(&|w| {
        w.put_uint(Tag::Context(1), 0x0000_0000_0001_B669).unwrap();
        w.put_null(Tag::Context(2)).unwrap();
        w.put_uint(Tag::Context(3), 1).unwrap(); // Added
        w.start_structure(Tag::Context(4)).unwrap();
        own_acl_entry(w);
        w.end_container().unwrap();
        w.put_uint(Tag::Context(254), 1).unwrap();
    });
    let e = AccessControlEntryChangedEvent::decode(&bytes).unwrap();
    // Events are exempt from the fabric-sensitive Option rule: the event's own
    // fields keep the model's optionality. These bindings fail to compile if a
    // field became Option.
    let admin: Nullable<u64> = e.admin_node_id;
    let passcode: Nullable<u16> = e.admin_passcode_id;
    let change: ChangeTypeEnum = e.change_type;
    let fabric: u8 = e.fabric_index;
    assert_eq!(admin, Nullable::Value(0x1_B669));
    assert_eq!(passcode, Nullable::Null);
    assert_eq!(change, ChangeTypeEnum::from_raw(1));
    assert_eq!(fabric, 1);
    // LatestValue is the shared datatype struct, so its sensitive fields are
    // Option — and present, because chip sends our own fabric's event in full.
    match e.latest_value {
        Nullable::Value(entry) => {
            assert_eq!(
                entry.privilege,
                Some(AccessControlEntryPrivilegeEnum::from_raw(5))
            );
            assert_eq!(entry.subjects, Some(Nullable::Value(vec![0x1122])));
        }
        Nullable::Null => panic!("LatestValue should be present"),
    }
}

#[test]
fn access_control_extension_changed_and_review_events_decode() {
    use clusters::access_control::{
        AccessControlExtensionChangedEvent, ChangeTypeEnum, FabricRestrictionReviewUpdateEvent,
    };
    let e = AccessControlExtensionChangedEvent::decode(&struct_of(&|w| {
        w.put_null(Tag::Context(1)).unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap(); // a PASE admin
        w.put_uint(Tag::Context(3), 2).unwrap(); // Removed
        w.put_null(Tag::Context(4)).unwrap();
        w.put_uint(Tag::Context(254), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(e.admin_node_id, Nullable::Null);
    assert_eq!(e.admin_passcode_id, Nullable::Value(0));
    assert_eq!(e.change_type, ChangeTypeEnum::from_raw(2));
    assert_eq!(e.latest_value, Nullable::Null);
    let e = FabricRestrictionReviewUpdateEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 77).unwrap();
        w.put_uint(Tag::Context(254), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(e.token, 77);
    assert_eq!(e.instruction, None);
    assert_eq!(e.arl_request_flow_url, None);
}

#[test]
fn electrical_energy_measured_events_decode() {
    use clusters::electrical_energy_measurement::{
        event_id as ev, CumulativeEnergyMeasuredEvent, PeriodicEnergyMeasuredEvent,
    };
    assert_eq!(ev::CUMULATIVE_ENERGY_MEASURED, 0x00);
    assert_eq!(ev::PERIODIC_ENERGY_MEASURED, 0x01);
    // An importing load (chip's FakeReadings 1 kW trigger): EnergyImported
    // only; EnergyExported absent (both are optional, feature-gated).
    let e = CumulativeEnergyMeasuredEvent::decode(&struct_of(&|w| {
        w.start_structure(Tag::Context(0)).unwrap();
        w.put_int(Tag::Context(0), 555).unwrap(); // Energy (mWh)
        w.put_uint(Tag::Context(1), 1_000).unwrap(); // StartTimestamp
        w.put_uint(Tag::Context(2), 1_002).unwrap(); // EndTimestamp
        w.end_container().unwrap();
    }))
    .unwrap();
    let imported = e.energy_imported.expect("EnergyImported present");
    assert_eq!(imported.energy, 555);
    assert_eq!(imported.start_timestamp, Some(1_000));
    assert_eq!(e.energy_exported, None);
    let e = PeriodicEnergyMeasuredEvent::decode(&struct_of(&|w| {
        w.start_structure(Tag::Context(1)).unwrap();
        w.put_int(Tag::Context(0), 2_500).unwrap();
        w.put_uint(Tag::Context(3), 9_000).unwrap(); // StartSystime
        w.end_container().unwrap();
    }))
    .unwrap();
    assert_eq!(e.energy_imported, None);
    assert_eq!(
        e.energy_exported.map(|x| x.start_systime),
        Some(Some(9_000))
    );
}

#[test]
fn electrical_power_measurement_period_ranges_event_decodes() {
    use clusters::electrical_power_measurement::{
        event_id, MeasurementPeriodRangesEvent, MeasurementTypeEnum,
    };
    assert_eq!(event_id::MEASUREMENT_PERIOD_RANGES, 0x00);
    let e = MeasurementPeriodRangesEvent::decode(&struct_of(&|w| {
        w.start_array(Tag::Context(0)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 1).unwrap(); // Voltage
        w.put_int(Tag::Context(1), 229_000).unwrap();
        w.put_int(Tag::Context(2), 231_000).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
    }))
    .unwrap();
    assert_eq!(e.ranges.len(), 1);
    assert_eq!(
        e.ranges[0].measurement_type,
        MeasurementTypeEnum::from_raw(1)
    );
    assert_eq!((e.ranges[0].min, e.ranges[0].max), (229_000, 231_000));
}

#[test]
fn door_lock_event_ids_pinned() {
    use clusters::door_lock::event_id as ev;
    assert_eq!(ev::DOOR_LOCK_ALARM, 0x00);
    assert_eq!(ev::DOOR_STATE_CHANGE, 0x01);
    assert_eq!(ev::LOCK_OPERATION, 0x02);
    assert_eq!(ev::LOCK_OPERATION_ERROR, 0x03);
    assert_eq!(ev::LOCK_USER_CHANGE, 0x04);
}

#[test]
fn door_lock_events_decode() {
    use clusters::door_lock::{
        AlarmCodeEnum, DataOperationTypeEnum, DoorLockAlarmEvent, DoorStateChangeEvent,
        DoorStateEnum, LockDataTypeEnum, LockOperationErrorEvent, LockOperationEvent,
        LockOperationTypeEnum, LockUserChangeEvent, OperationErrorEnum, OperationSourceEnum,
    };
    let e = DoorLockAlarmEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap(); // LockJammed
    }))
    .unwrap();
    assert_eq!(e.alarm_code, AlarmCodeEnum::from_raw(0));
    let e = DoorStateChangeEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap(); // DoorClosed
    }))
    .unwrap();
    assert_eq!(e.door_state, DoorStateEnum::from_raw(1));
    // A remote unlock with no PIN (lock-app): UserIndex null, Credentials null.
    let e = LockOperationEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap(); // Unlock
        w.put_uint(Tag::Context(1), 7).unwrap(); // Remote
        w.put_null(Tag::Context(2)).unwrap();
        w.put_uint(Tag::Context(3), 1).unwrap();
        w.put_uint(Tag::Context(4), 0x1_B669).unwrap();
        w.put_null(Tag::Context(5)).unwrap();
    }))
    .unwrap();
    assert_eq!(e.lock_operation_type, LockOperationTypeEnum::from_raw(1));
    assert_eq!(e.operation_source, OperationSourceEnum::from_raw(7));
    assert_eq!(e.user_index, Nullable::Null);
    assert_eq!(e.fabric_index, Nullable::Value(1));
    assert_eq!(e.source_node, Nullable::Value(0x1_B669));
    assert_eq!(e.credentials, Some(Nullable::Null));
    // A failed operation with a credential list; Credentials may also be absent.
    let e = LockOperationErrorEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_uint(Tag::Context(1), 7).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap(); // InvalidCredential
        w.put_uint(Tag::Context(3), 2).unwrap();
        w.put_null(Tag::Context(4)).unwrap();
        w.put_null(Tag::Context(5)).unwrap();
        w.start_array(Tag::Context(6)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 1).unwrap(); // Pin
        w.put_uint(Tag::Context(1), 3).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
    }))
    .unwrap();
    assert_eq!(e.operation_error, OperationErrorEnum::from_raw(1));
    assert_eq!(e.user_index, Nullable::Value(2));
    match e.credentials {
        Some(Nullable::Value(creds)) => assert_eq!(creds[0].credential_index, 3),
        other => panic!("expected one credential, got {other:?}"),
    }
    let e = LockUserChangeEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 2).unwrap(); // UserIndex
        w.put_uint(Tag::Context(1), 0).unwrap(); // Add
        w.put_uint(Tag::Context(2), 7).unwrap(); // Remote
        w.put_uint(Tag::Context(3), 2).unwrap();
        w.put_uint(Tag::Context(4), 1).unwrap();
        w.put_uint(Tag::Context(5), 0x1_B669).unwrap();
        w.put_uint(Tag::Context(6), 2).unwrap();
    }))
    .unwrap();
    assert_eq!(e.lock_data_type, LockDataTypeEnum::from_raw(2));
    assert_eq!(e.data_operation_type, DataOperationTypeEnum::from_raw(0));
    assert_eq!(e.data_index, Nullable::Value(2));
}

// ---- M9-A3 B1: events, derived cluster (BridgedDeviceBasicInformation) ------

#[test]
fn bridged_device_basic_information_event_ids_pinned() {
    use clusters::bridged_device_basic_information::event_id as ev;
    assert_eq!(ev::START_UP, 0x00);
    assert_eq!(ev::SHUT_DOWN, 0x01);
    assert_eq!(ev::LEAVE, 0x02);
    assert_eq!(ev::REACHABLE_CHANGED, 0x03);
    assert_eq!(ev::ACTIVE_CHANGED, 0x80);
}

#[test]
fn bridged_device_basic_information_events_decode() {
    // StartUp and ReachableChanged inherit their fields from
    // BasicInformation: the derived model elements have no children of their
    // own, so the dump must read the resolved `members`.
    use clusters::bridged_device_basic_information::{
        ActiveChangedEvent, ReachableChangedEvent, StartUpEvent,
    };
    let e = StartUpEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 9).unwrap();
    }))
    .unwrap();
    assert_eq!(e.software_version, 9);
    let e = ReachableChangedEvent::decode(&struct_of(&|w| {
        w.put_bool(Tag::Context(0), true).unwrap();
    }))
    .unwrap();
    assert!(e.reachable_new_value);
    // Mandatory in 1.4.2 (BridgedDeviceBasicInformationCluster.xml event 0x03).
    assert!(matches!(
        ReachableChangedEvent::decode(&struct_of(&|_| {})),
        Err(matter_clusters::error::ClusterError::MissingField(
            "ReachableNewValue"
        ))
    ));
    assert!(matches!(
        StartUpEvent::decode(&struct_of(&|_| {})),
        Err(matter_clusters::error::ClusterError::MissingField(
            "SoftwareVersion"
        ))
    ));
    let e = ActiveChangedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 30_000).unwrap();
    }))
    .unwrap();
    assert_eq!(e.promised_active_duration, 30_000);
}

// ---- M9-A3 B2: ModeBase-derived clusters ------------------------------------
//
// Every ModeBase derivative has one wire shape (1.4.2 ModeBase.xml):
// SupportedModes is a list of ModeOptionStruct { Label(0) string, Mode(1)
// uint8, ModeTags(2) list<ModeTagStruct { MfgCode(0) vendor-id, optional;
// Value(1) enum16 }> }; ChangeToMode is { NewMode(0) uint8 } and its response
// { Status(0) enum8, StatusText(1) string }. The field set is inherited from
// the id-less ModeBase: before the B2 `members` fix ModeOptionStruct came out
// with no fields at all.

/// `SupportedModes` as chip encodes it (`ModeBase/Structs.ipp` writes `MfgCode`
/// only when present): mode 0 "Normal" tagged [`Auto` (0x0000), the cluster's
/// first derived tag]; mode 7 "Vendor" tagged [mfg 0xFFF1 / 0x8001, a
/// manufacturer-specific value no codegen knows].
fn supported_modes(derived_tag: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_array(Tag::Anonymous).unwrap();
        for (label, mode, tags) in [
            ("Normal", 0u64, vec![(None, 0x0000u64), (None, derived_tag)]),
            ("Vendor", 7, vec![(Some(0xFFF1u64), 0x8001)]),
        ] {
            w.start_structure(Tag::Anonymous).unwrap();
            w.put_utf8(Tag::Context(0), label).unwrap();
            w.put_uint(Tag::Context(1), mode).unwrap();
            w.start_array(Tag::Context(2)).unwrap();
            for (mfg, value) in tags {
                w.start_structure(Tag::Anonymous).unwrap();
                if let Some(mfg) = mfg {
                    w.put_uint(Tag::Context(0), mfg).unwrap();
                }
                w.put_uint(Tag::Context(1), value).unwrap();
                w.end_container().unwrap();
            }
            w.end_container().unwrap();
            w.end_container().unwrap();
        }
        w.end_container().unwrap();
    }
    buf
}

/// One test per `ModeBase` derivative: `SupportedModes` (its most complex
/// attribute), `CurrentMode`, the `ChangeToMode` request bytes, and the
/// response with and without `StatusText` (only an unconditional `M` is
/// mandatory, and chip's `ModeBase` server omits it from the replies it builds
/// itself; an app delegate may set it).
macro_rules! mode_base_cluster_decodes {
    ($test:ident, $m:ident, $first_derived_tag:literal, $derived_variant:ident) => {
        #[test]
        fn $test() {
            use clusters::$m::{ChangeToModeResponse, ModeChangeStatus, ModeTag};
            use matter_clusters::error::ClusterError;
            let modes =
                clusters::$m::decode_supported_modes(&supported_modes($first_derived_tag)).unwrap();
            assert_eq!(modes.len(), 2);
            assert_eq!((modes[0].label.as_str(), modes[0].mode), ("Normal", 0));
            let tags: Vec<_> = modes[0]
                .mode_tags
                .iter()
                .map(|t| (t.mfg_code, t.value))
                .collect();
            assert_eq!(
                tags,
                [(None, ModeTag::Auto), (None, ModeTag::$derived_variant)]
            );
            assert_eq!((modes[1].label.as_str(), modes[1].mode), ("Vendor", 7));
            assert_eq!(modes[1].mode_tags[0].mfg_code, Some(0xFFF1));
            assert_eq!(modes[1].mode_tags[0].value, ModeTag::Unknown(0x8001));
            assert_eq!(clusters::$m::decode_current_mode(&uint_attr(7)).unwrap(), 7);

            let new_mode_7 = struct_of(&|w| w.put_uint(Tag::Context(0), 7).unwrap());
            assert_eq!(clusters::$m::encode_change_to_mode(7), new_mode_7);

            // What chip sends for an unsupported mode: Status only.
            let r = ChangeToModeResponse::decode(&struct_of(&|w| {
                w.put_uint(Tag::Context(0), 1).unwrap();
            }))
            .unwrap();
            assert_eq!(
                (r.status, r.status_text),
                (ModeChangeStatus::UnsupportedMode, None)
            );
            let r = ChangeToModeResponse::decode(&struct_of(&|w| {
                w.put_uint(Tag::Context(0), 0).unwrap();
                w.put_utf8(Tag::Context(1), "done").unwrap();
            }))
            .unwrap();
            assert_eq!(r.status, ModeChangeStatus::Success);
            assert_eq!(r.status_text.as_deref(), Some("done"));
            // An empty StatusText (which the spec allows) is present, not
            // absent; a manufacturer status (0x80..=0xBF) is Unknown, not an error.
            let r = ChangeToModeResponse::decode(&struct_of(&|w| {
                w.put_uint(Tag::Context(0), 0x80).unwrap();
                w.put_utf8(Tag::Context(1), "").unwrap();
            }))
            .unwrap();
            assert_eq!(r.status, ModeChangeStatus::Unknown(0x80));
            assert_eq!(r.status_text.as_deref(), Some(""));
            // Status itself stays mandatory.
            assert!(matches!(
                ChangeToModeResponse::decode(&struct_of(&|_| {})),
                Err(ClusterError::MissingField("Status"))
            ));
        }
    };
}

mode_base_cluster_decodes!(oven_mode_decodes, oven_mode, 0x4000, Bake);
mode_base_cluster_decodes!(
    laundry_washer_mode_decodes,
    laundry_washer_mode,
    0x4000,
    Normal
);
mode_base_cluster_decodes!(
    refrigerator_and_tcc_mode_decodes,
    refrigerator_and_temperature_controlled_cabinet_mode,
    0x4000,
    RapidCool
);
mode_base_cluster_decodes!(rvc_run_mode_decodes, rvc_run_mode, 0x4000, Idle);
mode_base_cluster_decodes!(rvc_clean_mode_decodes, rvc_clean_mode, 0x4000, DeepClean);
mode_base_cluster_decodes!(dishwasher_mode_decodes, dishwasher_mode, 0x4000, Normal);

#[test]
fn rvc_mode_change_status_keeps_base_and_derived_values() {
    // The derived ModeChangeStatus adds values to the base's 0..=3; on
    // `.children` the base values were missing, so a plain `Success` decoded
    // as Unknown(0).
    use clusters::{rvc_clean_mode, rvc_run_mode};
    assert_eq!(
        rvc_run_mode::ModeChangeStatus::from_raw(0),
        rvc_run_mode::ModeChangeStatus::Success
    );
    assert_eq!(
        rvc_run_mode::ModeChangeStatus::from_raw(0x41),
        rvc_run_mode::ModeChangeStatus::Stuck
    );
    assert_eq!(
        rvc_run_mode::ModeChangeStatus::from_raw(0x48),
        rvc_run_mode::ModeChangeStatus::BatteryLow
    );
    assert_eq!(
        rvc_clean_mode::ModeChangeStatus::from_raw(0x40),
        rvc_clean_mode::ModeChangeStatus::CleaningInProgress
    );
    assert_eq!(
        rvc_clean_mode::ModeChangeStatus::from_raw(3),
        rvc_clean_mode::ModeChangeStatus::InvalidInMode
    );
}

#[test]
fn mode_option_missing_mode_tags_is_an_error() {
    use matter_clusters::error::ClusterError;
    // ModeTags is mandatory (an empty list is valid; an absent one is not).
    let bytes = list_of(&[&|w| {
        w.put_utf8(Tag::Context(0), "x").unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
    }]);
    assert!(matches!(
        clusters::dishwasher_mode::decode_supported_modes(&bytes),
        Err(ClusterError::MissingField("ModeTags"))
    ));
}

mode_base_cluster_decodes!(energy_evse_mode_decodes, energy_evse_mode, 0x4000, Manual);
mode_base_cluster_decodes!(water_heater_mode_decodes, water_heater_mode, 0x4000, Off);
mode_base_cluster_decodes!(
    device_energy_management_mode_decodes,
    device_energy_management_mode,
    0x4000,
    NoOptimization
);

#[test]
fn microwave_oven_mode_decodes_without_commands() {
    // MicrowaveOvenMode disallows ChangeToMode (1.4.2 MicrowaveOvenMode.xml:
    // <disallowConform/>), so only its attributes are generated; there is no
    // encode_change_to_mode and command_id is empty.
    use clusters::microwave_oven_mode::{decode_current_mode, decode_supported_modes, ModeTag};
    let modes = decode_supported_modes(&supported_modes(0x4001)).unwrap();
    assert_eq!(modes[0].mode_tags[1].value, ModeTag::Defrost);
    assert_eq!(modes[1].mode_tags[0].value, ModeTag::Unknown(0x8001));
    assert_eq!(decode_current_mode(&uint_attr(0)).unwrap(), 0);
}

// ---- M9-A3 B2: ModeSelect ----------------------------------------------------
//
// ModeSelect is not a ModeBase derivative: its ModeOptionStruct carries
// SemanticTags (a cluster-local SemanticTagStruct { MfgCode(0) vendor-id,
// Value(1) enum16 }, distinct from the global `semtag`), and ChangeToMode has
// no response command.

#[test]
fn mode_select_standard_namespace_is_enum16() {
    // 1.4.2 ModeSelect.xml declares StandardNamespace enum16 (nullable); the
    // model's `namespace` is enum8, so the dump widens it (meta.relaxed, W).
    // A namespace id above 0xFF must decode, not fail as out of range.
    use clusters::mode_select::decode_standard_namespace;
    assert_eq!(
        decode_standard_namespace(&uint_attr(0x0101)).unwrap(),
        Nullable::Value(0x0101_u16)
    );
    assert_eq!(
        decode_standard_namespace(&null_attr()).unwrap(),
        Nullable::Null
    );
}

#[test]
fn mode_select_supported_modes_decode_cluster_local_semantic_tags() {
    // The first entry is chip all-clusters' "Black" mode exactly
    // (static-supported-modes-manager.cpp: mode 0, one tag { MfgCode 0,
    // Value 0 }; chip always writes the non-optional MfgCode). The second is
    // synthetic: a manufacturer tag whose Value needs the full enum16.
    let tag = |w: &mut TlvWriter<'_>, mfg: u64, value: u64| {
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), mfg).unwrap();
        w.put_uint(Tag::Context(1), value).unwrap();
        w.end_container().unwrap();
    };
    let bytes = list_of(&[
        &|w| {
            w.put_utf8(Tag::Context(0), "Black").unwrap();
            w.put_uint(Tag::Context(1), 0).unwrap();
            w.start_array(Tag::Context(2)).unwrap();
            tag(w, 0, 0);
            w.end_container().unwrap();
        },
        &|w| {
            w.put_utf8(Tag::Context(0), "Vendor").unwrap();
            w.put_uint(Tag::Context(1), 9).unwrap();
            w.start_array(Tag::Context(2)).unwrap();
            tag(w, 0xFFF1, 0x0102);
            w.end_container().unwrap();
        },
    ]);
    let modes = clusters::mode_select::decode_supported_modes(&bytes).unwrap();
    assert_eq!((modes[0].label.as_str(), modes[0].mode), ("Black", 0));
    let tags: Vec<(u16, u16)> = modes
        .iter()
        .flat_map(|m| m.semantic_tags.iter().map(|t| (t.mfg_code, t.value)))
        .collect();
    assert_eq!(tags, [(0, 0), (0xFFF1, 0x0102)]);
}

#[test]
fn mode_select_semantic_tag_missing_mfg_code_is_an_error() {
    // MfgCode is mandatory and non-nullable since ModeSelect revision 2.
    use matter_clusters::error::ClusterError;
    let bytes = list_of(&[&|w| {
        w.put_utf8(Tag::Context(0), "x").unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
        w.start_array(Tag::Context(2)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(1), 1).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
    }]);
    assert!(matches!(
        clusters::mode_select::decode_supported_modes(&bytes),
        Err(ClusterError::MissingField("MfgCode"))
    ));
}

#[test]
fn mode_select_writable_modes_and_change_to_mode_encode() {
    use clusters::mode_select::{
        decode_description, decode_on_mode, decode_start_up_mode, encode_change_to_mode,
        encode_on_mode, encode_start_up_mode,
    };
    assert_eq!(decode_description(&str_attr("Coffee")).unwrap(), "Coffee");
    for v in [Nullable::Null, Nullable::Value(4)] {
        assert_eq!(decode_start_up_mode(&encode_start_up_mode(v)).unwrap(), v);
        assert_eq!(decode_on_mode(&encode_on_mode(v)).unwrap(), v);
    }
    assert_eq!(
        encode_change_to_mode(4),
        struct_of(&|w| w.put_uint(Tag::Context(0), 4).unwrap())
    );
}

// ---- M9-A3 B2: AlarmBase-derived clusters -------------------------------------
//
// Mask / Latch / State / Supported are map32 AlarmBitmap attributes; Notify
// carries four of them (1.4.2 AlarmBase.xml). RefrigeratorAlarm disallows the
// RESET feature, so its Latch and Reset are not generated (chip's controller
// codegen omits them too) and ModifyEnabledAlarms is disallowed outright.

#[test]
fn alarm_event_ids_pinned() {
    assert_eq!(clusters::dishwasher_alarm::event_id::NOTIFY, 0x00);
    assert_eq!(clusters::refrigerator_alarm::event_id::NOTIFY, 0x00);
}

#[test]
fn dishwasher_alarm_attributes_commands_and_notify_decode() {
    use clusters::dishwasher_alarm::{
        decode_latch, decode_mask, decode_state, decode_supported, encode_modify_enabled_alarms,
        encode_reset, AlarmBitmap, NotifyEvent,
    };
    // chip all-clusters' boot values (dishwasher-alarm-stub.cpp): Supported
    // and Mask 0x2F, Latch 0x03, State 0x07.
    assert_eq!(
        decode_supported(&uint_attr(0x2F)).unwrap(),
        AlarmBitmap::from_bits_retain(0x2F)
    );
    assert_eq!(decode_mask(&uint_attr(0x2F)).unwrap().bits(), 0x2F);
    assert_eq!(
        decode_latch(&uint_attr(0x03)).unwrap(),
        AlarmBitmap::INFLOW_ERROR | AlarmBitmap::DRAIN_ERROR
    );
    assert_eq!(decode_state(&uint_attr(0x07)).unwrap().bits(), 0x07);
    // A bit beyond the six 1.4 alarms is kept, not dropped.
    assert_eq!(decode_state(&uint_attr(1 << 31)).unwrap().bits(), 1 << 31);

    assert_eq!(
        encode_reset(AlarmBitmap::INFLOW_ERROR),
        struct_of(&|w| w.put_uint(Tag::Context(0), 1).unwrap())
    );
    assert_eq!(
        encode_modify_enabled_alarms(AlarmBitmap::from_bits_retain(0x2F)),
        struct_of(&|w| w.put_uint(Tag::Context(0), 0x2F).unwrap())
    );

    // What chip sends after Reset(InflowError) from the boot state.
    let e = NotifyEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 0x01).unwrap();
        w.put_uint(Tag::Context(2), 0x06).unwrap();
        w.put_uint(Tag::Context(3), 0x2F).unwrap();
    }))
    .unwrap();
    assert_eq!(e.active, AlarmBitmap::empty());
    assert_eq!(e.inactive, AlarmBitmap::INFLOW_ERROR);
    assert_eq!(e.state, AlarmBitmap::DRAIN_ERROR | AlarmBitmap::DOOR_ERROR);
    assert_eq!(e.mask.bits(), 0x2F);
}

#[test]
fn notify_missing_a_field_is_an_error() {
    use matter_clusters::error::ClusterError;
    let three_of_four = struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
    });
    assert!(matches!(
        clusters::dishwasher_alarm::NotifyEvent::decode(&three_of_four),
        Err(ClusterError::MissingField("Mask"))
    ));
    assert!(matches!(
        clusters::refrigerator_alarm::NotifyEvent::decode(&three_of_four),
        Err(ClusterError::MissingField("Mask"))
    ));
}

#[test]
fn refrigerator_alarm_decodes_and_door_open_notify() {
    use clusters::refrigerator_alarm::{
        decode_mask, decode_state, decode_supported, AlarmBitmap, NotifyEvent,
    };
    // chip all-clusters' defaults (all-clusters-app.matter): Mask 1, State 0,
    // Supported 1.
    assert_eq!(decode_mask(&uint_attr(1)).unwrap(), AlarmBitmap::DOOR_OPEN);
    assert_eq!(decode_state(&uint_attr(0)).unwrap(), AlarmBitmap::empty());
    assert_eq!(
        decode_supported(&uint_attr(1)).unwrap(),
        AlarmBitmap::DOOR_OPEN
    );
    // What chip sends for app-pipe SetRefrigeratorDoorStatus DoorOpen=1.
    let e = NotifyEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
        w.put_uint(Tag::Context(3), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (e.active, e.inactive),
        (AlarmBitmap::DOOR_OPEN, AlarmBitmap::empty())
    );
    assert_eq!(
        (e.state, e.mask),
        (AlarmBitmap::DOOR_OPEN, AlarmBitmap::DOOR_OPEN)
    );
}

// ---- M9-A3 B2: ResourceMonitoring-derived clusters ----------------------------
//
// HepaFilterMonitoring, ActivatedCarbonFilterMonitoring and
// WaterTankLevelMonitoring share one shape (1.4.2 ResourceMonitoring.xml):
// Condition percent, DegradationDirection / ChangeIndication enums,
// InPlaceIndicator, a writable nullable LastChangedTime (epoch-s) and
// ReplacementProductList (list<ReplacementProductStruct>). They have no events.

/// chip all-clusters' first two replacement products
/// (resource-monitoring-delegates.cpp `ImmutableReplacementProductListManager`).
fn replacement_products() -> Vec<u8> {
    list_of(&[
        &|w| {
            w.put_uint(Tag::Context(0), 0).unwrap(); // Upc
            w.put_utf8(Tag::Context(1), "111112222233").unwrap();
        },
        &|w| {
            w.put_uint(Tag::Context(0), 1).unwrap(); // Gtin8
            w.put_utf8(Tag::Context(1), "gtin8xxx").unwrap();
        },
    ])
}

macro_rules! resource_monitoring_cluster_decodes {
    ($test:ident, $m:ident) => {
        #[test]
        fn $test() {
            use clusters::$m::{
                decode_change_indication, decode_condition, decode_degradation_direction,
                decode_in_place_indicator, decode_last_changed_time,
                decode_replacement_product_list, encode_last_changed_time, encode_reset_condition,
                ChangeIndicationEnum, DegradationDirectionEnum, ProductIdentifierTypeEnum,
            };
            let products = decode_replacement_product_list(&replacement_products()).unwrap();
            let got: Vec<_> = products
                .iter()
                .map(|p| {
                    (
                        p.product_identifier_type,
                        p.product_identifier_value.as_str(),
                    )
                })
                .collect();
            assert_eq!(
                got,
                [
                    (ProductIdentifierTypeEnum::Upc, "111112222233"),
                    (ProductIdentifierTypeEnum::Gtin8, "gtin8xxx"),
                ]
            );
            assert_eq!(decode_condition(&uint_attr(100)).unwrap(), 100);
            assert_eq!(
                decode_degradation_direction(&uint_attr(1)).unwrap(),
                DegradationDirectionEnum::Down
            );
            assert_eq!(
                decode_change_indication(&uint_attr(2)).unwrap(),
                ChangeIndicationEnum::Critical
            );
            assert!(decode_in_place_indicator(&bool_attr(true)).unwrap());
            for v in [Nullable::Null, Nullable::Value(788_918_400)] {
                assert_eq!(
                    decode_last_changed_time(&encode_last_changed_time(v)).unwrap(),
                    v
                );
            }
            // ResetCondition has no fields: an empty structure.
            assert_eq!(encode_reset_condition(), [0x15, 0x18]);
        }
    };
}

resource_monitoring_cluster_decodes!(hepa_filter_monitoring_decodes, hepa_filter_monitoring);
resource_monitoring_cluster_decodes!(
    activated_carbon_filter_monitoring_decodes,
    activated_carbon_filter_monitoring
);
resource_monitoring_cluster_decodes!(
    water_tank_level_monitoring_decodes,
    water_tank_level_monitoring
);

#[test]
fn replacement_product_missing_value_is_an_error() {
    use matter_clusters::error::ClusterError;
    let bytes = list_of(&[&|w| w.put_uint(Tag::Context(0), 4).unwrap()]);
    assert!(matches!(
        clusters::water_tank_level_monitoring::decode_replacement_product_list(&bytes),
        Err(ClusterError::MissingField("ProductIdentifierValue"))
    ));
}

// ---- M9-A3 B3: OperationalState and its derived clusters ---------------------
//
// OperationalState, OvenCavityOperationalState and RvcOperationalState share
// one shape (1.4.2 OperationalState.xml; the derived clusters inherit every
// field through `members`): PhaseList (nullable list<string>), CurrentPhase
// (nullable uint8), CountdownTime (nullable elapsed-s), OperationalStateList
// (list<OperationalStateStruct>), OperationalState (enum8), OperationalError
// (ErrorStateStruct); OperationalCommandResponse { CommandResponseState };
// events OperationalError { ErrorState } and OperationCompletion
// { CompletionErrorCode, TotalOperationalTime?, PausedTime? }.
// ErrorStateLabel / OperationalStateLabel have the expression conformance
// "ID >= 128 & ID <= 191", so they are optional (only an unconditional M is
// mandatory).

/// An anonymous array of anonymous UTF-8 strings.
fn str_list_attr(values: &[&str]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_array(Tag::Anonymous).unwrap();
        for v in values {
            w.put_utf8(Tag::Anonymous, v).unwrap();
        }
        w.end_container().unwrap();
    }
    buf
}

/// `OperationalStateList` as chip encodes it
/// (`OperationalState/Structs.ipp` writes the label only when present):
/// Stopped and Running without labels, and a manufacturer state 0x80 with one.
fn operational_state_list() -> Vec<u8> {
    list_of(&[
        &|w| w.put_uint(Tag::Context(0), 0).unwrap(),
        &|w| w.put_uint(Tag::Context(0), 1).unwrap(),
        &|w| {
            w.put_uint(Tag::Context(0), 0x80).unwrap();
            w.put_utf8(Tag::Context(1), "Preheating").unwrap();
        },
    ])
}

/// `ErrorStateStruct` with only `ErrorStateID`: what chip's
/// `GenericOperationalError(id)` encodes (no label, no details).
fn error_state(id: u64) -> Vec<u8> {
    struct_of(&|w| w.put_uint(Tag::Context(0), id).unwrap())
}

/// Two tests per `OperationalState`-shaped cluster: every attribute, then the
/// command response and both events, including the optional and nullable
/// fields chip leaves out or sends as null.
macro_rules! operational_state_cluster_decodes {
    ($attributes_test:ident, $payloads_test:ident, $m:ident) => {
        #[test]
        fn $attributes_test() {
            use clusters::$m::{
                decode_countdown_time, decode_current_phase, decode_operational_error,
                decode_operational_state, decode_operational_state_list, decode_phase_list,
                ErrorStateEnum, OperationalStateEnum,
            };
            let states = decode_operational_state_list(&operational_state_list()).unwrap();
            let got: Vec<_> = states
                .iter()
                .map(|s| (s.operational_state_id, s.operational_state_label.as_deref()))
                .collect();
            assert_eq!(
                got,
                [
                    (OperationalStateEnum::Stopped, None),
                    (OperationalStateEnum::Running, None),
                    (OperationalStateEnum::Unknown(0x80), Some("Preheating")),
                ]
            );
            // A null PhaseList (no phases) and a real one.
            assert_eq!(decode_phase_list(&null_attr()).unwrap(), Nullable::Null);
            assert_eq!(
                decode_phase_list(&str_list_attr(&["pre-soak", "rinse"])).unwrap(),
                Nullable::Value(vec!["pre-soak".to_string(), "rinse".to_string()])
            );
            assert_eq!(decode_current_phase(&null_attr()).unwrap(), Nullable::Null);
            assert_eq!(
                decode_current_phase(&uint_attr(1)).unwrap(),
                Nullable::Value(1)
            );
            assert_eq!(decode_countdown_time(&null_attr()).unwrap(), Nullable::Null);
            assert_eq!(
                decode_countdown_time(&uint_attr(30)).unwrap(),
                Nullable::Value(30)
            );
            assert_eq!(
                decode_operational_state(&uint_attr(3)).unwrap(),
                OperationalStateEnum::Error
            );
            let err = decode_operational_error(&error_state(0)).unwrap();
            assert_eq!(
                (
                    err.error_state_id,
                    err.error_state_label,
                    err.error_state_details
                ),
                (ErrorStateEnum::NoError, None, None)
            );
            // A manufacturer error (0x80..=0xBF) with its label and details.
            let err = decode_operational_error(&struct_of(&|w| {
                w.put_uint(Tag::Context(0), 0x80).unwrap();
                w.put_utf8(Tag::Context(1), "Door ajar").unwrap();
                w.put_utf8(Tag::Context(2), "close the door").unwrap();
            }))
            .unwrap();
            assert_eq!(err.error_state_id, ErrorStateEnum::Unknown(0x80));
            assert_eq!(err.error_state_label.as_deref(), Some("Door ajar"));
            assert_eq!(err.error_state_details.as_deref(), Some("close the door"));
        }

        #[test]
        fn $payloads_test() {
            use clusters::$m::{
                ErrorStateEnum, OperationCompletionEvent, OperationalCommandResponse,
                OperationalErrorEvent,
            };
            // What chip sends for Pause from Stopped (OperationalStateCluster.cpp
            // HandlePauseState): CommandInvalidInState, no label.
            let r = OperationalCommandResponse::decode(&struct_of(&|w| {
                w.start_structure(Tag::Context(0)).unwrap();
                w.put_uint(Tag::Context(0), 3).unwrap();
                w.end_container().unwrap();
            }))
            .unwrap();
            assert_eq!(
                r.command_response_state.error_state_id,
                ErrorStateEnum::CommandInvalidInState
            );
            let e = OperationalErrorEvent::decode(&struct_of(&|w| {
                w.start_structure(Tag::Context(0)).unwrap();
                w.put_uint(Tag::Context(0), 2).unwrap();
                w.end_container().unwrap();
            }))
            .unwrap();
            assert_eq!(
                e.error_state.error_state_id,
                ErrorStateEnum::UnableToCompleteOperation
            );
            // OperationCompletion with both times (rvc-app's ActivityComplete
            // sends 100 and 10), then with one absent and one null.
            let e = OperationCompletionEvent::decode(&struct_of(&|w| {
                w.put_uint(Tag::Context(0), 0).unwrap();
                w.put_uint(Tag::Context(1), 100).unwrap();
                w.put_uint(Tag::Context(2), 10).unwrap();
            }))
            .unwrap();
            assert_eq!(
                (
                    e.completion_error_code,
                    e.total_operational_time,
                    e.paused_time
                ),
                (0, Some(Nullable::Value(100)), Some(Nullable::Value(10)))
            );
            let e = OperationCompletionEvent::decode(&struct_of(&|w| {
                w.put_uint(Tag::Context(0), 2).unwrap();
                w.put_null(Tag::Context(2)).unwrap();
            }))
            .unwrap();
            assert_eq!(
                (
                    e.completion_error_code,
                    e.total_operational_time,
                    e.paused_time
                ),
                (2, None, Some(Nullable::Null))
            );
        }
    };
}

operational_state_cluster_decodes!(
    operational_state_attributes_decode,
    operational_state_payloads_decode,
    operational_state
);
operational_state_cluster_decodes!(
    oven_cavity_operational_state_attributes_decode,
    oven_cavity_operational_state_payloads_decode,
    oven_cavity_operational_state
);
operational_state_cluster_decodes!(
    rvc_operational_state_attributes_decode,
    rvc_operational_state_payloads_decode,
    rvc_operational_state
);

#[test]
fn operational_state_event_ids_pinned() {
    for (error, completion) in [
        (
            clusters::operational_state::event_id::OPERATIONAL_ERROR,
            clusters::operational_state::event_id::OPERATION_COMPLETION,
        ),
        (
            clusters::oven_cavity_operational_state::event_id::OPERATIONAL_ERROR,
            clusters::oven_cavity_operational_state::event_id::OPERATION_COMPLETION,
        ),
        (
            clusters::rvc_operational_state::event_id::OPERATIONAL_ERROR,
            clusters::rvc_operational_state::event_id::OPERATION_COMPLETION,
        ),
    ] {
        assert_eq!((error, completion), (0x00, 0x01));
    }
}

#[test]
fn operational_state_commands_are_empty_structures() {
    // Every OperationalState-family request carries no fields (1.4.2
    // OperationalState.xml, RvcOperationalState.xml GoHome). The disallowed
    // ones are not generated: OvenCavity Pause/Resume, Rvc Start/Stop.
    const EMPTY: [u8; 2] = [0x15, 0x18];
    use clusters::{
        operational_state as os, oven_cavity_operational_state as oven,
        rvc_operational_state as rvc,
    };
    for bytes in [
        os::encode_pause(),
        os::encode_stop(),
        os::encode_start(),
        os::encode_resume(),
        oven::encode_stop(),
        oven::encode_start(),
        rvc::encode_pause(),
        rvc::encode_resume(),
        rvc::encode_go_home(),
    ] {
        assert_eq!(bytes, EMPTY);
    }
    assert_eq!(rvc::command_id::GO_HOME, 0x80);
    assert_eq!(oven::command_id::OPERATIONAL_COMMAND_RESPONSE, 0x04);
    assert_eq!(rvc::command_id::OPERATIONAL_COMMAND_RESPONSE, 0x04);
}

#[test]
fn rvc_operational_state_keeps_base_and_derived_values() {
    // The derived enums add values to the base's 0..=3; read from `members`,
    // a plain Stopped / NoError decodes as itself, not Unknown.
    use clusters::rvc_operational_state::{ErrorStateEnum, OperationalStateEnum};
    assert_eq!(
        OperationalStateEnum::from_raw(0),
        OperationalStateEnum::Stopped
    );
    assert_eq!(
        OperationalStateEnum::from_raw(0x40),
        OperationalStateEnum::SeekingCharger
    );
    assert_eq!(
        OperationalStateEnum::from_raw(0x42),
        OperationalStateEnum::Docked
    );
    assert_eq!(
        OperationalStateEnum::from_raw(0x46),
        OperationalStateEnum::UpdatingMaps
    );
    assert_eq!(ErrorStateEnum::from_raw(0), ErrorStateEnum::NoError);
    assert_eq!(ErrorStateEnum::from_raw(0x41), ErrorStateEnum::Stuck);
    assert_eq!(
        ErrorStateEnum::from_raw(0x4E),
        ErrorStateEnum::NavigationSensorObscured
    );
    assert_eq!(
        ErrorStateEnum::from_raw(0x4F),
        ErrorStateEnum::Unknown(0x4F)
    );
}

#[test]
fn operational_state_missing_mandatory_fields_are_errors() {
    use matter_clusters::error::ClusterError;
    // CommandResponseState, ErrorState and ErrorStateID are unconditional M.
    assert!(matches!(
        clusters::operational_state::OperationalCommandResponse::decode(&struct_of(&|_| {})),
        Err(ClusterError::MissingField("CommandResponseState"))
    ));
    assert!(matches!(
        clusters::rvc_operational_state::OperationalErrorEvent::decode(&struct_of(&|_| {})),
        Err(ClusterError::MissingField("ErrorState"))
    ));
    assert!(matches!(
        clusters::oven_cavity_operational_state::decode_operational_error(&struct_of(&|w| {
            w.put_utf8(Tag::Context(1), "label only").unwrap();
        })),
        Err(ClusterError::MissingField("ErrorStateId"))
    ));
    assert!(matches!(
        clusters::operational_state::OperationCompletionEvent::decode(&struct_of(&|w| {
            w.put_uint(Tag::Context(1), 5).unwrap();
        })),
        Err(ClusterError::MissingField("CompletionErrorCode"))
    ));
}

// ---- M9-A3 B3: appliance controls --------------------------------------------
//
// TemperatureControl, LaundryWasherControls, LaundryDryerControls and
// MicrowaveOvenControl: feature-gated scalar attributes, lists of strings and
// of enums, writable nullable attributes, and all-optional command fields
// (1.4.2 TemperatureControl.xml, LaundryWasherControls.xml,
// LaundryDryerControls.xml, MicrowaveOvenControl.xml). No events.

#[test]
fn temperature_control_decodes_and_set_temperature_encodes() {
    use clusters::temperature_control::{
        decode_max_temperature, decode_min_temperature, decode_selected_temperature_level,
        decode_step, decode_supported_temperature_levels, decode_temperature_setpoint,
        encode_set_temperature, Feature,
    };
    // chip all-clusters' levels (static-supported-temperature-levels.cpp).
    assert_eq!(
        decode_supported_temperature_levels(&str_list_attr(&["Hot", "Warm", "Freezing"])).unwrap(),
        ["Hot", "Warm", "Freezing"]
    );
    assert_eq!(decode_selected_temperature_level(&uint_attr(0)).unwrap(), 0);
    // Temperatures are signed 0.01 °C.
    assert_eq!(
        decode_temperature_setpoint(&int_attr(-1250)).unwrap(),
        -1250
    );
    assert_eq!(decode_min_temperature(&int_attr(-2000)).unwrap(), -2000);
    assert_eq!(decode_max_temperature(&int_attr(25000)).unwrap(), 25000);
    assert_eq!(decode_step(&int_attr(50)).unwrap(), 50);
    assert_eq!(Feature::TL.bits(), 0b010);
    // Each field is present only under its feature, so both are optional.
    assert_eq!(
        encode_set_temperature(None, Some(2)),
        struct_of(&|w| w.put_uint(Tag::Context(1), 2).unwrap())
    );
    assert_eq!(encode_set_temperature(None, None), [0x15, 0x18]);
}

#[test]
fn laundry_washer_controls_decode_and_writes_encode() {
    use clusters::laundry_washer_controls::{
        decode_number_of_rinses, decode_spin_speed_current, decode_spin_speeds,
        decode_supported_rinses, encode_number_of_rinses, encode_spin_speed_current,
        NumberOfRinsesEnum,
    };
    // chip all-clusters' options (laundry-washer-controls-delegate-impl.cpp).
    assert_eq!(
        decode_spin_speeds(&str_list_attr(&["Off", "Low", "Medium", "High"])).unwrap(),
        ["Off", "Low", "Medium", "High"]
    );
    assert_eq!(
        decode_supported_rinses(&uint_array_attr(&[1, 2])).unwrap(),
        [NumberOfRinsesEnum::Normal, NumberOfRinsesEnum::Extra]
    );
    for v in [Nullable::Null, Nullable::Value(2)] {
        assert_eq!(
            decode_spin_speed_current(&encode_spin_speed_current(v)).unwrap(),
            v
        );
    }
    assert_eq!(encode_spin_speed_current(Nullable::Value(2)), uint_attr(2));
    assert_eq!(encode_spin_speed_current(Nullable::Null), null_attr());
    assert_eq!(
        decode_number_of_rinses(&encode_number_of_rinses(NumberOfRinsesEnum::Max)).unwrap(),
        NumberOfRinsesEnum::Max
    );
    // A rinse count a newer revision adds is kept, not an error.
    assert_eq!(
        decode_number_of_rinses(&uint_attr(9)).unwrap(),
        NumberOfRinsesEnum::Unknown(9)
    );
}

#[test]
fn laundry_dryer_controls_decode_and_write_encodes() {
    use clusters::laundry_dryer_controls::{
        decode_selected_dryness_level, decode_supported_dryness_levels,
        encode_selected_dryness_level, DrynessLevelEnum,
    };
    // chip all-clusters' levels (laundry-dryer-controls-delegate-impl.cpp).
    assert_eq!(
        decode_supported_dryness_levels(&uint_array_attr(&[0, 1, 3])).unwrap(),
        [
            DrynessLevelEnum::Low,
            DrynessLevelEnum::Normal,
            DrynessLevelEnum::Max
        ]
    );
    for v in [Nullable::Null, Nullable::Value(DrynessLevelEnum::Extra)] {
        assert_eq!(
            decode_selected_dryness_level(&encode_selected_dryness_level(v)).unwrap(),
            v
        );
    }
    assert_eq!(
        encode_selected_dryness_level(Nullable::Value(DrynessLevelEnum::Normal)),
        uint_attr(1)
    );
}

#[test]
fn microwave_oven_control_decodes_and_commands_encode() {
    use clusters::microwave_oven_control::{
        decode_cook_time, decode_max_cook_time, decode_max_power, decode_min_power,
        decode_power_setting, decode_power_step, decode_selected_watt_index,
        decode_supported_watts, decode_watt_rating, encode_add_more_time,
        encode_set_cooking_parameters, Feature,
    };
    // chip microwave-oven-app's values (MicrowaveOvenControlCluster.cpp,
    // examples/microwave-oven-app/microwave-oven-common/src/
    // microwave-oven-device.cpp, and include/microwave-oven-device.h L229-233,
    // chip master 5cd2917a: power 20..=90 in steps of 10, at most 86400 s of
    // cook time, 90 by default): cook time 30 s of at most 86400 s, power
    // 20..=90 in steps of 10, set to 90, 1000 W rating (MicrowaveOvenInit's
    // non-WATTS branch, ~L48: `mWattRating = kExampleWatt5`). SupportedWatts
    // and SelectedWattIndex 4 are the WATTS-branch values (~L43-44: the last
    // index of the five-entry watt list); the app's default features are
    // PWRNUM|PWRLMTS, so it serves neither.
    assert_eq!(decode_cook_time(&uint_attr(30)).unwrap(), 30);
    assert_eq!(decode_max_cook_time(&uint_attr(86_400)).unwrap(), 86_400);
    assert_eq!(decode_power_setting(&uint_attr(90)).unwrap(), 90);
    assert_eq!(decode_min_power(&uint_attr(20)).unwrap(), 20);
    assert_eq!(decode_max_power(&uint_attr(90)).unwrap(), 90);
    assert_eq!(decode_power_step(&uint_attr(10)).unwrap(), 10);
    assert_eq!(decode_watt_rating(&uint_attr(1000)).unwrap(), 1000);
    assert_eq!(
        decode_supported_watts(&uint_array_attr(&[100, 300, 500, 800, 1000])).unwrap(),
        [100, 300, 500, 800, 1000]
    );
    assert_eq!(decode_selected_watt_index(&uint_attr(4)).unwrap(), 4);
    assert_eq!(
        (Feature::PWRNUM | Feature::PWRLMTS).bits(),
        0b101,
        "microwave-oven-app's features"
    );
    // Every SetCookingParameters field is optional; absent ones are omitted.
    assert_eq!(
        encode_set_cooking_parameters(None, Some(45), Some(60), None, None),
        struct_of(&|w| {
            w.put_uint(Tag::Context(1), 45).unwrap();
            w.put_uint(Tag::Context(2), 60).unwrap();
        })
    );
    assert_eq!(
        encode_set_cooking_parameters(Some(1), None, None, Some(4), Some(false)),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 1).unwrap();
            w.put_uint(Tag::Context(3), 4).unwrap();
            w.put_bool(Tag::Context(4), false).unwrap();
        })
    );
    assert_eq!(
        encode_set_cooking_parameters(None, None, None, None, None),
        [0x15, 0x18]
    );
    assert_eq!(
        encode_add_more_time(10),
        struct_of(&|w| w.put_uint(Tag::Context(0), 10).unwrap())
    );
}

// ---- M9-A3 B3: ServiceArea -----------------------------------------------------
//
// SupportedAreas is a list of AreaStruct { AreaID uint32, MapID nullable
// uint32, AreaInfo AreaInfoStruct { LocationInfo nullable
// LocationDescriptorStruct, LandmarkInfo nullable LandmarkInfoStruct } }
// (1.4.2 ServiceArea.xml). LocationDescriptorStruct is the Matter-global
// `locationdesc` (LocationName string, FloorNumber nullable int16, AreaType
// nullable `tag`), generated under chip's name for it; the landmark and area
// type `tag`s are raw uint8 (their values come from the Common Landmark /
// Area namespaces).

/// One `AreaStruct` as chip writes it (`ServiceArea/Structs.ipp`: every field,
/// null where unset).
#[allow(clippy::type_complexity)]
fn area_entry(
    id: u64,
    map: u64,
    location: Option<(&str, Option<i64>, Option<u64>)>,
    landmark: Option<(u64, Option<u64>)>,
) -> impl Fn(&mut TlvWriter<'_>) + '_ {
    move |w| {
        w.put_uint(Tag::Context(0), id).unwrap();
        w.put_uint(Tag::Context(1), map).unwrap();
        w.start_structure(Tag::Context(2)).unwrap();
        match location {
            Some((name, floor, area_type)) => {
                w.start_structure(Tag::Context(0)).unwrap();
                w.put_utf8(Tag::Context(0), name).unwrap();
                match floor {
                    Some(f) => w.put_int(Tag::Context(1), f).unwrap(),
                    None => w.put_null(Tag::Context(1)).unwrap(),
                }
                match area_type {
                    Some(t) => w.put_uint(Tag::Context(2), t).unwrap(),
                    None => w.put_null(Tag::Context(2)).unwrap(),
                }
                w.end_container().unwrap();
            }
            None => w.put_null(Tag::Context(0)).unwrap(),
        }
        match landmark {
            Some((tag, position)) => {
                w.start_structure(Tag::Context(1)).unwrap();
                w.put_uint(Tag::Context(0), tag).unwrap();
                match position {
                    Some(p) => w.put_uint(Tag::Context(1), p).unwrap(),
                    None => w.put_null(Tag::Context(1)).unwrap(),
                }
                w.end_container().unwrap();
            }
            None => w.put_null(Tag::Context(1)).unwrap(),
        }
        w.end_container().unwrap();
    }
}

#[test]
fn service_area_supported_areas_decode_rvc_app_topology() {
    use clusters::service_area::{decode_supported_areas, LocationDescriptorStruct};
    // rvc-app's areas (rvc-service-area-delegate.cpp SetMapTopology): A (7)
    // and B (1234567) on map 3, C (10050) and D (0x88888888) on map 245;
    // PlayRoom 0x41, BackDoor 0x02, Couch 0x0D, NextTo 0x01.
    let area_a = area_entry(7, 3, Some(("My Location A", Some(4), None)), None);
    let area_b = area_entry(1_234_567, 3, Some(("My Location B", None, None)), None);
    let area_c = area_entry(
        10_050,
        245,
        Some(("", Some(-1), Some(0x41))),
        Some((0x02, Some(0x01))),
    );
    let area_d = area_entry(
        0x8888_8888,
        245,
        Some(("My Location D", None, None)),
        Some((0x0D, Some(0x01))),
    );
    let areas = decode_supported_areas(&list_of(&[&area_a, &area_b, &area_c, &area_d])).unwrap();
    let ids: Vec<(u32, Nullable<u32>)> = areas.iter().map(|x| (x.area_id, x.map_id)).collect();
    assert_eq!(
        ids,
        [
            (7, Nullable::Value(3)),
            (1_234_567, Nullable::Value(3)),
            (10_050, Nullable::Value(245)),
            (0x8888_8888, Nullable::Value(245)),
        ]
    );
    let location = |i: usize| -> LocationDescriptorStruct {
        match &areas[i].area_info.location_info {
            Nullable::Value(l) => l.clone(),
            Nullable::Null => panic!("area {i}: null LocationInfo"),
        }
    };
    let a_loc = location(0);
    assert_eq!(
        (
            a_loc.location_name.as_str(),
            a_loc.floor_number,
            a_loc.area_type
        ),
        ("My Location A", Nullable::Value(4), Nullable::Null)
    );
    let c_loc = location(2);
    assert_eq!(
        (
            c_loc.location_name.as_str(),
            c_loc.floor_number,
            c_loc.area_type
        ),
        ("", Nullable::Value(-1), Nullable::Value(0x41))
    );
    assert_eq!(areas[0].area_info.landmark_info, Nullable::Null);
    match &areas[3].area_info.landmark_info {
        Nullable::Value(l) => {
            assert_eq!(
                (l.landmark_tag, l.relative_position_tag),
                (0x0D, Nullable::Value(0x01))
            );
        }
        Nullable::Null => panic!("area D: null LandmarkInfo"),
    }
}

#[test]
fn service_area_maps_selection_and_progress_decode() {
    use clusters::service_area::{
        decode_current_area, decode_estimated_end_time, decode_progress, decode_selected_areas,
        decode_supported_maps, OperationalStatusEnum,
    };
    let maps = decode_supported_maps(&list_of(&[
        &|w| {
            w.put_uint(Tag::Context(0), 3).unwrap();
            w.put_utf8(Tag::Context(1), "My Map XX").unwrap();
        },
        &|w| {
            w.put_uint(Tag::Context(0), 245).unwrap();
            w.put_utf8(Tag::Context(1), "My Map YY").unwrap();
        },
    ]))
    .unwrap();
    let got: Vec<(u32, &str)> = maps.iter().map(|m| (m.map_id, m.name.as_str())).collect();
    assert_eq!(got, [(3, "My Map XX"), (245, "My Map YY")]);
    assert_eq!(
        decode_selected_areas(&uint_array_attr(&[7, 1_234_567])).unwrap(),
        [7, 1_234_567]
    );
    assert_eq!(decode_current_area(&null_attr()).unwrap(), Nullable::Null);
    assert_eq!(
        decode_current_area(&uint_attr(7)).unwrap(),
        Nullable::Value(7)
    );
    assert_eq!(
        decode_estimated_end_time(&null_attr()).unwrap(),
        Nullable::Null
    );
    // Progress during a run (service-area-server.cpp SetProgressStatus): the
    // operating area's TotalOperationalTime is null, a pending one has none.
    let progress = decode_progress(&list_of(&[
        &|w| {
            w.put_uint(Tag::Context(0), 7).unwrap();
            w.put_uint(Tag::Context(1), 1).unwrap();
            w.put_null(Tag::Context(2)).unwrap();
        },
        &|w| {
            w.put_uint(Tag::Context(0), 1_234_567).unwrap();
            w.put_uint(Tag::Context(1), 0).unwrap();
        },
    ]))
    .unwrap();
    let got: Vec<_> = progress
        .iter()
        .map(|p| {
            (
                p.area_id,
                p.status,
                p.total_operational_time,
                p.estimated_time,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                7,
                OperationalStatusEnum::Operating,
                Some(Nullable::Null),
                None
            ),
            (1_234_567, OperationalStatusEnum::Pending, None, None),
        ]
    );
}

#[test]
fn service_area_commands_encode_and_responses_decode() {
    use clusters::service_area::{
        encode_select_areas, encode_skip_area, SelectAreasResponse, SelectAreasStatus,
        SkipAreaResponse, SkipAreaStatus,
    };
    // SelectAreas carries a list<uint32>; SkipArea one uint32.
    let mut select = Vec::new();
    {
        let mut w = TlvWriter::new(&mut select);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(0)).unwrap();
        w.put_uint(Tag::Anonymous, 7).unwrap();
        w.put_uint(Tag::Anonymous, 0x8888_8888).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
    }
    assert_eq!(encode_select_areas(&vec![7, 0x8888_8888]), select);
    assert_eq!(
        encode_skip_area(7),
        struct_of(&|w| w.put_uint(Tag::Context(0), 7).unwrap())
    );
    // chip always sends StatusText, empty unless the status needs a reason.
    let r = SelectAreasResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 3).unwrap();
        w.put_utf8(
            Tag::Context(1),
            "all selected areas must be in the same map",
        )
        .unwrap();
    }))
    .unwrap();
    assert_eq!(r.status, SelectAreasStatus::InvalidSet);
    assert_eq!(r.status_text, "all selected areas must be in the same map");
    let r = SkipAreaResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_utf8(Tag::Context(1), "").unwrap();
    }))
    .unwrap();
    assert_eq!(
        (r.status, r.status_text.as_str()),
        (SkipAreaStatus::InvalidAreaList, "")
    );
}

#[test]
fn service_area_missing_mandatory_fields_are_errors() {
    use matter_clusters::error::ClusterError;
    // StatusText is an unconditional M in 1.4.2, and chip always sends it.
    assert!(matches!(
        clusters::service_area::SelectAreasResponse::decode(&struct_of(&|w| {
            w.put_uint(Tag::Context(0), 0).unwrap();
        })),
        Err(ClusterError::MissingField("StatusText"))
    ));
    // AreaInfo is mandatory in every AreaStruct.
    assert!(matches!(
        clusters::service_area::decode_supported_areas(&list_of(&[&|w| {
            w.put_uint(Tag::Context(0), 7).unwrap();
            w.put_null(Tag::Context(1)).unwrap();
        }])),
        Err(ClusterError::MissingField("AreaInfo"))
    ));
    // LocationName is mandatory inside a present LocationDescriptorStruct.
    assert!(matches!(
        clusters::service_area::LocationDescriptorStruct::decode(&struct_of(&|w| {
            w.put_null(Tag::Context(1)).unwrap();
            w.put_null(Tag::Context(2)).unwrap();
        })),
        Err(ClusterError::MissingField("LocationName"))
    ));
}

// ---- M9-A3 B4: Thermostat weekly schedule (Matter 1.4 SCH, supplemented) ----
//
// Feature SCH (bit 3): StartOfWeek / NumberOfWeeklyTransitions /
// NumberOfDailyTransitions, SetWeeklySchedule / GetWeeklySchedule /
// ClearWeeklySchedule and GetWeeklyScheduleResponse (1.4.2 Thermostat.xml).
// The 1.5.1 model removed them; the dump adds them from supplement-1.4.json and
// reuses the model's own ScheduleDayOfWeekBitmap, ScheduleModeBitmap,
// StartOfWeekEnum and WeeklyScheduleTransitionStruct.

#[test]
fn thermostat_weekly_schedule_attributes_and_response_decode() {
    use clusters::thermostat::{
        attribute_id, command_id, decode_number_of_daily_transitions,
        decode_number_of_weekly_transitions, decode_start_of_week, Feature,
        GetWeeklyScheduleResponse, ScheduleDayOfWeekBitmap, ScheduleModeBitmap, StartOfWeekEnum,
    };
    assert_eq!(Feature::SCH.bits(), 1 << 3);
    assert_eq!(
        (
            attribute_id::START_OF_WEEK,
            attribute_id::NUMBER_OF_WEEKLY_TRANSITIONS,
            attribute_id::NUMBER_OF_DAILY_TRANSITIONS
        ),
        (0x0020, 0x0021, 0x0022)
    );
    assert_eq!(
        (
            command_id::GET_WEEKLY_SCHEDULE_RESPONSE,
            command_id::SET_WEEKLY_SCHEDULE,
            command_id::GET_WEEKLY_SCHEDULE,
            command_id::CLEAR_WEEKLY_SCHEDULE
        ),
        (0x00, 0x01, 0x02, 0x03)
    );
    assert_eq!(
        decode_start_of_week(&uint_attr(1)).unwrap(),
        StartOfWeekEnum::Monday
    );
    assert_eq!(
        decode_number_of_weekly_transitions(&uint_attr(70)).unwrap(),
        70
    );
    assert_eq!(
        decode_number_of_daily_transitions(&uint_attr(10)).unwrap(),
        10
    );
    // A reply for Saturday and Sunday, heat and cool, one transition whose
    // heat setpoint is null (no heat change at 06:00).
    let r = GetWeeklyScheduleResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_uint(Tag::Context(1), 0b0100_0001).unwrap();
        w.put_uint(Tag::Context(2), 0b11).unwrap();
        w.start_array(Tag::Context(3)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 360).unwrap();
        w.put_null(Tag::Context(1)).unwrap();
        w.put_int(Tag::Context(2), -150).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
    }))
    .unwrap();
    assert_eq!(r.number_of_transitions_for_sequence, 1);
    assert_eq!(
        r.day_of_week_for_sequence,
        ScheduleDayOfWeekBitmap::SUNDAY | ScheduleDayOfWeekBitmap::SATURDAY
    );
    assert_eq!(r.mode_for_sequence, ScheduleModeBitmap::all());
    let t = &r.transitions[..];
    assert_eq!(t.len(), 1);
    assert_eq!(
        (t[0].transition_time, t[0].heat_setpoint, t[0].cool_setpoint),
        (360, Nullable::Null, Nullable::Value(-150))
    );
}

#[test]
fn thermostat_weekly_schedule_requests_encode() {
    use clusters::thermostat::{
        encode_clear_weekly_schedule, encode_get_weekly_schedule, ScheduleDayOfWeekBitmap,
        ScheduleModeBitmap,
    };
    assert_eq!(encode_clear_weekly_schedule(), [0x15, 0x18]);
    assert_eq!(
        encode_get_weekly_schedule(
            ScheduleDayOfWeekBitmap::AWAY,
            ScheduleModeBitmap::COOL_SETPOINT_PRESENT
        ),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 0x80).unwrap();
            w.put_uint(Tag::Context(1), 0x02).unwrap();
        })
    );
}

#[test]
fn thermostat_weekly_schedule_response_missing_list_is_an_error() {
    use matter_clusters::error::ClusterError;
    // Transitions is an unconditional M in 1.4.2 (Thermostat.xml:1293).
    assert!(matches!(
        clusters::thermostat::GetWeeklyScheduleResponse::decode(&struct_of(&|w| {
            w.put_uint(Tag::Context(0), 0).unwrap();
            w.put_uint(Tag::Context(1), 1).unwrap();
            w.put_uint(Tag::Context(2), 1).unwrap();
        })),
        Err(ClusterError::MissingField("Transitions"))
    ));
}

// ---- M9-A3 B4: WindowCovering absolute position (Matter 1.4 ABS, supplemented) ----
//
// Feature ABS (bit 3, provisional in 1.4.2): the physical and installed limits,
// CurrentPositionLift/Tilt (nullable), GoToLiftValue / GoToTiltValue
// (1.4.2 WindowCovering.xml). The 1.5.1 model removed them; the dump adds them
// from supplement-1.4.json.

#[test]
fn window_covering_absolute_position_decodes_and_encodes() {
    use clusters::window_covering::{
        attribute_id as a, command_id, decode_current_position_lift, decode_current_position_tilt,
        decode_installed_closed_limit_lift, decode_installed_closed_limit_tilt,
        decode_installed_open_limit_lift, decode_installed_open_limit_tilt,
        decode_physical_closed_limit_lift, decode_physical_closed_limit_tilt,
        encode_go_to_lift_value, encode_go_to_tilt_value, Feature,
    };
    assert_eq!(Feature::ABS.bits(), 1 << 3);
    assert_eq!(
        [
            a::PHYSICAL_CLOSED_LIMIT_LIFT,
            a::PHYSICAL_CLOSED_LIMIT_TILT,
            a::CURRENT_POSITION_LIFT,
            a::CURRENT_POSITION_TILT,
            a::INSTALLED_OPEN_LIMIT_LIFT,
            a::INSTALLED_CLOSED_LIMIT_LIFT,
            a::INSTALLED_OPEN_LIMIT_TILT,
            a::INSTALLED_CLOSED_LIMIT_TILT
        ],
        [0x0001, 0x0002, 0x0003, 0x0004, 0x0010, 0x0011, 0x0012, 0x0013]
    );
    assert_eq!(
        (command_id::GO_TO_LIFT_VALUE, command_id::GO_TO_TILT_VALUE),
        (0x04, 0x07)
    );
    // chip all-clusters' defaults (all-clusters-app.matter, both refs).
    assert_eq!(
        decode_physical_closed_limit_lift(&uint_attr(0xFFFF)).unwrap(),
        0xFFFF
    );
    assert_eq!(
        decode_physical_closed_limit_tilt(&uint_attr(0xFFFF)).unwrap(),
        0xFFFF
    );
    assert_eq!(decode_installed_open_limit_lift(&uint_attr(0)).unwrap(), 0);
    assert_eq!(
        decode_installed_closed_limit_lift(&uint_attr(0xFFFF)).unwrap(),
        0xFFFF
    );
    assert_eq!(decode_installed_open_limit_tilt(&uint_attr(0)).unwrap(), 0);
    assert_eq!(
        decode_installed_closed_limit_tilt(&uint_attr(0xFFFF)).unwrap(),
        0xFFFF
    );
    // The current positions are nullable (unknown position).
    assert_eq!(
        decode_current_position_lift(&null_attr()).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        decode_current_position_tilt(&uint_attr(0x7FFF)).unwrap(),
        Nullable::Value(0x7FFF)
    );
    assert!(decode_installed_open_limit_lift(&uint_attr(0x1_0000)).is_err());
    assert_eq!(
        encode_go_to_lift_value(250),
        struct_of(&|w| w.put_uint(Tag::Context(0), 250).unwrap())
    );
    assert_eq!(
        encode_go_to_tilt_value(900),
        struct_of(&|w| w.put_uint(Tag::Context(0), 900).unwrap())
    );
}

// ---- M9-A3 B4: SmokeCoAlarm and BooleanStateConfiguration ----------------
//
// SmokeCoAlarm (1.4.2 SmokeCOAlarm.xml): enum and bool attributes, a writable
// SmokeSensitivityLevel, SelfTestRequest, and eleven events -- five carry an
// AlarmSeverityLevel, six carry nothing (those get only their `event_id`
// const, like BasicInformation `ShutDown`). BooleanStateConfiguration (1.4.2
// BooleanStateConfiguration.xml): bitmap attributes, a writable sensitivity
// level, SuppressAlarm / EnableDisableAlarm, AlarmsStateChanged and
// SensorFault.

#[test]
fn smoke_co_alarm_attributes_decode() {
    use clusters::smoke_co_alarm::{
        decode_battery_alert, decode_co_state, decode_contamination_state, decode_device_muted,
        decode_end_of_service_alert, decode_expiry_date, decode_expressed_state,
        decode_hardware_fault_alert, decode_interconnect_co_alarm, decode_interconnect_smoke_alarm,
        decode_smoke_sensitivity_level, decode_smoke_state, decode_test_in_progress,
        encode_smoke_sensitivity_level, AlarmStateEnum, ContaminationStateEnum, EndOfServiceEnum,
        ExpressedStateEnum, Feature, MuteStateEnum, SensitivityEnum,
    };
    assert_eq!(
        (Feature::SMOKE | Feature::CO).bits(),
        3,
        "all-clusters' features"
    );
    assert_eq!(
        decode_expressed_state(&uint_attr(1)).unwrap(),
        ExpressedStateEnum::SmokeAlarm
    );
    // chip master adds Inoperative (9, revision 2): kept, not an error.
    assert_eq!(
        decode_expressed_state(&uint_attr(9)).unwrap(),
        ExpressedStateEnum::Unknown(9)
    );
    assert_eq!(
        decode_smoke_state(&uint_attr(2)).unwrap(),
        AlarmStateEnum::Critical
    );
    assert_eq!(
        decode_co_state(&uint_attr(1)).unwrap(),
        AlarmStateEnum::Warning
    );
    assert_eq!(
        decode_battery_alert(&uint_attr(0)).unwrap(),
        AlarmStateEnum::Normal
    );
    assert_eq!(
        decode_device_muted(&uint_attr(1)).unwrap(),
        MuteStateEnum::Muted
    );
    assert!(decode_test_in_progress(&bool_attr(true)).unwrap());
    assert!(!decode_hardware_fault_alert(&bool_attr(false)).unwrap());
    assert_eq!(
        decode_end_of_service_alert(&uint_attr(1)).unwrap(),
        EndOfServiceEnum::Expired
    );
    assert_eq!(
        decode_interconnect_smoke_alarm(&uint_attr(1)).unwrap(),
        AlarmStateEnum::Warning
    );
    assert_eq!(
        decode_interconnect_co_alarm(&uint_attr(0)).unwrap(),
        AlarmStateEnum::Normal
    );
    assert_eq!(
        decode_contamination_state(&uint_attr(3)).unwrap(),
        ContaminationStateEnum::Critical
    );
    // all-clusters' ExpiryDate default at master (epoch-s).
    assert_eq!(
        decode_expiry_date(&uint_attr(3_976_214_400)).unwrap(),
        3_976_214_400
    );
    assert_eq!(
        decode_smoke_sensitivity_level(&encode_smoke_sensitivity_level(SensitivityEnum::Low))
            .unwrap(),
        SensitivityEnum::Low
    );
    assert_eq!(
        encode_smoke_sensitivity_level(SensitivityEnum::Standard),
        uint_attr(1)
    );
}

#[test]
fn smoke_co_alarm_events_and_self_test_decode() {
    use clusters::smoke_co_alarm::{
        encode_self_test_request, event_id, AlarmStateEnum, CoAlarmEvent, InterconnectCoAlarmEvent,
        InterconnectSmokeAlarmEvent, LowBatteryEvent, SmokeAlarmEvent,
    };
    assert_eq!(encode_self_test_request(), [0x15, 0x18]);
    let severity = |v| struct_of(&|w| w.put_uint(Tag::Context(0), v).unwrap());
    assert_eq!(
        SmokeAlarmEvent::decode(&severity(2))
            .unwrap()
            .alarm_severity_level,
        AlarmStateEnum::Critical
    );
    assert_eq!(
        CoAlarmEvent::decode(&severity(1))
            .unwrap()
            .alarm_severity_level,
        AlarmStateEnum::Warning
    );
    assert_eq!(
        LowBatteryEvent::decode(&severity(1))
            .unwrap()
            .alarm_severity_level,
        AlarmStateEnum::Warning
    );
    assert_eq!(
        InterconnectSmokeAlarmEvent::decode(&severity(1))
            .unwrap()
            .alarm_severity_level,
        AlarmStateEnum::Warning
    );
    assert_eq!(
        InterconnectCoAlarmEvent::decode(&severity(1))
            .unwrap()
            .alarm_severity_level,
        AlarmStateEnum::Warning
    );
    // All eleven event ids; the six fieldless events (HardwareFault,
    // EndOfService, SelfTestComplete, AlarmMuted, MuteEnded, AllClear) have
    // only these ids (no payload struct).
    assert_eq!(
        [
            event_id::SMOKE_ALARM,
            event_id::CO_ALARM,
            event_id::LOW_BATTERY,
            event_id::HARDWARE_FAULT,
            event_id::END_OF_SERVICE,
            event_id::SELF_TEST_COMPLETE,
            event_id::ALARM_MUTED,
            event_id::MUTE_ENDED,
            event_id::INTERCONNECT_SMOKE_ALARM,
            event_id::INTERCONNECT_CO_ALARM,
            event_id::ALL_CLEAR
        ],
        [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A]
    );
    assert!(matches!(
        SmokeAlarmEvent::decode(&struct_of(&|_| {})),
        Err(matter_clusters::error::ClusterError::MissingField(
            "AlarmSeverityLevel"
        ))
    ));
}

#[test]
fn boolean_state_configuration_decodes_and_commands_encode() {
    use clusters::boolean_state_configuration::{
        decode_alarms_active, decode_alarms_enabled, decode_alarms_supported,
        decode_alarms_suppressed, decode_current_sensitivity_level,
        decode_default_sensitivity_level, decode_sensor_fault, decode_supported_sensitivity_levels,
        encode_current_sensitivity_level, encode_enable_disable_alarm, encode_suppress_alarm,
        AlarmModeBitmap, AlarmsStateChangedEvent, Feature, SensorFaultBitmap, SensorFaultEvent,
    };
    // all-clusters: features 0x0F, three levels (default 2), both alarms.
    assert_eq!(Feature::all().bits(), 0x0F);
    assert_eq!(
        decode_supported_sensitivity_levels(&uint_attr(3)).unwrap(),
        3
    );
    assert_eq!(decode_default_sensitivity_level(&uint_attr(2)).unwrap(), 2);
    assert_eq!(
        decode_current_sensitivity_level(&encode_current_sensitivity_level(1)).unwrap(),
        1
    );
    assert_eq!(
        decode_alarms_supported(&uint_attr(3)).unwrap(),
        AlarmModeBitmap::all()
    );
    assert_eq!(
        decode_alarms_active(&uint_attr(0)).unwrap(),
        AlarmModeBitmap::empty()
    );
    assert_eq!(
        decode_alarms_enabled(&uint_attr(3)).unwrap(),
        AlarmModeBitmap::all()
    );
    assert_eq!(
        decode_alarms_suppressed(&uint_attr(1)).unwrap(),
        AlarmModeBitmap::VISUAL
    );
    // A bit a later revision adds survives the decode.
    assert_eq!(
        decode_sensor_fault(&uint_attr(0x8001)).unwrap().bits(),
        0x8001
    );
    assert_eq!(
        encode_suppress_alarm(AlarmModeBitmap::VISUAL),
        struct_of(&|w| w.put_uint(Tag::Context(0), 1).unwrap())
    );
    assert_eq!(
        encode_enable_disable_alarm(AlarmModeBitmap::all()),
        struct_of(&|w| w.put_uint(Tag::Context(0), 3).unwrap())
    );
    // chip's server sends AlarmsSuppressed only with the SPRS feature (master
    // BooleanStateConfigurationCluster::GenerateAlarmsStateChangedEvent;
    // v1.4.2.0 emitAlarmsStateChangedEvent).
    let e = AlarmsStateChangedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 3).unwrap();
        w.put_uint(Tag::Context(1), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (e.alarms_active, e.alarms_suppressed),
        (AlarmModeBitmap::all(), Some(AlarmModeBitmap::VISUAL))
    );
    let e = AlarmsStateChangedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
    }))
    .unwrap();
    assert_eq!(e.alarms_suppressed, None);
    let f = SensorFaultEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(f.sensor_fault, SensorFaultBitmap::GENERAL_FAULT);
}

// ---- M9-A3 B4: ValveConfigurationAndControl -------------------------------
//
// 1.4.2 ValveConfigurationControl.xml: nullable durations, states and levels,
// writable DefaultOpenDuration (nullable) and DefaultOpenLevel, Open (two
// optional fields, OpenDuration also nullable) and Close, events
// ValveStateChanged { ValveState, ValveLevel? } and ValveFault.

#[test]
fn valve_attributes_decode_and_writes_encode() {
    use clusters::valve_configuration_and_control::{
        decode_auto_close_time, decode_current_level, decode_current_state,
        decode_default_open_duration, decode_default_open_level, decode_level_step,
        decode_open_duration, decode_remaining_duration, decode_target_level, decode_target_state,
        decode_valve_fault, encode_default_open_duration, encode_default_open_level, Feature,
        StatusCodeEnum, ValveFaultBitmap, ValveStateEnum,
    };
    assert_eq!(
        (Feature::TS | Feature::LVL).bits(),
        3,
        "all-clusters' features"
    );
    // A closed valve with no countdown: chip nulls OpenDuration and
    // RemainingDuration (and AutoCloseTime with TS) on Close.
    assert_eq!(decode_open_duration(&null_attr()).unwrap(), Nullable::Null);
    assert_eq!(
        decode_remaining_duration(&null_attr()).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        decode_auto_close_time(&null_attr()).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        decode_auto_close_time(&uint_attr(0x0006_1A2B_3C4D_5E6F)).unwrap(),
        Nullable::Value(0x0006_1A2B_3C4D_5E6F)
    );
    assert_eq!(
        decode_remaining_duration(&uint_attr(59)).unwrap(),
        Nullable::Value(59)
    );
    assert_eq!(
        decode_current_state(&uint_attr(2)).unwrap(),
        Nullable::Value(ValveStateEnum::Transitioning)
    );
    assert_eq!(decode_target_state(&null_attr()).unwrap(), Nullable::Null);
    assert_eq!(
        decode_current_level(&uint_attr(50)).unwrap(),
        Nullable::Value(50)
    );
    assert_eq!(decode_target_level(&null_attr()).unwrap(), Nullable::Null);
    assert_eq!(decode_level_step(&uint_attr(2)).unwrap(), 2);
    assert_eq!(decode_default_open_level(&uint_attr(100)).unwrap(), 100);
    assert_eq!(
        decode_valve_fault(&uint_attr(0b10_0001)).unwrap(),
        ValveFaultBitmap::GENERAL_FAULT | ValveFaultBitmap::CURRENT_EXCEEDED
    );
    for v in [Nullable::Null, Nullable::Value(30)] {
        assert_eq!(
            decode_default_open_duration(&encode_default_open_duration(v)).unwrap(),
            v
        );
    }
    assert_eq!(encode_default_open_level(50), uint_attr(50));
    // The cluster-specific status chip answers Open/Close with during a fault.
    assert_eq!(
        StatusCodeEnum::from_raw(2),
        StatusCodeEnum::FailureDueToFault
    );
}

#[test]
fn valve_commands_encode_and_events_decode() {
    use clusters::valve_configuration_and_control::{
        encode_close, encode_open, ValveFaultBitmap, ValveFaultEvent, ValveStateChangedEvent,
        ValveStateEnum,
    };
    // OpenDuration: absent (use DefaultOpenDuration), null (open until
    // closed) and a value are three different requests.
    assert_eq!(encode_open(None, None), [0x15, 0x18]);
    assert_eq!(
        encode_open(Some(Nullable::Value(60)), None),
        struct_of(&|w| w.put_uint(Tag::Context(0), 60).unwrap())
    );
    assert_eq!(
        encode_open(Some(Nullable::Null), Some(50)),
        struct_of(&|w| {
            w.put_null(Tag::Context(0)).unwrap();
            w.put_uint(Tag::Context(1), 50).unwrap();
        })
    );
    assert_eq!(encode_close(), [0x15, 0x18]);
    // v1.4.2.0's server sends ValveState only; master adds ValveLevel with LVL.
    let e = ValveStateChangedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
    }))
    .unwrap();
    assert_eq!((e.valve_state, e.valve_level), (ValveStateEnum::Open, None));
    let e = ValveStateChangedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (e.valve_state, e.valve_level),
        (ValveStateEnum::Closed, Some(0))
    );
    let f = ValveFaultEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(f.valve_fault, ValveFaultBitmap::GENERAL_FAULT);
    assert!(matches!(
        ValveStateChangedEvent::decode(&struct_of(&|_| {})),
        Err(matter_clusters::error::ClusterError::MissingField(
            "ValveState"
        ))
    ));
}

// ---- M9-A3 B4: ScenesManagement ----------------------------------------------
//
// 1.4.2 Scenes.xml (cluster 0x0062): SceneTableSize, FabricSceneInfo (a
// fabric-scoped list of SceneInfoStruct whose CurrentScene, CurrentGroup and
// SceneValid are fabric-sensitive, so Option), eight commands with their
// responses, no events. AttributeValuePairStruct's eight value fields are a
// choice group (O.a): each is an Option, and exactly one should be present.

/// Our fabric's `FabricSceneInfo` entry as chip encodes it (every field).
fn own_scene_info(w: &mut TlvWriter<'_>) {
    w.put_uint(Tag::Context(0), 2).unwrap();
    w.put_uint(Tag::Context(1), 1).unwrap();
    w.put_uint(Tag::Context(2), 0).unwrap();
    w.put_bool(Tag::Context(3), true).unwrap();
    w.put_uint(Tag::Context(4), 6).unwrap();
    w.put_uint(Tag::Context(254), 1).unwrap();
}

/// Another fabric's entry as chip's `SceneInfoStruct::EncodeForRead` sends it
/// on an unfiltered read: `SceneCount`, `RemainingCapacity` and `FabricIndex` only
/// (`zzz_generated/app-common/clusters/ScenesManagement/Structs.ipp`).
fn other_fabric_scene_info(w: &mut TlvWriter<'_>) {
    w.put_uint(Tag::Context(0), 1).unwrap();
    w.put_uint(Tag::Context(4), 7).unwrap();
    w.put_uint(Tag::Context(254), 2).unwrap();
}

#[test]
fn scenes_fabric_scene_info_decodes_with_other_fabrics_fields_withheld() {
    use clusters::scenes_management::{decode_fabric_scene_info, decode_scene_table_size};
    // all-clusters' SceneTableSize default (16).
    assert_eq!(decode_scene_table_size(&uint_attr(16)).unwrap(), 16);
    let list =
        decode_fabric_scene_info(&list_of(&[&own_scene_info, &other_fabric_scene_info])).unwrap();
    let got: Vec<_> = list
        .iter()
        .map(|e| {
            (
                e.fabric_index,
                e.scene_count,
                e.current_scene,
                e.current_group,
                e.scene_valid,
                e.remaining_capacity,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (1, 2, Some(1), Some(0), Some(true), 6),
            (2, 1, None, None, None, 7)
        ]
    );
}

#[test]
fn scenes_scene_info_with_one_sensitive_field_missing_still_decodes() {
    // chip withholds the three sensitive fields together, but each is relaxed
    // on its own, so an entry missing any one of them decodes.
    // SceneInfoStruct is decode-only (FabricSceneInfo is read-only and no
    // request carries it, so it has no encoder: M9-A3 B4 encoder rule).
    for missing in 1..=3 {
        let bytes = struct_of(&|w| {
            w.put_uint(Tag::Context(0), 2).unwrap();
            if missing != 1 {
                w.put_uint(Tag::Context(1), 1).unwrap();
            }
            if missing != 2 {
                w.put_uint(Tag::Context(2), 0).unwrap();
            }
            if missing != 3 {
                w.put_bool(Tag::Context(3), true).unwrap();
            }
            w.put_uint(Tag::Context(4), 6).unwrap();
            w.put_uint(Tag::Context(254), 1).unwrap();
        });
        let e = clusters::scenes_management::SceneInfoStruct::decode(&bytes).unwrap();
        assert_eq!(
            [
                e.current_scene.is_none(),
                e.current_group.is_none(),
                e.scene_valid.is_none()
            ],
            [missing == 1, missing == 2, missing == 3]
        );
    }
}

#[test]
fn scenes_responses_decode() {
    use clusters::scenes_management::{
        AddSceneResponse, CopySceneResponse, GetSceneMembershipResponse, ViewSceneResponse,
    };
    let add = AddSceneResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
    }))
    .unwrap();
    assert_eq!((add.status, add.group_id, add.scene_id), (0, 0, 1));
    // A found scene: transition, name and one OnOff extension field set.
    let view = ViewSceneResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
        w.put_uint(Tag::Context(3), 1000).unwrap();
        w.put_utf8(Tag::Context(4), "Evening").unwrap();
        w.start_array(Tag::Context(5)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 6).unwrap();
        w.start_array(Tag::Context(1)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 1).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
    }))
    .unwrap();
    assert_eq!(view.transition_time, Some(1000));
    assert_eq!(view.scene_name.as_deref(), Some("Evening"));
    let efs = view.extension_field_set_structs.unwrap();
    assert_eq!(efs.len(), 1);
    assert_eq!(efs[0].cluster_id, 6);
    let pair = &efs[0].attribute_value_list[0];
    assert_eq!(
        (pair.attribute_id, pair.value_unsigned8, pair.value_signed16),
        (0, Some(1), None)
    );
    // NotFound (0x8B): chip sets only Status, GroupId and SceneId
    // (master ScenesManagementCluster::HandleViewScene; v1.4.2.0
    // ScenesServer::HandleViewScene / ViewSceneParse; "Status == Success"
    // fields).
    let missing = ViewSceneResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0x8B).unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
        w.put_uint(Tag::Context(2), 9).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            missing.status,
            missing.transition_time,
            missing.scene_name,
            missing.extension_field_set_structs
        ),
        (0x8B, None, None, None)
    );
    let membership = GetSceneMembershipResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 14).unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap();
        w.start_array(Tag::Context(3)).unwrap();
        w.put_uint(Tag::Anonymous, 1).unwrap();
        w.put_uint(Tag::Anonymous, 2).unwrap();
        w.end_container().unwrap();
    }))
    .unwrap();
    assert_eq!(
        (membership.capacity, membership.scene_list),
        (Nullable::Value(14), Some(vec![1, 2]))
    );
    let unknown_capacity = GetSceneMembershipResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0x85).unwrap();
        w.put_null(Tag::Context(1)).unwrap();
        w.put_uint(Tag::Context(2), 7).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (unknown_capacity.capacity, unknown_capacity.scene_list),
        (Nullable::Null, None)
    );
    let copy = CopySceneResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            copy.status,
            copy.group_identifier_from,
            copy.scene_identifier_from
        ),
        (0, 0, 1)
    );
}

#[test]
fn scenes_requests_encode() {
    use clusters::scenes_management::{
        encode_copy_scene, encode_get_scene_membership, encode_recall_scene,
        encode_remove_all_scenes, encode_remove_scene, encode_store_scene, encode_view_scene,
        CopyModeBitmap,
    };
    let group_scene = struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 1).unwrap();
    });
    assert_eq!(encode_view_scene(0, 1), group_scene);
    assert_eq!(encode_remove_scene(0, 1), group_scene);
    assert_eq!(encode_store_scene(0, 1), group_scene);
    let group_only = struct_of(&|w| w.put_uint(Tag::Context(0), 0).unwrap());
    assert_eq!(encode_remove_all_scenes(0), group_only);
    assert_eq!(encode_get_scene_membership(0), group_only);
    // RecallScene's TransitionTime: absent, null and a value differ.
    assert_eq!(encode_recall_scene(0, 1, None), group_scene);
    assert_eq!(
        encode_recall_scene(0, 1, Some(Nullable::Null)),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 0).unwrap();
            w.put_uint(Tag::Context(1), 1).unwrap();
            w.put_null(Tag::Context(2)).unwrap();
        })
    );
    assert_eq!(
        encode_copy_scene(CopyModeBitmap::empty(), 0, 1, 0, 3),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 0).unwrap();
            w.put_uint(Tag::Context(1), 0).unwrap();
            w.put_uint(Tag::Context(2), 1).unwrap();
            w.put_uint(Tag::Context(3), 0).unwrap();
            w.put_uint(Tag::Context(4), 3).unwrap();
        })
    );
}

// ---- M9-A3 B5: EnergyPreference and PowerTopology --------------------------
//
// Attributes only (1.4.2 EnergyPreference.xml, PowerTopology.xml). Values are
// all-clusters' (energy-preference-delegate.cpp, the same at v1.4.2.0 and
// master; v1.4.2.0's power-topology-stub.cpp).

#[test]
fn energy_preference_attributes_decode() {
    use clusters::energy_preference::{
        decode_current_energy_balance, decode_current_low_power_mode_sensitivity,
        decode_energy_balances, decode_energy_priorities, decode_low_power_mode_sensitivities,
        encode_current_energy_balance, encode_current_low_power_mode_sensitivity,
        EnergyPriorityEnum, Feature,
    };
    use matter_clusters::error::ClusterError;
    assert_eq!(
        (Feature::BALA | Feature::LPMS).bits(),
        3,
        "all-clusters' features"
    );
    // all-clusters' three EnergyBalances; the middle step has no label.
    let balances = decode_energy_balances(&list_of(&[
        &|w| {
            w.put_uint(Tag::Context(0), 0).unwrap();
            w.put_utf8(Tag::Context(1), "Efficient").unwrap();
        },
        &|w| w.put_uint(Tag::Context(0), 50).unwrap(),
        &|w| {
            w.put_uint(Tag::Context(0), 100).unwrap();
            w.put_utf8(Tag::Context(1), "Comfort").unwrap();
        },
    ]))
    .unwrap();
    let got: Vec<(u8, Option<&str>)> = balances
        .iter()
        .map(|b| (b.step, b.label.as_deref()))
        .collect();
    assert_eq!(
        got,
        [(0, Some("Efficient")), (50, None), (100, Some("Comfort"))]
    );
    assert_eq!(
        decode_energy_priorities(&uint_array_attr(&[2, 0])).unwrap(),
        [EnergyPriorityEnum::Efficiency, EnergyPriorityEnum::Comfort]
    );
    // A priority a later revision adds is kept, not an error.
    assert_eq!(
        decode_energy_priorities(&uint_array_attr(&[9])).unwrap(),
        [EnergyPriorityEnum::Unknown(9)]
    );
    let sensitivities = decode_low_power_mode_sensitivities(&list_of(&[&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_utf8(Tag::Context(1), "1 Minute").unwrap();
    }]))
    .unwrap();
    assert_eq!(sensitivities[0].label.as_deref(), Some("1 Minute"));
    assert_eq!(
        decode_current_energy_balance(&encode_current_energy_balance(1)).unwrap(),
        1
    );
    assert_eq!(encode_current_low_power_mode_sensitivity(2), uint_attr(2));
    assert_eq!(
        decode_current_low_power_mode_sensitivity(&uint_attr(2)).unwrap(),
        2
    );
    // Step is mandatory.
    assert!(matches!(
        decode_energy_balances(&list_of(&[&|w| w
            .put_utf8(Tag::Context(1), "No step")
            .unwrap()])),
        Err(ClusterError::MissingField("Step"))
    ));
}

#[test]
fn power_topology_attributes_decode() {
    use clusters::power_topology::{decode_active_endpoints, decode_available_endpoints, Feature};
    // all-clusters v1.4.2.0 sets SET | DYPF and serves both lists empty (its
    // stub delegate has no endpoints); evse-app sets NODE only and serves
    // neither attribute.
    assert_eq!((Feature::SET | Feature::DYPF).bits(), 0b1100);
    assert_eq!(Feature::NODE.bits(), 1);
    assert_eq!(
        decode_available_endpoints(&uint_array_attr(&[])).unwrap(),
        Vec::<u16>::new()
    );
    assert_eq!(
        decode_active_endpoints(&uint_array_attr(&[1, 2])).unwrap(),
        [1, 2]
    );
}

// ---- M9-A3 B5: WaterHeaterManagement ---------------------------------------
//
// 1.4.2 WaterHeaterManagement.xml. Boost takes a WaterHeaterBoostInfoStruct,
// which BoostStarted carries back; BoostEnded has no fields. Only HeaterTypes
// (WhmManufacturer::Init: ImmersionElement1) and the feature bits (EM | TP)
// are all-clusters v1.4.2.0's; the other values are illustrative. Master's
// all-clusters no longer serves the cluster.

#[test]
fn water_heater_management_attributes_decode() {
    use clusters::water_heater_management::{
        decode_boost_state, decode_estimated_heat_required, decode_heat_demand,
        decode_heater_types, decode_tank_percentage, decode_tank_volume, BoostStateEnum, Feature,
        WaterHeaterHeatSourceBitmap,
    };
    assert_eq!(
        (Feature::EM | Feature::TP).bits(),
        3,
        "all-clusters' features"
    );
    assert_eq!(
        decode_heater_types(&uint_attr(1)).unwrap(),
        WaterHeaterHeatSourceBitmap::IMMERSION_ELEMENT1
    );
    assert_eq!(
        decode_heat_demand(&uint_attr(0)).unwrap(),
        WaterHeaterHeatSourceBitmap::empty()
    );
    // A heat source a later revision adds survives the decode.
    assert_eq!(decode_heat_demand(&uint_attr(0x41)).unwrap().bits(), 0x41);
    assert_eq!(decode_tank_volume(&uint_attr(150)).unwrap(), 150);
    // energy-mWh is a signed 64-bit integer.
    assert_eq!(
        decode_estimated_heat_required(&int_attr(4_000_000)).unwrap(),
        4_000_000
    );
    assert_eq!(decode_tank_percentage(&uint_attr(80)).unwrap(), 80);
    assert_eq!(
        decode_boost_state(&uint_attr(1)).unwrap(),
        BoostStateEnum::Active
    );
}

#[test]
fn water_heater_boost_encodes_and_boost_started_decodes() {
    use clusters::water_heater_management::{
        encode_boost, encode_cancel_boost, event_id, BoostStartedEvent, WaterHeaterBoostInfoStruct,
    };
    // Heat to 80 % of the tank, reheat when it falls to 20 %: OneShot is left
    // out (at most one of OneShot and TargetReheat), TargetPercentage goes
    // with TargetReheat. chip's server refuses either rule's breach with
    // InvalidCommand before its delegate runs (both refs); the encoder does
    // not check them.
    let info = WaterHeaterBoostInfoStruct {
        duration: 60,
        one_shot: None,
        emergency_boost: None,
        temporary_setpoint: None,
        target_percentage: Some(80),
        target_reheat: Some(20),
    };
    let boost_info = |w: &mut TlvWriter<'_>| {
        w.start_structure(Tag::Context(0)).unwrap();
        w.put_uint(Tag::Context(0), 60).unwrap();
        w.put_uint(Tag::Context(4), 80).unwrap();
        w.put_uint(Tag::Context(5), 20).unwrap();
        w.end_container().unwrap();
    };
    assert_eq!(encode_boost(info.clone()), struct_of(&boost_info));
    assert_eq!(encode_cancel_boost(), [0x15, 0x18]);
    // BoostStarted echoes the request's BoostInfo (chip's
    // GenerateBoostStartedEvent copies every field it was given).
    let e = BoostStartedEvent::decode(&struct_of(&boost_info)).unwrap();
    assert_eq!(e.boost_info, info);
    // A one-shot emergency boost to a temporary 65.00 °C setpoint.
    let e = BoostStartedEvent::decode(&struct_of(&|w| {
        w.start_structure(Tag::Context(0)).unwrap();
        w.put_uint(Tag::Context(0), 3600).unwrap();
        w.put_bool(Tag::Context(1), true).unwrap();
        w.put_bool(Tag::Context(2), true).unwrap();
        w.put_int(Tag::Context(3), 6500).unwrap();
        w.end_container().unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            e.boost_info.one_shot,
            e.boost_info.emergency_boost,
            e.boost_info.temporary_setpoint,
            e.boost_info.target_percentage
        ),
        (Some(true), Some(true), Some(6500), None)
    );
    // BoostEnded carries nothing: an id only.
    assert_eq!(
        [event_id::BOOST_STARTED, event_id::BOOST_ENDED],
        [0x00, 0x01]
    );
}

// ---- M9-A3 B5: DeviceEnergyManagement --------------------------------------
//
// 1.4.2 DeviceEnergyManagement.xml. Forecast is a nullable ForecastStruct
// whose Slots each may carry a list of CostStruct; PowerAdjustmentCapability
// is a nullable struct whose own list is nullable too. evse-app sets PA, PFR,
// STA, PAU, FA and CON (master evse-app/linux/main.cpp; v1.4.2.0 has no
// evse-app, and energy-management-app/linux/main.cpp sets the same six);
// v1.4.2.0's all-clusters sets PA, PFR, SFR, STA and PAU
// (device-energy-management-stub.cpp, absent at master).

#[test]
fn device_energy_management_attributes_and_capability_decode() {
    use clusters::device_energy_management::{
        decode_abs_max_power, decode_abs_min_power, decode_esa_can_generate, decode_esa_state,
        decode_esa_type, decode_opt_out_state, decode_power_adjustment_capability, ESAStateEnum,
        ESATypeEnum, Feature, OptOutStateEnum, PowerAdjustReasonEnum,
    };
    assert_eq!(
        (Feature::PA | Feature::PFR | Feature::STA | Feature::PAU | Feature::FA | Feature::CON)
            .bits(),
        0x7B,
        "evse-app (master) / energy-management-app (v1.4.2.0) features"
    );
    assert_eq!(decode_esa_type(&uint_attr(0)).unwrap(), ESATypeEnum::Evse);
    assert_eq!(
        decode_esa_type(&uint_attr(255)).unwrap(),
        ESATypeEnum::Other
    );
    assert!(!decode_esa_can_generate(&bool_attr(false)).unwrap());
    assert_eq!(
        decode_esa_state(&uint_attr(3)).unwrap(),
        ESAStateEnum::PowerAdjustActive
    );
    // power-mW is a signed 64-bit integer (evse-app at master and
    // energy-management-app at v1.4.2.0: 1.2 kW to 7.6 kW).
    assert_eq!(
        decode_abs_min_power(&int_attr(1_200_000)).unwrap(),
        1_200_000
    );
    assert_eq!(
        decode_abs_max_power(&int_attr(7_600_000)).unwrap(),
        7_600_000
    );
    assert_eq!(
        decode_opt_out_state(&uint_attr(2)).unwrap(),
        OptOutStateEnum::GridOptOut
    );
    // Null until the device can adjust; then a list (itself nullable) of
    // ranges and a reason.
    assert_eq!(
        decode_power_adjustment_capability(&null_attr()).unwrap(),
        Nullable::Null
    );
    let cap = decode_power_adjustment_capability(&struct_of(&|w| {
        w.start_array(Tag::Context(0)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_int(Tag::Context(0), 1_200_000).unwrap();
        w.put_int(Tag::Context(1), 7_600_000).unwrap();
        w.put_uint(Tag::Context(2), 10).unwrap();
        w.put_uint(Tag::Context(3), 3600).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(1), 1).unwrap();
    }))
    .unwrap();
    let Nullable::Value(cap) = cap else {
        panic!("expected a capability")
    };
    assert_eq!(
        cap.cause,
        PowerAdjustReasonEnum::LocalOptimizationAdjustment
    );
    let Nullable::Value(ranges) = &cap.power_adjust_capability else {
        panic!("expected a list")
    };
    assert_eq!(
        (
            ranges[0].min_power,
            ranges[0].max_power,
            ranges[0].max_duration
        ),
        (1_200_000, 7_600_000, 3600)
    );
    let cap = decode_power_adjustment_capability(&struct_of(&|w| {
        w.put_null(Tag::Context(0)).unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
    }))
    .unwrap();
    let Nullable::Value(cap) = cap else {
        panic!("expected a capability")
    };
    assert_eq!(cap.power_adjust_capability, Nullable::Null);
}

#[test]
fn device_energy_management_forecast_decodes_nested_slots_and_costs() {
    use clusters::device_energy_management::{
        decode_forecast, CostTypeEnum, ForecastUpdateReasonEnum,
    };
    // Two slots: the first pausable with a financial cost, the second with
    // neither PAU nor cost fields.
    let forecast = decode_forecast(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 7).unwrap(); // ForecastId
        w.put_uint(Tag::Context(1), 0).unwrap(); // ActiveSlotNumber
        w.put_uint(Tag::Context(2), 1000).unwrap(); // StartTime
        w.put_uint(Tag::Context(3), 4600).unwrap(); // EndTime
        w.put_null(Tag::Context(4)).unwrap(); // EarliestStartTime (STA)
        w.put_uint(Tag::Context(5), 9000).unwrap(); // LatestEndTime (STA)
        w.put_bool(Tag::Context(6), true).unwrap(); // IsPausable
        w.start_array(Tag::Context(7)).unwrap(); // Slots
        w.start_structure(Tag::Anonymous).unwrap();
        for (tag, v) in [(0, 600), (1, 1200), (2, 900), (3, 0), (4, 900)] {
            w.put_uint(Tag::Context(tag), v).unwrap();
        }
        w.put_bool(Tag::Context(5), true).unwrap(); // SlotIsPausable
        w.put_uint(Tag::Context(6), 60).unwrap();
        w.put_uint(Tag::Context(7), 600).unwrap();
        w.put_int(Tag::Context(9), 2_500_000).unwrap(); // NominalPower (PFR)
        w.start_array(Tag::Context(13)).unwrap(); // Costs
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 0).unwrap(); // Financial
        w.put_int(Tag::Context(1), -150).unwrap(); // a credit
        w.put_uint(Tag::Context(2), 2).unwrap();
        w.put_uint(Tag::Context(3), 826).unwrap(); // GBP
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        for (tag, v) in [(0, 300), (1, 300), (2, 300), (3, 0), (4, 300)] {
            w.put_uint(Tag::Context(tag), v).unwrap();
        }
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(8), 1).unwrap(); // ForecastUpdateReason
    }))
    .unwrap();
    let Nullable::Value(f) = forecast else {
        panic!("expected a forecast")
    };
    assert_eq!(
        (
            f.forecast_id,
            f.active_slot_number,
            f.earliest_start_time,
            f.latest_end_time
        ),
        (7, Nullable::Value(0), Some(Nullable::Null), Some(9000))
    );
    assert_eq!(
        f.forecast_update_reason,
        ForecastUpdateReasonEnum::LocalOptimization
    );
    assert_eq!(f.slots.len(), 2);
    let costs = f.slots[0].costs.as_ref().unwrap();
    assert_eq!(
        (
            costs[0].cost_type,
            costs[0].value,
            costs[0].decimal_points,
            costs[0].currency
        ),
        (CostTypeEnum::Financial, -150, 2, Some(826))
    );
    assert_eq!(f.slots[0].nominal_power, Some(2_500_000));
    assert_eq!(f.slots[0].slot_is_pausable, Some(true));
    assert_eq!(
        (
            f.slots[1].costs.as_ref(),
            f.slots[1].slot_is_pausable,
            f.slots[1].manufacturer_esa_state
        ),
        (None, None, None)
    );
    assert_eq!(decode_forecast(&null_attr()).unwrap(), Nullable::Null);
}

#[test]
fn device_energy_management_requests_encode_and_events_decode() {
    use clusters::device_energy_management::{
        encode_cancel_request, encode_modify_forecast_request, encode_pause_request,
        encode_power_adjust_request, encode_request_constraint_based_forecast, event_id,
        AdjustmentCauseEnum, CauseEnum, ConstraintsStruct, PowerAdjustEndEvent, ResumedEvent,
        SlotAdjustmentStruct,
    };
    assert_eq!(
        encode_power_adjust_request(-3_000_000, 600, AdjustmentCauseEnum::GridOptimization),
        struct_of(&|w| {
            w.put_int(Tag::Context(0), -3_000_000).unwrap();
            w.put_uint(Tag::Context(1), 600).unwrap();
            w.put_uint(Tag::Context(2), 1).unwrap();
        })
    );
    assert_eq!(
        encode_pause_request(60, AdjustmentCauseEnum::LocalOptimization),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 60).unwrap();
            w.put_uint(Tag::Context(1), 0).unwrap();
        })
    );
    assert_eq!(encode_cancel_request(), [0x15, 0x18]);
    // Without PFR a slot adjustment leaves NominalPower out.
    let adjust = vec![SlotAdjustmentStruct {
        slot_index: 0,
        nominal_power: None,
        duration: 1200,
    }];
    assert_eq!(
        encode_modify_forecast_request(7, &adjust, AdjustmentCauseEnum::LocalOptimization),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 7).unwrap();
            w.start_array(Tag::Context(1)).unwrap();
            w.start_structure(Tag::Anonymous).unwrap();
            w.put_uint(Tag::Context(0), 0).unwrap();
            w.put_uint(Tag::Context(2), 1200).unwrap();
            w.end_container().unwrap();
            w.end_container().unwrap();
            w.put_uint(Tag::Context(2), 0).unwrap();
        })
    );
    let constraints = vec![ConstraintsStruct {
        start_time: 5000,
        duration: 600,
        nominal_power: Some(1_000_000),
        maximum_energy: Some(200_000),
        load_control: None,
    }];
    assert_eq!(
        encode_request_constraint_based_forecast(
            &constraints,
            AdjustmentCauseEnum::GridOptimization
        ),
        struct_of(&|w| {
            w.start_array(Tag::Context(0)).unwrap();
            w.start_structure(Tag::Anonymous).unwrap();
            w.put_uint(Tag::Context(0), 5000).unwrap();
            w.put_uint(Tag::Context(1), 600).unwrap();
            w.put_int(Tag::Context(2), 1_000_000).unwrap();
            w.put_int(Tag::Context(3), 200_000).unwrap();
            w.end_container().unwrap();
            w.end_container().unwrap();
            w.put_uint(Tag::Context(1), 1).unwrap();
        })
    );
    let end = PowerAdjustEndEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 4).unwrap();
        w.put_uint(Tag::Context(1), 42).unwrap();
        w.put_int(Tag::Context(2), 1_500).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (end.cause, end.duration, end.energy_use),
        (CauseEnum::Cancelled, 42, 1_500)
    );
    let resumed =
        ResumedEvent::decode(&struct_of(&|w| w.put_uint(Tag::Context(0), 0).unwrap())).unwrap();
    assert_eq!(resumed.cause, CauseEnum::NormalCompletion);
    // PowerAdjustStart and Paused carry nothing: ids only.
    assert_eq!(
        [
            event_id::POWER_ADJUST_START,
            event_id::POWER_ADJUST_END,
            event_id::PAUSED,
            event_id::RESUMED
        ],
        [0x00, 0x01, 0x02, 0x03]
    );
}

// ---- M9-A3 B5: EnergyEvse ---------------------------------------------------
//
// 1.4.2 EnergyEVSE.xml (cluster revision 3; the 1.5.1 model is revision 4,
// with the same elements). Master's evse-app sets all five features (PREF,
// SOC, PNC, RFID, V2X; evse-common/src/EnergyEvseMain.cpp) and serves every
// attribute. evse-app does not exist at v1.4.2.0: there the EVSE lives in
// energy-management-app (energy-evse/src/EnergyEvseMain.cpp), which sets
// PREF and RFID only, as does all-clusters (energy-evse-stub.cpp).

#[test]
fn energy_evse_attributes_decode() {
    use clusters::energy_evse::{
        decode_approximate_ev_efficiency, decode_battery_capacity, decode_charging_enabled_until,
        decode_circuit_capacity, decode_fault_state, decode_session_duration, decode_session_id,
        decode_state, decode_state_of_charge, decode_supply_state, decode_vehicle_id,
        encode_approximate_ev_efficiency, encode_randomization_delay_window,
        encode_user_maximum_charge_current, FaultStateEnum, Feature, StateEnum, SupplyStateEnum,
    };
    assert_eq!(Feature::all().bits(), 0x1F, "master evse-app's features");
    assert_eq!(
        (Feature::PREF | Feature::RFID).bits(),
        0x09,
        "v1.4.2.0 energy-management-app's and all-clusters' features"
    );
    assert_eq!(decode_state(&null_attr()).unwrap(), Nullable::Null);
    assert_eq!(
        decode_state(&uint_attr(3)).unwrap(),
        Nullable::Value(StateEnum::PluggedInCharging)
    );
    assert_eq!(
        decode_supply_state(&uint_attr(4)).unwrap(),
        SupplyStateEnum::DisabledDiagnostics
    );
    assert_eq!(
        decode_fault_state(&uint_attr(255)).unwrap(),
        FaultStateEnum::Other
    );
    assert_eq!(
        decode_fault_state(&uint_attr(7)).unwrap(),
        FaultStateEnum::GroundFault
    );
    assert_eq!(
        decode_charging_enabled_until(&null_attr()).unwrap(),
        Nullable::Null
    );
    // amperage-mA is a signed 64-bit integer (the basic-functionality trigger
    // sets 32 A, in master's evse-app and v1.4.2.0's energy-management-app).
    assert_eq!(decode_circuit_capacity(&int_attr(32_000)).unwrap(), 32_000);
    assert_eq!(
        decode_state_of_charge(&uint_attr(20)).unwrap(),
        Nullable::Value(20)
    );
    assert_eq!(
        decode_battery_capacity(&int_attr(70_000_000)).unwrap(),
        Nullable::Value(70_000_000)
    );
    // Master evse-app's kEVSESetVehicleID trigger (not in v1.4.2.0).
    assert_eq!(
        decode_vehicle_id(&str_attr("Test-Vehicle-ID-012345789-ABCDEF")).unwrap(),
        Nullable::Value("Test-Vehicle-ID-012345789-ABCDEF".to_string())
    );
    assert_eq!(decode_session_id(&null_attr()).unwrap(), Nullable::Null);
    assert_eq!(
        decode_session_duration(&uint_attr(90)).unwrap(),
        Nullable::Value(90)
    );
    assert_eq!(encode_user_maximum_charge_current(32_000), int_attr(32_000));
    assert_eq!(encode_randomization_delay_window(600), uint_attr(600));
    assert_eq!(
        encode_approximate_ev_efficiency(Nullable::Null),
        null_attr()
    );
    assert_eq!(
        decode_approximate_ev_efficiency(&encode_approximate_ev_efficiency(Nullable::Value(3500)))
            .unwrap(),
        Nullable::Value(3500)
    );
}

#[test]
fn energy_evse_commands_encode_and_targets_decode() {
    use clusters::energy_evse::{
        encode_disable, encode_enable_charging, encode_enable_discharging, encode_set_targets,
        ChargingTargetScheduleStruct, ChargingTargetStruct, GetTargetsResponse,
        TargetDayOfWeekBitmap,
    };
    assert_eq!(encode_disable(), [0x15, 0x18]);
    // Charge until further notice, between 6 A and 32 A.
    assert_eq!(
        encode_enable_charging(Nullable::Null, 6_000, 32_000),
        struct_of(&|w| {
            w.put_null(Tag::Context(0)).unwrap();
            w.put_int(Tag::Context(1), 6_000).unwrap();
            w.put_int(Tag::Context(2), 32_000).unwrap();
        })
    );
    assert_eq!(
        encode_enable_discharging(Nullable::Value(1_000), 16_000),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 1_000).unwrap();
            w.put_int(Tag::Context(1), 16_000).unwrap();
        })
    );
    // Weekdays: 80 % by 07:30; Saturday: 25 kWh by 10:00. Each target carries
    // one of TargetSoC / AddedEnergy (choice group a: at least one). These
    // vectors are wire-only: chip's server applies its SOC rule (with SOC,
    // TargetSoC is required; without SOC, it may only be absent or 100), so
    // it would refuse them live (the Saturday target with SOC, the weekday
    // one without).
    let schedules = vec![
        ChargingTargetScheduleStruct {
            day_of_week_for_sequence: TargetDayOfWeekBitmap::MONDAY
                | TargetDayOfWeekBitmap::TUESDAY
                | TargetDayOfWeekBitmap::WEDNESDAY
                | TargetDayOfWeekBitmap::THURSDAY
                | TargetDayOfWeekBitmap::FRIDAY,
            charging_targets: vec![ChargingTargetStruct {
                target_time_minutes_past_midnight: 450,
                target_soc: Some(80),
                added_energy: None,
            }],
        },
        ChargingTargetScheduleStruct {
            day_of_week_for_sequence: TargetDayOfWeekBitmap::SATURDAY,
            charging_targets: vec![ChargingTargetStruct {
                target_time_minutes_past_midnight: 600,
                target_soc: None,
                added_energy: Some(25_000_000),
            }],
        },
    ];
    let wire = struct_of(&|w| {
        w.start_array(Tag::Context(0)).unwrap();
        for (days, time, soc, energy) in [
            (0x3E, 450, Some(80), None),
            (0x40, 600, None, Some(25_000_000)),
        ] {
            w.start_structure(Tag::Anonymous).unwrap();
            w.put_uint(Tag::Context(0), days).unwrap();
            w.start_array(Tag::Context(1)).unwrap();
            w.start_structure(Tag::Anonymous).unwrap();
            w.put_uint(Tag::Context(0), time).unwrap();
            if let Some(soc) = soc {
                w.put_uint(Tag::Context(1), soc).unwrap();
            }
            if let Some(energy) = energy {
                w.put_int(Tag::Context(2), energy).unwrap();
            }
            w.end_container().unwrap();
            w.end_container().unwrap();
            w.end_container().unwrap();
        }
        w.end_container().unwrap();
    });
    assert_eq!(encode_set_targets(&schedules), wire);
    // GetTargetsResponse carries the same list back.
    assert_eq!(
        GetTargetsResponse::decode(&wire)
            .unwrap()
            .charging_target_schedules,
        schedules
    );
}

#[test]
fn energy_evse_events_decode() {
    use clusters::energy_evse::{
        event_id, EnergyTransferStartedEvent, EnergyTransferStoppedEvent,
        EnergyTransferStoppedReasonEnum, EvConnectedEvent, EvNotDetectedEvent, FaultEvent,
        FaultStateEnum, RfidEvent, StateEnum,
    };
    let connected =
        EvConnectedEvent::decode(&struct_of(&|w| w.put_uint(Tag::Context(0), 3).unwrap())).unwrap();
    assert_eq!(connected.session_id, 3);
    // Without V2X the discharge fields are left out.
    let gone = EvNotDetectedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 3).unwrap();
        w.put_uint(Tag::Context(1), 1).unwrap();
        w.put_uint(Tag::Context(2), 120).unwrap();
        w.put_int(Tag::Context(3), 0).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            gone.state,
            gone.session_duration,
            gone.session_energy_discharged
        ),
        (StateEnum::PluggedInNoDemand, 120, None)
    );
    let started = EnergyTransferStartedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 3).unwrap();
        w.put_uint(Tag::Context(1), 3).unwrap();
        w.put_int(Tag::Context(2), 32_000).unwrap();
        w.put_int(Tag::Context(3), 0).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (started.maximum_current, started.maximum_discharge_current),
        (32_000, Some(0))
    );
    // Field 3 is reserved: EnergyTransferStopped's tags are 0, 1, 2, 4, 5.
    let stopped = EnergyTransferStoppedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 3).unwrap();
        w.put_uint(Tag::Context(1), 2).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
        w.put_int(Tag::Context(4), 1_500).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            stopped.reason,
            stopped.energy_transferred,
            stopped.energy_discharged
        ),
        (EnergyTransferStoppedReasonEnum::EvseStopped, 1_500, None)
    );
    // A fault with no session in progress: SessionID is null.
    let fault = FaultEvent::decode(&struct_of(&|w| {
        w.put_null(Tag::Context(0)).unwrap();
        w.put_uint(Tag::Context(1), 6).unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap();
        w.put_uint(Tag::Context(4), 7).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            fault.session_id,
            fault.state,
            fault.fault_state_previous_state,
            fault.fault_state_current_state
        ),
        (
            Nullable::Null,
            StateEnum::Fault,
            FaultStateEnum::NoError,
            FaultStateEnum::GroundFault
        )
    );
    // Master evse-app's kEVSETriggerRFID tag (not in v1.4.2.0).
    let uid = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99];
    let rfid =
        RfidEvent::decode(&struct_of(&|w| w.put_bytes(Tag::Context(0), &uid).unwrap())).unwrap();
    assert_eq!(rfid.uid, uid);
    assert_eq!(
        [
            event_id::EV_CONNECTED,
            event_id::EV_NOT_DETECTED,
            event_id::ENERGY_TRANSFER_STARTED,
            event_id::ENERGY_TRANSFER_STOPPED,
            event_id::FAULT,
            event_id::RFID
        ],
        [0x00, 0x01, 0x02, 0x03, 0x04, 0x05]
    );
}

// ---- M9-A3 B5: Messages -----------------------------------------------------
//
// 1.4.2 Messages.xml makes the Messages attribute fabric-scoped: on our
// unfiltered read, another fabric's entry carries only FabricIndex. chip does
// not fabric-scope MessageStruct or the three events (zap messages-cluster.xml
// "TODO: enable isFabricScoped"; no field 254 in its generated types, v1.4.2.0
// and master): tv-app sends every field of every entry and no FabricIndex. All
// three shapes must decode.

/// One `MessageStruct` entry's fields, as chip's tv-app encodes them: no `FabricIndex`.
fn message_fields(w: &mut TlvWriter<'_>) {
    w.put_bytes(Tag::Context(0), &[0x11; 16]).unwrap();
    w.put_uint(Tag::Context(1), 3).unwrap(); // Critical
    w.put_uint(Tag::Context(2), 0x03).unwrap();
    w.put_null(Tag::Context(3)).unwrap();
    w.put_uint(Tag::Context(4), 60_000).unwrap();
    w.put_utf8(Tag::Context(5), "Storm warning").unwrap();
    w.start_array(Tag::Context(6)).unwrap();
    w.start_structure(Tag::Anonymous).unwrap();
    w.put_uint(Tag::Context(0), 1).unwrap();
    w.put_utf8(Tag::Context(1), "OK").unwrap();
    w.end_container().unwrap();
    w.end_container().unwrap();
}

#[test]
fn messages_list_decodes_chip_and_spec_shapes() {
    use clusters::messages::{decode_messages, MessageControlBitmap, MessagePriorityEnum};
    let list = decode_messages(&list_of(&[
        // chip's tv-app: every field, no FabricIndex.
        &message_fields,
        // The spec's shape for our own fabric: every field plus FabricIndex.
        &|w| {
            message_fields(w);
            w.put_uint(Tag::Context(254), 1).unwrap();
        },
        // The spec's shape for another fabric: FabricIndex only.
        &|w| w.put_uint(Tag::Context(254), 2).unwrap(),
    ]))
    .unwrap();
    assert_eq!(list.len(), 3);
    let chip = &list[0];
    assert_eq!(chip.message_id.as_deref(), Some(&[0x11; 16][..]));
    assert_eq!(chip.priority, Some(MessagePriorityEnum::Critical));
    assert_eq!(
        chip.message_control,
        Some(MessageControlBitmap::CONFIRMATION_REQUIRED | MessageControlBitmap::RESPONSE_REQUIRED)
    );
    assert_eq!(
        (chip.start_time, chip.duration),
        (Some(Nullable::Null), Some(Nullable::Value(60_000)))
    );
    assert_eq!(chip.message_text.as_deref(), Some("Storm warning"));
    let responses = chip.responses.as_ref().unwrap();
    assert_eq!(
        (
            responses[0].message_response_id,
            responses[0].label.as_str()
        ),
        (1, "OK")
    );
    assert_eq!(chip.fabric_index, None);
    assert_eq!(list[1].fabric_index, Some(1));
    assert_eq!(list[1].message_text, chip.message_text);
    let other = &list[2];
    assert_eq!(other.fabric_index, Some(2));
    assert_eq!(
        (
            &other.message_id,
            other.priority,
            other.message_control,
            other.start_time,
            other.duration,
            &other.message_text,
            &other.responses
        ),
        (&None, None, None, None, None, &None, &None)
    );
}

#[test]
fn messages_ids_and_events_decode() {
    use clusters::messages::{
        decode_active_message_ids, event_id, FutureMessagePreferenceEnum, MessageCompleteEvent,
        MessagePresentedEvent, MessageQueuedEvent,
    };
    let ids = {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_array(Tag::Anonymous).unwrap();
        w.put_bytes(Tag::Anonymous, &[0x11; 16]).unwrap();
        w.put_bytes(Tag::Anonymous, &[0x22; 16]).unwrap();
        w.end_container().unwrap();
        buf
    };
    assert_eq!(
        decode_active_message_ids(&ids).unwrap(),
        [vec![0x11; 16], vec![0x22; 16]]
    );
    // With FabricIndex (the spec) and without it (chip's generated events).
    let queued = MessageQueuedEvent::decode(&struct_of(&|w| {
        w.put_bytes(Tag::Context(0), &[0x11; 16]).unwrap();
        w.put_uint(Tag::Context(254), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (queued.message_id, queued.fabric_index),
        (vec![0x11; 16], Some(1))
    );
    let presented = MessagePresentedEvent::decode(&struct_of(&|w| {
        w.put_bytes(Tag::Context(0), &[0x11; 16]).unwrap();
    }))
    .unwrap();
    assert_eq!(presented.fabric_index, None);
    // Without RESP / RPLY, ResponseId and Reply are left out; the
    // preference is mandatory but nullable.
    let complete = MessageCompleteEvent::decode(&struct_of(&|w| {
        w.put_bytes(Tag::Context(0), &[0x11; 16]).unwrap();
        w.put_null(Tag::Context(3)).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            complete.response_id,
            complete.reply,
            complete.future_messages_preference
        ),
        (None, None, Nullable::Null)
    );
    let complete = MessageCompleteEvent::decode(&struct_of(&|w| {
        w.put_bytes(Tag::Context(0), &[0x11; 16]).unwrap();
        w.put_uint(Tag::Context(1), 1).unwrap();
        w.put_utf8(Tag::Context(2), "On my way").unwrap();
        w.put_uint(Tag::Context(3), 2).unwrap();
        w.put_uint(Tag::Context(254), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            complete.response_id,
            complete.reply.clone(),
            complete.future_messages_preference
        ),
        (
            Some(Nullable::Value(1)),
            Some(Nullable::Value("On my way".to_string())),
            Nullable::Value(FutureMessagePreferenceEnum::Reduced)
        )
    );
    assert_eq!(
        [
            event_id::MESSAGE_QUEUED,
            event_id::MESSAGE_PRESENTED,
            event_id::MESSAGE_COMPLETE
        ],
        [0x00, 0x01, 0x02]
    );
}

#[test]
fn present_messages_request_without_responses_omits_tag_6() {
    // Responses is optional (RESP-gated): `None` leaves tag 6 out entirely,
    // rather than sending an empty list. The matter.js vector
    // (byte_parity) covers the `Some` case.
    use clusters::messages::{
        encode_present_messages_request, MessageControlBitmap, MessagePriorityEnum,
    };
    let got = encode_present_messages_request(
        &vec![0x22; 16],
        MessagePriorityEnum::Low,
        MessageControlBitmap::empty(),
        Nullable::Null,
        Nullable::Value(60_000),
        &"Hello".to_string(),
        None,
    );
    assert_eq!(
        got,
        struct_of(&|w| {
            w.put_bytes(Tag::Context(0), &[0x22; 16]).unwrap();
            w.put_uint(Tag::Context(1), 0).unwrap(); // Low
            w.put_uint(Tag::Context(2), 0).unwrap();
            w.put_null(Tag::Context(3)).unwrap();
            w.put_uint(Tag::Context(4), 60_000).unwrap();
            w.put_utf8(Tag::Context(5), "Hello").unwrap();
        })
    );
}

// ---- M9-A3 B6: localization and power-source configuration ----------------
//
// Attributes only (1.4.2 LocalizationConfiguration.xml,
// LocalizationTimeFormat.xml, LocalizationUnit.xml,
// PowerSourceConfigurationCluster.xml). Values from all-clusters, the same at
// v1.4.2.0 and master: the locales are 3 of the 8 its device-info provider
// serves (AllClustersExampleDeviceInfoProviderImpl.cpp), Gregorian (4) is its
// only calendar, [Fahrenheit, Celsius] are linux/main-common.cpp's
// temperature units, and Sources [2, 1, 0] lists the endpoints that carry
// PowerSource, sorted by their Order (chip's
// power-source-configuration-server.cpp). The rest is synthetic: the
// calendar list's 0xFF and 42 entries, the Japanese calendar, the hour
// formats and Kelvin.

/// An anonymous array of anonymous UTF-8 strings (a `list<string>` value).
fn str_array_attr(values: &[&str]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.start_array(Tag::Anonymous).unwrap();
        for v in values {
            w.put_utf8(Tag::Anonymous, v).unwrap();
        }
        w.end_container().unwrap();
    }
    buf
}

#[test]
fn localization_configuration_attributes_decode() {
    use clusters::localization_configuration::{
        decode_active_locale, decode_supported_locales, encode_active_locale,
    };
    assert_eq!(
        decode_supported_locales(&str_array_attr(&["en-US", "de-DE", "ja-JP"])).unwrap(),
        ["en-US", "de-DE", "ja-JP"]
    );
    assert_eq!(decode_active_locale(&str_attr("en-US")).unwrap(), "en-US");
    assert_eq!(
        encode_active_locale(&"de-DE".to_string()),
        str_attr("de-DE")
    );
    // A locale is a string; an integer is refused.
    assert!(decode_active_locale(&uint_attr(1)).is_err());
}

#[test]
fn time_format_and_unit_localization_attributes_decode() {
    use clusters::time_format_localization::{
        decode_active_calendar_type, decode_hour_format, decode_supported_calendar_types,
        encode_active_calendar_type, encode_hour_format, CalendarTypeEnum, Feature as TimeFeature,
        HourFormatEnum,
    };
    use clusters::unit_localization::{
        decode_supported_temperature_units, decode_temperature_unit, encode_temperature_unit,
        Feature as UnitFeature, TempUnitEnum,
    };
    assert_eq!(TimeFeature::CALFMT.bits(), 1);
    assert_eq!(UnitFeature::TEMP.bits(), 1);
    // The enum values are 12Hr = 0, 24Hr = 1, UseActiveLocale = 0xFF.
    assert_eq!(
        decode_hour_format(&uint_attr(1)).unwrap(),
        HourFormatEnum::Hr24
    );
    assert_eq!(
        decode_hour_format(&uint_attr(0xFF)).unwrap(),
        HourFormatEnum::UseActiveLocale
    );
    assert_eq!(encode_hour_format(HourFormatEnum::Hr12), uint_attr(0));
    assert_eq!(
        decode_supported_calendar_types(&uint_array_attr(&[4, 0xFF, 42])).unwrap(),
        [
            CalendarTypeEnum::Gregorian,
            CalendarTypeEnum::UseActiveLocale,
            CalendarTypeEnum::Unknown(42)
        ]
    );
    assert_eq!(
        decode_active_calendar_type(&encode_active_calendar_type(CalendarTypeEnum::Japanese))
            .unwrap(),
        CalendarTypeEnum::Japanese
    );
    assert_eq!(
        decode_supported_temperature_units(&uint_array_attr(&[0, 1])).unwrap(),
        [TempUnitEnum::Fahrenheit, TempUnitEnum::Celsius]
    );
    assert_eq!(
        decode_temperature_unit(&uint_attr(2)).unwrap(),
        TempUnitEnum::Kelvin
    );
    assert_eq!(encode_temperature_unit(TempUnitEnum::Celsius), uint_attr(1));
}

#[test]
fn power_source_configuration_sources_decode() {
    use clusters::power_source_configuration::{attribute_id, decode_sources, CLUSTER_ID};
    assert_eq!((CLUSTER_ID, attribute_id::SOURCES), (0x002E, 0x0000));
    // Endpoints in source order (all-clusters: Order 1 on endpoint 2, 2 on
    // endpoint 1, 3 on endpoint 0).
    assert_eq!(
        decode_sources(&uint_array_attr(&[2, 1, 0])).unwrap(),
        [2, 1, 0]
    );
    assert_eq!(
        decode_sources(&uint_array_attr(&[])).unwrap(),
        Vec::<u16>::new()
    );
}

// ---- M9-A3 B6: flat diagnostics and DiagnosticLogs -------------------------
//
// 1.4.2 DiagnosticsSoftware.xml, DiagnosticsWiFi.xml, DiagnosticsEthernet.xml,
// DiagnosticLogsCluster.xml. The trigger event payloads are the ones chip's
// all-clusters test-event triggers emit (software-diagnostics-stub.cpp,
// wifi-diagnostics-stub.cpp; the same values at v1.4.2.0 and master): the
// Wi-Fi values are the stub's own; the SoftwareFault process id and time
// are an example run's. The bare SoftwareFault and the Unknown-cause
// AssociationFailure are synthetic.

/// Encode a single anonymous-tagged octet string (an `octstr` value).
fn bytes_attr(v: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    TlvWriter::new(&mut buf)
        .put_bytes(Tag::Anonymous, v)
        .unwrap();
    buf
}

#[test]
fn software_diagnostics_attributes_and_fault_event_decode() {
    use clusters::software_diagnostics::{
        decode_current_heap_high_watermark, decode_thread_metrics, encode_reset_watermarks,
        event_id, Feature, SoftwareFaultEvent,
    };
    use matter_clusters::error::ClusterError;
    assert_eq!(Feature::WTRMRK.bits(), 1);
    // A thread with every field, and one with only its mandatory Id.
    let metrics = decode_thread_metrics(&list_of(&[
        &|w| {
            w.put_uint(Tag::Context(0), 7).unwrap();
            w.put_utf8(Tag::Context(1), "chip").unwrap();
            w.put_uint(Tag::Context(2), 1024).unwrap();
            w.put_uint(Tag::Context(3), 512).unwrap();
            w.put_uint(Tag::Context(4), 8192).unwrap();
        },
        &|w| w.put_uint(Tag::Context(0), 8).unwrap(),
    ]))
    .unwrap();
    assert_eq!(
        (
            metrics[0].id,
            metrics[0].name.as_deref(),
            metrics[0].stack_free_current,
            metrics[0].stack_free_minimum,
            metrics[0].stack_size
        ),
        (7, Some("chip"), Some(1024), Some(512), Some(8192))
    );
    assert_eq!((metrics[1].id, metrics[1].name.as_deref()), (8, None));
    assert!(matches!(
        decode_thread_metrics(&list_of(&[&|w| w
            .put_utf8(Tag::Context(1), "no id")
            .unwrap()])),
        Err(ClusterError::MissingField("Id"))
    ));
    assert_eq!(
        decode_current_heap_high_watermark(&uint_attr(4096)).unwrap(),
        4096
    );
    // ResetWatermarks carries no fields.
    assert_eq!(encode_reset_watermarks(), struct_of(&|_| {}));
    // all-clusters' trigger: the process id as Id and as Name, the time as
    // the recording.
    let fault = SoftwareFaultEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 84977).unwrap();
        w.put_utf8(Tag::Context(1), "84977").unwrap();
        w.put_bytes(Tag::Context(2), b"Sun Oct 11 10:46:21 2026")
            .unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            fault.id,
            fault.name.as_deref(),
            fault.fault_recording.as_deref()
        ),
        (84977, Some("84977"), Some(&b"Sun Oct 11 10:46:21 2026"[..]))
    );
    let bare = SoftwareFaultEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
    }))
    .unwrap();
    assert_eq!((bare.name, bare.fault_recording), (None, None));
    assert_eq!(event_id::SOFTWARE_FAULT, 0x00);
}

#[test]
fn wifi_network_diagnostics_attributes_and_events_decode() {
    use clusters::wifi_network_diagnostics::{
        decode_bssid, decode_rssi, decode_security_type, decode_wifi_version, event_id,
        AssociationFailureCauseEnum, AssociationFailureEvent, ConnectionStatusEnum,
        ConnectionStatusEvent, DisconnectionEvent, Feature, SecurityTypeEnum, WiFiVersionEnum,
    };
    assert_eq!((Feature::PKTCNT | Feature::ERRCNT).bits(), 3);
    // A node not on Wi-Fi serves every attribute null (chip all-clusters on
    // macOS); a connected one serves values.
    assert_eq!(decode_bssid(&null_attr()).unwrap(), Nullable::Null);
    assert_eq!(
        decode_bssid(&bytes_attr(&[0x02, 0x11, 0x22, 0x33, 0x44, 0x55])).unwrap(),
        Nullable::Value(vec![0x02, 0x11, 0x22, 0x33, 0x44, 0x55])
    );
    assert_eq!(
        decode_security_type(&uint_attr(1)).unwrap(),
        Nullable::Value(SecurityTypeEnum::None)
    );
    assert_eq!(
        decode_wifi_version(&uint_attr(5)).unwrap(),
        Nullable::Value(WiFiVersionEnum::Ax)
    );
    assert_eq!(decode_rssi(&int_attr(-61)).unwrap(), Nullable::Value(-61));
    // The three events as all-clusters' triggers emit them.
    let disconnection = DisconnectionEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 3).unwrap();
    }))
    .unwrap();
    assert_eq!(disconnection.reason_code, 3);
    let failure = AssociationFailureEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 2).unwrap();
        w.put_uint(Tag::Context(1), 4).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (failure.association_failure_cause, failure.status),
        (AssociationFailureCauseEnum::AuthenticationFailed, 4)
    );
    // AssociationFailureCauseEnum has a member named Unknown (0), so a value
    // this revision does not know is `Unrecognized`.
    let unknown = AssociationFailureEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 0).unwrap();
    }))
    .unwrap();
    assert_eq!(
        unknown.association_failure_cause,
        AssociationFailureCauseEnum::Unknown
    );
    assert_eq!(
        AssociationFailureCauseEnum::from_raw(9),
        AssociationFailureCauseEnum::Unrecognized(9)
    );
    let status = ConnectionStatusEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(status.connection_status, ConnectionStatusEnum::NotConnected);
    assert_eq!(
        [
            event_id::DISCONNECTION,
            event_id::ASSOCIATION_FAILURE,
            event_id::CONNECTION_STATUS
        ],
        [0x00, 0x01, 0x02]
    );
}

#[test]
fn ethernet_network_diagnostics_attributes_decode() {
    use clusters::ethernet_network_diagnostics::{
        decode_carrier_detect, decode_full_duplex, decode_packet_rx_count, decode_phy_rate,
        encode_reset_counts, Feature, PHYRateEnum,
    };
    assert_eq!((Feature::PKTCNT | Feature::ERRCNT).bits(), 3);
    assert_eq!(decode_phy_rate(&null_attr()).unwrap(), Nullable::Null);
    assert_eq!(
        decode_phy_rate(&uint_attr(2)).unwrap(),
        Nullable::Value(PHYRateEnum::Rate1G)
    );
    // 3 is 2.5 Gbit/s; the model names it Rate25G.
    assert_eq!(PHYRateEnum::from_raw(3), PHYRateEnum::Rate25G);
    assert_eq!(
        decode_full_duplex(&bool_attr(true)).unwrap(),
        Nullable::Value(true)
    );
    assert_eq!(decode_carrier_detect(&null_attr()).unwrap(), Nullable::Null);
    assert_eq!(
        decode_packet_rx_count(&uint_attr(u64::MAX)).unwrap(),
        u64::MAX
    );
    assert_eq!(encode_reset_counts(), struct_of(&|_| {}));
}

#[test]
fn diagnostic_logs_request_and_response_round_trip() {
    use clusters::diagnostic_logs::{
        command_id, encode_retrieve_logs_request, IntentEnum, RetrieveLogsResponse, StatusEnum,
        TransferProtocolEnum,
    };
    use matter_clusters::error::ClusterError;
    assert_eq!(
        (
            command_id::RETRIEVE_LOGS_REQUEST,
            command_id::RETRIEVE_LOGS_RESPONSE
        ),
        (0x00, 0x01)
    );
    // ResponsePayload needs no designator; BDX carries one.
    assert_eq!(
        encode_retrieve_logs_request(
            IntentEnum::EndUserSupport,
            TransferProtocolEnum::ResponsePayload,
            None
        ),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 0).unwrap();
            w.put_uint(Tag::Context(1), 0).unwrap();
        })
    );
    assert_eq!(
        encode_retrieve_logs_request(
            IntentEnum::CrashLogs,
            TransferProtocolEnum::Bdx,
            Some("crash.log".to_string())
        ),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 2).unwrap();
            w.put_uint(Tag::Context(1), 1).unwrap();
            w.put_utf8(Tag::Context(2), "crash.log").unwrap();
        })
    );
    // chip's server with no logs answers NoLogs with an empty LogContent and
    // no timestamps.
    let none = RetrieveLogsResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 2).unwrap();
        w.put_bytes(Tag::Context(1), &[]).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            none.status,
            none.log_content,
            none.utc_time_stamp,
            none.time_since_boot
        ),
        (StatusEnum::NoLogs, vec![], None, None)
    );
    let some = RetrieveLogsResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_bytes(Tag::Context(1), b"log").unwrap();
        w.put_uint(Tag::Context(2), 1_700_000_000_000_000).unwrap();
        w.put_uint(Tag::Context(3), 42).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (some.status, some.utc_time_stamp, some.time_since_boot),
        (StatusEnum::Success, Some(1_700_000_000_000_000), Some(42))
    );
    // LogContent is mandatory.
    assert!(matches!(
        RetrieveLogsResponse::decode(&struct_of(&|w| w.put_uint(Tag::Context(0), 2).unwrap())),
        Err(ClusterError::MissingField("LogContent"))
    ));
}

// ---- M9-A3 B6: ThreadNetworkDiagnostics -------------------------------------
//
// 1.4.2 DiagnosticsThread.xml (cluster revision 3). On a node without Thread
// (all-clusters on macOS: CHIP_DEVICE_CONFIG_ENABLE_THREAD is 0), chip
// master's provider serves the nullable attributes null, the three lists
// empty and every counter 0; at v1.4.2.0 the same build fails those reads
// (its provider returns CHIP_ERROR_NOT_IMPLEMENTED). Only the feature map
// below is all-clusters' own (0x0F at both refs); every other value is
// synthetic, and the table entries are hand-built with every field.

#[test]
fn thread_network_diagnostics_tables_and_structs_decode() {
    use clusters::thread_network_diagnostics::{
        decode_active_network_faults_list, decode_mesh_local_prefix, decode_neighbor_table,
        decode_operational_dataset_components, decode_route_table, decode_routing_role,
        decode_security_policy, Feature, NetworkFaultEnum, RoutingRoleEnum,
    };
    assert_eq!(
        (Feature::PKTCNT | Feature::ERRCNT | Feature::MLECNT | Feature::MACCNT).bits(),
        0x0F,
        "all-clusters' features"
    );
    let neighbors = decode_neighbor_table(&list_of(&[&|w| {
        w.put_uint(Tag::Context(0), 0x1122_3344_5566_7788).unwrap();
        w.put_uint(Tag::Context(1), 12).unwrap();
        w.put_uint(Tag::Context(2), 0x4400).unwrap();
        w.put_uint(Tag::Context(3), 100).unwrap();
        w.put_uint(Tag::Context(4), 200).unwrap();
        w.put_uint(Tag::Context(5), 3).unwrap();
        w.put_int(Tag::Context(6), -60).unwrap();
        w.put_null(Tag::Context(7)).unwrap();
        w.put_uint(Tag::Context(8), 0).unwrap();
        w.put_uint(Tag::Context(9), 1).unwrap();
        w.put_bool(Tag::Context(10), true).unwrap();
        w.put_bool(Tag::Context(11), true).unwrap();
        w.put_bool(Tag::Context(12), false).unwrap();
        w.put_bool(Tag::Context(13), false).unwrap();
    }]))
    .unwrap();
    let n = &neighbors[0];
    assert_eq!(
        (n.ext_address, n.rloc16, n.lqi, n.average_rssi, n.last_rssi),
        (
            0x1122_3344_5566_7788,
            0x4400,
            3,
            Nullable::Value(-60),
            Nullable::Null
        )
    );
    assert!(n.rx_on_when_idle && n.full_thread_device && !n.is_child);
    let routes = decode_route_table(&list_of(&[&|w| {
        w.put_uint(Tag::Context(0), 0x0102_0304_0506_0708).unwrap();
        w.put_uint(Tag::Context(1), 0x0400).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
        w.put_uint(Tag::Context(3), 63).unwrap();
        w.put_uint(Tag::Context(4), 0).unwrap();
        w.put_uint(Tag::Context(5), 3).unwrap();
        w.put_uint(Tag::Context(6), 3).unwrap();
        w.put_uint(Tag::Context(7), 5).unwrap();
        w.put_bool(Tag::Context(8), true).unwrap();
        w.put_bool(Tag::Context(9), true).unwrap();
    }]))
    .unwrap();
    assert_eq!(
        (routes[0].router_id, routes[0].next_hop, routes[0].allocated),
        (1, 63, true)
    );
    // MeshLocalPrefix is an `ipv6pre`: carried as its raw bytes. This is the
    // 9-byte shape chip's OpenThread provider sends (both refs): the prefix
    // length in bits (0x40), then the 8 prefix bytes (here fd00:db8::/64).
    let prefix = [0x40, 0xfd, 0x00, 0x0d, 0xb8, 0x00, 0x00, 0x00, 0x00];
    assert_eq!(
        decode_mesh_local_prefix(&bytes_attr(&prefix)).unwrap(),
        Nullable::Value(prefix.to_vec())
    );
    assert_eq!(
        decode_mesh_local_prefix(&null_attr()).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        decode_routing_role(&uint_attr(6)).unwrap(),
        Nullable::Value(RoutingRoleEnum::Leader)
    );
    let Nullable::Value(policy) = decode_security_policy(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 672).unwrap();
        w.put_uint(Tag::Context(1), 0x00F7).unwrap();
    }))
    .unwrap() else {
        panic!("SecurityPolicy is not null");
    };
    assert_eq!((policy.rotation_time, policy.flags), (672, 0x00F7));
    let Nullable::Value(components) = decode_operational_dataset_components(&struct_of(&|w| {
        for tag in 0..=11 {
            w.put_bool(Tag::Context(tag), tag % 2 == 0).unwrap();
        }
    }))
    .unwrap() else {
        panic!("OperationalDatasetComponents is not null");
    };
    assert!(components.active_timestamp_present && !components.pending_timestamp_present);
    assert!(!components.channel_mask_present);
    assert_eq!(
        decode_active_network_faults_list(&uint_array_attr(&[1, 3])).unwrap(),
        [NetworkFaultEnum::LinkDown, NetworkFaultEnum::NetworkJammed]
    );
    assert_eq!(
        decode_neighbor_table(&uint_array_attr(&[])).unwrap(),
        Vec::new()
    );
}

#[test]
fn thread_network_diagnostics_events_decode() {
    use clusters::thread_network_diagnostics::{
        event_id, ConnectionStatusEnum, ConnectionStatusEvent, NetworkFaultChangeEvent,
        NetworkFaultEnum,
    };
    let status = ConnectionStatusEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
    }))
    .unwrap();
    assert_eq!(status.connection_status, ConnectionStatusEnum::Connected);
    let change = NetworkFaultChangeEvent::decode(&struct_of(&|w| {
        w.start_array(Tag::Context(0)).unwrap();
        w.put_uint(Tag::Anonymous, 1).unwrap();
        w.put_uint(Tag::Anonymous, 2).unwrap();
        w.end_container().unwrap();
        w.start_array(Tag::Context(1)).unwrap();
        w.end_container().unwrap();
    }))
    .unwrap();
    assert_eq!(
        (change.current, change.previous),
        (
            vec![
                NetworkFaultEnum::LinkDown,
                NetworkFaultEnum::HardwareFailure
            ],
            vec![]
        )
    );
    assert_eq!(
        [event_id::CONNECTION_STATUS, event_id::NETWORK_FAULT_CHANGE],
        [0x00, 0x01]
    );
}

// ---- M9-A3 B6: GeneralCommissioning and NetworkCommissioning ---------------
//
// Codecs only: matter-commissioning drives commissioning with its
// own hand-written subset of these commands; its
// tests/generated_codec_parity.rs pins that the two encode identically.
// BasicCommissioningInfo 60 / 900 is what chip all-clusters serves (the
// CHIPDeviceConfig.h defaults), and the one-entry Networks list has the shape
// of its macOS Ethernet driver's (the host's interface name; "en3" is one
// host's). Every other value is synthetic.

#[test]
fn general_commissioning_attributes_and_responses_decode() {
    use clusters::general_commissioning::{
        decode_basic_commissioning_info, decode_breadcrumb, decode_location_capability,
        encode_arm_fail_safe, encode_breadcrumb, encode_set_regulatory_config, ArmFailSafeResponse,
        CommissioningCompleteResponse, CommissioningErrorEnum, Feature, RegulatoryLocationTypeEnum,
        SetRegulatoryConfigResponse,
    };
    use matter_clusters::error::ClusterError;
    assert_eq!((Feature::TC.bits(), Feature::NR.bits()), (1, 2));
    let info = decode_basic_commissioning_info(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 60).unwrap();
        w.put_uint(Tag::Context(1), 900).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            info.fail_safe_expiry_length_seconds,
            info.max_cumulative_failsafe_seconds
        ),
        (60, 900)
    );
    assert_eq!(decode_breadcrumb(&encode_breadcrumb(7)).unwrap(), 7);
    assert_eq!(
        decode_location_capability(&uint_attr(0)).unwrap(),
        RegulatoryLocationTypeEnum::Indoor
    );
    assert_eq!(
        encode_arm_fail_safe(60, 7),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 60).unwrap();
            w.put_uint(Tag::Context(1), 7).unwrap();
        })
    );
    assert_eq!(
        encode_set_regulatory_config(RegulatoryLocationTypeEnum::Outdoor, &"XX".to_string(), 0),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 1).unwrap();
            w.put_utf8(Tag::Context(1), "XX").unwrap();
            w.put_uint(Tag::Context(2), 0).unwrap();
        })
    );
    // chip's server sends DebugText (always empty) in each of these three
    // responses; the "no fail-safe" text below is synthetic.
    let arm = ArmFailSafeResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_utf8(Tag::Context(1), "").unwrap();
    }))
    .unwrap();
    assert_eq!(
        (arm.error_code, arm.debug_text.as_str()),
        (CommissioningErrorEnum::Ok, "")
    );
    let config = SetRegulatoryConfigResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_utf8(Tag::Context(1), "").unwrap();
    }))
    .unwrap();
    assert_eq!(config.error_code, CommissioningErrorEnum::ValueOutsideRange);
    let complete = CommissioningCompleteResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 3).unwrap();
        w.put_utf8(Tag::Context(1), "no fail-safe").unwrap();
    }))
    .unwrap();
    assert_eq!(complete.error_code, CommissioningErrorEnum::NoFailSafe);
    // DebugText is mandatory in the specification (and in chip and matter.js).
    assert!(matches!(
        ArmFailSafeResponse::decode(&struct_of(&|w| w.put_uint(Tag::Context(0), 0).unwrap())),
        Err(ClusterError::MissingField("DebugText"))
    ));
}

#[test]
fn network_commissioning_attributes_requests_and_responses_decode() {
    use clusters::network_commissioning::{
        decode_last_connect_error_value, decode_last_network_id, decode_last_networking_status,
        decode_networks, decode_supported_thread_features, decode_supported_wifi_bands,
        encode_scan_networks, ConnectNetworkResponse, Feature, NetworkCommissioningStatusEnum,
        ScanNetworksResponse, ThreadCapabilitiesBitmap, WiFiBandEnum, WiFiSecurityBitmap,
    };
    assert_eq!(
        (Feature::WI.bits(), Feature::TH.bits(), Feature::ET.bits()),
        (1, 2, 4)
    );
    // all-clusters' Ethernet network: the interface name, not connected.
    let networks = decode_networks(&list_of(&[&|w| {
        w.put_bytes(Tag::Context(0), b"en3").unwrap();
        w.put_bool(Tag::Context(1), false).unwrap();
    }]))
    .unwrap();
    assert_eq!(
        (networks[0].network_id.as_slice(), networks[0].connected),
        (&b"en3"[..], false)
    );
    assert_eq!(
        decode_last_networking_status(&null_attr()).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        decode_last_networking_status(&uint_attr(7)).unwrap(),
        Nullable::Value(NetworkCommissioningStatusEnum::AuthFailure)
    );
    assert_eq!(
        decode_last_network_id(&null_attr()).unwrap(),
        Nullable::Null
    );
    assert_eq!(
        decode_last_connect_error_value(&int_attr(-2)).unwrap(),
        Nullable::Value(-2)
    );
    assert_eq!(
        decode_supported_wifi_bands(&uint_array_attr(&[0, 2])).unwrap(),
        [WiFiBandEnum::Band2G4, WiFiBandEnum::Band5G]
    );
    assert_eq!(
        decode_supported_thread_features(&uint_attr(0b1010)).unwrap(),
        ThreadCapabilitiesBitmap::IS_ROUTER_CAPABLE
            | ThreadCapabilitiesBitmap::IS_FULL_THREAD_DEVICE
    );
    // ScanNetworks' Ssid is optional and nullable: absent, null and a value
    // are three different requests.
    assert_eq!(encode_scan_networks(None, None), struct_of(&|_| {}));
    assert_eq!(
        encode_scan_networks(Some(Nullable::Null), Some(1)),
        struct_of(&|w| {
            w.put_null(Tag::Context(0)).unwrap();
            w.put_uint(Tag::Context(1), 1).unwrap();
        })
    );
    assert_eq!(
        encode_scan_networks(Some(Nullable::Value(b"home".to_vec())), None),
        struct_of(&|w| w.put_bytes(Tag::Context(0), b"home").unwrap())
    );
    let scan = ScanNetworksResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.start_array(Tag::Context(2)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 0b1000).unwrap();
        w.put_bytes(Tag::Context(1), b"home").unwrap();
        w.put_bytes(Tag::Context(2), &[2, 0, 0, 0, 0, 1]).unwrap();
        w.put_uint(Tag::Context(3), 36).unwrap();
        w.put_uint(Tag::Context(4), 2).unwrap();
        w.put_int(Tag::Context(5), -40).unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
    }))
    .unwrap();
    let wifi = &scan.wifi_scan_results.as_ref().unwrap()[0];
    assert_eq!(
        (wifi.security, wifi.channel, wifi.wifi_band, wifi.rssi),
        (
            Some(WiFiSecurityBitmap::WPA2_PERSONAL),
            Some(36),
            Some(WiFiBandEnum::Band5G),
            Some(-40)
        )
    );
    assert_eq!(scan.thread_scan_results, None);
    let connect = ConnectNetworkResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_null(Tag::Context(2)).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (connect.networking_status, connect.error_value),
        (NetworkCommissioningStatusEnum::Success, Nullable::Null)
    );
}

// ---- M9-A3 B6: Actions ------------------------------------------------------
//
// 1.4.2 ActionsCluster.xml. The ActionList entry, the EndpointLists entry and
// SetupURL are what all-clusters serves on endpoint 1 (the action and endpoint
// lists in all-clusters-common/include/bridged-actions-stub.h, the SetupURL
// default "https://example.com" in all-clusters-app.matter; the same values at
// v1.4.2.0 and master), except SupportedCommands: the stub serves 0 for every
// action, so 0b11 here is synthetic, to exercise the bitmap. The requests and
// both events are synthetic: chip's server refuses every command before the
// stub's delegate runs (InvalidCommand on a served action, whose
// SupportedCommands is 0; NotFound on an unknown action id; v1.4.2.0
// actions-server.cpp HandleCommand, master ActionsCluster.cpp
// ValidateActionExists / ValidateCommandSupported), and the stub never emits
// either event.

#[test]
fn actions_lists_and_requests_decode() {
    use clusters::actions::{
        command_id, decode_action_list, decode_endpoint_lists, decode_setup_url,
        encode_instant_action, encode_instant_action_with_transition, ActionStateEnum,
        ActionTypeEnum, CommandBits, EndpointListTypeEnum,
    };
    let actions = decode_action_list(&list_of(&[&|w| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_utf8(Tag::Context(1), "TurnOnLight").unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
        w.put_uint(Tag::Context(3), 0).unwrap();
        w.put_uint(Tag::Context(4), 0b11).unwrap();
        w.put_uint(Tag::Context(5), 0).unwrap();
    }]))
    .unwrap();
    let a = &actions[0];
    assert_eq!(
        (
            a.action_id,
            a.name.as_str(),
            a.r#type,
            a.endpoint_list_id,
            a.state
        ),
        (
            0,
            "TurnOnLight",
            ActionTypeEnum::Scene,
            0,
            ActionStateEnum::Inactive
        )
    );
    assert_eq!(
        a.supported_commands,
        CommandBits::INSTANT_ACTION | CommandBits::INSTANT_ACTION_WITH_TRANSITION
    );
    let lists = decode_endpoint_lists(&list_of(&[&|w| {
        w.put_uint(Tag::Context(0), 2).unwrap();
        w.put_utf8(Tag::Context(1), "Toggle").unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap();
        w.start_array(Tag::Context(3)).unwrap();
        for ep in [1, 2, 3] {
            w.put_uint(Tag::Anonymous, ep).unwrap();
        }
        w.end_container().unwrap();
    }]))
    .unwrap();
    assert_eq!(
        (
            lists[0].endpoint_list_id,
            lists[0].r#type,
            lists[0].endpoints.clone()
        ),
        (2, EndpointListTypeEnum::Other, vec![1, 2, 3])
    );
    assert_eq!(
        decode_setup_url(&str_attr("https://example.com")).unwrap(),
        "https://example.com"
    );
    // InvokeId is optional; TransitionTime keeps its own tag.
    assert_eq!(
        encode_instant_action(7, None),
        struct_of(&|w| w.put_uint(Tag::Context(0), 7).unwrap())
    );
    assert_eq!(
        encode_instant_action_with_transition(1, None, 50),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 1).unwrap();
            w.put_uint(Tag::Context(2), 50).unwrap();
        })
    );
    assert_eq!(
        encode_instant_action_with_transition(1, Some(9), 50),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 1).unwrap();
            w.put_uint(Tag::Context(1), 9).unwrap();
            w.put_uint(Tag::Context(2), 50).unwrap();
        })
    );
    assert_eq!(
        (
            command_id::INSTANT_ACTION,
            command_id::DISABLE_ACTION_WITH_DURATION
        ),
        (0x00, 0x0B)
    );
}

#[test]
fn actions_events_decode() {
    use clusters::actions::{
        event_id, ActionErrorEnum, ActionFailedEvent, ActionStateEnum, StateChangedEvent,
    };
    let changed = StateChangedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_uint(Tag::Context(1), 9).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (changed.action_id, changed.invoke_id, changed.new_state),
        (1, 9, ActionStateEnum::Active)
    );
    let failed = ActionFailedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_uint(Tag::Context(1), 9).unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap();
        w.put_uint(Tag::Context(3), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(failed.error, ActionErrorEnum::Interrupted);
    // ActionErrorEnum has a member named Unknown (0): an unknown value is
    // `Unrecognized`.
    assert_eq!(ActionErrorEnum::from_raw(0), ActionErrorEnum::Unknown);
    assert_eq!(
        ActionErrorEnum::from_raw(5),
        ActionErrorEnum::Unrecognized(5)
    );
    assert_eq!(
        [event_id::STATE_CHANGED, event_id::ACTION_FAILED],
        [0x00, 0x01]
    );
}

// ---- M9-A3 B6: EcosystemInformation and CommissionerControl ----------------
//
// 1.4.2 EcosystemInformationCluster.xml, CommissionerControlCluster.xml. No
// chip example app fills EcosystemInformation's directories (fabric-bridge-app
// and fabric-sync add the cluster to bridged-device endpoints at runtime but
// never call AddDeviceInfo / AddLocationInfo, at v1.4.2.0 and master), so the
// entries are hand-built to match chip's encoding (zzz_generated/app-common/
// clusters/EcosystemInformation/Structs.ipp, the same at v1.4.2.0 and master):
// our own fabric's entry carries every field; another fabric's carries
// FabricIndex only, so its fabric-sensitive fields are Option.

/// Our fabric's `EcosystemDeviceStruct`, as chip encodes it for its own fabric.
fn own_ecosystem_device(w: &mut TlvWriter<'_>) {
    w.put_utf8(Tag::Context(0), "Kitchen lamp").unwrap();
    w.put_uint(Tag::Context(1), 1_700_000_000_000_000).unwrap();
    w.put_uint(Tag::Context(2), 3).unwrap();
    w.put_uint(Tag::Context(3), 1).unwrap();
    w.start_array(Tag::Context(4)).unwrap();
    w.start_structure(Tag::Anonymous).unwrap();
    w.put_uint(Tag::Context(0), 0x0100).unwrap();
    w.put_uint(Tag::Context(1), 3).unwrap();
    w.end_container().unwrap();
    w.end_container().unwrap();
    w.start_array(Tag::Context(5)).unwrap();
    w.put_utf8(Tag::Anonymous, "kitchen").unwrap();
    w.end_container().unwrap();
    w.put_uint(Tag::Context(6), 1_700_000_000_000_001).unwrap();
    w.put_uint(Tag::Context(254), 1).unwrap();
}

#[test]
fn ecosystem_device_directory_decodes_own_and_other_fabric_entries() {
    use clusters::ecosystem_information::decode_device_directory;
    let devices =
        decode_device_directory(&list_of(&[&own_ecosystem_device, &other_fabric_entry])).unwrap();
    let own = &devices[0];
    assert_eq!(own.device_name.as_deref(), Some("Kitchen lamp"));
    assert_eq!(own.device_name_last_edit, Some(1_700_000_000_000_000));
    assert_eq!(
        (own.bridged_endpoint, own.original_endpoint),
        (Some(3), Some(1))
    );
    let types = own.device_types.as_ref().unwrap();
    assert_eq!((types[0].device_type, types[0].revision), (0x0100, 3));
    assert_eq!(
        own.unique_location_ids.as_deref(),
        Some(&["kitchen".to_string()][..])
    );
    assert_eq!(
        own.unique_location_ids_last_edit,
        Some(1_700_000_000_000_001)
    );
    assert_eq!(own.fabric_index, 1);
    // Another fabric's entry: every fabric-sensitive field withheld.
    let other = &devices[1];
    assert_eq!(other.fabric_index, 2);
    assert_eq!(
        (
            &other.device_name,
            other.device_name_last_edit,
            other.bridged_endpoint,
            other.original_endpoint
        ),
        (&None, None, None, None)
    );
    assert_eq!(
        (
            &other.device_types,
            &other.unique_location_ids,
            other.unique_location_ids_last_edit
        ),
        (&None, &None, None)
    );
}

#[test]
fn ecosystem_location_directory_decodes_the_global_location_struct() {
    use clusters::ecosystem_information::decode_location_directory;
    let locations = decode_location_directory(&list_of(&[
        &|w| {
            w.put_utf8(Tag::Context(0), "kitchen").unwrap();
            // LocationDescriptor: the global `locationdesc`, generated as
            // LocationDescriptorStruct (as in ServiceArea).
            w.start_structure(Tag::Context(1)).unwrap();
            w.put_utf8(Tag::Context(0), "Kitchen").unwrap();
            w.put_int(Tag::Context(1), 0).unwrap();
            w.put_null(Tag::Context(2)).unwrap();
            w.end_container().unwrap();
            w.put_uint(Tag::Context(2), 1_700_000_000_000_002).unwrap();
            w.put_uint(Tag::Context(254), 1).unwrap();
        },
        &other_fabric_entry,
    ]))
    .unwrap();
    let own = &locations[0];
    assert_eq!(own.unique_location_id.as_deref(), Some("kitchen"));
    let place = own.location_descriptor.as_ref().unwrap();
    assert_eq!(
        (
            place.location_name.as_str(),
            place.floor_number,
            place.area_type
        ),
        ("Kitchen", Nullable::Value(0), Nullable::Null)
    );
    assert_eq!(
        own.location_descriptor_last_edit,
        Some(1_700_000_000_000_002)
    );
    let other = &locations[1];
    assert_eq!(
        (
            &other.unique_location_id,
            &other.location_descriptor,
            other.location_descriptor_last_edit,
            other.fabric_index
        ),
        (&None, &None, None, 2)
    );
}

#[test]
fn commissioner_control_requests_response_and_event_decode() {
    use clusters::commissioner_control::{
        command_id, decode_supported_device_categories, encode_commission_node,
        encode_request_commissioning_approval, event_id, CommissioningRequestResultEvent,
        ReverseOpenCommissioningWindow, SupportedDeviceCategoryBitmap,
    };
    assert_eq!(
        decode_supported_device_categories(&uint_attr(1)).unwrap(),
        SupportedDeviceCategoryBitmap::FABRIC_SYNCHRONIZATION
    );
    assert_eq!(
        encode_request_commissioning_approval(0x1122, 0xFFF1, 0x8000, None),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 0x1122).unwrap();
            w.put_uint(Tag::Context(1), 0xFFF1).unwrap();
            w.put_uint(Tag::Context(2), 0x8000).unwrap();
        })
    );
    assert_eq!(
        encode_commission_node(0x1122, 30),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 0x1122).unwrap();
            w.put_uint(Tag::Context(1), 30).unwrap();
        })
    );
    // CommissionNode answers with ReverseOpenCommissioningWindow (0x02).
    assert_eq!(
        (
            command_id::COMMISSION_NODE,
            command_id::REVERSE_OPEN_COMMISSIONING_WINDOW
        ),
        (0x01, 0x02)
    );
    let window = ReverseOpenCommissioningWindow::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 180).unwrap();
        w.put_bytes(Tag::Context(1), &[0xAB; 97]).unwrap();
        w.put_uint(Tag::Context(2), 3840).unwrap();
        w.put_uint(Tag::Context(3), 1000).unwrap();
        w.put_bytes(Tag::Context(4), &[0x5A; 32]).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            window.commissioning_timeout,
            window.pake_passcode_verifier.len(),
            window.discriminator,
            window.iterations,
            window.salt.len()
        ),
        (180, 97, 3840, 1000, 32)
    );
    // A fabric-sensitive event: chip sends it only to its own fabric, in full
    // (events are exempt from the fabric-sensitive Option rule).
    let result = CommissioningRequestResultEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0x1122).unwrap();
        w.put_uint(Tag::Context(1), 0x0102_0304_0506_0708).unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap();
        w.put_uint(Tag::Context(254), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            result.request_id,
            result.client_node_id,
            result.status_code,
            result.fabric_index
        ),
        (0x1122, 0x0102_0304_0506_0708, 0, 1)
    );
    assert_eq!(event_id::COMMISSIONING_REQUEST_RESULT, 0x00);
}

// ---- M9-A3 B6: network infrastructure ---------------------------------------
//
// 1.4.2 WiFiNetworkManagement.xml, ThreadBorderRouterManagement.xml,
// ThreadNetworkDirectory.xml. network-manager-app is chip's only example app
// that hosts all three (thread-br-app and others also host
// ThreadBorderRouterManagement); none is built here (they need ZAP at build
// time), so these values
// are hand-built to the 1.4.2 shapes. The passphrase and the datasets are
// secrets on a real network: the values here are synthetic, and they are
// compared with `assert!(a == b)` so a failure never prints them.

#[test]
#[allow(clippy::manual_assert_eq)] // assert_eq! would print secret bytes on failure.
fn wifi_network_management_attributes_and_passphrase_response_decode() {
    use clusters::wifi_network_management::{
        command_id, decode_passphrase_surrogate, decode_ssid, encode_network_passphrase_request,
        NetworkPassphraseResponse,
    };
    assert_eq!(decode_ssid(&null_attr()).unwrap(), Nullable::Null);
    assert_eq!(
        decode_ssid(&bytes_attr(b"home")).unwrap(),
        Nullable::Value(b"home".to_vec())
    );
    assert_eq!(
        decode_passphrase_surrogate(&uint_attr(17)).unwrap(),
        Nullable::Value(17)
    );
    assert_eq!(encode_network_passphrase_request(), struct_of(&|_| {}));
    assert_eq!(
        (
            command_id::NETWORK_PASSPHRASE_REQUEST,
            command_id::NETWORK_PASSPHRASE_RESPONSE
        ),
        (0x00, 0x01)
    );
    let response = NetworkPassphraseResponse::decode(&struct_of(&|w| {
        w.put_bytes(Tag::Context(0), b"correct horse").unwrap();
    }))
    .unwrap();
    assert!(response.passphrase == b"correct horse");
}

#[test]
#[allow(clippy::manual_assert_eq)] // assert_eq! would print secret bytes on failure.
fn thread_border_router_management_requests_and_dataset_response_decode() {
    use clusters::thread_border_router_management::{
        command_id, decode_active_dataset_timestamp, decode_border_agent_id,
        decode_border_router_name, encode_get_active_dataset_request,
        encode_set_active_dataset_request, encode_set_pending_dataset_request, DatasetResponse,
        Feature,
    };
    assert_eq!(Feature::PC.bits(), 1);
    assert_eq!(
        decode_border_router_name(&str_attr("network-manager")).unwrap(),
        "network-manager"
    );
    assert_eq!(
        decode_border_agent_id(&bytes_attr(&[0x11; 16])).unwrap(),
        vec![0x11; 16]
    );
    assert_eq!(
        decode_active_dataset_timestamp(&null_attr()).unwrap(),
        Nullable::Null
    );
    // GetActiveDatasetRequest and GetPendingDatasetRequest both answer with
    // DatasetResponse (0x02).
    assert_eq!(encode_get_active_dataset_request(), struct_of(&|_| {}));
    assert_eq!(command_id::DATASET_RESPONSE, 0x02);
    let dataset = vec![0x0e, 0x08, 0, 0, 0, 0, 0, 1, 0, 0];
    assert!(
        encode_set_active_dataset_request(&Secret::new(dataset.clone()), Some(9))
            == struct_of(&|w| {
                w.put_bytes(Tag::Context(0), &dataset).unwrap();
                w.put_uint(Tag::Context(1), 9).unwrap();
            })
    );
    assert!(
        encode_set_pending_dataset_request(&Secret::new(dataset.clone()))
            == struct_of(&|w| w.put_bytes(Tag::Context(0), &dataset).unwrap())
    );
    let response = DatasetResponse::decode(&struct_of(&|w| {
        w.put_bytes(Tag::Context(0), &dataset).unwrap();
    }))
    .unwrap();
    assert!(response.dataset == dataset);
}

#[test]
#[allow(clippy::manual_assert_eq)] // assert_eq! would print secret bytes on failure.
fn thread_network_directory_networks_requests_and_response_decode() {
    use clusters::thread_network_directory::{
        command_id, decode_preferred_extended_pan_id, decode_thread_network_table_size,
        decode_thread_networks, encode_add_network, encode_get_operational_dataset,
        encode_preferred_extended_pan_id, OperationalDatasetResponse,
    };
    let networks = decode_thread_networks(&list_of(&[&|w| {
        w.put_bytes(Tag::Context(0), &[0xDE, 0xAD, 0, 0xBE, 0xEF, 0, 0xCA, 0xFE])
            .unwrap();
        w.put_utf8(Tag::Context(1), "MyHome").unwrap();
        w.put_uint(Tag::Context(2), 15).unwrap();
        w.put_uint(Tag::Context(3), 1).unwrap();
    }]))
    .unwrap();
    let n = &networks[0];
    assert_eq!(
        (n.network_name.as_str(), n.channel, n.active_timestamp),
        ("MyHome", 15, 1)
    );
    // The extended PAN id is not a secret: compared by value.
    assert_eq!(
        n.extended_pan_id,
        [0xDE, 0xAD, 0, 0xBE, 0xEF, 0, 0xCA, 0xFE]
    );
    assert_eq!(
        decode_thread_network_table_size(&uint_attr(10)).unwrap(),
        10
    );
    // PreferredExtendedPanID is writable and nullable.
    assert_eq!(
        encode_preferred_extended_pan_id(Nullable::Null),
        null_attr()
    );
    let xpan = vec![0xDE, 0xAD, 0, 0xBE, 0xEF, 0, 0xCA, 0xFE];
    assert_eq!(
        decode_preferred_extended_pan_id(&encode_preferred_extended_pan_id(Nullable::Value(
            xpan.clone()
        )))
        .unwrap(),
        Nullable::Value(xpan.clone())
    );
    // AddNetwork carries an operational dataset (a secret on a real network).
    assert!(
        encode_add_network(&Secret::new(vec![0x0e, 0x08]))
            == struct_of(&|w| w.put_bytes(Tag::Context(0), &[0x0e, 0x08]).unwrap())
    );
    assert_eq!(
        encode_get_operational_dataset(&xpan),
        struct_of(&|w| w.put_bytes(Tag::Context(0), &xpan).unwrap())
    );
    assert_eq!(
        (
            command_id::GET_OPERATIONAL_DATASET,
            command_id::OPERATIONAL_DATASET_RESPONSE
        ),
        (0x02, 0x03)
    );
    let response = OperationalDatasetResponse::decode(&struct_of(&|w| {
        w.put_bytes(Tag::Context(0), &[0x0e, 0x08]).unwrap();
    }))
    .unwrap();
    assert!(response.operational_dataset == [0x0e, 0x08]);
}

// ---- M9-A3 B6: secrets stay out of `Debug` ----------------------------------
//
// A Thread dataset, a Wi-Fi passphrase, a group epoch key or a PAKE verifier
// logged with `{:?}` must not leak: their structs print `<redacted>` there
// (the dump's SECRET_FIELDS). The values themselves are untouched. The values
// below are synthetic; no assertion prints a `{:?}` rendering or a secret
// value on failure, since a failing redaction would print the very bytes it
// failed to hide.

/// Assert that `shown` (a `{:?}` rendering) carries the `"<redacted>"`
/// placeholder and none of `secret`'s renderings: its `Debug` byte list
/// (decimal, as `Vec<u8>` prints), that list in hex (`{:x?}`), and the bytes
/// read as text. Never prints `shown`.
fn assert_redacted(shown: &str, secret: &[u8]) {
    assert!(
        shown.contains("\"<redacted>\""),
        "the placeholder is missing"
    );
    let decimal = format!("{secret:?}");
    let decimal = decimal.trim_start_matches('[').trim_end_matches(']');
    assert!(
        !shown.contains(decimal),
        "the secret's Debug byte list leaked"
    );
    let hex = format!("{secret:x?}");
    let hex = hex.trim_start_matches('[').trim_end_matches(']');
    assert!(!shown.contains(hex), "the secret's hex byte list leaked");
    let text = String::from_utf8_lossy(secret);
    assert!(!shown.contains(text.as_ref()), "the secret's text leaked");
}

#[test]
#[allow(clippy::manual_assert_eq)] // assert_eq! would print secret bytes on failure.
fn secret_fields_are_redacted_in_debug_output() {
    use clusters::commissioner_control::ReverseOpenCommissioningWindow;
    use clusters::group_key_management::{GroupKeySecurityPolicyEnum, GroupKeySetStruct};
    use clusters::thread_border_router_management::DatasetResponse;
    use clusters::thread_network_directory::OperationalDatasetResponse;
    use clusters::wifi_network_management::NetworkPassphraseResponse;

    // WiFiNetworkManagement: the passphrase. The whole rendering is pinned:
    // a one-field struct prints nothing else.
    let passphrase = NetworkPassphraseResponse::decode(&struct_of(&|w| {
        w.put_bytes(Tag::Context(0), b"correct horse").unwrap();
    }))
    .unwrap();
    assert!(passphrase.passphrase == b"correct horse");
    let shown = format!("{passphrase:?}");
    assert!(
        shown == "NetworkPassphraseResponse { passphrase: \"<redacted>\" }",
        "unexpected NetworkPassphraseResponse rendering"
    );
    assert_redacted(&shown, b"correct horse");
    // `{:#?}` and `{:x?}` go through the same impl.
    assert_redacted(&format!("{passphrase:#?}"), b"correct horse");
    assert_redacted(&format!("{passphrase:x?}"), b"correct horse");

    // ThreadBorderRouterManagement and ThreadNetworkDirectory: a dataset; its
    // network key (TLV type 5) is among the bytes.
    let dataset_bytes = [0x05, 0x10, 0xAA, 0xBB, 0xCC];
    let dataset = DatasetResponse::decode(&struct_of(&|w| {
        w.put_bytes(Tag::Context(0), &dataset_bytes).unwrap();
    }))
    .unwrap();
    assert!(dataset.dataset == dataset_bytes);
    let shown = format!("{dataset:?}");
    assert!(shown == "DatasetResponse { dataset: \"<redacted>\" }");
    assert_redacted(&shown, &dataset_bytes);
    let operational = OperationalDatasetResponse::decode(&struct_of(&|w| {
        w.put_bytes(Tag::Context(0), &dataset_bytes).unwrap();
    }))
    .unwrap();
    assert!(operational.operational_dataset == dataset_bytes);
    let shown = format!("{operational:?}");
    assert!(shown == "OperationalDatasetResponse { operational_dataset: \"<redacted>\" }");
    assert_redacted(&shown, &dataset_bytes);

    // CommissionerControl: the PAKE verifier; the salt and the rest print.
    let verifier = [0xC4; 97];
    let window = ReverseOpenCommissioningWindow::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 180).unwrap();
        w.put_bytes(Tag::Context(1), &verifier).unwrap();
        w.put_uint(Tag::Context(2), 3840).unwrap();
        w.put_uint(Tag::Context(3), 1000).unwrap();
        w.put_bytes(Tag::Context(4), &[0x01, 0x02]).unwrap();
    }))
    .unwrap();
    assert!(window.pake_passcode_verifier == verifier);
    let shown = format!("{window:?}");
    assert!(
        shown
            == "ReverseOpenCommissioningWindow { commissioning_timeout: 180, \
                pake_passcode_verifier: \"<redacted>\", discriminator: 3840, \
                iterations: 1000, salt: [1, 2] }",
        "unexpected ReverseOpenCommissioningWindow rendering"
    );
    assert_redacted(&shown, &verifier);

    // GroupKeyManagement: the three epoch keys; the start times and the rest
    // print as the derive printed them.
    let key = [0xAB; 16];
    let keys = GroupKeySetStruct {
        group_key_set_id: 0x0042,
        group_key_security_policy: GroupKeySecurityPolicyEnum::from_raw(0),
        epoch_key0: Nullable::Value(key.to_vec()),
        epoch_start_time0: Nullable::Value(1234),
        epoch_key1: Nullable::Value(key.to_vec()),
        epoch_start_time1: Nullable::Value(5678),
        epoch_key2: Nullable::Null,
        epoch_start_time2: Nullable::Null,
        group_key_multicast_policy: None,
        fabric_index: None,
    };
    let shown = format!("{keys:?}");
    assert!(shown.starts_with("GroupKeySetStruct { group_key_set_id: 66, "));
    assert!(shown.contains(", epoch_key0: \"<redacted>\", epoch_start_time0: Value(1234), "));
    assert!(shown.contains(", epoch_key1: \"<redacted>\", epoch_start_time1: Value(5678), "));
    assert!(shown.contains(", epoch_key2: \"<redacted>\", epoch_start_time2: Null, "));
    assert!(shown.ends_with(", group_key_multicast_policy: None, fabric_index: None }"));
    assert_eq!(shown.matches("\"<redacted>\"").count(), 3);
    assert_redacted(&shown, &key);
    assert!(!shown.contains("171"), "an epoch key byte leaked");
}

// ---- M9-A3 B7: media basics -----------------------------------------------
//
// WakeOnLan, LowPower, KeypadInput, MediaInput, AudioOutput and
// TargetNavigator (1.4.2 WakeOnLAN.xml, LowPower.xml, KeypadInput.xml,
// MediaInput.xml, AudioOutput.xml, TargetNavigator.xml). These values are
// tv-app's, the same at master 5cd2917a and v1.4.2.0
// (examples/tv-app/tv-common/clusters/): MediaInputManager's two HDMI inputs,
// the first two of AudioOutputManager's three HDMI outputs and its current
// output (1), TargetNavigatorManager's two targets and its NavigateTarget
// refusal (TargetNotFound, "error"), KeypadInputManager's FeatureMap (7) and
// its UnsupportedKey answer. Everything else is synthetic: the third output's
// type (9, an unknown OutputTypeEnum value; tv-app's is HDMI), the MAC
// address, the link-local address, the event payload, the
// InvalidKeyInCurrentState answer, the input with no description, and every
// request argument.

#[test]
fn wake_on_lan_low_power_and_keypad_input_codecs() {
    use clusters::keypad_input::{
        encode_send_key, CecKeyCodeEnum, Feature, SendKeyResponse, StatusEnum,
    };
    use clusters::{low_power, wake_on_lan};
    assert_eq!(
        wake_on_lan::decode_mac_address(&str_attr("0A1B2C3D4E5F")).unwrap(),
        "0A1B2C3D4E5F"
    );
    let link_local = [
        0xFE, 0x80, 0, 0, 0, 0, 0, 0, 0x02, 0x1B, 0x2C, 0xFF, 0xFE, 0x3D, 0x4E, 0x5F,
    ];
    assert_eq!(
        wake_on_lan::decode_link_local_address(&bytes_attr(&link_local)).unwrap(),
        link_local
    );
    // Sleep has no fields: an empty anonymous structure.
    assert_eq!(low_power::encode_sleep(), struct_of(&|_| {}));
    assert_eq!(
        (Feature::NV | Feature::LK | Feature::NK).bits(),
        0x07,
        "tv-app's KeypadInput FeatureMap"
    );
    // Key codes are not contiguous: Data (0x76) is the last, Number0OrNumber10 0x20.
    assert_eq!(
        encode_send_key(CecKeyCodeEnum::Data),
        struct_of(&|w| w.put_uint(Tag::Context(0), 0x76).unwrap())
    );
    assert_eq!(
        CecKeyCodeEnum::from_raw(0x20),
        CecKeyCodeEnum::Number0OrNumber10
    );
    assert_eq!(
        CecKeyCodeEnum::from_raw(0x0E),
        CecKeyCodeEnum::Unknown(0x0E)
    );
    // 0x4F is `Reserved` in the 1.4.2 spec XML and chip's zap XML, not a key;
    // the model (like matter.js) has no variant for it.
    assert_eq!(
        CecKeyCodeEnum::from_raw(0x4F),
        CecKeyCodeEnum::Unknown(0x4F)
    );
    let response = |status| struct_of(&|w| w.put_uint(Tag::Context(0), status).unwrap());
    assert_eq!(
        SendKeyResponse::decode(&response(1)).unwrap().status,
        StatusEnum::UnsupportedKey
    );
    assert_eq!(
        SendKeyResponse::decode(&response(2)).unwrap().status,
        StatusEnum::InvalidKeyInCurrentState
    );
    assert!(SendKeyResponse::decode(&struct_of(&|_| {})).is_err());
}

#[test]
fn media_input_and_audio_output_lists_and_requests() {
    use clusters::audio_output::{self as ao, OutputTypeEnum};
    use clusters::media_input::{self as mi, InputTypeEnum};
    let input = |index: u64, name: &'static str| {
        move |w: &mut TlvWriter<'_>| {
            w.put_uint(Tag::Context(0), index).unwrap();
            w.put_uint(Tag::Context(1), 4).unwrap(); // Hdmi
            w.put_utf8(Tag::Context(2), name).unwrap();
            w.put_utf8(Tag::Context(3), "High-Definition Multimedia Interface")
                .unwrap();
        }
    };
    let inputs = mi::decode_input_list(&list_of(&[&input(1, "HDMI"), &input(2, "HDMI")])).unwrap();
    let got: Vec<(u8, InputTypeEnum, &str)> = inputs
        .iter()
        .map(|i| (i.index, i.input_type, i.name.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (1, InputTypeEnum::Hdmi, "HDMI"),
            (2, InputTypeEnum::Hdmi, "HDMI")
        ]
    );
    // Description is mandatory in InputInfoStruct.
    let no_description = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_uint(Tag::Context(1), 4).unwrap();
        w.put_utf8(Tag::Context(2), "HDMI").unwrap();
    };
    assert!(mi::decode_input_list(&list_of(&[&no_description])).is_err());
    assert_eq!(
        mi::encode_rename_input(2, &"Console".to_string()),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 2).unwrap();
            w.put_utf8(Tag::Context(1), "Console").unwrap();
        })
    );
    assert_eq!(mi::encode_show_input_status(), struct_of(&|_| {}));
    let output = |index: u64, kind: u64| {
        move |w: &mut TlvWriter<'_>| {
            w.put_uint(Tag::Context(0), index).unwrap();
            w.put_uint(Tag::Context(1), kind).unwrap();
            w.put_utf8(Tag::Context(2), "HDMI").unwrap();
        }
    };
    let outputs =
        ao::decode_output_list(&list_of(&[&output(1, 0), &output(2, 0), &output(3, 9)])).unwrap();
    let got: Vec<(u8, OutputTypeEnum)> = outputs.iter().map(|o| (o.index, o.output_type)).collect();
    assert_eq!(
        got,
        [
            (1, OutputTypeEnum::Hdmi),
            (2, OutputTypeEnum::Hdmi),
            (3, OutputTypeEnum::Unknown(9))
        ]
    );
    assert_eq!(ao::decode_current_output(&uint_attr(1)).unwrap(), 1);
    assert_eq!(
        ao::encode_select_output(3),
        struct_of(&|w| w.put_uint(Tag::Context(0), 3).unwrap())
    );
}

#[test]
fn target_navigator_list_response_and_event_decode() {
    use clusters::target_navigator::{
        self as tn, NavigateTargetResponse, StatusEnum, TargetUpdatedEvent,
    };
    let target = |id: u64, name: &'static str| {
        move |w: &mut TlvWriter<'_>| {
            w.put_uint(Tag::Context(0), id).unwrap();
            w.put_utf8(Tag::Context(1), name).unwrap();
        }
    };
    let targets = tn::decode_target_list(&list_of(&[
        &target(1, "exampleName"),
        &target(2, "exampleName"),
    ]))
    .unwrap();
    let got: Vec<(u8, &str)> = targets
        .iter()
        .map(|t| (t.identifier, t.name.as_str()))
        .collect();
    assert_eq!(got, [(1, "exampleName"), (2, "exampleName")]);
    assert_eq!(
        tn::encode_navigate_target(2, Some("hint".to_string())),
        struct_of(&|w| {
            w.put_uint(Tag::Context(0), 2).unwrap();
            w.put_utf8(Tag::Context(1), "hint").unwrap();
        })
    );
    assert_eq!(
        tn::encode_navigate_target(1, None),
        struct_of(&|w| w.put_uint(Tag::Context(0), 1).unwrap())
    );
    let refused = NavigateTargetResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_utf8(Tag::Context(1), "error").unwrap();
    }))
    .unwrap();
    assert_eq!(
        (refused.status, refused.data.as_deref()),
        (StatusEnum::TargetNotFound, Some("error"))
    );
    // TargetUpdated: all three fields optional (1.4.2); chip's server would
    // send an empty list, target 0 and empty data (target-navigator-server.cpp).
    let full = TargetUpdatedEvent::decode(&struct_of(&|w| {
        w.start_array(Tag::Context(0)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        target(5, "menu")(w);
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(1), 5).unwrap();
        w.put_bytes(Tag::Context(2), &[0xAB]).unwrap();
    }))
    .unwrap();
    assert_eq!(full.current_target, Some(5));
    assert_eq!(full.target_list.as_ref().map(Vec::len), Some(1));
    assert_eq!(full.data.as_deref(), Some(&[0xAB][..]));
    let empty = TargetUpdatedEvent::decode(&struct_of(&|_| {})).unwrap();
    assert_eq!(
        (empty.target_list, empty.current_target, empty.data),
        (None, None, None)
    );
}

// ---- M9-A3 B7: the Content App platform ------------------------------------
//
// ApplicationBasic, ApplicationLauncher, AccountLogin and ContentAppObserver
// (1.4.2 ApplicationBasic.xml, ApplicationLauncher.xml, AccountLogin.xml,
// ContentAppObserver.xml). ApplicationBasic's names, ids, Application and
// AllowedVendorList are what tv-app's static Content App endpoint 3 serves
// (ApplicationBasicManager's defaults, master 5cd2917a and v1.4.2.0), and
// Status 0 is chip's server delegate's initial value; Status 3 is synthetic.
// The CatalogList is ApplicationLauncherManager's (both refs). The setup PIN
// is Vendor3's in tv-app (AppTv.cpp, both refs). Everything else is
// synthetic.

#[test]
fn application_basic_attributes_decode() {
    use clusters::application_basic::{self as ab, ApplicationStatusEnum};
    assert_eq!(
        ab::decode_vendor_name(&str_attr("exampleVendorName1")).unwrap(),
        "exampleVendorName1"
    );
    assert_eq!(ab::decode_vendor_id(&uint_attr(1)).unwrap(), 1);
    assert_eq!(ab::decode_product_id(&uint_attr(1)).unwrap(), 1);
    let app = ab::decode_application(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 123).unwrap();
        w.put_utf8(Tag::Context(1), "applicationId").unwrap();
    }))
    .unwrap();
    assert_eq!(
        (app.catalog_vendor_id, app.application_id.as_str()),
        (123, "applicationId")
    );
    assert_eq!(
        ab::decode_status(&uint_attr(0)).unwrap(),
        ApplicationStatusEnum::Stopped
    );
    assert_eq!(
        ab::decode_status(&uint_attr(3)).unwrap(),
        ApplicationStatusEnum::ActiveVisibleNotFocus
    );
    assert_eq!(
        ab::decode_allowed_vendor_list(&uint_array_attr(&[1, 456])).unwrap(),
        [1, 456]
    );
    // ApplicationStruct's ApplicationId is mandatory.
    assert!(
        ab::decode_application(&struct_of(&|w| w.put_uint(Tag::Context(0), 123).unwrap())).is_err()
    );
}

#[test]
fn application_launcher_current_app_requests_and_response() {
    use clusters::application_launcher::{
        self as al, ApplicationStruct, LauncherResponse, StatusEnum,
    };
    assert_eq!(
        al::decode_catalog_list(&uint_array_attr(&[123, 456])).unwrap(),
        [123, 456]
    );
    assert_eq!(
        al::decode_current_app(&null_attr()).unwrap(),
        Nullable::Null
    );
    // CurrentApp nests ApplicationStruct in ApplicationEPStruct; Endpoint is
    // optional.
    let Nullable::Value(current) = al::decode_current_app(&struct_of(&|w| {
        w.start_structure(Tag::Context(0)).unwrap();
        w.put_uint(Tag::Context(0), 9050).unwrap();
        w.put_utf8(Tag::Context(1), "App3").unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(1), 6).unwrap();
    }))
    .unwrap() else {
        panic!("CurrentApp decoded as null");
    };
    assert_eq!(
        (
            current.application.catalog_vendor_id,
            current.application.application_id.as_str(),
            current.endpoint
        ),
        (9050, "App3", Some(6))
    );
    let app = ApplicationStruct {
        catalog_vendor_id: 123,
        application_id: "exampleid".to_string(),
    };
    assert_eq!(
        al::encode_launch_app(Some(app.clone()), Some(b"data".to_vec())),
        struct_of(&|w| {
            w.start_structure(Tag::Context(0)).unwrap();
            w.put_uint(Tag::Context(0), 123).unwrap();
            w.put_utf8(Tag::Context(1), "exampleid").unwrap();
            w.end_container().unwrap();
            w.put_bytes(Tag::Context(1), b"data").unwrap();
        })
    );
    // Application is optional (a Content App endpoint launches itself).
    assert_eq!(al::encode_stop_app(None), struct_of(&|_| {}));
    let launched = LauncherResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 3).unwrap();
        w.put_bytes(Tag::Context(1), b"data").unwrap();
    }))
    .unwrap();
    assert_eq!(
        (launched.status, launched.data.as_deref()),
        (StatusEnum::PendingUserApproval, Some(&b"data"[..]))
    );
}

#[test]
#[allow(clippy::manual_assert_eq)] // assert_eq! would print the setup PIN on failure.
fn account_login_requests_secret_response_and_event() {
    use clusters::account_login::{self as alog, GetSetupPinResponse, LoggedOutEvent};
    assert_eq!(
        alog::encode_get_setup_pin(&"AAAABBBBCCCCDDDD".to_string()),
        struct_of(&|w| w.put_utf8(Tag::Context(0), "AAAABBBBCCCCDDDD").unwrap())
    );
    // Login's payload carries the setup PIN, so it is compared with assert!.
    assert!(
        alog::encode_login(
            &"AAAABBBBCCCCDDDD".to_string(),
            &Secret::new("20202021".to_string()),
            Some(0x1122)
        ) == struct_of(&|w| {
            w.put_utf8(Tag::Context(0), "AAAABBBBCCCCDDDD").unwrap();
            w.put_utf8(Tag::Context(1), "20202021").unwrap();
            w.put_uint(Tag::Context(2), 0x1122).unwrap();
        }),
        "unexpected Login payload"
    );
    assert_eq!(alog::encode_logout(None), struct_of(&|_| {}));
    let pin = GetSetupPinResponse::decode(&struct_of(&|w| {
        w.put_utf8(Tag::Context(0), "20202021").unwrap();
    }))
    .unwrap();
    assert!(pin.setup_pin == "20202021");
    // The whole rendering is pinned: a one-field struct prints nothing else.
    let shown = format!("{pin:?}");
    assert!(
        shown == "GetSetupPinResponse { setup_pin: \"<redacted>\" }",
        "unexpected GetSetupPinResponse rendering"
    );
    assert_redacted(&shown, b"20202021");
    // `{:#?}` and `{:x?}` go through the same impl.
    assert_redacted(&format!("{pin:#?}"), b"20202021");
    assert_redacted(&format!("{pin:x?}"), b"20202021");
    // LoggedOut: Node optional. chip's codegen adds the implicit FabricIndex
    // (tag 254) of a fabric-sensitive event; the model has no such field, so
    // the decoder skips it.
    let ev = LoggedOutEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 0x0102_0304_0506_0708).unwrap();
        w.put_uint(Tag::Context(254), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(ev.node, Some(0x0102_0304_0506_0708));
    assert_eq!(
        LoggedOutEvent::decode(&struct_of(&|_| {})).unwrap().node,
        None
    );
}

#[test]
fn content_app_observer_message_and_response() {
    use clusters::content_app_observer::{self as cao, ContentAppMessageResponse, StatusEnum};
    // The specification makes Data mandatory and EncodingHint optional; chip's
    // codegen has them the other way round (both refs), and its generated
    // decoder checks neither field's presence, so the encoding below suits
    // both.
    assert_eq!(
        cao::encode_content_app_message(&"hello".to_string(), None),
        struct_of(&|w| w.put_utf8(Tag::Context(0), "hello").unwrap())
    );
    assert_eq!(
        cao::encode_content_app_message(&"hello".to_string(), Some("text/plain".to_string())),
        struct_of(&|w| {
            w.put_utf8(Tag::Context(0), "hello").unwrap();
            w.put_utf8(Tag::Context(1), "text/plain").unwrap();
        })
    );
    let response = ContentAppMessageResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_utf8(Tag::Context(1), "exampleData").unwrap();
    }))
    .unwrap();
    assert_eq!(
        (
            response.status,
            response.data.as_deref(),
            response.encoding_hint
        ),
        (StatusEnum::UnexpectedData, Some("exampleData"), None)
    );
}

// ---- M9-A3 B7: MediaPlayback -------------------------------------------------
//
// 1.4.2 MediaPlayback.xml. tv-app's values (MediaPlaybackManager.h and .cpp, at
// master 5cd2917a and v1.4.2.0): Duration 80000; the "activeAudioTrackId_0"
// track with "languageCode1" and "displayName1"; the "activeAudioTrackId_1" id;
// an ActiveTextTrack with an empty id and a null TrackAttributes; and the
// SeekOutOfRange "data response" reply to a Seek past 80000. Every other value
// is synthetic: the "en-main" track and its characteristics, CurrentState,
// StartTime, SampledPosition, PlaybackSpeed, the Rewind and AudioOutputIndex
// arguments, and the event.

/// A `TrackStruct` with id `id` whose `TrackAttributes` is written by
/// `attrs` (`None`: TLV null).
fn track_struct(w: &mut TlvWriter<'_>, id: &str, attrs: Option<&dyn Fn(&mut TlvWriter<'_>)>) {
    w.put_utf8(Tag::Context(0), id).unwrap();
    match attrs {
        Some(write) => {
            w.start_structure(Tag::Context(1)).unwrap();
            write(w);
            w.end_container().unwrap();
        }
        None => w.put_null(Tag::Context(1)).unwrap(),
    }
}

#[test]
fn media_playback_tracks_decode_chip_and_spec_shapes() {
    use clusters::media_playback::{self as mp, CharacteristicEnum};
    // tv-app's ActiveTextTrack before a text track is activated: TrackAttributes
    // is null, which chip's codegen allows and the specification does not
    // (FIELD_RELAXATIONS class N).
    let Nullable::Value(text) =
        mp::decode_active_text_track(&struct_of(&|w| track_struct(w, "", None))).unwrap()
    else {
        panic!("ActiveTextTrack decoded as null");
    };
    assert_eq!(
        (text.id.as_str(), text.track_attributes),
        ("", Nullable::Null)
    );
    // chip numbers DisplayName at tag 1 (its TrackAttributesStruct has no
    // Characteristics): tv-app's display name is a string at the tag where the
    // specification has the Characteristics list, so it is skipped.
    let Nullable::Value(audio) = mp::decode_active_audio_track(&struct_of(&|w| {
        track_struct(
            w,
            "activeAudioTrackId_0",
            Some(&|w| {
                w.put_utf8(Tag::Context(0), "languageCode1").unwrap();
                w.put_utf8(Tag::Context(1), "displayName1").unwrap();
            }),
        );
    }))
    .unwrap() else {
        panic!("ActiveAudioTrack decoded as null");
    };
    let Nullable::Value(attrs) = &audio.track_attributes else {
        panic!("TrackAttributes decoded as null");
    };
    assert_eq!(
        (
            attrs.language_code.as_str(),
            attrs.characteristics.clone(),
            attrs.display_name.clone()
        ),
        ("languageCode1", None, None)
    );
    // A null DisplayName at chip's tag 1 (its codegen allows one; tv-app always
    // sends a string, both refs) is the specification's null Characteristics.
    let Nullable::Value(nulled) = mp::decode_active_audio_track(&struct_of(&|w| {
        track_struct(
            w,
            "activeAudioTrackId_0",
            Some(&|w| {
                w.put_utf8(Tag::Context(0), "languageCode1").unwrap();
                w.put_null(Tag::Context(1)).unwrap();
            }),
        );
    }))
    .unwrap() else {
        panic!("ActiveAudioTrack decoded as null");
    };
    let Nullable::Value(attrs) = &nulled.track_attributes else {
        panic!("TrackAttributes decoded as null");
    };
    assert_eq!(
        (attrs.characteristics.clone(), attrs.display_name.clone()),
        (Some(Nullable::Null), None)
    );
    // The specification's shape: Characteristics (MediaPlayback's own
    // CharacteristicEnum) at tag 1, DisplayName at tag 2.
    let tracks = mp::decode_available_audio_tracks(&list_of(&[&|w: &mut TlvWriter<'_>| {
        track_struct(
            w,
            "en-main",
            Some(&|w| {
                w.put_utf8(Tag::Context(0), "en").unwrap();
                w.start_array(Tag::Context(1)).unwrap();
                w.put_uint(Tag::Anonymous, 4).unwrap();
                w.put_uint(Tag::Anonymous, 17).unwrap();
                w.end_container().unwrap();
                w.put_null(Tag::Context(2)).unwrap();
            }),
        );
    }]))
    .unwrap();
    let Nullable::Value(tracks) = tracks else {
        panic!("AvailableAudioTracks decoded as null");
    };
    let Nullable::Value(attrs) = &tracks[0].track_attributes else {
        panic!("TrackAttributes decoded as null");
    };
    assert_eq!(
        attrs.characteristics,
        Some(Nullable::Value(vec![
            CharacteristicEnum::MainProgram,
            CharacteristicEnum::Karaoke
        ]))
    );
    assert_eq!(attrs.display_name, Some(Nullable::Null));
    assert_eq!(
        mp::decode_available_text_tracks(&null_attr()).unwrap(),
        Nullable::Null
    );
}

#[test]
fn media_playback_attributes_requests_responses_and_event() {
    use clusters::media_playback::{
        self as mp, PlaybackResponse, PlaybackStateEnum, StateChangedEvent, StatusEnum,
    };
    assert_eq!(
        mp::decode_current_state(&uint_attr(3)).unwrap(),
        PlaybackStateEnum::Buffering
    );
    assert_eq!(
        mp::decode_duration(&uint_attr(80_000)).unwrap(),
        Nullable::Value(80_000)
    );
    assert_eq!(mp::decode_start_time(&null_attr()).unwrap(), Nullable::Null);
    let Nullable::Value(position) = mp::decode_sampled_position(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1_700_000_000_000_000).unwrap();
        w.put_null(Tag::Context(1)).unwrap();
    }))
    .unwrap() else {
        panic!("SampledPosition decoded as null");
    };
    assert_eq!(
        (position.updated_at, position.position),
        (1_700_000_000_000_000, Nullable::Null)
    );
    assert!((mp::decode_playback_speed(&float_attr(-2.0)).unwrap() - -2.0).abs() < f32::EPSILON);
    assert_eq!(
        mp::encode_seek(80_001),
        struct_of(&|w| w.put_uint(Tag::Context(0), 80_001).unwrap())
    );
    assert_eq!(mp::encode_play(), struct_of(&|_| {}));
    assert_eq!(
        mp::encode_rewind(Some(true)),
        struct_of(&|w| w.put_bool(Tag::Context(0), true).unwrap())
    );
    // AudioOutputIndex: absent, null and a value are three requests.
    let track = "activeAudioTrackId_1".to_string();
    let with = |index: Option<Nullable<u8>>| mp::encode_activate_audio_track(&track, index);
    let id = |w: &mut TlvWriter<'_>| w.put_utf8(Tag::Context(0), "activeAudioTrackId_1").unwrap();
    assert_eq!(with(None), struct_of(&id));
    assert_eq!(
        with(Some(Nullable::Null)),
        struct_of(&|w| {
            id(w);
            w.put_null(Tag::Context(1)).unwrap();
        })
    );
    assert_eq!(
        with(Some(Nullable::Value(0))),
        struct_of(&|w| {
            id(w);
            w.put_uint(Tag::Context(1), 0).unwrap();
        })
    );
    let response = PlaybackResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 5).unwrap();
        w.put_utf8(Tag::Context(1), "data response").unwrap();
    }))
    .unwrap();
    assert_eq!(
        (response.status, response.data.as_deref()),
        (StatusEnum::SeekOutOfRange, Some("data response"))
    );
    let ev = StateChangedEvent::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.start_structure(Tag::Context(3)).unwrap();
        w.put_uint(Tag::Context(0), 10).unwrap();
        w.put_uint(Tag::Context(1), 1_000).unwrap();
        w.end_container().unwrap();
        w.put_bool(Tag::Context(8), false).unwrap();
    }))
    .unwrap();
    assert_eq!(ev.current_state, PlaybackStateEnum::Paused);
    assert_eq!(
        ev.sampled_position.map(|p| p.position),
        Some(Nullable::Value(1_000))
    );
    assert_eq!((ev.duration, ev.audio_advance_unmuted), (None, Some(false)));
    assert!(
        StateChangedEvent::decode(&struct_of(&|_| {})).is_err(),
        "CurrentState is mandatory"
    );
}

// ---- M9-A3 B7: ContentLauncher -----------------------------------------------
//
// 1.4.2 ContentLauncher.xml. AcceptHeader is tv-app's video-player endpoint's
// (two "example" entries; master 5cd2917a and v1.4.2.0); the rest is
// synthetic. The request encoders' wire bytes are pinned against matter.js in
// byte_parity.rs.

#[test]
fn content_launcher_attributes_response_and_copied_enum() {
    use clusters::content_launcher::{
        self as cl, CharacteristicEnum, LauncherResponse, StatusEnum, SupportedProtocolsBitmap,
    };
    use clusters::media_playback::CharacteristicEnum as Original;
    assert_eq!(
        cl::decode_accept_header(&str_array_attr(&["example", "example"])).unwrap(),
        ["example", "example"]
    );
    assert_eq!(
        cl::decode_supported_streaming_protocols(&uint_attr(3)).unwrap(),
        SupportedProtocolsBitmap::DASH | SupportedProtocolsBitmap::HLS
    );
    let refused = LauncherResponse::decode(&struct_of(&|w| {
        w.put_uint(Tag::Context(0), 2).unwrap();
    }))
    .unwrap();
    assert_eq!(
        (refused.status, refused.data),
        (StatusEnum::AuthFailed, None)
    );
    // ContentLauncher's copy of MediaPlayback's CharacteristicEnum maps every
    // value to the same variant name as the original (`Debug` prints the
    // name, and `Unknown(18)` for the first value past the end). Comparing
    // `to_raw` alone could not fail: every enum gives `from_raw(x).to_raw() == x`.
    for raw in 0..=18 {
        assert_eq!(
            format!("{:?}", CharacteristicEnum::from_raw(raw)),
            format!("{:?}", Original::from_raw(raw))
        );
    }
    assert_eq!(
        CharacteristicEnum::from_raw(17),
        CharacteristicEnum::Karaoke
    );
    assert_eq!(
        CharacteristicEnum::from_raw(18),
        CharacteristicEnum::Unknown(18)
    );
}

#[test]
fn content_launcher_request_structs_decode_what_they_encode() {
    use clusters::content_launcher::{
        CharacteristicEnum, DimensionStruct, MetricTypeEnum, PlaybackPreferencesStruct,
        StyleInformationStruct, TrackPreferenceStruct,
    };
    // A DimensionStruct's doubles are TLV FLOAT64, and a FLOAT32 on the wire
    // is widened like chip's TLVReader::Get(double&).
    let style = StyleInformationStruct {
        image_url: None,
        color: Some("#FFFFFF".to_string()),
        size: Some(DimensionStruct {
            width: 0.5,
            height: 2.0,
            metric: MetricTypeEnum::Percentage,
        }),
    };
    let bytes = style.encode();
    assert_eq!(
        bytes,
        struct_of(&|w| {
            w.put_utf8(Tag::Context(1), "#FFFFFF").unwrap();
            w.start_structure(Tag::Context(2)).unwrap();
            w.put_double(Tag::Context(0), 0.5).unwrap();
            w.put_double(Tag::Context(1), 2.0).unwrap();
            w.put_uint(Tag::Context(2), 1).unwrap();
            w.end_container().unwrap();
        })
    );
    assert_eq!(StyleInformationStruct::decode(&bytes).unwrap(), style);
    let narrowed = DimensionStruct::decode(&struct_of(&|w| {
        w.put_float(Tag::Context(0), 0.5).unwrap();
        w.put_double(Tag::Context(1), 2.0).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
    }))
    .unwrap();
    assert_eq!(narrowed, style.size.clone().unwrap());
    // Absent, null and present preferences are three different structs.
    let track = TrackPreferenceStruct {
        language_code: "en".to_string(),
        characteristics: Some(Nullable::Null),
        audio_output_index: None,
    };
    for prefs in [
        PlaybackPreferencesStruct {
            playback_position: None,
            text_track: None,
            audio_tracks: None,
        },
        PlaybackPreferencesStruct {
            playback_position: Some(Nullable::Null),
            text_track: Some(Nullable::Null),
            audio_tracks: Some(Nullable::Null),
        },
        PlaybackPreferencesStruct {
            playback_position: Some(Nullable::Value(42)),
            text_track: Some(Nullable::Value(track.clone())),
            audio_tracks: Some(Nullable::Value(vec![TrackPreferenceStruct {
                characteristics: Some(Nullable::Value(vec![CharacteristicEnum::Caption])),
                audio_output_index: Some(Nullable::Value(1)),
                ..track.clone()
            }])),
        },
    ] {
        assert_eq!(
            PlaybackPreferencesStruct::decode(&prefs.encode()).unwrap(),
            prefs
        );
    }
}

// ---- M9-A3 B7: Channel --------------------------------------------------------
//
// 1.4.2 Channel.xml. The channel, lineup and program values, and the
// MultipleMatches response without data, are tv-app's ChannelManager (master
// 5cd2917a and v1.4.2.0). The null CurrentChannel (tv-app always sends a
// channel), the program's RecordingFlag, the external ids, the page tokens
// and the series info are synthetic.

/// A `ChannelInfoStruct` body: major, minor, and the three names tv-app sets.
fn channel_info(w: &mut TlvWriter<'_>, major: u64, minor: u64, name: &str) {
    w.put_uint(Tag::Context(0), major).unwrap();
    w.put_uint(Tag::Context(1), minor).unwrap();
    w.put_utf8(Tag::Context(2), name).unwrap();
    w.put_utf8(Tag::Context(3), "KAAL-TV").unwrap();
    w.put_utf8(Tag::Context(4), "KAAL").unwrap();
}

#[test]
fn channel_attributes_and_change_channel_response_decode() {
    use clusters::channel::{self as ch, ChangeChannelResponse, LineupInfoTypeEnum, StatusEnum};
    let list = ch::decode_channel_list(&list_of(&[&|w: &mut TlvWriter<'_>| {
        channel_info(w, 6, 0, "ABC");
    }]))
    .unwrap();
    assert_eq!(
        (
            list[0].major_number,
            list[0].name.as_deref(),
            list[0].identifier.as_deref(),
            list[0].r#type
        ),
        (6, Some("ABC"), None, None)
    );
    let Nullable::Value(lineup) = ch::decode_lineup(&struct_of(&|w| {
        w.put_utf8(Tag::Context(0), "Comcast").unwrap();
        w.put_utf8(Tag::Context(1), "Comcast King County").unwrap();
        w.put_utf8(Tag::Context(2), "98052").unwrap();
        w.put_uint(Tag::Context(3), 0).unwrap();
    }))
    .unwrap() else {
        panic!("Lineup decoded as null");
    };
    assert_eq!(
        (lineup.operator_name.as_str(), lineup.lineup_info_type),
        ("Comcast", LineupInfoTypeEnum::Mso)
    );
    assert_eq!(
        ch::decode_current_channel(&null_attr()).unwrap(),
        Nullable::Null
    );
    let multiple =
        ChangeChannelResponse::decode(&struct_of(&|w| w.put_uint(Tag::Context(0), 1).unwrap()))
            .unwrap();
    assert_eq!(
        (multiple.status, multiple.data),
        (StatusEnum::MultipleMatches, None)
    );
    assert_eq!(
        ch::encode_change_channel(&"PBS".to_string()),
        struct_of(&|w| w.put_utf8(Tag::Context(0), "PBS").unwrap())
    );
    assert_eq!(
        ch::encode_skip_channel(-3),
        struct_of(&|w| w.put_int(Tag::Context(0), -3).unwrap())
    );
}

#[test]
fn program_guide_response_decodes_nested_programs() {
    use clusters::channel::{ProgramGuideResponse, RecordingFlagBitmap};
    let response = ProgramGuideResponse::decode(&struct_of(&|w| {
        w.start_structure(Tag::Context(0)).unwrap();
        w.put_null(Tag::Context(0)).unwrap();
        w.start_structure(Tag::Context(1)).unwrap();
        w.put_uint(Tag::Context(0), 10).unwrap();
        w.put_utf8(Tag::Context(1), "next").unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.start_array(Tag::Context(1)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_utf8(Tag::Context(0), "progid-abc1").unwrap();
        w.start_structure(Tag::Context(1)).unwrap();
        channel_info(w, 6, 0, "ABC");
        w.end_container().unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap();
        w.put_uint(Tag::Context(3), 1800).unwrap();
        w.put_utf8(Tag::Context(4), "ABC Title1").unwrap();
        w.put_utf8(Tag::Context(5), "My Program Subtitle1").unwrap();
        w.put_uint(Tag::Context(14), 0b101).unwrap();
        w.start_structure(Tag::Context(15)).unwrap();
        w.put_utf8(Tag::Context(0), "1").unwrap();
        w.put_utf8(Tag::Context(1), "2").unwrap();
        w.end_container().unwrap();
        w.start_array(Tag::Context(18)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_utf8(Tag::Context(0), "imdb").unwrap();
        w.put_utf8(Tag::Context(1), "tt0001").unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
    }))
    .unwrap();
    assert_eq!(response.paging.previous_token, Some(Nullable::Null));
    assert!(
        matches!(&response.paging.next_token, Some(Nullable::Value(t)) if t.after.as_deref() == Some("next"))
    );
    let program = &response.program_list[0];
    assert_eq!(
        (
            program.identifier.as_str(),
            program.channel.major_number,
            program.end_time,
            program.title.as_str()
        ),
        ("progid-abc1", 6, 1800, "ABC Title1")
    );
    assert_eq!(
        program.recording_flag,
        Some(RecordingFlagBitmap::SCHEDULED | RecordingFlagBitmap::RECORDED)
    );
    assert!(matches!(&program.series_info, Some(Nullable::Value(s)) if s.episode == "2"));
    let ids = program.external_id_list.as_ref().unwrap();
    assert_eq!(
        (ids[0].name.as_str(), ids[0].value.as_str()),
        ("imdb", "tt0001")
    );
}

#[test]
fn get_program_guide_keeps_absent_and_null_page_tokens_apart() {
    use clusters::channel::encode_get_program_guide;
    let window = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 3600).unwrap();
    };
    assert_eq!(
        encode_get_program_guide(0, 3600, None, None, None, None, None),
        struct_of(&window)
    );
    assert_eq!(
        encode_get_program_guide(0, 3600, None, Some(Nullable::Null), None, None, None),
        struct_of(&|w| {
            window(w);
            w.put_null(Tag::Context(3)).unwrap();
        })
    );
    // A null PageToken and a null RecordingFlag (tag 5, the specification's
    // id): the bytes matter.js 0.16.11's TlvGetProgramGuideRequest writes for
    // the same request.
    assert_eq!(
        encode_get_program_guide(
            0,
            3600,
            None,
            Some(Nullable::Null),
            Some(Nullable::Null),
            None,
            None
        ),
        [0x15, 0x24, 0x00, 0x00, 0x25, 0x01, 0x10, 0x0e, 0x34, 0x03, 0x34, 0x05, 0x18]
    );
}

#[test]
fn channel_additional_info_copy_matches_content_launchers() {
    // Channel's AdditionalInfoStruct is a copy of ContentLauncher's: the two
    // must encode the same bytes and each decode the other's.
    use clusters::channel::AdditionalInfoStruct;
    use clusters::content_launcher::AdditionalInfoStruct as Original;
    let copy = AdditionalInfoStruct {
        name: "imdb".to_string(),
        value: "tt0001".to_string(),
    };
    let original = Original {
        name: "imdb".to_string(),
        value: "tt0001".to_string(),
    };
    assert_eq!(copy.encode(), original.encode());
    assert_eq!(
        AdditionalInfoStruct::decode(&original.encode()).unwrap(),
        copy
    );
    assert_eq!(Original::decode(&copy.encode()).unwrap(), original);
}

// ---- M9-A3 B7: ContentControl (provisional) ---------------------------------
//
// 1.4.2 ContentControl.xml, where the whole cluster is provisional. Only the
// FeatureMap is tv-app's (0x1C: BU, OCR, SCR; its `ram` default 28, master
// 5cd2917a and v1.4.2.0). Every other value here, scalar or list, request or
// response, is synthetic. tv-app's delegate is a stub, and chip's server
// registers no attribute-access handler, so tv-app's callback rating lists
// have no reader; its `ram` ScreenDailyTime has no default (and ST is not in
// its FeatureMap); its ResetPIN handler is empty, so it never sends a PIN
// (all master 5cd2917a and v1.4.2.0).

#[test]
fn content_control_attributes_decode() {
    use clusters::content_control::{self as cc, DayOfWeekBitmap, Feature};
    assert_eq!((Feature::BU | Feature::OCR | Feature::SCR).bits(), 0x1C);
    assert!(!cc::decode_enabled(&bool_attr(false)).unwrap());
    let rating = |name: &'static str, desc: Option<&'static str>| {
        move |w: &mut TlvWriter<'_>| {
            w.put_utf8(Tag::Context(0), name).unwrap();
            if let Some(d) = desc {
                w.put_utf8(Tag::Context(1), d).unwrap();
            }
        }
    };
    let ratings = cc::decode_on_demand_ratings(&list_of(&[
        &rating("G", Some("General")),
        &rating("PG", None),
    ]))
    .unwrap();
    let got: Vec<(&str, Option<&str>)> = ratings
        .iter()
        .map(|r| (r.rating_name.as_str(), r.rating_name_desc.as_deref()))
        .collect();
    assert_eq!(got, [("G", Some("General")), ("PG", None)]);
    assert_eq!(
        cc::decode_screen_daily_time(&uint_attr(3600)).unwrap(),
        3600
    );
    // A time window nests a list of time periods; its index is null until the
    // device assigns one.
    let windows = cc::decode_block_content_time_window(&list_of(&[&|w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 7).unwrap();
        w.put_uint(Tag::Context(1), 0b0100_0001).unwrap();
        w.start_array(Tag::Context(2)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        for (tag, v) in [(0, 22), (1, 0), (2, 23), (3, 59)] {
            w.put_uint(Tag::Context(tag), v).unwrap();
        }
        w.end_container().unwrap();
        w.end_container().unwrap();
    }]))
    .unwrap();
    assert_eq!(windows[0].time_window_index, Nullable::Value(7));
    assert_eq!(
        windows[0].day_of_week,
        DayOfWeekBitmap::SUNDAY | DayOfWeekBitmap::SATURDAY
    );
    assert_eq!(
        (
            windows[0].time_period[0].start_hour,
            windows[0].time_period[0].end_minute
        ),
        (22, 59)
    );
    let channels = cc::decode_block_channel_list(&list_of(&[&|w: &mut TlvWriter<'_>| {
        w.put_null(Tag::Context(0)).unwrap();
        w.put_uint(Tag::Context(1), 9).unwrap();
        w.put_uint(Tag::Context(2), 1).unwrap();
    }]))
    .unwrap();
    assert_eq!(
        (
            channels[0].block_channel_index,
            channels[0].major_number,
            channels[0].identifier.clone()
        ),
        (Nullable::Null, 9, None)
    );
}

#[test]
#[allow(clippy::manual_assert_eq)] // assert_eq! would print a content-control PIN on failure.
fn content_control_requests_encode_and_pin_response_is_secret() {
    use clusters::content_control::{
        self as cc, BlockChannelStruct, DayOfWeekBitmap, ResetPinResponse, TimePeriodStruct,
        TimeWindowStruct,
    };
    // UpdatePIN's payload carries two PINs, so it is compared with assert!.
    assert!(
        cc::encode_update_pin(
            &Secret::new("1234".to_string()),
            &Secret::new("5678".to_string())
        ) == struct_of(&|w| {
            w.put_utf8(Tag::Context(0), "1234").unwrap();
            w.put_utf8(Tag::Context(1), "5678").unwrap();
        }),
        "unexpected UpdatePIN payload"
    );
    // PINCode is optional; BonusTime is mandatory.
    assert_eq!(
        cc::encode_add_bonus_time(None, 60),
        struct_of(&|w| w.put_uint(Tag::Context(1), 60).unwrap())
    );
    // With a PIN the payload is compared with assert! too.
    assert!(
        cc::encode_add_bonus_time(Some(Secret::new("1234".to_string())), 60)
            == struct_of(&|w| {
                w.put_utf8(Tag::Context(0), "1234").unwrap();
                w.put_uint(Tag::Context(1), 60).unwrap();
            }),
        "unexpected AddBonusTime payload"
    );
    let channel = BlockChannelStruct {
        block_channel_index: Nullable::Null,
        major_number: 9,
        minor_number: 1,
        identifier: Some("PBS".to_string()),
    };
    assert_eq!(
        cc::encode_add_block_channels(&vec![channel]),
        struct_of(&|w| {
            w.start_array(Tag::Context(0)).unwrap();
            w.start_structure(Tag::Anonymous).unwrap();
            w.put_null(Tag::Context(0)).unwrap();
            w.put_uint(Tag::Context(1), 9).unwrap();
            w.put_uint(Tag::Context(2), 1).unwrap();
            w.put_utf8(Tag::Context(3), "PBS").unwrap();
            w.end_container().unwrap();
            w.end_container().unwrap();
        })
    );
    let window = TimeWindowStruct {
        time_window_index: Nullable::Null,
        day_of_week: DayOfWeekBitmap::MONDAY,
        time_period: vec![TimePeriodStruct {
            start_hour: 8,
            start_minute: 30,
            end_hour: 9,
            end_minute: 0,
        }],
    };
    assert_eq!(TimeWindowStruct::decode(&window.encode()).unwrap(), window);
    assert_eq!(
        cc::encode_remove_block_content_time_window(&vec![3, 4]),
        struct_of(&|w| {
            w.start_array(Tag::Context(0)).unwrap();
            w.put_uint(Tag::Anonymous, 3).unwrap();
            w.put_uint(Tag::Anonymous, 4).unwrap();
            w.end_container().unwrap();
        })
    );
    let reset = ResetPinResponse::decode(&struct_of(&|w| {
        w.put_utf8(Tag::Context(0), "424242").unwrap();
    }))
    .unwrap();
    // assert! rather than assert_eq!: a failure must not print the PIN.
    assert!(reset.pin_code == "424242");
    // The whole rendering is pinned: a one-field struct prints nothing else.
    let shown = format!("{reset:?}");
    assert!(
        shown == "ResetPinResponse { pin_code: \"<redacted>\" }",
        "unexpected ResetPinResponse rendering"
    );
    assert_redacted(&shown, b"424242");
    // `{:#?}` and `{:x?}` go through the same impl.
    assert_redacted(&format!("{reset:#?}"), b"424242");
    assert_redacted(&format!("{reset:x?}"), b"424242");
}
