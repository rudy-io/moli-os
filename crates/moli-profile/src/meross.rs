//! Encrypted Meross local messages.
//!
//! Recent firmwares (seen: MSS110 7.3.46, hardware 7.0.0) list
//! `Appliance.Encrypt.ECDHE` among their abilities: they still answer a
//! `GET` in clear, but drop the connection, without a byte, on a `SET`
//! that is not encrypted. The message is then sent as base64 of
//! AES-256-CBC (zero IV of ASCII `0`s, zero padding), with a key derived
//! from the device's own uuid and MAC and the Meross key; the answer comes
//! back the same way. Protocol knowledge from the community (meross_lan);
//! no code copied.

use aes::Aes256;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt as _, BlockEncrypt as _, KeyInit as _};
use md5::{Digest as _, Md5};

/// The ability a device lists when its orders must be encrypted.
pub(crate) const ENCRYPTED: &str = "Appliance.Encrypt.ECDHE";

const IV: [u8; 16] = [b'0'; 16];

/// How a device takes its orders.
#[derive(Clone, Copy)]
pub(crate) enum Orders {
    Clear,
    Encrypted(Cipher),
}

impl Orders {
    pub(crate) fn cipher(self) -> Option<Cipher> {
        match self {
            Self::Clear => None,
            Self::Encrypted(cipher) => Some(cipher),
        }
    }
}

/// One device's message key.
#[derive(Clone, Copy)]
pub(crate) struct Cipher([u8; 32]);

impl Cipher {
    /// `md5(uuid[3..22] ‖ key[1..9] ‖ mac ‖ key[10..28])` in hex: 32
    /// characters, used as they are as the AES-256 key. A range past the
    /// end of a value is cut short (an empty key gives nothing).
    pub(crate) fn new(uuid: &str, key: &str, mac: &str) -> Self {
        fn part(s: &str, from: usize, to: usize) -> &[u8] {
            let s = s.as_bytes();
            &s[from.min(s.len())..to.min(s.len())]
        }
        let mut hash = Md5::new();
        for piece in [
            part(uuid, 3, 22),
            part(key, 1, 9),
            mac.as_bytes(),
            part(key, 10, 28),
        ] {
            hash.update(piece);
        }
        let hex = crate::hex(&hash.finalize());
        let mut key = [0u8; 32];
        key.copy_from_slice(hex.as_bytes());
        Self(key)
    }

    /// The message as the device reads it: base64 text.
    pub(crate) fn seal(&self, message: &[u8]) -> Vec<u8> {
        let cipher = Aes256::new(GenericArray::from_slice(&self.0));
        let mut data = message.to_vec();
        data.resize(message.len().div_ceil(16) * 16, 0);
        let mut previous = IV;
        for block in data.as_chunks_mut::<16>().0 {
            for (b, p) in block.iter_mut().zip(previous) {
                *b ^= p;
            }
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
            previous = *block;
        }
        base64_encode(&data).into_bytes()
    }

    /// An answer back in clear (`None`: not one of this device's messages).
    pub(crate) fn open(&self, body: &[u8]) -> Option<Vec<u8>> {
        let mut data = base64_decode(std::str::from_utf8(body).ok()?.trim())?;
        if data.is_empty() || data.len() % 16 != 0 {
            return None;
        }
        let cipher = Aes256::new(GenericArray::from_slice(&self.0));
        let mut previous = IV;
        for block in data.as_chunks_mut::<16>().0 {
            let sealed = *block;
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
            for (b, p) in block.iter_mut().zip(previous) {
                *b ^= p;
            }
            previous = sealed;
        }
        let end = data.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
        data.truncate(end);
        Some(data)
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut n, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = ALPHABET.iter().position(|a| *a == c)?;
        n = n << 6 | u32::try_from(v).ok()?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((n >> bits).to_le_bytes()[0]);
            n &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fictional device; expected values computed independently (Python's
    /// hashlib and `openssl enc -aes-256-cbc -nopad`).
    fn fictional() -> Cipher {
        Cipher::new(
            "00000000000000000000000000000001",
            "fictional-device-key",
            "02:00:00:00:00:01",
        )
    }

    #[test]
    fn the_key_comes_from_the_device_and_the_meross_key() {
        assert_eq!(&fictional().0, b"5e5775068a9dbd200cc00cf1f95a8c51");
    }

    #[test]
    fn a_message_is_sealed_like_the_device_expects_and_opens_again() {
        let message = br#"{"header":{"method":"SET"},"payload":{}}"#;
        let sealed = fictional().seal(message);
        assert_eq!(
            sealed,
            b"2aWI6Tx9JhX6cOGoxTDU3P2xZQPBuP+si2GCfYsLgrqSUW4cHv72WFofmSYmjkBL"
        );
        assert_eq!(fictional().open(&sealed).unwrap(), message);
    }

    #[test]
    fn what_is_not_a_sealed_message_does_not_open() {
        assert!(fictional().open(br#"{"header":{}}"#).is_none());
        assert!(fictional().open(b"").is_none());
        assert!(fictional().open(b"AAAA").is_none(), "not whole blocks");
    }

    #[test]
    fn an_empty_key_still_gives_a_key() {
        let keyless = Cipher::new("00000000000000000000000000000001", "", "02:00:00:00:00:01");
        let message = b"{}";
        assert_eq!(keyless.open(&keyless.seal(message)).unwrap(), message);
    }

    #[test]
    fn base64_round_trips_every_length() {
        for len in 0..10u8 {
            let bytes: Vec<u8> = (0..len).map(|i| i.wrapping_mul(97)).collect();
            assert_eq!(base64_decode(&base64_encode(&bytes)).unwrap(), bytes);
        }
        assert_eq!(base64_encode(b"Ma"), "TWE=");
    }
}
