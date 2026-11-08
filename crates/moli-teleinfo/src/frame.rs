//! Incremental parser for teleinfo frames (Linky TIC channel).
//!
//! A frame is `STX(0x02)` + LF, then lines of `TAG SEP VALUE… SEP CHK \r\n`,
//! then `ETX(0x03)` right after the last line's CR. Two wire formats, both
//! 7E1 (7 data bits, even parity, 1 stop bit):
//!
//! - **standard** (9600 baud): separator is the tab. The checksum covers
//!   every byte after the tag, *including* the last tab before the checksum.
//! - **legacy** (1200 baud, « historique »): separator is the space. The
//!   checksum covers every byte after the tag, *excluding* the last space.
//!
//! Checksum: `(sum(prefix bytes) & 0x3F) + 0x20` — an ASCII char 0x20..0x7F.
//! The format of a frame is detected per line (tab → standard, space →
//! legacy), so a stream whose mode changes between frames still parses.
//!
//! Lines with a bad checksum are counted (`Frame::bad_lines`) and dropped;
//! they never abort the frame. Bytes outside any frame are noise: dropped.

const STX: u8 = 0x02;
const ETX: u8 = 0x03;
/// A real teleinfo frame is under 2 kB: beyond this it is noise, resync on STX.
const MAX_FRAME_BYTES: usize = 8 * 1024;

/// The wire format a frame was parsed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 9600 baud, tab-separated lines (a Linky in « standard » mode).
    Standard,
    /// 1200 baud, space-separated lines (a Linky in « historique » mode).
    Legacy,
}

/// One verified line of a frame: its tag and its value fields.
/// `values` holds every field after the tag (standard frames may carry two,
/// e.g. `SMAXSN` = date + power): `values[0]` is the primary one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub tag: String,
    pub values: Vec<String>,
}

/// A complete frame, verified line by line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The format of the first verified line (None: no verified line).
    pub mode: Option<Mode>,
    /// The verified lines, in wire order.
    pub lines: Vec<Line>,
    /// Lines that failed checksum or shape: dropped, counted.
    pub bad_lines: usize,
}

/// Feeds raw port bytes and yields the frames they contain.
#[derive(Clone, Debug, Default)]
pub struct FrameParser {
    in_frame: bool,
    buf: Vec<u8>,
}

impl FrameParser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resyncs the parser (a new port session: the buffer may hold a
    /// half-frame from before the disconnect).
    pub fn reset(&mut self) {
        self.in_frame = false;
        self.buf.clear();
    }

    /// Consumes a chunk of raw bytes and returns the complete frames it
    /// contains (possibly none: the chunk may hold only a partial frame).
    #[must_use]
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Frame> {
        let mut frames = Vec::new();
        for &byte in chunk {
            if !self.in_frame {
                if byte == STX {
                    self.in_frame = true;
                    self.buf.clear();
                }
                continue;
            }
            if byte == ETX {
                self.in_frame = false;
                if let Some(frame) = parse_frame(&self.buf) {
                    frames.push(frame);
                }
                self.buf.clear();
                continue;
            }
            if self.buf.len() >= MAX_FRAME_BYTES {
                // Oversized: noise. Drop the half-frame, wait for the next STX.
                self.in_frame = false;
                self.buf.clear();
                continue;
            }
            self.buf.push(byte);
        }
        frames
    }
}

/// Parses the bytes between STX and ETX. None: an empty (or fully invalid)
/// frame — nothing to publish, nothing to blame.
fn parse_frame(raw: &[u8]) -> Option<Frame> {
    // 7E1 bytes are ASCII-safe; any residue of a line shift degrades to the
    // replacement char, fails the checksum, and is counted.
    let text = String::from_utf8_lossy(raw);
    let mut frame = Frame {
        mode: None,
        lines: Vec::new(),
        bad_lines: 0,
    };
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        match parse_line(line) {
            Some((mode, parsed)) => {
                frame.mode = Some(mode);
                frame.lines.push(parsed);
            }
            None => frame.bad_lines += 1,
        }
    }
    if frame.lines.is_empty() && frame.bad_lines == 0 {
        return None;
    }
    Some(frame)
}

/// One line: `TAG SEP VALUE[ SEP VALUE…] SEP CHK`. The last field is the
/// checksum char; the standard format counts its separator in the sum, the
/// legacy one does not. None: bad checksum, bad shape, or bad tag.
fn parse_line(line: &str) -> Option<(Mode, Line)> {
    let bytes = line.as_bytes();
    // Minimum shape: a tag, a separator, a checksum char (the value may be
    // empty: `DATE …` carries an empty second field on some meters).
    if bytes.len() < 3 {
        return None;
    }
    let checksum = bytes[bytes.len() - 1];
    if !is_checksum_char(checksum) {
        return None;
    }
    // The prefix is everything the checksum is computed over.
    let (prefix, mode) = if line.contains('\t') {
        (&line[..line.len() - 1], Mode::Standard) // the last tab is in the sum
    } else {
        (&line[..line.len() - 2], Mode::Legacy) // the last space is not
    };
    let expected = (sum_bytes(prefix) & 0x3F) + 0x20;
    if expected != u32::from(checksum) {
        return None;
    }
    let (tag, values) = if mode == Mode::Standard {
        let fields: Vec<&str> = prefix.split('\t').collect();
        if fields.len() < 2 {
            return None;
        }
        let mut values = fields[1..].to_vec();
        // The final separator (counted in the checksum) leaves a trailing
        // empty field: drop that artifact, keep a genuine empty value before
        // it (`DATE` carries an empty second field on some meters).
        if values.last().is_some_and(|v| v.is_empty()) {
            values.pop();
        }
        (fields[0], values)
    } else {
        match prefix.split_once(' ') {
            Some((t, v)) => (t, vec![v]),
            None => return None,
        }
    };
    if !is_tag(tag) {
        return None;
    }
    Some((
        mode,
        Line {
            tag: tag.to_owned(),
            values: values.into_iter().map(ToOwned::to_owned).collect(),
        },
    ))
}

/// Tag charset observed on both modes: letters, digits, `-`, `+`
/// (`ADCO`, `EASD01`, `SMAXSN-1`, `PJOURF+1`…).
fn is_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'+')
}

fn is_checksum_char(byte: u8) -> bool {
    (0x20..=0x7F).contains(&byte)
}

fn sum_bytes(text: &str) -> u32 {
    text.bytes().map(u32::from).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// The real captures committed as fixtures of the integration package.
    fn fixture(name: &str) -> PathBuf {
        let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        here.join("../../integrations/teleinfo/fixtures").join(name)
    }

    fn parse_all(text: &str) -> Vec<Frame> {
        let mut parser = FrameParser::new();
        parser.feed(text.as_bytes())
    }

    /// Standard-mode checksum char (same rule as the parser).
    fn std_checksum(prefix: &str) -> char {
        char::from_u32((sum_bytes(prefix) & 0x3F) + 0x20).unwrap()
    }

    #[test]
    fn a_frame_is_found_between_stx_and_etx() {
        // Standard frame, two lines (the checksums are computed below, not
        // hardcoded: they must agree with this very parser's rule).
        let line1 = format!(
            "ADSC\t000000000002\t{}",
            std_checksum("ADSC\t000000000002\t")
        );
        let line2 = format!("VTIC\t02\t{}", std_checksum("VTIC\t02\t"));
        let frame_text = format!("\u{02}\n{line1}\r\n{line2}\r\u{03}");
        let frames = parse_all(&frame_text);
        assert_eq!(frames.len(), 1, "{frame_text:?}");
        let frame = &frames[0];
        assert_eq!(frame.mode, Some(Mode::Standard));
        assert_eq!(frame.bad_lines, 0);
        assert_eq!(
            frame.lines,
            vec![
                Line {
                    tag: "ADSC".into(),
                    values: vec!["000000000002".into()]
                },
                Line {
                    tag: "VTIC".into(),
                    values: vec!["02".into()]
                },
            ]
        );
    }

    #[test]
    fn the_standard_checksum_counts_the_last_separator() {
        // From the real standard capture: `EASD03\t000000000\t"` — the final
        // tab is inside the summed prefix.
        let prefix = "EASD03\t000000000\t";
        assert_eq!((sum_bytes(prefix) & 0x3F) + 0x20, u32::from(b'"'));
        // And a line carrying an empty second field (DATE on a TEMPO meter).
        let date_prefix = "DATE\tH200214230804\t\t";
        assert_eq!((sum_bytes(date_prefix) & 0x3F) + 0x20, u32::from(b';'));
    }

    #[test]
    fn the_legacy_checksum_skips_the_last_separator() {
        // From the real legacy capture: `ADCO 000000000001 X` and `PTEC HPJB P`.
        assert_eq!(
            (sum_bytes("ADCO 000000000001") & 0x3F) + 0x20,
            u32::from(b'X')
        );
        assert_eq!((sum_bytes("PTEC HPJB") & 0x3F) + 0x20, u32::from(b'P'));
    }

    #[test]
    fn a_bad_checksum_line_is_dropped_and_counted() {
        let good = format!(
            "ADSC\t000000000002\t{}",
            std_checksum("ADSC\t000000000002\t")
        );
        let bad = format!("VTIC\t02\t{}", (b'0' as char));
        let frame_text = format!("\u{02}\n{good}\r\n{bad}\r\u{03}");
        let frames = parse_all(&frame_text);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].lines.len(), 1);
        assert_eq!(frames[0].lines[0].tag, "ADSC");
        assert_eq!(frames[0].bad_lines, 1);
    }

    #[test]
    fn partial_chunks_reassemble_into_one_frame() {
        let good = format!(
            "ADSC\t000000000002\t{}",
            std_checksum("ADSC\t000000000002\t")
        );
        let line = format!("VTIC\t02\t{}", std_checksum("VTIC\t02\t"));
        let whole = format!("\u{02}\n{good}\r\n{line}\r\u{03}");
        let bytes = whole.as_bytes();
        let mut parser = FrameParser::new();
        assert!(parser.feed(&bytes[..5]).is_empty(), "no frame yet");
        assert!(
            parser.feed(&bytes[5..bytes.len() - 3]).is_empty(),
            "still no frame"
        );
        let frames = parser.feed(&bytes[bytes.len() - 3..]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].lines.len(), 2);
    }

    #[test]
    fn noise_outside_frames_is_dropped() {
        let good = format!(
            "ADSC\t000000000002\t{}",
            std_checksum("ADSC\t000000000002\t")
        );
        let line = format!("VTIC\t02\t{}", std_checksum("VTIC\t02\t"));
        let whole = format!("\u{02}\n{good}\r\n{line}\r\u{03}");
        let text = format!("garbage before\u{01}\u{00}\u{04}{whole}trailing\u{05}");
        let frames = parse_all(&text);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].lines.len(), 2);
    }

    #[test]
    fn an_oversized_frame_is_dropped_and_resyncs() {
        let mut parser = FrameParser::new();
        let junk = vec![b'x'; MAX_FRAME_BYTES + 16];
        let _ = parser.feed(&[STX]);
        let _ = parser.feed(&junk);
        assert!(parser.feed(b"still inside the dropped frame").is_empty());
        // A real frame right after the noise still parses.
        let good = format!(
            "ADSC\t000000000002\t{}",
            std_checksum("ADSC\t000000000002\t")
        );
        let line = format!("VTIC\t02\t{}", std_checksum("VTIC\t02\t"));
        let whole = format!("\u{02}\n{good}\r\n{line}\r\u{03}");
        let frames = parser.feed(whole.as_bytes());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].lines.len(), 2);
    }

    #[test]
    fn the_real_standard_capture_parses() {
        let bytes = fs::read(fixture("standard_raw.txt")).expect("standard capture fixture");
        let mut parser = FrameParser::new();
        let frames = parser.feed(&bytes);
        assert_eq!(frames.len(), 7, "the capture holds 7 complete frames");
        // The capture was made on a noisy line: 12 corrupted lines (bit flips,
        // stray 0x0C bytes, `@` tag damage) fail the checksum and get dropped.
        let total_bad: usize = frames.iter().map(|f| f.bad_lines).sum();
        assert_eq!(total_bad, 12, "corrupted lines in the standard capture");
        for frame in &frames {
            assert_eq!(frame.mode, Some(Mode::Standard));
            assert!(!frame.lines.is_empty());
            // The first line of every frame is ADSC, clean in this capture.
            assert_eq!(frame.lines[0].tag, "ADSC");
            let tags: Vec<&str> = frame.lines.iter().map(|l| l.tag.as_str()).collect();
            assert!(tags.contains(&"EAST"), "EAST missing in a frame");
        }
        // The first frame is clean: 55 lines (ADSC…PJOURF+1).
        let first = &frames[0];
        assert_eq!(first.lines.len(), 55, "first frame of the standard capture");
        assert_eq!(first.bad_lines, 0);
        // Spot-check the parsing of a multi-field line (SMAXSN: date + power).
        let smaxsn = first.lines.iter().find(|l| l.tag == "SMAXSN").unwrap();
        assert_eq!(
            smaxsn.values,
            vec!["H200214175135".to_owned(), "10802".to_owned()]
        );
    }

    #[test]
    fn the_real_legacy_capture_parses() {
        let bytes = fs::read(fixture("legacy_raw.txt")).expect("legacy capture fixture");
        let mut parser = FrameParser::new();
        let frames = parser.feed(&bytes);
        assert!(!frames.is_empty(), "the real capture must yield frames");
        for frame in &frames {
            assert_eq!(frame.mode, Some(Mode::Legacy));
            assert_eq!(frame.bad_lines, 0, "a bad line in the legacy capture");
            assert!(!frame.lines.is_empty());
        }
        let first = &frames[0];
        assert_eq!(first.lines.len(), 16, "first frame of the legacy capture");
        let get = |tag: &str| first.lines.iter().find(|l| l.tag == tag).unwrap();
        assert_eq!(get("ADCO").values[0], "000000000001");
        assert_eq!(get("PTEC").values[0], "HPJB");
        assert_eq!(get("DEMAIN").values[0], "BLEU");
        assert_eq!(get("OPTARIF").values[0], "BBR(");
        assert_eq!(get("PAPP").values[0], "00120");
        assert_eq!(get("MOTDETAT").values[0], "000000");
    }
}
