//! Device-side PASE state machine.
//!
//! Drives the 5-message PASE handshake (or 3-message known-params path)
//! from the device's (verifier's) perspective. Sans-IO: the caller feeds raw
//! TLV bytes in and gets raw TLV bytes out; no sockets, no async, no I/O.
//!
//! # Protocol flow (negotiation path — Matter Core Spec §3.10.5)
//!
//! ```text
//! Commissioner (Prover)          Device (Verifier)
//! ───────────────────────────────────────────────
//!   → PBKDFParamRequest  ────────────────────────>
//!                        handle_pbkdf_request()
//!                        next_message()  [PBKDFParamResponse]
//!                        <──────── PBKDFParamResponse
//!   → Pake1              ────────────────────────>
//!                        handle_pake1()
//!                        next_message()  [Pake2]
//!                        <──────── Pake2
//!   → Pake3              ────────────────────────>
//!                        handle_pake3()
//!                        finish() → PaseSessionKeys
//! ```
//!
//! # Known-params path
//!
//! When the commissioner already has the PBKDF params cached it skips
//! `PBKDFParamRequest` and sends Pake1 directly. The verifier's
//! `AwaitingFirstMessage` state accepts either message kind and branches
//! automatically.
//!
//! ```text
//!   → Pake1  →  handle_pake1()  →  next_message() [Pake2]
//!          →  handle_pake3()  →  finish()
//! ```
//!
//! # Verifier values
//!
//! The device stores `w0` (32-byte big-endian scalar) and `L` (65-byte
//! uncompressed P-256 point) computed from the setup PIN at provisioning time.
//! The PIN is **never** stored after provisioning. See [`PaseVerifier::new`] for
//! the production constructor and [`PaseVerifier::new_from_pin`] for the
//! test/convenience path.
//!
//! # Transcript context composition
//!
//! Same as the prover — `hash_context` in `spake2plus.rs`:
//! `context = SHA-256("CHIP PAKE V1 Commissioning" || pbkdfReq || pbkdfResp)`.
//! For the known-params path (no param exchange): `context = SHA-256("CHIP PAKE V1 Commissioning")`.
//!
//! # Session key assignment (device = responder)
//!
//! From matter.js `NodeSession.ts` with `isInitiator = false`:
//! ```ts
//! const decryptKey  = isInitiator ? keys.slice(16, 32) : keys.slice(0, 16);
//! const encryptKey  = isInitiator ? keys.slice(0, 16)  : keys.slice(16, 32);
//! ```
//! So for the device (responder):
//! - `r2i_key` (device → commissioner, device encrypts) = `blob[16..32]`.
//! - `i2r_key` (commissioner → device, device decrypts) = `blob[0..16]`.
//!
//! The prover (commissioner) uses the opposite assignment, so both sides
//! produce the same `i2r_key` and `r2i_key` in `PaseSessionKeys` from
//! their respective perspectives.

use p256::elliptic_curve::group::ff::PrimeField; // PrimeField for Scalar::from_repr
use ring::rand::{SecureRandom, SystemRandom};

use crate::error::{Error, Result};
use crate::pase::kdf::{derive_l, derive_w0_w1, validate_params};
use crate::pase::messages::{
    Pake1, Pake2, Pake3, PbkdfParamRequest, PbkdfParamResponse, PbkdfParamsInner,
};
use crate::pase::spake2plus::{
    compute_ca, compute_cb, compute_y, compute_z_v_verifier, derive_confirmation_keys,
    derive_session_keys, hash_context, ka_ke_from_transcript, sample_scalar, transcript_hash,
    verify_tag,
};
use crate::pase::{PaseMessageKind, PasePbkdfParams, PaseSessionKeys};
use zeroize::Zeroize;

// =============================================================================
// Internal state enum
// =============================================================================

/// Internal states of the device-side PASE handshake.
///
/// Each variant corresponds to a point in the protocol flow from Matter Core
/// Spec §3.10.5 as seen by the responder. Named for the *next action* the
/// state machine expects or is ready to perform.
///
/// The `AwaitingFirstMessage` state is a branch point: the verifier accepts
/// either `PBKDFParamRequest` (negotiation path) or `Pake1` (known-params
/// path) as its first inbound message. The caller does not need to know which
/// path the commissioner will choose.
enum State {
    /// Waiting for the first inbound message.
    ///
    /// The verifier does not know yet whether the commissioner will send
    /// `PBKDFParamRequest` (negotiation) or `Pake1` (known-params). Both
    /// are valid here.
    ///
    /// `y_scalar` is pre-sampled so that `handle_pake1` is infallible for
    /// randomness after construction succeeds.
    AwaitingFirstMessage {
        w0: p256::Scalar,
        /// 65-byte uncompressed P-256 point `L = w1·P` stored by the device.
        l: [u8; 65],
        params: PasePbkdfParams,
        y_scalar: p256::Scalar,
        /// Pre-sampled 32-byte responder nonce, used if the commissioner sends
        /// a `PBKDFParamRequest`.
        responder_random: [u8; 32],
        /// Session ID to include in `PBKDFParamResponse`. Set to 0 by the
        /// production constructors; overrideable for testing.
        responder_session_id: u16,
    },

    /// `PBKDFParamRequest` received; `next_message()` will emit the Response.
    ///
    /// Holds the verbatim bytes of the request (for transcript context), the
    /// responder random, and enough to build the Response.
    ReadyToSendPbkdfResponse {
        w0: p256::Scalar,
        l: [u8; 65],
        params: PasePbkdfParams,
        y_scalar: p256::Scalar,
        /// Verbatim TLV bytes of the `PBKDFParamRequest`, saved for transcript context.
        request_bytes: Vec<u8>,
        responder_random: [u8; 32],
        initiator_random: [u8; 32],
        responder_session_id: u16,
    },

    /// `PBKDFParamResponse` sent; waiting for Pake1.
    ///
    /// `transcript_context` is already computed as
    /// `SHA-256(SPAKE_CONTEXT || pbkdfReq || pbkdfResp)`.
    AwaitingPake1 {
        w0: p256::Scalar,
        l: [u8; 65],
        y_scalar: p256::Scalar,
        /// SHA-256 context hash to pass into `transcript_hash`.
        transcript_context: [u8; 32],
    },

    /// Pake1 received; `next_message()` will emit Pake2 (Y + cB).
    ReadyToSendPake2 {
        /// Our Y point (65 bytes, uncompressed P-256), to send in Pake2.
        y_bytes: [u8; 65],
        /// Our confirmation tag `cB = HMAC-SHA256(KcB, X)`, to send in Pake2.
        cb: [u8; 32],
        /// The expected `cA = HMAC-SHA256(KcA, Y)` that Pake3 must carry.
        ca_expected: [u8; 32],
        session_keys: PaseSessionKeys,
    },

    /// Pake3 received and `cA` verified; `finish()` may be called.
    Complete { session_keys: PaseSessionKeys },

    /// Sentinel used during `std::mem::replace` state transitions.
    ///
    /// This variant is **never observable** to callers: every `mem::replace`
    /// immediately replaces `Poisoned` with the next real state, or returns
    /// an error before storing it. If somehow reached, all methods return
    /// [`Error::HandshakeIncomplete`].
    Poisoned,
}

/// Names the variant only. `w0`, `L`, the ephemeral scalar `y` and the
/// confirmation values `cB` and expected `cA` live in these variants'
/// fields, and a derived `Debug` would print all of them (p256's `Scalar`
/// prints as `Scalar(0x…)`). The session keys already print redacted.
/// Nothing needs any of them in a log, so none are printed. Written by
/// hand, not derived, so that a `Debug` added to the public state machine
/// later cannot print them either.
impl core::fmt::Debug for State {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::AwaitingFirstMessage { .. } => "AwaitingFirstMessage",
            Self::ReadyToSendPbkdfResponse { .. } => "ReadyToSendPbkdfResponse",
            Self::AwaitingPake1 { .. } => "AwaitingPake1",
            Self::ReadyToSendPake2 { .. } => "ReadyToSendPake2",
            Self::Complete { .. } => "Complete",
            Self::Poisoned => "Poisoned",
        })
    }
}

// =============================================================================
// PaseVerifier
// =============================================================================

/// Device-side PASE state machine.
///
/// Drives the SPAKE2+ handshake from the device's (responder's) perspective.
/// Sans-IO: the caller is responsible for transmitting and receiving bytes.
///
/// # Construction
///
/// - [`PaseVerifier::new`] — production path; device stores pre-computed `w0`
///   and `L` (never the PIN after provisioning).
/// - [`PaseVerifier::new_from_pin`] — test/convenience path; derives `w0` and
///   `L` from the PIN via PBKDF2.
///
/// # Driving the handshake
///
/// 1. Feed the inbound `PBKDFParamRequest` bytes into
///    [`handle_pbkdf_request`][Self::handle_pbkdf_request] (negotiation path),
///    or skip to step 3 (known-params path).
/// 2. Call [`next_message`][Self::next_message] to emit `PBKDFParamResponse`.
/// 3. Feed the inbound `Pake1` bytes into
///    [`handle_pake1`][Self::handle_pake1].
/// 4. Call [`next_message`][Self::next_message] to emit Pake2.
/// 5. Feed the inbound `Pake3` bytes into
///    [`handle_pake3`][Self::handle_pake3].
/// 6. Call [`finish`][Self::finish] to retrieve the [`PaseSessionKeys`].
///
/// Use [`expected_inbound`][Self::expected_inbound] at any point to query
/// which message type the machine is currently waiting for.
pub struct PaseVerifier {
    state: State,
}

impl PaseVerifier {
    // ─── Public constructors ──────────────────────────────────────────────

    /// Production constructor: device stores pre-computed verification values.
    ///
    /// In production the PIN is hashed to `w0` and `L` once at provisioning
    /// time and the raw PIN is discarded. Pass those stored values here.
    ///
    /// Validates `params` against Matter spec §3.10.3 bounds before accepting.
    ///
    /// # Parameters
    ///
    /// - `w0`: 32-byte big-endian P-256 scalar derived from the PIN.
    /// - `l`: 65-byte uncompressed P-256 point `L = w1·P`.
    /// - `params`: PBKDF2 parameters used when `w0`/`L` were derived.
    /// - `responder_session_id`: the non-zero secured-session id this device
    ///   advertises (in `PBKDFParamResponse`) for the peer to address it by.
    ///
    /// # Errors
    ///
    /// - [`Error::PbkdfIterationsTooLow`] if `params.iterations < 1000`.
    /// - [`Error::PbkdfSaltLengthInvalid`] if `params.salt.len()` ∉ \[16, 32\].
    /// - [`Error::InvalidScalar`] if the CSPRNG is broken and a non-zero
    ///   scalar cannot be sampled after 16 attempts (practically impossible).
    /// - [`Error::PinDerivationFailed`] if the nonce fill fails.
    pub fn new(
        w0: [u8; 32],
        l: [u8; 65],
        params: PasePbkdfParams,
        responder_session_id: u16,
    ) -> Result<Self> {
        validate_params(params.iterations, &params.salt)?;
        let rng = SystemRandom::new();
        Self::new_using_rng(w0, l, params, responder_session_id, &rng)
    }

    /// Deterministic constructor for testing — accepts an injectable RNG.
    ///
    /// Production code should always use [`new`][Self::new].
    pub(crate) fn new_using_rng(
        w0_bytes: [u8; 32],
        l: [u8; 65],
        params: PasePbkdfParams,
        responder_session_id: u16,
        rng: &dyn SecureRandom,
    ) -> Result<Self> {
        validate_params(params.iterations, &params.salt)?;

        // Decode `w0` from 32-byte big-endian representation.
        // `Scalar::from_repr` returns `CtOption<Scalar>` — this is a direct
        // conversion (no modular reduction needed because the stored value is
        // already reduced at provisioning time).
        let w0_opt: Option<p256::Scalar> =
            p256::Scalar::from_repr(p256::FieldBytes::from(w0_bytes)).into();
        let w0 = w0_opt.ok_or(Error::InvalidScalar)?;
        // The zero scalar is not a valid w0 — it would collapse all SPAKE2+ math.
        // `is_zero()` returns `subtle::Choice`; convert via `bool::from`.
        if bool::from(p256::elliptic_curve::group::ff::Field::is_zero(&w0)) {
            return Err(Error::InvalidScalar);
        }

        let y_scalar = sample_scalar(rng)?;
        let mut responder_random = [0u8; 32];
        rng.fill(&mut responder_random)
            .map_err(|_| Error::PinDerivationFailed)?;

        Ok(Self {
            state: State::AwaitingFirstMessage {
                w0,
                l,
                params,
                y_scalar,
                responder_random,
                responder_session_id,
            },
        })
    }

    /// Test/convenience constructor: derive `w0` and `L` from the PIN.
    ///
    /// In production a device never stores the PIN after provisioning —
    /// it stores `w0` and `L` instead. This constructor is provided for
    /// tests and development use where deriving from a PIN is convenient.
    ///
    /// # Parameters
    ///
    /// - `responder_session_id`: the non-zero secured-session id this device
    ///   advertises (in `PBKDFParamResponse`) for the peer to address it by.
    ///
    /// # Errors
    ///
    /// - [`Error::PbkdfIterationsTooLow`] / [`Error::PbkdfSaltLengthInvalid`]
    ///   if `params` are out of spec.
    /// - [`Error::PinDerivationFailed`] if PBKDF2 fails.
    /// - [`Error::InvalidScalar`] if the CSPRNG is broken.
    pub fn new_from_pin(
        pin: u32,
        params: PasePbkdfParams,
        responder_session_id: u16,
    ) -> Result<Self> {
        let rng = SystemRandom::new();
        Self::new_from_pin_using_rng(pin, params, responder_session_id, &rng)
    }

    /// Deterministic constructor for testing — derives `w0`/`L` from the PIN
    /// and accepts an injectable RNG for the SPAKE2+ scalar and nonce.
    ///
    /// Production code should always use [`new_from_pin`][Self::new_from_pin].
    pub(crate) fn new_from_pin_using_rng(
        pin: u32,
        params: PasePbkdfParams,
        responder_session_id: u16,
        rng: &dyn SecureRandom,
    ) -> Result<Self> {
        let (w0_scalar, w1_scalar) = derive_w0_w1(pin, &params.salt, params.iterations)?;
        let l = derive_l(&w1_scalar);
        // Encode w0 as 32-byte big-endian for the `new_using_rng` constructor.
        // `Scalar::to_bytes()` returns `FieldBytes` in big-endian order.
        let w0_be: p256::FieldBytes = w0_scalar.to_bytes();
        let mut w0_arr = [0u8; 32];
        w0_arr.copy_from_slice(&w0_be);
        Self::new_using_rng(w0_arr, l, params, responder_session_id, rng)
    }

    // Deterministic-input constructors below are used only by
    // `crate::test_support`, which is itself behind the `test-support`
    // feature. Gated to match, so a default-feature build does not compile
    // them as dead code.
    /// Deterministic constructor for testing — injects a fixed `y` scalar
    /// directly, bypassing the RNG. Accepts pre-computed `w0` and `L`.
    ///
    /// Uses an all-zero `responder_random` and `responder_session_id = 0`.
    /// For full matter.js byte-parity tests, use
    /// [`new_with_scalar_and_random`][Self::new_with_scalar_and_random] instead.
    ///
    /// Production code should always use [`new`][Self::new].
    ///
    /// # Errors
    ///
    /// - [`Error::PbkdfIterationsTooLow`] / [`Error::PbkdfSaltLengthInvalid`]
    ///   if `params` are out of spec.
    /// - [`Error::InvalidScalar`] if `w0_bytes` or `y_scalar_bytes` is zero or
    ///   not a valid P-256 scalar.
    #[cfg(feature = "test-support")]
    pub(crate) fn new_with_scalar(
        w0_bytes: [u8; 32],
        l: [u8; 65],
        params: PasePbkdfParams,
        y_scalar_bytes: [u8; 32],
    ) -> Result<Self> {
        Self::new_with_scalar_and_random(w0_bytes, l, params, y_scalar_bytes, [0u8; 32], 0)
    }

    /// Deterministic constructor for testing — injects a fixed `y` scalar,
    /// `responder_random`, and `responder_session_id` directly.
    ///
    /// Used by `test_support` to reproduce matter.js fixture handshakes that
    /// capture a specific responder nonce and session ID in
    /// `PBKDFParamResponse`. Production code always uses [`new`][Self::new].
    ///
    /// # Errors
    ///
    /// - [`Error::PbkdfIterationsTooLow`] / [`Error::PbkdfSaltLengthInvalid`]
    ///   if `params` are out of spec.
    /// - [`Error::InvalidScalar`] if `w0_bytes` or `y_scalar_bytes` is zero or
    ///   not a valid P-256 scalar.
    #[cfg(feature = "test-support")]
    pub(crate) fn new_with_scalar_and_random(
        w0_bytes: [u8; 32],
        l: [u8; 65],
        params: PasePbkdfParams,
        y_scalar_bytes: [u8; 32],
        responder_random: [u8; 32],
        responder_session_id: u16,
    ) -> Result<Self> {
        use p256::elliptic_curve::group::ff::Field;
        validate_params(params.iterations, &params.salt)?;

        let w0_opt: Option<p256::Scalar> =
            p256::Scalar::from_repr(p256::FieldBytes::from(w0_bytes)).into();
        let w0 = w0_opt.ok_or(Error::InvalidScalar)?;
        if bool::from(w0.is_zero()) {
            return Err(Error::InvalidScalar);
        }

        let y_opt: Option<p256::Scalar> =
            p256::Scalar::from_repr(p256::FieldBytes::from(y_scalar_bytes)).into();
        let y_scalar = y_opt.ok_or(Error::InvalidScalar)?;
        if bool::from(y_scalar.is_zero()) {
            return Err(Error::InvalidScalar);
        }

        Ok(Self {
            state: State::AwaitingFirstMessage {
                w0,
                l,
                params,
                y_scalar,
                responder_random,
                responder_session_id,
            },
        })
    }

    /// Deterministic constructor for testing — derives `w0`/`L` from the PIN
    /// and injects a fixed `y` scalar directly, bypassing the RNG.
    ///
    /// Used by `test_support` to construct a verifier with a known scalar for
    /// matter.js byte-parity tests.
    ///
    /// Production code should always use [`new_from_pin`][Self::new_from_pin].
    ///
    /// # Errors
    ///
    /// - [`Error::PbkdfIterationsTooLow`] / [`Error::PbkdfSaltLengthInvalid`]
    ///   if `params` are out of spec.
    /// - [`Error::PinDerivationFailed`] if PBKDF2 fails.
    /// - [`Error::InvalidScalar`] if `y_scalar_bytes` is zero or not a valid
    ///   P-256 scalar.
    #[cfg(feature = "test-support")]
    pub(crate) fn new_from_pin_with_scalar(
        pin: u32,
        params: PasePbkdfParams,
        y_scalar_bytes: [u8; 32],
    ) -> Result<Self> {
        let (w0_scalar, w1_scalar) = derive_w0_w1(pin, &params.salt, params.iterations)?;
        let l = derive_l(&w1_scalar);
        let w0_be: p256::FieldBytes = w0_scalar.to_bytes();
        let mut w0_arr = [0u8; 32];
        w0_arr.copy_from_slice(&w0_be);
        Self::new_with_scalar(w0_arr, l, params, y_scalar_bytes)
    }

    // ─── State inspection ─────────────────────────────────────────────────

    /// Returns the message kind the state machine is currently waiting to
    /// receive, or `None` if the machine is in an outbound-only or completed
    /// state.
    ///
    /// Useful for routing inbound messages in a dispatcher.
    pub fn expected_inbound(&self) -> Option<PaseMessageKind> {
        match &self.state {
            State::AwaitingFirstMessage { .. } => {
                // Either PbkdfParamRequest or Pake1 is valid; return the more
                // common (negotiation-path) expectation. Callers that need
                // strict routing should branch on `handle_pbkdf_request` vs
                // `handle_pake1`.
                Some(PaseMessageKind::PbkdfParamRequest)
            }
            State::AwaitingPake1 { .. } => Some(PaseMessageKind::Pake1),
            State::ReadyToSendPake2 { .. } => Some(PaseMessageKind::Pake3),
            _ => None,
        }
    }

    // ─── Inbound handlers ─────────────────────────────────────────────────

    /// Process an inbound `PBKDFParamRequest` message (negotiation path).
    ///
    /// Decodes the request, captures the raw bytes for transcript composition,
    /// and transitions to `ReadyToSendPbkdfResponse`. After this call,
    /// [`next_message`][Self::next_message] emits `PBKDFParamResponse`.
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedMessage`] if called from any state other than
    ///   `AwaitingFirstMessage`.
    /// - [`Error::Codec`] on TLV decoding failure.
    /// - [`Error::InvalidParameter`] if the request is malformed.
    pub fn handle_pbkdf_request(&mut self, bytes: &[u8]) -> Result<()> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::AwaitingFirstMessage {
                w0,
                l,
                params,
                y_scalar,
                responder_random,
                responder_session_id,
            } => {
                // Decode the request so we can capture `initiator_random`.
                // We also keep the verbatim bytes for transcript context.
                let req = PbkdfParamRequest::decode(bytes)?;

                self.state = State::ReadyToSendPbkdfResponse {
                    w0,
                    l,
                    params,
                    y_scalar,
                    request_bytes: bytes.to_vec(),
                    responder_random,
                    initiator_random: req.initiator_random,
                    responder_session_id,
                };
                Ok(())
            }

            other => {
                self.state = other;
                Err(Error::UnexpectedMessage {
                    expected: PaseMessageKind::PbkdfParamRequest,
                    got: PaseMessageKind::PbkdfParamRequest,
                })
            }
        }
    }

    /// Process an inbound `Pake1` message.
    ///
    /// Valid in two states:
    /// - `AwaitingFirstMessage` — commissioner skipped param negotiation
    ///   (known-params path); context = `SHA-256(SPAKE_CONTEXT)`.
    /// - `AwaitingPake1` — negotiation complete; context already computed.
    ///
    /// After this call, [`next_message`][Self::next_message] emits Pake2.
    ///
    /// # Cryptography
    ///
    /// 1. Decode X from Pake1 TLV.
    /// 2. Compute `Y = y·P + w0·N` (verifier's SPAKE2+ share).
    /// 3. Compute `Z = y·(X − w0·M)` and `V = y·L` (shared secrets).
    /// 4. Compute the SPAKE2+ transcript hash `TT_HASH`.
    /// 5. Split `Ka` (first 16 bytes) and `Ke` (last 16 bytes) from `TT_HASH`.
    /// 6. Derive confirmation keys `KcA`/`KcB` from `Ka`.
    /// 7. Compute `cB = HMAC-SHA256(KcB, X)` (our confirmation tag to send).
    /// 8. Compute `cA_expected = HMAC-SHA256(KcA, Y)` (to verify in Pake3).
    /// 9. Derive session keys from `Ke`.
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedMessage`] if called from the wrong state.
    /// - [`Error::Codec`] on TLV decoding failure.
    /// - [`Error::InvalidParameter`] if X is not a valid P-256 point.
    /// - [`Error::PinDerivationFailed`] on HKDF failure.
    pub fn handle_pake1(&mut self, bytes: &[u8]) -> Result<()> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            // Known-params path: commissioner sent Pake1 as the first message.
            State::AwaitingFirstMessage {
                w0,
                l,
                y_scalar,
                // params, responder_random, and responder_session_id are not used
                // on the known-params path (no PbkdfParam exchange).
                ..
            } => {
                // context = SHA-256("CHIP PAKE V1 Commissioning") — no param exchange.
                let transcript_context = hash_context(&[]);
                self.state = State::Poisoned; // keep Poisoned while we do crypto
                self.compute_pake2(w0, l, y_scalar, transcript_context, bytes)
            }

            // Negotiation path: param exchange complete, now handle Pake1.
            State::AwaitingPake1 {
                w0,
                l,
                y_scalar,
                transcript_context,
            } => {
                self.state = State::Poisoned; // keep Poisoned while we do crypto
                self.compute_pake2(w0, l, y_scalar, transcript_context, bytes)
            }

            other => {
                self.state = other;
                Err(Error::UnexpectedMessage {
                    expected: PaseMessageKind::Pake1,
                    got: PaseMessageKind::Pake1,
                })
            }
        }
    }

    /// Process an inbound `Pake3` message.
    ///
    /// Verifies the commissioner's confirmation tag `cA` using constant-time
    /// comparison (`subtle::ConstantTimeEq`). If verification succeeds the
    /// state machine transitions to `Complete` and [`finish`][Self::finish]
    /// may be called.
    ///
    /// # Security
    ///
    /// Tag comparison MUST be constant-time. This is enforced by routing
    /// through `verify_tag` (in `pase::spake2plus`) which uses
    /// `subtle::ConstantTimeEq`.
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedMessage`] if called before Pake2 was sent.
    /// - [`Error::ConfirmationTagMismatch`] if `cA` fails constant-time
    ///   verification (wrong PIN on the commissioner's side).
    /// - [`Error::Codec`] on TLV decoding failure.
    pub fn handle_pake3(&mut self, bytes: &[u8]) -> Result<()> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::ReadyToSendPake2 {
                y_bytes: _,
                cb: _,
                ca_expected,
                session_keys,
            } => {
                let pake3 = Pake3::decode(bytes)?;

                // CT-eq — never use `==` on HMAC tags.
                verify_tag(&ca_expected, &pake3.verifier)?;

                self.state = State::Complete { session_keys };
                Ok(())
            }

            other => {
                self.state = other;
                Err(Error::UnexpectedMessage {
                    expected: PaseMessageKind::Pake3,
                    got: PaseMessageKind::Pake3,
                })
            }
        }
    }

    /// Produce the next outbound message.
    ///
    /// - After [`handle_pbkdf_request`][Self::handle_pbkdf_request]: emits
    ///   `PBKDFParamResponse` (TLV bytes).
    /// - After [`handle_pake1`][Self::handle_pake1]: emits Pake2 (TLV bytes).
    ///
    /// Calling from any other state returns [`Error::UnexpectedMessage`].
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedMessage`] if called from the wrong state.
    /// - [`Error::Codec`] on TLV encoding failure.
    pub fn next_message(&mut self) -> Result<Vec<u8>> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::ReadyToSendPbkdfResponse {
                w0,
                l,
                params,
                y_scalar,
                request_bytes,
                responder_random,
                initiator_random,
                responder_session_id,
            } => {
                // §3.10.5 step 2: build and send PBKDFParamResponse.
                // Include our PBKDF parameters (the commissioner set
                // has_pbkdf_parameters=false, so we must include them).
                let resp = PbkdfParamResponse {
                    initiator_random,
                    responder_random,
                    responder_session_id,
                    pbkdf_parameters: Some(PbkdfParamsInner {
                        iterations: params.iterations,
                        salt: params.salt.clone(),
                    }),
                    responder_session_params: None,
                };
                let resp_bytes = resp.encode()?;

                // Compose transcript context: SHA-256(SPAKE_CONTEXT || req || resp).
                let transcript_context = hash_context(&[&request_bytes, &resp_bytes]);

                self.state = State::AwaitingPake1 {
                    w0,
                    l,
                    y_scalar,
                    transcript_context,
                };
                Ok(resp_bytes)
            }

            State::ReadyToSendPake2 {
                y_bytes,
                cb,
                ca_expected,
                session_keys,
            } => {
                // §3.10.5: send Y and cB in Pake2.
                let pake2_bytes = Pake2 {
                    y: y_bytes,
                    verifier: cb,
                }
                .encode()?;
                // Keep ca_expected and session_keys for when Pake3 arrives.
                self.state = State::ReadyToSendPake2 {
                    y_bytes,
                    cb,
                    ca_expected,
                    session_keys,
                };
                Ok(pake2_bytes)
            }

            other => {
                self.state = other;
                Err(Error::UnexpectedMessage {
                    expected: PaseMessageKind::PbkdfParamResponse,
                    got: PaseMessageKind::PbkdfParamResponse,
                })
            }
        }
    }

    /// Finalise the session and retrieve the derived session keys.
    ///
    /// May only be called after [`handle_pake3`][Self::handle_pake3] has
    /// successfully verified `cA` (i.e., the state machine is in `Complete`).
    ///
    /// # Errors
    ///
    /// - [`Error::HandshakeIncomplete`] if called before the handshake has
    ///   completed all phases.
    pub fn finish(self) -> Result<PaseSessionKeys> {
        match self.state {
            State::Complete { session_keys } => Ok(session_keys),
            _ => Err(Error::HandshakeIncomplete),
        }
    }
}

// =============================================================================
// Internal helpers
// =============================================================================

impl PaseVerifier {
    /// Perform the verifier-side SPAKE2+ cryptography for Pake1 processing.
    ///
    /// This is a shared helper called from `handle_pake1` in both the
    /// known-params and post-negotiation code paths. Sets `self.state` to
    /// `ReadyToSendPake2` on success.
    ///
    /// The state machine must be in `Poisoned` before this call (the caller
    /// has already `mem::replace`d it out). On error, the machine remains
    /// `Poisoned` — the caller should not recover from a crypto failure.
    // kca / kcb are the SPAKE2+ confirmation key names from Matter
    // Core Spec §3.10 (RFC 9383 §3.3). Renaming would impair
    // verification against the spec.
    #[allow(clippy::similar_names)]
    fn compute_pake2(
        &mut self,
        w0: p256::Scalar,
        l: [u8; 65],
        y_scalar: p256::Scalar,
        transcript_context: [u8; 32],
        pake1_bytes: &[u8],
    ) -> Result<()> {
        let pake1 = Pake1::decode(pake1_bytes)?;

        // §3.10.5 verifier side:
        //   Y = y·P + w0·N
        //   Z = y·(X − w0·M)
        //   V = y·L
        let y_bytes = compute_y(&y_scalar, &w0);
        let (z_bytes, v_bytes) = compute_z_v_verifier(&y_scalar, &w0, &l, &pake1.x)?;

        // Transcript hash: SHA-256 over all protocol elements.
        let t_t = transcript_hash(
            &transcript_context,
            &pake1.x,
            &y_bytes,
            &z_bytes,
            &v_bytes,
            &w0,
        );

        // Split Ka (first 16 bytes) and Ke (last 16 bytes). Both are SPAKE2+
        // secrets (`ke` is the root session secret); zeroize them after their
        // last use so no copy lingers on the stack.
        let (mut ka, mut ke) = ka_ke_from_transcript(&t_t);

        // Derive KcA and KcB from Ka.
        let (kca, kcb) = derive_confirmation_keys(&ka)?;
        ka.zeroize();

        // cB = HMAC-SHA256(KcB, X) — sent to the commissioner in Pake2.
        let cb = compute_cb(&kcb, &pake1.x);

        // cA_expected = HMAC-SHA256(KcA, Y) — verified when Pake3 arrives.
        let ca_expected = compute_ca(&kca, &y_bytes);

        // Derive 48-byte session key material from Ke.
        // `build_session_keys` copies `ke` into the `ZeroizeOnDrop`
        // `PaseSessionKeys`; wipe our local copy afterwards.
        let session_keys_blob = derive_session_keys(&ke)?;
        let session_keys = build_session_keys(ke, &session_keys_blob);
        ke.zeroize();

        self.state = State::ReadyToSendPake2 {
            y_bytes,
            cb,
            ca_expected,
            session_keys,
        };
        Ok(())
    }
}

// =============================================================================
// Session-key builder (device = responder)
// =============================================================================

/// Build a [`PaseSessionKeys`] from `Ke` and the 48-byte derived key blob.
///
/// Key assignment for the device (= responder), per matter.js `NodeSession.ts`
/// with `isInitiator = false`:
/// ```ts
/// const decryptKey = isInitiator ? keys.slice(16, 32) : keys.slice(0, 16);
/// const encryptKey = isInitiator ? keys.slice(0, 16)  : keys.slice(16, 32);
/// const attestationKey = keys.slice(32, 48);
/// ```
///
/// - `i2r_key` = `decryptKey` (device decrypts incoming from commissioner).
/// - `r2i_key` = `encryptKey` (device encrypts outgoing to commissioner).
///
/// By convention, `PaseSessionKeys::i2r_key` is the initiator→responder
/// direction key. From the device's perspective:
/// - Initiator→responder (i2r): the device *decrypts* → `blob[0..16]` (because
///   `isInitiator=false` means `decryptKey = blob[0..16]`).
/// - Responder→initiator (r2i): the device *encrypts* → `blob[16..32]`.
///
/// This is the mirror of `prover.rs`'s `build_session_keys`, which assigns:
/// - `i2r_key` = `blob[0..16]` (commissioner encrypts).
/// - `r2i_key` = `blob[16..32]` (commissioner decrypts).
///
/// Both sides end up with the same `i2r_key` and `r2i_key` values.
fn build_session_keys(ke: [u8; 16], blob_48: &[u8; 48]) -> PaseSessionKeys {
    let mut i2r_key = [0u8; 16];
    let mut r2i_key = [0u8; 16];
    let mut attestation_key = [0u8; 16];

    // Device = responder (isInitiator = false):
    //   decryptKey (i2r) = blob[0..16]
    //   encryptKey (r2i) = blob[16..32]
    i2r_key.copy_from_slice(&blob_48[0..16]);
    r2i_key.copy_from_slice(&blob_48[16..32]);
    attestation_key.copy_from_slice(&blob_48[32..48]);

    PaseSessionKeys {
        ke,
        i2r_key,
        r2i_key,
        attestation_key,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::pase::messages::PbkdfParamResponse;

    /// Shared PBKDF params used across tests: valid per spec §3.10.3.
    fn test_params() -> PasePbkdfParams {
        PasePbkdfParams {
            iterations: 1_000,
            salt: vec![0x42u8; 16],
        }
    }

    /// PIN used in tests; matches the matter.js canonical test PIN.
    const TEST_PIN: u32 = 20_202_021;

    // ─── Construction ─────────────────────────────────────────────────────

    #[test]
    fn new_from_pin_accepts_valid_params() {
        let _ = PaseVerifier::new_from_pin(TEST_PIN, test_params(), 0x0033).unwrap();
    }

    #[test]
    fn new_from_pin_rejects_low_iterations() {
        let params = PasePbkdfParams {
            iterations: 999,
            salt: vec![0u8; 16],
        };
        assert!(matches!(
            PaseVerifier::new_from_pin(TEST_PIN, params, 0x0033),
            Err(Error::PbkdfIterationsTooLow(999))
        ));
    }

    #[test]
    fn new_from_pin_rejects_short_salt() {
        let params = PasePbkdfParams {
            iterations: 1_000,
            salt: vec![0u8; 15],
        };
        assert!(matches!(
            PaseVerifier::new_from_pin(TEST_PIN, params, 0x0033),
            Err(Error::PbkdfSaltLengthInvalid(15))
        ));
    }

    #[test]
    fn new_raw_rejects_invalid_w0_scalar() {
        // All-zeros is not a valid P-256 scalar (zero scalar is the identity — rejected).
        let w0_zero = [0u8; 32];
        let l = [0x04u8; 65];
        let params = test_params();
        assert!(matches!(
            PaseVerifier::new(w0_zero, l, params, 0x0033),
            Err(Error::InvalidScalar)
        ));
    }

    // ─── expected_inbound ─────────────────────────────────────────────────

    #[test]
    fn expected_inbound_after_construction_is_pbkdf_request() {
        let v = PaseVerifier::new_from_pin(TEST_PIN, test_params(), 0x0033).unwrap();
        assert_eq!(
            v.expected_inbound(),
            Some(PaseMessageKind::PbkdfParamRequest)
        );
    }

    // ─── Negotiation path state transitions ───────────────────────────────

    #[test]
    fn handle_pbkdf_request_advances_state() {
        let mut v = PaseVerifier::new_from_pin(TEST_PIN, test_params(), 0x0033).unwrap();

        // Build a minimal valid PbkdfParamRequest.
        let req = PbkdfParamRequest {
            initiator_random: [0x11u8; 32],
            initiator_session_id: 0,
            passcode_id: 0,
            has_pbkdf_parameters: false,
            initiator_session_params: None,
        };
        let req_bytes = req.encode().unwrap();

        v.handle_pbkdf_request(&req_bytes).unwrap();
        // After request, ready to send response — no inbound expected.
        assert_eq!(v.expected_inbound(), None);
    }

    #[test]
    fn next_message_after_pbkdf_request_emits_response() {
        let mut v = PaseVerifier::new_from_pin(TEST_PIN, test_params(), 0x0033).unwrap();

        let req = PbkdfParamRequest {
            initiator_random: [0x11u8; 32],
            initiator_session_id: 0,
            passcode_id: 0,
            has_pbkdf_parameters: false,
            initiator_session_params: None,
        };
        v.handle_pbkdf_request(&req.encode().unwrap()).unwrap();

        let resp_bytes = v.next_message().unwrap();
        // PbkdfParamResponse is an anonymous TLV structure.
        assert_eq!(resp_bytes[0], 0x15, "first byte must be 0x15 (anon struct)");

        // After sending response, waiting for Pake1.
        assert_eq!(v.expected_inbound(), Some(PaseMessageKind::Pake1));

        // Round-trip: the response must decode successfully.
        let decoded = PbkdfParamResponse::decode(&resp_bytes).unwrap();
        // PBKDF params must be present (commissioner said has_pbkdf_parameters=false).
        assert!(decoded.pbkdf_parameters.is_some());
        let inner = decoded.pbkdf_parameters.unwrap();
        assert_eq!(inner.iterations, 1_000);
        assert_eq!(inner.salt, vec![0x42u8; 16]);
    }

    // ─── Out-of-order rejection ───────────────────────────────────────────

    #[test]
    fn out_of_order_handle_pake3_returns_unexpected_message() {
        // Verifier is AwaitingFirstMessage; Pake3 is wrong here.
        let mut v = PaseVerifier::new_from_pin(TEST_PIN, test_params(), 0x0033).unwrap();
        let dummy_pake3 = Pake3 {
            verifier: [0x00u8; 32],
        };
        let pake3_bytes = dummy_pake3.encode().unwrap();
        assert!(matches!(
            v.handle_pake3(&pake3_bytes),
            Err(Error::UnexpectedMessage { .. })
        ));
    }

    #[test]
    fn finish_before_complete_returns_handshake_incomplete() {
        let v = PaseVerifier::new_from_pin(TEST_PIN, test_params(), 0x0033).unwrap();
        assert!(matches!(v.finish(), Err(Error::HandshakeIncomplete)));
    }

    // ─── Tag mismatch rejection ───────────────────────────────────────────

    #[test]
    fn handle_pake3_rejects_wrong_ca_tag() {
        use crate::pase::kdf::derive_w0_w1;
        use crate::pase::spake2plus::{compute_x, sample_scalar};
        use ring::rand::SystemRandom;

        let rng = SystemRandom::new();
        let params = test_params();

        // Verifier side.
        let mut v = PaseVerifier::new_from_pin(TEST_PIN, params.clone(), 0x0033).unwrap();

        // Derive prover-side values to build a plausible Pake1.
        let (w0_scalar, _w1_scalar) =
            derive_w0_w1(TEST_PIN, &params.salt, params.iterations).unwrap();
        let x_scalar = sample_scalar(&rng).unwrap();
        let x_bytes = compute_x(&x_scalar, &w0_scalar);
        let pake1_bytes = Pake1 { x: x_bytes }.encode().unwrap();

        // Drive verifier: known-params path (skip PbkdfParamRequest).
        v.handle_pake1(&pake1_bytes).unwrap();
        let _pake2_bytes = v.next_message().unwrap();

        // Send Pake3 with a wrong cA tag (all zeros — almost certainly wrong).
        let wrong_pake3 = Pake3 {
            verifier: [0x00u8; 32],
        };
        let wrong_pake3_bytes = wrong_pake3.encode().unwrap();
        assert!(matches!(
            v.handle_pake3(&wrong_pake3_bytes),
            Err(Error::ConfirmationTagMismatch)
        ));
    }

    // ─── responder_session_id propagation ────────────────────────────────

    #[test]
    fn verifier_advertises_responder_session_id() {
        let params = test_params();
        let mut verifier = PaseVerifier::new_from_pin(TEST_PIN, params, 0x0033).unwrap();
        // Drive a PBKDFParamRequest in so next_message() emits the response.
        let mut prover =
            crate::pase::prover::PaseProver::new_with_negotiation(TEST_PIN, 0x0001).unwrap();
        let req = prover.start().unwrap();
        verifier.handle_pbkdf_request(&req).unwrap();
        let resp = verifier.next_message().unwrap();
        let decoded = crate::pase::messages::PbkdfParamResponse::decode(&resp).unwrap();
        assert_eq!(decoded.responder_session_id, 0x0033);
    }

    // ─── Known-params path (Pake1 as first message) ───────────────────────

    #[test]
    fn handle_pake1_as_first_message_succeeds() {
        use crate::pase::kdf::derive_w0_w1;
        use crate::pase::spake2plus::{compute_x, sample_scalar};
        use ring::rand::SystemRandom;

        let rng = SystemRandom::new();
        let params = test_params();
        let mut v = PaseVerifier::new_from_pin(TEST_PIN, params.clone(), 0x0033).unwrap();

        let (w0_scalar, _) = derive_w0_w1(TEST_PIN, &params.salt, params.iterations).unwrap();
        let x_scalar = sample_scalar(&rng).unwrap();
        let x_bytes = compute_x(&x_scalar, &w0_scalar);
        let pake1_bytes = Pake1 { x: x_bytes }.encode().unwrap();

        // Feed Pake1 directly — no PbkdfParamRequest.
        v.handle_pake1(&pake1_bytes).unwrap();
        // Ready to send Pake2.
        assert_eq!(v.expected_inbound(), Some(PaseMessageKind::Pake3));

        let pake2_bytes = v.next_message().unwrap();
        assert_eq!(pake2_bytes[0], 0x15, "Pake2 must be anon TLV structure");

        let decoded = Pake2::decode(&pake2_bytes).unwrap();
        assert_eq!(
            decoded.y[0], 0x04,
            "Y must have SEC1 uncompressed prefix 0x04"
        );
        assert_eq!(decoded.verifier.len(), 32);
    }

    // ─── `State`'s Debug names the variant only ───────────────────────────
    //
    // Most `State` variants hold secrets (`w0`, `L`, the ephemeral scalar `y`,
    // confirmation tags, session keys). Nothing formats `State` today, but a
    // future `#[derive(Debug)]` on the public state machine would print it, so
    // `State`'s own `Debug` prints the variant name and nothing else. Every
    // check compares the whole rendering against the bare name, so no field can
    // be present. Static messages only: a failing assertion never prints the
    // state.

    /// Every `Debug` flavour of `state` is exactly `name`.
    fn assert_state_shows_only(state: &State, name: &str) {
        for shown in [
            format!("{state:?}"),
            format!("{state:#?}"),
            format!("{state:x?}"),
            format!("{state:X?}"),
            format!("{state:#x?}"),
            format!("{state:#X?}"),
        ] {
            assert!(
                shown == name,
                "State's Debug prints more than its variant name"
            );
        }
    }

    #[test]
    fn state_debug_names_the_variant_only() {
        let verifier = PaseVerifier::new_from_pin(TEST_PIN, test_params(), 0x0033).unwrap();
        assert_state_shows_only(&verifier.state, "AwaitingFirstMessage");
        assert_state_shows_only(&State::Poisoned, "Poisoned");
    }
}
