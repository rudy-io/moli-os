//! Cross-verifies `matter-codec` against the captured TLV vectors.
//!
//! Loads `test-vectors/tlv/manifest.toml` (shipped in M0), iterates every
//! entry, encodes the structured `encode` block via [`TlvWriter`], asserts
//! byte equality with the matching `.bin`, then decodes the `.bin` via
//! [`TlvReader::read_value`] and asserts structural equality with the
//! constructed value.
//!
//! Vectors whose `encode` block uses kinds not yet implemented (strings,
//! bytes, containers, non-{anonymous,context} tags) are filtered out at
//! load time and counted as "skipped". Phase 2 and beyond will widen the
//! filter.

// Test code: unwrap/expect are allowed in tests with documented justification (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

use matter_codec::{Tag, TlvReader, TlvWriter, Value};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Manifest {
    vector: Vec<VectorEntry>,
}

#[derive(Debug, Deserialize)]
struct VectorEntry {
    id: String,
    #[allow(dead_code)]
    description: String,
    source: String,
    file: String,
    encode: Encode,
}

#[derive(Debug, Deserialize)]
struct Encode {
    tag: TagDesc,
    value: ValueDesc,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TagDesc {
    Anonymous,
    Context { number: u8 },
    CommonProfile { number: u32 },
    ImplicitProfile { number: u32 },
    FullyQualified { vendor: u16, profile: u16, tag: u32 },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ValueDesc {
    Bool {
        value: bool,
    },
    Null,
    Uint {
        #[allow(dead_code)]
        width: u8,
        value: UintLit,
    },
    Int {
        #[allow(dead_code)]
        width: u8,
        value: IntLit,
    },
    Float {
        width: u8,
        value: String,
    },
    Utf8 {
        value: String,
    },
    Bytes {
        value: String,
    },
    Structure {
        members: Vec<MemberDesc>,
    },
    Array {
        elements: Vec<ElementDesc>,
    },
    List {
        members: Vec<MemberDesc>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum UintLit {
    Direct(u64),
    Hex(String),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum IntLit {
    Direct(i64),
    Str(String),
}

#[derive(Debug, Deserialize)]
struct MemberDesc {
    tag: TagDesc,
    value: ValueDesc,
}

#[derive(Debug, Deserialize)]
struct ElementDesc {
    // Tag is present on the manifest's array element entries but
    // `Value::Array` discards it (the spec requires anonymous tags
    // on array children, which the reader produces by construction).
    // Kept #[allow(dead_code)] because TOML deserialization still
    // reads the field even though build_value's Array branch ignores it.
    #[allow(dead_code)]
    tag: TagDesc,
    value: ValueDesc,
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/matter-codec; go up two levels.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace")
        .to_path_buf()
}

fn load_manifest() -> Manifest {
    let path = workspace_root().join("test-vectors/tlv/manifest.toml");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    toml::from_str(&text).expect("parse manifest.toml")
}

fn load_bin(file: &str) -> Vec<u8> {
    let path = workspace_root().join("test-vectors/tlv").join(file);
    fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// build_tag keeps Result<_, &str> signature so the caller loop can use the
// same `match build_tag(...) { Ok => ..., Err => skip }` pattern uniformly,
// even though all arms are currently Ok. The lint fires because no arm is Err;
// future phases may add tag forms we don't yet support, keeping Err reachable.
#[allow(clippy::unnecessary_wraps)]
fn build_tag(desc: &TagDesc) -> Result<Tag, &'static str> {
    match desc {
        TagDesc::Anonymous => Ok(Tag::Anonymous),
        TagDesc::Context { number } => Ok(Tag::Context(*number)),
        TagDesc::CommonProfile { number } => Ok(Tag::CommonProfile(*number)),
        TagDesc::ImplicitProfile { number } => Ok(Tag::ImplicitProfile(*number)),
        TagDesc::FullyQualified {
            vendor,
            profile,
            tag,
        } => Ok(Tag::FullyQualified {
            vendor: *vendor,
            profile: *profile,
            tag: *tag,
        }),
    }
}

fn parse_uint(lit: &UintLit) -> u64 {
    match lit {
        UintLit::Direct(n) => *n,
        UintLit::Hex(s) => {
            let s = s.trim_start_matches("0x");
            u64::from_str_radix(s, 16).expect("hex uint literal")
        }
    }
}

fn parse_int(lit: &IntLit) -> i64 {
    match lit {
        IntLit::Direct(n) => *n,
        IntLit::Str(s) => s.parse::<i64>().expect("decimal i64 literal"),
    }
}

// Harness intentionally narrows f64 -> f32 for vectors that declared width = 4.
#[allow(clippy::cast_possible_truncation)]
fn build_value(desc: &ValueDesc) -> Result<Value, &'static str> {
    match desc {
        ValueDesc::Bool { value } => Ok(Value::Bool(*value)),
        ValueDesc::Null => Ok(Value::Null),
        ValueDesc::Uint { value, .. } => Ok(Value::Uint(parse_uint(value))),
        ValueDesc::Int { value, .. } => Ok(Value::Int(parse_int(value))),
        ValueDesc::Float { width, value } => {
            let parsed: f64 = value.parse().expect("float literal");
            match width {
                4 => Ok(Value::Float(parsed as f32)),
                8 => Ok(Value::Double(parsed)),
                _ => Err("float width != 4 or 8"),
            }
        }
        ValueDesc::Utf8 { value } => Ok(Value::Utf8(value.clone())),
        ValueDesc::Bytes { value } => {
            let bytes = hex_decode(value).map_err(|()| "bytes hex literal")?;
            Ok(Value::Bytes(bytes))
        }
        ValueDesc::Structure { members } => {
            let mut out = Vec::with_capacity(members.len());
            for m in members {
                let t = build_tag(&m.tag)?;
                let v = build_value(&m.value)?;
                out.push((t, v));
            }
            Ok(Value::Structure(out))
        }
        ValueDesc::Array { elements } => {
            let mut out = Vec::with_capacity(elements.len());
            for e in elements {
                // Tags on array elements are ignored — Value::Array stores
                // only values, and the spec requires every array element
                // to be anonymously tagged anyway. The reader produces
                // anonymous tags inside arrays, which match by construction.
                out.push(build_value(&e.value)?);
            }
            Ok(Value::Array(out))
        }
        ValueDesc::List { members } => {
            let mut out = Vec::with_capacity(members.len());
            for m in members {
                let t = build_tag(&m.tag)?;
                let v = build_value(&m.value)?;
                out.push((t, v));
            }
            Ok(Value::List(out))
        }
    }
}

#[allow(clippy::result_unit_err)]
fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    // Length is verified even above, so `as_chunks` leaves no remainder.
    let (pairs, _) = s.as_bytes().as_chunks::<2>();
    for chunk in pairs {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

#[allow(clippy::result_unit_err)]
fn hex_nibble(b: u8) -> Result<u8, ()> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(()),
    }
}

#[test]
fn all_phase_1_vectors_encode_and_decode_correctly() {
    let manifest = load_manifest();
    let mut processed = 0;
    let mut skipped = Vec::new();

    for entry in &manifest.vector {
        let tag = match build_tag(&entry.encode.tag) {
            Ok(t) => t,
            Err(reason) => {
                skipped.push((entry.id.clone(), reason));
                continue;
            }
        };
        let value = match build_value(&entry.encode.value) {
            Ok(v) => v,
            Err(reason) => {
                skipped.push((entry.id.clone(), reason));
                continue;
            }
        };

        let expected = load_bin(&entry.file);

        // Encode side
        let mut buf = Vec::with_capacity(expected.len());
        let mut w = TlvWriter::new(&mut buf);
        w.write_value(tag, &value).unwrap_or_else(|e| {
            panic!(
                "encode failed for {}: {e}\n  source: {}",
                entry.id, entry.source
            )
        });
        assert_eq!(
            buf, expected,
            "encode bytes differ for {} ({})\n  expected: {:02x?}\n  actual:   {:02x?}",
            entry.id, entry.source, expected, buf,
        );

        // Decode side
        let mut r = TlvReader::new(&expected);
        let (decoded_tag, decoded_value) = r
            .read_value()
            .unwrap_or_else(|e| panic!("decode failed for {}: {e}", entry.id));
        assert_eq!(decoded_tag, tag, "decoded tag differs for {}", entry.id);
        assert_eq!(
            decoded_value, value,
            "decoded value differs for {}",
            entry.id
        );
        assert!(
            r.is_empty(),
            "decoder did not consume all bytes for {}",
            entry.id
        );

        processed += 1;
    }

    // Sanity: we must have processed every vector in the manifest. M0
    // shipped 23 + 1 post-M0 (vector 0024 added in phase 2 as a
    // context-tag scalar); phase 3 closes the last skip category
    // (containers) so all 24 vectors are processable. The `>=` keeps
    // the assertion forward-compatible with future manifest additions.
    assert!(
        processed >= 24,
        "expected at least 24 vectors to be processed, got {processed}; skipped = {skipped:?}"
    );

    eprintln!(
        "processed {processed} vector(s); skipped {} (later phases):",
        skipped.len()
    );
    for (id, reason) in &skipped {
        eprintln!("  {id}: {reason}");
    }
}
