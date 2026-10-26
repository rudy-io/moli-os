//! Bytes as text: base64 and hex, the one copy every crate uses.
//!
//! Decoding is lenient on purpose (what devices and clouds send varies):
//! standard or URL-safe alphabet, padding or none, whitespace ignored.

use base64::Engine as _;
use base64::alphabet;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{
    GeneralPurpose, GeneralPurposeConfig, STANDARD, URL_SAFE_NO_PAD,
};

/// Any padding, trailing bits tolerated: what the hand-written decoders took.
const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// URL-safe, any padding, nothing else.
const STRICT_URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Standard base64, padded.
#[must_use]
pub fn base64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// URL-safe base64, without padding (JWT, PKCE, web push).
#[must_use]
pub fn base64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Base64, standard or URL-safe, padded or not, whitespace ignored.
#[must_use]
pub fn from_base64(text: &str) -> Option<Vec<u8>> {
    let text: String = text
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    LENIENT.decode(text).ok()
}

/// URL-safe base64 only, padded or not (a signed token's parts: the
/// standard alphabet and whitespace are refused).
#[must_use]
pub fn from_base64url(text: &str) -> Option<Vec<u8>> {
    STRICT_URL.decode(text).ok()
}

/// Bytes in lower-case hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// Hex of either case, an even number of digits.
#[must_use]
pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_examples() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), coded);
            assert_eq!(from_base64(coded).unwrap(), plain.as_bytes());
        }
        assert_eq!(base64(&[0xfb, 0xff, 0xfe]), "+//+");
        assert_eq!(base64url(&[0xfb, 0xff, 0xfe]), "-__-");
        assert_eq!(base64url(b"ab"), "YWI");
    }

    #[test]
    fn decoding_takes_what_devices_send() {
        assert_eq!(from_base64("YWI").unwrap(), b"ab");
        assert_eq!(from_base64("YWI=").unwrap(), b"ab");
        assert_eq!(from_base64(" YW\nI= ").unwrap(), b"ab");
        assert_eq!(from_base64("-__-").unwrap(), [0xfb, 0xff, 0xfe]);
        assert_eq!(from_base64("+//+").unwrap(), [0xfb, 0xff, 0xfe]);
        assert!(from_base64("Y!WI").is_none());
        assert!(from_base64("Y").is_none());
        assert_eq!(from_base64url("-__-").unwrap(), [0xfb, 0xff, 0xfe]);
        assert_eq!(from_base64url("YWI=").unwrap(), b"ab");
        assert!(from_base64url("a+b/").is_none());
        assert!(from_base64url(" YWI").is_none());
    }

    #[test]
    fn hex_both_ways() {
        assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
        assert_eq!(from_hex("00AB10").unwrap(), [0, 0xab, 0x10]);
        assert!(from_hex("abc").is_none());
        assert!(from_hex("zz").is_none());
    }
}
