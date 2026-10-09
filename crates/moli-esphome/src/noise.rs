//! `Noise_NNpsk0_25519_ChaChaPoly_SHA256`, the ESPHome API's encryption,
//! written from the Noise specification (revision 34) with `ring`'s
//! primitives: X25519, ChaCha20-Poly1305, SHA-256 and HMAC-SHA-256.
//!
//! The pattern: `-> psk, e` then `<- e, ee`. Moli is the initiator. Each
//! side keeps one key and a 64-bit counter per direction afterwards.

use anyhow::{Context as _, bail};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::digest::{self, SHA256};
use ring::hmac;
use ring::rand::SystemRandom;

const PROTOCOL: &[u8] = b"Noise_NNpsk0_25519_ChaChaPoly_SHA256";
/// ESPHome's prologue: `NoiseAPIInit` and the length (u16, big-endian) of
/// the client hello's body, always empty.
pub const PROLOGUE: &[u8] = b"NoiseAPIInit\x00\x00";
const DH_LEN: usize = 32;
const TAG_LEN: usize = 16;
/// The first handshake message: an ephemeral key and the tag of an empty payload.
pub const MSG1_LEN: usize = DH_LEN + TAG_LEN;

/// The base64 key of an ESPHome API (`api: encryption: key:`): 32 bytes.
pub fn psk_from_base64(text: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = base64(text.trim()).context("the API key is not base64")?;
    bytes
        .try_into()
        .map_err(|b: Vec<u8>| anyhow::anyhow!("the API key is {} bytes, not 32", b.len()))
}

/// A new random key, in the base64 form ESPHome takes (and gives back).
pub fn new_key_base64() -> anyhow::Result<String> {
    use ring::rand::SecureRandom as _;
    let mut key = [0u8; 32];
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| anyhow::anyhow!("no random key"))?;
    Ok(base64_encode(&key))
}

/// Standard base64 with padding.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((word >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64 with padding (the key's only form).
fn base64(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(u32::from(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    }
    let text = text.as_bytes();
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for chunk in text.chunks(4) {
        let pad = chunk.iter().rev().take_while(|c| **c == b'=').count();
        if pad > 2 {
            return None;
        }
        let mut word = 0u32;
        for &c in &chunk[..4 - pad] {
            word = (word << 6) | value(c)?;
        }
        word <<= 6 * pad;
        let bytes = word.to_be_bytes();
        out.extend_from_slice(&bytes[1..4 - pad]);
    }
    Some(out)
}

/// HMAC-SHA-256.
fn mac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut ctx = hmac::Context::with_key(&key);
    for part in parts {
        ctx.update(part);
    }
    let tag = ctx.sign();
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

/// Noise's HKDF with two or three outputs.
fn hkdf(chaining_key: &[u8; 32], input: &[u8]) -> ([u8; 32], [u8; 32], [u8; 32]) {
    let temp = mac(chaining_key, &[input]);
    let one = mac(&temp, &[&[1]]);
    let two = mac(&temp, &[&one, &[2]]);
    let three = mac(&temp, &[&two, &[3]]);
    (one, two, three)
}

/// ChaCha20-Poly1305 nonce: 32 bits of zeros, then the counter little-endian.
fn nonce(n: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[4..].copy_from_slice(&n.to_le_bytes());
    Nonce::assume_unique_for_key(bytes)
}

fn cipher(key: &[u8; 32]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("a 32-byte key"))
}

/// One direction of an established session: its key and counter.
pub struct CipherState {
    key: LessSafeKey,
    n: u64,
}

impl std::fmt::Debug for CipherState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CipherState")
            .field("n", &self.n)
            .finish_non_exhaustive()
    }
}

impl CipherState {
    fn new(key: &[u8; 32]) -> Self {
        Self {
            key: cipher(key),
            n: 0,
        }
    }

    /// `plain` sealed with an empty associated data (ESPHome's frames).
    pub fn seal(&mut self, plain: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut out = plain.to_vec();
        let n = self.next()?;
        self.key
            .seal_in_place_append_tag(nonce(n), Aad::empty(), &mut out)
            .map_err(|_| anyhow::anyhow!("noise: cannot seal"))?;
        Ok(out)
    }

    /// A sealed frame opened; an error if it was tampered with or out of order.
    pub fn open(&mut self, sealed: &[u8]) -> anyhow::Result<Vec<u8>> {
        if sealed.len() < TAG_LEN {
            bail!("noise: frame shorter than its tag");
        }
        let mut buf = sealed.to_vec();
        let n = self.next()?;
        let plain = self
            .key
            .open_in_place(nonce(n), Aad::empty(), &mut buf)
            .map_err(|_| anyhow::anyhow!("noise: frame not authentic"))?
            .len();
        buf.truncate(plain);
        Ok(buf)
    }

    fn next(&mut self) -> anyhow::Result<u64> {
        let n = self.n;
        // 2^64 - 1 is reserved by Noise: a connection never gets near it.
        self.n = n
            .checked_add(1)
            .filter(|n| *n != u64::MAX)
            .context("noise: counter exhausted")?;
        Ok(n)
    }
}

/// The handshake's symmetric state: chaining key, hash, and the key once set.
struct Symmetric {
    ck: [u8; 32],
    h: [u8; 32],
    k: Option<[u8; 32]>,
    n: u64,
}

impl Symmetric {
    fn new(prologue: &[u8]) -> Self {
        // A protocol name longer than 32 bytes is hashed.
        let mut h = [0u8; 32];
        h.copy_from_slice(digest::digest(&SHA256, PROTOCOL).as_ref());
        let mut state = Self {
            ck: h,
            h,
            k: None,
            n: 0,
        };
        state.mix_hash(prologue);
        state
    }

    fn mix_hash(&mut self, data: &[u8]) {
        let mut ctx = digest::Context::new(&SHA256);
        ctx.update(&self.h);
        ctx.update(data);
        self.h.copy_from_slice(ctx.finish().as_ref());
    }

    fn mix_key(&mut self, input: &[u8]) {
        let (ck, k, _) = hkdf(&self.ck, input);
        self.ck = ck;
        self.k = Some(k);
        self.n = 0;
    }

    fn mix_key_and_hash(&mut self, input: &[u8]) {
        let (ck, temp_h, k) = hkdf(&self.ck, input);
        self.ck = ck;
        self.mix_hash(&temp_h);
        self.k = Some(k);
        self.n = 0;
    }

    fn encrypt_and_hash(&mut self, plain: &[u8]) -> anyhow::Result<Vec<u8>> {
        let key = self.k.context("noise: no key yet")?;
        let mut out = plain.to_vec();
        cipher(&key)
            .seal_in_place_append_tag(nonce(self.n), Aad::from(self.h), &mut out)
            .map_err(|_| anyhow::anyhow!("noise: cannot seal"))?;
        self.n += 1;
        self.mix_hash(&out);
        Ok(out)
    }

    fn decrypt_and_hash(&mut self, sealed: &[u8]) -> anyhow::Result<Vec<u8>> {
        let key = self.k.context("noise: no key yet")?;
        let mut buf = sealed.to_vec();
        let plain = cipher(&key)
            .open_in_place(nonce(self.n), Aad::from(self.h), &mut buf)
            .map_err(|_| anyhow::anyhow!("noise: handshake not authentic (wrong API key?)"))?
            .len();
        self.n += 1;
        self.mix_hash(sealed);
        buf.truncate(plain);
        Ok(buf)
    }

    /// The two transport keys: the initiator sends with the first.
    fn split(&self) -> (CipherState, CipherState) {
        let (one, two, _) = hkdf(&self.ck, &[]);
        (CipherState::new(&one), CipherState::new(&two))
    }
}

/// The initiator's side, between its first message and the responder's.
pub struct Initiator {
    state: Symmetric,
    e: EphemeralPrivateKey,
}

impl std::fmt::Debug for Initiator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Initiator").finish_non_exhaustive()
    }
}

/// An established session: what Moli sends, what it receives.
#[derive(Debug)]
pub struct Session {
    pub send: CipherState,
    pub receive: CipherState,
}

impl Initiator {
    /// `-> psk, e`: the first message (48 bytes) and the state to finish with.
    pub fn start(psk: &[u8; 32], prologue: &[u8]) -> anyhow::Result<(Self, Vec<u8>)> {
        let mut state = Symmetric::new(prologue);
        state.mix_key_and_hash(psk);
        let e = EphemeralPrivateKey::generate(&X25519, &SystemRandom::new())
            .map_err(|_| anyhow::anyhow!("noise: no ephemeral key"))?;
        let public = e
            .compute_public_key()
            .map_err(|_| anyhow::anyhow!("noise: no public key"))?;
        let mut message = public.as_ref().to_vec();
        state.mix_hash(public.as_ref());
        // In a PSK handshake, `e` also feeds the key.
        state.mix_key(public.as_ref());
        message.extend(state.encrypt_and_hash(&[])?);
        Ok((Self { state, e }, message))
    }

    /// `<- e, ee`: the responder's message read, the session keys derived.
    pub fn finish(self, message: &[u8]) -> anyhow::Result<Session> {
        let Self { mut state, e } = self;
        if message.len() < DH_LEN + TAG_LEN {
            bail!("noise: handshake answer of {} bytes", message.len());
        }
        let (re, rest) = message.split_at(DH_LEN);
        state.mix_hash(re);
        state.mix_key(re);
        let shared =
            agreement::agree_ephemeral(e, &UnparsedPublicKey::new(&X25519, re), <[u8]>::to_vec)
                .map_err(|_| anyhow::anyhow!("noise: bad ephemeral key from the device"))?;
        state.mix_key(&shared);
        state.decrypt_and_hash(rest)?;
        let (send, receive) = state.split();
        Ok(Session { send, receive })
    }
}

/// The responder, for tests: what an ESPHome device does.
#[cfg(test)]
pub(crate) fn respond(
    psk: &[u8; 32],
    prologue: &[u8],
    message: &[u8],
) -> anyhow::Result<(Vec<u8>, Session)> {
    let mut state = Symmetric::new(prologue);
    state.mix_key_and_hash(psk);
    let (ie, rest) = message.split_at(DH_LEN);
    state.mix_hash(ie);
    state.mix_key(ie);
    state.decrypt_and_hash(rest)?;
    let e = EphemeralPrivateKey::generate(&X25519, &SystemRandom::new()).unwrap();
    let public = e.compute_public_key().unwrap();
    let mut answer = public.as_ref().to_vec();
    state.mix_hash(public.as_ref());
    state.mix_key(public.as_ref());
    let shared =
        agreement::agree_ephemeral(e, &UnparsedPublicKey::new(&X25519, ie), <[u8]>::to_vec)
            .unwrap();
    state.mix_key(&shared);
    answer.extend(state.encrypt_and_hash(&[])?);
    let (one, two) = state.split();
    // The responder receives with the first key, sends with the second.
    Ok((
        answer,
        Session {
            send: two,
            receive: one,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_keys_are_read() {
        let key = psk_from_base64("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=").unwrap();
        assert_eq!(key[0], 0);
        assert_eq!(key[31], 31);
        assert!(psk_from_base64("AAEC").is_err(), "3 bytes");
        assert!(psk_from_base64("A!==").is_err());
        assert_eq!(base64("TW9saQ=="), Some(b"Moli".to_vec()));
        assert_eq!(base64("TW9saSE="), Some(b"Moli!".to_vec()));
        for bytes in [&b"Moli"[..], b"Moli!", b"Moli!!", b""] {
            assert_eq!(base64(&base64_encode(bytes)), Some(bytes.to_vec()));
        }
        assert_eq!(base64_encode(b"Moli"), "TW9saQ==");
        let key = new_key_base64().unwrap();
        assert_eq!(psk_from_base64(&key).unwrap().len(), 32);
        assert_ne!(key, new_key_base64().unwrap());
    }

    #[test]
    fn hkdf_matches_rfc5869_style_chaining() {
        // HKDF-Expand by hand against HMAC: outputs chain as Noise says.
        let ck = [7u8; 32];
        let (one, two, three) = hkdf(&ck, b"input");
        let temp = mac(&ck, &[b"input"]);
        assert_eq!(one, mac(&temp, &[&[1]]));
        assert_eq!(two, mac(&temp, &[&one, &[2]]));
        assert_eq!(three, mac(&temp, &[&two, &[3]]));
        assert_ne!(one, two);
    }

    #[test]
    fn the_handshake_agrees_and_frames_go_both_ways() {
        let psk = [42u8; 32];
        let (initiator, first) = Initiator::start(&psk, PROLOGUE).unwrap();
        assert_eq!(first.len(), MSG1_LEN);
        let (answer, mut device) = respond(&psk, PROLOGUE, &first).unwrap();
        assert_eq!(answer.len(), MSG1_LEN);
        let mut moli = initiator.finish(&answer).unwrap();
        let sealed = moli.send.seal(b"hello").unwrap();
        assert_eq!(device.receive.open(&sealed).unwrap(), b"hello");
        let back = device.send.seal(b"bonjour").unwrap();
        assert_eq!(moli.receive.open(&back).unwrap(), b"bonjour");
        // A replayed or reordered frame does not open.
        assert!(moli.receive.open(&back).is_err());
    }

    #[test]
    fn a_wrong_key_or_prologue_fails_the_handshake() {
        let (_, first) = Initiator::start(&[1u8; 32], PROLOGUE).unwrap();
        assert!(respond(&[2u8; 32], PROLOGUE, &first).is_err());
        assert!(respond(&[1u8; 32], b"another prologue", &first).is_err());
        // An answer that is not the device's: refused.
        let (initiator, _) = Initiator::start(&[1u8; 32], PROLOGUE).unwrap();
        assert!(initiator.finish(&[9u8; MSG1_LEN]).is_err());
    }
}
