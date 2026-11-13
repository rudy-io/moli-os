//! Property: `decode(encode(v)) == v` for arbitrary `(Tag, Value)`.
//!
//! Phase 4 of matter-codec. Catches encoder/decoder asymmetries that the
//! M0 vector harness can't see because the vectors are a finite hand-picked
//! sample. The proptest strategies generate values with bounded depth
//! (≤ 4) and bounded string/bytes length (≤ 64 chars/bytes).
//!
//! Floats exclude NaN because `NaN != NaN` would make the equality
//! assertion meaningless. Infinity is allowed (`Inf == Inf` is true).

// Test code: unwrap/expect are allowed in tests with documented justification (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use matter_codec::{Element, Tag, TlvReader, TlvWriter, Value};
use proptest::prelude::*;

fn arb_tag() -> impl Strategy<Value = Tag> {
    prop_oneof![
        Just(Tag::Anonymous),
        any::<u8>().prop_map(Tag::Context),
        any::<u32>().prop_map(Tag::CommonProfile),
        any::<u32>().prop_map(Tag::ImplicitProfile),
        (any::<u16>(), any::<u16>(), any::<u32>()).prop_map(|(vendor, profile, tag)| {
            Tag::FullyQualified {
                vendor,
                profile,
                tag,
            }
        }),
    ]
}

/// Generates an arbitrary scalar `Value`. Used as the leaf for the
/// recursive container strategy, `arb_value`.
fn arb_scalar_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<bool>().prop_map(Value::Bool),
        any::<u64>().prop_map(Value::Uint),
        any::<i64>().prop_map(Value::Int),
        any::<f32>()
            .prop_filter("no NaN", |f| !f.is_nan())
            .prop_map(Value::Float),
        any::<f64>()
            .prop_filter("no NaN", |f| !f.is_nan())
            .prop_map(Value::Double),
        Just(Value::Null),
        // Exclude IS1 (0x1F): it is the Matter localized-string separator, so
        // the reader presents only the text before it. A value
        // containing a raw 0x1F is therefore not in the codec's round-trip
        // domain — `decode(encode("a\u{1F}b")) == "a"`, by design.
        prop::string::string_regex("[^\u{1F}]{0,64}")
            .unwrap()
            .prop_map(Value::Utf8),
        prop::collection::vec(any::<u8>(), 0..=64).prop_map(Value::Bytes),
    ]
}

/// Generates an arbitrary `Value`, scalar or container, bounded to depth
/// 4. Container children also use this strategy via `prop_recursive`.
fn arb_value() -> impl Strategy<Value = Value> {
    arb_scalar_value().prop_recursive(
        4,  // max depth
        32, // max total nodes
        4,  // max children per container
        |inner| {
            prop_oneof![
                prop::collection::vec((arb_tag(), inner.clone()), 0..=4).prop_map(Value::Structure),
                prop::collection::vec(inner.clone(), 0..=4).prop_map(Value::Array),
                prop::collection::vec((arb_tag(), inner), 0..=4).prop_map(Value::List),
            ]
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Scalar round-trip identity.
    #[test]
    fn scalar_round_trip(tag in arb_tag(), value in arb_scalar_value()) {
        let mut buf = Vec::new();
        TlvWriter::new(&mut buf).write_value(tag, &value).unwrap();
        let (decoded_tag, decoded_value) = TlvReader::new(&buf).read_value().unwrap();
        prop_assert_eq!(decoded_tag, tag);
        prop_assert_eq!(decoded_value, value);
    }

    /// Full round-trip identity including containers up to depth 4.
    #[test]
    fn full_round_trip(tag in arb_tag(), value in arb_value()) {
        let mut buf = Vec::new();
        TlvWriter::new(&mut buf).write_value(tag, &value).unwrap();
        let (decoded_tag, decoded_value) = TlvReader::new(&buf).read_value().unwrap();
        prop_assert_eq!(decoded_tag, tag);
        // For Array specifically: the writer forces Tag::Anonymous on
        // every element regardless of input, and the reader discards
        // child tags. Both directions agree because `value` is the
        // outer Value, not the inner element tags. The structural
        // recursion bottoms out at scalars where tags are non-existent
        // (scalars carry no tag of their own — the tag belongs to the
        // element slot, supplied by the container parent).
        prop_assert_eq!(decoded_value, value);
    }

    /// `skip_container` stops exactly at the container boundary: encode an
    /// anonymous struct (arbitrary uint fields + one nested struct) followed
    /// by a sentinel scalar; opening the struct and skipping it must leave
    /// the reader precisely at the sentinel. Public-API only (no internal
    /// cursor access).
    #[test]
    fn skip_container_stops_at_boundary(fields in proptest::collection::vec(any::<u64>(), 0..16)) {
        let mut buf = Vec::new();
        {
            let mut w = TlvWriter::new(&mut buf);
            // the container to skip:
            w.start_structure(Tag::Anonymous).unwrap();
            for (i, v) in fields.iter().enumerate() {
                let ctx = u8::try_from(i % 200).unwrap();
                w.put_uint(Tag::Context(ctx), *v).unwrap();
            }
            w.start_structure(Tag::Context(200)).unwrap();
            w.put_uint(Tag::Anonymous, 1).unwrap();
            w.end_container().unwrap();
            w.end_container().unwrap();
            // the sentinel that must come next:
            w.put_uint(Tag::Context(7), 0xDEAD).unwrap();
        }
        let mut r = TlvReader::new(&buf);
        let opened_container = matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        );
        prop_assert!(opened_container);
        r.skip_container().unwrap();
        let at_sentinel = matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(7),
                value: Value::Uint(0xDEAD)
            })
        );
        prop_assert!(at_sentinel);
    }

    /// The borrowed streaming walk is the owned walk, modulo ownership:
    /// for every encoded (tag, value), `next_ref()` yields the same element
    /// sequence as `next()` (compared through `Element::from`).
    #[test]
    fn next_ref_equals_next_modulo_ownership(tag in arb_tag(), value in arb_value()) {
        let mut buf = Vec::new();
        TlvWriter::new(&mut buf).write_value(tag, &value).unwrap();
        let mut owned = TlvReader::new(&buf);
        let mut byref = TlvReader::new(&buf);
        loop {
            let o = owned.next().unwrap();
            let r = byref.next_ref().unwrap();
            match (o, r) {
                (None, None) => break,
                (Some(o), Some(r)) => prop_assert_eq!(o, Element::from(r)),
                (o, r) => prop_assert!(false, "walk mismatch: {:?} vs {:?}", o, r),
            }
        }
    }

    /// `finish` accepts exactly one complete top-level structure: the whole
    /// encoding passes, every strict prefix (a truncated message) fails, and
    /// any non-empty suffix (trailing data) fails with `TrailingData`. The
    /// strict close chip's `MessageParser::ExitContainer` enforces.
    #[test]
    fn finish_accepts_exactly_one_complete_struct(
        members in prop::collection::vec((arb_tag(), arb_value()), 0..=4),
        suffix in prop::collection::vec(any::<u8>(), 1..=8),
    ) {
        let mut buf = Vec::new();
        TlvWriter::new(&mut buf)
            .write_value(Tag::Anonymous, &Value::Structure(members))
            .unwrap();
        // Open the struct, then let `finish` walk the rest.
        let strict = |bytes: &[u8]| -> Result<(), matter_codec::Error> {
            let mut r = TlvReader::new(bytes);
            r.next()?;
            r.finish()
        };
        prop_assert!(strict(&buf).is_ok());
        for n in 1..buf.len() {
            prop_assert!(strict(&buf[..n]).is_err(), "prefix of {} / {} bytes accepted", n, buf.len());
        }
        let mut tailed = buf.clone();
        tailed.extend_from_slice(&suffix);
        prop_assert!(matches!(strict(&tailed), Err(matter_codec::Error::TrailingData)));
    }
}
