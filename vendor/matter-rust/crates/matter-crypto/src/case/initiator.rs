//! Initiator-side CASE state machine.
//!
//! Drives the 3-message Sigma1 / Sigma2 / Sigma3 handshake from the
//! initiator's perspective. Sans-IO: the caller is responsible for
//! transmitting and receiving bytes; this module only handles the
//! cryptographic state transitions.
//!
//! # Protocol flow (new-session path — Matter Core Spec §4.13.2.4)
//!
//! ```text
//! Initiator (us)                    Responder
//! ─────────────────────────────────────────────────────────
//! new() / new_using_rng()
//! start()
//!   → Sigma1  ──────────────────────────────────────────>
//!              <──────────────────────────────── Sigma2
//! handle_sigma2()
//! next_message()
//!   → Sigma3  ──────────────────────────────────────────>
//!              <──────────────────────────────── StatusReport: Success
//! finish() → CaseSessionOutput
//! ```
//!
//! # Protocol flow (resumption path — Matter Core Spec §4.13.2.4)
//!
//! ```text
//! Initiator (us)                    Responder
//! ─────────────────────────────────────────────────────────
//! new_with_resumption() / new_with_resumption_using_rng()
//! start()
//!   → Sigma1 (with resumption_id + initiator_resume_mic)  ──>
//!
//!   Case A — responder accepts resumption:
//!              <──────────────── Sigma2_Resume
//! handle_sigma2_resume()
//! finish() → CaseSessionOutput   (NO Sigma3 or Sigma3_Resume to send)
//!
//!   Case B — responder declines (no matching record): sends normal Sigma2
//!              <──────────────── Sigma2
//! handle_sigma2()               (falls back to the new-session path)
//! next_message()
//!   → Sigma3  ──────────────────────────────────────────>
//! finish() → CaseSessionOutput
//! ```
//!
//! **Note:** `Sigma3_Resume` does NOT exist as a wire message. After
//! `handle_sigma2_resume` the initiator transitions directly to `Complete`;
//! the implicit mutual-key-confirmation is the first encrypted M5 message.
//!
//! # KDF inputs (pinned from matter.js `CaseClient.ts` + `NodeSession.ts`)
//!
//! ## `DestinationId` (§4.13.2.4 step 1)
//!
//! ```text
//! salt = initiatorRandom(32) || rcacPublicKey(65) || fabricId_le8 || nodeId_le8
//! DestinationId = HMAC-SHA256(IPK, salt)
//! ```
//! Pinned from `Fabric.ts#generateSalt` + `signHmac(IPK, salt)`.
//!
//! ## S2K — Sigma2 TBE decryption key
//!
//! ```text
//! sigma2Salt = IPK(16) || responderRandom(32) || responderEphPub(65) || SHA-256(s1_bytes)
//! S2K = HKDF(secret=sharedSecret, salt=sigma2Salt, info="Sigma2", len=16)
//! ```
//! Pinned from `CaseClient.ts` lines 193–199.
//!
//! ## S3K — Sigma3 TBE encryption key
//!
//! ```text
//! sigma3Salt = IPK(16) || SHA-256(s1_bytes || s2_bytes)
//! S3K = HKDF(secret=sharedSecret, salt=sigma3Salt, info="Sigma3", len=16)
//! ```
//! Pinned from `CaseClient.ts` lines 244–248.
//!
//! ## Session keys
//!
//! ```text
//! sessionSalt = IPK(16) || SHA-256(s1_bytes || s2_bytes || s3_bytes)
//! keys(48) = HKDF(secret=sharedSecret, salt=sessionSalt, info="SessionKeys", len=48)
//! i2r_key = keys[0..16]   (initiator to responder encrypt key)
//! r2i_key = keys[16..32]  (responder to initiator decrypt key)
//! attestation_challenge = keys[32..48]
//! ```
//! Pinned from `NodeSession.ts` lines 61–82 (`isInitiator=true` branch).
//!
//! ## `TBEData2` layout (plaintext after S2K decrypt)
//!
//! ```text
//! TlvEncryptedDataSigma2 = {
//!     1: responderNoc (bytes),
//!     2: responderIcac (bytes, optional),
//!     3: signature (64 bytes),
//!     4: resumptionId (16 bytes),
//! }
//! ```
//!
//! ## `TBSData2` (signed payload verified from peer's NOC key)
//!
//! ```text
//! TlvSignedData = {
//!     1: responderNoc (bytes),
//!     2: responderIcac (bytes, optional),
//!     3: responderPublicKey (65 bytes) = responderEphPub,
//!     4: initiatorPublicKey (65 bytes) = initiatorEphPub,
//! }
//! ```
//! Pinned from `CaseMessages.ts` (`TlvSignedData`).
//!
//! ## `TBEData3` layout (plaintext before S3K encrypt)
//!
//! ```text
//! TlvEncryptedDataSigma3 = {
//!     1: responderNoc (bytes) = our NOC,
//!     2: responderIcac (bytes, optional) = our ICAC,
//!     3: signature (64 bytes),
//! }
//! ```
//!
//! ## `TBSData3` (what we sign with our NOC key)
//!
//! ```text
//! TlvSignedData = {
//!     1: responderNoc (bytes) = our NOC,
//!     2: responderIcac (bytes, optional) = our ICAC,
//!     3: responderPublicKey (65 bytes) = our ephemeral pub,
//!     4: initiatorPublicKey (65 bytes) = peer's ephemeral pub,
//! }
//! ```
//! Note: in Sigma3 the initiator plays the role of "responder" in `TlvSignedData`
//! because the field names in matter.js's `TlvSignedData` were defined from the
//! SIGMA-I Sigma2 perspective; `TlvSignedData` is re-used symmetrically in Sigma3.
//! Pinned from `CaseClient.ts` lines 249–254.

use p256::SecretKey;
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::Zeroizing;

use matter_cert::{CertificateChain, MatterCertificate, MatterTime, Signature, TrustedRoots};

use crate::case::messages::{
    SessionParams, Sigma1, Sigma2, Sigma2Resume, Sigma3, SIGMA1_SESSION_PARAMS_TAG,
};
use crate::case::sigma::{
    aead_decrypt, aead_encrypt, compute_dest_id, compute_sigma1_resume_mic, decode_tbedata2,
    derive_resume_session_keys, ecdh_shared_secret, encode_tbedata3, encode_tbs_data,
    generate_ephemeral_keypair, hkdf_derive, transcript_hash, verify_sigma2_resume_mic,
    AEAD_KEY_LEN, HKDF_INFO_SIGMA2, HKDF_INFO_SIGMA3, NONCE_TBE_DATA2, NONCE_TBE_DATA3,
};
use crate::case::{
    CaseCredentials, CaseMessageKind, CaseSessionKeys, CaseSessionOutput, LocalInfo, PeerInfo,
    ResumptionId, ResumptionRecord,
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

/// Internal states of the initiator-side CASE handshake.
///
/// Named for the *next expected* action at each point.
/// `Poisoned` is a sentinel used during `std::mem::replace` transitions;
/// it is never observable to callers (all methods replace it immediately
/// with either the next real state or an error return).
enum State {
    /// Initial state: `start()` has not been called yet.
    ///
    /// The ephemeral keypair and initiator random are pre-sampled here so
    /// that `start()` cannot fail due to randomness.
    AwaitingStart {
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        peer_node_id: u64,
        peer_fabric_id: u64,
        eph_secret: SecretKey,
        eph_pub: [u8; 65],
        initiator_random: [u8; 32],
        initiator_session_id: u16,
        /// When `Some`, the caller supplied a prior-session record and `start()`
        /// will populate Sigma1's resumption fields from it.
        resumption_record: Option<ResumptionRecord>,
    },

    /// `start()` emitted Sigma1; waiting for the responder's Sigma2 (or
    /// `Sigma2_Resume` if `resumption_attempt` is `Some`).
    AwaitingSigma2 {
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        peer_node_id: u64,
        peer_fabric_id: u64,
        eph_secret: SecretKey,
        eph_pub: [u8; 65],
        /// Used for resumption MIC computation and for the fallback new-session
        /// `Sigma2` path. Always present on the wire in `sigma1.initiator_random`.
        initiator_random: [u8; 32],
        initiator_session_id: u16,
        sigma1_bytes: Vec<u8>,
        /// When `Some`, the Sigma1 we sent included resumption fields.
        /// `handle_sigma2_resume` consumes this; `handle_sigma2` discards it.
        resumption_attempt: Option<ResumptionRecord>,
    },

    /// `handle_sigma2()` has processed Sigma2 and produced Sigma3 + session
    /// keys; `next_message()` will hand off the Sigma3 bytes and move to
    /// `Complete`.
    ReadyToSendSigma3 {
        sigma3_bytes: Vec<u8>,
        session_keys: CaseSessionKeys,
        peer: PeerInfo,
        local: LocalInfo,
        /// Record built in `process_sigma2` from the responder's fresh
        /// `resumption_id` (`TBEData2`) + this session's ECDH secret; carried
        /// through to `Complete` for the caller to persist.
        resumption_record: Option<ResumptionRecord>,
    },

    /// `next_message()` has emitted Sigma3 (or `handle_sigma2_resume` completed
    /// the resumption path); `finish()` may be called.
    Complete {
        session_keys: CaseSessionKeys,
        peer: PeerInfo,
        local: LocalInfo,
        /// Resumption record for the caller to persist. `None` when the responder
        /// did not supply resumption-supporting session parameters. On the resumption
        /// path this carries the updated record with the new ID from `Sigma2_Resume`.
        resumption_record: Option<ResumptionRecord>,
    },

    /// Sentinel during `std::mem::replace` transitions.
    Poisoned,
}

/// Names the variant only. The operational signing key, the IPK, the
/// ephemeral secret, the session keys and the resumption record live in
/// these variants' fields, but each of those already prints redacted or
/// opaquely, so a derived `Debug` would have exposed essentially nothing
/// secret here. Printing the variant name alone keeps it that way whatever
/// field is added later, and matches the other three handshake machines.
/// Written by hand, not derived, so that a `Debug` added to the public state
/// machine later cannot print a secret either.
impl core::fmt::Debug for State {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::AwaitingStart { .. } => "AwaitingStart",
            Self::AwaitingSigma2 { .. } => "AwaitingSigma2",
            Self::ReadyToSendSigma3 { .. } => "ReadyToSendSigma3",
            Self::Complete { .. } => "Complete",
            Self::Poisoned => "Poisoned",
        })
    }
}

// ---------------------------------------------------------------------------
// CaseInitiator
// ---------------------------------------------------------------------------

/// Initiator-side CASE state machine (new-session and resumption paths).
///
/// Drives the Sigma1 / Sigma2 / Sigma3 handshake (or the faster
/// Sigma1 / `Sigma2_Resume` resumption path) from the initiator's perspective.
/// Sans-IO: the caller feeds raw bytes in via [`handle_sigma2`][Self::handle_sigma2]
/// or [`handle_sigma2_resume`][Self::handle_sigma2_resume] and reads raw bytes
/// out via [`start`][Self::start] and [`next_message`][Self::next_message].
///
/// # Construction
///
/// New-session path:
/// - [`CaseInitiator::new`] — production constructor; uses the OS CSPRNG.
/// - `new_using_rng` (crate-internal) — deterministic constructor for tests.
///
/// Resumption path (M4.2):
/// - [`CaseInitiator::new_with_resumption`] — production constructor with a
///   prior-session [`ResumptionRecord`].
/// - `new_with_resumption_using_rng` (crate-internal) — deterministic variant.
///
/// # Driving the new-session handshake
///
/// 1. Call [`start`][Self::start] → get Sigma1 bytes; send them.
/// 2. Receive Sigma2 bytes from the peer.
/// 3. Call [`handle_sigma2`][Self::handle_sigma2] with those bytes.
/// 4. Call [`next_message`][Self::next_message] → get Sigma3 bytes; send them.
/// 5. After the peer confirms with a `StatusReport: Success`, call
///    [`finish`][Self::finish] to retrieve [`CaseSessionOutput`].
///
/// # Driving the resumption handshake
///
/// 1. Call [`start`][Self::start] → get Sigma1 bytes (with resumption fields); send them.
/// 2. Receive the response from the peer:
///    - If the peer accepts resumption: call [`handle_sigma2_resume`][Self::handle_sigma2_resume].
///      Then call [`finish`][Self::finish] directly (no Sigma3 to send).
///    - If the peer declines (sends a regular Sigma2): call [`handle_sigma2`][Self::handle_sigma2]
///      normally, then [`next_message`][Self::next_message] and [`finish`][Self::finish].
///
/// Use [`expected_inbound`][Self::expected_inbound] at any point to query
/// which message the machine is currently waiting to receive.
pub struct CaseInitiator {
    state: State,
    /// The responder's advertised [`SessionParameters`](crate::SessionParameters)
    /// from Sigma2 (context tag 5) or `Sigma2_Resume` (context tag **4**),
    /// readable via [`peer_session_params`][Self::peer_session_params].
    ///
    /// Kept on the struct rather than in [`State`] so it survives every
    /// transition and does not have to be threaded through each variant.
    peer_session_params: Option<crate::SessionParameters>,
    /// What *we* advertise in Sigma1's `initiatorSessionParams` (context tag
    /// 5). `None` — the default — omits the optional element entirely, which
    /// is what this crate did before the builder existed and keeps every
    /// existing byte-parity fixture valid.
    ///
    /// Set via [`with_session_params`][Self::with_session_params]. This crate
    /// deliberately does not pick the values itself: the data-model and
    /// interaction-model revisions are properties of the *caller's*
    /// implementation, not of the crypto layer, and inventing them here would
    /// let them drift from what the caller actually puts on the wire.
    local_session_params: Option<crate::SessionParameters>,
    /// Wall-clock instant at which inbound peer certificate chains are checked
    /// for temporal validity (`not_before <= now <= not_after`). Injected at
    /// construction so this crate never reads the system clock itself — the
    /// controller layer supplies the real time. See `process_sigma2`.
    validation_time: MatterTime,
}

impl CaseInitiator {
    // ─── Public constructors ──────────────────────────────────────────────

    /// Construct an initiator using the OS CSPRNG (new-session path).
    ///
    /// Pre-samples the ephemeral keypair and 32-byte initiator random so that
    /// [`start`][Self::start] cannot fail due to randomness.
    ///
    /// `initiator_session_id` is the non-zero secured-session id this initiator
    /// advertises in Sigma1 (tag 2) for the peer to address us by; it is recorded
    /// as `CaseSessionOutput.local.session_id` once the handshake completes.
    ///
    /// For the resumption path, use [`new_with_resumption`][Self::new_with_resumption].
    ///
    /// `now` is the wall-clock instant against which the peer's operational
    /// certificate chain is checked for temporal validity during Sigma2. This
    /// crate never reads the system clock; the caller (controller layer) must
    /// supply the real time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EphemeralKeyGenerationFailed`] if the OS RNG fails
    /// (extremely unlikely in practice).
    pub fn new(
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        peer_node_id: u64,
        peer_fabric_id: u64,
        initiator_session_id: u16,
        now: MatterTime,
    ) -> Result<Self> {
        let rng = SystemRandom::new();
        Self::new_inner(
            credentials,
            trusted_roots,
            peer_node_id,
            peer_fabric_id,
            initiator_session_id,
            None,
            now,
            &rng,
        )
    }

    /// Deterministic constructor for testing — accepts an injectable RNG.
    ///
    /// Production code should always use [`new`][Self::new].
    ///
    /// # Errors
    ///
    /// Returns [`Error::EphemeralKeyGenerationFailed`] if the RNG fails.
    // Used in the case roundtrip integration test (tests/case_roundtrip.rs).
    #[allow(dead_code)]
    pub(crate) fn new_using_rng(
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        peer_node_id: u64,
        peer_fabric_id: u64,
        now: MatterTime,
        rng: &dyn SecureRandom,
    ) -> Result<Self> {
        Self::new_inner(
            credentials,
            trusted_roots,
            peer_node_id,
            peer_fabric_id,
            0,
            None,
            now,
            rng,
        )
    }

    /// Construct an initiator with a prior-session [`ResumptionRecord`], using
    /// the OS CSPRNG.
    ///
    /// When [`start`][Self::start] is called, the Sigma1 message will include
    /// `resumption_id` (tag 6) and `initiator_resume_mic` (tag 7). The
    /// responder may reply with `Sigma2_Resume` (call
    /// [`handle_sigma2_resume`][Self::handle_sigma2_resume]) or fall back to a
    /// regular `Sigma2` (call [`handle_sigma2`][Self::handle_sigma2]).
    ///
    /// `initiator_session_id` is the non-zero secured-session id we advertise
    /// in Sigma1 (tag 2) for the peer to address us by, exactly as in
    /// [`new`][Self::new].
    ///
    /// `now` is the wall-clock instant against which the peer's operational
    /// certificate chain is checked for temporal validity during Sigma2 (used
    /// only on the non-resumption fallback path). See [`new`][Self::new].
    ///
    /// # Errors
    ///
    /// Returns [`Error::EphemeralKeyGenerationFailed`] if the OS RNG fails.
    pub fn new_with_resumption(
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        peer_node_id: u64,
        peer_fabric_id: u64,
        record: ResumptionRecord,
        initiator_session_id: u16,
        now: MatterTime,
    ) -> Result<Self> {
        let rng = SystemRandom::new();
        Self::new_with_resumption_using_rng(
            credentials,
            trusted_roots,
            peer_node_id,
            peer_fabric_id,
            record,
            initiator_session_id,
            now,
            &rng,
        )
    }

    /// Deterministic resumption constructor for testing — accepts an injectable
    /// RNG.
    ///
    /// Production code should always use
    /// [`new_with_resumption`][Self::new_with_resumption].
    ///
    /// # Errors
    ///
    /// Returns [`Error::EphemeralKeyGenerationFailed`] if the RNG fails.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_resumption_using_rng(
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        peer_node_id: u64,
        peer_fabric_id: u64,
        record: ResumptionRecord,
        initiator_session_id: u16,
        now: MatterTime,
        rng: &dyn SecureRandom,
    ) -> Result<Self> {
        Self::new_inner(
            credentials,
            trusted_roots,
            peer_node_id,
            peer_fabric_id,
            initiator_session_id,
            Some(record),
            now,
            rng,
        )
    }

    // Deterministic-input constructors below are used only by
    // `crate::test_support`, which is itself behind the `test-support`
    // feature. Gated to match, so a default-feature build does not compile
    // them as dead code.
    /// Deterministic new-session constructor for byte-parity testing — injects
    /// a pre-computed ephemeral private key and initiator random, bypassing
    /// the RNG entirely.
    ///
    /// This mirrors `new_using_rng` but derives the ephemeral public key
    /// from the supplied private key bytes rather than sampling from an RNG.
    /// The only valid caller is `test_support::case_initiator_with_eph_key`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EphemeralKeyGenerationFailed`] if `eph_private_key`
    /// is zero, >= the P-256 curve order, or otherwise not a valid scalar.
    #[cfg(feature = "test-support")]
    pub(crate) fn new_with_eph_and_random(
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        peer_node_id: u64,
        peer_fabric_id: u64,
        eph_private_key: [u8; 32],
        initiator_random: [u8; 32],
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
            state: State::AwaitingStart {
                credentials,
                trusted_roots,
                peer_node_id,
                peer_fabric_id,
                eph_secret,
                eph_pub,
                initiator_random,
                initiator_session_id: 0,
                resumption_record: None,
            },
            validation_time: now,
            peer_session_params: None,
            local_session_params: None,
        })
    }

    /// Deterministic resumption constructor for byte-parity testing — injects
    /// a pre-computed ephemeral private key and initiator random, bypassing
    /// the RNG entirely.
    ///
    /// Same as `new_with_eph_and_random` but includes a prior-session
    /// `ResumptionRecord` so that Sigma1 carries resumption fields.
    /// The only valid caller is `test_support::case_initiator_with_resumption_eph_key`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EphemeralKeyGenerationFailed`] if `eph_private_key`
    /// is zero, >= the P-256 curve order, or otherwise not a valid scalar.
    #[allow(clippy::too_many_arguments)]
    #[cfg(feature = "test-support")]
    pub(crate) fn new_with_resumption_eph_and_random(
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        peer_node_id: u64,
        peer_fabric_id: u64,
        record: ResumptionRecord,
        eph_private_key: [u8; 32],
        initiator_random: [u8; 32],
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
            state: State::AwaitingStart {
                credentials,
                trusted_roots,
                peer_node_id,
                peer_fabric_id,
                eph_secret,
                eph_pub,
                initiator_random,
                initiator_session_id: 0,
                resumption_record: Some(record),
            },
            validation_time: now,
            peer_session_params: None,
            local_session_params: None,
        })
    }

    /// Internal shared constructor: produces an `AwaitingStart` state with
    /// an optional resumption record baked in.
    ///
    /// Called by all four public/crate-internal constructors.
    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        peer_node_id: u64,
        peer_fabric_id: u64,
        initiator_session_id: u16,
        resumption_record: Option<ResumptionRecord>,
        now: MatterTime,
        rng: &dyn SecureRandom,
    ) -> Result<Self> {
        let (eph_secret, eph_pub) = generate_ephemeral_keypair(rng)?;
        let mut initiator_random = [0u8; 32];
        rng.fill(&mut initiator_random)
            .map_err(|_| Error::EphemeralKeyGenerationFailed)?;
        Ok(Self {
            state: State::AwaitingStart {
                credentials,
                trusted_roots,
                peer_node_id,
                peer_fabric_id,
                eph_secret,
                eph_pub,
                initiator_random,
                initiator_session_id,
                resumption_record,
            },
            validation_time: now,
            peer_session_params: None,
            local_session_params: None,
        })
    }

    /// Advertise `params` to the responder in Sigma1's optional
    /// `initiatorSessionParams` element (context tag 5).
    ///
    /// Must be called before [`start`][Self::start]; afterwards the Sigma1
    /// bytes are already built and this has no effect.
    ///
    /// # Why advertise at all
    ///
    /// Both reference implementations always send this element — chip treats a
    /// missing local config as a hard error (`CASESession::EncodeSigma1`).
    /// Omitting it is still spec-conformant, but it costs you: the peer sizes
    /// its own Sigma2/Sigma3-ack retransmits to the spec defaults instead of
    /// to your timings, and assumes `maxPathsPerInvoke = 1` and pre-1.3
    /// behaviour. There is no mDNS channel for a commissioner's MRP
    /// parameters, so this element is the only way to say otherwise.
    ///
    /// An [empty](crate::SessionParameters::is_empty) `params` omits the
    /// element rather than emitting `{}`.
    #[must_use]
    pub fn with_session_params(mut self, params: crate::SessionParameters) -> Self {
        self.local_session_params = (!params.is_empty()).then_some(params);
        self
    }

    // ─── State inspection ─────────────────────────────────────────────────

    /// The responder's advertised [`SessionParameters`](crate::SessionParameters),
    /// captured from Sigma2's `responderSessionParams` (context tag 5) or from
    /// `Sigma2_Resume`'s (context tag **4** — the numbering differs between the
    /// two messages).
    ///
    /// `None` before the Sigma2 step has run, and also when the responder
    /// omitted the element.
    ///
    /// # Why the initiator wants this
    ///
    /// It is the device's own statement of its MRP timings, and it is more
    /// trustworthy than the mDNS TXT `SII`/`SAI`/`SAT` a connect starts from:
    /// the advertisement can be stale, cached, or absent entirely. chip
    /// re-sizes the exchange from this value after validating Sigma2 and
    /// *before* sending Sigma3 (`CASESession::HandleSigma2`), so Sigma3's
    /// retransmits are sized to what the device just said rather than to what
    /// mDNS said earlier.
    ///
    /// Merge it over the discovery-derived config rather than replacing that
    /// config wholesale — see `MrpConfig::merge_session_params`.
    #[must_use]
    pub fn peer_session_params(&self) -> Option<crate::SessionParameters> {
        self.peer_session_params
    }

    /// Returns the CASE message kind the machine is currently waiting to
    /// receive, or `None` if the machine is in an outbound-only state,
    /// has completed, or has been poisoned.
    ///
    /// On the resumption path (after `start()` was called with a resumption
    /// record), returns `Sigma2Resume` to indicate that the peer may send either
    /// `Sigma2_Resume` (accepted) or a plain `Sigma2` (declined fallback). The
    /// returned value is advisory — the caller must inspect the actual inbound
    /// message type and route to the appropriate `handle_*` method.
    pub fn expected_inbound(&self) -> Option<CaseMessageKind> {
        match &self.state {
            State::AwaitingSigma2 {
                resumption_attempt: Some(_),
                ..
            } => Some(CaseMessageKind::Sigma2Resume),
            State::AwaitingSigma2 {
                resumption_attempt: None,
                ..
            } => Some(CaseMessageKind::Sigma2),
            _ => None,
        }
    }

    // ─── Handshake methods ────────────────────────────────────────────────

    /// Produce the Sigma1 message bytes and advance to `AwaitingSigma2`.
    ///
    /// On the resumption path (constructed with
    /// [`new_with_resumption`][Self::new_with_resumption]), the emitted Sigma1
    /// will include `resumption_id` (tag 6) and `initiator_resume_mic` (tag 7),
    /// signalling to the responder that it may send `Sigma2_Resume` instead of
    /// `Sigma2`.
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedCaseMessage`] if called from the wrong state.
    /// - [`Error::Codec`] on TLV encoding failure.
    /// - [`Error::EphemeralKeyGenerationFailed`] if MIC computation fails
    ///   (only possible if AES-CCM internal state is inconsistent — not expected
    ///   in practice).
    pub fn start(&mut self) -> Result<Vec<u8>> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::AwaitingStart {
                credentials,
                trusted_roots,
                peer_node_id,
                peer_fabric_id,
                eph_secret,
                eph_pub,
                initiator_random,
                initiator_session_id,
                resumption_record,
            } => {
                let dest_id = compute_dest_id(
                    &credentials.ipk,
                    &credentials.rcac_public_key,
                    credentials.fabric_id,
                    peer_node_id,
                    &initiator_random,
                );

                // Populate resumption fields when we have a prior-session record.
                // `sigma1_resume_mic` is derived from the shared_secret and the OLD
                // resumption_id (the one already stored in the record), using the
                // freshly-sampled initiator_random as part of the HKDF salt.
                // This lets the responder verify we hold the correct shared secret
                // without exposing the secret itself.
                let (resumption_id_field, initiator_resume_mic_field) = match &resumption_record {
                    Some(record) => {
                        let mic = compute_sigma1_resume_mic(
                            &record.shared_secret,
                            &initiator_random,
                            &record.id.0,
                        )?;
                        (Some(record.id.0), Some(mic))
                    }
                    None => (None, None),
                };

                // Encode our own advertisement, if the caller set one.
                // Sigma1 carries it at context tag 5.
                let initiator_session_params = self
                    .local_session_params
                    .map(|p| p.encode(SIGMA1_SESSION_PARAMS_TAG).map(SessionParams::from))
                    .transpose()?;

                let sigma1 = Sigma1 {
                    initiator_random,
                    initiator_session_id,
                    dest_id,
                    initiator_eph_pub: eph_pub,
                    initiator_session_params,
                    resumption_id: resumption_id_field,
                    initiator_resume_mic: initiator_resume_mic_field,
                };
                let sigma1_bytes = sigma1.encode()?;

                self.state = State::AwaitingSigma2 {
                    credentials,
                    trusted_roots,
                    peer_node_id,
                    peer_fabric_id,
                    eph_secret,
                    eph_pub,
                    initiator_random,
                    initiator_session_id,
                    sigma1_bytes: sigma1_bytes.clone(),
                    resumption_attempt: resumption_record,
                };
                Ok(sigma1_bytes)
            }
            other => {
                self.state = other;
                Err(Error::UnexpectedCaseMessage {
                    expected: CaseMessageKind::Sigma1,
                    got: CaseMessageKind::Sigma2,
                })
            }
        }
    }

    /// Process the inbound Sigma2 message, verify the peer's credentials,
    /// and produce Sigma3.
    ///
    /// After this call succeeds, call [`next_message`][Self::next_message] to
    /// retrieve the Sigma3 bytes that must be sent to the responder.
    ///
    /// # Sigma2 processing steps
    ///
    /// 1. Parse Sigma2 TLV.
    /// 2. ECDH shared secret from our ephemeral secret + peer's ephemeral
    ///    public key.
    /// 3. Derive S2K via HKDF (see module doc for salt composition).
    /// 4. AES-128-CCM decrypt the encrypted blob using S2K and the
    ///    `NCASE_Sigma2N` nonce.
    /// 5. Parse `TBEData2` = `{ responderNoc, responderIcac?, signature,
    ///    resumptionId }`.
    /// 6. Validate the peer's NOC chain against `trusted_roots`.
    /// 7. Check that the NOC's `NodeId` and `FabricId` match expectations.
    /// 8. Verify the peer's ECDSA signature over `TBSData2`.
    /// 9. Build `TBSData3`, sign with our NOC's private key.
    /// 10. Encode `TBEData3`, encrypt with S3K and `NCASE_Sigma3N` nonce.
    /// 11. Derive the final session keys.
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedCaseMessage`] if called from the wrong state.
    /// - [`Error::Codec`] on TLV decode / encode failure.
    /// - [`Error::InvalidParameter`] if the peer's ephemeral public key is
    ///   not a valid P-256 point.
    /// - [`Error::EncryptedBlobDecryptionFailed`] if the encrypted blob
    ///   fails AEAD verification.
    /// - [`Error::InvalidPeerNocChain`] if chain validation fails.
    /// - [`Error::FabricIdMismatch`] / [`Error::PeerNodeIdMismatch`] if the
    ///   peer's identity doesn't match expectations.
    /// - [`Error::PeerSignatureInvalid`] if the peer's ECDSA signature fails.
    /// - [`Error::SigningFailed`] if our own signing operation fails.
    /// - [`Error::EphemeralKeyGenerationFailed`] on HKDF failure.
    pub fn handle_sigma2(&mut self, bytes: &[u8]) -> Result<()> {
        let now = self.validation_time;
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::AwaitingSigma2 {
                credentials,
                trusted_roots,
                peer_node_id,
                peer_fabric_id,
                eph_secret,
                eph_pub,
                initiator_random: _,
                initiator_session_id,
                sigma1_bytes,
                resumption_attempt: _, // Responder declined (or never attempted) — discard.
            } => {
                let processed = process_sigma2(
                    bytes,
                    &credentials,
                    &trusted_roots,
                    peer_node_id,
                    peer_fabric_id,
                    &eph_secret,
                    &eph_pub,
                    initiator_session_id,
                    &sigma1_bytes,
                    now,
                )?;
                self.peer_session_params = processed.peer_session_params;
                self.state = State::ReadyToSendSigma3 {
                    sigma3_bytes: processed.sigma3_bytes,
                    session_keys: processed.session_keys,
                    peer: processed.peer,
                    local: processed.local,
                    resumption_record: Some(processed.resumption_record),
                };
                Ok(())
            }
            other => {
                self.state = other;
                Err(Error::UnexpectedCaseMessage {
                    expected: CaseMessageKind::Sigma2,
                    got: CaseMessageKind::Sigma3,
                })
            }
        }
    }

    /// Process the inbound `Sigma2_Resume` message and complete the resumption
    /// handshake.
    ///
    /// May only be called after [`start`][Self::start] when the initiator was
    /// constructed with [`new_with_resumption`][Self::new_with_resumption] (i.e.,
    /// the sent Sigma1 carried resumption fields).
    ///
    /// **No `Sigma3_Resume` or `Sigma3` to send.** After this call succeeds the
    /// handshake is complete from the initiator's side. Call [`finish`][Self::finish]
    /// directly to obtain the [`CaseSessionOutput`].
    ///
    /// # Resumption session-key layout
    ///
    /// Pinned from matter.js `NodeSession.create` (`isResumption = true` branch):
    /// ```text
    /// keys = HKDF(ikm  = shared_secret,
    ///             salt = initiatorRandom || OLD_resumption_id,
    ///             info = "SessionResumptionKeys",
    ///             len  = 48)
    /// keys[0..16]  → r2i_key          (responder-to-initiator)
    /// keys[16..32] → i2r_key          (initiator-to-responder)
    /// keys[32..48] → attestation_challenge
    /// ```
    /// Note the **reversed** byte assignment vs the new-session path
    /// (where `keys[0..16]` is `i2r` and `keys[16..32]` is `r2i`).
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedCaseMessage`] if called from the wrong state or if
    ///   the initiator never attempted resumption.
    /// - [`Error::Codec`] on TLV decode failure.
    /// - [`Error::ResumptionMacMismatch`] if the `sigma2_resume_mic` in the
    ///   message does not verify.
    /// - [`Error::EphemeralKeyGenerationFailed`] on HKDF failure.
    pub fn handle_sigma2_resume(&mut self, bytes: &[u8]) -> Result<()> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::AwaitingSigma2 {
                credentials,
                initiator_random,
                initiator_session_id,
                resumption_attempt: Some(record),
                // The new-session fields below are not needed for the resumption
                // path but must be destructured to satisfy exhaustiveness.
                trusted_roots: _,
                peer_node_id: _,
                peer_fabric_id: _,
                eph_secret: _,
                eph_pub: _,
                sigma1_bytes: _,
            } => {
                let sigma2_resume = Sigma2Resume::decode(bytes)?;
                // Note the container tag: `Sigma2_Resume` carries
                // `responderSessionParams` at context tag **4**, where Sigma1
                // and Sigma2 use 5. The decode is driven by the message type,
                // so the number cannot be crossed here.
                self.peer_session_params = sigma2_resume.session_parameters()?;
                let new_resumption_id = sigma2_resume.resumption_id;

                // Step 1: Verify sigma2_resume_mic.
                // The MIC is computed over the NEW resumption_id (generated by
                // the responder for this session), using initiatorRandom as part
                // of the HKDF salt. This proves the responder holds the same
                // shared_secret as the record.
                verify_sigma2_resume_mic(
                    &record.shared_secret,
                    &initiator_random,
                    &new_resumption_id,
                    &sigma2_resume.resume_mic,
                )?;

                // Step 2: Derive resumed session keys.
                // Salt uses initiatorRandom || OLD resumptionId (record.id.0).
                // info = "SessionResumptionKeys", len = 48.
                // Key layout: [0..16]=i2r, [16..32]=r2i, [32..48]=attestation —
                // the SAME as the new-session layout (chip's
                // CryptoContext::InitFromSecret splits I2RKey || R2IKey ||
                // AttestationChallenge for kSessionResumption too; live-verified
                // against chip's OTA requestor).
                let blob = derive_resume_session_keys(
                    &record.shared_secret,
                    &initiator_random,
                    &record.id.0,
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

                // Step 3: Build the next resumption record.
                // The responder supplied a fresh resumption_id (new_resumption_id)
                // for use in the next resumption attempt. The shared_secret is
                // re-used unchanged — matter.js does not re-derive it on resumption.
                // (If this turns out to be wrong, M4.3 byte-parity testing will
                // surface it; setting it to None is the safe conservative fallback,
                // but re-using is the observed matter.js behaviour.)
                let next_record = ResumptionRecord {
                    id: ResumptionId(new_resumption_id),
                    shared_secret: record.shared_secret, // re-use unchanged
                    peer: record.peer.clone(),
                    expires_at: None, // M6 commissioning sets a real expiry.
                };

                // Step 4: Build peer / local identity structs.
                // The resumption path re-uses the cached peer identity from the
                // record (we didn't verify a fresh NOC chain — that's the point of
                // resumption). The peer's session ID comes from Sigma2_Resume.
                let peer = PeerInfo {
                    session_id: sigma2_resume.responder_session_id,
                    // `record` is `Drop` (ZeroizeOnDrop), so its non-`Copy`
                    // fields cannot be moved out — clone the peer identity.
                    ..record.peer.clone()
                };
                let local = LocalInfo {
                    node_id: credentials.node_id,
                    fabric_id: credentials.fabric_id,
                    session_id: initiator_session_id,
                };

                // No Sigma3_Resume — transition directly to Complete.
                self.state = State::Complete {
                    session_keys,
                    peer,
                    local,
                    resumption_record: Some(next_record),
                };
                Ok(())
            }

            // Initiator never attempted resumption; receiving Sigma2_Resume is a
            // protocol violation. State is left Poisoned (unrecoverable).
            State::AwaitingSigma2 {
                resumption_attempt: None,
                ..
            } => Err(Error::UnexpectedCaseMessage {
                expected: CaseMessageKind::Sigma2,
                got: CaseMessageKind::Sigma2Resume,
            }),

            // Any other state is also invalid.
            other => {
                self.state = other;
                Err(Error::UnexpectedCaseMessage {
                    expected: CaseMessageKind::Sigma2Resume,
                    got: CaseMessageKind::Sigma2Resume,
                })
            }
        }
    }

    /// Retrieve the next outbound message (Sigma3) and advance to `Complete`.
    ///
    /// Must be called after a successful [`handle_sigma2`][Self::handle_sigma2].
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedCaseMessage`] if called from the wrong state.
    pub fn next_message(&mut self) -> Result<Vec<u8>> {
        let prev = std::mem::replace(&mut self.state, State::Poisoned);
        match prev {
            State::ReadyToSendSigma3 {
                sigma3_bytes,
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
                Ok(sigma3_bytes)
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
    /// May only be called after [`next_message`][Self::next_message] has
    /// emitted Sigma3 (i.e., the state machine is in the `Complete` state).
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
// Helper: Sigma2 processing inner logic
// ---------------------------------------------------------------------------

/// Everything [`process_sigma2`] produces.
///
/// A struct rather than a tuple: the function returns six related values and a
/// six-tuple at the call site is unreadable (and trips `clippy::type_complexity`).
struct Sigma2Processed {
    /// Encoded Sigma3, ready for the caller to send.
    sigma3_bytes: Vec<u8>,
    /// Derived symmetric session keys.
    session_keys: CaseSessionKeys,
    /// Peer identity verified from the responder's NOC chain.
    peer: PeerInfo,
    /// Our own identity, mirrored for symmetry.
    local: LocalInfo,
    /// Resumption record to persist, pairing the responder-supplied resumption
    /// id from `TBEData2` with this session's ECDH secret.
    resumption_record: ResumptionRecord,
    /// The responder's advertised session parameters (Sigma2 context tag 5),
    /// `None` when it omitted the optional element.
    peer_session_params: Option<crate::SessionParameters>,
}

/// Execute the full Sigma2 verification + Sigma3 construction logic.
///
/// Extracted from `CaseInitiator::handle_sigma2` to keep that method's
/// line count within the `clippy::too_many_lines` limit.
///
/// Returns a [`Sigma2Processed`] on success. Its `resumption_record` pairs the
/// responder's fresh `resumption_id` (from `TBEData2`) with this session's ECDH
/// `SharedSecret`; the caller persists it for a future `Sigma1` resumption
/// fast-path.
///
/// # Errors
///
/// See `CaseInitiator::handle_sigma2` for the full error taxonomy.
// The 10-step SIGMA-I protocol is intentionally kept as one function for
// auditability: a reviewer must be able to trace every step in sequence
// without jumping across files. The 100-line limit is relaxed here.
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
fn process_sigma2(
    sigma2_bytes: &[u8],
    credentials: &CaseCredentials,
    trusted_roots: &TrustedRoots,
    peer_node_id: u64,
    peer_fabric_id: u64,
    eph_secret: &SecretKey,
    eph_pub: &[u8; 65],
    initiator_session_id: u16,
    sigma1_bytes: &[u8],
    now: MatterTime,
) -> Result<Sigma2Processed> {
    let sigma2 = Sigma2::decode(sigma2_bytes)?;
    let peer_session_params = sigma2.session_parameters()?;

    // Step 1: ECDH shared secret from our eph secret + peer's eph pub.
    // Wrap in `Zeroizing` so the raw ECDH output is wiped when this function
    // returns; it is the root secret all Sigma2/Sigma3/session keys derive from.
    let shared_secret = Zeroizing::new(ecdh_shared_secret(eph_secret, &sigma2.responder_eph_pub)?);

    // Step 2: Derive S2K.
    // sigma2Salt = IPK(16) || responderRandom(32) || responderEphPub(65) || SHA-256(sigma1)
    let h_sigma1 = transcript_hash(&[sigma1_bytes]);
    let mut sigma2_salt: Vec<u8> = Vec::with_capacity(16 + 32 + 65 + 32);
    sigma2_salt.extend_from_slice(&credentials.ipk);
    sigma2_salt.extend_from_slice(&sigma2.responder_random);
    sigma2_salt.extend_from_slice(&sigma2.responder_eph_pub);
    sigma2_salt.extend_from_slice(&h_sigma1);
    // `s2k` is a derived secret key; wrap in `Zeroizing` so it is wiped on return.
    let mut s2k = Zeroizing::new([0u8; AEAD_KEY_LEN]);
    hkdf_derive(
        shared_secret.as_slice(),
        &sigma2_salt,
        HKDF_INFO_SIGMA2,
        &mut *s2k,
    )?;

    // Step 3: AES-128-CCM decrypt.
    let sigma2_decrypted = aead_decrypt(&s2k, NONCE_TBE_DATA2, b"", &sigma2.encrypted)?;

    // Step 4: Parse TBEData2.
    let mut peer_tbe = decode_tbedata2(&sigma2_decrypted)?;

    // Step 5: Validate peer NOC chain against trusted roots at the injected
    // wall-clock instant (`not_before <= now <= not_after`). The certs are
    // MOVED into the chain Vec (no clones); the NOC is taken back out after
    // validation for the subject/key checks and the returned PeerInfo.
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

    // Step 6: Check peer NodeId + FabricId match expectations.
    let peer_dn = peer_noc.subject();
    let verified_node_id = peer_dn
        .node_id()
        .ok_or(Error::PeerNodeIdMismatch(0, peer_node_id))?;
    let verified_fabric_id = peer_dn.fabric_id().ok_or(Error::FabricIdMismatch {
        peer: 0,
        local: peer_fabric_id,
    })?;
    if verified_node_id != peer_node_id {
        return Err(Error::PeerNodeIdMismatch(verified_node_id, peer_node_id));
    }
    if verified_fabric_id != peer_fabric_id {
        return Err(Error::FabricIdMismatch {
            peer: verified_fabric_id,
            local: peer_fabric_id,
        });
    }

    // Step 7: Verify peer's ECDSA signature over TBSData2.
    // TBSData2 = TlvSignedData { responderNoc, responderIcac?, responderEphPub, initiatorEphPub }
    // — over the peer's exact wire bytes (kept in TbeData2), not a re-encoding.
    let peer_signed_data = encode_tbs_data(
        &peer_tbe.peer_noc_tlv,
        peer_tbe.peer_icac_tlv.as_deref(),
        &sigma2.responder_eph_pub,
        eph_pub,
    )?;
    let peer_sig =
        Signature::from_slice(&peer_tbe.peer_signature).map_err(|_| Error::PeerSignatureInvalid)?;
    peer_noc
        .public_key()
        .verify(&peer_signed_data, &peer_sig)
        .map_err(|_| Error::PeerSignatureInvalid)?;

    // Step 8: Build TBSData3 and sign with our NOC key.
    // The initiator plays the "responder" role in TlvSignedData because the
    // field names were defined from Sigma2's perspective. (CaseClient.ts lines 249–254)
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
    let our_signed_data = encode_tbs_data(
        &our_noc_tlv,
        our_icac_tlv.as_deref(),
        eph_pub,                   // our eph pub = "responderPublicKey"
        &sigma2.responder_eph_pub, // peer's eph pub = "initiatorPublicKey"
    )?;
    let our_signature = credentials
        .signer
        .sign_p256_sha256(&our_signed_data)
        .map_err(Error::SigningFailed)?;

    // Step 9: Encode TBEData3 and encrypt with S3K.
    // sigma3Salt = IPK(16) || SHA-256(sigma1 || sigma2)
    let h_s1_s2 = transcript_hash(&[sigma1_bytes, sigma2_bytes]);
    let mut sigma3_salt: Vec<u8> = Vec::with_capacity(16 + 32);
    sigma3_salt.extend_from_slice(&credentials.ipk);
    sigma3_salt.extend_from_slice(&h_s1_s2);
    // `s3k` is a derived secret key; wrap in `Zeroizing` so it is wiped on return.
    let mut s3k = Zeroizing::new([0u8; AEAD_KEY_LEN]);
    hkdf_derive(
        shared_secret.as_slice(),
        &sigma3_salt,
        HKDF_INFO_SIGMA3,
        &mut *s3k,
    )?;

    let sigma3_plaintext = encode_tbedata3(&our_noc_tlv, our_icac_tlv.as_deref(), &our_signature)?;
    let encrypted3 = aead_encrypt(&s3k, NONCE_TBE_DATA3, b"", &sigma3_plaintext)?;
    let sigma3_bytes = Sigma3 {
        encrypted: encrypted3,
    }
    .encode()?;

    // Step 10: Derive final session keys.
    // sessionSalt = IPK(16) || SHA-256(sigma1 || sigma2 || sigma3)
    let h_all = transcript_hash(&[sigma1_bytes, sigma2_bytes, &sigma3_bytes]);
    let mut session_salt: Vec<u8> = Vec::with_capacity(16 + 32);
    session_salt.extend_from_slice(&credentials.ipk);
    session_salt.extend_from_slice(&h_all);
    // `keys_blob` holds the raw 48-byte session-key material; wrap in
    // `Zeroizing` so it is wiped once the per-direction keys are split out.
    let mut keys_blob = Zeroizing::new([0u8; 48]);
    hkdf_derive(
        shared_secret.as_slice(),
        &session_salt,
        HKDF_INFO_SESSION_KEYS,
        &mut *keys_blob,
    )?;

    // Initiator key assignment (NodeSession.ts lines 75–77, isInitiator=true):
    //   encryptKey (i2r) = keys[0..16]
    //   decryptKey (r2i) = keys[16..32]
    //   attestationChallenge  = keys[32..48]
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
        node_id: verified_node_id,
        fabric_id: verified_fabric_id,
        noc: peer_noc,
        session_id: sigma2.responder_session_id,
    };
    let local = LocalInfo {
        node_id: credentials.node_id,
        fabric_id: credentials.fabric_id,
        session_id: initiator_session_id,
    };

    // Step 11: Build the resumption record for the caller to persist.
    // The responder's fresh resumption id arrived (encrypted) in TBEData2;
    // the record's IKM is this session's raw ECDH SharedSecret — the same
    // (id, secret) pair chip's SessionResumptionStorage and matter.js store,
    // so either peer can later initiate resumption against the other.
    let resumption_record = ResumptionRecord {
        id: ResumptionId(peer_tbe.resumption_id),
        shared_secret: *shared_secret,
        peer: peer.clone(),
        expires_at: None,
    };

    Ok(Sigma2Processed {
        sigma3_bytes,
        session_keys,
        peer,
        local,
        resumption_record,
        peer_session_params,
    })
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

    /// `new()` must accept valid credentials.
    #[test]
    fn new_succeeds_with_valid_credentials() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let _initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
    }

    // ─── start() ──────────────────────────────────────────────────────────

    /// `start()` must return a non-empty byte slice that starts with the
    /// anonymous TLV structure byte (0x15).
    #[test]
    fn start_returns_sigma1_bytes() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let mut initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        let bytes = initiator.start().unwrap();
        assert!(!bytes.is_empty(), "Sigma1 bytes must be non-empty");
        assert_eq!(bytes[0], 0x15, "anonymous structure must start with 0x15");
    }

    /// After `start()`, `expected_inbound()` must report `Sigma2`.
    #[test]
    fn expected_inbound_after_start_is_sigma2() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let mut initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        let _ = initiator.start().unwrap();
        assert_eq!(initiator.expected_inbound(), Some(CaseMessageKind::Sigma2));
    }

    /// `start()` must encode a Sigma1 that round-trips through the decoder.
    #[test]
    fn start_produces_valid_sigma1() {
        use crate::case::messages::Sigma1;
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let mut initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        let bytes = initiator.start().unwrap();
        // Must decode without error.
        let decoded = Sigma1::decode(&bytes).unwrap();
        // dest_id is 32 bytes.
        assert_eq!(decoded.dest_id.len(), 32);
        // initiator_eph_pub starts with 0x04 (SEC1 uncompressed).
        assert_eq!(
            decoded.initiator_eph_pub[0], 0x04,
            "ephemeral pub key must be SEC1 uncompressed"
        );
    }

    // ─── terminal-state guards ────────────────────────────────────────────

    /// `finish()` before the handshake is complete returns `HandshakeIncomplete`.
    #[test]
    fn finish_before_complete_returns_handshake_incomplete() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        assert!(matches!(
            initiator.finish(),
            Err(Error::HandshakeIncomplete)
        ));
    }

    /// `finish()` called immediately after `start()` returns `HandshakeIncomplete`.
    #[test]
    fn finish_after_start_returns_handshake_incomplete() {
        let creds2 = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let fresh = CaseInitiator::new(
            creds2,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        // The initiator is in AwaitingStart — finish() must fail.
        assert!(matches!(fresh.finish(), Err(Error::HandshakeIncomplete)));
    }

    // ─── out-of-order rejection ────────────────────────────────────────────

    /// Calling `handle_sigma2` before `start` (still in `AwaitingStart`)
    /// must return `UnexpectedCaseMessage`.
    #[test]
    fn handle_sigma2_before_start_is_rejected() {
        use crate::case::messages::Sigma2;
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let mut initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        let dummy_sigma2 = Sigma2 {
            responder_random: [0u8; 32],
            responder_session_id: 1,
            responder_eph_pub: [0x04; 65],
            encrypted: vec![0xAA; 80],
            responder_session_params: None,
        };
        let bytes = dummy_sigma2.encode().unwrap();
        assert!(matches!(
            initiator.handle_sigma2(&bytes),
            Err(Error::UnexpectedCaseMessage { .. })
        ));
    }

    /// Calling `next_message` before `handle_sigma2` must return
    /// `UnexpectedCaseMessage`.
    #[test]
    fn next_message_before_handle_sigma2_is_rejected() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let mut initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        let _ = initiator.start().unwrap();
        // Still in AwaitingSigma2; next_message is not valid here.
        assert!(matches!(
            initiator.next_message(),
            Err(Error::UnexpectedCaseMessage { .. })
        ));
    }

    /// Calling `start()` twice returns `UnexpectedCaseMessage` on the second call.
    #[test]
    fn double_start_is_rejected() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let mut initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        let _ = initiator.start().unwrap();
        assert!(matches!(
            initiator.start(),
            Err(Error::UnexpectedCaseMessage { .. })
        ));
    }

    // ─── expected_inbound() states ────────────────────────────────────────

    /// Before `start()`, `expected_inbound()` returns `None`.
    #[test]
    fn expected_inbound_before_start_is_none() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        assert_eq!(initiator.expected_inbound(), None);
    }

    // ─── TrustedRoots with an anchor ──────────────────────────────────────

    /// `TrustedRoots::add` works and produces a non-empty set.
    #[test]
    fn trusted_roots_with_anchor_is_non_empty() {
        let rcac = make_test_cert(0, 0x5678);
        let anchor = TrustAnchor::from_root_cert(&rcac);
        let mut roots = TrustedRoots::new();
        roots.add(anchor);
        assert!(!roots.is_empty());
        assert_eq!(roots.len(), 1);
    }

    // ─── M4.2: Resumption constructors ────────────────────────────────────

    /// Helper: build a minimal `PeerInfo` for use in `ResumptionRecord`.
    fn make_test_peer_info(node_id: u64, fabric_id: u64) -> PeerInfo {
        PeerInfo {
            node_id,
            fabric_id,
            noc: make_test_cert(node_id, fabric_id),
            session_id: 1,
        }
    }

    /// Helper: build a `ResumptionRecord` with given `shared_secret` and id.
    fn make_resumption_record(
        secret: [u8; 32],
        id: [u8; 16],
        node_id: u64,
        fabric_id: u64,
    ) -> ResumptionRecord {
        ResumptionRecord {
            id: ResumptionId(id),
            shared_secret: secret,
            peer: make_test_peer_info(node_id, fabric_id),
            expires_at: None,
        }
    }

    /// `new_with_resumption` must succeed with valid inputs.
    #[test]
    fn new_with_resumption_succeeds() {
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let record = make_resumption_record([0x01u8; 32], [0x02u8; 16], 0x1234, 0x5678);
        let _initiator = CaseInitiator::new_with_resumption(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            record,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
    }

    /// When started with a resumption record, `start()` must produce a Sigma1
    /// that round-trips through the decoder and includes non-`None` resumption
    /// fields (`resumption_id` and `initiator_resume_mic`).
    #[test]
    fn start_with_resumption_populates_sigma1_resume_fields() {
        use crate::case::messages::Sigma1;
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let record = make_resumption_record([0x01u8; 32], [0x02u8; 16], 0x1234, 0x5678);
        let mut initiator = CaseInitiator::new_with_resumption(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            record,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        let bytes = initiator.start().unwrap();
        let decoded = Sigma1::decode(&bytes).unwrap();
        assert!(
            decoded.resumption_id.is_some(),
            "Sigma1 must carry resumption_id when constructed with a record"
        );
        assert_eq!(
            decoded.resumption_id.unwrap(),
            [0x02u8; 16],
            "resumption_id in Sigma1 must match the record's id"
        );
        assert!(
            decoded.initiator_resume_mic.is_some(),
            "Sigma1 must carry initiator_resume_mic when constructed with a record"
        );
        // After start(), expected_inbound should be Sigma2Resume (resumption path).
        assert_eq!(
            initiator.expected_inbound(),
            Some(CaseMessageKind::Sigma2Resume)
        );
    }

    /// When started WITHOUT a resumption record, `start()` must produce a Sigma1
    /// with `None` resumption fields (the new-session path is unchanged).
    #[test]
    fn start_without_resumption_omits_sigma1_resume_fields() {
        use crate::case::messages::Sigma1;
        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let mut initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        let bytes = initiator.start().unwrap();
        let decoded = Sigma1::decode(&bytes).unwrap();
        assert!(
            decoded.resumption_id.is_none(),
            "Sigma1 must NOT carry resumption_id on the new-session path"
        );
        assert!(
            decoded.initiator_resume_mic.is_none(),
            "Sigma1 must NOT carry initiator_resume_mic on the new-session path"
        );
        // expected_inbound for non-resumption path must be Sigma2.
        assert_eq!(initiator.expected_inbound(), Some(CaseMessageKind::Sigma2));
    }

    /// `handle_sigma2_resume` must succeed when given a correctly-computed
    /// `Sigma2_Resume` message (valid MIC).
    #[test]
    fn handle_sigma2_resume_after_resumption_attempt_succeeds_with_valid_mic() {
        use crate::case::messages::Sigma2Resume;
        use crate::case::sigma::compute_sigma2_resume_mic;
        use ring::rand::SystemRandom;

        let shared_secret = [0x42u8; 32];
        let old_id = [0x11u8; 16];
        let new_id = [0x22u8; 16];

        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let record = make_resumption_record(shared_secret, old_id, 0x1234, 0x5678);

        // We need to know the initiator_random that will be sampled.
        // Use new_with_resumption_using_rng with a deterministic RNG.
        // ring's SystemRandom is not deterministic, so we use the production path
        // and extract the random from the emitted Sigma1 to compute the expected MIC.
        let rng = SystemRandom::new();
        let mut initiator = CaseInitiator::new_with_resumption_using_rng(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            record,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
            &rng,
        )
        .unwrap();

        // Start to emit Sigma1 (which contains the sampled initiator_random).
        let sigma1_bytes = initiator.start().unwrap();
        let sigma1 = crate::case::messages::Sigma1::decode(&sigma1_bytes).unwrap();
        let initiator_random = sigma1.initiator_random;

        // Compute the MIC as the responder would.
        let mic = compute_sigma2_resume_mic(&shared_secret, &initiator_random, &new_id).unwrap();

        // Build Sigma2_Resume.
        let sigma2_resume = Sigma2Resume {
            resumption_id: new_id,
            resume_mic: mic,
            responder_session_id: 0xBEEF,
            responder_session_params: None,
        };
        let sigma2_resume_bytes = sigma2_resume.encode().unwrap();

        // This must succeed.
        initiator
            .handle_sigma2_resume(&sigma2_resume_bytes)
            .unwrap();

        // finish() must succeed and carry the updated resumption record.
        let output = initiator.finish().unwrap();
        assert!(
            output.resumption_record.is_some(),
            "output must carry a resumption record after successful resumption"
        );
        assert_eq!(
            output.resumption_record.as_ref().unwrap().id.0,
            new_id,
            "resumption record id must be the NEW id from Sigma2_Resume"
        );
        // Verify resumed key derivation produces 16-byte keys.
        assert_ne!(
            output.keys.r2i_key, output.keys.i2r_key,
            "r2i and i2r keys must differ"
        );
    }

    /// `handle_sigma2_resume` must return `ResumptionMacMismatch` when the MIC
    /// in the `Sigma2_Resume` message is corrupted.
    #[test]
    fn handle_sigma2_resume_rejects_invalid_mic() {
        use crate::case::messages::Sigma2Resume;
        use crate::case::sigma::compute_sigma2_resume_mic;
        use ring::rand::SystemRandom;

        let shared_secret = [0x42u8; 32];
        let old_id = [0x11u8; 16];
        let new_id = [0x22u8; 16];

        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let record = make_resumption_record(shared_secret, old_id, 0x1234, 0x5678);

        let rng = SystemRandom::new();
        let mut initiator = CaseInitiator::new_with_resumption_using_rng(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            record,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
            &rng,
        )
        .unwrap();

        let sigma1_bytes = initiator.start().unwrap();
        let sigma1 = crate::case::messages::Sigma1::decode(&sigma1_bytes).unwrap();
        let initiator_random = sigma1.initiator_random;

        let mut mic =
            compute_sigma2_resume_mic(&shared_secret, &initiator_random, &new_id).unwrap();
        // Corrupt one byte to make MIC invalid.
        mic[0] ^= 0xFF;

        let sigma2_resume = Sigma2Resume {
            resumption_id: new_id,
            resume_mic: mic,
            responder_session_id: 0xBEEF,
            responder_session_params: None,
        };
        let sigma2_resume_bytes = sigma2_resume.encode().unwrap();

        assert!(
            matches!(
                initiator.handle_sigma2_resume(&sigma2_resume_bytes),
                Err(Error::ResumptionMacMismatch)
            ),
            "Corrupted MIC must be rejected with ResumptionMacMismatch"
        );
    }

    /// `handle_sigma2_resume` must return `UnexpectedCaseMessage` when the
    /// initiator was constructed WITHOUT a resumption record (no attempt was made).
    #[test]
    fn handle_sigma2_resume_without_resumption_attempt_returns_unexpected_message() {
        use crate::case::messages::Sigma2Resume;

        let creds = make_test_credentials(0x1234, 0x5678, [0xAB; 16], dummy_rcac_pub());
        let mut initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        let _ = initiator.start().unwrap();

        // Build any syntactically valid Sigma2_Resume.
        let sigma2_resume = Sigma2Resume {
            resumption_id: [0xAAu8; 16],
            resume_mic: [0xBBu8; 16],
            responder_session_id: 1,
            responder_session_params: None,
        };
        let bytes = sigma2_resume.encode().unwrap();

        assert!(
            matches!(
                initiator.handle_sigma2_resume(&bytes),
                Err(Error::UnexpectedCaseMessage { .. })
            ),
            "Receiving Sigma2_Resume without having attempted resumption must fail"
        );
    }

    // ─── `State`'s Debug names the variant only ───────────────────────────
    //
    // Most `State` variants hold secrets (the operational signing key, the IPK,
    // the ephemeral secret, session keys, resumption secrets). Nothing formats
    // `State` today, but a future `#[derive(Debug)]` on the public state machine
    // would print it, so `State`'s own `Debug` prints the variant name and
    // nothing else. Every check compares the whole rendering against the bare
    // name, so no field can be present. Static messages only: a failing
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
        let initiator = CaseInitiator::new(
            creds,
            empty_roots(),
            0x1234,
            0x5678,
            0x0001,
            MatterTime::from_unix_secs(2_000_000_000),
        )
        .unwrap();
        assert_state_shows_only(&initiator.state, "AwaitingStart");
        assert_state_shows_only(&State::Poisoned, "Poisoned");
    }
}
