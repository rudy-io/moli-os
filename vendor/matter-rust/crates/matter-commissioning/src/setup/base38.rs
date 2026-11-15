//! Matter's custom Base38 codec (Matter Core Spec §5.1.3.1).
//!
//! Alphabet: `0-9A-Z-.` in that order (38 characters). Encoder consumes
//! 3 bytes → 5 chars per full chunk; partial trailing chunks of 2 bytes
//! emit 4 chars; partial trailing chunks of 1 byte emit 2 chars.
//!
//! Private to the `setup` module.

use crate::setup::{Error, Result};

const ALPHABET: &[u8; 38] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-.";

/// Encode `bytes` as a Matter Base38 string.
pub(super) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 5);
    let (chunks, remainder) = bytes.as_chunks::<3>();
    for chunk in chunks {
        let value = u32::from(chunk[0]) | (u32::from(chunk[1]) << 8) | (u32::from(chunk[2]) << 16);
        push_chars(&mut out, value, 5);
    }
    match remainder {
        [] => {}
        [b0] => push_chars(&mut out, u32::from(*b0), 2),
        [b0, b1] => {
            let value = u32::from(*b0) | (u32::from(*b1) << 8);
            push_chars(&mut out, value, 4);
        }
        _ => unreachable!("as_chunks remainder is < 3"),
    }
    out
}

/// Decode a Matter Base38 string to bytes.
///
/// # Errors
/// Returns [`Error::InvalidBase38Char`] on any character outside
/// `ALPHABET`. (Length-aware errors are surfaced by the caller — Base38
/// itself is permissive about length.)
pub(super) fn decode(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 5 + 1);
    let s_bytes = s.as_bytes();
    let mut i = 0usize;
    while i < s_bytes.len() {
        let remaining = s_bytes.len() - i;
        let take = if remaining >= 5 { 5 } else { remaining };
        let chunk = &s_bytes[i..i + take];
        let mut value: u64 = 0;
        // Most-significant first: matter.js places character 0 in the
        // low end of `value`, so we accumulate the chunk in reverse.
        for (off, byte) in chunk.iter().enumerate().rev() {
            let idx = alphabet_index(*byte, i + off)?;
            value = value * 38 + u64::from(idx);
        }
        let byte_count = match take {
            5 => 3,
            4 => 2,
            2 => 1,
            1 | 3 => return Err(Error::InvalidBase38Char('?', i + take - 1)),
            _ => unreachable!(),
        };
        // Reject a chunk whose value does not fit in `byte_count`
        // bytes (e.g. "S6" -> 256 > 0xFF). The old code silently kept only the
        // low bytes, so distinct QR strings decoded to identical output — the
        // decode was not injective. chip rejects these (Base38Decode.cpp:152).
        if value >> (byte_count * 8) != 0 {
            return Err(Error::Base38ChunkOutOfRange { position: i });
        }
        for shift in 0..byte_count {
            out.push(((value >> (shift * 8)) & 0xff) as u8);
        }
        i += take;
    }
    Ok(out)
}

fn push_chars(out: &mut String, mut value: u32, count: usize) {
    for _ in 0..count {
        let idx = (value % 38) as usize;
        // ALPHABET entries are all ASCII (`0..=9 A..=Z - .`), so each
        // byte is a valid single-byte char.
        out.push(ALPHABET[idx] as char);
        value /= 38;
    }
}

#[allow(clippy::cast_possible_truncation)] // ALPHABET has 38 entries, so position() returns at most 37 — the cast to u8 is exact.
fn alphabet_index(byte: u8, position: usize) -> Result<u8> {
    ALPHABET
        .iter()
        .position(|c| *c == byte)
        .map(|idx| idx as u8)
        .ok_or(Error::InvalidBase38Char(byte as char, position))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::{decode, encode};

    #[test]
    fn encode_empty() {
        assert_eq!(encode(&[]), "");
    }

    #[test]
    fn encode_one_byte_zero() {
        // value=0 → both chars are '0' (alphabet[0])
        assert_eq!(encode(&[0]), "00");
    }

    #[test]
    fn encode_one_byte_ff() {
        // value=255 → 255 = 6*38 + 27; chars in low-to-high are (27, 6)
        // alphabet[27]='R', alphabet[6]='6' → "R6"
        assert_eq!(encode(&[0xFF]), "R6");
    }

    #[test]
    fn encode_two_bytes_zero() {
        assert_eq!(encode(&[0, 0]), "0000");
    }

    #[test]
    fn encode_three_bytes_zero() {
        assert_eq!(encode(&[0, 0, 0]), "00000");
    }

    #[test]
    fn roundtrip_empty() {
        let v = encode(&[]);
        assert_eq!(decode(&v).unwrap(), &[] as &[u8]);
    }

    #[test]
    fn roundtrip_small() {
        for bytes in [
            &[0u8][..],
            &[0xFF][..],
            &[0, 0][..],
            &[0xFF, 0xFF][..],
            &[0, 0, 0][..],
            &[0xFF, 0xFF, 0xFF][..],
            &[0x12, 0x34, 0x56][..],
            &[1, 2, 3, 4, 5, 6][..],
            &[1, 2, 3, 4, 5, 6, 7][..],
            &[1, 2, 3, 4, 5, 6, 7, 8][..],
        ] {
            let s = encode(bytes);
            let back = decode(&s).unwrap();
            assert_eq!(back, bytes, "roundtrip failed for {bytes:?}");
        }
    }

    #[test]
    fn decode_rejects_invalid_char() {
        let err = decode("MT@").unwrap_err();
        assert!(matches!(
            err,
            crate::setup::Error::InvalidBase38Char('@', 2)
        ));
    }

    #[test]
    fn decode_rejects_chunk_of_three() {
        // Length 3 cannot represent any valid byte-aligned chunk.
        let err = decode("ABC").unwrap_err();
        assert!(matches!(err, crate::setup::Error::InvalidBase38Char(_, _)));
    }

    #[test]
    fn decode_rejects_out_of_range_chunks() {
        // Mirrors chip TestQRCode.cpp: a chunk whose value exceeds its
        // byte width must error, not silently truncate. "S6" -> 256 (>0xFF,
        // 1 byte), "OE71" -> 65536 (>0xFFFF, 2 bytes), "QLS18" -> 0x1000000
        // (>0xFFFFFF, 3 bytes).
        for input in ["S6", "OE71", "QLS18"] {
            let err = decode(input).unwrap_err();
            assert!(
                matches!(err, crate::setup::Error::Base38ChunkOutOfRange { .. }),
                "{input:?} must be rejected as out-of-range, got {err:?}"
            );
        }
        // Boundary: an in-range 1-byte chunk must still decode ("R6" -> 255).
        assert_eq!(decode("R6").unwrap(), vec![0xFF]);
    }
}
