//! Matter TLV element values.
//!
//! Phase 3 of `matter-codec` adds container variants. The full TLV value
//! space is now represented.

use crate::tag::Tag;

/// A decoded Matter TLV value, collapsed across wire widths.
///
/// Integer widths and float widths are erased from the public type — the
/// encoder chooses the minimal wire width per the spec, and the decoder
/// produces the same Rust type regardless of the width the bytes used. If
/// you need exact-byte round-trip for non-minimal inputs, use the reader's
/// span APIs ([`crate::TlvReader::element_span`] /
/// [`crate::TlvReader::skip_container_span`] + [`crate::TlvReader::span_bytes`]),
/// which expose an element's raw bytes for width-preserving re-emission.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// A boolean.
    Bool(bool),

    /// An unsigned integer, encoded on the wire in 1, 2, 4, or 8 bytes
    /// (minimal width).
    Uint(u64),

    /// A signed integer, encoded on the wire in 1, 2, 4, or 8 bytes
    /// (minimal width).
    Int(i64),

    /// A 4-byte IEEE 754 single-precision float.
    Float(f32),

    /// An 8-byte IEEE 754 double-precision float.
    Double(f64),

    /// A UTF-8 string. The wire format is a 1/2/4/8-byte little-endian
    /// length field (writer picks the minimal width) followed by the
    /// raw UTF-8 bytes. The reader rejects invalid UTF-8 with
    /// [`crate::Error::InvalidUtf8`].
    Utf8(String),

    /// An octet string. The wire format is a 1/2/4/8-byte little-endian
    /// length field (writer picks the minimal width) followed by the
    /// raw bytes.
    Bytes(Vec<u8>),

    /// A structure. Each member carries its own tag; members are
    /// typically context-tagged but the spec permits any non-anonymous
    /// form.
    Structure(Vec<(Tag, Value)>),

    /// An array. Elements share a single type; the spec requires every
    /// element to carry an anonymous tag, which the reader enforces and
    /// the writer always emits.
    Array(Vec<Value>),

    /// A list. Members may carry any tag form (including anonymous), and
    /// member types are not required to be uniform.
    List(Vec<(Tag, Value)>),

    /// The TLV null value (element type `0x14`).
    Null,
}

/// A borrowed view of one decoded scalar TLV value — the zero-copy sibling
/// of [`Value`]. Strings and byte strings borrow directly from the reader's
/// input; scalars are carried by value. Containers never appear here: the
/// streaming [`crate::TlvReader::next_ref`] API reports them as
/// `ContainerStart`/`ContainerEnd` events, so no owned children are built.
///
/// `Utf8` carries the same IS1-truncated text the owned path presents (the
/// text before the first `0x1F` localized-string separator); access to the
/// raw suffix (LSID) remains a separate additive follow-up.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum ValueRef<'a> {
    /// A boolean.
    Bool(bool),
    /// An unsigned integer (any wire width).
    Uint(u64),
    /// A signed integer (any wire width).
    Int(i64),
    /// A 4-byte IEEE 754 single-precision float.
    Float(f32),
    /// An 8-byte IEEE 754 double-precision float.
    Double(f64),
    /// A UTF-8 string borrowing the reader's input (IS1-truncated text).
    Utf8(&'a str),
    /// An octet string borrowing the reader's input.
    Bytes(&'a [u8]),
    /// The TLV null value.
    Null,
}

impl From<ValueRef<'_>> for Value {
    // Called once per materialised element by the reader's tree builder (and
    // by every cross-crate `next()` caller through `Element::from`), so it is
    // inlined: the discriminant match then folds into the caller's own match
    // and only the string/bytes arms keep their allocation.
    #[inline]
    fn from(v: ValueRef<'_>) -> Self {
        match v {
            ValueRef::Bool(b) => Value::Bool(b),
            ValueRef::Uint(n) => Value::Uint(n),
            ValueRef::Int(n) => Value::Int(n),
            ValueRef::Float(f) => Value::Float(f),
            ValueRef::Double(f) => Value::Double(f),
            ValueRef::Utf8(s) => Value::Utf8(String::from(s)),
            ValueRef::Bytes(b) => Value::Bytes(b.to_vec()),
            ValueRef::Null => Value::Null,
        }
    }
}
