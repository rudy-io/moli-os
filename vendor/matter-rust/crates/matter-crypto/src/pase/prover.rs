//! Commissioner-side PASE state machine.
//!
//! Drives the 5-message PASE handshake (or 3-message known-params path)
//! from the commissioner's perspective. Sans-IO: the caller feeds raw TLV
//! bytes in and gets raw TLV bytes out; no sockets, no async, no I/O.
//!
//! # Protocol flow (negotiation path — Matter Core Spec §3.10.5)
//!
//! ```text
//! Commissioner (Prover)          Device (Verifier)
//! ───────────────────────────────────────────────
//! start()
//!   → PBKDFParamRequest  ────────────────────────>
//!                        <──────── PBKDFParamResponse
//! handle_pbkdf_response()
//! next_message()  [Pake1]
//!   → Pake1              ────────────────────────>
//!                        <──────── Pake2
//! handle_pake2()
//! next_message()  [Pake3]
//!   → Pake3              ────────────────────────>
//!                        <──────── StatusReport: Success
//! finish() → PaseSessionKeys
//! ```
//!
//! # Known-params path
//!
//! When the commissioner already has the PBKDF params cached it uses
//! `new_with_known_params`, which skips to Pake1 directly:
//!
//! ```text
//! start()  → Pake1  →  handle_pake2()  →  next_message() [Pake3]
//! ```
//!
//! # Transcript context composition (matter.js pin)
//!
//! `PaseClient.ts` line 89:
//! ```ts
//! context = await crypto.computeHash([SPAKE_CONTEXT, requestPayload, responsePayload])
//! ```
//! where `SPAKE_CONTEXT = Bytes.fromString("CHIP PAKE V1 Commissioning")`.
//! `computeHash([...])` concatenates the arrays and SHA-256-hashes the result,
//! so `context = SHA-256("CHIP PAKE V1 Commissioning" || pbkdfReq_bytes || pbkdfResp_bytes)`.
//!
//! For the known-params path there is no `PBKDFParam` exchange; matter.js does not
//! implement this path in `PaseClient.ts` (it always negotiates). We follow the
//! same SHA-256 construction but with zero extra bytes:
//! `context = SHA-256("CHIP PAKE V1 Commissioning")`.
//! This will be verified in M3.3 if/when the known-params path is tested against
//! matter.js.

use ring::rand::{SecureRandom, SystemRandom};

use crate::error::{Error, Result};
use crate::pase::kdf::{derive_w0_w1, validate_params};
use crate::pase::messages::{Pake1, Pake2, Pake3, PbkdfParamRequest, PbkdfParamResponse};
use crate::pase::spake2plus::{
    compute_ca, compute_cb, compute_x, compute_z_v_prover, derive_confirmation_keys,
    derive_session_keys, hash_context, ka_ke_from_transcript, sample_scalar, transcript_hash,
    verify_tag,
};
use crate::pase::{PaseMessageKind, PasePbkdfParams, PaseSessionKeys};
use zeroize::Zeroize;

// =============================================================================
// Internal state enum
// =============================================================================

/// Internal states of the commissioner-side PASE handshake.
///
/// Each variant corresponds to a point in the protocol flow defined in
/// Matter Core Spec §3.10.5. Named for the *next action* the state machine
/// expects or is ready to perform.
enum State {
    /// `start()` has not been called yet — negotiation path.
    ///
    /// Holds the pre-sampled x scalar and nonce so that `start()` is
    /// infallible after construction succeeds.
    AwaitingStartNegotiation {
        pin: u32,
        x_scalar: p256::Scalar,
        initiator_random: [u8; 32],
        /// Session ID to include in the `PBKDFParamRequest`. Caller-supplied,
        /// passed through from `new_with_negotiation` (or a fixture value via
        /// the test-only `new_with_negotiation_with_scalar_and_session_id`).
        initiator_session_id: u16,
    },

    /// `start()` has not been called yet — known-params path.
    AwaitingStartKnownParams {
        pin: u32,
        params: PasePbkdfParams,
        x_scalar: p256::Scalar,
    },

    /// `start()` sent `PBKDFParamRequest`; waiting for `PBKDFParamResponse`.
    ///
    /// `sent_request_bytes` is the verbatim TLV bytes of the request we sent,
    /// needed to compose the transcript context hash.
    AwaitingPbkdfResponse {
        pin: u32,
        x_scalar: p256::Scalar,
        sent_request_bytes: Vec<u8>,
    },

    /// `handle_pbkdf_response()` has processed the response; `next_message()`
    /// will derive w0/w1, compute X, and emit Pake1.
    ReadyToSendPake1 {
        pin: u32,
        params: PasePbkdfParams,
        x_scalar: p256::Scalar,
        /// SHA-256 context hash: SHA-256(SPAKE_CONTEXT || pbkdfReq || pbkdfResp).
        transcript_context: [u8; 32],
    },

    /// Pake1 sent; waiting for Pake2.
    AwaitingPake2 {
        w0: p256::Scalar,
        w1: p256::Scalar,
        x_scalar: p256::Scalar,
        x_bytes: [u8; 65],
        /// SHA-256 context hash to pass into `transcript_hash`.
        transcript_context: [u8; 32],
    },

    /// Pake2 verified; `next_message()` will emit Pake3.
    ReadyToSendPake3 {
        /// Our cA confirmation tag, already computed.
        ca: [u8; 32],
        session_keys: PaseSessionKeys,
    },

    /// Pake3 sent; `finish()` may be called.
    Complete { session_keys: PaseSessionKeys },

    /// Sentinel used during `std::mem::replace` state transitions.
    ///
    /// This variant is **never observable** to callers: every `mem::replace`
    /// immediately replaces `Poisoned` with the next real state, or returns
    /// an error before storing it. If somehow reached, all methods return
    /// `Error::HandshakeIncomplete`.
    Poisoned,
}

/// Names the variant only. The PIN, `w0`/`w1` and the ephemeral scalar `x`
/// live in these variants' fields, and a derived `Debug` would print all of
/// them (p256's `Scalar` prints as `Scalar(0x…)`). The session keys
/// already print redacted. Nothing needs any of them in a log, so none are
/// printed. Written by hand, not derived, so that a `Debug` added to the
/// public state machine later cannot print them either.
impl core::fmt::Debug for State {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::AwaitingStartNegotiation { .. } => "AwaitingStartNegotiation",
            Self::AwaitingStartKnownParams { .. } => "AwaitingStartKnownParams",
            Self::AwaitingPbkdfResponse { .. } => "AwaitingPbkdfResponse",
            Self::ReadyToSendPake1 { .. } => "ReadyToSendPake1",
            Self::AwaitingPake2 { .. } => "AwaitingPake2",
            Self::ReadyToSendPake3 { .. } => "ReadyToSendPake3",
            Self::Complete { .. } => "Complete",
            Self::Poisoned => "Poisoned",
        })
    }
}

// =============================================================================
// PaseProver
// =============================================================================

/// Commissioner-side PASE state machine.
///
/// Drives the SPAKE2+ handshake from the commissioner's (initiator's)
/// perspective. Sans-IO: the caller is responsible for transmitting and
/// receiving bytes.
///
/// # Construction
///
/// - [`PaseProver::new_with_negotiation`] — sends `PBKDFParamRequest` first
///   (the normal path when PBKDF params are not cached).
/// - [`PaseProver::new_with_known_params`] — skips negotiation; first message
///   is Pake1 (when PBKDF params are already known from a prior session).
///
/// # Driving the handshake
///
/// 1. Call [`start`][Self::start] to get the first outbound message bytes.
/// 2. Feed inbound bytes into [`handle_pbkdf_response`][Self::handle_pbkdf_response]
///    (negotiation path) or skip to step 3 (known-params path).
/// 3. Call [`next_message`][Self::next_message] to get Pake1 bytes.
/// 4. Feed inbound Pake2 bytes into [`handle_pake2`][Self::handle_pake2].
/// 5. Call [`next_message`][Self::next_message] to get Pake3 bytes.
/// 6. After the peer confirms success, call [`finish`][Self::finish] to
///    retrieve the [`PaseSessionKeys`].
///
/// Use [`expected_inbound`][Self::expected_inbound] at any point to query
/// which message type the machine is currently waiting for.
pub struct PaseProver {
    state: State,
    /// The responder's advertised session id, captured from
    /// `PBKDFParamResponse`. `None` until [`Self::handle_pbkdf_response`]
    /// is called (and always `None` on the known-params path, which
    /// exchanges no PBKDF messages).
    responder_session_id: Option<u16>,
}

impl PaseProver {
    // ─── Public constructors ──────────────────────────────────────────────

    /// Construct a prover that negotiates PBKDF parameters (sends
    /// `PBKDFParamRequest` first).
    ///
    /// `initiator_session_id` is the non-zero secured-session id this
    /// commissioner advertises for the peer to address us by. It is included
    /// in the `PBKDFParamRequest` wire message and hashed into the SPAKE2+
    /// transcript, so it must be fixed before [`start`][Self::start] is called.
    ///
    /// Pre-samples the SPAKE2+ `x` scalar and the 32-byte initiator nonce
    /// so that [`start`][Self::start] cannot fail due to randomness.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidScalar`] if the CSPRNG is broken and a non-zero
    ///   scalar cannot be sampled after 16 attempts (practically impossible).
    /// - [`Error::PinDerivationFailed`] if the nonce fill fails.
    pub fn new_with_negotiation(pin: u32, initiator_session_id: u16) -> Result<Self> {
        let rng = SystemRandom::new();
        Self::new_with_negotiation_using_rng(pin, initiator_session_id, &rng)
    }

    /// Deterministic constructor for testing — accepts an injectable RNG.
    ///
    /// Production code should always use [`new_with_negotiation`][Self::new_with_negotiation].
    pub(crate) fn new_with_negotiation_using_rng(
        pin: u32,
        initiator_session_id: u16,
        rng: &dyn SecureRandom,
    ) -> Result<Self> {
        let x_scalar = sample_scalar(rng)?;
        let mut initiator_random = [0u8; 32];
        rng.fill(&mut initiator_random)
            .map_err(|_| Error::PinDerivationFailed)?;
        Ok(Self {
            state: State::AwaitingStartNegotiation {
                pin,
                x_scalar,
                initiator_random,
                initiator_session_id,
            },
            responder_session_id: None,
        })
    }

    // Deterministic-input constructors below are used only by
    // `crate::test_support`, which is itself behind the `test-support`
    // feature. Gated to match, so a default-feature build does not compile
    // them as dead code.
    /// Deterministic constructor for testing — injects fixed `x` scalar and
    /// `initiator_random` bytes directly, bypassing the RNG.
    ///
    /// Used by `test_support` to construct a prover with known values for
    /// matter.js byte-parity tests. `x_scalar_bytes` must be a valid non-zero
    /// P-256 scalar in big-endian representation.
    ///
    /// Production code should always use [`new_with_negotiation`][Self::new_with_negotiation].
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidScalar`] if `x_scalar_bytes` is zero or not a valid
    ///   P-256 scalar (i.e., ≥ curve order).
    #[cfg(feature = "test-support")]
    pub(crate) fn new_with_negotiation_with_scalar(
        pin: u32,
        x_scalar_bytes: [u8; 32],
        initiator_random: [u8; 32],
    ) -> Result<Self> {
        Self::new_with_negotiation_with_scalar_and_session_id(
            pin,
            x_scalar_bytes,
            initiator_random,
            0,
        )
    }

    /// Deterministic constructor for testing — injects fixed `x` scalar,
    /// `initiator_random`, and `initiator_session_id` directly.
    ///
    /// Used by `test_support` to reproduce matter.js fixture handshakes that
    /// use a specific session ID in `PBKDFParamRequest`. Production code always
    /// uses [`new_with_negotiation`][Self::new_with_negotiation].
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidScalar`] if `x_scalar_bytes` is zero or not a valid
    ///   P-256 scalar (i.e., ≥ curve order).
    #[cfg(feature = "test-support")]
    pub(crate) fn new_with_negotiation_with_scalar_and_session_id(
        pin: u32,
        x_scalar_bytes: [u8; 32],
        initiator_random: [u8; 32],
        initiator_session_id: u16,
    ) -> Result<Self> {
        use p256::elliptic_curve::group::ff::{Field, PrimeField};
        let x_scalar_opt: Option<p256::Scalar> =
            p256::Scalar::from_repr(p256::FieldBytes::from(x_scalar_bytes)).into();
        let x_scalar = x_scalar_opt.ok_or(Error::InvalidScalar)?;
        if bool::from(x_scalar.is_zero()) {
            return Err(Error::InvalidScalar);
        }
        Ok(Self {
            state: State::AwaitingStartNegotiation {
                pin,
                x_scalar,
                initiator_random,
                initiator_session_id,
            },
            responder_session_id: None,
        })
    }

    /// Construct a prover with PBKDF parameters already known (skips negotiation;
    /// first message is Pake1).
    ///
    /// Validates `params` against Matter spec §3.10.3 bounds before accepting.
    ///
    /// `initiator_session_id` is accepted for API symmetry with
    /// [`new_with_negotiation`][Self::new_with_negotiation] but is **unused** on
    /// this path: the known-params flow sends no `PBKDFParamRequest`, so the id
    /// never reaches the wire or the transcript, and
    /// [`responder_session_id`][Self::responder_session_id] will always be
    /// `None`. (Secured-session-id negotiation requires the negotiation path,
    /// which the commissioning driver uses.)
    ///
    /// # Errors
    ///
    /// - [`Error::PbkdfIterationsTooLow`] if `params.iterations < 1000`.
    /// - [`Error::PbkdfSaltLengthInvalid`] if `params.salt.len()` ∉ \[16, 32\].
    /// - [`Error::InvalidScalar`] if the CSPRNG is broken.
    pub fn new_with_known_params(
        pin: u32,
        params: PasePbkdfParams,
        initiator_session_id: u16,
    ) -> Result<Self> {
        validate_params(params.iterations, &params.salt)?;
        let rng = SystemRandom::new();
        Self::new_with_known_params_using_rng(pin, params, initiator_session_id, &rng)
    }

    /// Deterministic constructor for testing — accepts an injectable RNG.
    ///
    /// Production code should always use
    /// [`new_with_known_params`][Self::new_with_known_params].
    pub(crate) fn new_with_known_params_using_rng(
        pin: u32,
        params: PasePbkdfParams,
        _initiator_session_id: u16,
        rng: &dyn SecureRandom,
    ) -> Result<Self> {
        validate_params(params.iterations, &params.salt)?;
        let x_scalar = sample_scalar(rng)?;
        // _initiator_session_id is not used on the known-params path (no
        // PBKDFParamRequest is sent), but we accept it for API symmetry.
        Ok(Self {
            state: State::AwaitingStartKnownParams {
                pin,
                params,
                x_scalar,
            },
            responder_session_id: None,
        })
    }

    /// Deterministic constructor for testing — injects a fixed `x` scalar
    /// directly, bypassing the RNG.
    ///
    /// Used by `test_support` to construct a prover with a known scalar for
    /// matter.js byte-parity tests. `x_scalar_bytes` must be a valid non-zero
    /// P-256 scalar in big-endian representation.
    ///
    /// Production code should always use
    /// [`new_with_known_params`][Self::new_with_known_params].
    ///
    /// # Errors
    ///
    /// - [`Error::PbkdfIterationsTooLow`] if `params.iterations < 1000`.
    /// - [`Error::PbkdfSaltLengthInvalid`] if `params.salt.len()` ∉ \[16, 32\].
    /// - [`Error::InvalidScalar`] if `x_scalar_bytes` is zero or not a valid
    ///   P-256 scalar.
    #[cfg(feature = "test-support")]
    pub(crate) fn new_with_known_params_with_scalar(
        pin: u32,
        params: PasePbkdfParams,
        x_scalar_bytes: [u8; 32],
    ) -> Result<Self> {
        use p256::elliptic_curve::group::ff::{Field, PrimeField};
        validate_params(params.iterations, &params.salt)?;
        let x_scalar_opt: Option<p256::Scalar> =
            p256::Scalar::from_repr(p256::FieldBytes::from(x_scalar_bytes)).into();
        let x_scalar = x_scalar_opt.ok_or(Error::InvalidScalar)?;
        if bool::from(x_scalar.is_zero()) {
            return Err(Error::InvalidScalar);
        }
        Ok(Self {
            state: State::AwaitingStartKnownParams {
                pin,
                params,
                x_scalar,
            },
            responder_session_id: None,
        })
    }

    // ─── State inspection ─────────────────────────────────────────────────

    /// Returns the message kind the state machine is currently waiting to
    /// receive, or `None` if the machine is in an outbound-only state
    /// (waiting to emit a message) or has completed / been poisoned.
    pub fn expected_inbound(&self) -> Option<PaseMessageKind> {
        match &self.state {
            State::AwaitingPbkdfResponse { .. } => Some(PaseMessageKind::PbkdfParamResponse),
            State::AwaitingPake2 { .. } => Some(PaseMessageKind::Pake2),
            _ => None,
        }
    }

    /// The responder's advertised secured-session id, captured from
    /// `PBKDFParamResponse`. `None` before [`Self::handle_pbkdf_response`]
    /// (and on the known-params path, which exchanges no PBKDF messages).
    #[must_use]
    pub fn responder_session_id(&self) -> Option<u16> {
        self.responder_session_id
    }

    // ─── Handshake methods ────────────────────────────────────────────────

    /// Produce the first outbound message.
    ///
    /// - Negotiation path: emits `PBKDFParamRequest` TLV bytes.
    /// - Known-params path: derives w0/w1, computes X, emits Pake1 TLV bytes.
    ///
    /// May only be called once, from the initial state. Repeated calls or
    /// calls from any later state return [`Error::UnexpectedMessage`].
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedMessage`] if called from the wrong state.
    /// - [`Error::Codec`] on TLV encoding failure.
    /// - [`Error::PinDerivationFailed`] / [`Error::PbkdfIterationsTooLow`] /
    ///   [`Error::PbkdfSaltLengthInvalid`] on KDF failure (known-params path).
    pub fn start(&mut self) -> Result<Vec<u8>> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::AwaitingStartNegotiation {
                pin,
                x_scalar,
                initiator_random,
                initiator_session_id,
            } => {
                // §3.10.5 step 1: commissioner sends PBKDFParamRequest.
                // passcode_id=0 per spec defaults for the negotiation path.
                // initiator_session_id is the caller-supplied value (the
                // secured-session id we advertise for the peer to address us by).
                let req = PbkdfParamRequest {
                    initiator_random,
                    initiator_session_id,
                    passcode_id: 0,
                    has_pbkdf_parameters: false,
                    initiator_session_params: None,
                };
                let bytes = req.encode()?;
                self.state = State::AwaitingPbkdfResponse {
                    pin,
                    x_scalar,
                    sent_request_bytes: bytes.clone(),
                };
                Ok(bytes)
            }

            State::AwaitingStartKnownParams {
                pin,
                params,
                x_scalar,
            } => {
                // §3.10.5 known-params shortcut: skip param negotiation.
                // context = SHA-256("CHIP PAKE V1 Commissioning") — no pbkdfReq/Resp to fold in.
                let transcript_context = hash_context(&[]);
                let (w0, w1) = derive_w0_w1(pin, &params.salt, params.iterations)?;
                let x_bytes = compute_x(&x_scalar, &w0);
                let pake1_bytes = Pake1 { x: x_bytes }.encode()?;
                self.state = State::AwaitingPake2 {
                    w0,
                    w1,
                    x_scalar,
                    x_bytes,
                    transcript_context,
                };
                Ok(pake1_bytes)
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

    /// Process an inbound `PBKDFParamResponse` message.
    ///
    /// Decodes the response, validates the PBKDF parameters, and composes
    /// the transcript context as `SHA-256(SPAKE_CONTEXT || pbkdfReq || pbkdfResp)`.
    ///
    /// After this call, [`next_message`][Self::next_message] emits Pake1.
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedMessage`] if called from the wrong state.
    /// - [`Error::InvalidParameter`] if the response is malformed or missing
    ///   the required `pbkdf_parameters` field.
    /// - [`Error::PbkdfIterationsTooLow`] / [`Error::PbkdfIterationsTooHigh`] /
    ///   [`Error::PbkdfSaltLengthInvalid`] if the responder's parameters are
    ///   out of spec. The too-high case caps a peer-supplied iteration count
    ///   to prevent a commissioner CPU denial-of-service.
    /// - [`Error::Codec`] on TLV decoding failure.
    pub fn handle_pbkdf_response(&mut self, bytes: &[u8]) -> Result<()> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::AwaitingPbkdfResponse {
                pin,
                x_scalar,
                sent_request_bytes,
            } => {
                // §3.10.5 step 2: decode and validate the response.
                let resp = PbkdfParamResponse::decode(bytes)?;

                // Capture the responder's advertised session id so callers can
                // retrieve it via `responder_session_id()`.
                self.responder_session_id = Some(resp.responder_session_id);

                // The responder MUST include pbkdf_parameters when we set
                // has_pbkdf_parameters=false (§3.10.5). If absent, abort.
                let params_inner = resp.pbkdf_parameters.ok_or(Error::InvalidParameter)?;
                let params = PasePbkdfParams {
                    iterations: params_inner.iterations,
                    salt: params_inner.salt,
                };
                validate_params(params.iterations, &params.salt)?;

                // §3.10.5 — compose transcript context.
                // matter.js PaseClient.ts line 89:
                //   context = SHA-256("CHIP PAKE V1 Commissioning" || pbkdfReq_bytes || pbkdfResp_bytes)
                let transcript_context = hash_context(&[&sent_request_bytes, bytes]);

                self.state = State::ReadyToSendPake1 {
                    pin,
                    params,
                    x_scalar,
                    transcript_context,
                };
                Ok(())
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

    /// Process an inbound `Pake2` message.
    ///
    /// Performs the SPAKE2+ cryptographic operations:
    /// 1. Decode Y from the Pake2 TLV.
    /// 2. Compute Z and V (the shared point values).
    /// 3. Compute the transcript hash `TT_HASH`.
    /// 4. Derive confirmation keys `KcA`, `KcB`.
    /// 5. Verify the device's confirmation tag `cB` in constant time via
    ///    `verify_tag` (subtle CT-EQ, never `==`).
    /// 6. Compute our confirmation tag `cA`.
    /// 7. Derive session keys.
    ///
    /// After this call, [`next_message`][Self::next_message] emits Pake3.
    ///
    /// # Security
    ///
    /// Tag comparison at step 5 MUST be constant-time. This is enforced by
    /// routing through `verify_tag` (in `pase::spake2plus`) which uses `subtle::ConstantTimeEq`.
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedMessage`] if called from the wrong state.
    /// - [`Error::InvalidParameter`] if Y is not a valid P-256 point.
    /// - [`Error::ConfirmationTagMismatch`] if the device's `cB` tag fails
    ///   constant-time verification (wrong PIN or compromised peer).
    /// - [`Error::Codec`] on TLV decoding failure.
    /// - [`Error::PinDerivationFailed`] on HKDF failure.
    // kca / kcb are the SPAKE2+ confirmation key names from Matter
    // Core Spec §3.10 (RFC 9383 §3.3). Renaming would impair
    // verification against the spec.
    #[allow(clippy::similar_names)]
    pub fn handle_pake2(&mut self, bytes: &[u8]) -> Result<()> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::AwaitingPake2 {
                w0,
                w1,
                x_scalar,
                x_bytes,
                transcript_context,
            } => {
                let pake2 = Pake2::decode(bytes)?;

                // §3.10.5 — commissioner side:
                //   Z = x · (Y − w0·N)
                //   V = w1 · (Y − w0·N)
                let (z_bytes, v_bytes) = compute_z_v_prover(&x_scalar, &w0, &w1, &pake2.y)?;

                // Transcript hash: SHA-256 over all protocol elements.
                // context is already SHA-256(SPAKE_CONTEXT || pbkdfReq || pbkdfResp).
                let t_t = transcript_hash(
                    &transcript_context,
                    &x_bytes,
                    &pake2.y,
                    &z_bytes,
                    &v_bytes,
                    &w0,
                );

                // Split Ka (first 16) and Ke (last 16). Both are SPAKE2+ secrets
                // (`ke` is the root session secret); zeroize them after their last
                // use so no copy lingers on the stack.
                let (mut ka, mut ke) = ka_ke_from_transcript(&t_t);

                // Derive KcA and KcB from Ka.
                let (kca, kcb) = derive_confirmation_keys(&ka)?;
                ka.zeroize();

                // Verify the device's confirmation tag cB = HMAC-SHA256(KcB, X).
                // MUST use constant-time comparison.
                let cb_expected = compute_cb(&kcb, &x_bytes);
                verify_tag(&cb_expected, &pake2.verifier)?;

                // Compute our confirmation tag cA = HMAC-SHA256(KcA, Y).
                let ca = compute_ca(&kca, &pake2.y);

                // Derive 48-byte session key material from Ke.
                // `build_session_keys` copies `ke` into the `ZeroizeOnDrop`
                // `PaseSessionKeys`; wipe our local copy afterwards.
                let session_keys_blob = derive_session_keys(&ke)?;
                let session_keys = build_session_keys(ke, &session_keys_blob);
                ke.zeroize();

                self.state = State::ReadyToSendPake3 { ca, session_keys };
                Ok(())
            }

            other => {
                self.state = other;
                Err(Error::UnexpectedMessage {
                    expected: PaseMessageKind::Pake2,
                    got: PaseMessageKind::Pake2,
                })
            }
        }
    }

    /// Produce the next outbound message.
    ///
    /// - After [`handle_pbkdf_response`][Self::handle_pbkdf_response]: emits Pake1.
    /// - After [`handle_pake2`][Self::handle_pake2]: emits Pake3.
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedMessage`] if called from the wrong state.
    /// - [`Error::Codec`] on TLV encoding failure.
    /// - [`Error::PinDerivationFailed`] / [`Error::PbkdfIterationsTooLow`] /
    ///   [`Error::PbkdfSaltLengthInvalid`] on KDF failure (Pake1 path only).
    pub fn next_message(&mut self) -> Result<Vec<u8>> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::ReadyToSendPake1 {
                pin,
                params,
                x_scalar,
                transcript_context,
            } => {
                // §3.10.5 step 3: derive w0/w1 from PIN, compute X, send Pake1.
                let (w0, w1) = derive_w0_w1(pin, &params.salt, params.iterations)?;
                let x_bytes = compute_x(&x_scalar, &w0);
                let pake1_bytes = Pake1 { x: x_bytes }.encode()?;
                self.state = State::AwaitingPake2 {
                    w0,
                    w1,
                    x_scalar,
                    x_bytes,
                    transcript_context,
                };
                Ok(pake1_bytes)
            }

            State::ReadyToSendPake3 { ca, session_keys } => {
                // §3.10.5 step 5: send our confirmation tag cA in Pake3.
                let pake3_bytes = Pake3 { verifier: ca }.encode()?;
                self.state = State::Complete { session_keys };
                Ok(pake3_bytes)
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

    /// Finalise the session and retrieve the derived session keys.
    ///
    /// May only be called after [`next_message`][Self::next_message] has
    /// emitted Pake3 (i.e., the state machine is in the `Complete` state).
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

/// Build a [`PaseSessionKeys`] from `Ke` and the 48-byte derived key blob.
///
/// Key assignment for commissioner (= initiator), per matter.js `NodeSession.ts`:
/// ```ts
/// const decryptKey = isInitiator ? keys.slice(16, 32) : keys.slice(0, 16);
/// const encryptKey = isInitiator ? keys.slice(0, 16)  : keys.slice(16, 32);
/// const attestationKey = keys.slice(32, 48);
/// ```
///
/// - `i2r_key` = `encryptKey` (commissioner encrypts for the device).
/// - `r2i_key` = `decryptKey` (commissioner decrypts incoming from device).
fn build_session_keys(ke: [u8; 16], blob_48: &[u8; 48]) -> PaseSessionKeys {
    let mut i2r_key = [0u8; 16];
    let mut r2i_key = [0u8; 16];
    let mut attestation_key = [0u8; 16];

    // Commissioner = initiator:
    //   encryptKey (i2r) = blob[0..16]
    //   decryptKey (r2i) = blob[16..32]
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
    use crate::pase::messages::{PbkdfParamResponse, PbkdfParamsInner};

    // ─── Construction ─────────────────────────────────────────────────────

    #[test]
    fn new_with_negotiation_accepts_any_pin() {
        let _ = PaseProver::new_with_negotiation(20_202_021, 0x0001).unwrap();
        let _ = PaseProver::new_with_negotiation(0, 0x0001).unwrap();
        let _ = PaseProver::new_with_negotiation(u32::MAX, 0x0001).unwrap();
    }

    #[test]
    fn new_with_known_params_rejects_low_iterations() {
        let params = PasePbkdfParams {
            iterations: 999,
            salt: vec![0u8; 16],
        };
        assert!(matches!(
            PaseProver::new_with_known_params(20_202_021, params, 0x0001),
            Err(Error::PbkdfIterationsTooLow(999))
        ));
    }

    #[test]
    fn new_with_known_params_rejects_short_salt() {
        let params = PasePbkdfParams {
            iterations: 1_000,
            salt: vec![0u8; 15],
        };
        assert!(matches!(
            PaseProver::new_with_known_params(20_202_021, params, 0x0001),
            Err(Error::PbkdfSaltLengthInvalid(15))
        ));
    }

    #[test]
    fn new_with_known_params_accepts_valid_params() {
        let params = PasePbkdfParams {
            iterations: 1_000,
            salt: vec![0x42u8; 16],
        };
        let _ = PaseProver::new_with_known_params(20_202_021, params, 0x0001).unwrap();
    }

    // ─── Negotiation path state transitions ───────────────────────────────

    #[test]
    fn start_negotiation_emits_tlv_structure() {
        let mut prover = PaseProver::new_with_negotiation(20_202_021, 0x0001).unwrap();
        let bytes = prover.start().unwrap();
        // An anonymous TLV structure starts with 0x15 (type=Structure, tag=Anonymous).
        assert_eq!(
            bytes[0], 0x15,
            "first byte must be anonymous structure tag 0x15"
        );
        assert_ne!(bytes, b"");
    }

    #[test]
    fn expected_inbound_after_start_negotiation_is_pbkdf_response() {
        let mut prover = PaseProver::new_with_negotiation(20_202_021, 0x0001).unwrap();
        let _ = prover.start().unwrap();
        assert_eq!(
            prover.expected_inbound(),
            Some(PaseMessageKind::PbkdfParamResponse)
        );
    }

    #[test]
    fn handle_pbkdf_response_advances_to_ready_to_send_pake1() {
        let mut prover = PaseProver::new_with_negotiation(20_202_021, 0x0001).unwrap();
        let _req_bytes = prover.start().unwrap();

        // Build a minimal valid PBKDFParamResponse with pbkdf_parameters.
        let resp = PbkdfParamResponse {
            initiator_random: [0x42u8; 32],
            responder_random: [0x11u8; 32],
            responder_session_id: 1,
            pbkdf_parameters: Some(PbkdfParamsInner {
                iterations: 1_000,
                salt: vec![0xABu8; 16],
            }),
            responder_session_params: None,
        };
        let resp_bytes = resp.encode().unwrap();

        // The function succeeds.
        prover.handle_pbkdf_response(&resp_bytes).unwrap();
        // Now the prover is ready to send Pake1 — no inbound expected yet.
        assert_eq!(prover.expected_inbound(), None);
    }

    #[test]
    fn handle_pbkdf_response_rejects_missing_pbkdf_params() {
        let mut prover = PaseProver::new_with_negotiation(20_202_021, 0x0001).unwrap();
        let _ = prover.start().unwrap();

        // Response without pbkdf_parameters should fail.
        let resp = PbkdfParamResponse {
            initiator_random: [0x42u8; 32],
            responder_random: [0x11u8; 32],
            responder_session_id: 1,
            pbkdf_parameters: None, // missing!
            responder_session_params: None,
        };
        let resp_bytes = resp.encode().unwrap();
        assert!(matches!(
            prover.handle_pbkdf_response(&resp_bytes),
            Err(Error::InvalidParameter)
        ));
    }

    #[test]
    fn next_message_after_pbkdf_response_emits_pake1() {
        let mut prover = PaseProver::new_with_negotiation(20_202_021, 0x0001).unwrap();
        let _ = prover.start().unwrap();

        let resp = PbkdfParamResponse {
            initiator_random: [0x42u8; 32],
            responder_random: [0x11u8; 32],
            responder_session_id: 1,
            pbkdf_parameters: Some(PbkdfParamsInner {
                iterations: 1_000,
                salt: vec![0xABu8; 16],
            }),
            responder_session_params: None,
        };
        prover
            .handle_pbkdf_response(&resp.encode().unwrap())
            .unwrap();

        let pake1_bytes = prover.next_message().unwrap();
        // Pake1 is an anonymous structure starting with 0x15.
        assert_eq!(pake1_bytes[0], 0x15);
        // After Pake1, we're awaiting Pake2.
        assert_eq!(prover.expected_inbound(), Some(PaseMessageKind::Pake2));
    }

    // ─── Terminal state guards ────────────────────────────────────────────

    #[test]
    fn finish_before_complete_returns_handshake_incomplete() {
        let prover = PaseProver::new_with_negotiation(20_202_021, 0x0001).unwrap();
        assert!(matches!(prover.finish(), Err(Error::HandshakeIncomplete)));
    }

    #[test]
    fn out_of_order_handle_pake2_returns_unexpected_message() {
        // The prover is in AwaitingStartNegotiation — handle_pake2 is wrong here.
        let mut prover = PaseProver::new_with_negotiation(20_202_021, 0x0001).unwrap();
        // Build plausible Pake2 bytes (will be rejected at state check, not decoding).
        let dummy_pake2 = Pake2 {
            y: [0x04u8; 65],
            verifier: [0x00u8; 32],
        };
        let pake2_bytes = dummy_pake2.encode().unwrap();
        assert!(matches!(
            prover.handle_pake2(&pake2_bytes),
            Err(Error::UnexpectedMessage { .. })
        ));
    }

    // ─── hash_context (now lives in spake2plus; tested there) ────────────
    //
    // The three hash_context tests have moved to `spake2plus::tests` since
    // the function itself moved there. We reference it through the import at
    // the top of this file (`use crate::pase::spake2plus::hash_context`) only
    // in the production call sites; tests live with the implementation.

    // ─── Session ID plumbing ──────────────────────────────────────────────

    #[test]
    fn prover_advertises_initiator_session_id_and_starts_unknowing_responder_id() {
        let mut prover = PaseProver::new_with_negotiation(20_202_021, 0x0011).unwrap();
        assert_eq!(prover.responder_session_id(), None);

        let req = prover.start().unwrap();
        let decoded_req = crate::pase::messages::PbkdfParamRequest::decode(&req).unwrap();
        assert_eq!(decoded_req.initiator_session_id, 0x0011);

        assert_eq!(prover.responder_session_id(), None);
    }

    // ─── `State`'s Debug names the variant only ───────────────────────────
    //
    // Most `State` variants hold secrets (the PIN, `w0`/`w1`, the ephemeral
    // scalar `x`, session keys). Nothing formats `State` today, but a future
    // `#[derive(Debug)]` on the public state machine would print it, so
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
        let prover = PaseProver::new_with_negotiation(20_202_021, 0x0001).unwrap();
        assert_state_shows_only(&prover.state, "AwaitingStartNegotiation");

        let params = PasePbkdfParams {
            iterations: 1_000,
            salt: vec![0x42u8; 16],
        };
        let prover = PaseProver::new_with_known_params(20_202_021, params, 0x0001).unwrap();
        assert_state_shows_only(&prover.state, "AwaitingStartKnownParams");

        assert_state_shows_only(&State::Poisoned, "Poisoned");
    }
}
