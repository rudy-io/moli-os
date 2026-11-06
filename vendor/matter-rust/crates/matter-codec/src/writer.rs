//! Streaming TLV encoder. Appends to a caller-provided `Vec<u8>`.

use crate::error::{Error, Result};
use crate::reader::MAX_DEPTH;
use crate::tag::Tag;
use crate::value::Value;
use crate::{element_type as et, tag_control as tc};

/// Stack-buffer size for one element header plus the widest fixed
/// payload: control octet (1) + fully-qualified-8 tag (8) + 8-byte
/// scalar payload or length field.
const MAX_HEADER: usize = 17;

/// A streaming TLV encoder that appends to a caller-provided `Vec<u8>`.
pub struct TlvWriter<'a> {
    out: &'a mut Vec<u8>,
}

/// Assemble the control octet + tag bytes for `tag` into the front of `buf`,
/// returning the byte count (1..=9). Byte layout is identical to what the
/// old per-push `write_tag` emitted; the golden vectors pin it.
#[inline]
fn encode_tag(tag: Tag, element_type: u8, buf: &mut [u8; MAX_HEADER]) -> usize {
    match tag {
        Tag::Anonymous => {
            buf[0] = tc::ANONYMOUS | element_type;
            1
        }
        Tag::Context(n) => {
            buf[0] = tc::CONTEXT | element_type;
            buf[1] = n;
            2
        }
        Tag::CommonProfile(n) => {
            if let Ok(n16) = u16::try_from(n) {
                buf[0] = tc::COMMON_PROFILE_2 | element_type;
                buf[1..3].copy_from_slice(&n16.to_le_bytes());
                3
            } else {
                buf[0] = tc::COMMON_PROFILE_4 | element_type;
                buf[1..5].copy_from_slice(&n.to_le_bytes());
                5
            }
        }
        Tag::ImplicitProfile(n) => {
            if let Ok(n16) = u16::try_from(n) {
                buf[0] = tc::IMPLICIT_PROFILE_2 | element_type;
                buf[1..3].copy_from_slice(&n16.to_le_bytes());
                3
            } else {
                buf[0] = tc::IMPLICIT_PROFILE_4 | element_type;
                buf[1..5].copy_from_slice(&n.to_le_bytes());
                5
            }
        }
        Tag::FullyQualified {
            vendor,
            profile,
            tag,
        } => {
            buf[1..3].copy_from_slice(&vendor.to_le_bytes());
            buf[3..5].copy_from_slice(&profile.to_le_bytes());
            if let Ok(tag16) = u16::try_from(tag) {
                buf[0] = tc::FULLY_QUALIFIED_6 | element_type;
                buf[5..7].copy_from_slice(&tag16.to_le_bytes());
                7
            } else {
                buf[0] = tc::FULLY_QUALIFIED_8 | element_type;
                buf[5..9].copy_from_slice(&tag.to_le_bytes());
                9
            }
        }
    }
}

impl<'a> TlvWriter<'a> {
    /// Construct a writer that appends to `out`. The writer borrows `out`
    /// mutably; release the borrow by dropping the writer.
    #[inline]
    pub fn new(out: &'a mut Vec<u8>) -> Self {
        Self { out }
    }

    /// Write a control octet + tag bytes as a single append.
    #[inline]
    fn write_tag(&mut self, tag: Tag, element_type: u8) {
        let mut buf = [0u8; MAX_HEADER];
        let n = encode_tag(tag, element_type, &mut buf);
        self.out.extend_from_slice(&buf[..n]);
    }

    /// Header + fixed-width payload assembled in one stack buffer, single
    /// `extend_from_slice`. `payload` is at most 8 bytes (widest scalar), so
    /// `n + payload.len() <= 17` always holds.
    #[inline]
    fn put_scalar(&mut self, tag: Tag, element_type: u8, payload: &[u8]) {
        let mut buf = [0u8; MAX_HEADER];
        let n = encode_tag(tag, element_type, &mut buf);
        buf[n..n + payload.len()].copy_from_slice(payload);
        self.out.extend_from_slice(&buf[..n + payload.len()]);
    }

    /// Emit a boolean value with the given tag.
    ///
    /// # Errors
    ///
    /// Currently infallible; returns `Ok(())` always. The `Result` return
    /// type is reserved for future I/O-backed writers.
    #[inline]
    pub fn put_bool(&mut self, tag: Tag, v: bool) -> Result<()> {
        let et = if v { et::BOOL_TRUE } else { et::BOOL_FALSE };
        self.put_scalar(tag, et, &[]);
        Ok(())
    }

    /// Emit a null value with the given tag.
    ///
    /// # Errors
    ///
    /// Currently infallible; returns `Ok(())` always. The `Result` return
    /// type is reserved for future I/O-backed writers.
    #[inline]
    pub fn put_null(&mut self, tag: Tag) -> Result<()> {
        self.put_scalar(tag, et::NULL, &[]);
        Ok(())
    }

    /// Emit an unsigned integer with the given tag. The minimum-width
    /// encoding (1, 2, 4, or 8 bytes) is chosen automatically per
    /// Matter Core Spec §A.2.
    ///
    /// # Errors
    ///
    /// Currently infallible; returns `Ok(())` always. The `Result` return
    /// type is reserved for future I/O-backed writers.
    #[inline]
    pub fn put_uint(&mut self, tag: Tag, v: u64) -> Result<()> {
        if let Ok(n) = u8::try_from(v) {
            self.put_scalar(tag, et::UINT8, &n.to_le_bytes());
        } else if let Ok(n) = u16::try_from(v) {
            self.put_scalar(tag, et::UINT16, &n.to_le_bytes());
        } else if let Ok(n) = u32::try_from(v) {
            self.put_scalar(tag, et::UINT32, &n.to_le_bytes());
        } else {
            self.put_scalar(tag, et::UINT64, &v.to_le_bytes());
        }
        Ok(())
    }

    /// Emit a signed integer with the given tag. The minimum-width
    /// encoding (1, 2, 4, or 8 bytes) is chosen automatically per
    /// Matter Core Spec §A.2.
    ///
    /// # Errors
    ///
    /// Currently infallible; returns `Ok(())` always. The `Result` return
    /// type is reserved for future I/O-backed writers.
    #[inline]
    pub fn put_int(&mut self, tag: Tag, v: i64) -> Result<()> {
        if let Ok(n) = i8::try_from(v) {
            self.put_scalar(tag, et::INT8, &n.to_le_bytes());
        } else if let Ok(n) = i16::try_from(v) {
            self.put_scalar(tag, et::INT16, &n.to_le_bytes());
        } else if let Ok(n) = i32::try_from(v) {
            self.put_scalar(tag, et::INT32, &n.to_le_bytes());
        } else {
            self.put_scalar(tag, et::INT64, &v.to_le_bytes());
        }
        Ok(())
    }

    /// Emit a single-precision IEEE 754 float with the given tag.
    ///
    /// # Errors
    ///
    /// Currently infallible; returns `Ok(())` always. The `Result` return
    /// type is reserved for future I/O-backed writers.
    #[inline]
    pub fn put_float(&mut self, tag: Tag, v: f32) -> Result<()> {
        self.put_scalar(tag, et::FLOAT32, &v.to_le_bytes());
        Ok(())
    }

    /// Emit a double-precision IEEE 754 float with the given tag.
    ///
    /// # Errors
    ///
    /// Currently infallible; returns `Ok(())` always. The `Result` return
    /// type is reserved for future I/O-backed writers.
    #[inline]
    pub fn put_double(&mut self, tag: Tag, v: f64) -> Result<()> {
        self.put_scalar(tag, et::FLOAT64, &v.to_le_bytes());
        Ok(())
    }

    /// Emit a UTF-8 string with the given tag. The minimum-width length
    /// field (1, 2, 4, or 8 bytes) is chosen automatically.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LengthOverflow`] if the string is longer than
    /// `u64::MAX` bytes (impossible in practice on any supported platform,
    /// but the return type is `Result` for portability).
    #[inline]
    pub fn put_utf8(&mut self, tag: Tag, v: &str) -> Result<()> {
        self.put_string_payload(
            tag,
            v.as_bytes(),
            et::UTF8_LEN8,
            et::UTF8_LEN16,
            et::UTF8_LEN32,
            et::UTF8_LEN64,
        )
    }

    /// Emit an octet string with the given tag. The minimum-width length
    /// field (1, 2, 4, or 8 bytes) is chosen automatically.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LengthOverflow`] if the slice is longer than
    /// `u64::MAX` bytes (impossible in practice on any supported platform,
    /// but the return type is `Result` for portability).
    #[inline]
    pub fn put_bytes(&mut self, tag: Tag, v: &[u8]) -> Result<()> {
        self.put_string_payload(
            tag,
            v,
            et::BYTES_LEN8,
            et::BYTES_LEN16,
            et::BYTES_LEN32,
            et::BYTES_LEN64,
        )
    }

    /// Splice an already-encoded TLV element into the stream under a new
    /// `tag`, replacing the element's own tag control.
    ///
    /// `element` MUST be a single complete TLV element encoded with an
    /// **anonymous** tag (one control octet, no tag bytes), e.g. the output
    /// of another `TlvWriter` that began with `start_structure(Tag::Anonymous)`.
    /// Used to embed pre-encoded command-fields / payloads under a context
    /// tag without re-parsing them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnexpectedEof`] if `element` is empty,
    /// [`Error::InvalidTagControl`] if `element` does not begin with an
    /// anonymous-tagged control octet, or [`Error::InvalidElementType`] if
    /// the element is a bare end-of-container marker (`0x18`), which is a
    /// delimiter, not a complete element.
    pub fn put_preencoded(&mut self, tag: Tag, element: &[u8]) -> Result<()> {
        let (&control, rest) = element.split_first().ok_or(Error::UnexpectedEof)?;
        if control & tc::TAG_CONTROL_MASK != tc::ANONYMOUS {
            return Err(Error::InvalidTagControl(control & tc::TAG_CONTROL_MASK));
        }
        let element_type = control & et::ELEMENT_TYPE_MASK;
        if element_type == et::END_OF_CONTAINER {
            return Err(Error::InvalidElementType(element_type));
        }
        self.write_tag(tag, element_type);
        self.out.extend_from_slice(rest);
        Ok(())
    }

    fn put_string_payload(
        &mut self,
        tag: Tag,
        bytes: &[u8],
        et_len8: u8,
        et_len16: u8,
        et_len32: u8,
        et_len64: u8,
    ) -> Result<()> {
        let len = bytes.len();
        let mut buf = [0u8; MAX_HEADER];
        let header_len = if let Ok(len8) = u8::try_from(len) {
            let n = encode_tag(tag, et_len8, &mut buf);
            buf[n] = len8;
            n + 1
        } else if let Ok(len16) = u16::try_from(len) {
            let n = encode_tag(tag, et_len16, &mut buf);
            buf[n..n + 2].copy_from_slice(&len16.to_le_bytes());
            n + 2
        } else if let Ok(len32) = u32::try_from(len) {
            let n = encode_tag(tag, et_len32, &mut buf);
            buf[n..n + 4].copy_from_slice(&len32.to_le_bytes());
            n + 4
        } else {
            let len64 = u64::try_from(len).map_err(|_| Error::LengthOverflow)?;
            let n = encode_tag(tag, et_len64, &mut buf);
            buf[n..n + 8].copy_from_slice(&len64.to_le_bytes());
            n + 8
        };
        // One reservation for header + payload, then two appends into
        // guaranteed-capacity space.
        self.out.reserve(header_len + len);
        self.out.extend_from_slice(&buf[..header_len]);
        self.out.extend_from_slice(bytes);
        Ok(())
    }

    /// Begin a structure with the given tag. Children must be emitted
    /// with their own `put_*` / `write_value` calls; the structure is
    /// closed with [`Self::end_container`].
    ///
    /// # Errors
    ///
    /// Currently infallible; returns `Ok(())` always. The `Result` return
    /// type is reserved for future I/O-backed writers.
    #[inline]
    pub fn start_structure(&mut self, tag: Tag) -> Result<()> {
        self.write_tag(tag, et::STRUCTURE);
        Ok(())
    }

    /// Begin an array with the given tag. Children MUST be emitted with
    /// `Tag::Anonymous` per the Matter spec.
    ///
    /// # Errors
    ///
    /// Currently infallible; returns `Ok(())` always. The `Result` return
    /// type is reserved for future I/O-backed writers.
    #[inline]
    pub fn start_array(&mut self, tag: Tag) -> Result<()> {
        self.write_tag(tag, et::ARRAY);
        Ok(())
    }

    /// Begin a list with the given tag. List members may carry any tag
    /// form (including anonymous).
    ///
    /// # Errors
    ///
    /// Currently infallible; returns `Ok(())` always. The `Result` return
    /// type is reserved for future I/O-backed writers.
    #[inline]
    pub fn start_list(&mut self, tag: Tag) -> Result<()> {
        self.write_tag(tag, et::LIST);
        Ok(())
    }

    /// Emit the end-of-container marker (`0x18`) closing the most
    /// recently-opened container. The marker has no tag.
    ///
    /// # Errors
    ///
    /// Currently infallible; returns `Ok(())` always. The `Result` return
    /// type is reserved for future I/O-backed writers.
    #[inline]
    pub fn end_container(&mut self) -> Result<()> {
        self.out.push(et::END_OF_CONTAINER);
        Ok(())
    }

    /// Walk a [`Value`] tree and emit the appropriate sequence of TLV
    /// elements. Scalar variants are dispatched to the corresponding
    /// `put_*` method; container variants (`Structure`, `Array`, `List`)
    /// recursively encode all members and close with [`Self::end_container`].
    ///
    /// Array elements are always written with [`Tag::Anonymous`] regardless of
    /// what tag is stored in the `Value`, enforcing the Matter spec requirement
    /// that array elements carry no tag.
    ///
    /// Container nesting is bounded by [`MAX_DEPTH`], mirroring the reader's
    /// limit. A `Value` tree nested deeper than that is rejected with
    /// [`Error::ContainerTooDeep`] rather than risking a stack overflow on a
    /// hostile or buggy input tree.
    ///
    /// # Errors
    ///
    /// - [`Error::ContainerTooDeep`] — the `value` tree nests containers more
    ///   than [`MAX_DEPTH`] levels deep.
    /// - Any error returned by the underlying `put_*` or container method.
    pub fn write_value(&mut self, tag: Tag, value: &Value) -> Result<()> {
        self.write_value_at_depth(tag, value, 0)
    }

    /// Recursive worker for [`Self::write_value`] that carries the current
    /// container nesting depth so it can fail closed before the native call
    /// stack is at risk.
    fn write_value_at_depth(&mut self, tag: Tag, value: &Value, depth: usize) -> Result<()> {
        match value {
            Value::Bool(v) => self.put_bool(tag, *v),
            Value::Null => self.put_null(tag),
            Value::Uint(v) => self.put_uint(tag, *v),
            Value::Int(v) => self.put_int(tag, *v),
            Value::Float(v) => self.put_float(tag, *v),
            Value::Double(v) => self.put_double(tag, *v),
            Value::Utf8(v) => self.put_utf8(tag, v),
            Value::Bytes(v) => self.put_bytes(tag, v),
            Value::Structure(members) => {
                if depth >= MAX_DEPTH {
                    return Err(Error::ContainerTooDeep);
                }
                self.start_structure(tag)?;
                for (member_tag, member_value) in members {
                    self.write_value_at_depth(*member_tag, member_value, depth + 1)?;
                }
                self.end_container()
            }
            Value::Array(elements) => {
                if depth >= MAX_DEPTH {
                    return Err(Error::ContainerTooDeep);
                }
                self.start_array(tag)?;
                for element in elements {
                    self.write_value_at_depth(Tag::Anonymous, element, depth + 1)?;
                }
                self.end_container()
            }
            Value::List(members) => {
                if depth >= MAX_DEPTH {
                    return Err(Error::ContainerTooDeep);
                }
                self.start_list(tag)?;
                for (member_tag, member_value) in members {
                    self.write_value_at_depth(*member_tag, member_value, depth + 1)?;
                }
                self.end_container()
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap is allowed in tests with
                              // a documented justification (CONTRIBUTING.md).
mod tests {
    use super::*;

    // --- Cycle 1: put_bool ---

    #[test]
    fn put_bool_true_anonymous_matches_vector_0001() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_bool(Tag::Anonymous, true).unwrap();
        assert_eq!(buf, [0x09]);
    }

    #[test]
    fn put_bool_false_anonymous_matches_vector_0002() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_bool(Tag::Anonymous, false).unwrap();
        assert_eq!(buf, [0x08]);
    }

    // --- Cycle 2: put_null ---

    #[test]
    fn put_null_anonymous_emits_0x14() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_null(Tag::Anonymous).unwrap();
        assert_eq!(buf, [0x14]);
    }

    // --- Cycle 3: put_uint ---

    #[test]
    fn put_uint_42_anonymous_picks_1_byte_width_matches_vector_0003() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::Anonymous, 42).unwrap();
        assert_eq!(buf, [0x04, 0x2A]);
    }

    #[test]
    fn put_uint_max_u8_anonymous_still_1_byte() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::Anonymous, 255).unwrap();
        assert_eq!(buf, [0x04, 0xFF]);
    }

    #[test]
    fn put_uint_0x1234_anonymous_2_byte_le() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::Anonymous, 0x1234).unwrap();
        assert_eq!(buf, [0x05, 0x34, 0x12]);
    }

    #[test]
    fn put_uint_0xcafebabe_anonymous_4_byte_le() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::Anonymous, 0xCAFE_BABE).unwrap();
        assert_eq!(buf, [0x06, 0xBE, 0xBA, 0xFE, 0xCA]);
    }

    #[test]
    fn put_uint_big_anonymous_8_byte_le() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::Anonymous, 0x0123_4567_89AB_CDEF).unwrap();
        assert_eq!(buf, [0x07, 0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01]);
    }

    // --- Cycle 4: put_int ---

    #[test]
    fn put_int_neg17_anonymous_matches_vector_0008() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_int(Tag::Anonymous, -17).unwrap();
        assert_eq!(buf, [0x00, 0xEF]);
    }

    #[test]
    fn put_int_neg128_anonymous_1_byte() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_int(Tag::Anonymous, -128).unwrap();
        assert_eq!(buf, [0x00, 0x80]);
    }

    #[test]
    fn put_int_neg129_anonymous_2_byte() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_int(Tag::Anonymous, -129).unwrap();
        assert_eq!(buf, [0x01, 0x7F, 0xFF]);
    }

    #[test]
    fn put_int_i32_min_anonymous_4_byte() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_int(Tag::Anonymous, i64::from(i32::MIN)).unwrap();
        assert_eq!(buf, [0x02, 0x00, 0x00, 0x00, 0x80]);
    }

    #[test]
    fn put_int_i64_min_anonymous_8_byte() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_int(Tag::Anonymous, i64::MIN).unwrap();
        assert_eq!(buf, [0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80]);
    }

    // --- Cycle 5: put_float and put_double ---

    #[test]
    fn put_float_zero_anonymous_matches_vector_0013() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_float(Tag::Anonymous, 0.0).unwrap();
        assert_eq!(buf, [0x0A, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn put_double_zero_anonymous_matches_vector_0014() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_double(Tag::Anonymous, 0.0).unwrap();
        assert_eq!(buf, [0x0B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    // --- Cycle 6: context tag emission ---

    #[test]
    fn put_uint_with_context_tag_5_emits_tag_byte() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::Context(5), 42).unwrap();
        // 0b001_00100 = 0x24 (context-tag form | UINT8 element type),
        // then tag number 0x05, then payload 0x2A.
        assert_eq!(buf, [0x24, 0x05, 0x2A]);
    }

    // --- Cycle 8: CommonProfile tag emission ---

    #[test]
    fn put_uint_with_common_profile_2_byte_tag() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::CommonProfile(7), 42).unwrap();
        // control = 0b010_00100 = 0x44 (CommonProfile 2-byte | UINT8)
        // tag bytes = 0x07 0x00 (LE u16), payload = 0x2A
        assert_eq!(buf, [0x44, 0x07, 0x00, 0x2A]);
    }

    #[test]
    fn put_uint_with_common_profile_4_byte_tag() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::CommonProfile(0x0001_2345), 42).unwrap();
        // control = 0b011_00100 = 0x64
        // tag bytes = 0x45 0x23 0x01 0x00 (LE u32), payload = 0x2A
        assert_eq!(buf, [0x64, 0x45, 0x23, 0x01, 0x00, 0x2A]);
    }

    #[test]
    fn put_uint_with_common_profile_at_u16_boundary_picks_2_byte() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::CommonProfile(0xFFFF), 0).unwrap();
        assert_eq!(buf, [0x44, 0xFF, 0xFF, 0x00]);
    }

    #[test]
    fn put_uint_with_common_profile_just_above_u16_picks_4_byte() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::CommonProfile(0x0001_0000), 0).unwrap();
        assert_eq!(buf, [0x64, 0x00, 0x00, 0x01, 0x00, 0x00]);
    }

    // --- Cycle 9: ImplicitProfile tag emission ---

    #[test]
    fn put_uint_with_implicit_profile_2_byte_tag() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::ImplicitProfile(7), 42).unwrap();
        // control = 0b100_00100 = 0x84
        assert_eq!(buf, [0x84, 0x07, 0x00, 0x2A]);
    }

    #[test]
    fn put_uint_with_implicit_profile_4_byte_tag() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(Tag::ImplicitProfile(0x0001_2345), 42).unwrap();
        // control = 0b101_00100 = 0xA4
        assert_eq!(buf, [0xA4, 0x45, 0x23, 0x01, 0x00, 0x2A]);
    }

    // --- Cycle 10: FullyQualified tag emission ---

    #[test]
    fn put_uint_with_fully_qualified_6_byte() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(
            Tag::FullyQualified {
                vendor: 0xFFF1,
                profile: 0x0006,
                tag: 5,
            },
            42,
        )
        .unwrap();
        // control = 0b110_00100 = 0xC4 (FQ 6-byte | UINT8)
        // vendor 0xF1 0xFF, profile 0x06 0x00, tag 0x05 0x00, payload 0x2A
        assert_eq!(buf, [0xC4, 0xF1, 0xFF, 0x06, 0x00, 0x05, 0x00, 0x2A]);
    }

    #[test]
    fn put_uint_with_fully_qualified_8_byte() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(
            Tag::FullyQualified {
                vendor: 0xFFF1,
                profile: 0x0006,
                tag: 0x0001_2345,
            },
            42,
        )
        .unwrap();
        // control = 0b111_00100 = 0xE4
        assert_eq!(
            buf,
            [0xE4, 0xF1, 0xFF, 0x06, 0x00, 0x45, 0x23, 0x01, 0x00, 0x2A]
        );
    }

    // --- Cycle 11: put_utf8 ---

    #[test]
    fn put_utf8_hello_anonymous_matches_vector_0015() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_utf8(Tag::Anonymous, "Hello!").unwrap();
        assert_eq!(buf, [0x0C, 0x06, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x21]);
    }

    #[test]
    fn put_utf8_empty_anonymous_matches_vector_0016() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_utf8(Tag::Anonymous, "").unwrap();
        assert_eq!(buf, [0x0C, 0x00]);
    }

    #[test]
    fn put_utf8_at_255_byte_boundary_uses_len8() {
        let s: String = "a".repeat(255);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_utf8(Tag::Anonymous, &s).unwrap();
        assert_eq!(buf.len(), 1 + 1 + 255);
        assert_eq!(buf[0], 0x0C);
        assert_eq!(buf[1], 0xFF);
        assert!(buf[2..].iter().all(|&b| b == b'a'));
    }

    #[test]
    fn put_utf8_at_256_bytes_picks_len16() {
        let s: String = "a".repeat(256);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_utf8(Tag::Anonymous, &s).unwrap();
        assert_eq!(buf.len(), 1 + 2 + 256);
        assert_eq!(buf[0], 0x0D);
        assert_eq!(&buf[1..3], &[0x00, 0x01]);
        assert!(buf[3..].iter().all(|&b| b == b'a'));
    }

    #[test]
    fn put_utf8_at_u16_max_uses_len16() {
        let s: String = "a".repeat(usize::from(u16::MAX));
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_utf8(Tag::Anonymous, &s).unwrap();
        assert_eq!(buf[0], 0x0D);
        assert_eq!(&buf[1..3], &[0xFF, 0xFF]);
        assert_eq!(buf.len(), 1 + 2 + usize::from(u16::MAX));
    }

    #[test]
    fn put_utf8_above_u16_max_picks_len32() {
        let len = usize::from(u16::MAX) + 1; // 65,536
        let s: String = "a".repeat(len);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_utf8(Tag::Anonymous, &s).unwrap();
        assert_eq!(buf[0], 0x0E);
        assert_eq!(&buf[1..5], &[0x00, 0x00, 0x01, 0x00]);
        assert_eq!(buf.len(), 1 + 4 + len);
    }

    // --- Cycle 12: put_bytes ---

    #[test]
    fn put_bytes_five_bytes_anonymous_matches_vector_0017() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_bytes(Tag::Anonymous, &[0x00, 0x01, 0x02, 0x03, 0x04])
            .unwrap();
        assert_eq!(buf, [0x10, 0x05, 0x00, 0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn put_bytes_empty_anonymous_matches_vector_0018() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_bytes(Tag::Anonymous, &[]).unwrap();
        assert_eq!(buf, [0x10, 0x00]);
    }

    #[test]
    fn put_bytes_at_256_bytes_picks_len16() {
        let data = vec![0xAB; 256];
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_bytes(Tag::Anonymous, &data).unwrap();
        assert_eq!(buf[0], 0x11);
        assert_eq!(&buf[1..3], &[0x00, 0x01]);
        assert_eq!(buf.len(), 1 + 2 + 256);
        assert!(buf[3..].iter().all(|&b| b == 0xAB));
    }

    // --- Cycle 7: write_value dispatch ---

    #[test]
    fn write_value_dispatches_on_utf8_and_bytes_variants() {
        for (value, expected) in [
            (
                Value::Utf8(String::from("Hello!")),
                vec![0x0C, 0x06, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x21],
            ),
            (
                Value::Bytes(vec![0x00, 0x01, 0x02, 0x03, 0x04]),
                vec![0x10, 0x05, 0x00, 0x01, 0x02, 0x03, 0x04],
            ),
        ] {
            let mut buf = Vec::new();
            let mut w = TlvWriter::new(&mut buf);
            w.write_value(Tag::Anonymous, &value).unwrap();
            assert_eq!(buf, expected, "value={value:?}");
        }
    }

    #[test]
    fn write_value_dispatches_on_variant() {
        // One sanity case per variant. Bytes are taken from earlier per-method tests.
        for (value, expected) in [
            (Value::Bool(true), vec![0x09]),
            (Value::Null, vec![0x14]),
            (Value::Uint(42), vec![0x04, 0x2A]),
            (Value::Int(-17), vec![0x00, 0xEF]),
            (Value::Float(0.0), vec![0x0A, 0x00, 0x00, 0x00, 0x00]),
            (
                Value::Double(0.0),
                vec![0x0B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            ),
        ] {
            let mut buf = Vec::new();
            let mut w = TlvWriter::new(&mut buf);
            w.write_value(Tag::Anonymous, &value).unwrap();
            assert_eq!(buf, expected, "value={value:?}");
        }
    }

    // --- container primitives ---

    #[test]
    fn start_structure_anonymous_emits_0x15() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        assert_eq!(buf, [0x15]);
    }

    #[test]
    fn start_array_anonymous_emits_0x16() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_array(Tag::Anonymous).unwrap();
        assert_eq!(buf, [0x16]);
    }

    #[test]
    fn start_list_anonymous_emits_0x17() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_list(Tag::Anonymous).unwrap();
        assert_eq!(buf, [0x17]);
    }

    #[test]
    fn end_container_emits_0x18() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.end_container().unwrap();
        assert_eq!(buf, [0x18]);
    }

    #[test]
    fn start_structure_with_context_tag_emits_combined_byte() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Context(7)).unwrap();
        // 0b001_10101 = 0x35 (context tag form | STRUCTURE element type)
        assert_eq!(buf, [0x35, 0x07]);
    }

    #[test]
    fn empty_structure_anonymous_matches_vector_0019() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.end_container().unwrap();
        assert_eq!(buf, [0x15, 0x18]);
    }

    #[test]
    fn structure_with_one_member_matches_vector_0021() {
        // [0x15, 0x24, 0x00, 0x2A, 0x18]
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 42).unwrap();
        w.end_container().unwrap();
        assert_eq!(buf, [0x15, 0x24, 0x00, 0x2A, 0x18]);
    }

    // --- write_value recursive container dispatch ---

    #[test]
    fn write_value_empty_structure_matches_vector_0019() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.write_value(Tag::Anonymous, &Value::Structure(Vec::new()))
            .unwrap();
        assert_eq!(buf, [0x15, 0x18]);
    }

    #[test]
    fn write_value_empty_array_matches_vector_0020() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.write_value(Tag::Anonymous, &Value::Array(Vec::new()))
            .unwrap();
        assert_eq!(buf, [0x16, 0x18]);
    }

    #[test]
    fn write_value_structure_with_ctx_member_matches_vector_0021() {
        let value = Value::Structure(vec![(Tag::Context(0), Value::Uint(42))]);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.write_value(Tag::Anonymous, &value).unwrap();
        assert_eq!(buf, [0x15, 0x24, 0x00, 0x2A, 0x18]);
    }

    #[test]
    fn write_value_array_of_three_uint8_matches_vector_0022() {
        let value = Value::Array(vec![Value::Uint(1), Value::Uint(2), Value::Uint(3)]);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.write_value(Tag::Anonymous, &value).unwrap();
        assert_eq!(buf, [0x16, 0x04, 0x01, 0x04, 0x02, 0x04, 0x03, 0x18]);
    }

    #[test]
    fn write_value_structure_with_bool_at_ctx7_matches_vector_0023() {
        let value = Value::Structure(vec![(Tag::Context(7), Value::Bool(true))]);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.write_value(Tag::Anonymous, &value).unwrap();
        assert_eq!(buf, [0x15, 0x29, 0x07, 0x18]);
    }

    #[test]
    fn write_value_empty_list_emits_0x17_0x18() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.write_value(Tag::Anonymous, &Value::List(Vec::new()))
            .unwrap();
        assert_eq!(buf, [0x17, 0x18]);
    }

    // --- put_preencoded ---

    #[test]
    fn put_preencoded_retags_anonymous_struct_to_context_1() {
        // An empty anonymous struct: 0x15 (anon-struct-start) 0x18 (end-container).
        let anonymous_struct = vec![0x15u8, 0x18];
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_preencoded(Tag::Context(1), &anonymous_struct)
            .unwrap();
        // Expected: context-tag struct at tag 1, then the body (0x18 end).
        // Control octet: tc::CONTEXT | et::STRUCTURE = 0x20 | 0x15 = 0x35
        // Tag byte: 0x01
        // Body: 0x18
        assert_eq!(buf, [0x35, 0x01, 0x18]);
    }

    #[test]
    fn put_preencoded_rejects_empty_input() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        assert!(matches!(
            w.put_preencoded(Tag::Context(0), &[]),
            Err(Error::UnexpectedEof)
        ));
    }

    #[test]
    fn put_preencoded_rejects_non_anonymous_input() {
        // A context-tagged bool (tc::CONTEXT | et::BOOL_FALSE = 0x20 | 0x08 = 0x28).
        let non_anonymous = vec![0x28u8, 0x00];
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        assert!(matches!(
            w.put_preencoded(Tag::Context(0), &non_anonymous),
            Err(Error::InvalidTagControl(_))
        ));
    }

    #[test]
    fn put_preencoded_rejects_bare_end_of_container() {
        // 0x18 is END_OF_CONTAINER — anonymous tag bits (0b000) are valid, but
        // the element type is the delimiter, not a complete element.
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        assert!(matches!(
            w.put_preencoded(Tag::Context(1), &[0x18]),
            Err(Error::InvalidElementType(_))
        ));
    }

    #[test]
    fn write_value_nested_structure() {
        // outer { ctx(0): inner { ctx(0): uint8=42 } }
        let inner = Value::Structure(vec![(Tag::Context(0), Value::Uint(42))]);
        let outer = Value::Structure(vec![(Tag::Context(0), inner)]);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.write_value(Tag::Anonymous, &outer).unwrap();
        // 0x15 = anon-struct-start
        //   0x35 0x00 = ctx-tag-struct-start at tag 0 (0b001_10101)
        //     0x24 0x00 0x2A = ctx-tag uint8=42 at tag 0
        //   0x18 = inner end
        // 0x18 = outer end
        assert_eq!(buf, [0x15, 0x35, 0x00, 0x24, 0x00, 0x2A, 0x18, 0x18]);
    }

    // --- write-side depth guard ---

    /// Build a `Value` tree of `levels` nested structures, innermost holding a
    /// single uint. `levels == 1` is one structure wrapping the scalar.
    fn nested_structure(levels: usize) -> Value {
        let mut v = Value::Uint(0);
        for _ in 0..levels {
            v = Value::Structure(vec![(Tag::Anonymous, v)]);
        }
        v
    }

    #[test]
    fn write_value_accepts_tree_at_max_depth() {
        // MAX_DEPTH nested containers is the deepest the reader accepts, so the
        // writer must accept it too (symmetric limits).
        let value = nested_structure(MAX_DEPTH);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        assert!(w.write_value(Tag::Anonymous, &value).is_ok());
    }

    #[test]
    fn write_value_rejects_over_deep_tree() {
        // One container deeper than the reader's limit must error (rather than
        // recurse far enough to risk a native stack overflow).
        let value = nested_structure(MAX_DEPTH + 1);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        assert!(matches!(
            w.write_value(Tag::Anonymous, &value),
            Err(Error::ContainerTooDeep)
        ));
    }

    #[test]
    fn write_value_over_deep_tree_roundtrips_with_reader_limit() {
        // A tree the writer accepts (== MAX_DEPTH) must also decode back, and a
        // tree one deeper that the writer rejects matches the reader's own cap.
        let ok = nested_structure(MAX_DEPTH);
        let mut buf = Vec::new();
        TlvWriter::new(&mut buf)
            .write_value(Tag::Anonymous, &ok)
            .unwrap();
        let (_, decoded) = crate::reader::TlvReader::new(&buf).read_value().unwrap();
        assert_eq!(decoded, ok);
    }

    // --- MAX_HEADER saturation ---

    /// The widest possible element: FQ-8 tag (control + 8 tag bytes) + u64
    /// payload = exactly `MAX_HEADER` (17) bytes. Pins that the stack buffer
    /// saturates without truncation.
    #[test]
    fn put_uint_fq8_u64_max_is_seventeen_bytes() {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.put_uint(
            Tag::FullyQualified {
                vendor: 0xFFF1,
                profile: 0x0006,
                tag: u32::MAX,
            },
            u64::MAX,
        )
        .unwrap();
        // control = FULLY_QUALIFIED_8 | UINT64 = 0xE7; then vendor LE,
        // profile LE, 4 tag bytes LE, 8 payload bytes.
        assert_eq!(
            buf,
            [
                0xE7, 0xF1, 0xFF, 0x06, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
                0xFF, 0xFF, 0xFF
            ]
        );
        assert_eq!(buf.len(), 17);
    }
}
