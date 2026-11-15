//! SIGMA-I math primitives for Matter CASE.
//!
//! Pure functions implementing the SIGMA-I cryptographic equations used
//! in Matter Core Spec §4.13 (CASE / SIGMA). No state is held here.
//! The state-bearing [`super::initiator`] and [`super::responder`] modules
//! call these helpers to compose each handshake step.
//!
//! # Primitives
//!
//! | Primitive | Crate | Notes |
//! |-----------|-------|-------|
//! | P-256 ephemeral keypair | `p256` | rejection-sampled from `ring::rand` |
//! | ECDH | `p256::ecdh` | 32-byte X-coordinate output |
//! | SHA-256 / HKDF-SHA256 | `ring` | mirror of `pase::kdf::hkdf_expand` |
//! | AES-128-CCM | `ccm + aes` | 13-byte nonce, 16-byte tag |
//! | constant-time compare | `subtle` | never `==` on tag bytes |
//!
//! # matter.js cross-reference
//!
//! All constants below were pinned against
//! `@matter/protocol/src/session/case/CaseMessages.ts` and
//! `@matter/general/src/crypto/Crypto.ts`:
//!
//! - `CRYPTO_ENCRYPT_ALGORITHM = "aes-128-ccm"` — confirmed AES-128-CCM.
//! - `CRYPTO_AEAD_NONCE_LENGTH_BYTES = 13` — 13-byte nonce.
//! - `CRYPTO_AEAD_MIC_LENGTH_BYTES = 16` — 16-byte tag.
//! - `KDFSR2_INFO = Bytes.fromString("Sigma2")` — Sigma2 encryption key.
//! - `KDFSR3_INFO = Bytes.fromString("Sigma3")` — Sigma3 encryption key.
//! - `KDFSR1_KEY_INFO = Bytes.fromString("Sigma1_Resume")`.
//! - `KDFSR2_KEY_INFO = Bytes.fromString("Sigma2_Resume")`.
//! - `TBE_DATA2_NONCE = Bytes.fromString("NCASE_Sigma2N")`.
//! - `TBE_DATA3_NONCE = Bytes.fromString("NCASE_Sigma3N")`.
//! - `RESUME1_MIC_NONCE = Bytes.fromString("NCASE_SigmaS1")`.
//! - `RESUME2_MIC_NONCE = Bytes.fromString("NCASE_SigmaS2")`.
//!
//! The transcript hash is `SHA-256(concatenation of message bytes)` — no
//! protocol-version prefix; the IPK-derived `operationalIdentityProtectionKey`
//! acts as the domain-separating context in every HKDF salt (CaseClient.ts
//! lines 153–159, 199–203, 220–223).

use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{NonZeroScalar, PublicKey as P256PublicKey, SecretKey};
use ring::digest::{Context, SHA256};
use ring::hkdf;
use ring::rand::SecureRandom;
use subtle::ConstantTimeEq;

use matter_cert::MatterCertificate;
use matter_codec::{Tag, TlvWriter};

use crate::aead;
use crate::error::{Error, Result};

// =============================================================================
// AEAD constants — re-exported from `crate::aead`
// =============================================================================
//
// Matter Core Spec §3.6:
// - `CRYPTO_SYMMETRIC_KEY_LENGTH_BYTES = 16` (AES-128) → `AEAD_KEY_LEN`
// - `CRYPTO_AEAD_NONCE_LENGTH_BYTES   = 13`           → `AEAD_NONCE_LEN`
// - `CRYPTO_AEAD_MIC_LENGTH_BYTES     = 16`           → `AEAD_TAG_LEN`
//
// The cipher itself lives in `crate::aead`; we re-export the size constants
// at crate-private visibility so existing call sites
// (`crate::case::sigma::AEAD_KEY_LEN`, etc.) keep compiling unchanged.

#[allow(unused_imports)] // `AEAD_TAG_LEN` is consumed by the local AEAD tests.
pub(crate) use crate::aead::{AEAD_KEY_LEN, AEAD_NONCE_LEN, AEAD_TAG_LEN};

// ---------------------------------------------------------------------------
// HKDF info labels — exact UTF-8 bytes as used by matter.js
// (from @matter/protocol/src/session/case/CaseMessages.ts)
// ---------------------------------------------------------------------------

/// HKDF info for deriving the Sigma2 `TBEData` encryption key.
///
/// `KDFSR2_INFO = Bytes.fromString("Sigma2")` in matter.js.
pub(crate) const HKDF_INFO_SIGMA2: &[u8] = b"Sigma2";

/// HKDF info for deriving the Sigma3 `TBEData` encryption key.
///
/// `KDFSR3_INFO = Bytes.fromString("Sigma3")` in matter.js.
pub(crate) const HKDF_INFO_SIGMA3: &[u8] = b"Sigma3";

/// HKDF info for deriving the `Sigma1_Resume` resumption key.
///
/// `KDFSR1_KEY_INFO = Bytes.fromString("Sigma1_Resume")` in matter.js.
#[allow(dead_code)] // M4.3: consumed by the CaseInitiator / CaseResponder resumption paths.
pub(crate) const HKDF_INFO_SIGMA1_RESUME: &[u8] = b"Sigma1_Resume";

/// HKDF info for deriving the `Sigma2_Resume` resumption key.
///
/// `KDFSR2_KEY_INFO = Bytes.fromString("Sigma2_Resume")` in matter.js.
#[allow(dead_code)] // M4.3: consumed by the CaseInitiator / CaseResponder resumption paths.
pub(crate) const HKDF_INFO_SIGMA2_RESUME: &[u8] = b"Sigma2_Resume";

/// HKDF info for deriving session keys on the resumption path.
///
/// Pinned from matter.js `NodeSession.ts`:
/// `SESSION_RESUMPTION_KEYS_INFO = Bytes.fromString("SessionResumptionKeys")`
///
/// Used instead of `"SessionKeys"` when `isResumption = true`.
#[allow(dead_code)] // M4.3: consumed by the CaseInitiator / CaseResponder resumption paths.
pub(crate) const HKDF_INFO_RESUMPTION_SESSION_KEYS: &[u8] = b"SessionResumptionKeys";

// ---------------------------------------------------------------------------
// AEAD nonces — exact UTF-8 bytes as used by matter.js.
// All nonces are exactly 13 bytes (CRYPTO_AEAD_NONCE_LENGTH_BYTES).
// ---------------------------------------------------------------------------

/// AEAD nonce for encrypting `TBEData` in Sigma2.
///
/// `TBE_DATA2_NONCE = Bytes.fromString("NCASE_Sigma2N")` — 13 bytes.
pub(crate) const NONCE_TBE_DATA2: &[u8; AEAD_NONCE_LEN] = b"NCASE_Sigma2N";

/// AEAD nonce for encrypting `TBEData` in Sigma3.
///
/// `TBE_DATA3_NONCE = Bytes.fromString("NCASE_Sigma3N")` — 13 bytes.
pub(crate) const NONCE_TBE_DATA3: &[u8; AEAD_NONCE_LEN] = b"NCASE_Sigma3N";

/// AEAD nonce for the `Sigma1_Resume` MIC (resumption MAC).
///
/// `RESUME1_MIC_NONCE = Bytes.fromString("NCASE_SigmaS1")` — 13 bytes.
#[allow(dead_code)] // M4.3: consumed by the CaseInitiator / CaseResponder resumption paths.
pub(crate) const NONCE_RESUME1_MIC: &[u8; AEAD_NONCE_LEN] = b"NCASE_SigmaS1";

/// AEAD nonce for the `Sigma2_Resume` MIC (resumption MAC).
///
/// `RESUME2_MIC_NONCE = Bytes.fromString("NCASE_SigmaS2")` — 13 bytes.
#[allow(dead_code)] // M4.3: consumed by the CaseInitiator / CaseResponder resumption paths.
pub(crate) const NONCE_RESUME2_MIC: &[u8; AEAD_NONCE_LEN] = b"NCASE_SigmaS2";

// =============================================================================
// Ephemeral keypair generation
// =============================================================================

/// Generate a fresh ephemeral P-256 keypair.
///
/// Returns `(secret_key, public_key_bytes_sec1_uncompressed)`.
///
/// The public key is 65 bytes in SEC1 uncompressed form (`0x04 || X || Y`).
///
/// # Errors
///
/// Returns [`Error::EphemeralKeyGenerationFailed`] if the RNG fails or if
/// 16 consecutive scalar samples are all zero or out of range (astronomically
/// unlikely in practice).
pub(crate) fn generate_ephemeral_keypair(rng: &dyn SecureRandom) -> Result<(SecretKey, [u8; 65])> {
    // P-256 scalars must be in [1, n-1]. We rejection-sample up to 16 times;
    // the probability of hitting zero is ~2^-256 per attempt.
    for _ in 0..16 {
        let mut bytes = [0u8; 32];
        rng.fill(&mut bytes)
            .map_err(|_| Error::EphemeralKeyGenerationFailed)?;

        // `NonZeroScalar::from_repr` returns a `CtOption<NonZeroScalar>`,
        // which is `Some` iff the bytes represent a value in [1, n-1].
        let scalar_opt = NonZeroScalar::from_repr(bytes.into());
        if let Some(scalar) = Option::<NonZeroScalar>::from(scalar_opt) {
            // `SecretKey::from(NonZeroScalar)` is the p256 0.14 spelling of
            // 0.13's `SecretKey::new(scalar.into())` — in 0.13 the `From` impl
            // *was* `SecretKey::new(scalar.into())`, so this is the identical
            // construction, not a re-derivation.
            let sk = SecretKey::from(scalar);
            let pk = sk.public_key();
            let encoded = pk.to_sec1_point(false); // false = uncompressed
            let mut pub_bytes = [0u8; 65];
            pub_bytes.copy_from_slice(encoded.as_bytes());
            return Ok((sk, pub_bytes));
        }
    }
    Err(Error::EphemeralKeyGenerationFailed)
}

// =============================================================================
// ECDH
// =============================================================================

/// Compute the P-256 ECDH shared secret, returning the 32-byte X-coordinate.
///
/// Matter uses the raw X-coordinate as the shared secret input to HKDF
/// (same as ANSI X9.63 without cofactor multiplication on P-256, which has
/// cofactor 1).
///
/// # Errors
///
/// Returns [`Error::InvalidParameter`] if `peer_pub_bytes` is not a valid
/// uncompressed P-256 public key.
pub(crate) fn ecdh_shared_secret(
    my_secret: &SecretKey,
    peer_pub_bytes: &[u8; 65],
) -> Result<[u8; 32]> {
    let peer_pub =
        P256PublicKey::from_sec1_bytes(peer_pub_bytes).map_err(|_| Error::InvalidParameter)?;
    // `diffie_hellman` performs scalar-multiplication and returns a
    // `SharedSecret` whose `raw_secret_bytes` is the big-endian X-coordinate.
    let shared = p256::ecdh::diffie_hellman(my_secret.to_nonzero_scalar(), peer_pub.as_affine());
    let bytes = shared.raw_secret_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes.as_ref());
    Ok(out)
}

// =============================================================================
// HKDF
// =============================================================================

/// HKDF-SHA256 with empty salt (`Extract`-then-`Expand`).
///
/// Mirrors `pase::kdf::hkdf_expand`. The caller passes a pseudo-random key
/// (`prk`, 32 bytes in all CASE call sites) and an info label; the function
/// fills `out` with the requested number of bytes.
///
/// Uses an empty salt for `Extract` because `prk` is already a well-seeded
/// pseudo-random key in all CASE call sites (it is itself the output of an
/// HKDF-Extract or ECDH step).
///
/// # Errors
///
/// Returns an error only if `out.len()` exceeds ring's HKDF output limit
/// (255 × hash-length = 8160 bytes for SHA-256), which is impossible for
/// any Matter message length.
#[allow(dead_code)] // M4.3: consumed by the resumption session-key derivation path.
pub(crate) fn hkdf_expand(prk: &[u8], info: &[u8], out: &mut [u8]) -> Result<()> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]);
    let prk_obj = salt.extract(prk);
    let info_arr = [info];
    let okm = prk_obj
        .expand(&info_arr, OutLen(out.len()))
        .map_err(|_| Error::EphemeralKeyGenerationFailed)?;
    okm.fill(out)
        .map_err(|_| Error::EphemeralKeyGenerationFailed)?;
    Ok(())
}

/// Full HKDF-SHA256 (`Extract(salt, ikm)` → `Expand(prk, info)`).
///
/// Mirrors matter.js's `crypto.createHkdfKey(secret, salt, info, length)`
/// which maps to the standard HKDF construction with a real salt:
/// - `Extract(salt, ikm)` → PRK
/// - `Expand(PRK, info, len)` → OKM
///
/// Used for all CASE key derivations where matter.js passes a compound salt
/// (`IPK || ...`):
/// - S2K: `createHkdfKey(sharedSecret, IPK||respRandom||respEphPub||H(σ1), "Sigma2", 16)`
/// - S3K: `createHkdfKey(sharedSecret, IPK||H(σ1||σ2), "Sigma3", 16)`
/// - Session keys: `createHkdfKey(sharedSecret, IPK||H(σ1||σ2||σ3), "SessionKeys", 48)`
///
/// # Errors
///
/// Returns [`Error::EphemeralKeyGenerationFailed`] if `out.len()` exceeds
/// ring's HKDF output limit (impossible for any Matter key size).
pub(crate) fn hkdf_derive(ikm: &[u8], salt: &[u8], info: &[u8], out: &mut [u8]) -> Result<()> {
    let salt_obj = hkdf::Salt::new(hkdf::HKDF_SHA256, salt);
    let prk = salt_obj.extract(ikm);
    let info_arr = [info];
    let okm = prk
        .expand(&info_arr, OutLen(out.len()))
        .map_err(|_| Error::EphemeralKeyGenerationFailed)?;
    okm.fill(out)
        .map_err(|_| Error::EphemeralKeyGenerationFailed)?;
    Ok(())
}

/// `KeyType` adapter so we can pass a runtime-determined output length to
/// `ring::hkdf::Prk::expand`.
struct OutLen(usize);

impl hkdf::KeyType for OutLen {
    fn len(&self) -> usize {
        self.0
    }
}

// =============================================================================
// Transcript hash
// =============================================================================

/// Compute a transcript hash: `SHA-256(msg[0] || msg[1] || ...)`.
///
/// Matter CASE uses this to bind each step's HKDF salt to all prior messages:
///
/// - Sigma2 salt includes `SHA-256(sigma1_bytes)`.
/// - Sigma3 salt includes `SHA-256(sigma1_bytes || sigma2_bytes)`.
/// - Session key salt includes `SHA-256(sigma1_bytes || sigma2_bytes || sigma3_bytes)`.
///
/// There is no protocol-version prefix; domain separation comes from the
/// IPK-derived `operationalIdentityProtectionKey` that also appears in every
/// salt (pinned from `CaseClient.ts`).
pub(crate) fn transcript_hash(messages: &[&[u8]]) -> [u8; 32] {
    // Incremental digest: feed each slice to one SHA-256 context instead of
    // concatenating into an intermediate buffer. The transcript is pure
    // concatenation (no length or version prefixes), so this is
    // byte-identical to hashing the joined buffer.
    let mut ctx = Context::new(&SHA256);
    for msg in messages {
        ctx.update(msg);
    }
    let d = ctx.finish();
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

// =============================================================================
// AEAD — AES-128-CCM (13-byte nonce, 16-byte tag)
// =============================================================================

/// Encrypt `plaintext` with AES-128-CCM, returning `ciphertext || tag`.
///
/// The output is `plaintext.len() + 16` bytes: the ciphertext followed by the
/// 16-byte authentication tag. This layout matches matter.js's
/// `crypto.encrypt(key, plaintext, nonce, aad?)`.
///
/// # Errors
///
/// Returns [`Error::EphemeralKeyGenerationFailed`] on cipher initialisation
/// failure (key length mismatch — impossible at compile time with fixed array).
/// Returns the same error on encryption failure (also not expected in practice).
pub(crate) fn aead_encrypt(
    key: &[u8; AEAD_KEY_LEN],
    nonce: &[u8; AEAD_NONCE_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    aead::encrypt(key, nonce, aad, plaintext)
}

/// Decrypt an AES-128-CCM ciphertext blob (`ciphertext || tag`).
///
/// The `ciphertext` slice must be at least 16 bytes (the tag). Returns the
/// original plaintext if the tag verifies; returns
/// [`Error::EncryptedBlobDecryptionFailed`] if decryption or tag verification
/// fails.
///
/// The `ccm` crate verifies the tag in constant time internally via `subtle`.
///
/// # Errors
///
/// Returns [`Error::EncryptedBlobDecryptionFailed`] on any authentication or
/// decryption failure.
pub(crate) fn aead_decrypt(
    key: &[u8; AEAD_KEY_LEN],
    nonce: &[u8; AEAD_NONCE_LEN],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    aead::decrypt(key, nonce, aad, ciphertext)
}

// =============================================================================
// Constant-time MAC verify
// =============================================================================

/// Constant-time 16-byte MAC comparison.
///
/// Used for Sigma resumption MAC tags (`RESUME1_MIC` / `RESUME2_MIC`).
/// MUST NOT use `==` because a timing side-channel on tag comparison can
/// allow an attacker to forge MACs byte-by-byte.
///
/// # Errors
///
/// Returns [`Error::ResumptionMacMismatch`] if the tags differ.
#[allow(dead_code)] // M4.3: consumed via verify_sigma1/sigma2_resume_mic.
pub(crate) fn verify_mac(expected: &[u8; 16], received: &[u8; 16]) -> Result<()> {
    if expected.ct_eq(received).unwrap_u8() == 1 {
        Ok(())
    } else {
        Err(Error::ResumptionMacMismatch)
    }
}

// =============================================================================
// Resumption math — Sigma1_Resume / Sigma2_Resume MICs + session keys
//
// These helpers implement the resumption-specific cryptographic equations
// pinned from matter.js CaseClient.js / CaseServer.js / NodeSession.js.
//
// IMPORTANT design note — how the MIC is computed:
//   matter.js: `crypto.encrypt(resumeKey, new Uint8Array(0), RESUME1_MIC_NONCE)`
//   This is AES-128-CCM(key, plaintext=[], nonce=..., aad=[]) → 16-byte tag only.
//   There is no additional AAD. The MAC is purely over the empty plaintext.
//   The domain separation is achieved by:
//     (a) using a different HKDF salt for each direction's key, and
//     (b) using different nonces for Sigma1 vs Sigma2.
//
// Key derivation salt layout (pinned from matter.js CaseClient.js):
//   sigma1_resume_key_salt = initiatorRandom || resumptionId
//   sigma2_resume_key_salt = initiatorRandom || newResumptionId   (responder's fresh ID)
//
// Session key derivation for resumed sessions (NodeSession.js):
//   salt = initiatorRandom || resumptionId   (the OLD id, from the record)
//   info = "SessionResumptionKeys"            (not "SessionKeys")
//   len  = 48 bytes
//   layout: [0..16] = i2r_key, [16..32] = r2i_key, [32..48] = attestation_challenge
// =============================================================================

/// Derive the resumption key used to compute or verify `sigma1_resume_mic`.
///
/// matter.js (CaseClient.js): `crypto.createHkdfKey(sharedSecret,
///   Bytes.concat(initiatorRandom, resumptionId), KDFSR1_KEY_INFO)`
///
/// # Errors
///
/// Returns [`Error::EphemeralKeyGenerationFailed`] if HKDF fails (only
/// possible if output length exceeds ring's limit — impossible here).
#[allow(dead_code)]
fn derive_sigma1_resume_key(
    shared_secret: &[u8; 32],
    initiator_random: &[u8; 32],
    resumption_id: &[u8; 16],
) -> Result<[u8; AEAD_KEY_LEN]> {
    // salt = initiatorRandom || resumptionId (48 bytes total).
    let mut salt = [0u8; 48];
    salt[..32].copy_from_slice(initiator_random);
    salt[32..].copy_from_slice(resumption_id);

    let mut key = [0u8; AEAD_KEY_LEN];
    hkdf_derive(shared_secret, &salt, HKDF_INFO_SIGMA1_RESUME, &mut key)?;
    Ok(key)
}

/// Derive the resumption key used to compute or verify `sigma2_resume_mic`.
///
/// matter.js (CaseServer.js `#resume`):
/// `crypto.createHkdfKey(sharedSecret,
///   Bytes.concat(cx.peerRandom, cx.localResumptionId), KDFSR2_KEY_INFO)`
///
/// Note: `new_resumption_id` is the **responder's** freshly-generated ID
/// that will replace the old record's ID on the next resumption.
///
/// # Errors
///
/// Returns [`Error::EphemeralKeyGenerationFailed`] if HKDF fails.
#[allow(dead_code)]
fn derive_sigma2_resume_key(
    shared_secret: &[u8; 32],
    initiator_random: &[u8; 32],
    new_resumption_id: &[u8; 16],
) -> Result<[u8; AEAD_KEY_LEN]> {
    // salt = initiatorRandom || newResumptionId (48 bytes total).
    let mut salt = [0u8; 48];
    salt[..32].copy_from_slice(initiator_random);
    salt[32..].copy_from_slice(new_resumption_id);

    let mut key = [0u8; AEAD_KEY_LEN];
    hkdf_derive(shared_secret, &salt, HKDF_INFO_SIGMA2_RESUME, &mut key)?;
    Ok(key)
}

/// Compute the 16-byte `sigma1_resume_mic` (called `initiatorResumeMic`
/// in matter.js).
///
/// This is the AES-128-CCM tag over an **empty plaintext** (no AAD):
/// `AES-128-CCM(key=S1RK, nonce="NCASE_SigmaS1", plaintext=[])`.
/// The tag is the only output — there is no ciphertext.
///
/// # Arguments
///
/// * `shared_secret` — 32-byte ECDH shared secret from the resumption record.
/// * `initiator_random` — 32-byte random from the Sigma1 message.
/// * `resumption_id` — 16-byte resumption ID from the resumption record.
///
/// # Errors
///
/// Returns [`Error::EphemeralKeyGenerationFailed`] on AES-CCM failure
/// (impossible in practice for valid key/nonce lengths).
#[allow(dead_code)] // M4.3: consumed by CaseInitiator::start (resumption path).
pub(crate) fn compute_sigma1_resume_mic(
    shared_secret: &[u8; 32],
    initiator_random: &[u8; 32],
    resumption_id: &[u8; 16],
) -> Result<[u8; 16]> {
    let key = derive_sigma1_resume_key(shared_secret, initiator_random, resumption_id)?;
    // Encrypt empty plaintext; the entire output is the 16-byte tag.
    let tag_bytes = aead_encrypt(&key, NONCE_RESUME1_MIC, &[], &[])?;
    // `aead_encrypt` of empty plaintext produces exactly 16 bytes (just the tag).
    let tag: [u8; 16] = tag_bytes
        .try_into()
        .map_err(|_| Error::EphemeralKeyGenerationFailed)?;
    Ok(tag)
}

/// Verify a received `sigma1_resume_mic` in constant time.
///
/// Recomputes the expected MIC from `shared_secret`, `initiator_random`, and
/// `resumption_id`, then compares against `received_mic` via
/// [`verify_mac`] (constant-time).
///
/// # Errors
///
/// - [`Error::EphemeralKeyGenerationFailed`] if key derivation fails.
/// - [`Error::ResumptionMacMismatch`] if the MIC does not match.
#[allow(dead_code)] // M4.3: consumed by CaseResponder::handle_sigma1 (resumption path).
pub(crate) fn verify_sigma1_resume_mic(
    shared_secret: &[u8; 32],
    initiator_random: &[u8; 32],
    resumption_id: &[u8; 16],
    received_mic: &[u8; 16],
) -> Result<()> {
    let expected = compute_sigma1_resume_mic(shared_secret, initiator_random, resumption_id)?;
    verify_mac(&expected, received_mic)
}

/// Compute the 16-byte `sigma2_resume_mic` (called `resumeMic` in matter.js).
///
/// This is the AES-128-CCM tag over an **empty plaintext** (no AAD):
/// `AES-128-CCM(key=S2RK, nonce="NCASE_SigmaS2", plaintext=[])`.
///
/// # Arguments
///
/// * `shared_secret` — 32-byte ECDH shared secret from the resumption record.
/// * `initiator_random` — 32-byte random from the Sigma1 message.
/// * `new_resumption_id` — 16-byte **new** resumption ID generated by the
///   responder for this `Sigma2_Resume` message. This becomes the record ID
///   after the handshake completes.
///
/// # Errors
///
/// Returns [`Error::EphemeralKeyGenerationFailed`] on AES-CCM failure.
#[allow(dead_code)] // M4.3: consumed by CaseResponder::accept_resumption.
pub(crate) fn compute_sigma2_resume_mic(
    shared_secret: &[u8; 32],
    initiator_random: &[u8; 32],
    new_resumption_id: &[u8; 16],
) -> Result<[u8; 16]> {
    let key = derive_sigma2_resume_key(shared_secret, initiator_random, new_resumption_id)?;
    let tag_bytes = aead_encrypt(&key, NONCE_RESUME2_MIC, &[], &[])?;
    let tag: [u8; 16] = tag_bytes
        .try_into()
        .map_err(|_| Error::EphemeralKeyGenerationFailed)?;
    Ok(tag)
}

/// Verify a received `sigma2_resume_mic` in constant time.
///
/// Recomputes the expected MIC and compares against `received_mic` via
/// [`verify_mac`] (constant-time).
///
/// # Errors
///
/// - [`Error::EphemeralKeyGenerationFailed`] if key derivation fails.
/// - [`Error::ResumptionMacMismatch`] if the MIC does not match.
#[allow(dead_code)] // M4.3: consumed by CaseInitiator::handle_sigma2_resume.
pub(crate) fn verify_sigma2_resume_mic(
    shared_secret: &[u8; 32],
    initiator_random: &[u8; 32],
    new_resumption_id: &[u8; 16],
    received_mic: &[u8; 16],
) -> Result<()> {
    let expected = compute_sigma2_resume_mic(shared_secret, initiator_random, new_resumption_id)?;
    verify_mac(&expected, received_mic)
}

/// Derive the 48-byte session key material for a **resumed** CASE session.
///
/// Pinned from matter.js `NodeSession.create` (NodeSession.js):
/// ```text
/// const keys = crypto.createHkdfKey(
///     sharedSecret,
///     salt:  initiatorRandom || oldResumptionId,
///     info:  "SessionResumptionKeys",
///     len:   48,
/// )
/// // layout (same as the new-session path; chip CryptoContext::InitFromSecret):
/// // [0..16]  = i2r_key
/// // [16..32] = r2i_key
/// // [32..48] = attestation_challenge
/// ```
///
/// The `old_resumption_id` here is the ID that was in the resumption record
/// **before** this resumption (i.e., the one the initiator sent in Sigma1
/// tag 6). This matches how matter.js forms `secureSessionSalt`:
/// `Bytes.concat(cx.peerRandom, cx.peerResumptionId)`.
///
/// # Returns
///
/// 48-byte array. Slices:
/// - `[0..16]`  — i2r encryption key (initiator-to-responder)
/// - `[16..32]` — r2i encryption key (responder-to-initiator)
/// - `[32..48]` — attestation challenge
///
/// # Errors
///
/// Returns [`Error::EphemeralKeyGenerationFailed`] if HKDF fails (impossible
/// for valid input sizes).
#[allow(dead_code)] // M4.3: consumed by CaseInitiator and CaseResponder on the resumption path.
pub(crate) fn derive_resume_session_keys(
    shared_secret: &[u8; 32],
    initiator_random: &[u8; 32],
    old_resumption_id: &[u8; 16],
) -> Result<[u8; 48]> {
    // salt = initiatorRandom || oldResumptionId (48 bytes total).
    let mut salt = [0u8; 48];
    salt[..32].copy_from_slice(initiator_random);
    salt[32..].copy_from_slice(old_resumption_id);

    let mut out = [0u8; 48];
    hkdf_derive(
        shared_secret,
        &salt,
        HKDF_INFO_RESUMPTION_SESSION_KEYS,
        &mut out,
    )?;
    Ok(out)
}

// =============================================================================
// Shared SIGMA-I helpers used by both initiator and responder
// =============================================================================

/// Compute the `DestinationId` for Sigma1.
///
/// Matter Core Spec §4.13.2.4 and matter.js `Fabric.#generateSalt`:
/// ```text
/// salt = initiatorRandom(32) || rcacPublicKey(65) || fabricId_le8 || nodeId_le8
/// DestinationId = HMAC-SHA256(key = IPK, message = salt)
/// ```
///
/// Note the order of inputs matches matter.js exactly:
/// random → rootPublicKey → fabricId → nodeId.
///
/// Both the initiator (when building Sigma1) and the responder (when
/// cross-checking the `dest_id` in a received Sigma1) call this function.
pub(crate) fn compute_dest_id(
    ipk: &[u8; 16],
    rcac_public_key: &[u8; 65],
    fabric_id: u64,
    node_id: u64,
    initiator_random: &[u8; 32],
) -> [u8; 32] {
    use ring::hmac;
    let key = hmac::Key::new(hmac::HMAC_SHA256, ipk);
    // Capacity: 32 (random) + 65 (rootPubKey) + 8 (fabricId) + 8 (nodeId) = 113 bytes.
    let mut salt: Vec<u8> = Vec::with_capacity(113);
    salt.extend_from_slice(initiator_random);
    salt.extend_from_slice(rcac_public_key);
    salt.extend_from_slice(&fabric_id.to_le_bytes());
    salt.extend_from_slice(&node_id.to_le_bytes());
    let tag = hmac::sign(&key, &salt);
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

/// Encode a `TlvSignedData` structure for ECDSA signing/verification.
///
/// matter.js `TlvSignedData` (CaseMessages.ts):
/// ```ts
/// TlvSignedData = TlvObject({
///     1: responderNoc (bytes),
///     2: responderIcac (bytes, optional),
///     3: responderPublicKey (65 bytes),
///     4: initiatorPublicKey (65 bytes),
/// })
/// ```
///
/// This structure is used symmetrically:
/// - In Sigma2 verification: `responder_noc_tlv` = responder's NOC, keys are
///   responder and initiator ephemeral public keys respectively.
/// - In Sigma3 signing: `responder_noc_tlv` = initiator's NOC (because the
///   initiator plays the "responder" role in the `TlvSignedData` field names,
///   which were defined from the Sigma2 perspective).
///
/// # Errors
///
/// Propagates [`Error::Codec`] on TLV write failure.
pub(crate) fn encode_tbs_data(
    responder_noc_tlv: &[u8],
    responder_icac_tlv: Option<&[u8]>,
    responder_public_key: &[u8; 65],
    initiator_public_key: &[u8; 65],
) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)?;
    w.put_bytes(Tag::Context(1), responder_noc_tlv)?;
    if let Some(icac) = responder_icac_tlv {
        w.put_bytes(Tag::Context(2), icac)?;
    }
    w.put_bytes(Tag::Context(3), responder_public_key)?;
    w.put_bytes(Tag::Context(4), initiator_public_key)?;
    w.end_container()?;
    Ok(buf)
}

/// Encode the `TBEData2` plaintext (responder's NOC chain + signature).
///
/// Maps to `TlvEncryptedDataSigma2` in matter.js CaseMessages.ts:
/// ```ts
/// {
///     1: responderNoc (bytes),
///     2: responderIcac (bytes, optional),
///     3: signature (64 bytes),
///     4: resumptionId (16 bytes),
/// }
/// ```
///
/// Used by the *responder* when constructing Sigma2.
/// `resumption_id` is 16 zero bytes in M4.1 (resumption support lands in M4.2).
///
/// # Errors
///
/// Propagates [`Error::Codec`] on TLV write failure.
pub(crate) fn encode_tbedata2(
    noc_tlv: &[u8],
    icac_tlv: Option<&[u8]>,
    signature: &[u8; 64],
    resumption_id: &[u8; 16],
) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)?;
    w.put_bytes(Tag::Context(1), noc_tlv)?;
    if let Some(icac) = icac_tlv {
        w.put_bytes(Tag::Context(2), icac)?;
    }
    w.put_bytes(Tag::Context(3), signature)?;
    w.put_bytes(Tag::Context(4), resumption_id)?;
    w.end_container()?;
    Ok(buf)
}

/// Parsed plaintext of the `encrypted` field in Sigma2.
///
/// Maps to `TlvEncryptedDataSigma2` in matter.js CaseMessages.ts.
/// Used by the *initiator* when verifying a received Sigma2.
pub(crate) struct TbeData2 {
    /// Responder's Node Operational Certificate.
    pub peer_noc: MatterCertificate,
    /// Responder's Intermediate CA Certificate (optional).
    pub peer_icac: Option<MatterCertificate>,
    /// Raw NOC TLV bytes exactly as received. TBS signature verification
    /// must run over the peer's wire bytes — keeping them here removes both
    /// a per-handshake re-serialization and the (previously load-bearing)
    /// assumption that `MatterCertificate::to_tlv` re-encodes byte-identically.
    pub peer_noc_tlv: Vec<u8>,
    /// Raw ICAC TLV bytes exactly as received (optional, as the ICAC is).
    pub peer_icac_tlv: Option<Vec<u8>>,
    /// Raw 64-byte r||s ECDSA signature from the responder.
    pub peer_signature: Vec<u8>,
    /// 16-byte fresh resumption ID from the responder. Paired with the
    /// session's ECDH secret in the initiator's persisted
    /// `ResumptionRecord` (see `process_sigma2`).
    pub resumption_id: [u8; 16],
}

/// Decode the `TBEData2` plaintext (responder's NOC + signature inside Sigma2).
///
/// # Errors
///
/// - [`Error::InvalidParameter`] if a required field is absent,
///   the structure is malformed, or a byte-string has the wrong length.
/// - [`Error::Codec`] on TLV decode failure.
/// - [`Error::InvalidPeerNocChain`] if the NOC or ICAC TLV cannot be
///   parsed as a `MatterCertificate`.
pub(crate) fn decode_tbedata2(plaintext: &[u8]) -> Result<TbeData2> {
    use matter_codec::{ContainerKind, Element, Tag as MTag, TlvReader, Value};

    let mut reader = TlvReader::new(plaintext);
    // Outer anonymous structure.
    match reader.next()? {
        Some(Element::ContainerStart {
            tag: MTag::Anonymous,
            kind: ContainerKind::Structure,
        }) => {}
        _ => return Err(Error::InvalidParameter),
    }

    let mut noc_bytes: Option<Vec<u8>> = None;
    let mut icac_bytes: Option<Vec<u8>> = None;
    let mut signature: Option<Vec<u8>> = None;
    let mut resumption_id: Option<[u8; 16]> = None;

    loop {
        match reader.next()? {
            Some(Element::ContainerEnd) => break,

            // Tag 1: responderNoc
            Some(Element::Scalar {
                tag: MTag::Context(1),
                value: Value::Bytes(b),
            }) => {
                noc_bytes = Some(b);
            }

            // Tag 2: responderIcac (optional)
            Some(Element::Scalar {
                tag: MTag::Context(2),
                value: Value::Bytes(b),
            }) => {
                icac_bytes = Some(b);
            }

            // Tag 3: signature (64 bytes)
            Some(Element::Scalar {
                tag: MTag::Context(3),
                value: Value::Bytes(b),
            }) => {
                signature = Some(b);
            }

            // Tag 4: resumptionId (16 bytes)
            Some(Element::Scalar {
                tag: MTag::Context(4),
                value: Value::Bytes(b),
            }) => {
                let arr: [u8; 16] = b.try_into().map_err(|_| Error::InvalidParameter)?;
                resumption_id = Some(arr);
            }

            None | Some(_) => return Err(Error::InvalidParameter),
        }
    }

    let noc_b = noc_bytes.ok_or(Error::InvalidParameter)?;
    let sig = signature.ok_or(Error::InvalidParameter)?;
    let rid = resumption_id.ok_or(Error::InvalidParameter)?;

    let peer_noc = MatterCertificate::from_tlv(&noc_b).map_err(Error::InvalidPeerNocChain)?;
    let peer_icac = match &icac_bytes {
        Some(b) => Some(MatterCertificate::from_tlv(b).map_err(Error::InvalidPeerNocChain)?),
        None => None,
    };

    Ok(TbeData2 {
        peer_noc,
        peer_icac,
        peer_noc_tlv: noc_b,
        peer_icac_tlv: icac_bytes,
        peer_signature: sig,
        resumption_id: rid,
    })
}

/// Encode the `TBEData3` plaintext (sender's NOC chain + signature).
///
/// Maps to `TlvEncryptedDataSigma3` in matter.js CaseMessages.ts:
/// ```ts
/// {
///     1: responderNoc (bytes),   -- sender's NOC TLV
///     2: responderIcac (bytes, optional), -- sender's ICAC TLV (optional)
///     3: signature (64 bytes),
/// }
/// ```
///
/// Note: both initiator (in Sigma3) and responder (when parsing) use this
/// structure. The `responder` field names in the TLV come from the Sigma2
/// perspective; they are re-used symmetrically in Sigma3.
///
/// # Errors
///
/// Propagates [`Error::Codec`] on TLV write failure.
pub(crate) fn encode_tbedata3(
    noc_tlv: &[u8],
    icac_tlv: Option<&[u8]>,
    signature: &[u8; 64],
) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)?;
    w.put_bytes(Tag::Context(1), noc_tlv)?;
    if let Some(icac) = icac_tlv {
        w.put_bytes(Tag::Context(2), icac)?;
    }
    w.put_bytes(Tag::Context(3), signature)?;
    w.end_container()?;
    Ok(buf)
}

/// Parsed plaintext of the `encrypted` field in Sigma3.
///
/// Maps to `TlvEncryptedDataSigma3` in matter.js CaseMessages.ts.
/// Used by the *responder* when verifying a received Sigma3.
// `peer_` prefix is intentional: mirrors `TbeData2` naming convention for symmetry.
// All fields describe the peer (initiator) so the prefix is meaningful, not redundant.
#[allow(clippy::struct_field_names)]
pub(crate) struct TbeData3 {
    /// Initiator's Node Operational Certificate.
    pub peer_noc: MatterCertificate,
    /// Initiator's Intermediate CA Certificate (optional).
    pub peer_icac: Option<MatterCertificate>,
    /// Raw NOC TLV bytes exactly as received. TBS signature verification
    /// must run over the peer's wire bytes — keeping them here removes both
    /// a per-handshake re-serialization and the (previously load-bearing)
    /// assumption that `MatterCertificate::to_tlv` re-encodes byte-identically.
    pub peer_noc_tlv: Vec<u8>,
    /// Raw ICAC TLV bytes exactly as received (optional, as the ICAC is).
    pub peer_icac_tlv: Option<Vec<u8>>,
    /// Raw 64-byte r||s ECDSA signature from the initiator.
    pub peer_signature: Vec<u8>,
}

/// Decode the `TBEData3` plaintext (initiator's NOC + signature inside Sigma3).
///
/// # Errors
///
/// - [`Error::InvalidParameter`] if a required field is absent,
///   the structure is malformed, or a byte-string has the wrong length.
/// - [`Error::Codec`] on TLV decode failure.
/// - [`Error::InvalidPeerNocChain`] if the NOC or ICAC TLV cannot be
///   parsed as a `MatterCertificate`.
pub(crate) fn decode_tbedata3(plaintext: &[u8]) -> Result<TbeData3> {
    use matter_codec::{ContainerKind, Element, Tag as MTag, TlvReader, Value};

    let mut reader = TlvReader::new(plaintext);
    match reader.next()? {
        Some(Element::ContainerStart {
            tag: MTag::Anonymous,
            kind: ContainerKind::Structure,
        }) => {}
        _ => return Err(Error::InvalidParameter),
    }

    let mut noc_bytes: Option<Vec<u8>> = None;
    let mut icac_bytes: Option<Vec<u8>> = None;
    let mut signature: Option<Vec<u8>> = None;

    loop {
        match reader.next()? {
            Some(Element::ContainerEnd) => break,

            // Tag 1: initiatorNoc (labelled "responderNoc" in the TLV spec; re-used symmetrically)
            Some(Element::Scalar {
                tag: MTag::Context(1),
                value: Value::Bytes(b),
            }) => {
                noc_bytes = Some(b);
            }

            // Tag 2: initiatorIcac (optional)
            Some(Element::Scalar {
                tag: MTag::Context(2),
                value: Value::Bytes(b),
            }) => {
                icac_bytes = Some(b);
            }

            // Tag 3: signature (64 bytes)
            Some(Element::Scalar {
                tag: MTag::Context(3),
                value: Value::Bytes(b),
            }) => {
                signature = Some(b);
            }

            None | Some(_) => return Err(Error::InvalidParameter),
        }
    }

    let noc_b = noc_bytes.ok_or(Error::InvalidParameter)?;
    let sig = signature.ok_or(Error::InvalidParameter)?;

    let peer_noc = MatterCertificate::from_tlv(&noc_b).map_err(Error::InvalidPeerNocChain)?;
    let peer_icac = match &icac_bytes {
        Some(b) => Some(MatterCertificate::from_tlv(b).map_err(Error::InvalidPeerNocChain)?),
        None => None,
    };

    Ok(TbeData3 {
        peer_noc,
        peer_icac,
        peer_noc_tlv: noc_b,
        peer_icac_tlv: icac_bytes,
        peer_signature: sig,
    })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use ring::rand::SystemRandom;

    // ─── Nonce length assertions ─────────────────────────────────────────────

    /// Prove at test time that all nonce constants are exactly 13 bytes.
    /// A mismatch would also be a compile error because of the `[u8; 13]` type,
    /// but an explicit assertion makes the intent obvious in the test output.
    #[test]
    fn nonce_constants_are_13_bytes() {
        assert_eq!(NONCE_TBE_DATA2.len(), AEAD_NONCE_LEN);
        assert_eq!(NONCE_TBE_DATA3.len(), AEAD_NONCE_LEN);
        assert_eq!(NONCE_RESUME1_MIC.len(), AEAD_NONCE_LEN);
        assert_eq!(NONCE_RESUME2_MIC.len(), AEAD_NONCE_LEN);
    }

    // ─── generate_ephemeral_keypair ──────────────────────────────────────────

    #[test]
    fn ephemeral_keypair_generates_valid_p256_point() {
        let rng = SystemRandom::new();
        let (_sk, pub_bytes) = generate_ephemeral_keypair(&rng).unwrap();
        // SEC1 uncompressed public keys always start with 0x04.
        assert_eq!(pub_bytes[0], 0x04, "SEC1 uncompressed prefix must be 0x04");
        assert_eq!(
            pub_bytes.len(),
            65,
            "SEC1 uncompressed P-256 point is 65 bytes"
        );
    }

    // ─── ecdh_shared_secret ──────────────────────────────────────────────────

    #[test]
    fn ecdh_is_symmetric() {
        let rng = SystemRandom::new();
        let (sk_a, pub_a) = generate_ephemeral_keypair(&rng).unwrap();
        let (sk_b, pub_b) = generate_ephemeral_keypair(&rng).unwrap();
        let shared_a = ecdh_shared_secret(&sk_a, &pub_b).unwrap();
        let shared_b = ecdh_shared_secret(&sk_b, &pub_a).unwrap();
        assert_eq!(shared_a, shared_b, "ECDH shared secret must be symmetric");
    }

    // ─── hkdf_expand ────────────────────────────────────────────────────────

    #[test]
    fn hkdf_expand_deterministic_and_input_sensitive() {
        let prk = [0x42u8; 32];
        let mut out_a = [0u8; 16];
        let mut out_b = [0u8; 16];
        let mut out_c = [0u8; 16];
        hkdf_expand(&prk, HKDF_INFO_SIGMA2, &mut out_a).unwrap();
        hkdf_expand(&prk, HKDF_INFO_SIGMA2, &mut out_b).unwrap();
        hkdf_expand(&prk, HKDF_INFO_SIGMA3, &mut out_c).unwrap();
        // Same inputs → same output.
        assert_eq!(out_a, out_b, "HKDF must be deterministic");
        // Different info → different output.
        assert_ne!(
            out_a, out_c,
            "Different info labels must produce different keys"
        );
    }

    // ─── transcript_hash ────────────────────────────────────────────────────

    #[test]
    fn transcript_hash_deterministic_and_input_sensitive() {
        let h1 = transcript_hash(&[b"sigma1", b"sigma2"]);
        let h2 = transcript_hash(&[b"sigma1", b"sigma2"]);
        assert_eq!(h1, h2, "Transcript hash must be deterministic");
        let h3 = transcript_hash(&[b"sigma1", b"sigma3"]);
        assert_ne!(h1, h3, "Different messages must produce different hashes");
    }

    #[test]
    fn transcript_hash_single_message_matches_sha256() {
        // transcript_hash([msg]) must equal SHA-256(msg) with no prefix.
        let msg = b"test message for sigma";
        let ours = transcript_hash(&[msg.as_slice()]);
        let d = ring::digest::digest(&ring::digest::SHA256, msg);
        assert_eq!(
            ours,
            d.as_ref(),
            "transcript_hash of one message must equal SHA-256(message)"
        );
    }

    // ─── AEAD ───────────────────────────────────────────────────────────────

    #[test]
    fn aead_round_trip() {
        let key = [0x11u8; AEAD_KEY_LEN];
        let nonce = *NONCE_TBE_DATA2;
        let aad = b"associated data";
        let plaintext = b"the quick brown fox jumps over the lazy dog";

        let ciphertext = aead_encrypt(&key, &nonce, aad, plaintext).unwrap();
        // Output must be plaintext length + 16-byte tag.
        assert_eq!(ciphertext.len(), plaintext.len() + AEAD_TAG_LEN);
        let decrypted = aead_decrypt(&key, &nonce, aad, &ciphertext).unwrap();
        assert_eq!(
            decrypted, plaintext,
            "Decrypted output must match original plaintext"
        );
    }

    #[test]
    fn aead_tampered_ciphertext_rejected() {
        let key = [0x11u8; AEAD_KEY_LEN];
        let nonce = *NONCE_TBE_DATA3;
        let aad = b"";
        let mut ciphertext = aead_encrypt(&key, &nonce, aad, b"plaintext").unwrap();
        // Flip a bit in the ciphertext body (not just the tag).
        ciphertext[0] ^= 1;
        assert!(
            matches!(
                aead_decrypt(&key, &nonce, aad, &ciphertext),
                Err(Error::EncryptedBlobDecryptionFailed)
            ),
            "Tampered ciphertext must fail decryption"
        );
    }

    #[test]
    fn aead_tampered_tag_rejected() {
        let key = [0x11u8; AEAD_KEY_LEN];
        let nonce = *NONCE_TBE_DATA2;
        let aad = b"some aad";
        let mut ciphertext = aead_encrypt(&key, &nonce, aad, b"plaintext content").unwrap();
        // Flip a bit in the tag (last 16 bytes).
        let tag_start = ciphertext.len() - AEAD_TAG_LEN;
        ciphertext[tag_start] ^= 1;
        assert!(
            matches!(
                aead_decrypt(&key, &nonce, aad, &ciphertext),
                Err(Error::EncryptedBlobDecryptionFailed)
            ),
            "Tampered tag must fail decryption"
        );
    }

    // ─── verify_mac ─────────────────────────────────────────────────────────

    #[test]
    fn verify_mac_accepts_match() {
        let a = [0x11u8; 16];
        let b = [0x11u8; 16];
        verify_mac(&a, &b).unwrap();
    }

    #[test]
    fn verify_mac_rejects_mismatch() {
        let a = [0x11u8; 16];
        let mut b = a;
        b[0] ^= 1;
        assert!(
            matches!(verify_mac(&a, &b), Err(Error::ResumptionMacMismatch)),
            "Single-byte difference must fail MAC verify"
        );
    }

    // ─── Resumption MICs ────────────────────────────────────────────────────

    /// `compute_sigma1_resume_mic` + `verify_sigma1_resume_mic` with the same
    /// inputs must agree (round-trip).
    #[test]
    fn sigma1_resume_mic_round_trip() {
        let secret = [0x01u8; 32];
        let random = [0x02u8; 32];
        let rid = [0x03u8; 16];

        let mic = compute_sigma1_resume_mic(&secret, &random, &rid).unwrap();
        // verify must succeed with the same inputs.
        verify_sigma1_resume_mic(&secret, &random, &rid, &mic).unwrap();
    }

    /// Different `shared_secret` must produce a different MIC — verify rejects it.
    #[test]
    fn sigma1_resume_mic_rejects_wrong_secret() {
        let secret_a = [0x01u8; 32];
        let secret_b = [0xFFu8; 32]; // different secret
        let random = [0x02u8; 32];
        let rid = [0x03u8; 16];

        let mic = compute_sigma1_resume_mic(&secret_a, &random, &rid).unwrap();
        assert!(
            matches!(
                verify_sigma1_resume_mic(&secret_b, &random, &rid, &mic),
                Err(Error::ResumptionMacMismatch)
            ),
            "Wrong shared_secret must cause MIC mismatch"
        );
    }

    /// Different `resumption_id` must produce a different MIC.
    #[test]
    fn sigma1_resume_mic_rejects_wrong_resumption_id() {
        let secret = [0x01u8; 32];
        let random = [0x02u8; 32];
        let rid_a = [0x03u8; 16];
        let rid_b = [0x04u8; 16];

        let mic = compute_sigma1_resume_mic(&secret, &random, &rid_a).unwrap();
        assert!(
            matches!(
                verify_sigma1_resume_mic(&secret, &random, &rid_b, &mic),
                Err(Error::ResumptionMacMismatch)
            ),
            "Wrong resumption_id must cause MIC mismatch"
        );
    }

    /// `compute_sigma2_resume_mic` + `verify_sigma2_resume_mic` round-trip.
    #[test]
    fn sigma2_resume_mic_round_trip() {
        let secret = [0x10u8; 32];
        let random = [0x20u8; 32];
        let new_rid = [0x30u8; 16];

        let mic = compute_sigma2_resume_mic(&secret, &random, &new_rid).unwrap();
        verify_sigma2_resume_mic(&secret, &random, &new_rid, &mic).unwrap();
    }

    /// Different `shared_secret` on the sigma2 side must be rejected.
    #[test]
    fn sigma2_resume_mic_rejects_wrong_secret() {
        let secret_a = [0x10u8; 32];
        let secret_b = [0xABu8; 32];
        let random = [0x20u8; 32];
        let new_rid = [0x30u8; 16];

        let mic = compute_sigma2_resume_mic(&secret_a, &random, &new_rid).unwrap();
        assert!(
            matches!(
                verify_sigma2_resume_mic(&secret_b, &random, &new_rid, &mic),
                Err(Error::ResumptionMacMismatch)
            ),
            "Wrong shared_secret must cause Sigma2 MIC mismatch"
        );
    }

    /// Sigma1 and Sigma2 MICs for the same key inputs must differ
    /// (different nonces and info labels).
    #[test]
    fn sigma1_and_sigma2_resume_mics_differ() {
        let secret = [0x42u8; 32];
        let random = [0x43u8; 32];
        let rid = [0x44u8; 16];

        let mic1 = compute_sigma1_resume_mic(&secret, &random, &rid).unwrap();
        let mic2 = compute_sigma2_resume_mic(&secret, &random, &rid).unwrap();
        assert_ne!(
            mic1, mic2,
            "Sigma1 and Sigma2 resume MICs must differ (different keys and nonces)"
        );
    }

    // ─── Resumption session keys ─────────────────────────────────────────────

    /// `derive_resume_session_keys` is deterministic.
    #[test]
    fn derive_resume_session_keys_deterministic() {
        let secret = [0x55u8; 32];
        let random = [0x66u8; 32];
        let rid = [0x77u8; 16];

        let keys_a = derive_resume_session_keys(&secret, &random, &rid).unwrap();
        let keys_b = derive_resume_session_keys(&secret, &random, &rid).unwrap();
        assert_eq!(
            keys_a, keys_b,
            "Same inputs must produce identical key material"
        );
    }

    /// Different `initiator_random` must produce different keys.
    #[test]
    fn derive_resume_session_keys_input_sensitive() {
        let secret = [0x55u8; 32];
        let random_a = [0x66u8; 32];
        let mut random_b = random_a;
        random_b[0] ^= 1;
        let rid = [0x77u8; 16];

        let keys_a = derive_resume_session_keys(&secret, &random_a, &rid).unwrap();
        let keys_b = derive_resume_session_keys(&secret, &random_b, &rid).unwrap();
        assert_ne!(
            keys_a, keys_b,
            "Different initiator_random must produce different keys"
        );
    }

    /// The 48-byte output has three distinct non-overlapping 16-byte sections.
    #[test]
    fn derive_resume_session_keys_produces_48_bytes() {
        let secret = [0x88u8; 32];
        let random = [0x99u8; 32];
        let rid = [0xAAu8; 16];

        let keys = derive_resume_session_keys(&secret, &random, &rid).unwrap();
        assert_eq!(keys.len(), 48);
        // The three slices must all be different from each other (key material is
        // pseudo-random; collision probability is negligible for any real input).
        let r2i = &keys[0..16];
        let i2r = &keys[16..32];
        let att = &keys[32..48];
        assert_ne!(r2i, i2r, "r2i and i2r keys must differ");
        assert_ne!(r2i, att, "r2i and attestation keys must differ");
        assert_ne!(i2r, att, "i2r and attestation keys must differ");
    }
}
