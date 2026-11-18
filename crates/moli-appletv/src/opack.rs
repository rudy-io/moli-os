//! OPACK, Apple's compact binary encoding of the Companion protocol's
//! messages (dictionaries of strings, numbers, bytes, lists). Read the way
//! pyatv reads it (MIT), including its back-references: a value already
//! seen in the message (a string, bytes, a number on more than one byte) is
//! sent again as its index in the list of values seen, each value once.
//! Written plainly, without back-references (the receiver needs none).

use std::collections::HashSet;

use anyhow::{Context as _, bail};

/// A decoded value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Uuid([u8; 16]),
    /// Absolute time, as the eight bytes say (pyatv reads an integer).
    Date(i64),
    List(Vec<Value>),
    Dict(Vec<(Value, Value)>),
}

impl Value {
    /// A dictionary from string keys (what every message is).
    #[must_use]
    pub fn dict<K: Into<String>>(entries: impl IntoIterator<Item = (K, Value)>) -> Self {
        Self::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (Self::Str(k.into()), v))
                .collect(),
        )
    }

    /// The value under a string key of a dictionary.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Dict(entries) => entries
                .iter()
                .find(|(k, _)| matches!(k, Self::Str(s) if s == key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::Str(s.to_owned())
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Self::Int(i)
    }
}

/// The encoding of `value`.
#[must_use]
pub fn pack(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write(value, &mut out);
    out
}

/// A long string's length: 0x61 to 0x64, then the length on 1 to 4 bytes.
fn string_length(out: &mut Vec<u8>, len: usize) {
    let bytes = (len as u64).to_le_bytes();
    let n: u8 = match len {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFF_FFFF => 3,
        _ => 4,
    };
    out.push(0x60 + n);
    out.extend_from_slice(&bytes[..usize::from(n)]);
}

/// Long bytes' length: 0x91 to 0x94, then the length on 1, 2, 4 or 8 bytes.
fn bytes_length(out: &mut Vec<u8>, len: usize) {
    let bytes = (len as u64).to_le_bytes();
    let (tag, n) = match len {
        0..=0xFF => (0x91, 1),
        0x100..=0xFFFF => (0x92, 2),
        0x1_0000..=0xFFFF_FFFF => (0x93, 4),
        _ => (0x94, 8),
    };
    out.push(tag);
    out.extend_from_slice(&bytes[..n]);
}

fn write(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.push(0x04),
        Value::Bool(b) => out.push(if *b { 0x01 } else { 0x02 }),
        Value::Uuid(u) => {
            out.push(0x05);
            out.extend_from_slice(u);
        }
        Value::Date(d) => {
            out.push(0x06);
            out.extend_from_slice(&d.to_le_bytes());
        }
        Value::Int(i) => match u64::try_from(*i) {
            Ok(u) if u < 0x28 => out.push(u8::try_from(u).unwrap_or(0) + 8),
            Ok(u) if u <= 0xFF => {
                out.push(0x30);
                out.push(u8::try_from(u).unwrap_or(0));
            }
            Ok(u) if u <= 0xFFFF => {
                out.push(0x31);
                out.extend_from_slice(&u.to_le_bytes()[..2]);
            }
            Ok(u) if u <= 0xFFFF_FFFF => {
                out.push(0x32);
                out.extend_from_slice(&u.to_le_bytes()[..4]);
            }
            // Eight bytes, two's complement for a negative number.
            _ => {
                out.push(0x33);
                out.extend_from_slice(&i.to_le_bytes());
            }
        },
        Value::Float(f) => {
            out.push(0x36);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Value::Str(s) => {
            let bytes = s.as_bytes();
            if bytes.len() <= 0x20 {
                out.push(0x40 + u8::try_from(bytes.len()).unwrap_or(0));
            } else {
                string_length(out, bytes.len());
            }
            out.extend_from_slice(bytes);
        }
        Value::Bytes(bytes) => {
            if bytes.len() <= 0x20 {
                out.push(0x70 + u8::try_from(bytes.len()).unwrap_or(0));
            } else {
                bytes_length(out, bytes.len());
            }
            out.extend_from_slice(bytes);
        }
        Value::List(items) => {
            out.push(0xD0 + u8::try_from(items.len().min(0xF)).unwrap_or(0xF));
            for item in items {
                write(item, out);
            }
            if items.len() >= 0xF {
                out.push(0x03);
            }
        }
        Value::Dict(entries) => {
            out.push(0xE0 + u8::try_from(entries.len().min(0xF)).unwrap_or(0xF));
            for (k, v) in entries {
                write(k, out);
                write(v, out);
            }
            if entries.len() >= 0xF {
                out.push(0x03);
            }
        }
    }
}

/// Decodes one value; what follows it is left. A message decodes to at most
/// [`BUDGET`] bytes, references counted at their full size: a small frame of
/// references to one long string is refused, not copied a million times.
pub fn unpack(data: &[u8]) -> anyhow::Result<(Value, &[u8])> {
    let mut seen = Seen::default();
    read(data, &mut seen, 0)
}

/// What one message may decode to, all values counted (each at least the
/// size of a [`Value`], so a frame of tiny values is bounded too). A message
/// is a few kilobytes; the list of applications, a few tens.
pub const BUDGET: usize = 2 << 20;

/// What a value counts against the budget (a list or a dictionary: its own
/// slot; its items are counted as they are read).
fn cost(value: &Value) -> usize {
    std::mem::size_of::<Value>()
        + match value {
            Value::Str(s) => s.len(),
            Value::Bytes(b) => b.len(),
            _ => 0,
        }
}

/// The values already seen in a message, each once, with what decoding has
/// cost so far.
#[derive(Default)]
struct Seen {
    values: Vec<Value>,
    /// Each value's encoding, to know in one step if it was seen (only
    /// scalars are remembered, and their encoding is exact).
    known: HashSet<Vec<u8>>,
    spent: usize,
}

impl Seen {
    /// Counts a decoded value against the budget.
    fn charge(&mut self, value: &Value) -> anyhow::Result<()> {
        self.spent = self.spent.saturating_add(cost(value));
        if self.spent > BUDGET {
            bail!("OPACK: message larger than {BUDGET} bytes once decoded");
        }
        Ok(())
    }

    fn remember(&mut self, value: &Value) {
        if self.known.insert(pack(value)) {
            self.values.push(value.clone());
        }
    }

    /// The value a reference names, counted before it is copied.
    fn refer(&mut self, index: usize) -> anyhow::Result<Value> {
        let value = self
            .values
            .get(index)
            .with_context(|| format!("OPACK: no value {index} to refer to"))?;
        // Only scalars are remembered: counting one costs nothing.
        self.spent = self.spent.saturating_add(cost(value));
        if self.spent > BUDGET {
            bail!("OPACK: message larger than {BUDGET} bytes once decoded");
        }
        Ok(value.clone())
    }
}

/// Nesting deeper than this is refused (a message is a few levels deep).
const DEPTH: usize = 32;

fn take(data: &[u8], n: usize) -> anyhow::Result<(&[u8], &[u8])> {
    if data.len() < n {
        bail!("OPACK: {n} bytes announced, {} left", data.len());
    }
    Ok(data.split_at(n))
}

fn le(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    buf[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(buf)
}

fn length(data: &[u8], n: usize) -> anyhow::Result<(usize, &[u8])> {
    let (len, rest) = take(data, n)?;
    let len = usize::try_from(le(len)).context("OPACK: length too large")?;
    Ok((len, rest))
}

fn text(bytes: &[u8]) -> anyhow::Result<Value> {
    Ok(Value::Str(
        std::str::from_utf8(bytes)
            .context("OPACK: string not UTF-8")?
            .to_owned(),
    ))
}

#[allow(clippy::too_many_lines)] // one arm per type, as the format lists them
fn read<'a>(data: &'a [u8], seen: &mut Seen, depth: usize) -> anyhow::Result<(Value, &'a [u8])> {
    if depth > DEPTH {
        bail!("OPACK: nested too deep");
    }
    let (&tag, rest) = data.split_first().context("OPACK: empty")?;
    let mut remember = true;
    let mut charged = false;
    let (value, rest) = match tag {
        0x01 | 0x02 => {
            remember = false;
            (Value::Bool(tag == 0x01), rest)
        }
        0x04 => {
            remember = false;
            (Value::Null, rest)
        }
        0x05 => {
            let (u, rest) = take(rest, 16)?;
            let mut uuid = [0u8; 16];
            uuid.copy_from_slice(u);
            (Value::Uuid(uuid), rest)
        }
        0x06 => {
            let (d, rest) = take(rest, 8)?;
            (Value::Date(i64::from_le_bytes(d.try_into()?)), rest)
        }
        0x08..=0x2F => {
            remember = false;
            (Value::Int(i64::from(tag - 8)), rest)
        }
        0x35 => {
            let (f, rest) = take(rest, 4)?;
            (
                Value::Float(f64::from(f32::from_le_bytes(f.try_into()?))),
                rest,
            )
        }
        0x36 => {
            let (f, rest) = take(rest, 8)?;
            (Value::Float(f64::from_le_bytes(f.try_into()?)), rest)
        }
        0x30..=0x33 => {
            let n = 1usize << (tag & 0xF);
            let (i, rest) = take(rest, n)?;
            let raw = le(i);
            // Eight bytes may be a negative number; fewer never are.
            #[allow(clippy::cast_possible_wrap)]
            let value = if n == 8 {
                raw as i64
            } else {
                i64::try_from(raw)?
            };
            (Value::Int(value), rest)
        }
        0x40..=0x60 => {
            let (s, rest) = take(rest, usize::from(tag - 0x40))?;
            (text(s)?, rest)
        }
        0x61..=0x64 => {
            let (len, rest) = length(rest, usize::from(tag & 0xF))?;
            let (s, rest) = take(rest, len)?;
            (text(s)?, rest)
        }
        0x70..=0x90 => {
            let (b, rest) = take(rest, usize::from(tag - 0x70))?;
            (Value::Bytes(b.to_vec()), rest)
        }
        0x91..=0x94 => {
            let (len, rest) = length(rest, 1 << ((tag & 0xF) - 1))?;
            let (b, rest) = take(rest, len)?;
            (Value::Bytes(b.to_vec()), rest)
        }
        0xA0..=0xC0 => {
            let index = usize::from(tag - 0xA0);
            charged = true;
            remember = false; // already in the list
            (seen.refer(index)?, rest)
        }
        0xC1..=0xC4 => {
            let (index, rest) = length(rest, usize::from(tag - 0xC0))?;
            charged = true;
            remember = false; // already in the list
            (seen.refer(index)?, rest)
        }
        0xD0..=0xDF => {
            remember = false;
            let count = tag & 0xF;
            let mut items = Vec::new();
            let mut ptr = rest;
            if count == 0xF {
                loop {
                    match ptr.first() {
                        Some(0x03) => break,
                        Some(_) => {}
                        None => bail!("OPACK: list not closed"),
                    }
                    let (item, next) = read(ptr, seen, depth + 1)?;
                    items.push(item);
                    ptr = next;
                }
                ptr = &ptr[1..];
            } else {
                for _ in 0..count {
                    let (item, next) = read(ptr, seen, depth + 1)?;
                    items.push(item);
                    ptr = next;
                }
            }
            (Value::List(items), ptr)
        }
        0xE0..=0xFF => {
            remember = false;
            let count = tag & 0xF;
            let mut entries = Vec::new();
            let mut ptr = rest;
            let mut entry = |ptr: &'a [u8], seen: &mut Seen| -> anyhow::Result<&'a [u8]> {
                let (k, next) = read(ptr, seen, depth + 1)?;
                let (v, next) = read(next, seen, depth + 1)?;
                entries.push((k, v));
                Ok(next)
            };
            if count == 0xF {
                loop {
                    match ptr.first() {
                        Some(0x03) => break,
                        Some(_) => {}
                        None => bail!("OPACK: dictionary not closed"),
                    }
                    ptr = entry(ptr, seen)?;
                }
                ptr = &ptr[1..];
            } else {
                for _ in 0..count {
                    ptr = entry(ptr, seen)?;
                }
            }
            (Value::Dict(entries), ptr)
        }
        other => bail!("OPACK: unknown type 0x{other:02x}"),
    };
    if !charged {
        seen.charge(&value)?;
    }
    // Each value once, as pyatv keeps them (a reference counts in that list).
    if remember {
        seen.remember(&value);
    }
    Ok((value, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors from pyatv's tests (tests/support/test_opack.py, MIT).

    fn s(x: &str) -> Value {
        Value::Str(x.into())
    }

    #[test]
    fn writes_what_pyatv_writes() {
        assert_eq!(pack(&Value::Bool(true)), b"\x01");
        assert_eq!(pack(&Value::Null), b"\x04");
        assert_eq!(pack(&Value::Int(0)), b"\x08");
        assert_eq!(pack(&Value::Int(0x27)), b"\x2f");
        assert_eq!(pack(&Value::Int(0x28)), b"\x30\x28");
        assert_eq!(pack(&Value::Int(0x1FF)), b"\x31\xff\x01");
        assert_eq!(pack(&Value::Int(0x1FF_FFFF)), b"\x32\xff\xff\xff\x01");
        assert_eq!(
            pack(&Value::Int(0x1FF_FFFF_FFFF_FFFF)),
            b"\x33\xff\xff\xff\xff\xff\xff\xff\x01"
        );
        assert_eq!(
            pack(&Value::Float(1.0)),
            b"\x36\x00\x00\x00\x00\x00\x00\xf0\x3f"
        );
        assert_eq!(pack(&s("abc")), b"\x43abc");
        let long = "a".repeat(33);
        assert_eq!(
            pack(&s(&long)),
            [b"\x61\x21".as_slice(), long.as_bytes()].concat()
        );
        let longer = "a".repeat(256);
        assert_eq!(
            pack(&s(&longer)),
            [b"\x62\x00\x01".as_slice(), longer.as_bytes()].concat()
        );
        assert_eq!(
            pack(&Value::Bytes(vec![0x12, 0x34, 0x56])),
            b"\x73\x12\x34\x56"
        );
        assert_eq!(
            pack(&Value::Bytes(vec![0x61; 33])),
            [b"\x91\x21".as_slice(), &[0x61; 33]].concat()
        );
        assert_eq!(
            pack(&Value::List(vec![
                Value::Int(1),
                s("test"),
                Value::Bool(false)
            ])),
            b"\xd3\x09\x44test\x02"
        );
        assert_eq!(
            pack(&Value::Dict(vec![
                (s("a"), Value::Int(12)),
                (Value::Bool(false), Value::Null)
            ])),
            b"\xe2\x41\x61\x14\x02\x04"
        );
        // Fifteen entries or more: open-ended, closed by 0x03.
        let list = Value::List(vec![Value::Int(1); 15]);
        let packed = pack(&list);
        assert_eq!(packed[0], 0xDF);
        assert_eq!(*packed.last().unwrap(), 0x03);
        assert_eq!(unpack(&packed).unwrap().0, list);
    }

    #[test]
    fn reads_what_pyatv_reads() {
        assert_eq!(unpack(b"\x2f").unwrap().0, Value::Int(0x27));
        assert_eq!(unpack(b"\x31\xff\x01").unwrap().0, Value::Int(0x1FF));
        assert_eq!(
            unpack(b"\x35\x00\x00\x80\x3f").unwrap().0,
            Value::Float(1.0)
        );
        assert_eq!(
            unpack(b"\xd3\x09\x44test\x02").unwrap().0,
            Value::List(vec![Value::Int(1), s("test"), Value::Bool(false)])
        );
        assert_eq!(
            unpack(b"\xe1\x01\xe1\x41\x61\x0a").unwrap().0,
            Value::Dict(vec![(
                Value::Bool(true),
                Value::Dict(vec![(s("a"), Value::Int(2))])
            )])
        );
        let (v, rest) = unpack(b"\x71\xac\xff").unwrap();
        assert_eq!((v, rest), (Value::Bytes(vec![0xAC]), b"\xff".as_slice()));
    }

    #[test]
    fn follows_back_references() {
        assert_eq!(
            unpack(b"\xd2\x41\x61\xa0").unwrap().0,
            Value::List(vec![s("a"), s("a")])
        );
        assert_eq!(
            unpack(b"\xd4\x43foo\x43bar\xa0\xa1").unwrap().0,
            Value::List(vec![s("foo"), s("bar"), s("foo"), s("bar")])
        );
        // A reference is not added again: "d" is the fourth value seen.
        assert_eq!(
            unpack(b"\xe3\x41\x61\x41\x62\x41\x63\xe1\x41\x64\xa0\xa3\x01")
                .unwrap()
                .0,
            Value::Dict(vec![
                (s("a"), s("b")),
                (s("c"), Value::Dict(vec![(s("d"), s("a"))])),
                (s("d"), Value::Bool(true)),
            ])
        );
        let list1 = [b"\xdf\x41\x61".as_slice(), &[0xA0; 15], b"\x03"].concat();
        let list2 = [b"\xdf\x41\x62".as_slice(), &[0xA1; 15], b"\x03"].concat();
        let both = [b"\xd2".as_slice(), &list1, &list2].concat();
        assert_eq!(
            unpack(&both).unwrap().0,
            Value::List(vec![
                Value::List(vec![s("a"); 16]),
                Value::List(vec![s("b"); 16])
            ])
        );
    }

    #[test]
    fn refuses_what_is_cut_or_unknown() {
        assert!(unpack(b"").is_err());
        assert!(unpack(b"\x43ab").is_err(), "string cut short");
        assert!(unpack(b"\xa0").is_err(), "reference to nothing");
        assert!(unpack(b"\xdf\x09").is_err(), "open list never closed");
        assert!(unpack(b"\x00").is_err());
        let deep = [vec![0xD1; 40], vec![0x08]].concat();
        assert!(unpack(&deep).is_err(), "too deep");
    }

    #[test]
    fn refuses_a_frame_that_would_decode_too_large() {
        // A 512 KiB string, then an open list of references to it: a frame of
        // about 1 MiB that would decode to hundreds of GiB.
        let long = vec![b'a'; 512 << 10];
        let mut frame = vec![0xD2, 0x63];
        frame.extend_from_slice(&u32::try_from(long.len()).unwrap().to_le_bytes()[..3]);
        frame.extend_from_slice(&long);
        frame.push(0xDF);
        frame.extend(std::iter::repeat_n(0xA0, 512 << 10));
        frame.push(0x03);
        let err = unpack(&frame).unwrap_err().to_string();
        assert!(err.contains("larger than"), "{err}");
        // So is a frame of many tiny values.
        let tiny = [vec![0xDF], vec![0x08; 1 << 20], vec![0x03]].concat();
        assert!(unpack(&tiny).is_err());
    }

    #[test]
    fn many_distinct_values_decode_quickly() {
        // Forty thousand small distinct strings: each is looked up in one
        // step, not compared to all the previous ones.
        let mut frame = vec![0xDF];
        for i in 0..40_000u32 {
            let s = format!("{i:05}");
            frame.push(0x40 + u8::try_from(s.len()).unwrap());
            frame.extend_from_slice(s.as_bytes());
        }
        frame.push(0x03);
        let start = std::time::Instant::now();
        let (value, _) = unpack(&frame).unwrap();
        assert!(matches!(value, Value::List(ref items) if items.len() == 40_000));
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn a_message_round_trips() {
        let message = Value::dict([
            ("_i", Value::from("_launchApp")),
            ("_t", Value::Int(2)),
            ("_x", Value::Int(70_000)),
            (
                "_c",
                Value::dict([("_bundleID", Value::from("com.netflix.Netflix"))]),
            ),
        ]);
        let packed = pack(&message);
        let (back, rest) = unpack(&packed).unwrap();
        assert!(rest.is_empty());
        assert_eq!(back, message);
        assert_eq!(back.get("_x").and_then(Value::as_int), Some(70_000));
        assert_eq!(
            back.get("_c")
                .and_then(|c| c.get("_bundleID"))
                .and_then(Value::as_str),
            Some("com.netflix.Netflix")
        );
    }
}
