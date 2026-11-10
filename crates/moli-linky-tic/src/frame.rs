//! The TIC stream of a Linky meter: frames, groups and their checksum.
//!
//! The meter sends frames in a loop, on a serial line (7 data bits, even parity,
//! one stop bit). A frame starts with STX and ends with ETX; it holds groups, each
//! one a line `LF … CR`:
//!
//! - **standard mode** (9600 baud): `LABEL HT [TIMESTAMP HT] DATA HT CHECKSUM`;
//! - **historic mode** (1200 baud): `LABEL SP DATA SP CHECKSUM`.
//!
//! The checksum is `(sum of the bytes & 0x3F) + 0x20`. The sum covers the whole group
//! before the checksum, with the separator that precedes it in standard mode, and
//! without it in historic mode. A group whose checksum fails is dropped: the next
//! frame carries the value again.

use serde::Deserialize;

const STX: u8 = 0x02;
const ETX: u8 = 0x03;
const LF: u8 = 0x0A;
const CR: u8 = 0x0D;
const HT: u8 = 0x09;
const SP: u8 = 0x20;
/// The longest group a meter sends is far below this: more means line noise.
const MAX_GROUP: usize = 256;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Standard,
    #[serde(alias = "historique")]
    Historic,
}

impl Mode {
    /// The line speed of this mode.
    #[must_use]
    pub fn baud_rate(self) -> u32 {
        match self {
            Self::Standard => 9600,
            Self::Historic => 1200,
        }
    }

    fn separator(self) -> u8 {
        match self {
            Self::Standard => HT,
            Self::Historic => SP,
        }
    }
}

/// One data group of a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub label: String,
    pub timestamp: Option<String>,
    pub value: String,
}

/// Why a group is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Not shaped like a group of this mode.
    Malformed,
    /// Shaped right, but the checksum does not match (a flipped bit on the line).
    Checksum,
}

/// What the stream says, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Group(Group),
    /// ETX: every group of the frame has been seen.
    FrameEnd,
    Dropped(Fault),
}

/// The checksum character of `bytes` (see the module documentation for what to sum).
#[must_use]
pub fn checksum(bytes: &[u8]) -> u8 {
    let sum = bytes.iter().fold(0_u32, |sum, byte| sum + u32::from(*byte));
    // Masked to six bits: always fits a byte.
    u8::try_from(sum & 0x3F).unwrap_or(0) + 0x20
}

/// Parses one group, the bytes between LF and CR.
pub fn parse_group(line: &[u8], mode: Mode) -> Result<Group, Fault> {
    let sep = mode.separator();
    // At least a label, a separator, a separator and the checksum.
    let [body @ .., last_sep, check] = line else {
        return Err(Fault::Malformed);
    };
    if body.len() < 2 || *last_sep != sep {
        return Err(Fault::Malformed);
    }
    let summed = match mode {
        Mode::Standard => &line[..line.len() - 1],
        Mode::Historic => body,
    };
    if checksum(summed) != *check {
        return Err(Fault::Checksum);
    }
    let text = |bytes: &[u8]| -> Result<String, Fault> {
        if bytes.iter().all(|b| b.is_ascii() && !b.is_ascii_control()) {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        } else {
            Err(Fault::Malformed)
        }
    };
    let fields: Vec<&[u8]> = body.split(|b| *b == sep).collect();
    match (mode, fields.as_slice()) {
        (_, [label, value]) if !label.is_empty() => Ok(Group {
            label: text(label)?,
            timestamp: None,
            value: text(value)?,
        }),
        (Mode::Standard, [label, timestamp, value]) if !label.is_empty() => Ok(Group {
            label: text(label)?,
            timestamp: Some(text(timestamp)?),
            value: text(value)?,
        }),
        _ => Err(Fault::Malformed),
    }
}

/// Turns the bytes of the line into events, whatever the size of the chunks it
/// is read in.
#[derive(Debug)]
pub struct Reader {
    mode: Mode,
    group: Vec<u8>,
    inside: bool,
}

impl Reader {
    #[must_use]
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            group: Vec::new(),
            inside: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &byte in bytes {
            match byte {
                // A frame boundary cuts whatever group was in progress: it was truncated.
                STX | ETX => {
                    self.inside = false;
                    self.group.clear();
                    if byte == ETX {
                        events.push(Event::FrameEnd);
                    }
                }
                LF => {
                    self.inside = true;
                    self.group.clear();
                }
                CR if self.inside => {
                    self.inside = false;
                    events.push(match parse_group(&self.group, self.mode) {
                        Ok(group) => Event::Group(group),
                        Err(fault) => Event::Dropped(fault),
                    });
                    self.group.clear();
                }
                other if self.inside => {
                    if self.group.len() < MAX_GROUP {
                        self.group.push(other);
                    } else {
                        // Noise: give the group up, wait for the next LF.
                        self.inside = false;
                        self.group.clear();
                        events.push(Event::Dropped(Fault::Malformed));
                    }
                }
                _ => {}
            }
        }
        events
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A well-formed group, the way a meter sends it. Values are made up.
    pub(crate) fn group(mode: Mode, parts: &[&str]) -> Vec<u8> {
        let sep = mode.separator();
        let mut body = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                body.push(sep);
            }
            body.extend_from_slice(part.as_bytes());
        }
        let mut line = vec![LF];
        let check = match mode {
            Mode::Standard => {
                body.push(sep);
                checksum(&body)
            }
            Mode::Historic => checksum(&body),
        };
        line.extend_from_slice(&body);
        if mode == Mode::Historic {
            line.push(sep);
        }
        line.push(check);
        line.push(CR);
        line
    }

    fn parse(mode: Mode, parts: &[&str]) -> Result<Group, Fault> {
        let line = group(mode, parts);
        parse_group(&line[1..line.len() - 1], mode)
    }

    #[test]
    fn the_checksum_follows_the_rule() {
        // "AB" HT "C" HT: 65 + 66 + 9 + 67 + 9 = 216; 216 & 63 = 24; 24 + 32 = 56 = '8'.
        assert_eq!(checksum(b"AB\tC\t"), b'8');
    }

    #[test]
    fn a_standard_group_with_and_without_a_timestamp() {
        let plain = parse(Mode::Standard, &["SINSTS", "00532"]).unwrap();
        assert_eq!(plain.label, "SINSTS");
        assert_eq!(plain.value, "00532");
        assert_eq!(plain.timestamp, None);
        let dated = parse(Mode::Standard, &["SMAXSN", "E261105083000", "03215"]).unwrap();
        assert_eq!(dated.timestamp.as_deref(), Some("E261105083000"));
        assert_eq!(dated.value, "03215");
    }

    #[test]
    fn a_standard_value_may_hold_spaces() {
        let group = parse(Mode::Standard, &["LTARF", "HEURE PLEINE "]).unwrap();
        assert_eq!(group.value, "HEURE PLEINE ");
    }

    #[test]
    fn a_standard_group_may_have_an_empty_value() {
        let group = parse(Mode::Standard, &["MSG1", ""]).unwrap();
        assert_eq!(group.value, "");
    }

    #[test]
    fn a_historic_group() {
        let group = parse(Mode::Historic, &["PAPP", "00532"]).unwrap();
        assert_eq!(group.label, "PAPP");
        assert_eq!(group.value, "00532");
    }

    #[test]
    fn a_historic_checksum_may_be_a_space() {
        // A value whose checksum is SP (0x20): the sum is a multiple of 64.
        let value = (0..=99_999_u32)
            .map(|n| format!("{n:09}"))
            .find(|v| checksum(format!("BASE {v}").as_bytes()) == SP)
            .unwrap();
        let group = parse(Mode::Historic, &["BASE", &value]).unwrap();
        assert_eq!(group.value, value);
    }

    #[test]
    fn a_flipped_bit_is_a_checksum_fault() {
        let mut line = group(Mode::Standard, &["SINSTS", "00532"]);
        // "SINSTS" -> "RINSTS": one bit of the first byte.
        line[1] ^= 0x01;
        assert_eq!(
            parse_group(&line[1..line.len() - 1], Mode::Standard),
            Err(Fault::Checksum)
        );
    }

    #[test]
    fn a_group_of_the_wrong_shape_is_malformed() {
        assert_eq!(parse_group(b"", Mode::Standard), Err(Fault::Malformed));
        assert_eq!(parse_group(b"AB", Mode::Standard), Err(Fault::Malformed));
        // A standard group read in historic mode.
        let line = group(Mode::Standard, &["SINSTS", "00532"]);
        assert!(parse_group(&line[1..line.len() - 1], Mode::Historic).is_err());
        // Four fields where a standard group holds at most three.
        assert_eq!(
            parse(Mode::Standard, &["A", "B", "C", "D"]),
            Err(Fault::Malformed)
        );
    }

    #[test]
    fn a_frame_is_read_whatever_the_chunks() {
        let mut stream = vec![STX];
        stream.extend(group(Mode::Standard, &["SINSTS", "00532"]));
        stream.extend(group(Mode::Standard, &["EAST", "027175524"]));
        stream.push(ETX);
        let whole = Reader::new(Mode::Standard).push(&stream);
        let mut reader = Reader::new(Mode::Standard);
        let mut by_byte = Vec::new();
        for byte in &stream {
            by_byte.extend(reader.push(&[*byte]));
        }
        assert_eq!(whole, by_byte);
        assert_eq!(whole.len(), 3);
        assert!(matches!(&whole[0], Event::Group(g) if g.label == "SINSTS"));
        assert!(matches!(&whole[1], Event::Group(g) if g.label == "EAST"));
        assert_eq!(whole[2], Event::FrameEnd);
    }

    #[test]
    fn a_stream_started_mid_frame_loses_only_the_partial_group() {
        let mut stream = b"ST\t00532\tX\r".to_vec();
        stream.extend(group(Mode::Standard, &["SINSTS", "00532"]));
        let events = Reader::new(Mode::Standard).push(&stream);
        // The tail before the first LF is ignored; the next group is read.
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::Group(g) if g.label == "SINSTS"));
    }

    #[test]
    fn a_truncated_group_is_dropped_at_the_next_boundary() {
        let mut stream = vec![LF];
        stream.extend_from_slice(b"EAST\t0271");
        stream.push(ETX);
        stream.push(STX);
        stream.extend(group(Mode::Standard, &["SINSTS", "00532"]));
        let events = Reader::new(Mode::Standard).push(&stream);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], Event::FrameEnd);
        assert!(matches!(&events[1], Event::Group(g) if g.label == "SINSTS"));
    }

    #[test]
    fn noise_without_end_cannot_grow_without_bound() {
        let mut stream = vec![LF];
        stream.extend(std::iter::repeat_n(b'A', MAX_GROUP + 50));
        let events = Reader::new(Mode::Standard).push(&stream);
        assert_eq!(events, vec![Event::Dropped(Fault::Malformed)]);
    }

    #[test]
    fn a_bad_checksum_does_not_stop_the_next_group() {
        let mut bad = group(Mode::Standard, &["SINSTS", "00532"]);
        let at = bad.len() - 2;
        bad[at] = if bad[at] == b'A' { b'B' } else { b'A' };
        let mut stream = bad;
        stream.extend(group(Mode::Standard, &["EAST", "027175524"]));
        let events = Reader::new(Mode::Standard).push(&stream);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], Event::Dropped(Fault::Checksum));
        assert!(matches!(&events[1], Event::Group(g) if g.label == "EAST"));
    }
}
