//! Forward-compatibility: decoders must tolerate fields/elements a newer
//! Matter revision adds (unknown nested containers, unknown bits) without
//! losing the fields they DO understand. Fixtures are hand-built synthetic
//! TLV (matter.js models the same revision we generate from, so it cannot
//! emit "a future field" — there is no capture source).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use matter_clusters::clusters::descriptor::{decode_device_type_list, DeviceTypeStruct};
use matter_clusters::clusters::occupancy_sensing::decode_occupancy;
use matter_codec::{Tag, TlvWriter};

/// `{ ctx0=u(0x1234_5678), <unknown ctx99 struct{ctx0=u(1)}>, ctx1=u(7) }`
fn device_type_with_unknown_nested() -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous).unwrap();
    w.put_uint(Tag::Context(0), 0x1234_5678).unwrap();
    // an unknown field a newer revision added, encoded as a nested struct:
    w.start_structure(Tag::Context(99)).unwrap();
    w.put_uint(Tag::Context(0), 1).unwrap();
    w.end_container().unwrap();
    w.put_uint(Tag::Context(1), 7).unwrap();
    w.end_container().unwrap();
    buf
}

#[test]
fn struct_decode_skips_unknown_nested_container() {
    let buf = device_type_with_unknown_nested();
    let d = DeviceTypeStruct::decode(&buf).expect("must decode despite unknown field");
    assert_eq!(d.device_type, 0x1234_5678);
    assert_eq!(d.revision, 7); // the field AFTER the unknown container survives
}

#[test]
fn list_decode_skips_unknown_container_element() {
    // an array of DeviceTypeStruct where an unknown nested array appears as a
    // stray element between two valid structs.
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_array(Tag::Anonymous).unwrap();
    // valid struct #1
    w.start_structure(Tag::Anonymous).unwrap();
    w.put_uint(Tag::Context(0), 10).unwrap();
    w.put_uint(Tag::Context(1), 1).unwrap();
    w.end_container().unwrap();
    // unknown non-struct element a newer revision might add:
    w.start_array(Tag::Anonymous).unwrap();
    w.put_uint(Tag::Anonymous, 5).unwrap();
    w.end_container().unwrap();
    // valid struct #2
    w.start_structure(Tag::Anonymous).unwrap();
    w.put_uint(Tag::Context(0), 20).unwrap();
    w.put_uint(Tag::Context(1), 2).unwrap();
    w.end_container().unwrap();
    w.end_container().unwrap();

    let list = decode_device_type_list(&buf).expect("must decode despite unknown element");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].device_type, 10);
    assert_eq!(list[1].device_type, 20);
}

#[test]
fn bitmap_decode_retains_unknown_bits() {
    // OccupancyBitmap defines bit0 (Occupied). 0b1000_0001 sets bit0 plus an
    // unknown reserved bit7 a newer revision might define.
    let raw: u8 = 0b1000_0001;
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::Anonymous, u64::from(raw)).unwrap();
    }
    let bm = decode_occupancy(&buf).expect("decodes");
    assert_eq!(
        bm.bits(),
        raw,
        "the unknown bit7 must survive the round-trip"
    );
}
