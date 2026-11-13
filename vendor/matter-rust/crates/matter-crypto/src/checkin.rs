//! Matter Check-In message codec (Matter Core §4.18.2) — the payload an ICD
//! sends unsolicited to a registered client when it briefly wakes. Reuses the
//! crate's AES-128-CCM AEAD and `ring` HMAC-SHA256; never implements primitives.
//!
//! Wire: `nonce(13) ‖ ciphertext ‖ mic(16)`, where `nonce = HMAC-SHA256(key,
//! counter)[..13]` and `ciphertext‖mic = AES-CCM(key, nonce, counter ‖ app_data,
//! aad = ∅)` (`counter` little-endian). A single 16-byte registration key is
//! used for both the HMAC (nonce) and the AES-CCM (payload).

#![forbid(unsafe_code)]

use crate::aead;

/// Length of the ICD registration / Check-In symmetric key.
pub const CHECKIN_KEY_LEN: usize = 16;

const NONCE_LEN: usize = 13; // AEAD nonce length
const MIC_LEN: usize = 16;
const COUNTER_LEN: usize = 4;
const MIN_PAYLOAD: usize = NONCE_LEN + COUNTER_LEN + MIC_LEN; // 33

/// Errors decoding a Check-In message.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CheckinError {
    /// Payload shorter than the 33-byte minimum.
    #[error("check-in payload too short")]
    TooShort,
    /// AEAD authentication failed (wrong key or tampered ciphertext).
    #[error("check-in decryption/authentication failed")]
    AuthFailed,
    /// The counter-derived nonce did not match the payload nonce.
    #[error("check-in nonce mismatch")]
    NonceMismatch,
    /// The AES-CCM encode produced no output (unreachable for fixed-length keys).
    #[error("check-in encode failed")]
    EncodeFailed,
}

/// The 13-byte Check-In nonce = first 13 bytes of `HMAC-SHA256(key, counter)`
/// (with `counter` little-endian).
fn checkin_nonce(key: &[u8; CHECKIN_KEY_LEN], counter: u32) -> [u8; NONCE_LEN] {
    let hk = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    let tag = ring::hmac::sign(&hk, &counter.to_le_bytes());
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&tag.as_ref()[..NONCE_LEN]);
    nonce
}

/// Encode a Check-In message payload for `counter` + `app_data` under `key`.
///
/// # Errors
/// [`CheckinError::EncodeFailed`] only if the AES-CCM layer fails — impossible in
/// practice for a fixed-length key and spec-bounded sizes.
pub fn encode_checkin(
    key: &[u8; CHECKIN_KEY_LEN],
    counter: u32,
    app_data: &[u8],
) -> Result<Vec<u8>, CheckinError> {
    let nonce = checkin_nonce(key, counter);
    let mut plaintext = Vec::with_capacity(COUNTER_LEN + app_data.len());
    plaintext.extend_from_slice(&counter.to_le_bytes());
    plaintext.extend_from_slice(app_data);
    let ct_tag =
        aead::encrypt(key, &nonce, &[], &plaintext).map_err(|_| CheckinError::EncodeFailed)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct_tag.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct_tag);
    Ok(out)
}

/// Decode + verify a Check-In payload, returning `(counter, app_data)`.
///
/// # Errors
/// [`CheckinError`] on a short payload, failed AEAD authentication, or a nonce
/// that does not match the decrypted counter.
pub fn decode_checkin(
    key: &[u8; CHECKIN_KEY_LEN],
    payload: &[u8],
) -> Result<(u32, Vec<u8>), CheckinError> {
    if payload.len() < MIN_PAYLOAD {
        return Err(CheckinError::TooShort);
    }
    let (nonce_bytes, ct_tag) = payload.split_at(NONCE_LEN);
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(nonce_bytes);
    let mut plaintext =
        aead::decrypt(key, &nonce, &[], ct_tag).map_err(|_| CheckinError::AuthFailed)?;
    if plaintext.len() < COUNTER_LEN {
        return Err(CheckinError::TooShort);
    }
    let mut c = [0u8; COUNTER_LEN];
    c.copy_from_slice(&plaintext[..COUNTER_LEN]);
    let counter = u32::from_le_bytes(c);
    // Verify the nonce is the one the counter derives (chip does this).
    if checkin_nonce(key, counter) != nonce {
        return Err(CheckinError::NonceMismatch);
    }
    plaintext.drain(..COUNTER_LEN);
    Ok((counter, plaintext))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // chip CheckIn_Message_test_vectors.h vector1: key, counter 12, empty appData.
    const KEY1: &str = "d90e13180d00baadd20cf5ed4913d3ff";
    const PAYLOAD1: &str = "4580d2c6f1310dc4eb64f1f8e8bdc21fb5195d747dd2879b2b0d43ce5b1c565078";

    fn key16(hex: &str) -> [u8; 16] {
        let mut k = [0u8; 16];
        k.copy_from_slice(&unhex(hex));
        k
    }

    #[test]
    fn encode_matches_chip_vector1() {
        assert_eq!(
            encode_checkin(&key16(KEY1), 12, &[]).unwrap(),
            unhex(PAYLOAD1)
        );
    }

    #[test]
    fn decode_matches_chip_vector1() {
        let (counter, app) = decode_checkin(&key16(KEY1), &unhex(PAYLOAD1)).unwrap();
        assert_eq!(counter, 12);
        assert_eq!(app, b"");
    }

    #[test]
    fn decode_rejects_wrong_key() {
        assert!(matches!(
            decode_checkin(&[0u8; 16], &unhex(PAYLOAD1)),
            Err(CheckinError::AuthFailed)
        ));
    }

    #[test]
    fn decode_rejects_too_short() {
        assert!(matches!(
            decode_checkin(&key16(KEY1), &[0u8; 10]),
            Err(CheckinError::TooShort)
        ));
    }

    #[test]
    fn roundtrip_with_app_data() {
        let key = [0x11u8; 16];
        let payload = encode_checkin(&key, 0x0102_0304, b"This").unwrap();
        let (c, app) = decode_checkin(&key, &payload).unwrap();
        assert_eq!(c, 0x0102_0304);
        assert_eq!(app, b"This");
    }
}
