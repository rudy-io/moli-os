//! The Wi-Fi Connect dongle's framing: `*`, a key byte, the obfuscated
//! text, `\r\n`. The text itself ends with `\r\n` too (the J.O.E. app sends
//! it, and the dongle drops a setting written without it). The obfuscation
//! is a per-nibble permutation keyed by the first byte, its own inverse:
//! not a cipher, a scrambling. Five byte values never appear raw: each is
//! sent as `0x1B` then itself XOR `0x80`.

const SBOX_A: [i64; 16] = [1, 0, 3, 2, 15, 14, 8, 10, 6, 13, 7, 12, 11, 9, 5, 4];
const SBOX_B: [i64; 16] = [9, 12, 6, 11, 10, 15, 2, 14, 13, 0, 4, 3, 1, 8, 7, 5];
const RESERVED: [u8; 5] = [0x00, 0x0A, 0x0D, 0x1B, 0x26];
const ESCAPE: u8 = 0x1B;

/// Longest frame kept while waiting for its end: the dongle's own replies
/// stay under 200 bytes; anything longer is noise and dropped.
const MAX_FRAME: usize = 2048;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn nibble(n: u8, pos: usize, key: u8) -> u8 {
    let (n, pos, hi, key) = (
        i64::from(n),
        pos as i64,
        i64::from(key >> 4),
        i64::from(key),
    );
    let i = (n + pos + hi).rem_euclid(16);
    let high = pos >> 4;
    let inner = (high + SBOX_A[i as usize] + key - pos - hi).rem_euclid(16);
    let outer = (SBOX_B[inner as usize] + hi + pos - key - high).rem_euclid(16);
    (SBOX_A[outer as usize] - pos - hi).rem_euclid(16) as u8
}

/// Scrambles or unscrambles (the same thing) bytes under `key`.
fn scramble(data: &[u8], key: u8) -> Vec<u8> {
    data.iter()
        .enumerate()
        .map(|(i, b)| (nibble(b >> 4, 2 * i, key) << 4) | nibble(b & 15, 2 * i + 1, key))
        .collect()
}

fn escape(b: u8, out: &mut Vec<u8>) {
    if RESERVED.contains(&b) {
        out.extend([ESCAPE, b ^ 0x80]);
    } else {
        out.push(b);
    }
}

/// A key the dongle takes: its low nibble is never 14 or 15 (the app
/// draws again then). `seed` is anything that changes from frame to frame.
#[must_use]
pub fn key(seed: u64) -> u8 {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    loop {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let k = x.to_le_bytes()[0];
        if k & 15 < 14 {
            return k;
        }
    }
}

/// One frame for `text` (`@TG:43`, or empty to end the session).
#[must_use]
pub fn frame(text: &str, key: u8) -> Vec<u8> {
    let mut body = text.as_bytes().to_vec();
    body.extend(b"\r\n");
    let mut out = vec![b'*'];
    escape(key, &mut out);
    for b in scramble(&body, key) {
        escape(b, &mut out);
    }
    out.extend(b"\r\n");
    out
}

/// The text of one frame's body (between `*` and `\r\n`).
fn text(body: &[u8]) -> String {
    let mut raw = Vec::with_capacity(body.len());
    let mut bytes = body.iter();
    while let Some(&b) = bytes.next() {
        if b == ESCAPE {
            if let Some(&next) = bytes.next() {
                raw.push(next ^ 0x80);
            }
        } else {
            raw.push(b);
        }
    }
    let Some((&key, rest)) = raw.split_first() else {
        return String::new();
    };
    let clear = scramble(rest, key);
    let clear: String = clear.iter().map(|&b| char::from(b)).collect();
    clear.trim_end_matches(['\r', '\n']).to_owned()
}

/// Bytes as they arrive, frames as they complete.
#[derive(Debug, Default)]
pub struct Frames {
    buf: Vec<u8>,
}

impl Frames {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() > MAX_FRAME {
            // No end in sight: keep only from the last start.
            let start = self
                .buf
                .iter()
                .rposition(|&b| b == b'*')
                .unwrap_or(self.buf.len());
            self.buf.drain(..start);
            if self.buf.len() > MAX_FRAME {
                self.buf.clear();
            }
        }
    }

    /// The next complete frame's text.
    pub fn next(&mut self) -> Option<String> {
        let star = self.buf.iter().position(|&b| b == b'*')?;
        let end = self.buf[star..].windows(2).position(|w| w == b"\r\n")? + star;
        let out = text(&self.buf[star + 1..end]);
        self.buf.drain(..end + 2);
        Some(out)
    }
}

/// Bytes in upper-case hex, as the machine writes them.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02X}");
            out
        })
}

/// The trailing byte of a setting or recipe (`ByteOperations.d` in the
/// app): every character of `text` summed, `-1 - sum`, low byte, in hex.
#[must_use]
pub fn checksum(text: &str) -> String {
    let sum = text.bytes().fold(0u8, u8::wrapping_add);
    format!("{:02X}", 0xFFu8.wrapping_sub(sum))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames made by the reference implementation (jura-connect, itself
    /// checked against real machines) for the same texts and keys.
    #[test]
    fn frames_match_the_reference() {
        for (text, key, expected) in VECTORS {
            assert_eq!(
                hex(&frame(text, *key)).to_ascii_lowercase(),
                *expected,
                "{text} {key:#x}"
            );
        }
    }

    #[test]
    fn a_frame_reads_back() {
        let mut frames = Frames::default();
        for seed in 0..500u64 {
            let text = format!("@tm:41,{seed:08X},{}", "&\r\n\0\x1b".repeat(3));
            let text = text.trim_end_matches(['\r', '\n']).to_owned();
            let bytes = frame(&text, key(seed));
            assert!(!bytes[1..bytes.len() - 2].windows(2).any(|w| w == b"\r\n"));
            // Delivered in two pieces, behind noise.
            let (a, b) = bytes.split_at(bytes.len() / 2);
            frames.push(b"noise");
            frames.push(a);
            assert_eq!(frames.next(), None);
            frames.push(b);
            assert_eq!(frames.next().as_deref(), Some(text.as_str()));
        }
    }

    #[test]
    fn keys_avoid_the_two_low_nibbles() {
        assert!((0..10_000).all(|s| key(s) & 15 < 14));
    }

    #[test]
    fn checksums_match_the_machine() {
        // Replies of a real E8: `@tm:02,0A00`, `@tm:13,21F096`, `@tm:41,…19`.
        assert_eq!(checksum("02,0A"), "00");
        assert_eq!(checksum("13,21F0"), "96");
        assert_eq!(checksum("41,0200080900000200000000000000"), "19");
    }

    #[test]
    fn a_flood_without_end_is_dropped() {
        let mut frames = Frames::default();
        frames.push(&[b'x'; 5000]);
        assert!(frames.buf.len() <= MAX_FRAME);
        frames.push(&frame("@TF:00", 3));
        assert_eq!(frames.next().as_deref(), Some("@TF:00"));
    }

    const VECTORS: &[(&str, u8, &str)] = &[
        ("@TG:43", 0x42, "2a4273ab635b9aa5dfa20d0a"),
        ("@TR:32,0A", 0x00, "2a1b80e72dba93bba8fb166b87b40d0a"),
        (
            "@HP:,4D6F6C69,",
            0x26,
            "2a1ba6788da91b8d94ddeea003a95e7d4f9273800d0a",
        ),
        (
            "@TP:02000809000002000100000000000000",
            0xBD,
            "2abd65fc87988bfacd76cc1b9bbaeee46aa46a031b80d76929fd28f194ddeb5306b5acb9e3f9aed898120d0a",
        ),
        ("", 0x1B, "2a1b9b62830d0a"),
        (
            "@TM:13,21F096",
            0xFD,
            "2afdd74fe6f2c87b5de9bd75e467a178870d0a",
        ),
    ];
}
