//! The little protobuf both protocols need: varints, length-delimited
//! fields, a reader that finds fields by number. Hand-written: a handful of
//! message shapes are not worth a code generator.

/// Appends `n` as a varint.
pub fn varint(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let low = u8::try_from(n & 0x7F).unwrap_or(0);
        n >>= 7;
        if n == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

/// A varint at `pos` (moved past it); `None` if cut short or too long.
#[must_use]
pub fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut n = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        n |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some(n);
        }
    }
    None
}

/// A message being written, field after field.
#[derive(Debug, Default)]
pub struct Writer(Vec<u8>);

impl Writer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn uint(mut self, field: u32, value: u64) -> Self {
        varint(u64::from(field) << 3, &mut self.0);
        varint(value, &mut self.0);
        self
    }

    #[must_use]
    pub fn bytes(mut self, field: u32, value: &[u8]) -> Self {
        varint((u64::from(field) << 3) | 2, &mut self.0);
        varint(value.len() as u64, &mut self.0);
        self.0.extend_from_slice(value);
        self
    }

    #[must_use]
    pub fn str(self, field: u32, value: &str) -> Self {
        self.bytes(field, value.as_bytes())
    }

    #[must_use]
    pub fn msg(self, field: u32, value: Writer) -> Self {
        self.bytes(field, &value.finish())
    }

    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field<'a> {
    Uint(u64),
    Bytes(&'a [u8]),
}

/// A decoded message: its fields, by number, in order.
#[derive(Clone, Debug, Default)]
pub struct Fields<'a>(Vec<(u32, Field<'a>)>);

/// Decodes one message; `None` if malformed.
#[must_use]
pub fn parse(buf: &[u8]) -> Option<Fields<'_>> {
    let mut fields = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let key = read_varint(buf, &mut pos)?;
        let number = u32::try_from(key >> 3).ok()?;
        let value = match key & 7 {
            0 => Field::Uint(read_varint(buf, &mut pos)?),
            2 => {
                let len = usize::try_from(read_varint(buf, &mut pos)?).ok()?;
                let end = pos.checked_add(len).filter(|end| *end <= buf.len())?;
                let slice = &buf[pos..end];
                pos = end;
                Field::Bytes(slice)
            }
            // Fixed 32 / 64 bits: kept as bytes, never read here.
            5 | 1 => {
                let len = if key & 7 == 5 { 4 } else { 8 };
                let end = pos.checked_add(len).filter(|end| *end <= buf.len())?;
                let slice = &buf[pos..end];
                pos = end;
                Field::Bytes(slice)
            }
            _ => return None,
        };
        fields.push((number, value));
    }
    Some(Fields(fields))
}

impl<'a> Fields<'a> {
    #[must_use]
    pub fn has(&self, number: u32) -> bool {
        self.0.iter().any(|(n, _)| *n == number)
    }

    #[must_use]
    pub fn uint(&self, number: u32) -> Option<u64> {
        self.0.iter().find_map(|(n, f)| match f {
            Field::Uint(v) if *n == number => Some(*v),
            _ => None,
        })
    }

    #[must_use]
    pub fn bytes(&self, number: u32) -> Option<&'a [u8]> {
        self.0.iter().find_map(|(n, f)| match f {
            Field::Bytes(v) if *n == number => Some(*v),
            _ => None,
        })
    }

    #[must_use]
    pub fn str(&self, number: u32) -> Option<&'a str> {
        std::str::from_utf8(self.bytes(number)?).ok()
    }

    /// A sub-message (an empty one when the field is present but empty).
    #[must_use]
    pub fn msg(&self, number: u32) -> Option<Fields<'a>> {
        parse(self.bytes(number)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_back() {
        let inner = Writer::new().uint(1, 615).str(5, "atvremote");
        let bytes = Writer::new().msg(1, inner).uint(40, 1).finish();
        let m = parse(&bytes).unwrap();
        let inner = m.msg(1).unwrap();
        assert_eq!(inner.uint(1), Some(615));
        assert_eq!(inner.str(5), Some("atvremote"));
        assert_eq!(m.uint(40), Some(1));
        assert!(!m.has(2));
    }

    #[test]
    fn varints_round_trip_and_bad_input_is_refused() {
        for n in [0, 1, 127, 128, 300, 615, u64::from(u32::MAX), u64::MAX] {
            let mut out = Vec::new();
            varint(n, &mut out);
            let mut pos = 0;
            assert_eq!(read_varint(&out, &mut pos), Some(n));
            assert_eq!(pos, out.len());
        }
        assert!(parse(&[0x0A, 0x05, 0x01]).is_none(), "length past the end");
        assert!(parse(&[0x0B]).is_none(), "group wire type");
        let mut pos = 0;
        assert_eq!(read_varint(&[0x80, 0x80], &mut pos), None, "cut short");
    }
}
