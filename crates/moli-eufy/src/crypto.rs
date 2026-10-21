//! What Eufy's cloud API needs: a P-256 key agreement with the server,
//! AES-256-CBC keyed by the shared secret (its first 16 bytes as the IV),
//! base64 and hexadecimal.

use aes::Aes256;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt as _, BlockEncrypt as _, KeyInit as _};
use anyhow::{Context as _, bail, ensure};
use p256::elliptic_curve::sec1::ToEncodedPoint as _;
use p256::{PublicKey, SecretKey};
use ring::rand::{SecureRandom as _, SystemRandom};

/// The client's half of the key agreement. Kept in the vault: the server
/// answers with the key it settled on at login, for this client key.
pub struct ClientKey(SecretKey);

impl std::fmt::Debug for ClientKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClientKey(<redacted>)")
    }
}

impl ClientKey {
    pub fn generate() -> anyhow::Result<Self> {
        loop {
            let bytes = random::<32>()?;
            // Out of the curve's range once in 2^32 draws: draw again.
            if let Ok(key) = SecretKey::from_slice(&bytes) {
                return Ok(Self(key));
            }
        }
    }

    pub fn from_hex(hex: &str) -> anyhow::Result<Self> {
        let bytes = unhex(hex).context("client key")?;
        Ok(Self(SecretKey::from_slice(&bytes).context("client key")?))
    }

    pub fn to_hex(&self) -> String {
        hex(&self.0.to_bytes())
    }

    /// Uncompressed SEC1, as the server expects it (`04…`).
    pub fn public_hex(&self) -> String {
        hex(self.0.public_key().to_encoded_point(false).as_bytes())
    }

    /// The AES key shared with the holder of `server` (uncompressed hex).
    pub fn shared(&self, server: &str) -> anyhow::Result<[u8; 32]> {
        let server = PublicKey::from_sec1_bytes(&unhex(server).context("server key")?)
            .context("server key")?;
        let shared = p256::ecdh::diffie_hellman(self.0.to_nonzero_scalar(), server.as_affine());
        Ok((*shared.raw_secret_bytes()).into())
    }
}

/// AES-256-CBC, PKCS#7, base64: how the password travels.
pub fn encrypt(key: &[u8; 32], plain: &[u8]) -> String {
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut chain: [u8; 16] = key[..16].try_into().expect("16 bytes");
    let pad = 16 - plain.len() % 16;
    let mut data = plain.to_vec();
    data.resize(plain.len() + pad, u8::try_from(pad).expect("at most 16"));
    for block in data.as_chunks_mut::<16>().0 {
        for (b, c) in block.iter_mut().zip(chain) {
            *b ^= c;
        }
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
        chain = *block;
    }
    base64(&data)
}

/// The reverse of [`encrypt`]; the server pads its JSON with a NUL, cut.
pub fn decrypt(key: &[u8; 32], text: &str) -> anyhow::Result<Vec<u8>> {
    let mut data = unbase64(text).context("not base64")?;
    ensure!(
        !data.is_empty() && data.len() % 16 == 0,
        "not whole AES blocks"
    );
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut chain: [u8; 16] = key[..16].try_into().expect("16 bytes");
    for block in data.as_chunks_mut::<16>().0 {
        let next = *block;
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
        for (b, c) in block.iter_mut().zip(chain) {
            *b ^= c;
        }
        chain = next;
    }
    let pad = usize::from(*data.last().expect("not empty"));
    if pad == 0
        || pad > 16
        || data[data.len() - pad..]
            .iter()
            .any(|&b| usize::from(b) != pad)
    {
        bail!("wrong key (bad padding)");
    }
    data.truncate(data.len() - pad);
    if let Some(end) = data.iter().position(|&b| b == 0) {
        data.truncate(end);
    }
    Ok(data)
}

pub fn random<const N: usize>() -> anyhow::Result<[u8; N]> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("no randomness"))?;
    Ok(bytes)
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64; padding and line breaks tolerated.
pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'=' | b'\n' | b'\r' => continue,
            _ => u32::try_from(ALPHABET.iter().position(|&a| a == c)?).ok()?,
        };
        acc = ((acc << 6) | v) & 0xFF_FFFF;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xFF).ok()?);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::SERVER_PUBLIC_KEY;

    // Vectors from Node's `crypto` (createECDH, aes-256-cbc), as
    // eufy-security-client uses it, with a fixed client key.
    const CLIENT: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    #[test]
    fn the_key_agreement_matches_node() {
        let key = ClientKey::from_hex(CLIENT).unwrap();
        assert_eq!(
            key.public_hex(),
            "040217e617f0b6443928278f96999e69a23a4f2c152bdf6d6cdf66e5b80282d4ed\
             194a7debcb97712d2dda3ca85aa8765a56f45fc758599652f2897c65306e5794"
        );
        assert_eq!(
            hex(&key.shared(SERVER_PUBLIC_KEY).unwrap()),
            "9556b6dbf1af62ac177266f24941b3abc321f5cd571eebce06a17066127ec77e"
        );
        assert_eq!(
            ClientKey::from_hex(&key.to_hex()).unwrap().public_hex(),
            key.public_hex()
        );
    }

    #[test]
    fn the_password_is_encrypted_like_node() {
        let shared = ClientKey::from_hex(CLIENT)
            .unwrap()
            .shared(SERVER_PUBLIC_KEY)
            .unwrap();
        assert_eq!(
            encrypt(&shared, b"correct horse"),
            "6KzdRHu3qPhyU6iA3tAKAA=="
        );
        assert_eq!(
            decrypt(&shared, "6KzdRHu3qPhyU6iA3tAKAA==").unwrap(),
            b"correct horse"
        );
    }

    #[test]
    fn answers_are_decrypted_and_cut_at_the_nul() {
        let shared = ClientKey::from_hex(CLIENT)
            .unwrap()
            .shared(SERVER_PUBLIC_KEY)
            .unwrap();
        let plain = decrypt(
            &shared,
            "8TzFCBKmUfrye2FZys2lsjiY8R/J16onAE3MN5UJerszWPdQO3s1l/GjgnhZrTO+",
        )
        .unwrap();
        assert_eq!(plain, br#"[{"station_sn":"T0000000000000000"}]"#);
        assert!(decrypt(&[0; 32], "6KzdRHu3qPhyU6iA3tAKAA==").is_err());
    }

    #[test]
    fn base64_round_trips() {
        for len in 0..10 {
            let bytes: Vec<u8> = (0..len).map(|i: u8| i.wrapping_mul(37)).collect();
            assert_eq!(unbase64(&base64(&bytes)).unwrap(), bytes);
        }
        assert_eq!(base64(b"Ma"), "TWE=");
        assert!(unbase64("**").is_none());
    }
}
