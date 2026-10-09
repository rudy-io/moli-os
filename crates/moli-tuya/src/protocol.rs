//! The Tuya local protocol (TCP 6668): framing, encryption, session keys.
//!
//! - 3.3: `0x55AA` frames with a CRC32; payloads AES-128-ECB with the
//!   device's local key; commands that change things carry a `3.3` header.
//! - 3.4: same frames, integrity by HMAC-SHA256; a session key is
//!   negotiated first (nonces exchanged, HMAC-checked).
//! - 3.5: `0x6699` frames, AES-128-GCM (header authenticated), same
//!   negotiation.
//!
//! Byte-for-byte checked against the output of existing implementations
//! (interoperability vectors in the tests); no code taken from them.

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt as _, BlockEncrypt as _, KeyInit as _};
use ring::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::hmac;
use ring::rand::{SecureRandom as _, SystemRandom};

pub const PREFIX_55AA: u32 = 0x0000_55AA;
pub const SUFFIX_55AA: u32 = 0x0000_AA55;
pub const PREFIX_6699: u32 = 0x0000_6699;
pub const SUFFIX_6699: u32 = 0x0000_9966;
/// Frames are small; a bigger length is a corrupt stream.
const MAX_LEN: usize = 4096;

pub mod cmd {
    pub const SESS_KEY_NEG_START: u32 = 3;
    pub const SESS_KEY_NEG_RESP: u32 = 4;
    pub const SESS_KEY_NEG_FINISH: u32 = 5;
    pub const CONTROL: u32 = 7;
    pub const STATUS: u32 = 8;
    pub const HEART_BEAT: u32 = 9;
    pub const DP_QUERY: u32 = 10;
    pub const CONTROL_NEW: u32 = 13;
    pub const DP_QUERY_NEW: u32 = 16;
    pub const UPDATEDPS: u32 = 18;
    pub const LAN_EXT_STREAM: u32 = 64;
}

/// Commands sent without the `3.x` version header.
fn bare(command: u32) -> bool {
    matches!(
        command,
        cmd::DP_QUERY
            | cmd::DP_QUERY_NEW
            | cmd::UPDATEDPS
            | cmd::HEART_BEAT
            | cmd::SESS_KEY_NEG_START
            | cmd::SESS_KEY_NEG_RESP
            | cmd::SESS_KEY_NEG_FINISH
            | cmd::LAN_EXT_STREAM
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Version {
    #[serde(rename = "3.3")]
    V33,
    #[serde(rename = "3.4")]
    V34,
    #[serde(rename = "3.5")]
    V35,
}

impl Version {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "3.3" | "3.2" => Some(Self::V33),
            "3.4" => Some(Self::V34),
            "3.5" => Some(Self::V35),
            _ => None,
        }
    }

    fn header(self) -> [u8; 15] {
        let mut h = [0u8; 15];
        h[..3].copy_from_slice(match self {
            Self::V33 => b"3.3",
            Self::V34 => b"3.4",
            Self::V35 => b"3.5",
        });
        h
    }

    /// Whether a session key must be negotiated after connecting.
    #[must_use]
    pub fn negotiates(self) -> bool {
        self != Self::V33
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("corrupt frame: {0}")]
    Corrupt(&'static str),
    #[error("integrity check failed (wrong local key?)")]
    Integrity,
    #[error("cannot decrypt (wrong local key?)")]
    Decrypt,
    #[error("session negotiation failed: {0}")]
    Session(&'static str),
}

/// One decoded frame: payload decrypted, return code stripped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub seqno: u32,
    pub cmd: u32,
    pub retcode: Option<u32>,
    pub payload: Vec<u8>,
}

// ---- primitives --------------------------------------------------------------

pub(crate) fn ecb_encrypt(key: &[u8; 16], data: &[u8], pad: bool) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut buf = data.to_vec();
    if pad {
        let n = 16 - buf.len() % 16;
        buf.extend(std::iter::repeat_n(u8::try_from(n).unwrap_or(16), n));
    }
    for block in buf.as_chunks_mut::<16>().0 {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    buf
}

pub(crate) fn ecb_decrypt(key: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return Err(ProtocolError::Decrypt);
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut buf = data.to_vec();
    for block in buf.as_chunks_mut::<16>().0 {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    let n = usize::from(*buf.last().ok_or(ProtocolError::Decrypt)?);
    if n == 0 || n > 16 || buf[buf.len() - n..].iter().any(|&b| usize::from(b) != n) {
        return Err(ProtocolError::Decrypt);
    }
    buf.truncate(buf.len() - n);
    Ok(buf)
}

fn gcm_key(key: &[u8; 16]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&AES_128_GCM, key).expect("16-byte AES key"))
}

/// Ciphertext followed by its 16-byte tag.
pub(crate) fn gcm_seal(key: &[u8; 16], iv: [u8; 12], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let mut buf = plain.to_vec();
    gcm_key(key)
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(iv), Aad::from(aad), &mut buf)
        .expect("AES-GCM seal");
    buf
}

pub(crate) fn gcm_open(
    key: &[u8; 16],
    iv: [u8; 12],
    aad: &[u8],
    sealed: &[u8],
) -> Result<Vec<u8>, ProtocolError> {
    let mut buf = sealed.to_vec();
    let plain = gcm_key(key)
        .open_in_place(Nonce::assume_unique_for_key(iv), Aad::from(aad), &mut buf)
        .map_err(|_| ProtocolError::Integrity)?;
    Ok(plain.to_vec())
}

pub(crate) fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data);
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    SystemRandom::new()
        .fill(&mut out)
        .expect("system randomness");
    out
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn len32(n: usize) -> [u8; 4] {
    u32::try_from(n).unwrap_or(u32::MAX).to_be_bytes()
}

// ---- codec -------------------------------------------------------------------

/// Encodes requests and decodes replies for one connection.
#[derive(Clone)]
pub struct Codec {
    pub version: Version,
    real_key: [u8; 16],
    /// The key in force: the local key, or the session key once negotiated.
    key: [u8; 16],
    seqno: u32,
    /// Fixed IV for tests; random otherwise.
    fixed_iv: Option<[u8; 12]>,
}

impl std::fmt::Debug for Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Keys never printed.
        f.debug_struct("Codec")
            .field("version", &self.version)
            .field("seqno", &self.seqno)
            .finish_non_exhaustive()
    }
}

impl Codec {
    #[must_use]
    pub fn new(version: Version, local_key: [u8; 16]) -> Self {
        Self {
            version,
            real_key: local_key,
            key: local_key,
            seqno: 1,
            fixed_iv: None,
        }
    }

    /// Back to the local key (a new connection negotiates again).
    pub fn reset(&mut self) {
        self.key = self.real_key;
    }

    /// Frames `payload` (JSON, or a nonce/HMAC during negotiation).
    pub fn encode(&mut self, command: u32, payload: &[u8]) -> Vec<u8> {
        let seqno = self.seqno;
        self.seqno = self.seqno.wrapping_add(1);
        let with_header = |v: Version| {
            let mut p = Vec::with_capacity(15 + payload.len());
            if !bare(command) {
                p.extend_from_slice(&v.header());
            }
            p.extend_from_slice(payload);
            p
        };
        match self.version {
            Version::V33 => {
                let encrypted = ecb_encrypt(&self.key, payload, true);
                let body = if bare(command) {
                    encrypted
                } else {
                    [&Version::V33.header()[..], &encrypted].concat()
                };
                frame_55aa(seqno, command, &body, None)
            }
            Version::V34 => {
                let body = ecb_encrypt(&self.key, &with_header(Version::V34), true);
                frame_55aa(seqno, command, &body, Some(&self.key))
            }
            Version::V35 => {
                let plain = with_header(Version::V35);
                let iv = self.fixed_iv.unwrap_or_else(random::<12>);
                frame_6699(seqno, command, &plain, &self.key, iv)
            }
        }
    }

    /// Takes one complete frame off the front of `buf`, if there is one.
    /// Garbage before a prefix is dropped.
    pub fn decode(&self, buf: &mut Vec<u8>) -> Result<Option<Frame>, ProtocolError> {
        let start = buf
            .windows(4)
            .position(|w| be32(w) == PREFIX_55AA || be32(w) == PREFIX_6699);
        match start {
            None => {
                // Keep a possible partial prefix.
                let keep = buf.len().min(3);
                buf.drain(..buf.len() - keep);
                return Ok(None);
            }
            Some(0) => {}
            Some(n) => {
                buf.drain(..n);
            }
        }
        if be32(buf) == PREFIX_55AA {
            self.decode_55aa(buf)
        } else {
            self.decode_6699(buf)
        }
    }

    fn decode_55aa(&self, buf: &mut Vec<u8>) -> Result<Option<Frame>, ProtocolError> {
        if buf.len() < 16 {
            return Ok(None);
        }
        let len = be32(&buf[12..16]) as usize;
        if len > MAX_LEN {
            buf.clear();
            return Err(ProtocolError::Corrupt("length"));
        }
        let total = 16 + len;
        if buf.len() < total {
            return Ok(None);
        }
        let frame: Vec<u8> = buf.drain(..total).collect();
        let end_len = if self.version == Version::V33 { 8 } else { 36 };
        if len < end_len + 4 || be32(&frame[total - 4..]) != SUFFIX_55AA {
            return Err(ProtocolError::Corrupt("suffix"));
        }
        let signed = &frame[..total - end_len];
        let check = &frame[total - end_len..total - 4];
        let good = if self.version == Version::V33 {
            crc32fast::hash(signed).to_be_bytes() == check
        } else {
            hmac_sha256(&self.key, signed) == check
        };
        if !good {
            return Err(ProtocolError::Integrity);
        }
        Ok(Some(Frame {
            seqno: be32(&frame[4..8]),
            cmd: be32(&frame[8..12]),
            retcode: Some(be32(&frame[16..20])),
            payload: frame[20..total - end_len].to_vec(),
        }))
    }

    fn decode_6699(&self, buf: &mut Vec<u8>) -> Result<Option<Frame>, ProtocolError> {
        if buf.len() < 18 {
            return Ok(None);
        }
        let len = be32(&buf[14..18]) as usize;
        if !(28..=MAX_LEN).contains(&len) {
            buf.clear();
            return Err(ProtocolError::Corrupt("length"));
        }
        let total = 18 + len + 4;
        if buf.len() < total {
            return Ok(None);
        }
        let frame: Vec<u8> = buf.drain(..total).collect();
        if be32(&frame[total - 4..]) != SUFFIX_6699 {
            return Err(ProtocolError::Corrupt("suffix"));
        }
        let mut iv = [0u8; 12];
        iv.copy_from_slice(&frame[18..30]);
        let plain = gcm_open(&self.key, iv, &frame[4..18], &frame[30..18 + len])?;
        // Device messages start with a small return code; a payload that
        // starts with JSON or a `3.x` header has none (tinytuya's rule).
        let (retcode, payload) = if plain.len() >= 4 && be32(&plain) & 0xFFFF_FF00 == 0 {
            (Some(be32(&plain)), plain[4..].to_vec())
        } else {
            (None, plain)
        };
        Ok(Some(Frame {
            seqno: be32(&frame[6..10]),
            cmd: be32(&frame[10..14]),
            retcode,
            payload,
        }))
    }

    /// The JSON a device sent (`None`: nothing to read, an ACK).
    pub fn open_json(&self, payload: &[u8]) -> Result<Option<serde_json::Value>, ProtocolError> {
        let header = self.version.header();
        let strip = |p: &[u8]| -> Vec<u8> {
            if p.starts_with(&header[..3]) && p.len() >= 15 {
                p[15..].to_vec()
            } else {
                p.to_vec()
            }
        };
        let plain = match self.version {
            _ if payload.is_empty() => return Ok(None),
            Version::V33 => {
                let body = strip(payload);
                if body.is_empty() {
                    return Ok(None);
                }
                ecb_decrypt(&self.key, &body)?
            }
            Version::V34 => strip(&ecb_decrypt(&self.key, payload)?),
            Version::V35 => strip(payload),
        };
        let text = String::from_utf8_lossy(&plain);
        let text = text.trim_end_matches('\0').trim();
        if text.is_empty() || !text.starts_with('{') {
            // « data unvalid », « json obj data unvalid »…: nothing usable.
            return Ok(None);
        }
        serde_json::from_str(text)
            .map(Some)
            .map_err(|_| ProtocolError::Corrupt("json"))
    }

    // ---- session key (3.4 / 3.5) ----

    /// Step 1: the frame opening the negotiation, and our nonce.
    pub fn session_start(&mut self) -> (Vec<u8>, [u8; 16]) {
        self.reset();
        let nonce = random::<16>();
        (self.encode(cmd::SESS_KEY_NEG_START, &nonce), nonce)
    }

    /// Step 2: checks the device's answer; returns the closing frame and
    /// switches to the session key.
    pub fn session_finish(
        &mut self,
        local: [u8; 16],
        answer: &Frame,
    ) -> Result<Vec<u8>, ProtocolError> {
        if answer.cmd != cmd::SESS_KEY_NEG_RESP {
            return Err(ProtocolError::Session("unexpected answer"));
        }
        let plain = match self.version {
            Version::V34 => ecb_decrypt(&self.real_key, &answer.payload)?,
            _ => answer.payload.clone(),
        };
        if plain.len() < 48 {
            return Err(ProtocolError::Session("short answer"));
        }
        let mut remote = [0u8; 16];
        remote.copy_from_slice(&plain[..16]);
        if hmac_sha256(&self.real_key, &local) != plain[16..48] {
            return Err(ProtocolError::Session(
                "device proof wrong (wrong local key?)",
            ));
        }
        let finish = self.encode(
            cmd::SESS_KEY_NEG_FINISH,
            &hmac_sha256(&self.real_key, &remote),
        );
        self.key = session_key(self.version, &self.real_key, &local, &remote);
        Ok(finish)
    }
}

fn session_key(version: Version, real: &[u8; 16], local: &[u8; 16], remote: &[u8; 16]) -> [u8; 16] {
    let mut x = [0u8; 16];
    for (i, b) in x.iter_mut().enumerate() {
        *b = local[i] ^ remote[i];
    }
    let derived = if version == Version::V34 {
        ecb_encrypt(real, &x, false)
    } else {
        let mut iv = [0u8; 12];
        iv.copy_from_slice(&local[..12]);
        gcm_seal(real, iv, &[], &x)
    };
    let mut key = [0u8; 16];
    key.copy_from_slice(&derived[..16]);
    key
}

fn frame_55aa(seqno: u32, command: u32, body: &[u8], hmac_key: Option<&[u8; 16]>) -> Vec<u8> {
    let end_len = if hmac_key.is_some() { 36 } else { 8 };
    let mut out = Vec::with_capacity(16 + body.len() + end_len);
    out.extend_from_slice(&PREFIX_55AA.to_be_bytes());
    out.extend_from_slice(&seqno.to_be_bytes());
    out.extend_from_slice(&command.to_be_bytes());
    out.extend_from_slice(&len32(body.len() + end_len));
    out.extend_from_slice(body);
    match hmac_key {
        Some(key) => out.extend_from_slice(&hmac_sha256(key, &out)),
        None => out.extend_from_slice(&crc32fast::hash(&out).to_be_bytes()),
    }
    out.extend_from_slice(&SUFFIX_55AA.to_be_bytes());
    out
}

fn frame_6699(seqno: u32, command: u32, plain: &[u8], key: &[u8; 16], iv: [u8; 12]) -> Vec<u8> {
    let mut out = Vec::with_capacity(18 + 12 + plain.len() + 16 + 4);
    out.extend_from_slice(&PREFIX_6699.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&seqno.to_be_bytes());
    out.extend_from_slice(&command.to_be_bytes());
    out.extend_from_slice(&len32(plain.len() + 12 + 16));
    let sealed = gcm_seal(key, iv, &out[4..18], plain);
    out.extend_from_slice(&iv);
    out.extend_from_slice(&sealed);
    out.extend_from_slice(&SUFFIX_6699.to_be_bytes());
    out
}

/// The key every Tuya device uses for its UDP broadcasts.
#[must_use]
pub fn udp_key() -> [u8; 16] {
    use md5::Digest as _;
    let mut key = [0u8; 16];
    key.copy_from_slice(&md5::Md5::digest(b"yGAdlopoPVldABfn"));
    key
}

/// A discovery broadcast (UDP 6666/6667/7000) → its JSON.
#[must_use]
pub fn open_broadcast(datagram: &[u8]) -> Option<serde_json::Value> {
    let key = udp_key();
    let plain = match be32(datagram.get(..4)?) {
        PREFIX_55AA => {
            let body = datagram.get(20..datagram.len().checked_sub(8)?)?;
            if datagram.get(8..12) == Some(&[0, 0, 0, 0]) {
                body.to_vec()
            } else {
                ecb_decrypt(&key, body).ok()?
            }
        }
        PREFIX_6699 => {
            let codec = Codec::new(Version::V35, key);
            let mut buf = datagram.to_vec();
            codec.decode(&mut buf).ok()??.payload
        }
        _ => ecb_decrypt(&key, datagram).ok()?,
    };
    let text = String::from_utf8_lossy(&plain);
    serde_json::from_str(text.trim_end_matches('\0')).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Interoperability vectors: frames produced by existing Python
    // implementations with the test key below, not a device's key.
    const KEY: [u8; 16] = *b"0123456789abcdef";
    const JSON: &[u8] = br#"{"dps":{"1":true}}"#;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn codec(version: Version, seqno: u32) -> Codec {
        let mut c = Codec::new(version, KEY);
        c.seqno = seqno;
        c
    }

    #[test]
    fn frames_match_the_reference_byte_for_byte() {
        assert_eq!(
            codec(Version::V33, 1).encode(cmd::CONTROL, JSON),
            hex(
                "000055aa000000010000000700000037332e33000000000000000000000000c27ee03f8be481e63320de7dc6eb429664215f365b0dad6b2ac8fab15b1d40bda72fbba70000aa55"
            )
        );
        assert_eq!(
            codec(Version::V33, 2).encode(cmd::DP_QUERY, JSON),
            hex(
                "000055aa000000020000000a00000028c27ee03f8be481e63320de7dc6eb429664215f365b0dad6b2ac8fab15b1d40bda2d021430000aa55"
            )
        );
        assert_eq!(
            codec(Version::V34, 3).encode(cmd::CONTROL_NEW, JSON),
            hex(
                "000055aa000000030000000d000000544490b05d74be9368c24a038cbaeded8e41fb8ef70a70a27b93a7e74ffe75426303000aeb4ff51d29188f82e4a162a1f6399f42b4a390ae5646b3257699a31b546ea88db0be9e7cba733bbca4d3a9da530000aa55"
            )
        );
        let mut v35 = codec(Version::V35, 4);
        v35.fixed_iv = Some(*b"abcdefghijkl");
        assert_eq!(
            v35.encode(cmd::CONTROL_NEW, JSON),
            hex(
                "000066990000000000040000000d0000003d6162636465666768696a6b6cd6fc49b553a559a55930b43af63edf7228719c941ac14870b5def70c20222dcc61faf7750e44f9794c58fb71b78c80de9200009966"
            )
        );
    }

    #[test]
    fn replies_decode_in_every_version() {
        // 3.5, as a device sends it: return code then JSON.
        let mut buf = hex(
            "0000669900000000000900000008000000327a797877767574737271706f37d3b4ac4e3f3038a5c2f0bbcb358898cab47333f5d8bc8a039e095804c8265d2dc3f985490000009966",
        );
        let c = codec(Version::V35, 1);
        let frame = c.decode(&mut buf).unwrap().unwrap();
        assert_eq!((frame.seqno, frame.cmd, frame.retcode), (9, 8, Some(0)));
        assert_eq!(
            c.open_json(&frame.payload).unwrap().unwrap()["dps"]["1"],
            true
        );
        assert!(buf.is_empty());

        // 3.3 and 3.4: what we encode, with a return code inserted, as a
        // device would answer.
        for version in [Version::V33, Version::V34] {
            let mut c = codec(version, 1);
            let mut sent = c.encode(cmd::CONTROL_NEW, JSON);
            sent.splice(16..16, [0, 0, 0, 0]);
            // Fix length and integrity after the insertion.
            let total = sent.len();
            let end_len = if version == Version::V33 { 8 } else { 36 };
            sent[12..16].copy_from_slice(&len32(total - 16));
            let check = if version == Version::V33 {
                crc32fast::hash(&sent[..total - end_len])
                    .to_be_bytes()
                    .to_vec()
            } else {
                hmac_sha256(&KEY, &sent[..total - end_len]).to_vec()
            };
            sent[total - end_len..total - 4].copy_from_slice(&check);
            let mut buf = [&b"garbage"[..], &sent].concat();
            let frame = c.decode(&mut buf).unwrap().unwrap();
            assert_eq!(frame.retcode, Some(0), "{version:?}");
            assert_eq!(
                c.open_json(&frame.payload).unwrap().unwrap()["dps"]["1"],
                true
            );
        }
    }

    #[test]
    fn a_push_without_return_code_keeps_its_header() {
        let mut c = codec(Version::V35, 1);
        c.fixed_iv = Some(*b"abcdefghijkl");
        // What a device pushes: `3.5` header + JSON, no return code.
        let mut buf = c.encode(cmd::CONTROL_NEW, JSON);
        let frame = c.decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame.retcode, None);
        assert_eq!(
            c.open_json(&frame.payload).unwrap().unwrap()["dps"]["1"],
            true
        );
    }

    #[test]
    fn partial_and_tampered_frames() {
        let mut c = codec(Version::V34, 1);
        let full = c.encode(cmd::HEART_BEAT, b"{}");
        let mut buf = full[..20].to_vec();
        assert_eq!(c.decode(&mut buf), Ok(None), "waits for the rest");
        let mut bad = full.clone();
        bad[20] ^= 1;
        assert_eq!(c.decode(&mut bad), Err(ProtocolError::Integrity));
        let mut wrong_key = full;
        assert!(
            Codec::new(Version::V34, *b"fedcba9876543210")
                .decode(&mut wrong_key)
                .is_err()
        );
    }

    #[test]
    fn session_keys_match_the_reference() {
        let (local, remote) = (*b"LOCALnonce012345", *b"REMOTEnonce67890");
        assert_eq!(
            session_key(Version::V34, &KEY, &local, &remote).to_vec(),
            hex("6b89b4dffa0d6edc91398dfc5ed4e4fb")
        );
        assert_eq!(
            session_key(Version::V35, &KEY, &local, &remote).to_vec(),
            hex("578a983bf02f158917d395cefd38c26c")
        );
        assert_eq!(
            hmac_sha256(&KEY, &local).to_vec(),
            hex("d24c1c376e0969e61e12949db29c0fb251982df307e364ed3b21e900ac72bb4e")
        );
    }

    #[test]
    fn negotiation_round_trip_against_a_simulated_device() {
        for version in [Version::V34, Version::V35] {
            let mut client = Codec::new(version, KEY);
            let (_, local) = client.session_start();
            // The device answers: its nonce + proof it knows the key.
            let remote = *b"REMOTEnonce67890";
            let answer = [&remote[..], &hmac_sha256(&KEY, &local)].concat();
            let payload = if version == Version::V34 {
                ecb_encrypt(&KEY, &answer, true)
            } else {
                answer
            };
            let frame = Frame {
                seqno: 1,
                cmd: cmd::SESS_KEY_NEG_RESP,
                retcode: Some(0),
                payload,
            };
            client.session_finish(local, &frame).unwrap();
            assert_eq!(client.key, session_key(version, &KEY, &local, &remote));
            // A forged proof is refused.
            let mut c = Codec::new(version, KEY);
            let (_, local) = c.session_start();
            let forged = Frame {
                seqno: 1,
                cmd: cmd::SESS_KEY_NEG_RESP,
                retcode: Some(0),
                payload: if version == Version::V34 {
                    ecb_encrypt(&KEY, &[0u8; 48], true)
                } else {
                    vec![0u8; 48]
                },
            };
            assert!(c.session_finish(local, &forged).is_err());
        }
    }

    #[test]
    fn broadcasts_open_with_the_well_known_key() {
        let json = br#"{"ip":"192.168.0.17","gwId":"bf00","version":"3.3"}"#;
        let body = ecb_encrypt(&udp_key(), json, true);
        let datagram = frame_55aa(0, 19, &[&[0u8, 0, 0, 0][..], &body].concat(), None);
        // A 55AA broadcast: header 16 + retcode 4, then the body.
        let found = open_broadcast(&datagram).unwrap();
        assert_eq!(found["version"], "3.3");
    }
}
