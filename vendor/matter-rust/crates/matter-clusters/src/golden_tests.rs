//! Runtime checks on the generated golden fixture (`golden.rs`).
//!
//! `codegen_golden` proves the generator's output matches and compiles; these
//! tests prove what that output *does*. They pin the fabric-sensitive encode
//! rule (a fabric-sensitive field is `Option`, and an encoder refuses a
//! withheld one) on the fixture's `ScopedEntryStruct`: `Secret` and
//! `Kind` are fabric-sensitive fields the dump relaxed for decoding only
//! (`mandatoryOnWrite`), `Alias` is fabric-sensitive but optional in the model
//! itself, and `Note` is an ordinary optional field.

// Test code: unwrap is allowed in tests (CONTRIBUTING.md); a failure here is a test failure.
#![allow(clippy::unwrap_used)]

use crate::error::ClusterError;
use crate::golden::{ModeEnum, ScopedEntryStruct};
use crate::types::Nullable;
use matter_codec::TlvWriter;

/// What a server stores for an entry it was sent: the written bytes plus the
/// accessing fabric's `FabricIndex` (tag 254, a one-byte unsigned integer)
/// before the structure's end. The write form leaves it out, as chip does.
fn with_fabric_index(written: &[u8], fabric_index: u8) -> Vec<u8> {
    let (end, body) = written.split_last().unwrap();
    assert_eq!(
        *end, 0x18,
        "an anonymous structure ends with end-of-container"
    );
    assert!(
        !written.windows(2).any(|w| w == [0x24, 0xFE]),
        "the write form carries no FabricIndex: {written:02x?}"
    );
    let mut stored = body.to_vec();
    stored.extend_from_slice(&[0x24, 0xFE, fabric_index, 0x18]);
    stored
}

/// An entry as our own fabric reads it: every sensitive field present.
fn own_entry() -> ScopedEntryStruct {
    ScopedEntryStruct {
        secret: Some(7),
        kind: Some(Nullable::Value(ModeEnum::Manual)),
        note: None,
        alias: None,
        fabric_index: 2,
    }
}

#[test]
fn sensitive_struct_with_every_sensitive_field_encodes_and_roundtrips() {
    let bytes = own_entry().encode().unwrap();
    assert_eq!(
        ScopedEntryStruct::decode(&with_fabric_index(&bytes, 2)).unwrap(),
        own_entry()
    );
}

#[test]
fn sensitive_struct_refuses_a_missing_sensitive_scalar() {
    let entry = ScopedEntryStruct {
        secret: None,
        ..own_entry()
    };
    assert!(matches!(
        entry.encode(),
        Err(ClusterError::MissingField("Secret"))
    ));
}

#[test]
fn sensitive_struct_refuses_a_missing_sensitive_nullable_enum() {
    let entry = ScopedEntryStruct {
        kind: None,
        ..own_entry()
    };
    assert!(matches!(
        entry.encode(),
        Err(ClusterError::MissingField("Kind"))
    ));
}

#[test]
fn sensitive_nullable_field_set_to_null_is_present_not_missing() {
    // `Some(Null)` is a value the caller chose to send; only `None` (withheld)
    // is refused.
    let entry = ScopedEntryStruct {
        kind: Some(Nullable::Null),
        ..own_entry()
    };
    assert!(entry.encode().is_ok());
}

#[test]
fn non_sensitive_optional_field_may_still_be_absent() {
    assert!(own_entry().note.is_none());
    assert!(own_entry().encode().is_ok());
}

#[test]
fn model_optional_sensitive_field_may_be_absent_on_write() {
    // Only fields the dump relaxed are required on write; a sensitive field
    // that is optional in the model itself keeps ordinary omit-on-None.
    assert!(own_entry().alias.is_none());
    let with_alias = ScopedEntryStruct {
        alias: Some("hall".to_string()),
        ..own_entry()
    };
    let bytes = with_alias.encode().unwrap();
    assert_eq!(
        ScopedEntryStruct::decode(&with_fabric_index(&bytes, 2)).unwrap(),
        with_alias
    );
}

#[test]
fn refused_write_fields_writes_nothing() {
    // Every sensitive field is checked before the first write, so a refusal
    // cannot leave a half-written structure in the caller's buffer.
    let entry = ScopedEntryStruct {
        kind: None,
        ..own_entry()
    };
    let mut buf = Vec::new();
    {
        let mut w = TlvWriter::new(&mut buf);
        assert!(entry.write_fields(&mut w).is_err());
    }
    assert!(buf.is_empty(), "refused write_fields wrote {buf:02x?}");
}

// ---- M9-A3 B7: nested structs and guarded lists in an encoded struct --------
//
// The fixture's `NestStruct` (reached by the `Nest` request) carries a nested
// struct in each guard form (mandatory, optional, nullable, and optional and
// nullable), a list of structs that is optional and nullable, an optional list
// of integers, a nullable list of enums and a `double`: the shapes
// ContentLauncher's request structs need. Every expected buffer below is built
// element by element with `TlvWriter` primitives, never with the generated
// writer under test.

use crate::golden::{NestStruct, PointStruct};
use matter_codec::Tag;

fn point(x: u16, y: u16) -> PointStruct {
    PointStruct { x, y }
}

/// Every optional field present and every nullable field a value.
fn full_nest() -> NestStruct {
    NestStruct {
        point: point(1, 2),
        maybe_point: Some(point(3, 4)),
        null_point: Nullable::Value(point(5, 6)),
        points: Some(Nullable::Value(vec![point(7, 8), point(9, 10)])),
        ids: Some(vec![11, 12]),
        modes: Nullable::Value(vec![ModeEnum::Auto, ModeEnum::Manual]),
        scale: 1.5,
        track: Some(Nullable::Value(point(13, 14))),
    }
}

/// The same struct with every optional field absent and every nullable one
/// null.
fn sparse_nest() -> NestStruct {
    NestStruct {
        point: point(1, 2),
        maybe_point: None,
        null_point: Nullable::Null,
        points: None,
        ids: None,
        modes: Nullable::Null,
        scale: -0.25,
        track: None,
    }
}

/// `PointStruct { x, y }` written by hand at `tag`.
fn write_point(w: &mut TlvWriter<'_>, tag: Tag, x: u64, y: u64) {
    w.start_structure(tag).unwrap();
    w.put_uint(Tag::Context(0), x).unwrap();
    w.put_uint(Tag::Context(1), y).unwrap();
    w.end_container().unwrap();
}

/// The fields of `full_nest()`, written by hand into an open container.
fn write_full_nest_fields(w: &mut TlvWriter<'_>) {
    write_point(w, Tag::Context(0), 1, 2);
    write_point(w, Tag::Context(1), 3, 4);
    write_point(w, Tag::Context(2), 5, 6);
    w.start_array(Tag::Context(3)).unwrap();
    write_point(w, Tag::Anonymous, 7, 8);
    write_point(w, Tag::Anonymous, 9, 10);
    w.end_container().unwrap();
    w.start_array(Tag::Context(4)).unwrap();
    w.put_uint(Tag::Anonymous, 11).unwrap();
    w.put_uint(Tag::Anonymous, 12).unwrap();
    w.end_container().unwrap();
    w.start_array(Tag::Context(5)).unwrap();
    w.put_uint(Tag::Anonymous, 0).unwrap();
    w.put_uint(Tag::Anonymous, 1).unwrap();
    w.end_container().unwrap();
    w.put_double(Tag::Context(6), 1.5).unwrap();
    write_point(w, Tag::Context(7), 13, 14);
}

/// `sparse_nest()` with `points` and `track` as given, written by hand:
/// `points_null` and `track_null` add a TLV null at tag 3 and tag 7, and
/// `track_point` a `PointStruct` at tag 7.
fn sparse_nest_bytes(
    points_null: bool,
    track_null: bool,
    track_point: Option<(u64, u64)>,
) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = TlvWriter::new(&mut out);
        w.start_structure(Tag::Anonymous).unwrap();
        write_point(&mut w, Tag::Context(0), 1, 2);
        w.put_null(Tag::Context(2)).unwrap();
        if points_null {
            w.put_null(Tag::Context(3)).unwrap();
        }
        w.put_null(Tag::Context(5)).unwrap();
        w.put_double(Tag::Context(6), -0.25).unwrap();
        if track_null {
            w.put_null(Tag::Context(7)).unwrap();
        }
        if let Some((x, y)) = track_point {
            write_point(&mut w, Tag::Context(7), x, y);
        }
        w.end_container().unwrap();
    }
    out
}

#[test]
fn nested_struct_fields_and_guarded_lists_encode_field_by_field() {
    // Each nested struct is a sub-structure at its context tag, each list an
    // array of anonymous elements, and `scale` a FLOAT64.
    let mut want = Vec::new();
    {
        let mut w = TlvWriter::new(&mut want);
        w.start_structure(Tag::Anonymous).unwrap();
        write_full_nest_fields(&mut w);
        w.end_container().unwrap();
    }
    assert_eq!(full_nest().encode(), want);
    assert_eq!(NestStruct::decode(&want).unwrap(), full_nest());
}

#[test]
fn absent_and_null_composite_fields_encode_differently() {
    // An absent optional field is left out; a null one is a TLV null at its
    // tag. Here MaybePoint, Points, Ids and Track are absent, NullPoint and
    // Modes null.
    let want = sparse_nest_bytes(false, false, None);
    assert_eq!(sparse_nest().encode(), want);
    assert_eq!(NestStruct::decode(&want).unwrap(), sparse_nest());
    // A null list of structs is not an absent one: a TLV null at tag 3.
    let null_points = NestStruct {
        points: Some(Nullable::Null),
        ..sparse_nest()
    };
    let want_null_points = sparse_nest_bytes(true, false, None);
    assert_eq!(null_points.encode(), want_null_points);
    assert_eq!(NestStruct::decode(&want_null_points).unwrap(), null_points);
}

#[test]
fn an_optional_nullable_nested_struct_is_absent_null_or_a_sub_structure() {
    // ContentLauncher `PlaybackPreferencesStruct.TextTrack`'s shape
    // (`Option<Nullable<Struct>>`): absent is left out, null is a TLV null at
    // tag 7, a value is a sub-structure at tag 7.
    let cases = [
        (None, sparse_nest_bytes(false, false, None)),
        (Some(Nullable::Null), sparse_nest_bytes(false, true, None)),
        (
            Some(Nullable::Value(point(13, 14))),
            sparse_nest_bytes(false, false, Some((13, 14))),
        ),
    ];
    for (track, want) in cases {
        let nest = NestStruct {
            track,
            ..sparse_nest()
        };
        assert_eq!(nest.encode(), want, "{nest:?}");
        assert_eq!(NestStruct::decode(&want).unwrap(), nest);
    }
}

#[test]
fn the_nest_request_carries_the_struct_at_its_context_tag() {
    // The command encoder opens the request field as a sub-structure at
    // context tag 0 and writes the nested fields inside it (the generated
    // `encode_nest`).
    let bytes = crate::golden::encode_nest(full_nest());
    let mut want = Vec::new();
    {
        let mut w = TlvWriter::new(&mut want);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_structure(Tag::Context(0)).unwrap();
        write_full_nest_fields(&mut w);
        w.end_container().unwrap();
        w.end_container().unwrap();
    }
    assert_eq!(bytes, want);
}

#[test]
fn a_secret_request_argument_encodes_its_value_and_debug_hides_it() {
    // `Unlock` (`Code` a secret string, `Pin` an optional secret byte string,
    // `Hint` plain): the wrapper changes no byte, and `{:?}` of the argument
    // prints the placeholder.
    use crate::golden::encode_unlock;
    use crate::types::Secret;
    let code = Secret::new("2468".to_string());
    let pin: Secret<Vec<u8>> = vec![1, 2, 3].into();
    assert_eq!(format!("{code:?} {pin:?}"), r#""<redacted>" "<redacted>""#);
    let want = |pin: Option<&[u8]>| {
        let mut want = Vec::new();
        let mut w = TlvWriter::new(&mut want);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_utf8(Tag::Context(0), "2468").unwrap();
        if let Some(pin) = pin {
            w.put_bytes(Tag::Context(1), pin).unwrap();
        }
        w.put_utf8(Tag::Context(2), "front door").unwrap();
        w.end_container().unwrap();
        want
    };
    let hint = "front door".to_string();
    assert_eq!(
        encode_unlock(&code, Some(pin), &hint),
        want(Some(&[1, 2, 3]))
    );
    assert_eq!(encode_unlock(&code, None, &hint), want(None));
}
