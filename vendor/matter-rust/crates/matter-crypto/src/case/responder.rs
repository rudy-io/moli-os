//! Responder-side CASE state machine.
//!
//! Drives the 3-message Sigma1 / Sigma2 / Sigma3 handshake from the
//! responder's perspective. Sans-IO: the caller is responsible for
//! transmitting and receiving bytes; this module only handles the
//! cryptographic state transitions.
//!
//! # Protocol flow (new-session path — Matter Core Spec §4.13.2.4)
//!
//! ```text
//! Initiator                         Responder (us)
//! ─────────────────────────────────────────────────────────
//!   Sigma1  ──────────────────────────────────────────>
//!                                    new() / new_using_rng()
//!                                    handle_sigma1()
//!              <──────────────────── next_message() → Sigma2
//!   Sigma3  ──────────────────────────────────────────>
//!                                    handle_sigma3()
//!              <──────────────────── StatusReport: Success
//!                                    finish() → CaseSessionOutput
//! ```
//!
//! Resumption path (`Sigma1` with resumption fields → `Sigma2_Resume`) is
//! implemented in M4.2. There is no `Sigma3_Resume` wire message — after
//! `Sigma2_Resume` is sent, the handshake is complete on the responder side.
//!
//! # KDF inputs (pinned from matter.js `CaseServer.ts` + `NodeSession.ts`)
//!
//! ## `DestinationId` verification (§4.13.2.4 step 1)
//!
//! The responder re-computes `DestinationId` and compares it against the
//! `dest_id` field in Sigma1 to determine whether this Sigma1 is addressed to
//! this fabric/node identity:
//!
//! ```text
//! salt = initiatorRandom(32) || rcacPublicKey(65) || fabricId_le8 || nodeId_le8
//! DestinationId = HMAC-SHA256(IPK, salt)
//! ```
//!
//! ## S2K — Sigma2 TBE encryption key
//!
//! ```text
//! sigma2Salt = IPK(16) || responderRandom(32) || responderEphPub(65) || SHA-256(sigma1_bytes)
//! S2K = HKDF(secret=sharedSecret, salt=sigma2Salt, info="Sigma2", len=16)
//! ```
//!
//! ## S3K — Sigma3 TBE decryption key
//!
//! ```text
//! sigma3Salt = IPK(16) || SHA-256(sigma1_bytes || sigma2_bytes)
//! S3K = HKDF(secret=sharedSecret, salt=sigma3Salt, info="Sigma3", len=16)
//! ```
//!
//! ## Session keys (responder assignment)
//!
//! ```text
//! sessionSalt = IPK(16) || SHA-256(sigma1_bytes || sigma2_bytes || sigma3_bytes)
//! keys(48) = HKDF(secret=sharedSecret, salt=sessionSalt, info="SessionKeys", len=48)
//! ```
//!
//! Responder key assignment differs from initiator (`NodeSession.ts`, `isInitiator=false`):
//! ```text
//! decryptKey (i2r) = keys[0..16]   -- responder decrypts what initiator encrypted
//! encryptKey (r2i) = keys[16..32]  -- responder encrypts to initiator
//! attestationChallenge = keys[32..48]
//! ```
//!
//! ## `TBSData2` (what we sign with our NOC key in Sigma2)
//!
//! ```text
//! TlvSignedData = {
//!     1: responderNoc (bytes) = our NOC,
//!     2: responderIcac (bytes, optional) = our ICAC,
//!     3: responderPublicKey (65 bytes) = our ephemeral pub,
//!     4: initiatorPublicKey (65 bytes) = initiator's ephemeral pub,
//! }
//! ```
//!
//! ## `TBSData3` (what we verify with initiator's NOC key from Sigma3)
//!
//! ```text
//! TlvSignedData = {
//!     1: responderNoc (bytes) = initiator's NOC,  ← note: field names from Sigma2 perspective
//!     2: responderIcac (bytes, optional) = initiator's ICAC,
//!     3: responderPublicKey (65 bytes) = initiator's ephemeral pub,
//!     4: initiatorPublicKey (65 bytes) = our ephemeral pub,
//! }
//! ```
//!
//! Pinned from `CaseServer.ts`; matter.js re-uses `TlvSignedData` symmetrically
//! in Sigma3 with the initiator in the "responder" position.

use p256::SecretKey;
use ring::rand::{SecureRandom, SystemRandom};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use matter_cert::{CertificateChain, MatterCertificate, MatterTime, Signature, TrustedRoots};

use crate::case::messages::{
    SessionParams, Sigma1, Sigma2, Sigma2Resume, Sigma3, SIGMA2_RESUME_SESSION_PARAMS_TAG,
    SIGMA2_SESSION_PARAMS_TAG,
};
use crate::case::sigma::{
    aead_decrypt, aead_encrypt, compute_dest_id, compute_sigma2_resume_mic, decode_tbedata3,
    derive_resume_session_keys, ecdh_shared_secret, encode_tbedata2, encode_tbs_data,
    generate_ephemeral_keypair, hkdf_derive, transcript_hash, verify_sigma1_resume_mic,
    AEAD_KEY_LEN, HKDF_INFO_SIGMA2, HKDF_INFO_SIGMA3, NONCE_TBE_DATA2, NONCE_TBE_DATA3,
};
use crate::case::{
    CaseCredentials, CaseMessageKind, CaseSessionKeys, CaseSessionOutput, LocalInfo, PeerInfo,
    ResumptionId, ResumptionRecord, Sigma1Outcome,
};
use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// HKDF info for session key derivation.
// Pinned from matter.js NodeSession.ts line 41:
//   const SESSION_KEYS_INFO = Bytes.fromString("SessionKeys")
// ---------------------------------------------------------------------------
const HKDF_INFO_SESSION_KEYS: &[u8] = b"SessionKeys";

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

/// Internal states of the responder-side CASE handshake.
///
/// Named for the *next expected* action at each point.
/// `Poisoned` is a sentinel used during `std::mem::replace` transitions;
/// it is never observable to callers (all methods replace it immediately
/// with either the next real state or an error return).
enum State {
    /// Initial state: `handle_sigma1()` has not been called yet.
    ///
    /// The ephemeral keypair and responder random are pre-sampled here so
    /// that `handle_sigma1()` cannot fail due to randomness.
    AwaitingSigma1 {
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        eph_secret: SecretKey,
        eph_pub: [u8; 65],
        responder_random: [u8; 32],
        responder_session_id: u16,
    },

    /// `handle_sigma1()` succeeded; the Sigma2 bytes are pre-built.
    /// `next_message()` retrieves them and advances to `AwaitingSigma3`.
    ReadyToSendSigma2 {
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        sigma2_bytes: Vec<u8>,
        sigma1_bytes: Vec<u8>,
        /// Raw ECDH shared secret. Wrapped in `Zeroizing` so the bytes are
        /// wiped on every drop path of this state (including an abandoned
        /// handshake), matching the initiator side.
        shared_secret: Zeroizing<[u8; 32]>,
        initiator_random: [u8; 32],
        responder_random: [u8; 32],
        initiator_eph_pub: [u8; 65],
        eph_pub: [u8; 65],
        initiator_session_id: u16,
        responder_session_id: u16,
        /// The fresh resumption id we sent (encrypted) in `TBEData2`; paired with
        /// `shared_secret` in the `ResumptionRecord` built after Sigma3.
        resumption_id: [u8; 16],
    },

    /// `next_message()` has emitted Sigma2; waiting for the initiator's Sigma3.
    AwaitingSigma3 {
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        sigma1_bytes: Vec<u8>,
        sigma2_bytes: Vec<u8>,
        /// Raw ECDH shared secret. Wrapped in `Zeroizing` so the bytes are
        /// wiped on every drop path of this state (including an abandoned
        /// handshake), matching the initiator side.
        shared_secret: Zeroizing<[u8; 32]>,
        /// Stored for M4.2 resumption (`Sigma1_Resume` MIC needs this).
        /// Not read in the new-session path implemented here.
        #[allow(dead_code)]
        initiator_random: [u8; 32],
        /// Stored for M4.2 resumption (`Sigma2_Resume` MIC needs this).
        /// Not read in the new-session path implemented here.
        #[allow(dead_code)]
        responder_random: [u8; 32],
        initiator_eph_pub: [u8; 65],
        eph_pub: [u8; 65],
        initiator_session_id: u16,
        responder_session_id: u16,
        /// The fresh resumption id we sent (encrypted) in `TBEData2`; paired with
        /// `shared_secret` in the `ResumptionRecord` built after Sigma3.
        resumption_id: [u8; 16],
    },

    /// `handle_sigma1()` surfaced a resumption request; the caller must look
    /// up the record in their session store and call either
    /// [`CaseResponder::accept_resumption`] or [`CaseResponder::reject_resumption`].
    AwaitingResumptionDecision {
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        /// Pre-generated ephemeral key (used if the caller falls back to the
        /// new-session path via `reject_resumption`).
        eph_secret: SecretKey,
        eph_pub: [u8; 65],
        responder_random: [u8; 32],
        responder_session_id: u16,
        /// Preserved for the new-session fallback path.
        initiator_random: [u8; 32],
        /// Preserved for the new-session fallback path (Sigma2 transcript).
        initiator_eph_pub: [u8; 65],
        initiator_session_id: u16,
        /// Raw Sigma1 bytes; needed for the new-session transcript if the caller
        /// falls back via `reject_resumption`.
        sigma1_bytes: Vec<u8>,
        /// The 16-byte resumption ID the initiator presented (Sigma1 tag 6).
        resumption_id_presented: [u8; 16],
        /// The 16-byte MIC the initiator presented (Sigma1 tag 7).
        initiator_resume_mic_received: [u8; 16],
    },

    /// `accept_resumption` completed; `next_message()` will return the
    /// `Sigma2_Resume` bytes and transition directly to `Complete`.
    ReadyToSendSigma2Resume {
        sigma2_resume_bytes: Vec<u8>,
        session_keys: CaseSessionKeys,
        peer: PeerInfo,
        local: LocalInfo,
        /// The updated resumption record to hand back via `CaseSessionOutput`.
        resumption_record: Option<ResumptionRecord>,
    },

    /// `handle_sigma3()` succeeded; `finish()` may be called.
    Complete {
        session_keys: CaseSessionKeys,
        peer: PeerInfo,
        local: LocalInfo,
        /// Fresh [`ResumptionRecord`] for the caller to persist: on the
        /// new-session path it pairs the resumption id we sent in `TBEData2`
        /// with the session's ECDH secret; on the resumption path it carries
        /// the updated id from `Sigma2_Resume`.
        resumption_record: Option<ResumptionRecord>,
    },

    /// Sentinel during `std::mem::replace` transitions.
    Poisoned,
}

/// Names the variant only. The ECDH `shared_secret` lives in these variants'
/// fields, and a derived `Debug` would print it byte for byte
/// (`Zeroizing<[u8; 32]>` forwards to the array). The operational signing
/// key, the IPK, the session keys and the resumption record already print
/// redacted, and the ephemeral secret prints opaquely. Nothing needs any of
/// them in a log, so none are printed. Written by hand, not derived, so that
/// a `Debug` added to the public state machine later cannot print them
/// either.
impl core::fmt::Debug for State {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::AwaitingSigma1 { .. } => "AwaitingSigma1",
            Self::ReadyToSendSigma2 { .. } => "ReadyToSendSigma2",
            Self::AwaitingSigma3 { .. } => "AwaitingSigma3",
            Self::AwaitingResumptionDecision { .. } => "AwaitingResumptionDecision",
            Self::ReadyToSendSigma2Resume { .. } => "ReadyToSendSigma2Resume",
            Self::Complete { .. } => "Complete",
            Self::Poisoned => "Poisoned",
        })
    }
}

// ---------------------------------------------------------------------------
// CaseResponder
// ---------------------------------------------------------------------------

/// Responder-side CASE state machine (new-session path).
///
/// Handles the Sigma1 / Sigma2 / Sigma3 handshake from the responder's
/// (device's) perspective. Sans-IO: the caller feeds raw bytes in via
/// [`handle_sigma1`][Self::handle_sigma1] and
/// [`handle_sigma3`][Self::handle_sigma3], and reads raw bytes out via
/// [`next_message`][Self::next_message].
///
/// # Construction
///
/// - [`CaseResponder::new`] — production constructor; uses the OS CSPRNG.
/// - `new_using_rng` (crate-internal) — deterministic constructor for tests;
///   accepts an injectable `ring::rand::SecureRandom`.
///
/// # Driving the handshake
///
/// 1. Receive Sigma1 bytes from the peer.
/// 2. Call [`handle_sigma1`][Self::handle_sigma1] with those bytes.
///    - Returns [`Sigma1Outcome::NewSession`] for a fresh session (M4.1).
///    - Returns `Err` if the `dest_id` doesn't match our fabric identity.
/// 3. Call [`next_message`][Self::next_message] → get Sigma2 bytes; send them.
/// 4. Receive Sigma3 bytes from the peer.
/// 5. Call [`handle_sigma3`][Self::handle_sigma3] with those bytes.
/// 6. Send a `StatusReport: Success` to the initiator.
/// 7. Call [`finish`][Self::finish] to retrieve [`CaseSessionOutput`].
///
/// Use [`expected_inbound`][Self::expected_inbound] at any point to query
/// which message the machine is currently waiting to receive.
pub struct CaseResponder {
    state: State,
    /// The initiator's advertised [`SessionParameters`](crate::SessionParameters)
    /// from Sigma1 (context tag 5), captured by
    /// [`handle_sigma1`][Self::handle_sigma1] and readable afterwards via
    /// [`peer_session_params`][Self::peer_session_params].
    ///
    /// Kept on the struct rather than in [`State`] because it must survive
    /// every transition (the caller reads it *after* Sigma1 to size the
    /// session it is about to register), and parking it in the enum would
    /// mean threading it through all six variants.
    peer_session_params: Option<crate::SessionParameters>,
    /// What *we* advertise back to the initiator, in Sigma2's
    /// `responderSessionParams` (context tag 5) or `Sigma2_Resume`'s (context
    /// tag 4). `None` — the default — omits the element, preserving the bytes
    /// this crate emitted before the builder existed.
    ///
    /// Set via [`with_session_params`][Self::with_session_params].
    local_session_params: Option<crate::SessionParameters>,
    /// Wall-clock instant at which the inbound initiator certificate chain is
    /// checked for temporal validity (`not_before <= now <= not_after`).
    /// Injected at construction so this crate never reads the system clock
    /// itself — the controller layer supplies the real time. See
    /// `process_sigma3`.
    validation_time: MatterTime,
    /// Byte-parity test seam: when `Some`, [`accept_resumption`][Self::accept_resumption]
    /// uses this as the fresh resumption id instead of sampling `SystemRandom`,
    /// so the emitted `Sigma2_Resume` is deterministic and comparable against a
    /// captured fixture. Always `None` in production (only the
    /// `test_support::case_responder_with_eph_key_and_resumption_id`
    /// constructor sets it).
    new_resumption_id_override: Option<[u8; 16]>,
}

impl CaseResponder {
    // ─── Public constructors ──────────────────────────────────────────────

    /// Construct a responder using the OS CSPRNG.
    ///
    /// Pre-samples the ephemeral keypair and 32-byte responder random so that
    /// [`handle_sigma1`][Self::handle_sigma1] cannot fail due to randomness.
    ///
    /// `responder_session_id` is the non-zero secured-session id this responder
    /// advertises in Sigma2 (tag 2) for the peer to address us by; it is
    /// recorded as `CaseSessionOutput.local.session_id` once the handshake
    /// completes.
    ///
    /// `now` is the wall-clock instant against which the initiator's
    /// operational certificate chain is checked for temporal validity during
    /// Sigma3. This crate never reads the system clock; the caller (controller
    /// layer) must supply the real time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EphemeralKeyGenerationFailed`] if the OS RNG fails
    /// (extremely unlikely in practice).
    pub fn new(
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        responder_session_id: u16,
        now: MatterTime,
    ) -> Result<Self> {
        let rng = SystemRandom::new();
        Self::new_using_rng(credentials, trusted_roots, responder_session_id, now, &rng)
    }

    /// Deterministic constructor for testing — accepts an injectable RNG.
    ///
    /// Production code should always use [`new`][Self::new].
    ///
    /// # Errors
    ///
    /// Returns [`Error::EphemeralKeyGenerationFailed`] if the RNG fails.
    pub(crate) fn new_using_rng(
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        responder_session_id: u16,
        now: MatterTime,
        rng: &dyn SecureRandom,
    ) -> Result<Self> {
        let (eph_secret, eph_pub) = generate_ephemeral_keypair(rng)?;
        let mut responder_random = [0u8; 32];
        rng.fill(&mut responder_random)
            .map_err(|_| Error::EphemeralKeyGenerationFailed)?;
        Ok(Self {
            state: State::AwaitingSigma1 {
                credentials,
                trusted_roots,
                eph_secret,
                eph_pub,
                responder_random,
                responder_session_id,
            },
            validation_time: now,
            new_resumption_id_override: None,
            peer_session_params: None,
            local_session_params: None,
        })
    }

    // Deterministic-input constructors below are used only by
    // `crate::test_support`, which is itself behind the `test-support`
    // feature. Gated to match, so a default-feature build does not compile
    // them as dead code.
    /// Deterministic constructor for byte-parity testing — injects a
    /// pre-computed ephemeral private key and responder random, bypassing
    /// the RNG entirely.
    ///
    /// This mirrors `new_using_rng` but derives the ephemeral public key
    /// from the supplied private key bytes rather than sampling from an RNG.
    /// The only valid caller is `test_support::case_responder_with_eph_key`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EphemeralKeyGenerationFailed`] if `eph_private_key`
    /// is zero, >= the P-256 curve order, or otherwise not a valid scalar.
    #[cfg(feature = "test-support")]
    pub(crate) fn new_with_eph_and_random(
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        eph_private_key: [u8; 32],
        responder_random: [u8; 32],
        now: MatterTime,
    ) -> Result<Self> {
        use p256::elliptic_curve::sec1::ToSec1Point;
        use p256::NonZeroScalar;
        let scalar_opt = NonZeroScalar::from_repr(eph_private_key.into());
        let scalar =
            Option::<NonZeroScalar>::from(scalar_opt).ok_or(Error::EphemeralKeyGenerationFailed)?;
        let eph_secret = SecretKey::from(scalar);
        let encoded = eph_secret.public_key().to_sec1_point(false);
        let mut eph_pub = [0u8; 65];
        eph_pub.copy_from_slice(encoded.as_bytes());
        Ok(Self {
            state: State::AwaitingSigma1 {
                credentials,
                trusted_roots,
                eph_secret,
                eph_pub,
                responder_random,
                responder_session_id: 0,
            },
            validation_time: now,
            new_resumption_id_override: None,
            peer_session_params: None,
            local_session_params: None,
        })
    }

    /// Byte-parity test seam: fix the resumption id that
    /// [`accept_resumption`][Self::accept_resumption] (`Sigma2_Resume`) and the
    /// new-session Sigma2 path (`TBEData2`) would otherwise sample from
    /// `SystemRandom`, so the emitted message is deterministic.
    /// The only valid caller is
    /// `test_support::case_responder_with_eph_key_and_resumption_id`.
    #[cfg(feature = "test-support")]
    pub(crate) fn set_new_resumption_id_override(&mut self, id: [u8; 16]) {
        self.new_resumption_id_override = Some(id);
    }

    /// Sample the fresh 16-byte resumption id this responder hands to the
    /// initiator (in `TBEData2` on the new-session path, in `Sigma2_Resume` on
    /// the resumption path), honouring the byte-parity override.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EphemeralKeyGenerationFailed`] if the OS RNG fails.
    fn fresh_resumption_id(&self) -> Result<[u8; 16]> {
        if let Some(id) = self.new_resumption_id_override {
            return Ok(id);
        }
        let mut id = [0u8; 16];
        SystemRandom::new()
            .fill(&mut id)
            .map_err(|_| Error::EphemeralKeyGenerationFailed)?;
        Ok(id)
    }

    /// Advertise `params` back to the initiator, in Sigma2's
    /// `responderSessionParams` (context tag 5) or, on the resumption path,
    /// `Sigma2_Resume`'s (context tag 4).
    ///
    /// Must be called before [`handle_sigma1`][Self::handle_sigma1], which is
    /// where the Sigma2 bytes are built.
    ///
    /// An [empty](crate::SessionParameters::is_empty) `params` omits the
    /// element rather than emitting `{}`.
    #[must_use]
    pub fn with_session_params(mut self, params: crate::SessionParameters) -> Self {
        self.local_session_params = (!params.is_empty()).then_some(params);
        self
    }

    // ─── State inspection ─────────────────────────────────────────────────

    /// The initiator's advertised [`SessionParameters`](crate::SessionParameters),
    /// captured from Sigma1's optional `initiatorSessionParams` element
    /// (context tag 5).
    ///
    /// `None` before [`handle_sigma1`][Self::handle_sigma1] has run, and also
    /// when the initiator omitted the element — which the spec defines as
    /// "assume the spec defaults", not "supports nothing".
    ///
    /// # Why the responder wants this
    ///
    /// The advertised `SESSION_IDLE_INTERVAL` / `SESSION_ACTIVE_INTERVAL` /
    /// `SESSION_ACTIVE_THRESHOLD` are the *initiator's* MRP timings. As the
    /// responder we have no mDNS record for the peer to size retransmits from
    /// — this element is the only channel through which a sleepy initiator can
    /// tell us "do not hammer me". Feed it to
    /// `MrpConfig::merge_session_params` in `matter-transport` and register the
    /// session with the result.
    ///
    /// chip applies it at exactly this point, before Sigma2 is even built
    /// (`CASESession::HandleSigma1`), so the handshake's own retransmits are
    /// already peer-sized.
    #[must_use]
    pub fn peer_session_params(&self) -> Option<crate::SessionParameters> {
        self.peer_session_params
    }

    /// Returns the CASE message kind the machine is currently waiting to
    /// receive, or `None` if the machine is in an outbound-only state,
    /// has completed, or has been poisoned.
    pub fn expected_inbound(&self) -> Option<CaseMessageKind> {
        match &self.state {
            State::AwaitingSigma1 { .. } => Some(CaseMessageKind::Sigma1),
            State::AwaitingSigma3 { .. } => Some(CaseMessageKind::Sigma3),
            _ => None,
        }
    }

    // ─── Handshake methods ────────────────────────────────────────────────

    /// Process the inbound Sigma1 message.
    ///
    /// Verifies that the `dest_id` in Sigma1 matches the responder's fabric
    /// identity.
    ///
    /// **New-session path:** If Sigma1 carries no resumption fields, computes
    /// the ECDH shared secret, builds and encrypts `TBEData2`, signs `TBSData2`
    /// with our NOC key, encodes the Sigma2 message, advances to
    /// `ReadyToSendSigma2`, and returns [`Sigma1Outcome::NewSession`].
    ///
    /// **Resumption path:** If Sigma1 carries both `resumption_id` (tag 6) and
    /// `initiator_resume_mic` (tag 7), transitions to `AwaitingResumptionDecision`
    /// and returns [`Sigma1Outcome::ResumptionRequested`]. The caller must then
    /// look up the `ResumptionRecord` and call either
    /// [`accept_resumption`][Self::accept_resumption] or
    /// [`reject_resumption`][Self::reject_resumption].
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedCaseMessage`] if called from the wrong state.
    /// - [`Error::InvalidParameter`] if the `dest_id` in Sigma1 does not match
    ///   our fabric identity, or TLV decode fails.
    /// - [`Error::EphemeralKeyGenerationFailed`] if ECDH or HKDF fails.
    /// - [`Error::SigningFailed`] if our NOC signing step fails.
    /// - [`Error::Codec`] on TLV encoding failure.
    // The two-path (new-session + resumption) dispatch is intentionally kept in
    // one function for auditability. The 100-line limit is relaxed here.
    #[allow(clippy::too_many_lines)]
    pub fn handle_sigma1(&mut self, bytes: &[u8]) -> Result<Sigma1Outcome> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::AwaitingSigma1 {
                credentials,
                trusted_roots,
                eph_secret,
                eph_pub,
                responder_random,
                responder_session_id,
            } => {
                // Decode Sigma1.
                let sigma1 = match Sigma1::decode(bytes) {
                    Ok(s) => s,
                    Err(e) => {
                        // Restore state so the machine isn't poisoned.
                        self.state = State::AwaitingSigma1 {
                            credentials,
                            trusted_roots,
                            eph_secret,
                            eph_pub,
                            responder_random,
                            responder_session_id,
                        };
                        return Err(e);
                    }
                };

                // Verify dest_id matches our fabric identity.
                let expected_dest_id = compute_dest_id(
                    &credentials.ipk,
                    &credentials.rcac_public_key,
                    credentials.fabric_id,
                    credentials.node_id,
                    &sigma1.initiator_random,
                );
                // `DestinationId` is an HMAC-SHA256 keyed by the secret IPK, so
                // it must be compared in constant time to avoid leaking timing
                // information about the keyed digest. `ct_eq` returns
                // `subtle::Choice` (1 = equal); `.into()` converts to `bool`.
                let dest_id_matches: bool = expected_dest_id.ct_eq(&sigma1.dest_id).into();
                if !dest_id_matches {
                    self.state = State::AwaitingSigma1 {
                        credentials,
                        trusted_roots,
                        eph_secret,
                        eph_pub,
                        responder_random,
                        responder_session_id,
                    };
                    return Err(Error::InvalidParameter);
                }

                // Capture the initiator's advertised session parameters, but
                // only now that `dest_id` has proved this Sigma1 is genuinely
                // for us — a rejected Sigma1 must leave nothing behind.
                //
                // A malformed element fails the handshake rather than being
                // ignored, matching chip's `DecodeSessionParametersIfPresent`
                // under `ReturnErrorOnFailure`. The decoder already tolerates
                // unknown tags and out-of-range values, so reaching this arm
                // means the TLV itself is broken.
                self.peer_session_params = match sigma1.session_parameters() {
                    Ok(p) => p,
                    Err(e) => {
                        self.state = State::AwaitingSigma1 {
                            credentials,
                            trusted_roots,
                            eph_secret,
                            eph_pub,
                            responder_random,
                            responder_session_id,
                        };
                        return Err(e);
                    }
                };

                let initiator_eph_pub = sigma1.initiator_eph_pub;
                let initiator_random = sigma1.initiator_random;
                let initiator_session_id = sigma1.initiator_session_id;
                let sigma1_bytes = bytes.to_vec();

                // Resumption path: both resumption_id AND initiator_resume_mic present.
                // Transition to AwaitingResumptionDecision so the caller can look up the
                // record and decide whether to accept or decline.
                if let (Some(resumption_id), Some(resume_mic)) =
                    (sigma1.resumption_id, sigma1.initiator_resume_mic)
                {
                    self.state = State::AwaitingResumptionDecision {
                        credentials,
                        trusted_roots,
                        eph_secret,
                        eph_pub,
                        responder_random,
                        responder_session_id,
                        initiator_random,
                        initiator_eph_pub,
                        initiator_session_id,
                        sigma1_bytes,
                        resumption_id_presented: resumption_id,
                        initiator_resume_mic_received: resume_mic,
                    };
                    return Ok(Sigma1Outcome::ResumptionRequested {
                        id: ResumptionId(resumption_id),
                    });
                }

                // New-session path. Sample the fresh resumption id we embed in
                // TBEData2 — sampled ONCE so the id the initiator persists and
                // the id we keep in state are the same value. The initiator may
                // present it in a later Sigma1 to resume this session.
                let (sigma2_bytes, shared_secret, resumption_id) =
                    match self.fresh_resumption_id().and_then(|rid| {
                        build_sigma2(
                            bytes,
                            &sigma1,
                            &credentials,
                            &eph_secret,
                            &eph_pub,
                            &responder_random,
                            responder_session_id,
                            &rid,
                            self.local_session_params,
                        )
                        .map(|(bytes, secret)| (bytes, secret, rid))
                    }) {
                        // Wrap the raw ECDH secret in `Zeroizing` immediately so
                        // it is wiped on every drop path once parked in `State`.
                        Ok((bytes, secret, rid)) => (bytes, Zeroizing::new(secret), rid),
                        Err(e) => {
                            self.state = State::AwaitingSigma1 {
                                credentials,
                                trusted_roots,
                                eph_secret,
                                eph_pub,
                                responder_random,
                                responder_session_id,
                            };
                            return Err(e);
                        }
                    };

                self.state = State::ReadyToSendSigma2 {
                    credentials,
                    trusted_roots,
                    sigma2_bytes,
                    sigma1_bytes,
                    shared_secret,
                    initiator_random,
                    responder_random,
                    initiator_eph_pub,
                    eph_pub,
                    initiator_session_id,
                    responder_session_id,
                    resumption_id,
                };

                Ok(Sigma1Outcome::NewSession)
            }
            other => {
                self.state = other;
                Err(Error::UnexpectedCaseMessage {
                    expected: CaseMessageKind::Sigma1,
                    got: CaseMessageKind::Sigma3,
                })
            }
        }
    }

    /// Accept a resumption attempt: verify the initiator's MIC, derive session
    /// keys, build the `Sigma2_Resume` message, and advance to
    /// `ReadyToSendSigma2Resume`.
    ///
    /// Must be called after [`handle_sigma1`][Self::handle_sigma1] returns
    /// [`Sigma1Outcome::ResumptionRequested`] with the caller-supplied
    /// [`ResumptionRecord`] that matches `id` in the outcome.
    ///
    /// # Resumption session-key layout
    ///
    /// Pinned from matter.js `NodeSession.create` (`isResumption = true` branch,
    /// responder `isInitiator = false`):
    /// ```text
    /// keys = HKDF(ikm  = shared_secret,
    ///             salt = initiatorRandom || OLD_resumption_id,
    ///             info = "SessionResumptionKeys",
    ///             len  = 48)
    /// // Responder (isInitiator=false) key assignment:
    /// keys[0..16]  → r2i_key          (responder encrypts to initiator)
    /// keys[16..32] → i2r_key          (responder decrypts from initiator)
    /// keys[32..48] → attestation_challenge
    /// ```
    ///
    /// This layout is the *same byte positions* as the initiator uses, but the
    /// semantic labels align with the responder's direction (see matter.js
    /// `NodeSession.ts` `isInitiator=false` branch).
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedCaseMessage`] if called from the wrong state.
    /// - [`Error::InvalidParameter`] if `record.id` does not match the
    ///   `resumption_id` the initiator presented.
    /// - [`Error::ResumptionMacMismatch`] if the `initiator_resume_mic` in
    ///   Sigma1 does not verify against `record.shared_secret`.
    /// - [`Error::EphemeralKeyGenerationFailed`] if the OS RNG or HKDF fails.
    /// - [`Error::Codec`] on TLV encoding failure.
    // Takes `record` by value deliberately: the caller hands over ownership of
    // the secret-bearing `ResumptionRecord` so it is consumed (and zeroized on
    // drop) here rather than lingering in the caller. We only clone the
    // non-`Copy` `peer` out of it (the record itself is `ZeroizeOnDrop`, so its
    // `shared_secret` cannot be moved out), which is why clippy no longer sees a
    // move that consumes the value.
    #[allow(clippy::needless_pass_by_value)]
    pub fn accept_resumption(&mut self, record: ResumptionRecord) -> Result<()> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::AwaitingResumptionDecision {
                credentials,
                trusted_roots: _,    // not needed on the resumption path
                eph_secret: _,       // not needed on the resumption path
                eph_pub: _,          // not needed on the resumption path
                responder_random: _, // not used in sigma2_resume_mic (confirmed from matter.js)
                responder_session_id,
                initiator_random,
                initiator_eph_pub: _,
                initiator_session_id,
                sigma1_bytes: _, // not needed on the resumption path
                resumption_id_presented,
                initiator_resume_mic_received,
            } => {
                // Step 1: Verify caller's record.id matches the resumption_id the
                // initiator presented. A mismatch means the caller looked up the
                // wrong record — this is an unrecoverable programming error, so we
                // leave the state Poisoned rather than restoring it.
                if record.id != ResumptionId(resumption_id_presented) {
                    return Err(Error::InvalidParameter);
                }

                // Step 2: Verify the initiator's sigma1_resume_mic in constant time.
                // Uses the OLD resumption_id (the one the initiator presented) and the
                // freshly-received initiator_random as the HKDF salt.
                verify_sigma1_resume_mic(
                    &record.shared_secret,
                    &initiator_random,
                    &resumption_id_presented,
                    &initiator_resume_mic_received,
                )?;

                // Step 3: Generate a fresh resumption ID for this new session.
                // The NEW id is what goes into Sigma2_Resume and into the caller's
                // persisted record after the handshake completes. Byte-parity
                // tests inject a fixed id via `new_resumption_id_override`.
                let new_resumption_id = self.fresh_resumption_id()?;

                // Step 4: Compute sigma2_resume_mic using the NEW resumption_id.
                // Pinned from matter.js CaseServer.ts `#resume`:
                //   key salt = initiatorRandom || newResumptionId
                //   info = "Sigma2_Resume"
                //   AES-128-CCM(key, plaintext=[], nonce="NCASE_SigmaS2") → 16-byte tag
                let sigma2_mic = compute_sigma2_resume_mic(
                    &record.shared_secret,
                    &initiator_random,
                    &new_resumption_id,
                )?;

                // Step 5: Derive the resumed session keys using the OLD resumption ID.
                //   salt = initiatorRandom || OLD_resumption_id
                //   info = "SessionResumptionKeys"
                //   len  = 48
                //   layout: [0..16]=i2r_key, [16..32]=r2i_key, [32..48]=attestation
                // The byte layout is the SAME as the new-session path — chip's
                // CryptoContext::InitFromSecret splits I2RKey || R2IKey ||
                // AttestationChallenge for kSessionResumption exactly as for
                // session establishment (live-verified against chip's OTA
                // requestor; the earlier r2i-first reading only survived because
                // both of our own sides agreed with each other).
                let blob = derive_resume_session_keys(
                    &record.shared_secret,
                    &initiator_random,
                    &resumption_id_presented,
                )?;
                let mut i2r_key = [0u8; 16];
                let mut r2i_key = [0u8; 16];
                let mut attestation_challenge = [0u8; 16];
                i2r_key.copy_from_slice(&blob[0..16]);
                r2i_key.copy_from_slice(&blob[16..32]);
                attestation_challenge.copy_from_slice(&blob[32..48]);
                let session_keys = CaseSessionKeys {
                    i2r_key,
                    r2i_key,
                    attestation_challenge,
                };

                // Step 6: Build the Sigma2_Resume wire message.
                // `Sigma2_Resume` carries the element at context tag **4**,
                // one lower than Sigma2's, because it has no eph-pub or
                // encrypted field.
                let responder_session_params = self
                    .local_session_params
                    .map(|p| {
                        p.encode(SIGMA2_RESUME_SESSION_PARAMS_TAG)
                            .map(SessionParams::from)
                    })
                    .transpose()?;
                let sigma2_resume = Sigma2Resume {
                    resumption_id: new_resumption_id,
                    resume_mic: sigma2_mic,
                    responder_session_id,
                    responder_session_params,
                };
                let sigma2_resume_bytes = sigma2_resume.encode()?;

                // Step 7: Build identity structs.
                // The resumption path re-uses the peer identity from the record; the
                // peer session ID comes from initiator_session_id (what the initiator
                // sent in Sigma1 tag 2, which is the session ID they want us to address
                // when sending back to them).
                let peer = PeerInfo {
                    session_id: initiator_session_id,
                    ..record.peer.clone()
                };
                let local = LocalInfo {
                    node_id: credentials.node_id,
                    fabric_id: credentials.fabric_id,
                    session_id: responder_session_id,
                };

                // Step 8: Build the next resumption record.
                // Carry the new_resumption_id forward; re-use shared_secret unchanged
                // (confirmed by matter.js — NodeSession does not re-derive on resumption).
                let next_record = ResumptionRecord {
                    id: ResumptionId(new_resumption_id),
                    shared_secret: record.shared_secret,
                    // `record` is `Drop` (ZeroizeOnDrop), so its non-`Copy`
                    // `peer` cannot be moved out — clone it.
                    peer: record.peer.clone(),
                    expires_at: None, // M6 commissioning sets a real expiry.
                };

                // Transition: after next_message() returns Sigma2_Resume, we go
                // directly to Complete. There is no inbound Sigma3_Resume to wait for
                // (confirmed from matter.js — the protocol ends after Sigma2_Resume).
                self.state = State::ReadyToSendSigma2Resume {
                    sigma2_resume_bytes,
                    session_keys,
                    peer,
                    local,
                    resumption_record: Some(next_record),
                };
                Ok(())
            }
            other => {
                self.state = other;
                Err(Error::UnexpectedCaseMessage {
                    expected: CaseMessageKind::Sigma1,
                    got: CaseMessageKind::Sigma1,
                })
            }
        }
    }

    /// Decline a resumption attempt and fall back to the new-session path.
    ///
    /// Must be called after [`handle_sigma1`][Self::handle_sigma1] returns
    /// [`Sigma1Outcome::ResumptionRequested`]. After this call, the state
    /// machine is in the same state as it would be after a regular Sigma1
    /// (new-session path). The next call to [`next_message`][Self::next_message]
    /// will return Sigma2 bytes (not `Sigma2_Resume`).
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedCaseMessage`] if called from the wrong state.
    pub fn reject_resumption(&mut self) -> Result<()> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::AwaitingResumptionDecision {
                credentials,
                trusted_roots,
                eph_secret,
                eph_pub,
                responder_random,
                responder_session_id,
                initiator_random,
                initiator_eph_pub,
                initiator_session_id,
                sigma1_bytes,
                // The resumption-specific fields are dropped; we're falling back.
                resumption_id_presented: _,
                initiator_resume_mic_received: _,
            } => {
                // Re-compute the Sigma2 using the pre-generated ephemeral keypair.
                // We pass a freshly decoded Sigma1 to build_sigma2; we have sigma1_bytes.
                let sigma1 = Sigma1::decode(&sigma1_bytes)?;

                // Fresh resumption id for the fallback full session (the id the
                // initiator presented belongs to the old, declined record).
                let resumption_id = self.fresh_resumption_id()?;
                let (sigma2_bytes, shared_secret) = build_sigma2(
                    &sigma1_bytes,
                    &sigma1,
                    &credentials,
                    &eph_secret,
                    &eph_pub,
                    &responder_random,
                    responder_session_id,
                    &resumption_id,
                    self.local_session_params,
                )?;
                // Wrap the raw ECDH secret in `Zeroizing` immediately so it is
                // wiped on every drop path once parked in `State`.
                let shared_secret = Zeroizing::new(shared_secret);

                // Transition to the standard new-session state, identical to
                // what handle_sigma1 (new-session path) would have produced.
                self.state = State::ReadyToSendSigma2 {
                    credentials,
                    trusted_roots,
                    sigma2_bytes,
                    sigma1_bytes,
                    shared_secret,
                    initiator_random,
                    responder_random,
                    initiator_eph_pub,
                    eph_pub,
                    initiator_session_id,
                    responder_session_id,
                    resumption_id,
                };
                Ok(())
            }
            other => {
                self.state = other;
                Err(Error::UnexpectedCaseMessage {
                    expected: CaseMessageKind::Sigma1,
                    got: CaseMessageKind::Sigma1,
                })
            }
        }
    }

    /// Retrieve the next outbound message and advance the state machine.
    ///
    /// **New-session path:** Returns the Sigma2 bytes and advances to
    /// `AwaitingSigma3`. Must be called after a successful
    /// [`handle_sigma1`][Self::handle_sigma1] that returned
    /// [`Sigma1Outcome::NewSession`], or after
    /// [`reject_resumption`][Self::reject_resumption].
    ///
    /// **Resumption path:** Returns the `Sigma2_Resume` bytes and advances
    /// directly to `Complete`. Must be called after a successful
    /// [`accept_resumption`][Self::accept_resumption]. There is no
    /// `Sigma3_Resume` — the handshake completes after `Sigma2_Resume` is sent
    /// (confirmed from matter.js).
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedCaseMessage`] if called from the wrong state.
    pub fn next_message(&mut self) -> Result<Vec<u8>> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::ReadyToSendSigma2 {
                credentials,
                trusted_roots,
                sigma2_bytes,
                sigma1_bytes,
                shared_secret,
                initiator_random,
                responder_random,
                initiator_eph_pub,
                eph_pub,
                initiator_session_id,
                responder_session_id,
                resumption_id,
            } => {
                self.state = State::AwaitingSigma3 {
                    credentials,
                    trusted_roots,
                    sigma1_bytes,
                    sigma2_bytes: sigma2_bytes.clone(),
                    shared_secret,
                    initiator_random,
                    responder_random,
                    initiator_eph_pub,
                    eph_pub,
                    initiator_session_id,
                    responder_session_id,
                    resumption_id,
                };
                Ok(sigma2_bytes)
            }

            // Resumption path: return Sigma2_Resume and transition directly to
            // Complete. No Sigma3_Resume to wait for (confirmed from matter.js).
            State::ReadyToSendSigma2Resume {
                sigma2_resume_bytes,
                session_keys,
                peer,
                local,
                resumption_record,
            } => {
                self.state = State::Complete {
                    session_keys,
                    peer,
                    local,
                    resumption_record,
                };
                Ok(sigma2_resume_bytes)
            }

            other => {
                self.state = other;
                Err(Error::UnexpectedCaseMessage {
                    expected: CaseMessageKind::Sigma2,
                    got: CaseMessageKind::Sigma1,
                })
            }
        }
    }

    /// Process the inbound Sigma3 message, verify the initiator's credentials,
    /// and derive the final session keys.
    ///
    /// # Sigma3 processing steps
    ///
    /// 1. Derive S3K via HKDF (same salt construction as initiator, mirrored).
    /// 2. AES-128-CCM decrypt the encrypted blob using S3K and the
    ///    `NCASE_Sigma3N` nonce.
    /// 3. Parse `TBEData3` = `{ initiatorNoc, initiatorIcac?, signature }`.
    /// 4. Validate the initiator's NOC chain against `trusted_roots`.
    /// 5. Extract initiator `NodeId` + `FabricId` from NOC subject.
    /// 6. Verify `FabricId` matches our credentials.
    /// 7. Verify the initiator's ECDSA signature over `TBSData3`.
    /// 8. Derive final session keys; assign i2r/r2i with responder convention.
    ///
    /// # Key assignment convention (responder, `isInitiator=false` in matter.js)
    ///
    /// ```text
    /// decryptKey  = keys[0..16]   (responder decrypts initiator traffic = i2r)
    /// encryptKey  = keys[16..32]  (responder encrypts to initiator = r2i)
    /// attestationChallenge = keys[32..48]
    /// ```
    ///
    /// Pinned from `NodeSession.ts`, `isInitiator=false` branch.
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedCaseMessage`] if called from the wrong state.
    /// - [`Error::EphemeralKeyGenerationFailed`] if HKDF fails.
    /// - [`Error::EncryptedBlobDecryptionFailed`] if the encrypted blob
    ///   fails AEAD verification.
    /// - [`Error::Codec`] / [`Error::InvalidParameter`] on TLV decode failure.
    /// - [`Error::InvalidPeerNocChain`] if chain validation fails.
    /// - [`Error::FabricIdMismatch`] if the initiator's NOC carries a
    ///   different `FabricId` than our credentials.
    /// - [`Error::PeerSignatureInvalid`] if the initiator's ECDSA signature
    ///   fails.
    pub fn handle_sigma3(&mut self, bytes: &[u8]) -> Result<()> {
        let now = self.validation_time;
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::AwaitingSigma3 {
                credentials,
                trusted_roots,
                sigma1_bytes,
                sigma2_bytes,
                shared_secret,
                initiator_random: _,
                responder_random: _,
                initiator_eph_pub,
                eph_pub,
                initiator_session_id,
                responder_session_id,
                resumption_id,
            } => {
                let (session_keys, peer, local) = match process_sigma3(
                    bytes,
                    &credentials,
                    &trusted_roots,
                    &shared_secret,
                    &sigma1_bytes,
                    &sigma2_bytes,
                    &initiator_eph_pub,
                    &eph_pub,
                    initiator_session_id,
                    responder_session_id,
                    now,
                ) {
                    Ok(v) => v,
                    Err(e) => {
                        // Poison — the handshake cannot be retried on error.
                        return Err(e);
                    }
                };

                // Pair the resumption id we sent in TBEData2 with this
                // session's ECDH secret — the same (id, secret) record the
                // initiator persists, so it can resume against us later.
                let resumption_record = ResumptionRecord {
                    id: ResumptionId(resumption_id),
                    shared_secret: *shared_secret,
                    peer: peer.clone(),
                    expires_at: None,
                };

                self.state = State::Complete {
                    session_keys,
                    peer,
                    local,
                    resumption_record: Some(resumption_record),
                };
                Ok(())
            }
            other => {
                self.state = other;
                Err(Error::UnexpectedCaseMessage {
                    expected: CaseMessageKind::Sigma3,
                    got: CaseMessageKind::Sigma1,
                })
            }
        }
    }

    /// Finalise the session and retrieve the derived [`CaseSessionOutput`].
    ///
    /// May only be called after [`handle_sigma3`][Self::handle_sigma3] has
    /// completed (i.e., the state machine is in the `Complete` state).
    ///
    /// # Errors
    ///
    /// - [`Error::HandshakeIncomplete`] if called before all handshake phases
    ///   have completed.
    pub fn finish(self) -> Result<CaseSessionOutput> {
        match self.state {
            State::Complete {
                session_keys,
                peer,
                local,
                resumption_record,
            } => Ok(CaseSessionOutput {
                keys: session_keys,
                peer,
                local,
                resumption_record,
            }),
            _ => Err(Error::HandshakeIncomplete),
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: Sigma2 construction inner logic
// ---------------------------------------------------------------------------

/// Build the Sigma2 message and return `(sigma2_bytes, shared_secret)`.
///
/// Extracted from `CaseResponder::handle_sigma1` to keep the method body
/// within the `clippy::too_many_lines` limit.
///
/// Steps performed:
/// 1. ECDH shared secret from our eph secret + initiator's eph pub.
/// 2. Derive S2K.
/// 3. Build `TBSData2` and sign with our NOC key.
/// 4. Encode `TBEData2` and encrypt with S2K + `NCASE_Sigma2N` nonce.
/// 5. Encode the Sigma2 wire message.
///
/// # Errors
///
/// See `CaseResponder::handle_sigma1` error documentation.
#[allow(clippy::too_many_arguments)]
fn build_sigma2(
    sigma1_bytes: &[u8],
    sigma1: &Sigma1,
    credentials: &CaseCredentials,
    eph_secret: &SecretKey,
    eph_pub: &[u8; 65],
    responder_random: &[u8; 32],
    responder_session_id: u16,
    resumption_id: &[u8; 16],
    local_session_params: Option<crate::SessionParameters>,
) -> Result<(Vec<u8>, [u8; 32])> {
    // Step 1: ECDH shared secret from our eph secret + initiator's eph pub.
    let shared_secret = ecdh_shared_secret(eph_secret, &sigma1.initiator_eph_pub)?;

    // Step 2: Derive S2K.
    // sigma2Salt = IPK(16) || responderRandom(32) || responderEphPub(65) || SHA-256(sigma1)
    let h_sigma1 = transcript_hash(&[sigma1_bytes]);
    let mut sigma2_salt: Vec<u8> = Vec::with_capacity(16 + 32 + 65 + 32);
    sigma2_salt.extend_from_slice(&credentials.ipk);
    sigma2_salt.extend_from_slice(responder_random);
    sigma2_salt.extend_from_slice(eph_pub);
    sigma2_salt.extend_from_slice(&h_sigma1);
    // `s2k` is a derived secret key; wrap in `Zeroizing` so it is wiped from
    // memory when this function returns. (`shared_secret` is returned to the
    // state machine, which wraps it in `Zeroizing` so it is wiped on every drop
    // path of the parked state.)
    let mut s2k = Zeroizing::new([0u8; AEAD_KEY_LEN]);
    hkdf_derive(&shared_secret, &sigma2_salt, HKDF_INFO_SIGMA2, &mut *s2k)?;

    // Step 3: Build TBSData2 and sign with our NOC key.
    // TBSData2 = TlvSignedData { ourNoc, ourIcac?, ourEphPub, initiatorEphPub }
    // Responder's NOC is "responderNoc"; initiator's eph pub is "initiatorPublicKey".
    let our_noc_tlv = credentials
        .noc
        .to_tlv()
        .map_err(|_| Error::SigningFailed(crate::case::signer::SignerError::Internal))?;
    let our_icac_tlv: Option<Vec<u8>> = match &credentials.icac {
        Some(icac) => Some(
            icac.to_tlv()
                .map_err(|_| Error::SigningFailed(crate::case::signer::SignerError::Internal))?,
        ),
        None => None,
    };
    let tbs_data2 = encode_tbs_data(
        &our_noc_tlv,
        our_icac_tlv.as_deref(),
        eph_pub,                   // our eph pub = "responderPublicKey"
        &sigma1.initiator_eph_pub, // initiator eph pub = "initiatorPublicKey"
    )?;
    let our_signature = credentials
        .signer
        .sign_p256_sha256(&tbs_data2)
        .map_err(Error::SigningFailed)?;

    // Step 4: Encode TBEData2, encrypt with S2K. `resumption_id` is the fresh
    // id the caller sampled (see `fresh_resumption_id`); the initiator persists
    // it alongside this session's ECDH secret for a later resumption attempt.
    let tbedata2_plaintext = encode_tbedata2(
        &our_noc_tlv,
        our_icac_tlv.as_deref(),
        &our_signature,
        resumption_id,
    )?;
    let encrypted2 = aead_encrypt(&s2k, NONCE_TBE_DATA2, b"", &tbedata2_plaintext)?;

    // Step 5: Encode Sigma2 wire message. Sigma2 carries our advertisement at
    // context tag 5.
    let responder_session_params = local_session_params
        .map(|p| p.encode(SIGMA2_SESSION_PARAMS_TAG).map(SessionParams::from))
        .transpose()?;
    let sigma2 = Sigma2 {
        responder_random: *responder_random,
        responder_session_id,
        responder_eph_pub: *eph_pub,
        encrypted: encrypted2,
        responder_session_params,
    };
    let sigma2_bytes = sigma2.encode()?;

    Ok((sigma2_bytes, shared_secret))
}

// ---------------------------------------------------------------------------
// Helper: Sigma3 processing inner logic
// ---------------------------------------------------------------------------

/// Execute the full Sigma3 verification + session key derivation.
///
/// Extracted from `CaseResponder::handle_sigma3` to keep that method's
/// line count within the `clippy::too_many_lines` limit.
///
/// Returns `(session_keys, peer, local)` on success.
///
/// # Errors
///
/// See `CaseResponder::handle_sigma3` for the full error taxonomy.
// The 8-step SIGMA-R protocol is intentionally kept as one function for
// auditability: a reviewer must be able to trace every step in sequence
// without jumping across files. The 100-line limit is relaxed here.
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
fn process_sigma3(
    sigma3_bytes: &[u8],
    credentials: &CaseCredentials,
    trusted_roots: &TrustedRoots,
    shared_secret: &[u8; 32],
    sigma1_bytes: &[u8],
    sigma2_bytes: &[u8],
    initiator_eph_pub: &[u8; 65],
    eph_pub: &[u8; 65],
    initiator_session_id: u16,
    responder_session_id: u16,
    now: MatterTime,
) -> Result<(CaseSessionKeys, PeerInfo, LocalInfo)> {
    let sigma3 = Sigma3::decode(sigma3_bytes)?;

    // Step 1: Derive S3K.
    // sigma3Salt = IPK(16) || SHA-256(sigma1 || sigma2)
    let h_s1_s2 = transcript_hash(&[sigma1_bytes, sigma2_bytes]);
    let mut sigma3_salt: Vec<u8> = Vec::with_capacity(16 + 32);
    sigma3_salt.extend_from_slice(&credentials.ipk);
    sigma3_salt.extend_from_slice(&h_s1_s2);
    // `s3k` is a derived secret key; wrap in `Zeroizing` so it is wiped from
    // memory when this function returns (success or error).
    let mut s3k = Zeroizing::new([0u8; AEAD_KEY_LEN]);
    hkdf_derive(shared_secret, &sigma3_salt, HKDF_INFO_SIGMA3, &mut *s3k)?;

    // Step 2: AES-128-CCM decrypt.
    let sigma3_decrypted = aead_decrypt(&s3k, NONCE_TBE_DATA3, b"", &sigma3.encrypted)?;

    // Step 3: Parse TBEData3.
    let mut peer_tbe = decode_tbedata3(&sigma3_decrypted)?;

    // Step 4: Validate initiator NOC chain against trusted roots at the
    // injected wall-clock instant (`not_before <= now <= not_after`). The
    // certs are MOVED into the chain Vec (no clones); the NOC is taken back
    // out after validation for the subject/key checks and the returned
    // PeerInfo.
    let mut chain_certs: Vec<MatterCertificate> = match peer_tbe.peer_icac.take() {
        Some(icac) => vec![peer_tbe.peer_noc, icac],
        None => vec![peer_tbe.peer_noc],
    };
    CertificateChain::new(&chain_certs)
        .validate(trusted_roots, now)
        .map_err(Error::InvalidPeerNocChain)?;
    // O(1); index 0 is the NOC in both arms. A leftover ICAC is dropped —
    // later code reads the raw `peer_icac_tlv` bytes, not the parsed cert.
    let peer_noc = chain_certs.swap_remove(0);

    // Step 5: Extract initiator NodeId + FabricId from NOC subject.
    let peer_dn = peer_noc.subject();
    let peer_node_id = peer_dn
        .node_id()
        .ok_or(Error::PeerNodeIdMismatch(0, credentials.node_id))?;
    let peer_fabric_id = peer_dn.fabric_id().ok_or(Error::FabricIdMismatch {
        peer: 0,
        local: credentials.fabric_id,
    })?;

    // Step 6: Verify FabricId matches our credentials.
    if peer_fabric_id != credentials.fabric_id {
        return Err(Error::FabricIdMismatch {
            peer: peer_fabric_id,
            local: credentials.fabric_id,
        });
    }

    // Step 7: Verify initiator's ECDSA signature over TBSData3.
    // In Sigma3, the initiator plays the "responder" role in TlvSignedData
    // (field names defined from Sigma2 perspective; re-used symmetrically).
    // Pinned from CaseServer.ts: initiatorEphPub → "responderPublicKey",
    //                             responderEphPub → "initiatorPublicKey".
    // — over the peer's exact wire bytes (kept in TbeData3), not a re-encoding.
    let peer_signed_data = encode_tbs_data(
        &peer_tbe.peer_noc_tlv,
        peer_tbe.peer_icac_tlv.as_deref(),
        initiator_eph_pub, // initiator's eph pub = "responderPublicKey" in TBSData3
        eph_pub,           // our eph pub = "initiatorPublicKey" in TBSData3
    )?;
    let peer_sig =
        Signature::from_slice(&peer_tbe.peer_signature).map_err(|_| Error::PeerSignatureInvalid)?;
    peer_noc
        .public_key()
        .verify(&peer_signed_data, &peer_sig)
        .map_err(|_| Error::PeerSignatureInvalid)?;

    // Step 8: Derive final session keys.
    // sessionSalt = IPK(16) || SHA-256(sigma1 || sigma2 || sigma3)
    let h_all = transcript_hash(&[sigma1_bytes, sigma2_bytes, sigma3_bytes]);
    let mut session_salt: Vec<u8> = Vec::with_capacity(16 + 32);
    session_salt.extend_from_slice(&credentials.ipk);
    session_salt.extend_from_slice(&h_all);
    // `keys_blob` holds the raw 48-byte session-key material; wrap in
    // `Zeroizing` so it is wiped once the per-direction keys are split out.
    let mut keys_blob = Zeroizing::new([0u8; 48]);
    hkdf_derive(
        shared_secret,
        &session_salt,
        HKDF_INFO_SESSION_KEYS,
        &mut *keys_blob,
    )?;

    // Responder key assignment (NodeSession.ts, isInitiator=false):
    //   decryptKey (i2r) = keys[0..16]   — responder decrypts what initiator encrypts
    //   encryptKey (r2i) = keys[16..32]  — responder encrypts to initiator
    //   attestationChallenge = keys[32..48]
    // The key bytes are identical to the initiator's derivation; only the
    // variable-name binding differs (swap which end "encrypts" and which "decrypts").
    let mut i2r_key = [0u8; 16];
    let mut r2i_key = [0u8; 16];
    let mut attestation_challenge = [0u8; 16];
    i2r_key.copy_from_slice(&keys_blob[0..16]);
    r2i_key.copy_from_slice(&keys_blob[16..32]);
    attestation_challenge.copy_from_slice(&keys_blob[32..48]);

    let session_keys = CaseSessionKeys {
        i2r_key,
        r2i_key,
        attestation_challenge,
    };

    let peer = PeerInfo {
        node_id: peer_node_id,
        fabric_id: peer_fabric_id,
        noc: peer_noc,
        session_id: initiator_session_id,
    };
    let local = LocalInfo {
        node_id: credentials.node_id,
        fabric_id: credentials.fabric_id,
        session_id: responder_session_id,
    };

    Ok((session_keys, peer, local))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::case::signer::{CaseSigner, RingSigner};
    use matter_cert::test_support::{build_unsigned, TestCertFields};
    use matter_cert::{
        BasicConstraints, DistinguishedName, DnAttribute, Extensions, MatterTime, TrustAnchor,
        TrustedRoots,
    };

    // ─── Test helpers ─────────────────────────────────────────────────────

    /// Build a minimal `MatterCertificate` suitable for unit tests.
    ///
    /// The cert is not validly signed; its purpose is to let state-machine
    /// tests exercise paths that don't reach chain validation.
    fn make_test_cert(node_id: u64, fabric_id: u64) -> MatterCertificate {
        let (signer, _) = RingSigner::generate().unwrap();
        let pk_bytes = *signer.public_key().as_bytes();
        let pub_key = matter_cert::PublicKey::new(pk_bytes).unwrap();
        let subject = DistinguishedName::new(vec![
            DnAttribute::FabricId(fabric_id),
            DnAttribute::NodeId(node_id),
        ]);
        let issuer = DistinguishedName::new(vec![DnAttribute::RcacId(1)]);
        let extensions = Extensions::builder()
            .basic_constraints(Some(BasicConstraints::new(false, None)))
            .build();
        build_unsigned(TestCertFields {
            serial: vec![1],
            issuer,
            not_before: MatterTime::from_unix_secs(0),
            not_after: MatterTime::NO_EXPIRY,
            subject,
            public_key: pub_key,
            extensions,
            signature: matter_cert::Signature::new([0u8; 64]),
        })
    }

    /// Build a `CaseCredentials` with a fresh `RingSigner` keypair.
    fn make_test_credentials(
        node_id: u64,
        fabric_id: u64,
        ipk: [u8; 16],
        rcac_public_key: [u8; 65],
    ) -> CaseCredentials {
        let (signer, _) = RingSigner::generate().unwrap();
        let noc = make_test_cert(node_id, fabric_id);
        CaseCredentials {
            noc,
            icac: None,
            signer: Box::new(signer),
            fabric_id,
            node_id,
            ipk,
            rcac_public_key,
        }
    }

    /// Build an empty `TrustedRoots` set (used for tests that don't reach
    /// chain validation).
    fn empty_roots() -> TrustedRoots {
        TrustedRoots::new()
    }

    /// A valid-looking RCAC public key (SEC1 uncompressed, prefix 0x04).
    fn dummy_rcac_pub() -> [u8; 65] {
        let mut k = [0u8; 65];
        k[0] = 0x04;
        k
    }

    // ─── Construction ─────────────────────────────────────────────────────

    /// `new()` must accept valid credentials without panicking.
    #[test]
    fn new_succeeds_with_valid_credentials() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let _responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
    }

    // ─── expected_inbound() states ────────────────────────────────────────

    /// Freshly constructed responder must be waiting for Sigma1.
    #[test]
    fn expected_inbound_initially_is_sigma1() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        assert_eq!(responder.expected_inbound(), Some(CaseMessageKind::Sigma1));
    }

    /// After `handle_sigma1` and `next_message`, `expected_inbound` is `Sigma3`.
    #[test]
    fn expected_inbound_after_next_message_is_sigma3() {
        use crate::case::messages::Sigma1;
        let ipk = [0xAB; 16];
        let mut rcac_pub = [0u8; 65];
        rcac_pub[0] = 0x04;

        let creds = make_test_credentials(0x1234, 0x5678, ipk, rcac_pub);
        let mut responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();

        // Compute the correct dest_id for this responder.
        let initiator_random = [0x42u8; 32];
        let dest_id = compute_dest_id(&ipk, &rcac_pub, 0x5678, 0x1234, &initiator_random);

        // Build a Sigma1 that addresses this responder.
        let sigma1 = Sigma1 {
            initiator_random,
            initiator_session_id: 1,
            dest_id,
            initiator_eph_pub: {
                let rng = ring::rand::SystemRandom::new();
                let (_, pub_bytes) = generate_ephemeral_keypair(&rng).unwrap();
                pub_bytes
            },
            initiator_session_params: None,
            resumption_id: None,
            initiator_resume_mic: None,
        };
        let sigma1_bytes = sigma1.encode().unwrap();

        let outcome = responder.handle_sigma1(&sigma1_bytes).unwrap();
        assert_eq!(outcome, Sigma1Outcome::NewSession);

        let _ = responder.next_message().unwrap();
        assert_eq!(responder.expected_inbound(), Some(CaseMessageKind::Sigma3));
    }

    // ─── handle_sigma1: dest_id mismatch ──────────────────────────────────

    /// `handle_sigma1` with a wrong `dest_id` must return `InvalidParameter`.
    #[test]
    fn handle_sigma1_unknown_dest_id_returns_invalid_parameter() {
        use crate::case::messages::Sigma1;
        let ipk = [0xAB; 16];
        let rcac_pub = dummy_rcac_pub();
        let creds = make_test_credentials(0x1234, 0x5678, ipk, rcac_pub);
        let mut responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();

        // Build a Sigma1 with a garbage dest_id — it won't match our identity.
        let sigma1 = Sigma1 {
            initiator_random: [0x11; 32],
            initiator_session_id: 1,
            dest_id: [0xFF; 32], // clearly wrong
            initiator_eph_pub: {
                let rng = ring::rand::SystemRandom::new();
                let (_, pub_bytes) = generate_ephemeral_keypair(&rng).unwrap();
                pub_bytes
            },
            initiator_session_params: None,
            resumption_id: None,
            initiator_resume_mic: None,
        };
        let sigma1_bytes = sigma1.encode().unwrap();

        assert!(matches!(
            responder.handle_sigma1(&sigma1_bytes),
            Err(Error::InvalidParameter)
        ));
    }

    // ─── handle_sigma1: resumption path ──────────────────────────────────

    /// Helper: build a Sigma1 that addresses the responder. Returns
    /// `(sigma1_bytes, initiator_random, noc_cert)` so callers can build a
    /// matching `ResumptionRecord`.
    ///
    /// When `resumption_id` and `resume_mic` are both `Some`, the Sigma1 carries
    /// resumption fields and `handle_sigma1` must return
    /// `Sigma1Outcome::ResumptionRequested`.
    fn build_sigma1_for_responder(
        ipk: &[u8; 16],
        rcac_pub: &[u8; 65],
        node_id: u64,
        fabric_id: u64,
        initiator_random: [u8; 32],
        resumption_id: Option<[u8; 16]>,
        resume_mic: Option<[u8; 16]>,
    ) -> Vec<u8> {
        let dest_id = compute_dest_id(ipk, rcac_pub, fabric_id, node_id, &initiator_random);
        let rng = ring::rand::SystemRandom::new();
        let (_, eph_pub) = generate_ephemeral_keypair(&rng).unwrap();
        let sigma1 = Sigma1 {
            initiator_random,
            initiator_session_id: 7,
            dest_id,
            initiator_eph_pub: eph_pub,
            initiator_session_params: None,
            resumption_id,
            initiator_resume_mic: resume_mic,
        };
        sigma1.encode().unwrap()
    }

    /// `handle_sigma1` with both `resumption_id` AND `initiator_resume_mic` must
    /// return `Sigma1Outcome::ResumptionRequested` carrying the correct ID.
    #[test]
    fn handle_sigma1_with_resumption_fields_returns_resumption_requested() {
        let ipk = [0xAB; 16];
        let rcac_pub = dummy_rcac_pub();
        let creds = make_test_credentials(0x1234, 0x5678, ipk, rcac_pub);
        let mut responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();

        let initiator_random = [0x42u8; 32];
        let resumption_id = [0xCC; 16];
        // A plausible (but not verified-here) 16-byte MIC.
        let resume_mic = [0xDD; 16];

        let sigma1_bytes = build_sigma1_for_responder(
            &ipk,
            &rcac_pub,
            0x1234,
            0x5678,
            initiator_random,
            Some(resumption_id),
            Some(resume_mic),
        );

        let outcome = responder.handle_sigma1(&sigma1_bytes).unwrap();
        assert_eq!(
            outcome,
            Sigma1Outcome::ResumptionRequested {
                id: ResumptionId(resumption_id),
            }
        );
    }

    /// `handle_sigma1` with only `resumption_id` (no MIC) must fall through to
    /// the new-session path since we require BOTH fields for resumption.
    #[test]
    fn handle_sigma1_with_only_resumption_id_takes_new_session_path() {
        let ipk = [0xAB; 16];
        let rcac_pub = dummy_rcac_pub();
        let creds = make_test_credentials(0x1234, 0x5678, ipk, rcac_pub);
        let mut responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();

        let initiator_random = [0x42u8; 32];
        let sigma1_bytes = build_sigma1_for_responder(
            &ipk,
            &rcac_pub,
            0x1234,
            0x5678,
            initiator_random,
            Some([0xCC; 16]), // resumption_id present
            None,             // MIC absent — should NOT trigger resumption path
        );

        let outcome = responder.handle_sigma1(&sigma1_bytes).unwrap();
        assert_eq!(outcome, Sigma1Outcome::NewSession);
    }

    // ─── Out-of-order rejection ────────────────────────────────────────────

    /// `next_message` before `handle_sigma1` must return `UnexpectedCaseMessage`.
    #[test]
    fn next_message_before_handle_sigma1_is_rejected() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let mut responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        assert!(matches!(
            responder.next_message(),
            Err(Error::UnexpectedCaseMessage { .. })
        ));
    }

    /// `handle_sigma3` before `handle_sigma1` must return `UnexpectedCaseMessage`.
    #[test]
    fn handle_sigma3_before_sigma1_is_rejected() {
        use crate::case::messages::Sigma3;
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let mut responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();

        let dummy_sigma3 = Sigma3 {
            encrypted: vec![0xAA; 80],
        };
        let bytes = dummy_sigma3.encode().unwrap();
        assert!(matches!(
            responder.handle_sigma3(&bytes),
            Err(Error::UnexpectedCaseMessage { .. })
        ));
    }

    // ─── finish() before Complete ──────────────────────────────────────────

    /// `finish()` before any handshake steps returns `HandshakeIncomplete`.
    #[test]
    fn finish_before_complete_returns_handshake_incomplete() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        assert!(matches!(
            responder.finish(),
            Err(Error::HandshakeIncomplete)
        ));
    }

    // ─── TrustedRoots helper verification ─────────────────────────────────

    /// Ensures the `TrustedRoots` type accepts roots correctly (used by both
    /// initiator and responder tests).
    #[test]
    fn trusted_roots_with_anchor_is_non_empty() {
        let rcac = make_test_cert(0, 0x5678);
        let anchor = TrustAnchor::from_root_cert(&rcac);
        let mut roots = TrustedRoots::new();
        roots.add(anchor);
        assert!(!roots.is_empty());
        assert_eq!(roots.len(), 1);
    }

    // ─── Resumption: accept_resumption / reject_resumption ────────────────

    /// Build a valid `ResumptionRecord` and the matching Sigma1 bytes
    /// (i.e., the MIC was computed correctly and will verify successfully).
    fn build_valid_resumption_setup(
        ipk: &[u8; 16],
        rcac_pub: &[u8; 65],
        node_id: u64,
        fabric_id: u64,
    ) -> (Vec<u8>, ResumptionRecord, [u8; 32]) {
        use crate::case::sigma::compute_sigma1_resume_mic;

        let shared_secret = [0x55u8; 32];
        let resumption_id = [0xAA; 16];
        let initiator_random = [0x11; 32];

        // Compute the MIC as the initiator would.
        let mic =
            compute_sigma1_resume_mic(&shared_secret, &initiator_random, &resumption_id).unwrap();

        let sigma1_bytes = build_sigma1_for_responder(
            ipk,
            rcac_pub,
            node_id,
            fabric_id,
            initiator_random,
            Some(resumption_id),
            Some(mic),
        );

        // Build a synthetic NOC to embed in the record (resumption re-uses cached peer).
        let noc = make_test_cert(node_id + 1, fabric_id);
        let peer = PeerInfo {
            node_id: node_id + 1,
            fabric_id,
            noc,
            session_id: 99,
        };
        let record = ResumptionRecord {
            id: ResumptionId(resumption_id),
            shared_secret,
            peer,
            expires_at: None,
        };

        (sigma1_bytes, record, initiator_random)
    }

    /// `accept_resumption` rejects a record whose ID doesn't match the one the
    /// initiator presented.
    #[test]
    fn accept_resumption_rejects_wrong_id() {
        let ipk = [0xAB; 16];
        let rcac_pub = dummy_rcac_pub();
        let creds = make_test_credentials(0x1234, 0x5678, ipk, rcac_pub);
        let mut responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();

        let (sigma1_bytes, mut record, _) =
            build_valid_resumption_setup(&ipk, &rcac_pub, 0x1234, 0x5678);

        let outcome = responder.handle_sigma1(&sigma1_bytes).unwrap();
        assert!(matches!(outcome, Sigma1Outcome::ResumptionRequested { .. }));

        // Tamper with the record ID — it no longer matches the presented ID.
        record.id = ResumptionId([0xFF; 16]);
        assert!(matches!(
            responder.accept_resumption(record),
            Err(Error::InvalidParameter)
        ));
    }

    /// `accept_resumption` rejects a record whose `shared_secret` produces a wrong MIC.
    #[test]
    fn accept_resumption_rejects_invalid_mic() {
        let ipk = [0xAB; 16];
        let rcac_pub = dummy_rcac_pub();
        let creds = make_test_credentials(0x1234, 0x5678, ipk, rcac_pub);
        let mut responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();

        let (sigma1_bytes, mut record, _) =
            build_valid_resumption_setup(&ipk, &rcac_pub, 0x1234, 0x5678);

        let outcome = responder.handle_sigma1(&sigma1_bytes).unwrap();
        assert!(matches!(outcome, Sigma1Outcome::ResumptionRequested { .. }));

        // Tamper with the shared secret — the MIC verification will fail.
        record.shared_secret = [0xFF; 32];
        assert!(matches!(
            responder.accept_resumption(record),
            Err(Error::ResumptionMacMismatch)
        ));
    }

    /// After `handle_sigma1` (resumption) + `reject_resumption`, calling
    /// `next_message` returns a Sigma2 (new-session path) rather than `Sigma2_Resume`.
    #[test]
    fn reject_resumption_transitions_to_new_session_path() {
        use crate::case::messages::Sigma2;

        let ipk = [0xAB; 16];
        let rcac_pub = dummy_rcac_pub();
        let creds = make_test_credentials(0x1234, 0x5678, ipk, rcac_pub);
        let mut responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();

        let (sigma1_bytes, _record, _) =
            build_valid_resumption_setup(&ipk, &rcac_pub, 0x1234, 0x5678);

        let outcome = responder.handle_sigma1(&sigma1_bytes).unwrap();
        assert!(matches!(outcome, Sigma1Outcome::ResumptionRequested { .. }));

        responder.reject_resumption().unwrap();

        // next_message() must succeed and return some bytes.
        let outbound = responder.next_message().unwrap();
        assert!(
            !outbound.is_empty(),
            "next_message after reject_resumption must return Sigma2 bytes"
        );

        // The returned bytes should decode as a valid Sigma2 (not Sigma2_Resume).
        // Sigma2 has tag 3 = responder_eph_pub (65 bytes); Sigma2_Resume has tag 1 = resumption_id (16 bytes).
        // A successful Sigma2::decode is sufficient confirmation.
        Sigma2::decode(&outbound).unwrap();
    }

    /// `accept_resumption` + `next_message` returns `Sigma2_Resume` bytes and
    /// transitions to Complete; `finish` returns a resumption record.
    #[test]
    fn accept_resumption_then_next_message_returns_sigma2_resume() {
        use crate::case::messages::Sigma2Resume;

        let ipk = [0xAB; 16];
        let rcac_pub = dummy_rcac_pub();
        let creds = make_test_credentials(0x1234, 0x5678, ipk, rcac_pub);
        let mut responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();

        let (sigma1_bytes, record, _) =
            build_valid_resumption_setup(&ipk, &rcac_pub, 0x1234, 0x5678);
        let old_id = record.id;

        let outcome = responder.handle_sigma1(&sigma1_bytes).unwrap();
        assert!(matches!(outcome, Sigma1Outcome::ResumptionRequested { .. }));

        responder.accept_resumption(record).unwrap();

        // next_message() must return Sigma2_Resume bytes.
        let outbound = responder.next_message().unwrap();
        assert_ne!(outbound, b"");

        // The bytes must decode as a Sigma2_Resume.
        let sigma2_resume = Sigma2Resume::decode(&outbound).unwrap();

        // The new resumption_id must differ from the old one (it was freshly generated).
        assert_ne!(
            sigma2_resume.resumption_id, old_id.0,
            "Sigma2_Resume must carry a fresh resumption_id"
        );

        // finish() must succeed and carry a resumption_record with the new id.
        let output = responder.finish().unwrap();
        let next_record = output.resumption_record.unwrap();
        assert_eq!(
            next_record.id.0, sigma2_resume.resumption_id,
            "CaseSessionOutput resumption_record.id must match Sigma2_Resume resumption_id"
        );
    }

    // ─── `State`'s Debug names the variant only ───────────────────────────
    //
    // Most `State` variants hold secrets (the operational signing key, the IPK,
    // the ephemeral secret, the ECDH shared secret, session keys). Nothing
    // formats `State` today, but a future `#[derive(Debug)]` on the public state
    // machine would print it, so `State`'s own `Debug` prints the variant name
    // and nothing else. Every check compares the whole rendering against the
    // bare name, so no field can be present. Static messages only: a failing
    // assertion never prints the state.

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
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let responder = CaseResponder::new(
            creds,
            empty_roots(),
            0x0002,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        assert_state_shows_only(&responder.state, "AwaitingSigma1");
        assert_state_shows_only(&State::Poisoned, "Poisoned");
    }
}
