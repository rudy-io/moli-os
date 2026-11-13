//! AES-128-CCM-128 (16-byte key, 13-byte nonce, 16-byte tag) AEAD helpers.
//!
//! Used internally by [`crate::case`] for SIGMA-I encrypted blobs and
//! externally by `matter-transport` for the Matter secured-message
//! framing layer (Matter Core Spec §4.5). The cipher itself comes from
//! the `aes` + `ccm` crates; this module is a thin, typed adapter that
//! matches matter.js's `crypto.encrypt`/`decrypt` byte layout (`ciphertext
//! || tag`).
//!
//! We never implement primitives here — only the type-safe wrapper.

use aes::Aes128;
use ccm::{
    aead::{Aead, AeadInOut, KeyInit, Payload},
    consts::{U13, U16},
    Ccm, Key, Nonce,
};

use crate::error::{Error, Result};

/// AES-128-CCM with a 16-byte tag and a 13-byte nonce — the Matter cipher.
type Aes128Ccm = Ccm<Aes128, U16, U13>;

/// AES-128 key length in bytes.
pub const AEAD_KEY_LEN: usize = 16;

/// AES-CCM nonce length in bytes (Matter uses 13-byte nonces).
pub const AEAD_NONCE_LEN: usize = 13;

/// AEAD authentication tag length in bytes.
pub const AEAD_TAG_LEN: usize = 16;

/// AES-128-CCM-128 encrypt: returns `ciphertext || tag` (so
/// `output.len() == plaintext.len() + AEAD_TAG_LEN`).
///
/// `aad` may be empty. Matches matter.js's `crypto.encrypt(key, plaintext,
/// nonce, aad?)` byte-for-byte.
///
/// This builds a fresh key schedule on every call. Prefer [`SessionAead`]
/// for any path that encrypts more than once per key (e.g. every outgoing
/// message on a session) to avoid repeating AES key expansion.
///
/// # Errors
///
/// Returns [`Error::EncryptionFailed`] on encryption failure (not
/// expected in practice for the spec-bounded message sizes).
pub fn encrypt(
    key: &[u8; AEAD_KEY_LEN],
    nonce: &[u8; AEAD_NONCE_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    SessionAead::new(key).encrypt(nonce, aad, plaintext)
}

/// AES-128-CCM-128 decrypt: input is `ciphertext || tag` (so
/// `ciphertext.len() >= AEAD_TAG_LEN`). Returns the plaintext if the tag
/// verifies.
///
/// `aad` may be empty. The `ccm` crate verifies the tag in constant time
/// internally via `subtle`.
///
/// This builds a fresh key schedule on every call. Prefer [`SessionAead`]
/// for any path that decrypts more than once per key (e.g. every inbound
/// message on a session) to avoid repeating AES key expansion.
///
/// # Errors
///
/// Returns [`Error::EncryptedBlobDecryptionFailed`] on any authentication
/// or decryption failure. The error is intentionally not specific —
/// distinguishing "wrong key" from "tampered ciphertext" is a spec-level
/// design choice that prevents oracle attacks.
pub fn decrypt(
    key: &[u8; AEAD_KEY_LEN],
    nonce: &[u8; AEAD_NONCE_LEN],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    SessionAead::new(key).decrypt(nonce, aad, ciphertext)
}

/// AES-128-CTR keystream application (encrypt == decrypt), using the CCM
/// counter-block convention for a 13-byte nonce — the construction chip's
/// `AES_CTR_crypt` uses for group-message **privacy** obfuscation (Matter
/// Core Spec §4.8.3).
///
/// Implemented as AES-CCM **encryption with empty AAD, discarding the tag**:
/// CCM's payload keystream IS CTR mode with counter blocks
/// `flags(L=2) || nonce || counter` starting at 1, which is byte-identical to
/// chip's CTR construction for the same nonce (verified end-to-end by the
/// full-frame privacy KAT in `matter-transport`, from connectedhomeip
/// `TestSessionManagerDispatch.cpp`). This stays composition — the cipher
/// itself remains the `aes`+`ccm` crates.
///
/// Applying the function twice with the same key + nonce returns the input
/// (XOR keystream), so one function serves obfuscation and de-obfuscation.
///
/// This builds a fresh key schedule on every call. Prefer [`SessionAead`]
/// for any path that applies the keystream more than once per key to avoid
/// repeating AES key expansion.
///
/// # Errors
///
/// Returns [`Error::EncryptionFailed`] if the underlying cipher fails (not
/// expected in practice for spec-bounded sizes).
pub fn ctr_apply(
    key: &[u8; AEAD_KEY_LEN],
    nonce: &[u8; AEAD_NONCE_LEN],
    data: &[u8],
) -> Result<Vec<u8>> {
    SessionAead::new(key).ctr_apply(nonce, data)
}

/// A session-scoped AES-128-CCM-128 cipher with the key schedule computed
/// once at construction.
///
/// The free functions [`encrypt`], [`decrypt`], and [`ctr_apply`] each
/// build a fresh `Aes128Ccm` cipher — including running AES-128 key
/// expansion — on every call. That is the right trade-off for a one-shot
/// use (a single CASE handshake blob, say), but it repeats the same
/// key-schedule computation on every packet for any path that
/// encrypts/decrypts more than once per key — most notably encrypting
/// every outgoing Matter message and decrypting every inbound one on a
/// live session. `SessionAead` runs AES-128 key expansion once, at
/// construction, and reuses the expanded schedule (via the `ccm`/`aes`
/// crates' own internal caching) for every subsequent call.
///
/// This is purely a performance optimisation: for identical
/// key/nonce/aad/input, every method here produces output byte-identical
/// to the corresponding free function. It composes the same `aes` +
/// `ccm` crates the free functions use — no cryptographic primitive is
/// reimplemented here.
///
/// Holds the expanded AES-128 key schedule for its lifetime; there is no
/// `Debug` derive because printing that schedule would leak key material
/// (see the manual [`Debug`] impl below).
///
/// **Secret hygiene:** the expanded key schedule is NOT zeroized on drop —
/// the `ccm`/`aes` types we compose do not implement `ZeroizeOnDrop`, and we
/// do not reimplement them. That is acceptable here because a `SessionAead`
/// is always constructed from key material that is itself already resident
/// unzeroized for the whole session (the session keys it is derived from),
/// so dropping the handle removes no guarantee the caller had. `SessionAead`
/// is therefore NOT a secret-erasure boundary: a caller that needs key
/// material scrubbed must scrub the source key bytes (see
/// [`crate::pase::PaseSessionKeys`], which is `ZeroizeOnDrop`) and drop every
/// derived handle, and must not treat this type as providing erasure.
pub struct SessionAead(Aes128Ccm);

impl SessionAead {
    /// Construct a cipher handle with the AES-128 key schedule computed
    /// once, from a fixed-length key.
    ///
    /// Infallible: unlike `Aes128Ccm::new_from_slice` (which the free
    /// functions used to call directly, and which validates a runtime
    /// slice length), a `&[u8; AEAD_KEY_LEN]` is always a valid key length
    /// by construction, so key initialisation cannot fail.
    pub fn new(key: &[u8; AEAD_KEY_LEN]) -> Self {
        let key_arr: Key<Aes128Ccm> = (*key).into();
        Self(Aes128Ccm::new(&key_arr))
    }

    /// AES-128-CCM-128 encrypt using the cached key schedule. See
    /// [`encrypt`] for the exact byte layout (`ciphertext || tag`) and
    /// matter.js compatibility notes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EncryptionFailed`] on encryption failure (not
    /// expected in practice for the spec-bounded message sizes).
    pub fn encrypt(
        &self,
        nonce: &[u8; AEAD_NONCE_LEN],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let nonce_arr: Nonce<U13> = (*nonce).into();
        self.0
            .encrypt(
                &nonce_arr,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| Error::EncryptionFailed)
    }

    /// AES-128-CCM-128 decrypt using the cached key schedule. See
    /// [`decrypt`] for the exact byte layout and error semantics.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EncryptedBlobDecryptionFailed`] on any
    /// authentication or decryption failure.
    pub fn decrypt(
        &self,
        nonce: &[u8; AEAD_NONCE_LEN],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>> {
        let nonce_arr: Nonce<U13> = (*nonce).into();
        self.0
            .decrypt(
                &nonce_arr,
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| Error::EncryptedBlobDecryptionFailed)
    }

    /// In-place seal: encrypts `buf` in place and appends the 16-byte tag
    /// (so `buf.len()` grows by [`AEAD_TAG_LEN`]), using the cached key
    /// schedule. Avoids the extra allocation-and-copy [`encrypt`] pays for
    /// callers that already own a mutable buffer to encrypt into (e.g. a
    /// pre-assembled outgoing packet).
    ///
    /// Produces the same bytes `encrypt(...)` would for the same
    /// key/nonce/aad/plaintext.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EncryptionFailed`] on encryption failure. On
    /// error, `buf`'s contents are unspecified — the underlying
    /// `ccm`/`aead` crates make no guarantee it is restored to its input
    /// state, so callers must not read `buf` after an error.
    pub fn encrypt_in_place(
        &self,
        nonce: &[u8; AEAD_NONCE_LEN],
        aad: &[u8],
        buf: &mut Vec<u8>,
    ) -> Result<()> {
        let nonce_arr: Nonce<U13> = (*nonce).into();
        self.0
            .encrypt_in_place(&nonce_arr, aad, buf)
            .map_err(|_| Error::EncryptionFailed)
    }

    /// In-place open: verifies and strips the 16-byte tag, truncating
    /// `buf` to the plaintext on success, using the cached key schedule.
    /// Avoids the extra allocation-and-copy [`decrypt`] pays for callers
    /// that already own the ciphertext in a mutable buffer (e.g. a
    /// received packet being decrypted in place).
    ///
    /// # Errors
    ///
    /// Returns [`Error::EncryptedBlobDecryptionFailed`] on any
    /// authentication or decryption failure. On error, `buf`'s contents
    /// are unspecified — the underlying `ccm`/`aead` crates make no
    /// guarantee it is restored to its input state, so callers must not
    /// read `buf` after an error.
    pub fn decrypt_in_place(
        &self,
        nonce: &[u8; AEAD_NONCE_LEN],
        aad: &[u8],
        buf: &mut Vec<u8>,
    ) -> Result<()> {
        let nonce_arr: Nonce<U13> = (*nonce).into();
        self.0
            .decrypt_in_place(&nonce_arr, aad, buf)
            .map_err(|_| Error::EncryptedBlobDecryptionFailed)
    }

    /// CTR keystream application (see [`ctr_apply`]) using the cached key
    /// schedule.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EncryptionFailed`] if the underlying cipher fails
    /// (not expected in practice for spec-bounded sizes).
    pub fn ctr_apply(&self, nonce: &[u8; AEAD_NONCE_LEN], data: &[u8]) -> Result<Vec<u8>> {
        let mut out = self.encrypt(nonce, &[], data)?;
        out.truncate(data.len()); // drop the CCM tag — only the keystream XOR remains
        Ok(out)
    }
}

impl core::fmt::Debug for SessionAead {
    /// Prints a fixed opaque placeholder — never the expanded key
    /// schedule. `Ccm<Aes128, U16, U13>` does not implement `Debug`
    /// itself, and even if it did, printing key material would be a
    /// hygiene bug (see [`crate::pase::PaseSessionKeys`]'s redacted
    /// `Debug` for the same discipline applied to raw key bytes).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SessionAead(<aes-128-ccm>)")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    #[test]
    fn ctr_apply_is_an_involution() {
        let key = [0x42u8; AEAD_KEY_LEN];
        let nonce = [0x17u8; AEAD_NONCE_LEN];
        let data = b"obfuscate me please";
        let once = ctr_apply(&key, &nonce, data).unwrap();
        assert_ne!(&once[..], &data[..]);
        let twice = ctr_apply(&key, &nonce, &once).unwrap();
        assert_eq!(&twice[..], &data[..]);
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = [0x42u8; AEAD_KEY_LEN];
        let nonce = [0x17u8; AEAD_NONCE_LEN];
        let aad = b"matter aad";
        let plaintext = b"the quick brown fox jumps over the lazy dog";

        let ciphertext = encrypt(&key, &nonce, aad, plaintext).unwrap();
        assert_eq!(ciphertext.len(), plaintext.len() + AEAD_TAG_LEN);

        let decrypted = decrypt(&key, &nonce, aad, &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn tampered_ciphertext_rejected() {
        let key = [0x42u8; AEAD_KEY_LEN];
        let nonce = [0x17u8; AEAD_NONCE_LEN];
        let mut ciphertext = encrypt(&key, &nonce, b"", b"payload").unwrap();
        ciphertext[0] ^= 1;
        assert!(decrypt(&key, &nonce, b"", &ciphertext).is_err());
    }

    #[test]
    fn wrong_key_rejected() {
        let key = [0x42u8; AEAD_KEY_LEN];
        let bad_key = [0x43u8; AEAD_KEY_LEN];
        let nonce = [0x17u8; AEAD_NONCE_LEN];
        let ciphertext = encrypt(&key, &nonce, b"", b"payload").unwrap();
        assert!(decrypt(&bad_key, &nonce, b"", &ciphertext).is_err());
    }

    #[test]
    fn wrong_aad_rejected() {
        let key = [0x42u8; AEAD_KEY_LEN];
        let nonce = [0x17u8; AEAD_NONCE_LEN];
        let ciphertext = encrypt(&key, &nonce, b"good aad", b"payload").unwrap();
        assert!(decrypt(&key, &nonce, b"bad aad", &ciphertext).is_err());
    }

    #[test]
    fn session_aead_matches_free_functions() {
        let key = [0x42u8; AEAD_KEY_LEN];
        let nonce = [0x17u8; AEAD_NONCE_LEN];
        let aad = b"matter aad";
        let plaintext = b"the quick brown fox jumps over the lazy dog";

        let handle = SessionAead::new(&key);

        let via_handle = handle.encrypt(&nonce, aad, plaintext).unwrap();
        let via_free_fn = encrypt(&key, &nonce, aad, plaintext).unwrap();
        assert_eq!(via_handle, via_free_fn);

        let decrypted_by_handle = handle.decrypt(&nonce, aad, &via_free_fn).unwrap();
        let decrypted_by_free_fn = decrypt(&key, &nonce, aad, &via_handle).unwrap();
        assert_eq!(decrypted_by_handle, plaintext);
        assert_eq!(decrypted_by_free_fn, plaintext);

        let keystream_by_handle = handle.ctr_apply(&nonce, plaintext).unwrap();
        let keystream_by_free_fn = ctr_apply(&key, &nonce, plaintext).unwrap();
        assert_eq!(keystream_by_handle, keystream_by_free_fn);
    }

    #[test]
    fn in_place_matches_vec_api() {
        let key = [0x42u8; AEAD_KEY_LEN];
        let nonce = [0x17u8; AEAD_NONCE_LEN];
        let aad = b"matter aad";
        let plaintext = b"the quick brown fox jumps over the lazy dog".to_vec();

        let session = SessionAead::new(&key);

        let expected_ct = session.encrypt(&nonce, aad, &plaintext).unwrap();

        let mut buf = plaintext.clone();
        session.encrypt_in_place(&nonce, aad, &mut buf).unwrap();
        assert_eq!(buf, expected_ct);

        session.decrypt_in_place(&nonce, aad, &mut buf).unwrap();
        assert_eq!(buf, plaintext);

        // Tampered buffer: decrypt_in_place errors. Buffer contents after
        // failure are unspecified (not asserted) — callers must not read
        // `buf` after a decryption error.
        let mut tampered = expected_ct.clone();
        tampered[0] ^= 1;
        assert!(session
            .decrypt_in_place(&nonce, aad, &mut tampered)
            .is_err());
    }

    /// A buffer too short to even hold the 16-byte tag must be rejected, not
    /// mis-read. `decrypt_in_place` is on the inbound path (attacker-supplied
    /// bytes), so this pins the `aead` crate's length guard: a truncated
    /// datagram returns `Err` rather than underflowing the tag split.
    #[test]
    fn decrypt_in_place_rejects_buffer_shorter_than_tag() {
        let key = [0x42u8; AEAD_KEY_LEN];
        let nonce = [0x17u8; AEAD_NONCE_LEN];
        let session = SessionAead::new(&key);

        let mut too_short = vec![0xAAu8; 4]; // < AEAD_TAG_LEN
        assert!(session
            .decrypt_in_place(&nonce, b"aad", &mut too_short)
            .is_err());

        // Boundary: exactly one byte short of a bare tag is still rejected.
        let mut one_short = vec![0xAAu8; AEAD_TAG_LEN - 1];
        assert!(session
            .decrypt_in_place(&nonce, b"aad", &mut one_short)
            .is_err());
    }

    /// Compile-time proof that a `SessionAead` can be cached inside a
    /// `Session` that is moved across threads / held in a `tokio` task —
    /// the whole point of caching it per session. Mirrors the static-assert
    /// pattern in `crate::pase`'s secret-hygiene tests.
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn session_aead_is_send_and_sync() {
        assert_send_sync::<SessionAead>();
    }

    #[test]
    fn session_aead_debug_is_opaque() {
        let key = [0x42u8; AEAD_KEY_LEN];
        let session = SessionAead::new(&key);
        assert_eq!(format!("{session:?}"), "SessionAead(<aes-128-ccm>)");
    }
}
