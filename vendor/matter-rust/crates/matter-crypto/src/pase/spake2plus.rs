//! SPAKE2+ math for Matter PASE.
//!
//! Pure functions implementing the SPAKE2+ protocol equations from
//! Matter Core Spec §3.10.2 over P-256, using:
//! - Matter-specific points M and N (spec §3.10.2, pinned from matter.js Spake2p.ts)
//! - p256 for scalar/point arithmetic
//! - ring for SHA-256, HMAC, HKDF
//! - subtle for constant-time comparison (`ConstantTimeEq`)
//!
//! # Transcript hash layout (matter.js cross-reference)
//!
//! `computeTranscriptHash` in `@matter/general/src/crypto/Spake2p.ts` uses
//! `addToContext(TTwriter, data)` which writes `uint64_LE(data.len) || data`
//! for each entry, in this order:
//! 1. context (SHA-256(SPAKE_CONTEXT || pbkdfReq || pbkdfResp) in protocol; raw bytes in math tests)
//! 2. pA = `""` (empty party identifier — Matter spec §3.10.5 sets both to empty)
//! 3. pB = `""` (empty party identifier)
//! 4. M (uncompressed SEC1, 65 bytes)
//! 5. N (uncompressed SEC1, 65 bytes)
//! 6. X (uncompressed SEC1, 65 bytes)
//! 7. Y (uncompressed SEC1, 65 bytes)
//! 8. Z (uncompressed SEC1, 65 bytes)
//! 9. V (uncompressed SEC1, 65 bytes)
//! 10. w0 (32-byte big-endian)
//!
//! # Key split (matter.js cross-reference)
//!
//! `computeSecretAndVerifiers` in Spake2p.ts:
//! - `TT_HASH = SHA-256(transcript)` — 32 bytes
//! - `Ka = TT_HASH[0..16]` (used as HKDF `PRK` for confirmation keys)
//! - `Ke = TT_HASH[16..32]` (session key material, returned to the protocol layer)
//! - `KcAB = HKDF-SHA256(Ka, salt=[], "ConfirmationKeys", 32)` → `KcA||KcB` (16 each)
//! - `hAY = HMAC-SHA256(KcA, Y)` (commissioner confirmation tag)
//! - `hBX = HMAC-SHA256(KcB, X)` (verifier confirmation tag)
//! - `Ke` is passed as `sharedSecret` to `NodeSession`, which derives per-session
//!   encryption/decryption/attestation keys via `HKDF-SHA256(Ke, salt=[], "SessionKeys", 48)`.
//!
//! No state. All functions take inputs and produce outputs. The state machine
//! in `prover.rs` / `verifier.rs` (M3.2) chains them.

// The dead_code allow was removed when M3.2 landed (prover.rs + verifier.rs).

use std::sync::LazyLock;

// Field for is_zero; PrimeField for from_repr.
use p256::elliptic_curve::group::ff::{Field, PrimeField};
// `FromSec1Point` / `ToSec1Point` are the p256 0.14 names for the traits 0.13
// called `FromEncodedPoint` / `ToEncodedPoint`, and `Sec1Point` is the 0.14
// name for the type 0.13 exported as `p256::EncodedPoint`. Same traits, same
// type, same SEC1 encoding — the 0.13 names survive only as deprecated
// forwarders.
use p256::elliptic_curve::sec1::{FromSec1Point, ToSec1Point};
use p256::{AffinePoint, ProjectivePoint, Scalar, Sec1Point};
use ring::digest::{digest, SHA256};
use ring::hmac;
use ring::rand::SecureRandom;
use subtle::ConstantTimeEq;

use crate::error::{Error, Result};
use crate::pase::kdf::hkdf_expand;

// =============================================================================
// Matter SPAKE2+ constants — pinned from matter.js Spake2p.ts / Matter spec §3.10.2
// =============================================================================

/// SPAKE2+ point M — SEC1 uncompressed, 65 bytes.
///
/// matter.js stores M as compressed `"02886e2f..."` and calls `.toBytes(false)`.
/// The 65-byte uncompressed form below is the canonical transcript encoding.
/// Derived by decompressing the spec §3.10.2 compressed point on P-256.
///
/// Hex: `04886e2f97ace46e55ba9dd7242579f2993b64e16ef3dcab95afd497333d8fa12f`
///       `5ff355163e43ce224e0b0e65ff02ac8e5c7be09419c785e0ca547d55a12e2d20`
#[rustfmt::skip]
const M_BYTES: [u8; 65] = [
    0x04, 0x88, 0x6e, 0x2f, 0x97, 0xac, 0xe4, 0x6e, 0x55, 0xba, 0x9d, 0xd7, 0x24, 0x25, 0x79, 0xf2,
    0x99, 0x3b, 0x64, 0xe1, 0x6e, 0xf3, 0xdc, 0xab, 0x95, 0xaf, 0xd4, 0x97, 0x33, 0x3d, 0x8f, 0xa1,
    0x2f, 0x5f, 0xf3, 0x55, 0x16, 0x3e, 0x43, 0xce, 0x22, 0x4e, 0x0b, 0x0e, 0x65, 0xff, 0x02, 0xac,
    0x8e, 0x5c, 0x7b, 0xe0, 0x94, 0x19, 0xc7, 0x85, 0xe0, 0xca, 0x54, 0x7d, 0x55, 0xa1, 0x2e, 0x2d,
    0x20,
];

/// SPAKE2+ point N — SEC1 uncompressed, 65 bytes.
///
/// matter.js stores N as compressed `"03d8bbd6..."` and calls `.toBytes(false)`.
///
/// Hex: `04d8bbd6c639c62937b04d997f38c3770719c629d7014d49a24b4f98baa1292b49`
///       `07d60aa6bfade45008a636337f5168c64d9bd36034808cd564490b1e656edbe7`
#[rustfmt::skip]
const N_BYTES: [u8; 65] = [
    0x04, 0xd8, 0xbb, 0xd6, 0xc6, 0x39, 0xc6, 0x29, 0x37, 0xb0, 0x4d, 0x99, 0x7f, 0x38, 0xc3, 0x77,
    0x07, 0x19, 0xc6, 0x29, 0xd7, 0x01, 0x4d, 0x49, 0xa2, 0x4b, 0x4f, 0x98, 0xba, 0xa1, 0x29, 0x2b,
    0x49, 0x07, 0xd6, 0x0a, 0xa6, 0xbf, 0xad, 0xe4, 0x50, 0x08, 0xa6, 0x36, 0x33, 0x7f, 0x51, 0x68,
    0xc6, 0x4d, 0x9b, 0xd3, 0x60, 0x34, 0x80, 0x8c, 0xd5, 0x64, 0x49, 0x0b, 0x1e, 0x65, 0x6e, 0xdb,
    0xe7,
];

/// SPAKE2+ M, decoded once — SEC1 point decompression costs a modular
/// square root, and M is needed on every `compute_x`/`compute_z_v_verifier`.
static M_POINT: LazyLock<ProjectivePoint> = LazyLock::new(|| point_from_spec_bytes(&M_BYTES));

/// SPAKE2+ N, decoded once (see [`M_POINT`]).
static N_POINT: LazyLock<ProjectivePoint> = LazyLock::new(|| point_from_spec_bytes(&N_BYTES));

/// HKDF info-string for confirmation keys (32 bytes → KcA||KcB, 16 each).
///
/// Pinned from matter.js Spake2p.ts: `Bytes.fromString("ConfirmationKeys")`.
const INFO_CONFIRMATION_KEYS: &[u8] = b"ConfirmationKeys";

/// HKDF info-string for session keys (48 bytes → decrypt||encrypt||attestation, 16 each).
///
/// Pinned from matter.js NodeSession.ts: `Bytes.fromString("SessionKeys")`.
const INFO_SESSION_KEYS: &[u8] = b"SessionKeys";

// =============================================================================
// Point decoding helpers
// =============================================================================

/// Decode a fixed spec-defined point. Only used on M and N — both are
/// known-valid P-256 points per the Matter spec. `expect` is sound here
/// because the bytes are compile-time constants that are correct by specification.
#[allow(clippy::expect_used)] // M and N are spec-fixed constants, not user input.
fn point_from_spec_bytes(bytes: &[u8; 65]) -> ProjectivePoint {
    let encoded = Sec1Point::from_bytes(bytes.as_slice())
        .expect("M/N bytes are valid SEC1 uncompressed encoding");
    let affine_opt: Option<AffinePoint> = AffinePoint::from_sec1_point(&encoded).into();
    affine_opt
        .map(ProjectivePoint::from)
        .expect("M/N bytes are a valid P-256 affine point")
}

/// Decode a peer-supplied point. Returns `Err(InvalidParameter)` if
/// the bytes are not a valid P-256 SEC1 uncompressed point.
fn decode_peer_point(bytes: &[u8; 65]) -> Result<ProjectivePoint> {
    let encoded = Sec1Point::from_bytes(bytes.as_slice()).map_err(|_| Error::InvalidParameter)?;
    let affine_opt: Option<AffinePoint> = AffinePoint::from_sec1_point(&encoded).into();
    affine_opt
        .map(ProjectivePoint::from)
        .ok_or(Error::InvalidParameter)
}

/// Encode a projective point as SEC1 uncompressed (65 bytes, prefix 0x04).
fn encode_point(p: &ProjectivePoint) -> [u8; 65] {
    let encoded = p.to_affine().to_sec1_point(false);
    let mut out = [0u8; 65];
    out.copy_from_slice(encoded.as_bytes());
    out
}

// =============================================================================
// Scalar sampling
// =============================================================================

/// Sample a fresh non-zero P-256 scalar from `rng`.
///
/// Per Matter Core Spec §3.10.2 the scalars `x` (commissioner) and `y`
/// (verifier) are uniformly random in [1, q-1]. We sample 32 bytes,
/// convert to a scalar (which inherently reduces mod q via
/// `Scalar::from_repr`), and reject if zero. Probability of a zero scalar
/// from a CSPRNG is ≈ 1/2^256, so looping 16 times is purely defensive.
pub(crate) fn sample_scalar(rng: &dyn SecureRandom) -> Result<Scalar> {
    for _ in 0..16 {
        let mut bytes = [0u8; 32];
        rng.fill(&mut bytes).map_err(|_| Error::InvalidScalar)?;
        // `Scalar::from_repr` returns `CtOption<Scalar>` — reduced mod q.
        // Convert via `Option::from` to keep constant-time behaviour.
        let scalar_opt: Option<Scalar> = Scalar::from_repr(bytes.into()).into();
        if let Some(s) = scalar_opt {
            // `is_zero` returns `Choice` (subtle crate); use `bool::from`.
            if !bool::from(s.is_zero()) {
                return Ok(s);
            }
        }
    }
    Err(Error::InvalidScalar)
}

// =============================================================================
// X / Y computation
// =============================================================================

/// Compute `X = x · P + w0 · M` — the commissioner's Pake1 payload.
///
/// `x` is the commissioner's random scalar; `w0` is derived from the PIN
/// via [`crate::pase::kdf::derive_w0_w1`]. Output is 65-byte SEC1 uncompressed.
pub(crate) fn compute_x(x: &Scalar, w0: &Scalar) -> [u8; 65] {
    let xp = ProjectivePoint::GENERATOR * x;
    let w0m = *M_POINT * w0;
    encode_point(&(xp + w0m))
}

/// Compute `Y = y · P + w0 · N` — the verifier's Pake2 payload.
///
/// `y` is the verifier's random scalar; `w0` is from the stored verifier
/// value. Output is 65-byte SEC1 uncompressed.
pub(crate) fn compute_y(y: &Scalar, w0: &Scalar) -> [u8; 65] {
    let yp = ProjectivePoint::GENERATOR * y;
    let w0n = *N_POINT * w0;
    encode_point(&(yp + w0n))
}

// =============================================================================
// Z / V computation
// =============================================================================

/// Commissioner-side derivation: `Z = x·(Y − w0·N)`, `V = w1·(Y − w0·N)`.
///
/// `y_bytes` is the 65-byte Pake2 payload received from the verifier.
/// Returns `Err(InvalidParameter)` if `y_bytes` is not a valid P-256 point.
pub(crate) fn compute_z_v_prover(
    x: &Scalar,
    w0: &Scalar,
    w1: &Scalar,
    y_bytes: &[u8; 65],
) -> Result<([u8; 65], [u8; 65])> {
    let y = decode_peer_point(y_bytes)?;
    // yn = Y - w0·N
    let w0n = *N_POINT * w0;
    let yn = y - w0n;
    let z = yn * x;
    let v = yn * w1;
    Ok((encode_point(&z), encode_point(&v)))
}

/// Verifier-side derivation: `Z = y·(X − w0·M)`, `V = y·L`.
///
/// `x_bytes` is the 65-byte Pake1 payload from the commissioner.
/// `l_bytes` is the pre-computed `L = w1·P` stored alongside w0 in the
/// verifier's PAKE verifier value (from [`crate::pase::kdf::derive_l`]).
/// Returns `Err(InvalidParameter)` if either point is invalid.
pub(crate) fn compute_z_v_verifier(
    y: &Scalar,
    w0: &Scalar,
    l_bytes: &[u8; 65],
    x_bytes: &[u8; 65],
) -> Result<([u8; 65], [u8; 65])> {
    let x_point = decode_peer_point(x_bytes)?;
    // x_minus_w0m = X - w0·M
    let w0m = *M_POINT * w0;
    let x_minus_w0m = x_point - w0m;
    let z_point = x_minus_w0m * y;
    let l_point = decode_peer_point(l_bytes)?;
    let v_point = l_point * y;
    Ok((encode_point(&z_point), encode_point(&v_point)))
}

// =============================================================================
// Transcript hash
// =============================================================================

/// Compute the SPAKE2+ transcript hash `TT_HASH = SHA-256(transcript)`.
///
/// The transcript is the concatenation of length-prefixed entries (each entry
/// is `uint64_LE(len) || data`), in the order defined by matter.js's
/// `computeTranscriptHash`:
///
/// ```text
/// context  (raw bytes; in real protocol: SHA-256(SPAKE_CONTEXT || pbkdfReq || pbkdfResp))
/// pA = ""  (empty party identifier, Matter spec §3.10.5)
/// pB = ""  (empty party identifier)
/// M        (65-byte SEC1 uncompressed)
/// N        (65-byte SEC1 uncompressed)
/// X        (65-byte SEC1 uncompressed)
/// Y        (65-byte SEC1 uncompressed)
/// Z        (65-byte SEC1 uncompressed)
/// V        (65-byte SEC1 uncompressed)
/// w0       (32-byte big-endian scalar)
/// ```
///
/// Returns 32 bytes. The caller splits them into `Ka` (first 16) and `Ke` (last 16)
/// via [`ka_ke_from_transcript`].
pub(crate) fn transcript_hash(
    context: &[u8],
    x_bytes: &[u8; 65],
    y_bytes: &[u8; 65],
    z_bytes: &[u8; 65],
    v_bytes: &[u8; 65],
    w0: &Scalar,
) -> [u8; 32] {
    // Pre-compute fixed entries.
    let m_bytes = &M_BYTES;
    let n_bytes = &N_BYTES;
    let w0_be = scalar_to_be_bytes(w0);
    let empty: &[u8] = b"";

    // Build the transcript by appending length-prefixed entries.
    // Capacity calculation (upper bound):
    //   9 entries of (8 + up to 65) bytes + context (variable)
    //   = 8 * (1 + 9) + context.len() + 2*0 + 4*65 + 65 + 65 + 32
    //   We over-allocate a fixed 1024 bytes; no heap pressure for small bufs.
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    append_length_prefixed(&mut buf, context);
    append_length_prefixed(&mut buf, empty); // pA
    append_length_prefixed(&mut buf, empty); // pB
    append_length_prefixed(&mut buf, m_bytes.as_slice());
    append_length_prefixed(&mut buf, n_bytes.as_slice());
    append_length_prefixed(&mut buf, x_bytes.as_slice());
    append_length_prefixed(&mut buf, y_bytes.as_slice());
    append_length_prefixed(&mut buf, z_bytes.as_slice());
    append_length_prefixed(&mut buf, v_bytes.as_slice());
    append_length_prefixed(&mut buf, w0_be.as_slice());

    let d = digest(&SHA256, &buf);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

/// Append `uint64_LE(data.len) || data` to `buf`.
///
/// Matches matter.js's `addToContext(TTwriter, data)` which calls
/// `TTwriter.writeUInt64(data.byteLength)` then `writeByteArray(data)`.
fn append_length_prefixed(buf: &mut Vec<u8>, data: &[u8]) {
    // Length as little-endian 64-bit integer.
    let len_u64 = data.len() as u64;
    buf.extend_from_slice(&len_u64.to_le_bytes());
    buf.extend_from_slice(data);
}

fn scalar_to_be_bytes(s: &Scalar) -> [u8; 32] {
    // p256::Scalar::to_bytes() returns FieldBytes (a 32-byte `Array`) in
    // big-endian format, matching matter.js's `numberToBytesBE(w0, 32)`.
    // `&fb` derefs to `&[u8]`, so no explicit `as_slice` is needed.
    let fb = s.to_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&fb);
    out
}

// =============================================================================
// Ka / Ke split
// =============================================================================

/// Split the 32-byte transcript hash into Ka (first 16 bytes) and Ke (last 16 bytes).
///
/// Pinned from matter.js `computeSecretAndVerifiers`:
/// ```text
/// const Ka = TT_HASH.slice(0, 16);
/// const Ke = TT_HASH.slice(16, 32);
/// ```
///
/// - `Ka` is used as the HKDF PRK for confirmation key derivation.
/// - `Ke` is the PASE session secret returned to the protocol layer; it is
///   passed as `sharedSecret` to `NodeSession`, which derives encrypt/decrypt/
///   attestation keys from it via HKDF `"SessionKeys"`.
pub(crate) fn ka_ke_from_transcript(t_t: &[u8; 32]) -> ([u8; 16], [u8; 16]) {
    let mut ka = [0u8; 16];
    let mut ke = [0u8; 16];
    ka.copy_from_slice(&t_t[..16]);
    ke.copy_from_slice(&t_t[16..]);
    (ka, ke)
}

// =============================================================================
// Confirmation keys + tags
// =============================================================================

/// Derive confirmation keys `KcA` and `KcB` from `Ka` (first 16 bytes of `TT_HASH`).
///
/// Pinned from matter.js:
/// ```text
/// const KcAB = HKDF(Ka, salt=[], "ConfirmationKeys", 32);
/// const KcA = KcAB.slice(0, 16);
/// const KcB = KcAB.slice(16, 32);
/// ```
// kca / kcb are the SPAKE2+ confirmation key names from Matter
// Core Spec §3.10 (RFC 9383 §3.3). Renaming would impair
// verification against the spec.
#[allow(clippy::similar_names)]
pub(crate) fn derive_confirmation_keys(ka: &[u8; 16]) -> Result<([u8; 16], [u8; 16])> {
    let mut out = [0u8; 32];
    hkdf_expand(ka, INFO_CONFIRMATION_KEYS, &mut out)?;
    let mut kca = [0u8; 16];
    let mut kcb = [0u8; 16];
    kca.copy_from_slice(&out[..16]);
    kcb.copy_from_slice(&out[16..]);
    Ok((kca, kcb))
}

/// Commissioner confirmation tag: `cA = HMAC-SHA256(KcA, Y)`.
///
/// Pinned from matter.js: `hAY = crypto.signHmac(KcA, Y)`.
pub(crate) fn compute_ca(kca: &[u8; 16], y_bytes: &[u8; 65]) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, kca);
    let tag = hmac::sign(&key, y_bytes.as_slice());
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

/// Verifier confirmation tag: `cB = HMAC-SHA256(KcB, X)`.
///
/// Pinned from matter.js: `hBX = crypto.signHmac(KcB, X)`.
pub(crate) fn compute_cb(kcb: &[u8; 16], x_bytes: &[u8; 65]) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, kcb);
    let tag = hmac::sign(&key, x_bytes.as_slice());
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

/// Constant-time confirmation tag verification.
///
/// MUST be used instead of `==` for tag bytes to prevent timing side-channels.
pub(crate) fn verify_tag(expected: &[u8; 32], received: &[u8; 32]) -> Result<()> {
    // CT-EQ: must NEVER use `==` on tag bytes — leaks timing info.
    // `subtle::ConstantTimeEq` is the idiomatic non-deprecated constant-time
    // byte comparison in the Rust ecosystem and is already a transitive
    // dependency via p256. `ct_eq` on `[u8]` returns `subtle::Choice`
    // (1 = equal, 0 = not equal).
    if expected.ct_eq(received).unwrap_u8() == 1 {
        Ok(())
    } else {
        Err(Error::ConfirmationTagMismatch)
    }
}

// =============================================================================
// Session keys derivation
// =============================================================================

/// Derive 48-byte session key material from `Ke` (the second 16 bytes of `TT_HASH`).
///
/// This matches what matter.js's `NodeSession.create` does on the `sharedSecret`
/// (which is `Ke`):
/// ```text
/// const keys = HKDF(sharedSecret=Ke, salt=[], "SessionKeys", 48);
/// const decryptKey  = initiator ? keys[16..32] : keys[0..16];
/// const encryptKey  = initiator ? keys[0..16]  : keys[16..32];
/// const attestKey   = keys[32..48];
/// ```
///
/// Returns the full 48 bytes. M3.2's `PaseProver/PaseVerifier::finish()` will
/// split them into the three 16-byte keys and pack them into `PaseSessionKeys`.
pub(crate) fn derive_session_keys(ke: &[u8; 16]) -> Result<[u8; 48]> {
    let mut out = [0u8; 48];
    hkdf_expand(ke, INFO_SESSION_KEYS, &mut out)?;
    Ok(out)
}

// =============================================================================
// Transcript context hash
// =============================================================================

/// SPAKE2+ context string — pinned from matter.js PaseMessenger.ts.
///
/// `export const SPAKE_CONTEXT = Bytes.fromString("CHIP PAKE V1 Commissioning");`
const SPAKE_CONTEXT: &[u8] = b"CHIP PAKE V1 Commissioning";

/// Compose the SPAKE2+ transcript context hash.
///
/// Computes `SHA-256("CHIP PAKE V1 Commissioning" || extra[0] || extra[1] || ...)`.
///
/// - Negotiation path: `extras = [pbkdfReq_bytes, pbkdfResp_bytes]`.
/// - Known-params path: `extras = []`.
///
/// This matches matter.js `PaseClient.ts` / `PaseServer.ts`:
/// ```ts
/// context = await crypto.computeHash([SPAKE_CONTEXT, requestPayload, responsePayload])
/// ```
/// where `computeHash(arr)` concatenates the array elements and SHA-256-hashes
/// the result.
///
/// Both `PaseProver` and `PaseVerifier` call this function; it is the single
/// canonical implementation of the context composition.
pub(crate) fn hash_context(extras: &[&[u8]]) -> [u8; 32] {
    let mut buf: Vec<u8> =
        Vec::with_capacity(SPAKE_CONTEXT.len() + extras.iter().map(|e| e.len()).sum::<usize>());
    buf.extend_from_slice(SPAKE_CONTEXT);
    for e in extras {
        buf.extend_from_slice(e);
    }
    let d = digest(&SHA256, &buf);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use ring::rand::SystemRandom;

    // ─── hash_context ─────────────────────────────────────────────────────

    #[test]
    fn hash_context_empty_extras_is_deterministic() {
        let a = hash_context(&[]);
        let b = hash_context(&[]);
        assert_eq!(a, b);
    }

    #[test]
    fn hash_context_with_extras_differs_from_empty() {
        let empty = hash_context(&[]);
        let with_extra = hash_context(&[b"hello"]);
        assert_ne!(empty, with_extra);
    }

    #[test]
    fn hash_context_is_sha256_of_spake_context_concatenation() {
        let req = b"req_bytes";
        let resp = b"resp_bytes";
        // Manually compute: SHA-256("CHIP PAKE V1 Commissioning" || req || resp).
        let mut buf = Vec::new();
        buf.extend_from_slice(b"CHIP PAKE V1 Commissioning");
        buf.extend_from_slice(req);
        buf.extend_from_slice(resp);
        let expected_raw = digest(&SHA256, &buf);
        let mut expected = [0u8; 32];
        expected.copy_from_slice(expected_raw.as_ref());

        let got = hash_context(&[req, resp]);
        assert_eq!(got, expected);
    }

    // ─── Point constant tests ──────────────────────────────────────────────

    #[test]
    fn m_and_n_decode_as_valid_p256_points() {
        // Both must produce non-zero (non-identity) distinct points.
        let m = point_from_spec_bytes(&M_BYTES);
        let n = point_from_spec_bytes(&N_BYTES);
        assert_ne!(encode_point(&m), [0u8; 65]);
        assert_ne!(encode_point(&n), [0u8; 65]);
        assert_ne!(encode_point(&m), encode_point(&n));
    }

    #[test]
    fn m_and_n_round_trip() {
        // Decoding then re-encoding must be identity.
        let m = point_from_spec_bytes(&M_BYTES);
        let n = point_from_spec_bytes(&N_BYTES);
        assert_eq!(encode_point(&m), M_BYTES);
        assert_eq!(encode_point(&n), N_BYTES);
    }

    #[test]
    fn lazylock_m_n_points_pin_to_decoded_bytes() {
        // Ensure the lazy-initialized statics match the independent decoder.
        assert_eq!(*M_POINT, point_from_spec_bytes(&M_BYTES));
        assert_eq!(*N_POINT, point_from_spec_bytes(&N_BYTES));
    }

    // ─── Scalar sampling ──────────────────────────────────────────────────

    #[test]
    fn sample_scalar_yields_nonzero() {
        let rng = SystemRandom::new();
        for _ in 0..16 {
            let s = sample_scalar(&rng).unwrap();
            assert!(!bool::from(s.is_zero()));
        }
    }

    // ─── X / Y computation ────────────────────────────────────────────────

    #[test]
    fn compute_x_starts_with_uncompressed_prefix() {
        let rng = SystemRandom::new();
        let x = sample_scalar(&rng).unwrap();
        let w0 = sample_scalar(&rng).unwrap();
        let x_bytes = compute_x(&x, &w0);
        assert_eq!(
            x_bytes[0], 0x04,
            "X must be SEC1 uncompressed (prefix 0x04)"
        );
    }

    #[test]
    fn compute_y_starts_with_uncompressed_prefix() {
        let rng = SystemRandom::new();
        let y = sample_scalar(&rng).unwrap();
        let w0 = sample_scalar(&rng).unwrap();
        let y_bytes = compute_y(&y, &w0);
        assert_eq!(
            y_bytes[0], 0x04,
            "Y must be SEC1 uncompressed (prefix 0x04)"
        );
    }

    // ─── Z / V agreement — the core correctness test ──────────────────────

    /// THE critical symmetric-correctness test.
    /// If this passes, the SPAKE2+ math is internally consistent:
    /// prover and verifier independently compute the same shared secret.
    #[test]
    fn compute_z_v_prover_and_verifier_agree() {
        let rng = SystemRandom::new();
        let scalar_x = sample_scalar(&rng).unwrap();
        let scalar_y = sample_scalar(&rng).unwrap();
        let w0 = sample_scalar(&rng).unwrap();
        let w1 = sample_scalar(&rng).unwrap();

        let x_bytes = compute_x(&scalar_x, &w0);
        let y_bytes = compute_y(&scalar_y, &w0);
        // L = w1 · P (the verifier's stored public value).
        let l_bytes = encode_point(&(ProjectivePoint::GENERATOR * w1));

        let (z_prover, v_prover) = compute_z_v_prover(&scalar_x, &w0, &w1, &y_bytes).unwrap();
        let (z_verifier, v_verifier) =
            compute_z_v_verifier(&scalar_y, &w0, &l_bytes, &x_bytes).unwrap();

        assert_eq!(
            z_prover, z_verifier,
            "Z must match between prover and verifier"
        );
        assert_eq!(
            v_prover, v_verifier,
            "V must match between prover and verifier"
        );
    }

    // ─── Transcript hash ──────────────────────────────────────────────────

    #[test]
    fn transcript_hash_is_deterministic() {
        let rng = SystemRandom::new();
        let w0 = sample_scalar(&rng).unwrap();
        let context = b"CHIP PAKE V1 Commissioning";
        let x_pt = [0x01u8; 65];
        let y_pt = [0x02u8; 65];
        let z_pt = [0x03u8; 65];
        let v_pt = [0x04u8; 65];

        let hash_a = transcript_hash(context, &x_pt, &y_pt, &z_pt, &v_pt, &w0);
        let hash_b = transcript_hash(context, &x_pt, &y_pt, &z_pt, &v_pt, &w0);
        assert_eq!(hash_a, hash_b, "transcript_hash must be deterministic");
    }

    #[test]
    fn transcript_hash_is_input_sensitive() {
        let rng = SystemRandom::new();
        let w0 = sample_scalar(&rng).unwrap();
        let context = b"CHIP PAKE V1 Commissioning";
        let x_pt = [0x01u8; 65];
        let y_pt = [0x02u8; 65];
        let z_pt = [0x03u8; 65];
        let v_pt = [0x04u8; 65];

        let base = transcript_hash(context, &x_pt, &y_pt, &z_pt, &v_pt, &w0);

        // Flip one bit in X.
        let mut x_pt2 = x_pt;
        x_pt2[10] ^= 1;
        let changed = transcript_hash(context, &x_pt2, &y_pt, &z_pt, &v_pt, &w0);
        assert_ne!(base, changed, "different X must produce different hash");
    }

    // ─── Ka / Ke split ────────────────────────────────────────────────────

    #[test]
    #[allow(clippy::cast_possible_truncation)] // i is 0..32, safely fits in u8.
    fn ka_ke_split_is_correct() {
        let t_t: [u8; 32] = {
            let mut buf = [0u8; 32];
            for (i, byte) in buf.iter_mut().enumerate() {
                *byte = i as u8;
            }
            buf
        };
        let (ka, ke) = ka_ke_from_transcript(&t_t);
        assert_eq!(ka, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(
            ke,
            [16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31]
        );
        assert_ne!(ka, ke, "Ka and Ke must be different halves");
    }

    // ─── Confirmation keys ────────────────────────────────────────────────

    #[test]
    // kca / kcb are the SPAKE2+ confirmation key names from Matter Core
    // Spec §3.10 (RFC 9383 §3.3).
    #[allow(clippy::similar_names)]
    fn confirmation_keys_split_correctly() {
        let ka = [0x42u8; 16];
        let (kca, kcb) = derive_confirmation_keys(&ka).unwrap();
        assert_ne!(kca, kcb, "KcA and KcB must differ");
        assert_eq!(kca.len(), 16);
        assert_eq!(kcb.len(), 16);
    }

    #[test]
    fn confirmation_tags_are_input_sensitive() {
        let kca = [0x11u8; 16];
        let y1 = [0x22u8; 65];
        let mut y2 = y1;
        y2[5] ^= 1;
        assert_ne!(
            compute_ca(&kca, &y1),
            compute_ca(&kca, &y2),
            "different Y must produce different cA"
        );
    }

    // ─── Tag verification ─────────────────────────────────────────────────

    #[test]
    fn verify_tag_accepts_matching_tags() {
        let t1 = [0x11u8; 32];
        let t2 = [0x11u8; 32];
        verify_tag(&t1, &t2).unwrap();
    }

    #[test]
    fn verify_tag_rejects_mismatched_tags() {
        let t1 = [0x11u8; 32];
        let mut t3 = t1;
        t3[31] ^= 1;
        assert!(
            matches!(verify_tag(&t1, &t3), Err(Error::ConfirmationTagMismatch)),
            "mismatched tags must return ConfirmationTagMismatch"
        );
    }

    // ─── Session keys ─────────────────────────────────────────────────────

    #[test]
    fn session_keys_are_48_bytes_and_deterministic() {
        let ke = [0x42u8; 16];
        let a = derive_session_keys(&ke).unwrap();
        let b = derive_session_keys(&ke).unwrap();
        assert_eq!(a, b, "derive_session_keys must be deterministic");
        assert_eq!(a.len(), 48);
    }

    // ─── End-to-end handshake math ────────────────────────────────────────

    /// The strongest single test: prover and verifier independently compute
    /// the same `TT_HASH` and therefore the same session keys.
    #[test]
    #[allow(clippy::similar_names)] // ka_prover/ke_prover etc. are domain-correct names.
    fn full_handshake_math_produces_matching_session_keys() {
        let rng = SystemRandom::new();
        let scalar_x = sample_scalar(&rng).unwrap();
        let scalar_y = sample_scalar(&rng).unwrap();
        let w0 = sample_scalar(&rng).unwrap();
        let w1 = sample_scalar(&rng).unwrap();

        let x_bytes = compute_x(&scalar_x, &w0);
        let y_bytes = compute_y(&scalar_y, &w0);
        let l_bytes = encode_point(&(ProjectivePoint::GENERATOR * w1));

        // Both sides compute Z and V independently.
        let (z_prover, v_prover) = compute_z_v_prover(&scalar_x, &w0, &w1, &y_bytes).unwrap();
        let (z_verifier, v_verifier) =
            compute_z_v_verifier(&scalar_y, &w0, &l_bytes, &x_bytes).unwrap();
        assert_eq!(z_prover, z_verifier, "Z must match");
        assert_eq!(v_prover, v_verifier, "V must match");

        // Both sides compute the transcript hash (using raw context bytes for this test;
        // in the real protocol, context = SHA-256(SPAKE_CONTEXT || pbkdfReq || pbkdfResp)).
        let context = b"CHIP PAKE V1 Commissioning";
        let tt_prover = transcript_hash(context, &x_bytes, &y_bytes, &z_prover, &v_prover, &w0);
        let tt_verifier =
            transcript_hash(context, &x_bytes, &y_bytes, &z_verifier, &v_verifier, &w0);
        assert_eq!(tt_prover, tt_verifier, "TT_HASH must match");

        // Ka/Ke split is deterministic.
        let (ka_prover, ke_prover) = ka_ke_from_transcript(&tt_prover);
        let (ka_verifier, ke_verifier) = ka_ke_from_transcript(&tt_verifier);
        assert_eq!(ka_prover, ka_verifier, "Ka must match");
        assert_eq!(ke_prover, ke_verifier, "Ke must match");

        // Session keys derived from Ke must match.
        let session_keys_p = derive_session_keys(&ke_prover).unwrap();
        let session_keys_v = derive_session_keys(&ke_verifier).unwrap();
        assert_eq!(
            session_keys_p, session_keys_v,
            "SessionKeys must match end-to-end"
        );

        // Confirmation key derivation from Ka must also be consistent.
        let (confirm_a_prover, confirm_b_prover) = derive_confirmation_keys(&ka_prover).unwrap();
        let (confirm_a_verifier, confirm_b_verifier) =
            derive_confirmation_keys(&ka_verifier).unwrap();
        assert_eq!(confirm_a_prover, confirm_a_verifier, "KcA must match");
        assert_eq!(confirm_b_prover, confirm_b_verifier, "KcB must match");

        // cA and cB must also agree.
        let tag_a_prover = compute_ca(&confirm_a_prover, &y_bytes);
        let tag_a_verifier = compute_ca(&confirm_a_verifier, &y_bytes);
        assert_eq!(tag_a_prover, tag_a_verifier, "cA must match");

        let tag_b_prover = compute_cb(&confirm_b_prover, &x_bytes);
        let tag_b_verifier = compute_cb(&confirm_b_verifier, &x_bytes);
        assert_eq!(tag_b_prover, tag_b_verifier, "cB must match");

        // Each side can verify the other's tag.
        verify_tag(&tag_a_prover, &tag_a_verifier).unwrap();
        verify_tag(&tag_b_prover, &tag_b_verifier).unwrap();
    }
}
