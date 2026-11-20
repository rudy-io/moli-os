//! Per-session state and the [`SessionManager`] that owns it.
//!
//! M5.1 shipped the registration / counter / replay-window layer. M5.2
//! threads the protocol header and the per-session [`MrpState`] machine
//! through the manager so the same `SessionManager` value drives both the
//! framing and the reliability layers.
//!
//! Public surface added in M5.2:
//! - [`Session::mrp`] — one [`MrpState`] per session.
//! - [`SessionManager::encode_outbound`] / [`SessionManager::decode_inbound`]
//!   — new richer signatures that take the protocol-header inputs (opcode,
//!   protocol id, exchange id, MRP flags, `now`) and return structured
//!   output values rather than raw byte buffers.
//! - [`SessionManager::poll_timeout`] / [`SessionManager::handle_timeout`]
//!   — fold per-session [`MrpTimerEvent`]s into manager-wide
//!   [`MrpEvent`]s and build standalone-ack packets for the
//!   `StandaloneAckDeadlineFired` and duplicate-reliable-resend paths.

use std::collections::HashMap;
use std::time::Instant;

use matter_crypto::case::CaseSessionOutput;
use matter_crypto::pase::PaseSessionKeys;

use crate::error::{Error, Result};
use crate::framing::{
    decrypt_secured_with_header, encode_secured_with_cipher, MessageCounter, NodeId, ReplayWindow,
    SecuredMessageFlags, SecuredMessageHeader, SecurityFlags, SessionId,
};
use crate::mrp::{
    ExchangeKey, InboundOutcome, MrpConfig, MrpEvent, MrpFlags, MrpProvenance, MrpState,
    MrpTimerEvent,
};
use crate::protocol_header::{build_standalone_ack_header, encode_protocol_header, ProtocolId};

/// Which side of a session this end occupies. Decides which key in
/// [`SessionKeys`] is used for encoding vs decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRole {
    /// We sent the first message of the handshake (PASE prover / CASE
    /// initiator).
    Initiator,
    /// We received the first message of the handshake (PASE verifier /
    /// CASE responder).
    Responder,
}

/// Symmetric key material for a single Matter session.
///
/// `Debug` is written by hand so that every key prints as `<redacted>`:
/// [`Session`] derives `Debug` and prints this struct, and the keys decrypt
/// and forge the session's traffic, so they must never reach a log through a
/// stray `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionKeys {
    /// Encrypts traffic flowing initiator → responder.
    /// Secret: this struct's `Debug` output shows `<redacted>` instead.
    pub i2r_key: [u8; 16],
    /// Encrypts traffic flowing responder → initiator.
    /// Secret: this struct's `Debug` output shows `<redacted>` instead.
    pub r2i_key: [u8; 16],
    /// Attestation challenge key (PASE) / attestation challenge (CASE).
    /// Secret: this struct's `Debug` output shows `<redacted>` instead.
    pub attestation_key: [u8; 16],
}

impl core::fmt::Debug for SessionKeys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Same name, fields and order as the derive this replaces; every
        // field is secret, so every value is withheld.
        f.debug_struct("SessionKeys")
            .field("i2r_key", &"<redacted>")
            .field("r2i_key", &"<redacted>")
            .field("attestation_key", &"<redacted>")
            .finish()
    }
}

impl From<PaseSessionKeys> for SessionKeys {
    fn from(p: PaseSessionKeys) -> Self {
        Self {
            i2r_key: p.i2r_key,
            r2i_key: p.r2i_key,
            attestation_key: p.attestation_key,
        }
    }
}

impl SessionKeys {
    /// Build session keys from a completed CASE handshake output.
    #[must_use]
    pub fn from_case_output(out: &CaseSessionOutput) -> Self {
        Self {
            i2r_key: out.keys.i2r_key,
            r2i_key: out.keys.r2i_key,
            attestation_key: out.keys.attestation_challenge,
        }
    }
}

/// Light identity hint about the peer, captured at registration time.
///
/// PASE sessions usually leave both fields `None` (commissioning has no
/// operational identity yet). CASE sessions populate both from the peer's
/// validated NOC.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PeerHint {
    /// Peer's operational Node ID (CASE only).
    pub node_id: Option<NodeId>,
    /// Peer's fabric ID (CASE only).
    pub fabric_id: Option<u64>,
}

/// Single session — keys, counters, replay tracking, role, peer hint, MRP
/// state.
#[derive(Debug)]
#[non_exhaustive]
pub struct Session {
    /// Our local ID. Peers send packets addressed to this ID.
    pub local_id: SessionId,
    /// Peer's local ID. Outbound headers carry this in their `session_id`
    /// field so the peer can demux on receipt.
    pub peer_id: SessionId,
    /// Whether we're the initiator or the responder of this session.
    pub role: SessionRole,
    /// Symmetric session keys.
    ///
    /// Fixed for the life of the session: Matter re-keys by establishing a
    /// NEW session (PASE/CASE handshake), never by mutating an existing
    /// one's keys. The cached AES-CCM ciphers below are derived from these
    /// bytes on first use and are NOT invalidated afterwards, so overwriting
    /// this field in place would leave the ciphers on the old key. Register
    /// a fresh session instead — a debug build trips an assertion if you
    /// don't (each cached cipher keeps a copy of its source key to check).
    pub keys: SessionKeys,
    /// Next outbound message counter. Every `register_*` method starts it at
    /// `Crypto_DRBG(len = 28) + 1`, a CSPRNG value in `[1, 2^28]`, as Matter
    /// Core Spec §4.6.1.1 and §4.6.2 require, so a passive observer cannot
    /// read a session's age or traffic volume off the cleartext header. A
    /// resumed CASE session is a new session and gets a fresh random start.
    /// If the system CSPRNG fails it starts at 1 instead (logged at `warn`).
    /// It advances by one per send, and sending stops with
    /// [`Error::CounterOverflow`] once it reaches `u32::MAX`.
    pub outbound_counter: u32,
    /// Inbound replay window for this session.
    pub replay_window: ReplayWindow,
    /// Peer identity hint (CASE) or default (PASE).
    pub peer: PeerHint,
    /// Transport address the peer was last known at, stamped by the owner
    /// after registration (the controller sets it when it caches the
    /// session). `None` until stamped. Routing hint only — NOT part of the
    /// session's cryptographic identity, and never consulted by
    /// encode/decode. Lives on the session so eviction (explicit `remove`
    /// or the internal oldest-session cap eviction) can never strand a
    /// stale address index entry.
    pub peer_addr: Option<std::net::SocketAddr>,
    /// OUR node id mixed into the AES-CCM nonce of outbound frames (spec
    /// §4.8.2): the local *operational* node id on CASE sessions, `0` on
    /// PASE sessions. Decoupled from the wire header, which omits the
    /// source node id on secured unicast messages.
    pub local_nonce_node_id: u64,
    /// The PEER's node id mixed into the nonce when decrypting inbound
    /// frames: the peer's operational node id on CASE sessions, `0` on PASE.
    pub peer_nonce_node_id: u64,
    /// Per-session MRP state. Holds the exchange table, pending retransmits,
    /// pending piggyback-ack slot, and recent-reliable cache.
    pub mrp: MrpState,
    /// True when the underlying transport is reliable and ordered (BTP).
    /// Forces MRP off for this session per Matter spec 4.12: outbound R-flag
    /// suppressed, no retransmit registration, no inbound ack scheduling.
    /// Mirrors chip's `SecureSession::AllowsMRP()`, which returns `false`
    /// whenever the session's transport type is not UDP.
    pub transport_reliable: bool,
    /// Monotonic creation sequence, assigned by [`SessionManager`] on insert.
    /// Used ONLY to pick the oldest session to evict when the table hits its
    /// cap; not part of the wire or session identity.
    pub(crate) created_seq: u64,
    /// Cached AES-CCM cipher over [`SessionKeys::i2r_key`], built on first
    /// use. Private: it is a derived cache of [`Self::keys`], not session
    /// state a caller may set.
    cipher_i2r: Option<CachedCipher>,
    /// Cached AES-CCM cipher over [`SessionKeys::r2i_key`], built on first
    /// use. Private, as above.
    cipher_r2i: Option<CachedCipher>,
}

/// A cached AES-CCM cipher, plus — in debug builds only — the key bytes it
/// was built from.
struct CachedCipher {
    cipher: matter_crypto::aead::SessionAead,
    /// Copy of the key `cipher` was built from, kept ONLY so the accessors
    /// can assert the cache still matches [`Session::keys`] (which is `pub`,
    /// hence mutable out-of-crate via `SessionManager::get_mut`).
    ///
    /// `#[cfg(debug_assertions)]`: the field itself — not just the assertions
    /// — is absent from release builds, so a release binary carries no second
    /// copy of the session keys.
    #[cfg(debug_assertions)]
    key: [u8; 16],
}

/// Opaque `Debug`: both the expanded key schedule inside
/// [`matter_crypto::aead::SessionAead`] and the debug-only key copy above are
/// raw key material, and [`Session`] derives `Debug`.
impl core::fmt::Debug for CachedCipher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CachedCipher(<key material redacted>)")
    }
}

impl Session {
    /// Cached cipher for the initiator→responder key, built on first use.
    ///
    /// Session keys never change once the session is registered (see
    /// [`Self::keys`]), so the cache needs no invalidation hook — the
    /// `debug_assert` catches a caller that breaks that rule anyway.
    fn cipher_i2r(&mut self) -> &matter_crypto::aead::SessionAead {
        let key = self.keys.i2r_key;
        let cached = self.cipher_i2r.get_or_insert_with(|| CachedCipher {
            cipher: matter_crypto::aead::SessionAead::new(&key),
            #[cfg(debug_assertions)]
            key,
        });
        // `#[cfg]`, not `debug_assert!`: the field it reads does not exist in
        // release builds, so the statement must not be compiled there either.
        #[cfg(debug_assertions)]
        assert_eq!(
            cached.key, key,
            "i2r_key changed after the cipher cache was built — register a fresh session instead of mutating `keys`"
        );
        &cached.cipher
    }

    /// Cached cipher for the responder→initiator key, built on first use.
    fn cipher_r2i(&mut self) -> &matter_crypto::aead::SessionAead {
        let key = self.keys.r2i_key;
        let cached = self.cipher_r2i.get_or_insert_with(|| CachedCipher {
            cipher: matter_crypto::aead::SessionAead::new(&key),
            #[cfg(debug_assertions)]
            key,
        });
        #[cfg(debug_assertions)]
        assert_eq!(
            cached.key, key,
            "r2i_key changed after the cipher cache was built — register a fresh session instead of mutating `keys`"
        );
        &cached.cipher
    }

    /// Cipher for OUR outbound direction. Mirrors the key choice in
    /// [`crate::framing::encode_secured`] exactly: initiator sends under
    /// i2r, responder under r2i.
    pub(crate) fn outbound_cipher(&mut self) -> &matter_crypto::aead::SessionAead {
        match self.role {
            SessionRole::Initiator => self.cipher_i2r(),
            SessionRole::Responder => self.cipher_r2i(),
        }
    }

    /// Cipher for the PEER's outbound direction (what we decrypt with),
    /// borrowed together with the replay window because the inbound path
    /// needs both at once and they live in disjoint fields of `self`.
    ///
    /// Mirrors the key choice in [`crate::framing::decode_secured`] exactly:
    /// the peer's sending key is the opposite of ours.
    pub(crate) fn inbound_cipher_and_window(
        &mut self,
    ) -> (&matter_crypto::aead::SessionAead, &mut ReplayWindow) {
        let (slot, key) = match self.role {
            SessionRole::Initiator => (&mut self.cipher_r2i, self.keys.r2i_key),
            SessionRole::Responder => (&mut self.cipher_i2r, self.keys.i2r_key),
        };
        let cached = slot.get_or_insert_with(|| CachedCipher {
            cipher: matter_crypto::aead::SessionAead::new(&key),
            #[cfg(debug_assertions)]
            key,
        });
        #[cfg(debug_assertions)]
        assert_eq!(
            cached.key, key,
            "the peer's session key changed after the cipher cache was built — register a fresh session instead of mutating `keys`"
        );
        (&cached.cipher, &mut self.replay_window)
    }
}

/// Structured output of [`SessionManager::encode_outbound`]. Carries the
/// wire bytes plus the bookkeeping the caller needs to track the message
/// (exchange id, counter, whether a piggyback ack was attached).
#[derive(Debug)]
#[non_exhaustive]
pub struct EncodeOutboundOutput {
    /// Fully-encrypted secured-message bytes ready to hand to the UDP
    /// transport.
    pub wire_bytes: Vec<u8>,
    /// Exchange identifier this message belongs to. Allocated by MRP if the
    /// caller did not provide one.
    pub exchange_id: u16,
    /// Whether the local side originated the exchange. Mirrors the `I` flag
    /// in the protocol header.
    pub is_local_initiator: bool,
    /// Outbound message counter consumed by this send (the slot used in the
    /// secured-message header before [`Session::outbound_counter`] was
    /// advanced).
    pub message_counter: MessageCounter,
    /// Whether MRP drained a pending piggyback-ack into this outbound
    /// header. Useful for tests and instrumentation; the caller usually
    /// does not need to act on it.
    pub piggyback_acked: bool,
}

/// Structured output of [`SessionManager::decode_inbound`]. The three
/// variants cover the cases a caller cares about: a fresh application
/// message, a standalone-ack-only inbound, and a duplicate-reliable inbound
/// for which the manager has already pre-built a standalone-ack packet to
/// re-send.
///
/// `Debug` is written by hand so that an `AppMessage`'s decrypted `payload`
/// prints as its length only (`"<N bytes>"`): it is whatever plaintext the
/// peer sent, which can be a Wi-Fi passphrase, a Thread dataset or key
/// material, so it must never reach a log through a stray `{:?}`.
#[non_exhaustive]
pub enum DecodeInboundOutput {
    /// A new application message arrived. `payload` is the bytes AFTER the
    /// protocol header.
    AppMessage {
        /// Local session ID this message was demuxed onto.
        session_id: SessionId,
        /// Exchange the message belongs to.
        exchange_id: u16,
        /// Whether the peer is the initiator (i.e. we are the responder).
        is_initiator: bool,
        /// Protocol id from the decoded protocol header.
        protocol_id: ProtocolId,
        /// Protocol opcode from the decoded protocol header.
        opcode: u8,
        /// Decrypted application payload (post-header).
        /// Plaintext: this enum's `Debug` output shows only its length.
        payload: Vec<u8>,
    },
    /// The inbound was a standalone-ack-only message; no application bytes
    /// follow. MRP has already cleared any matching pending retransmit.
    AckOnly {
        /// Local session ID this message was demuxed onto.
        session_id: SessionId,
        /// Exchange identifier.
        exchange_id: u16,
        /// Counter that was acknowledged.
        acked_counter: MessageCounter,
    },
    /// The inbound packet authenticated but its counter is a duplicate (the
    /// replay window's highest, an already-seen counter in the window, a
    /// counter older than the window, or 0), and it has R set on a session
    /// that is not `transport_reliable`. As chip does, the manager built a
    /// standalone ACK for it (consuming one outbound counter slot), on the
    /// duplicate's exchange with the opposite `I` flag, and dropped any ACK
    /// still buffered for that counter, so it is acknowledged exactly once. The
    /// duplicate's piggybacked ACK was not processed, and it must not be
    /// delivered: send `ack_packet` to the peer and nothing else.
    DuplicateReliableAckResent {
        /// Local session ID this message was demuxed onto.
        session_id: SessionId,
        /// Exchange identifier, from the duplicate's protocol header.
        exchange_id: u16,
        /// Fully-encrypted standalone-ack packet ready to send.
        ack_packet: Vec<u8>,
    },
}

impl core::fmt::Debug for DecodeInboundOutput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Same variant names, fields and order as the derive this replaces;
        // only the decrypted payload's bytes are withheld. The ack packet is
        // ciphertext, so it still prints in full.
        match self {
            Self::AppMessage {
                session_id,
                exchange_id,
                is_initiator,
                protocol_id,
                opcode,
                payload,
            } => f
                .debug_struct("AppMessage")
                .field("session_id", session_id)
                .field("exchange_id", exchange_id)
                .field("is_initiator", is_initiator)
                .field("protocol_id", protocol_id)
                .field("opcode", opcode)
                .field("payload", &format!("<{} bytes>", payload.len()))
                .finish(),
            Self::AckOnly {
                session_id,
                exchange_id,
                acked_counter,
            } => f
                .debug_struct("AckOnly")
                .field("session_id", session_id)
                .field("exchange_id", exchange_id)
                .field("acked_counter", acked_counter)
                .finish(),
            Self::DuplicateReliableAckResent {
                session_id,
                exchange_id,
                ack_packet,
            } => f
                .debug_struct("DuplicateReliableAckResent")
                .field("session_id", session_id)
                .field("exchange_id", exchange_id)
                .field("ack_packet", ack_packet)
                .finish(),
        }
    }
}

/// Default cap on the number of concurrently-registered secured sessions.
///
/// Bounds the session table's memory as defense-in-depth: a peer (or a flood of
/// half-open handshakes that complete) cannot grow the table without limit.
/// Sized far above any realistic controller workload (a home has tens of
/// devices, each typically one operational session), so legitimate use never
/// approaches it; tune via [`SessionManager::set_max_sessions`].
pub const DEFAULT_MAX_SESSIONS: usize = 256;

/// Owns all per-session state for one Matter node.
#[derive(Debug)]
pub struct SessionManager {
    sessions: HashMap<SessionId, Session>,
    next_local_id: u16,
    /// Monotonic counter stamped onto each session as `created_seq`, so the
    /// oldest can be identified for eviction when the table is full.
    next_seq: u64,
    /// Cap on `sessions.len()`; inserting into a full table evicts the oldest.
    max_sessions: usize,
    /// The next exchange id this node will hand out, across ALL its sessions.
    ///
    /// Matter Core Spec §4.10.2: *"The first Exchange ID for a given Initiator
    /// Node SHALL be a random integer. All subsequent Exchange IDs created by
    /// that Initiator SHALL be the last Exchange ID it created incremented by
    /// one. An Exchange ID is an unsigned integer that rolls over to zero when
    /// its maximum value is exceeded."* One counter per **node**, not per
    /// session — chip keeps a single `mNextExchangeId` on its `ExchangeManager`
    /// for the same reason (`ExchangeMgr.cpp:67`, `:118`).
    next_exchange_id: u16,
}

/// Draw the node's first exchange id (Matter Core Spec §4.10.2: it "SHALL be a
/// random integer").
///
/// Falls back to a fixed non-zero value if the system CSPRNG is unavailable
/// rather than panicking: [`SessionManager::new`] is infallible by design and
/// CONTRIBUTING.md forbids `unwrap`/`expect` in library code. The fallback costs
/// unpredictability, not correctness — uniqueness is scoped to
/// `{session, exchange id, role}`, which the monotonic increment preserves
/// either way.
fn seed_exchange_id() -> u16 {
    let mut bytes = [0u8; 2];
    if matter_crypto::random_bytes(&mut bytes).is_err() {
        tracing::warn!(
            target: "matter_transport::session",
            "system CSPRNG unavailable seeding the exchange-id counter; \
             falling back to a fixed seed (exchange ids become predictable)"
        );
        return 1;
    }
    u16::from_le_bytes(bytes)
}

/// Map four DRBG bytes to a first outbound message counter:
/// `Crypto_DRBG(len = 28) + 1` (Matter Core Spec §4.6.1.1), a value in
/// `[1, 2^28]`.
///
/// Keeps the low 28 bits and adds one, exactly as chip does
/// (`GetRandU32() & kMessageCounterRandomInitMask`, then a pre-increment
/// before first use; `src/transport/MessageCounter.h`) and matter.js does
/// (`(randomUint32 >>> 4) + 1`). It never returns 0: a peer's replay state
/// starts at 0 (chip `PeerMessageCounter::kInitialSyncValue`), so a first
/// counter of 0 would read as a duplicate. The largest result, `2^28`, leaves
/// about `2^32 - 2^28` counters for traffic before exhaustion.
fn initial_counter_from_drbg(bytes: [u8; 4]) -> u32 {
    (u32::from_le_bytes(bytes) & 0x0FFF_FFFF) + 1
}

/// Draw a new secure session's first outbound message counter (Matter Core
/// Spec §4.6.1.1 and §4.6.2: every session SHALL start at a CSPRNG value in
/// `[1, 2^28]`).
///
/// Falls back to 1, with a `warn`, if the system CSPRNG fails. chip does not
/// fall back: `Crypto::GetRandU32()` aborts the process on a DRBG failure
/// (`VerifyOrDie`, `src/crypto/RandUtils.cpp`). A library must not abort its
/// host, and returning an error would make every `register_*` method
/// fallible, a breaking change, so this keeps registration infallible as
/// [`seed_exchange_id`] does. The fallback is the pre-0.7.4 behaviour and
/// costs only the traffic-analysis resistance the random start exists for
/// (the counter travels in the cleartext header). Confidentiality,
/// integrity and replay protection do not depend on it: every session has
/// fresh keys, so no nonce repeats under one key. In practice the branch is
/// unreachable, because the session being registered was just established
/// by a PASE or CASE handshake that drew from this same CSPRNG and fails
/// closed if it is unavailable.
fn seed_message_counter() -> u32 {
    let mut bytes = [0u8; 4];
    if matter_crypto::random_bytes(&mut bytes).is_err() {
        tracing::warn!(
            target: "matter_transport::session",
            "system CSPRNG unavailable seeding a session's outbound message \
             counter; falling back to 1 (spec §4.6.1.1 wants a random start, \
             so the counter now reveals how many messages the session sent)"
        );
        return 1;
    }
    initial_counter_from_drbg(bytes)
}

/// Which exchange [`SessionManager::encode_on`] sends on.
#[derive(Debug, Clone, Copy)]
enum OutboundExchange {
    /// A new exchange we start, on the node's next exchange id.
    New,
    /// An existing exchange named by id alone ([`MrpState::resolve`] picks
    /// the role).
    ById(u16),
    /// An existing exchange named by id and role.
    Exact(ExchangeKey),
}

/// Which existing exchange the finish and abort methods end.
#[derive(Debug, Clone, Copy)]
enum ExchangeTarget {
    /// Named by id alone ([`MrpState::resolve`] picks the role).
    ById(u16),
    /// Named by id and role.
    Exact(ExchangeKey),
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    /// Create an empty manager with the default session cap
    /// ([`DEFAULT_MAX_SESSIONS`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            next_local_id: 1,
            next_seq: 0,
            max_sessions: DEFAULT_MAX_SESSIONS,
            next_exchange_id: seed_exchange_id(),
        }
    }

    /// Move the node-wide exchange-id counter (spec §4.10.2) past the id it
    /// holds, once that id has been used for a locally-initiated exchange.
    ///
    /// Wraps **through** zero: the spec says the id "rolls over to zero when its
    /// maximum value is exceeded", so 0 is a legal id and skipping it would be a
    /// gratuitous deviation.
    ///
    /// Takes the field, not `&mut self`, so [`Self::encode_outbound`] can call
    /// it while it holds a session borrowed from the table. It reads the id
    /// first and spends it only once the message has been encrypted, so a
    /// failed send spends none.
    fn advance_exchange_id(next_exchange_id: &mut u16) {
        *next_exchange_id = next_exchange_id.wrapping_add(1);
    }

    /// Hand out the next exchange id, the way [`Self::encode_outbound`] does
    /// for a send that succeeds (read the counter, then advance it).
    #[cfg(test)]
    fn allocate_exchange_id(&mut self) -> u16 {
        let id = self.next_exchange_id;
        Self::advance_exchange_id(&mut self.next_exchange_id);
        id
    }

    /// Override the maximum number of concurrently-registered sessions. Values
    /// below 1 are clamped to 1 (the table must hold at least the session being
    /// registered). Registering into a full table evicts the oldest session.
    pub fn set_max_sessions(&mut self, max_sessions: usize) {
        self.max_sessions = max_sessions.max(1);
    }

    /// Register a session whose keys came from a completed PASE handshake.
    /// Returns the allocated local session ID.
    pub fn register_pase(
        &mut self,
        keys: PaseSessionKeys,
        role: SessionRole,
        peer_session_id: u16,
        peer: PeerHint,
    ) -> SessionId {
        self.register(SessionKeys::from(keys), role, peer_session_id, peer)
    }

    /// Register a session whose keys came from a completed CASE handshake.
    ///
    /// The session is inserted under `output.local.session_id` — the id this
    /// node advertised in Sigma1 — so inbound secured packets from the peer
    /// demux correctly. The peer's Node ID, Fabric ID, and session ID are
    /// pulled from `output` automatically.
    /// Convenience for callers with no peer discovery result to hand: the peer's
    /// MRP parameters are **unknown**, so the session gets the spec defaults.
    /// That is now genuinely the spec's "we were told nothing" configuration —
    /// it used to be a local 4200 ms idle base found in no specification, which
    /// silently gave these sessions an 8.4x longer window than peer-sized ones.
    ///
    /// Prefer [`Self::register_case_with_mrp`] wherever the peer's advertised
    /// `SII`/`SAI`/`SAT` are available.
    pub fn register_case(&mut self, output: &CaseSessionOutput, role: SessionRole) -> SessionId {
        self.register_case_with_mrp(output, role, MrpConfig::default(), MrpProvenance::Unknown)
    }

    /// [`Self::register_case`] but with the peer's advertised MRP retransmit
    /// config (from its operational mDNS TXT `SII`/`SAI`/`SAT`, via
    /// [`MatterService::peer_mrp_config`](crate::MatterService::peer_mrp_config)).
    ///
    /// Sizing retransmits to the peer's own parameters is what keeps us from
    /// hammering a sleepy device that polls on a long idle interval.
    /// The freshly-registered session has no pending MRP state yet, so applying
    /// the config here (right at creation, before any traffic) is safe.
    pub fn register_case_with_mrp(
        &mut self,
        output: &CaseSessionOutput,
        role: SessionRole,
        mrp_config: MrpConfig,
        provenance: MrpProvenance,
    ) -> SessionId {
        let local_id = SessionId(output.local.session_id);
        let peer = PeerHint {
            node_id: Some(NodeId(output.peer.node_id)),
            fabric_id: Some(output.peer.fabric_id),
        };
        self.insert_session(
            local_id,
            SessionKeys::from_case_output(output),
            role,
            output.peer.session_id,
            peer,
        );
        // CASE sessions mix each side's OPERATIONAL node id into the AES-CCM
        // nonce (spec §4.8.2) — devices silently drop frames built with the
        // PASE-style zero nonce node id (observed: Tapo P110M, M6.6.5).
        if let Some(session) = self.sessions.get_mut(&local_id) {
            session.local_nonce_node_id = output.local.node_id;
            session.peer_nonce_node_id = output.peer.node_id;
            session.mrp = MrpState::new(mrp_config);
        }
        // The one line that answers "why did this session time out after N
        // seconds". Emitted once per session establishment, at `info` because
        // an operator debugging a timeout will not have `debug` on, and until
        // now there was nothing in the logs distinguishing a peer-sized window
        // from a defaulted one.
        tracing::info!(
            target: "matter_transport::session",
            session_id = local_id.0,
            peer_node_id = output.peer.node_id,
            role = ?role,
            provenance = ?provenance,
            initial_idle_ms = u64::try_from(mrp_config.initial_idle.as_millis()).unwrap_or(u64::MAX),
            initial_active_ms =
                u64::try_from(mrp_config.initial_active.as_millis()).unwrap_or(u64::MAX),
            idle_threshold_ms =
                u64::try_from(mrp_config.idle_threshold.as_millis()).unwrap_or(u64::MAX),
            max_transmissions = mrp_config.max_transmissions,
            total_window_ms =
                u64::try_from(mrp_config.total_idle_window().as_millis()).unwrap_or(u64::MAX),
            "CASE session registered",
        );
        local_id
    }

    /// The MRP retransmit configuration a registered session is using.
    ///
    /// Exists so the sizing a session ended up with is *observable* rather than
    /// only loggable. Until this existed the only record of it was the
    /// `"CASE session registered"` tracing line, which a test cannot assert on
    /// and an embedded build does not emit at all.
    ///
    /// Returns `None` for an unknown [`SessionId`].
    #[must_use]
    pub fn mrp_config(&self, id: SessionId) -> Option<MrpConfig> {
        self.sessions.get(&id).map(|s| s.mrp.config())
    }

    /// Reserve and return the next free local session id WITHOUT creating a
    /// session. The commissioning driver allocates this first, advertises it
    /// in the handshake, then registers the finished session under the same
    /// id via [`Self::register_pase_with_local_id`] or
    /// [`Self::register_case`].
    pub fn allocate_session_id(&mut self) -> SessionId {
        self.allocate_id()
    }

    /// Register a completed PASE session under a caller-chosen local id (the
    /// id previously advertised to the peer via
    /// [`Self::allocate_session_id`]).
    ///
    /// Use this when the commissioning driver has already embedded the local
    /// session id in the handshake message (e.g. the `PBKDFParamRequest`
    /// `initiator_session_id` field) and must register the completed session
    /// under that same id so the peer's secured replies demux correctly.
    pub fn register_pase_with_local_id(
        &mut self,
        local_id: SessionId,
        keys: PaseSessionKeys,
        role: SessionRole,
        peer_session_id: u16,
        peer: PeerHint,
    ) {
        self.insert_session(
            local_id,
            SessionKeys::from(keys),
            role,
            peer_session_id,
            peer,
        );
    }

    fn register(
        &mut self,
        keys: SessionKeys,
        role: SessionRole,
        peer_session_id: u16,
        peer: PeerHint,
    ) -> SessionId {
        let local_id = self.allocate_id();
        self.insert_session(local_id, keys, role, peer_session_id, peer);
        local_id
    }

    /// Build and insert a [`Session`] under the given `local_id`.
    ///
    /// Any pre-existing session stored at `local_id` is silently replaced
    /// (the caller is responsible for ensuring uniqueness, e.g. by calling
    /// [`Self::allocate_session_id`] first).
    fn insert_session(
        &mut self,
        local_id: SessionId,
        keys: SessionKeys,
        role: SessionRole,
        peer_session_id: u16,
        peer: PeerHint,
    ) {
        // Session id 0 is reserved for the unsecured session (spec §4.4.4).
        // `allocate_id` never returns it, and a completed handshake must have
        // advertised a non-zero id; registering a secured session under 0 would
        // shadow the unsecured path. Guard the invariant (the CASE *resumption*
        // initiator still stubs its session id to 0 — that path must supply a
        // real id before it registers via `register_case`).
        debug_assert_ne!(
            local_id.0, 0,
            "secured session registered under reserved id 0"
        );
        // DoS defense: bound the table. If it is at capacity and this is a NEW
        // local id (a replacement of an existing id does not grow the table),
        // evict a victim before inserting. IDLE-FIRST: prefer a session with no
        // in-flight reliable work (`mrp.has_pending() == false`) over one
        // mid-exchange, so a burst of new sessions never tears down an active
        // handshake while an idle session is reclaimable; break ties by oldest
        // (`created_seq`). `false < true`, so the tuple key sorts idle sessions
        // ahead of busy ones.
        if self.sessions.len() >= self.max_sessions && !self.sessions.contains_key(&local_id) {
            if let Some(victim) = self
                .sessions
                .iter()
                .min_by_key(|(_, s)| (s.mrp.has_pending(), s.created_seq))
                .map(|(id, _)| *id)
            {
                self.sessions.remove(&victim);
            }
        }
        let created_seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        let session = Session {
            local_id,
            peer_id: SessionId(peer_session_id),
            role,
            keys,
            // Spec §4.6.1.1 / §4.6.2: a fresh CSPRNG start per session. Every
            // `register_*` path (PASE, CASE, resumed CASE) lands here, so each
            // new session, resumed ones included, gets its own random start.
            outbound_counter: seed_message_counter(),
            replay_window: ReplayWindow::new(),
            peer,
            // Stamped later by the owner (the controller, once it has the
            // session id and knows which peer address it corresponds to);
            // `None` until then.
            peer_addr: None,
            // PASE default: zero nonce node ids (spec §4.8.2). `register_case`
            // overwrites both with the operational node ids.
            local_nonce_node_id: 0,
            peer_nonce_node_id: 0,
            mrp: MrpState::new(MrpConfig::default()),
            // Default false everywhere sessions are constructed: MRP stays
            // on unless the caller explicitly opts a session into a
            // reliable transport via `set_transport_reliable`.
            transport_reliable: false,
            created_seq,
            // Built lazily on the session's first encrypt/decrypt so
            // registering a session stays cheap; keys never change, so the
            // caches never need invalidating.
            cipher_i2r: None,
            cipher_r2i: None,
        };
        self.sessions.insert(local_id, session);
    }

    fn allocate_id(&mut self) -> SessionId {
        // Skip zero (reserved for unsecured sessions per spec §4.4.4).
        loop {
            if self.next_local_id == 0 {
                self.next_local_id = 1;
            }
            let candidate = SessionId(self.next_local_id);
            self.next_local_id = self.next_local_id.wrapping_add(1);
            if !self.sessions.contains_key(&candidate) {
                return candidate;
            }
        }
    }

    /// Look up a session by its local ID.
    #[must_use]
    pub fn get(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id)
    }

    /// Number of currently-registered secured sessions. Never exceeds the cap
    /// set by [`Self::set_max_sessions`] (default [`DEFAULT_MAX_SESSIONS`]).
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Mutable lookup.
    #[must_use]
    pub fn get_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.sessions.get_mut(&id)
    }

    /// Drop a session (e.g. on close, on counter overflow, on attestation
    /// failure).
    ///
    /// A standalone ACK the session still owes is dropped with it; a caller
    /// closing the session on purpose takes those first with
    /// [`Self::flush_session_acks`].
    pub fn remove(&mut self, id: SessionId) -> Option<Session> {
        self.sessions.remove(&id)
    }

    /// Mark a session as running over a reliable, ordered transport (BTP).
    /// Forces MRP off for the session per Matter spec 4.12 (see
    /// [`Session::transport_reliable`]).
    ///
    /// # Errors
    ///
    /// [`Error::UnknownSession`] if `id` does not name a registered session.
    pub fn set_transport_reliable(&mut self, id: SessionId, reliable: bool) -> Result<()> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or(Error::UnknownSession(id.0))?;
        session.transport_reliable = reliable;
        Ok(())
    }

    /// Read a session's [`Session::transport_reliable`] flag.
    ///
    /// Returns `None` if `id` does not name a registered session.
    #[must_use]
    pub fn is_transport_reliable(&self, id: SessionId) -> Option<bool> {
        self.sessions.get(&id).map(|s| s.transport_reliable)
    }

    /// Encode an outbound Matter message via the named session.
    ///
    /// `exchange_id` is `None` to start a new exchange (we are its
    /// initiator, so the message carries the `I` flag), or `Some(id)` to
    /// send on an existing one. An id alone names one exchange unless both
    /// sides have started an exchange with it, which Matter allows (each
    /// node allocates its own ids, Matter 1.4 Core §4.10.2). Then `Some(id)`
    /// means the exchange **we** started. Otherwise it means the exchange
    /// the session holds with that id, and an id it does not hold is the
    /// peer's (`I=0`). To name the exchange without that ambiguity (to answer
    /// the peer's exchange after a collision, or to continue one of ours
    /// that has already been reclaimed), use
    /// [`Self::encode_outbound_with_role`].
    ///
    /// The pipeline is:
    /// 1. MRP builds the wire payload (protocol header || `app_payload`),
    ///    on a new exchange id if `exchange_id` is `None`, and copies into
    ///    it any piggyback ACK still owed on the same exchange.
    /// 2. Framing encrypts the payload under the session keys, producing
    ///    the secured-message wire bytes.
    /// 3. Only now, with the message certain to go out, does anything
    ///    change: the exchange id is spent, MRP records the exchange and
    ///    drains the ACK the message carries, and the outbound counter
    ///    advances.
    /// 4. MRP records the packet for retransmit if `mrp_flags.reliable` is
    ///    set, and updates the idle/active timing baseline regardless.
    ///
    /// On any error nothing has changed: the node's next exchange id, the
    /// session's exchange table, the ACK it owes and its outbound counter
    /// are as they were, so a failed send leaks no exchange slot and loses
    /// no ACK, and the next send reuses the counter nothing was sent under.
    ///
    /// # Errors
    ///
    /// - [`Error::UnknownSession`] if `session_id` does not exist.
    /// - [`Error::CounterOverflow`] if the session's outbound counter would
    ///   wrap past `u32::MAX`. The session must be re-keyed (a new PASE /
    ///   CASE handshake) to continue.
    /// - [`Error::PayloadTooLarge`] if the encoded wire payload (protocol
    ///   header + `app_payload`) exceeds the framing cap.
    /// - [`Error::Crypto`] if the AES-CCM cipher fails (not expected for a
    ///   payload under the cap).
    #[allow(clippy::too_many_arguments)] // Threaded protocol-header inputs.
    pub fn encode_outbound(
        &mut self,
        session_id: SessionId,
        exchange_id: Option<u16>,
        opcode: u8,
        protocol_id: ProtocolId,
        app_payload: &[u8],
        mrp_flags: MrpFlags,
        now: Instant,
    ) -> Result<EncodeOutboundOutput> {
        let target = match exchange_id {
            None => OutboundExchange::New,
            Some(id) => OutboundExchange::ById(id),
        };
        self.encode_on(
            session_id,
            target,
            opcode,
            protocol_id,
            app_payload,
            mrp_flags,
            now,
        )
    }

    /// [`Self::encode_outbound`] on an existing exchange named by its id
    /// **and** its role: `is_local_initiator` is true for an exchange we
    /// started and false for one the peer started.
    ///
    /// Matter identifies an exchange by {session, exchange id, role}
    /// (Matter 1.4 Core §4.10.2-3), because an exchange we start and one the
    /// peer starts may share an id. This is the form to use for a message on
    /// an exchange you already know the role of:
    ///
    /// - a follow-up on an exchange we started (a timed action, the next
    ///   chunk of a write, the `StatusResponse` that asks for the next read
    ///   chunk): `true`, the [`EncodeOutboundOutput::is_local_initiator`] of
    ///   the message that started it;
    /// - an answer on an exchange the peer started: `false`, which is
    ///   `!is_initiator` of the [`DecodeInboundOutput::AppMessage`] being
    ///   answered.
    ///
    /// The message carries the `I` flag exactly when `is_local_initiator` is
    /// true, and only the ACK owed on that exchange can ride it; an ACK owed
    /// on the other exchange with the same id stays owed. Unlike
    /// `encode_outbound(Some(id))`, this holds even after the exchange has
    /// been reclaimed (see [`Self::finish_exchange`]): a follow-up on one of
    /// ours still goes out with `I=1`.
    ///
    /// Nothing changes on error, exactly as for [`Self::encode_outbound`].
    ///
    /// # Errors
    ///
    /// As for [`Self::encode_outbound`]: [`Error::UnknownSession`],
    /// [`Error::CounterOverflow`], [`Error::PayloadTooLarge`] and
    /// [`Error::Crypto`].
    #[allow(clippy::too_many_arguments)] // Threaded protocol-header inputs.
    pub fn encode_outbound_with_role(
        &mut self,
        session_id: SessionId,
        exchange_id: u16,
        is_local_initiator: bool,
        opcode: u8,
        protocol_id: ProtocolId,
        app_payload: &[u8],
        mrp_flags: MrpFlags,
        now: Instant,
    ) -> Result<EncodeOutboundOutput> {
        self.encode_on(
            session_id,
            OutboundExchange::Exact(ExchangeKey {
                id: exchange_id,
                is_local_initiator,
            }),
            opcode,
            protocol_id,
            app_payload,
            mrp_flags,
            now,
        )
    }

    /// The pipeline behind [`Self::encode_outbound`] and
    /// [`Self::encode_outbound_with_role`]; `target` says which exchange the
    /// message goes on.
    #[allow(clippy::too_many_arguments)] // Threaded protocol-header inputs.
    fn encode_on(
        &mut self,
        session_id: SessionId,
        target: OutboundExchange,
        opcode: u8,
        protocol_id: ProtocolId,
        app_payload: &[u8],
        mrp_flags: MrpFlags,
        now: Instant,
    ) -> Result<EncodeOutboundOutput> {
        // A fresh exchange takes the next id from the node-wide counter (spec
        // §4.10.2), which lives on the manager, not the session. It is only
        // read here and spent in step 3, so a failed send spends no id.
        let next_exchange_id = self.next_exchange_id;

        let session = self
            .sessions
            .get_mut(&session_id)
            .ok_or(Error::UnknownSession(session_id.0))?;

        let (exchange, is_new_local_exchange) = match target {
            OutboundExchange::New => (
                ExchangeKey {
                    id: next_exchange_id,
                    is_local_initiator: true,
                },
                true,
            ),
            OutboundExchange::ById(id) => (session.mrp.resolve(id), false),
            OutboundExchange::Exact(exchange) => (exchange, false),
        };

        if session.outbound_counter == u32::MAX {
            return Err(Error::CounterOverflow);
        }

        // A transport_reliable session (BTP) forces MRP off regardless of
        // what the caller asked for: the underlying transport already
        // guarantees ordered, reliable delivery, so we never set the R bit
        // and never register a retransmit for this send. Mirrors chip's
        // `SecureSession::AllowsMRP()`.
        let effective_mrp_flags = MrpFlags {
            reliable: mrp_flags.reliable && !session.transport_reliable,
        };

        // 1. MRP builds the wire payload (protocol header || app_payload) on
        //    `exchange`, changing nothing. The key's role sets the `I` flag
        //    and picks which owed ACK, if any, can ride.
        let prepared = session.mrp.build_outbound(
            opcode,
            protocol_id,
            exchange,
            app_payload,
            effective_mrp_flags,
        );

        // 2. Framing encrypts. Its size check runs before it encrypts, and
        //    every error returns here, before step 3 changes anything.
        let secured_header = SecuredMessageHeader {
            flags: SecuredMessageFlags::empty(),
            session_id: session.peer_id,
            security_flags: SecurityFlags::empty(),
            message_counter: MessageCounter(session.outbound_counter),
            source_node_id: None,
            destination_node_id: None,
        };
        // Read the nonce node id out before borrowing the cipher: the cipher
        // accessor borrows `session` (to fill the lazy cache on first use),
        // so the scalar it needs must be copied first.
        let nonce_node_id = session.local_nonce_node_id;
        let wire_bytes = encode_secured_with_cipher(
            &secured_header,
            &prepared.wire_payload,
            session.outbound_cipher(),
            nonce_node_id,
        )?;

        // 3. The message will be sent: commit. Spend the exchange id, record
        //    the exchange and drain the ACK the header carries, then advance
        //    the counter.
        if is_new_local_exchange {
            Self::advance_exchange_id(&mut self.next_exchange_id);
        }
        session.mrp.commit_outbound(&prepared);
        let counter = MessageCounter(session.outbound_counter);
        session.outbound_counter = session.outbound_counter.wrapping_add(1);

        // 4. Record the send. mark_packet_sent always updates the
        //    idle/active timing baseline; pending-ack registration is gated
        //    internally by the `reliable` flag (already folded down to
        //    `effective_mrp_flags` above, so a transport_reliable session
        //    never registers a retransmit). The retransmit buffer is only
        //    retained for reliable messages, so we clone the wire bytes ONLY
        //    when `reliable` is set — an unreliable send hands `mark_packet_sent`
        //    an empty Vec (which it drops without copying), avoiding a full
        //    packet copy on every fire-and-forget datagram.
        let retransmit_copy = if effective_mrp_flags.reliable {
            wire_bytes.clone()
        } else {
            Vec::new()
        };
        session.mrp.mark_packet_sent_on(
            counter,
            prepared.exchange(),
            retransmit_copy,
            effective_mrp_flags.reliable,
            now,
        );

        Ok(EncodeOutboundOutput {
            wire_bytes,
            exchange_id: prepared.exchange_id,
            is_local_initiator: prepared.is_local_initiator,
            message_counter: counter,
            piggyback_acked: prepared.piggyback_acked,
        })
    }

    /// Decode + verify + replay-check an inbound packet, then dispatch into
    /// MRP for exchange / ack / duplicate-reliable handling.
    ///
    /// The replay window is consulted only after the packet authenticates.
    /// If it rejects the counter as a duplicate (`ReplayedCounter`
    /// or `CounterTooOld`, counter 0 included) and the message has R set on
    /// a session that is not `transport_reliable`, the manager builds a
    /// standalone ACK for it on its own exchange with the opposite role (the
    /// [`DecodeInboundOutput::DuplicateReliableAckResent`] variant), as
    /// chip's `ExchangeManager::SendStandaloneAckIfNeeded` and
    /// `ReliableMessageContext::HandleNeedsAckInner` do. Any ACK still
    /// buffered for that counter is dropped, so it is sent once. A
    /// duplicate's piggybacked ACK is not processed, and the duplicate is
    /// never delivered.
    ///
    /// # Errors
    ///
    /// - [`Error::MalformedHeader`] if the secured-message header is
    ///   malformed.
    /// - [`Error::UnknownSession`] if the header's `session_id` does not
    ///   match a registered session.
    /// - [`Error::ReplayedCounter`] / [`Error::CounterTooOld`] per the
    ///   session's replay window, for a duplicate that gets no ACK: one
    ///   without R, or any duplicate on a `transport_reliable` session.
    /// - [`Error::DecryptionFailed`] on AES-CCM tag failure.
    /// - [`Error::MalformedProtocolHeader`] if the decrypted bytes do not
    ///   parse as a valid Matter protocol header.
    /// - [`Error::CounterOverflow`] if building a duplicate-reliable
    ///   standalone-ack packet would overflow the outbound counter.
    pub fn decode_inbound(&mut self, packet: &[u8], now: Instant) -> Result<DecodeInboundOutput> {
        // Parse the secured header ONCE: we need it here to learn the
        // session_id, and the framing decoder takes it (plus its length) so
        // the same bytes are not re-parsed per inbound packet.
        let (peeked, rest) = crate::framing::decode_header(packet)?;
        let header_len = packet.len() - rest.len();
        let local_id = peeked.session_id;
        let peer_counter = peeked.message_counter;

        let session = self
            .sessions
            .get_mut(&local_id)
            .ok_or(Error::UnknownSession(local_id.0))?;

        // Attempt full decode. Scoped in a `let` so the cipher/replay-window
        // borrows of `session` end before the match arms below use `session`
        // again.
        let nonce_node_id = session.peer_nonce_node_id;
        let (replay, decrypted) = {
            let (cipher, replay_window) = session.inbound_cipher_and_window();
            let plaintext =
                decrypt_secured_with_header(packet, &peeked, header_len, cipher, nonce_node_id)?;
            // Only an authenticated message reaches the replay
            // window, and only it can earn a duplicate ACK below.
            (replay_window.check_and_record(peer_counter.0), plaintext)
        };
        match replay {
            Ok(()) => {
                let outcome = session.mrp.process_inbound(decrypted, peer_counter, now)?;
                // A transport_reliable session (BTP) skips ack scheduling
                // entirely, even if the peer's message carried R=1:
                // `process_inbound` above unconditionally arms a
                // standalone-ack deadline for a reliable-flagged inbound, so
                // undo that here rather than threading the flag through MRP.
                // Defensive only — a well-behaved BTP peer never sets R.
                if session.transport_reliable {
                    if let InboundOutcome::AppMessage {
                        exchange_id,
                        is_initiator,
                        ..
                    } = &outcome
                    {
                        session
                            .mrp
                            .cancel_pending_outbound_ack(ExchangeKey::of_inbound(
                                *exchange_id,
                                *is_initiator,
                            ));
                    }
                }
                Ok(match outcome {
                    InboundOutcome::AppMessage {
                        exchange_id,
                        is_initiator,
                        protocol_id,
                        opcode,
                        payload,
                    } => DecodeInboundOutput::AppMessage {
                        session_id: local_id,
                        exchange_id,
                        is_initiator,
                        protocol_id,
                        opcode,
                        payload,
                    },
                    InboundOutcome::AckOnly {
                        exchange_id,
                        acked_counter,
                    } => DecodeInboundOutput::AckOnly {
                        session_id: local_id,
                        exchange_id,
                        acked_counter,
                    },
                    InboundOutcome::DuplicateReliable { .. } => {
                        // process_inbound only emits DuplicateReliable on
                        // the replay-window rejection path, which is
                        // handled in the Err arm below. If we ever get
                        // here, MRP and the replay window are out of sync
                        // — surface as DecryptionFailed (the closest
                        // existing variant) rather than panicking.
                        return Err(Error::DecryptionFailed);
                    }
                })
            }
            Err(err @ (Error::ReplayedCounter { .. } | Error::CounterTooOld { .. })) => {
                Self::answer_duplicate(session, local_id, peer_counter, &decrypted, err)
            }
            Err(other) => Err(other),
        }
    }

    /// Answer an authenticated duplicate the way chip does
    /// (`SessionManager.cpp:963-1005`, `ExchangeMgr.cpp:415-446`,
    /// `ReliableMessageContext.cpp:116-153`, the same at master 5cd2917a and
    /// v1.4.2.0): if it has R set and the session runs MRP (not
    /// `transport_reliable`; chip's `AllowsMRP()` is UDP-only), build one
    /// standalone ACK for its counter, on its exchange with the opposite
    /// role, and drop any ACK still buffered for that counter so it is sent
    /// once. Its piggybacked ACK is not processed, no exchange state is
    /// touched, and nothing is delivered. Otherwise return `replay_err`.
    fn answer_duplicate(
        session: &mut Session,
        session_id: SessionId,
        counter: MessageCounter,
        plaintext: &[u8],
        replay_err: Error,
    ) -> Result<DecodeInboundOutput> {
        let (header, _) = crate::protocol_header::decode_protocol_header(plaintext)?;
        let reliable = header
            .exchange_flags
            .contains(crate::protocol_header::ExchangeFlags::RELIABLE);
        if !reliable || session.transport_reliable {
            return Err(replay_err);
        }
        // The duplicate's I flag names its sender's role, so the ACK goes
        // out with the opposite one (chip creates the ephemeral exchange
        // with `!payloadHeader.IsInitiator()`).
        let is_local_initiator = !header
            .exchange_flags
            .contains(crate::protocol_header::ExchangeFlags::INITIATOR);
        let ack_packet = Self::build_standalone_ack_packet(
            session,
            header.exchange_id,
            counter,
            is_local_initiator,
        )?;
        // The duplicate gets its ACK now; drop any copy still buffered for
        // the same counter so a later finish or timer does not send a second
        // (chip clears its pending flag here too).
        session.mrp.forget_ack_for(counter);
        Ok(DecodeInboundOutput::DuplicateReliableAckResent {
            session_id,
            exchange_id: header.exchange_id,
            ack_packet,
        })
    }

    /// Earliest pending MRP deadline across every registered session, or
    /// `None` if no session has any pending retransmit or piggyback-ack
    /// flush deadline.
    #[must_use]
    pub fn poll_timeout(&self) -> Option<Instant> {
        self.sessions
            .values()
            .filter_map(|s| s.mrp.poll_timeout())
            .min()
    }

    /// Drain every session's MRP timer events at `now`, folding them into
    /// manager-wide [`MrpEvent`]s. Standalone-ack-deadline events are
    /// materialised into encrypted ack packets here (consuming a session
    /// outbound counter slot) so the caller only sees ready-to-send bytes.
    ///
    /// If building a standalone-ack packet fails because the session's
    /// outbound counter is exhausted, a [`MrpEvent::SessionCounterExhausted`]
    /// is emitted instead of the ack: the counter cannot wrap (Matter Core
    /// Spec §4.6.2), so the session must be re-keyed. Surfacing the event
    /// (rather than silently dropping the ack) lets the caller tear the
    /// session down promptly instead of leaving the peer un-acked with no
    /// signal.
    pub fn handle_timeout(&mut self, now: Instant) -> Vec<MrpEvent> {
        let mut out = Vec::new();
        let session_ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for sid in session_ids {
            let Some(session) = self.sessions.get_mut(&sid) else {
                continue;
            };
            let timer_events = session.mrp.handle_timeout(now);
            for event in timer_events {
                out.extend(Self::fold_timer_event(sid, session, event));
            }
        }
        out
    }

    /// Fold one per-session timer event into the manager-level [`MrpEvent`],
    /// building the encrypted packet for a
    /// [`MrpTimerEvent::StandaloneAckDeadlineFired`] (one outbound counter).
    ///
    /// The one path every standalone ACK from MRP's timer or close takes, so
    /// [`Self::handle_timeout`], [`Self::finish_exchange`],
    /// [`Self::abort_exchange`] and [`Self::flush_session_acks`] (which
    /// [`Self::flush_pending_acks`] calls per session) cannot drift apart. An
    /// exhausted counter becomes
    /// [`MrpEvent::SessionCounterExhausted`] (the counter cannot overflow,
    /// Matter Core Spec §4.6.2, so the caller must re-key); any other build
    /// failure (`PayloadTooLarge`, impossible for a header-only packet) drops
    /// the event.
    fn fold_timer_event(
        session_id: SessionId,
        session: &mut Session,
        event: MrpTimerEvent,
    ) -> Option<MrpEvent> {
        match event {
            MrpTimerEvent::Retransmit {
                exchange_id,
                counter,
                packet,
            } => Some(MrpEvent::Retransmit {
                session_id,
                exchange_id,
                counter,
                packet,
            }),
            MrpTimerEvent::Expired {
                exchange_id,
                counter,
            } => Some(MrpEvent::Expired {
                session_id,
                exchange_id,
                counter,
            }),
            MrpTimerEvent::StandaloneAckDeadlineFired {
                exchange_id,
                ack_counter,
                is_local_initiator,
            } => match Self::build_standalone_ack_packet(
                session,
                exchange_id,
                ack_counter,
                is_local_initiator,
            ) {
                Ok(packet) => Some(MrpEvent::SendStandaloneAck {
                    session_id,
                    exchange_id,
                    packet,
                }),
                Err(Error::CounterOverflow) => Some(MrpEvent::SessionCounterExhausted {
                    session_id,
                    exchange_id,
                    ack_counter,
                }),
                Err(_) => None,
            },
        }
    }

    /// Finish an exchange: return the standalone ACK it still owes as an
    /// [`MrpEvent::SendStandaloneAck`] to send now, and reclaim it
    /// ([`MrpState::flush_exchange_acks`]). Our own retransmits on it keep
    /// running (chip's `Close()`).
    ///
    /// Not to be confused with [`MrpState::close_exchange`], which is an
    /// abort that **drops** the owed ACK unsent; this method is the one that
    /// sends it. [`Self::abort_exchange`] is the abort that sends it first.
    ///
    /// Matter Core Spec §4.12 lets an ACK wait up to `MRP_STANDALONE_ACK_TIMEOUT`
    /// for a message to piggyback on; once the exchange is over there will
    /// be none (§4.12 does not allow piggybacking onto another exchange), so
    /// chip (`ExchangeContext::DoClose` → `FlushAcks`) and matter.js
    /// (`MessageExchange.close()`) send it at once. Call this at the point an
    /// exchange ends, before answering whoever waited on it, and never while
    /// the exchange will carry another message from us (see
    /// [`MrpState::flush_exchange_acks`]).
    ///
    /// Returns what [`Self::handle_timeout`] would have: a
    /// [`MrpEvent::SendStandaloneAck`], or a
    /// [`MrpEvent::SessionCounterExhausted`] if the counter is spent. Empty
    /// when nothing is owed, the session is unknown, or the session runs over
    /// a reliable transport (BTP never buffers an MRP ACK).
    ///
    /// `exchange_id` means the exchange we started if the session holds one
    /// with this id, and otherwise the peer's, as for
    /// [`Self::encode_outbound`]; [`Self::finish_exchange_with_role`] names
    /// the role explicitly.
    #[must_use = "the returned events carry standalone-ACK packets to send"]
    pub fn finish_exchange(&mut self, session_id: SessionId, exchange_id: u16) -> Vec<MrpEvent> {
        self.end_exchange(session_id, ExchangeTarget::ById(exchange_id), false)
    }

    /// [`Self::finish_exchange`] for the exchange named by its id **and**
    /// its role (`is_local_initiator`: true for an exchange we started,
    /// false for one the peer started), as
    /// [`Self::encode_outbound_with_role`] names it. Only that exchange's
    /// owed ACK is sent and only it is reclaimed; an exchange with the same
    /// id and the other role is left alone.
    #[must_use = "the returned events carry standalone-ACK packets to send"]
    pub fn finish_exchange_with_role(
        &mut self,
        session_id: SessionId,
        exchange_id: u16,
        is_local_initiator: bool,
    ) -> Vec<MrpEvent> {
        let exchange = ExchangeKey {
            id: exchange_id,
            is_local_initiator,
        };
        self.end_exchange(session_id, ExchangeTarget::Exact(exchange), false)
    }

    /// Abort an exchange the way chip's `Abort()` does: send the ACK it still
    /// owes first, exactly as [`Self::finish_exchange`], then drop our own
    /// pending retransmits on it ([`MrpState::close_exchange`]). For an
    /// exchange we are abandoning, such as a cancelled request.
    ///
    /// `exchange_id` names the exchange as for [`Self::finish_exchange`];
    /// [`Self::abort_exchange_with_role`] names the role explicitly.
    #[must_use = "the returned events carry standalone-ACK packets to send"]
    pub fn abort_exchange(&mut self, session_id: SessionId, exchange_id: u16) -> Vec<MrpEvent> {
        self.end_exchange(session_id, ExchangeTarget::ById(exchange_id), true)
    }

    /// [`Self::abort_exchange`] for the exchange named by its id **and** its
    /// role, as [`Self::finish_exchange_with_role`] names it. Our
    /// retransmits on an exchange with the same id and the other role keep
    /// running.
    #[must_use = "the returned events carry standalone-ACK packets to send"]
    pub fn abort_exchange_with_role(
        &mut self,
        session_id: SessionId,
        exchange_id: u16,
        is_local_initiator: bool,
    ) -> Vec<MrpEvent> {
        let exchange = ExchangeKey {
            id: exchange_id,
            is_local_initiator,
        };
        self.end_exchange(session_id, ExchangeTarget::Exact(exchange), true)
    }

    /// The body of the finish and abort methods: flush the exchange's owed
    /// ACKs, then, for an abort, drop its retransmits and its entry.
    fn end_exchange(
        &mut self,
        session_id: SessionId,
        target: ExchangeTarget,
        abort: bool,
    ) -> Vec<MrpEvent> {
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return Vec::new();
        };
        // Resolve once, before the flush can reclaim the entry that decides
        // which exchange a bare id means.
        let exchange = match target {
            ExchangeTarget::ById(id) => session.mrp.resolve(id),
            ExchangeTarget::Exact(exchange) => exchange,
        };
        let flushed = session.mrp.flush_exchange_acks_on(exchange);
        if abort {
            session.mrp.close_exchange_on(exchange);
        }
        flushed
            .into_iter()
            .filter_map(|event| Self::fold_timer_event(session_id, session, event))
            .collect()
    }

    /// Every standalone ACK any session still owes, as [`Self::finish_exchange`]
    /// would return them, in ascending session-id order. For teardown, when
    /// nothing will pump [`Self::handle_timeout`] again.
    #[must_use = "the returned events carry standalone-ACK packets to send"]
    pub fn flush_pending_acks(&mut self) -> Vec<MrpEvent> {
        let mut session_ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        session_ids.sort_unstable_by_key(|sid| sid.0);
        let mut out = Vec::new();
        for sid in session_ids {
            out.extend(self.flush_session_acks(sid));
        }
        out
    }

    /// Every standalone ACK one session still owes, as
    /// [`Self::finish_exchange`] would return them, in ascending exchange-id
    /// order: for a caller about to [`Self::remove`] the session on purpose.
    ///
    /// matter.js sends these when it closes a session (`Session.ts:279-284`:
    /// each open exchange's `close()` flushes its ACK while the session still
    /// exists). chip, in effect, does not: its release path reaches
    /// `FlushAcks` only after `SessionHolderWithDelegate::SessionReleased`
    /// has released the session (`Session.h:178-184`), so the send fails with
    /// `CHIP_ERROR_CONNECTION_ABORTED` (`ExchangeContext.cpp:101` at master
    /// 5cd2917a, `:99` at v1.4.2.0). [`Self::remove`] itself and the table's
    /// internal eviction flush nothing, as in chip.
    ///
    /// Empty for an unknown session, or one on a reliable transport (BTP
    /// never buffers an MRP ACK).
    ///
    /// Call it only when the session will carry nothing more: like
    /// [`Self::finish_exchange`], it reclaims every exchange it leaves idle, so
    /// a later message on one of our still-open exchanges would go out on a
    /// re-created exchange without the initiator flag.
    #[must_use = "the returned events carry standalone-ACK packets to send"]
    pub fn flush_session_acks(&mut self, session_id: SessionId) -> Vec<MrpEvent> {
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return Vec::new();
        };
        let flushed = session.mrp.flush_all_acks();
        flushed
            .into_iter()
            .filter_map(|event| Self::fold_timer_event(session_id, session, event))
            .collect()
    }

    /// Build a standalone-ack secured-message packet. Used by
    /// [`Self::decode_inbound`]'s duplicate-reliable-resend path and by
    /// [`Self::fold_timer_event`], which every timer and close path goes
    /// through.
    ///
    /// Consumes one outbound counter slot on `session`.
    ///
    /// # Errors
    ///
    /// - [`Error::CounterOverflow`] if `session.outbound_counter` is
    ///   already `u32::MAX`.
    /// - [`Error::PayloadTooLarge`] surfacing from the underlying
    ///   framing encode — the cipher-taking internal twin of
    ///   [`crate::framing::encode_secured`]. It will not occur in practice
    ///   for a standalone-ack header (the encoded form is well under 16
    ///   bytes) but is propagated for completeness.
    pub(crate) fn build_standalone_ack_packet(
        session: &mut Session,
        exchange_id: u16,
        ack_counter: MessageCounter,
        is_local_initiator: bool,
    ) -> Result<Vec<u8>> {
        if session.outbound_counter == u32::MAX {
            return Err(Error::CounterOverflow);
        }
        let header = build_standalone_ack_header(exchange_id, ack_counter, is_local_initiator);
        let mut payload = Vec::with_capacity(16);
        encode_protocol_header(&header, &mut payload);
        // No app payload.
        let secured_header = SecuredMessageHeader {
            flags: SecuredMessageFlags::empty(),
            session_id: session.peer_id,
            security_flags: SecurityFlags::empty(),
            message_counter: MessageCounter(session.outbound_counter),
            source_node_id: None,
            destination_node_id: None,
        };
        let nonce_node_id = session.local_nonce_node_id;
        let packet = encode_secured_with_cipher(
            &secured_header,
            &payload,
            session.outbound_cipher(),
            nonce_node_id,
        )?;
        session.outbound_counter = session.outbound_counter.wrapping_add(1);
        Ok(packet)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use std::time::Instant;

    use matter_crypto::pase::PaseSessionKeys;

    use super::*;
    use crate::mrp::MrpFlags;
    use crate::protocol_header::ProtocolId;

    /// The exchange-id counter belongs to the NODE, not the session.
    ///
    /// Matter Core Spec §4.10.2: one counter per initiator node, incrementing.
    /// It used to live on `MrpState`, i.e. one per session, which meant two
    /// sessions handed out the same ids.
    #[test]
    fn exchange_ids_come_from_one_counter_across_sessions() {
        let mut mgr = SessionManager::new();
        let first = mgr.allocate_exchange_id();
        let second = mgr.allocate_exchange_id();
        assert_eq!(
            second,
            first.wrapping_add(1),
            "ids must increment from a single node-wide counter"
        );
    }

    /// The first exchange id "SHALL be a random integer" (§4.10.2). Two managers
    /// in one process must therefore not start from the same place.
    ///
    /// Probabilistic by nature: with a 16-bit seed a genuine collision happens
    /// once in 65536 runs, so this samples several managers and requires only
    /// that they are not ALL identical — enough to catch a hard-coded seed,
    /// which is the regression that matters, without flaking.
    #[test]
    fn the_first_exchange_id_is_seeded_randomly() {
        let seeds: std::collections::HashSet<u16> = (0..16)
            .map(|_| SessionManager::new().allocate_exchange_id())
            .collect();
        assert!(
            seeds.len() > 1,
            "every SessionManager started from the same exchange id -- the seed is not random"
        );
    }

    /// The spec says the id "rolls over to zero when its maximum value is
    /// exceeded", so 0 is legal. The old per-session allocator skipped it.
    #[test]
    fn exchange_id_wraps_through_zero() {
        let mut mgr = SessionManager::new();
        mgr.next_exchange_id = u16::MAX;
        assert_eq!(mgr.allocate_exchange_id(), u16::MAX);
        assert_eq!(mgr.allocate_exchange_id(), 0, "0 is a legal exchange id");
        assert_eq!(mgr.allocate_exchange_id(), 1);
    }

    /// Two `SessionManager`s sharing one symmetric key set, cross-registered as
    /// Initiator/Responder. Both allocate local id 1 (allocator starts at 1),
    /// so each side's `peer_session_id` is 1.
    fn paired_sessions() -> (SessionManager, SessionManager) {
        let keys = PaseSessionKeys {
            ke: [0u8; 16],
            i2r_key: [1u8; 16],
            r2i_key: [2u8; 16],
            attestation_key: [3u8; 16],
        };
        let mut a = SessionManager::new();
        let mut b = SessionManager::new();
        a.register_pase(keys.clone(), SessionRole::Initiator, 1, PeerHint::default());
        b.register_pase(keys, SessionRole::Responder, 1, PeerHint::default());
        (a, b)
    }

    /// The largest first counter `Crypto_DRBG(len = 28) + 1` can produce
    /// (spec §4.6.1.1): `0x0FFF_FFFF + 1`.
    const MAX_INITIAL_COUNTER: u32 = 1 << 28;

    /// The DRBG-to-counter mapping hits both ends of `[1, 2^28]` and ignores
    /// the top four bits, as chip's `kMessageCounterRandomInitMask` does.
    #[test]
    fn initial_counter_from_drbg_spans_the_spec_range() {
        assert_eq!(initial_counter_from_drbg([0x00; 4]), 1);
        assert_eq!(initial_counter_from_drbg([0xFF; 4]), MAX_INITIAL_COUNTER);
        // Only the top nibble set: masked away, so the smallest value.
        assert_eq!(initial_counter_from_drbg([0x00, 0x00, 0x00, 0xF0]), 1);
        // Little-endian, like the exchange-id seed.
        assert_eq!(initial_counter_from_drbg([0x01, 0x00, 0x00, 0x00]), 2);
    }

    /// Spec §4.6.1.1 / §4.6.2: every new secure session's outbound counter
    /// starts at a CSPRNG value in `[1, 2^28]`. Checked on both PASE register
    /// paths (CASE and resumed CASE share `insert_session` and are checked
    /// on real handshake outputs in matter-commissioning's CASE tests).
    ///
    /// Distinctness is probabilistic: 16 draws from 2^28 values all being
    /// equal has probability 2^-420, so a failure means the counter is not
    /// being seeded (on the start-at-1 code every draw was 1).
    #[test]
    fn new_sessions_start_at_a_random_counter_in_the_spec_range() {
        let mut mgr = SessionManager::new();
        let mut counters = Vec::new();
        for _ in 0..8 {
            let sid = mgr.register_pase(
                PaseSessionKeys {
                    ke: [0u8; 16],
                    i2r_key: [1u8; 16],
                    r2i_key: [2u8; 16],
                    attestation_key: [3u8; 16],
                },
                SessionRole::Initiator,
                1,
                PeerHint::default(),
            );
            counters.push(mgr.get(sid).unwrap().outbound_counter);
        }
        for _ in 0..8 {
            let sid = mgr.allocate_session_id();
            mgr.register_pase_with_local_id(
                sid,
                PaseSessionKeys {
                    ke: [0u8; 16],
                    i2r_key: [1u8; 16],
                    r2i_key: [2u8; 16],
                    attestation_key: [3u8; 16],
                },
                SessionRole::Responder,
                1,
                PeerHint::default(),
            );
            counters.push(mgr.get(sid).unwrap().outbound_counter);
        }
        for c in &counters {
            assert!(
                (1..=MAX_INITIAL_COUNTER).contains(c),
                "counter {c:#x} outside [1, 2^28]"
            );
        }
        assert!(
            counters.iter().any(|c| *c != counters[0]),
            "16 sessions all started at {:#x}: the counter is not CSPRNG-seeded",
            counters[0]
        );
    }

    /// A seeded start changes nothing about advancing and exhaustion: from
    /// the largest seed the counter climbs by one per send, and near the top
    /// it sends up to `u32::MAX - 1` and then refuses rather than wrap.
    #[test]
    fn counter_advances_and_exhausts_from_a_high_start() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let send = |mgr: &mut SessionManager| {
            mgr.encode_outbound(
                sid,
                None,
                0x02,
                ProtocolId::INTERACTION_MODEL,
                b"x",
                MrpFlags { reliable: false },
                Instant::now(),
            )
        };

        a.sessions.get_mut(&sid).unwrap().outbound_counter = MAX_INITIAL_COUNTER;
        let out = send(&mut a).unwrap();
        assert_eq!(out.message_counter, MessageCounter(MAX_INITIAL_COUNTER));
        assert_eq!(
            a.get(sid).unwrap().outbound_counter,
            MAX_INITIAL_COUNTER + 1
        );
        // The peer accepts a first counter at the top of the seed range.
        b.decode_inbound(&out.wire_bytes, Instant::now()).unwrap();

        a.sessions.get_mut(&sid).unwrap().outbound_counter = u32::MAX - 2;
        let out = send(&mut a).unwrap();
        assert_eq!(out.message_counter, MessageCounter(u32::MAX - 2));
        b.decode_inbound(&out.wire_bytes, Instant::now()).unwrap();
        let out = send(&mut a).unwrap();
        assert_eq!(out.message_counter, MessageCounter(u32::MAX - 1));
        b.decode_inbound(&out.wire_bytes, Instant::now()).unwrap();
        assert!(matches!(send(&mut a), Err(Error::CounterOverflow)));
        assert_eq!(a.get(sid).unwrap().outbound_counter, u32::MAX);
    }

    #[test]
    fn decode_inbound_surfaces_protocol_and_opcode() {
        let (mut a, mut b) = paired_sessions();
        let session = SessionId(1);
        let out = a
            .encode_outbound(
                session,
                None,
                0x08, // InvokeRequest
                ProtocolId::INTERACTION_MODEL,
                b"payload",
                MrpFlags { reliable: true },
                Instant::now(),
            )
            .unwrap();
        let DecodeInboundOutput::AppMessage {
            protocol_id,
            opcode,
            payload,
            ..
        } = b.decode_inbound(&out.wire_bytes, Instant::now()).unwrap()
        else {
            panic!("expected AppMessage");
        };
        assert_eq!(protocol_id, ProtocolId::INTERACTION_MODEL);
        assert_eq!(opcode, 0x08);
        assert_eq!(payload, b"payload");
    }

    /// An UNRELIABLE outbound must NOT register a retransmit entry — there is
    /// no pending deadline afterwards. This pins the perf fix that skips the
    /// retransmit-buffer clone for fire-and-forget datagrams: a clone is only
    /// worthwhile if MRP will actually hold the bytes for re-send.
    #[test]
    fn unreliable_outbound_registers_no_retransmit() {
        let (mut a, _b) = paired_sessions();
        let now = Instant::now();
        a.encode_outbound(
            SessionId(1),
            None,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"fire-and-forget",
            MrpFlags { reliable: false },
            now,
        )
        .unwrap();
        assert_eq!(
            a.poll_timeout(),
            None,
            "an unreliable send must not schedule a retransmit"
        );
    }

    /// An unreliable round-trip still decodes correctly. This exercises the
    /// AAD-slice path in `decode_secured` (the AAD is sliced from the input
    /// header bytes rather than re-encoded) and the in-place payload split in
    /// MRP `process_inbound` on a non-reliable packet.
    #[test]
    fn unreliable_roundtrip_decodes() {
        let (mut a, mut b) = paired_sessions();
        let out = a
            .encode_outbound(
                SessionId(1),
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"unreliable payload",
                MrpFlags { reliable: false },
                Instant::now(),
            )
            .unwrap();
        let DecodeInboundOutput::AppMessage {
            protocol_id,
            opcode,
            payload,
            ..
        } = b.decode_inbound(&out.wire_bytes, Instant::now()).unwrap()
        else {
            panic!("expected AppMessage");
        };
        assert_eq!(protocol_id, ProtocolId::INTERACTION_MODEL);
        assert_eq!(opcode, 0x08);
        assert_eq!(payload, b"unreliable payload");
    }

    /// When a standalone-ack is due but the session's outbound counter is
    /// exhausted (`u32::MAX`), `handle_timeout` must SURFACE the condition
    /// (via `MrpEvent::SessionCounterExhausted`) rather than silently
    /// dropping the ack and leaving the peer un-acked with no signal.
    #[test]
    fn standalone_ack_counter_overflow_surfaces_event() {
        let (_a, mut b) = paired_sessions();
        let session_id = SessionId(1);
        let now = Instant::now();

        // Buffer a reliable inbound on `b` so a standalone-ack becomes due.
        // We hand-build a reliable inbound payload directly through MRP so we
        // don't need a real encrypted frame from `a`.
        {
            let session = b.sessions.get_mut(&session_id).unwrap();
            let header = crate::protocol_header::ProtocolHeader {
                exchange_flags: crate::protocol_header::ExchangeFlags::INITIATOR
                    | crate::protocol_header::ExchangeFlags::RELIABLE,
                opcode: 0x02,
                exchange_id: 0x4242,
                protocol_id: ProtocolId::INTERACTION_MODEL,
                ack_counter: None,
            };
            let mut payload = Vec::new();
            encode_protocol_header(&header, &mut payload);
            session
                .mrp
                .process_inbound(payload, MessageCounter(100), now)
                .unwrap();
            // Force the outbound counter to the exhaustion point.
            session.outbound_counter = u32::MAX;
        }

        // Fire the standalone-ack deadline. The ack cannot be built (counter
        // exhausted), so we must see a SessionCounterExhausted event.
        let events = b.handle_timeout(now + std::time::Duration::from_millis(200));
        assert!(
            events.iter().any(|e| matches!(
                e,
                MrpEvent::SessionCounterExhausted {
                    session_id: s,
                    exchange_id: 0x4242,
                    ack_counter,
                } if *s == session_id && *ack_counter == MessageCounter(100)
            )),
            "counter-overflow standalone-ack must surface SessionCounterExhausted, got {events:?}"
        );
    }

    /// Hand a reliable inbound for `exchange_id` straight to `mgr`'s MRP, so a
    /// standalone ACK is buffered without a real encrypted frame (the same
    /// shortcut `standalone_ack_counter_overflow_surfaces_event` takes).
    fn buffer_reliable_inbound(
        mgr: &mut SessionManager,
        session_id: SessionId,
        exchange_id: u16,
        counter: MessageCounter,
        now: Instant,
    ) {
        let header = crate::protocol_header::ProtocolHeader {
            exchange_flags: crate::protocol_header::ExchangeFlags::INITIATOR
                | crate::protocol_header::ExchangeFlags::RELIABLE,
            opcode: 0x02,
            exchange_id,
            protocol_id: ProtocolId::INTERACTION_MODEL,
            ack_counter: None,
        };
        let mut payload = Vec::new();
        encode_protocol_header(&header, &mut payload);
        mgr.sessions
            .get_mut(&session_id)
            .unwrap()
            .mrp
            .process_inbound(payload, counter, now)
            .unwrap();
    }

    /// Counter exhaustion: finishing an exchange whose ACK cannot be built (the
    /// outbound counter is at `u32::MAX`; spec §4.6.2 forbids an overflow)
    /// reports it once, as `handle_timeout` does, and never wraps the counter.
    #[test]
    fn finish_exchange_on_an_exhausted_counter_surfaces_the_event_once() {
        let (_a, mut b) = paired_sessions();
        let session_id = SessionId(1);
        let now = Instant::now();
        buffer_reliable_inbound(&mut b, session_id, 0x4242, MessageCounter(100), now);
        b.sessions.get_mut(&session_id).unwrap().outbound_counter = u32::MAX;

        let events = b.finish_exchange(session_id, 0x4242);

        assert!(
            matches!(
                events.as_slice(),
                [MrpEvent::SessionCounterExhausted {
                    session_id: s,
                    exchange_id: 0x4242,
                    ack_counter,
                }] if *s == session_id && *ack_counter == MessageCounter(100)
            ),
            "got {events:?}"
        );
        assert_eq!(
            b.sessions.get(&session_id).unwrap().outbound_counter,
            u32::MAX,
            "never wraps"
        );
        assert_eq!(
            b.handle_timeout(now + std::time::Duration::from_secs(1))
                .len(),
            0,
            "reported once, not again by the timer"
        );
    }

    /// BTP: a `transport_reliable` session never buffers an MRP ACK, even
    /// when a buggy peer sets R (BTP acknowledges at its own layer, spec
    /// §4.12), so finishing an exchange on it sends nothing and spends no
    /// counter.
    #[test]
    fn finish_exchange_on_a_transport_reliable_session_is_empty() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        // Only `b` is on BTP: `a` still sets R=1, which `b` must ignore.
        b.set_transport_reliable(sid, true).unwrap();
        let now = Instant::now();
        let out = a
            .encode_outbound(
                sid,
                None,
                0x02,
                ProtocolId::INTERACTION_MODEL,
                b"req",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        b.decode_inbound(&out.wire_bytes, now).unwrap();
        let counter_before = b.sessions.get(&sid).unwrap().outbound_counter;

        // Checked first, while nothing has taken any buffered ACK yet.
        assert_eq!(b.flush_session_acks(sid).len(), 0);
        assert_eq!(b.finish_exchange(sid, out.exchange_id).len(), 0);
        assert_eq!(b.flush_pending_acks().len(), 0);
        assert_eq!(
            b.sessions.get(&sid).unwrap().outbound_counter,
            counter_before
        );
    }

    /// A session that is gone: finishing or aborting on it is a no-op value.
    #[test]
    fn finish_exchange_on_an_unknown_session_is_empty() {
        let (_a, mut b) = paired_sessions();
        assert_eq!(b.finish_exchange(SessionId(99), 1).len(), 0);
        assert_eq!(b.abort_exchange(SessionId(99), 1).len(), 0);
    }

    /// A caller removing a session on purpose takes the ACKs it still owes
    /// first, as packets the peer accepts; a second call finds nothing, and an
    /// unknown session yields nothing. matter.js sends these when it closes a
    /// session; chip's release path reaches `FlushAcks` only after the
    /// session holder is released, so its send fails.
    #[test]
    fn flush_session_acks_returns_what_a_session_owes() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        let out = a
            .encode_outbound(
                sid,
                None,
                0x05,
                ProtocolId::INTERACTION_MODEL,
                b"report",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        b.decode_inbound(&out.wire_bytes, now).unwrap();

        let events = b.flush_session_acks(sid);

        let [MrpEvent::SendStandaloneAck {
            session_id,
            exchange_id,
            packet,
        }] = events.as_slice()
        else {
            panic!("expected one standalone ACK, got {events:?}");
        };
        assert_eq!((*session_id, *exchange_id), (sid, out.exchange_id));
        match a.decode_inbound(packet, now).unwrap() {
            DecodeInboundOutput::AckOnly { acked_counter, .. } => {
                assert_eq!(acked_counter, out.message_counter);
            }
            other => panic!("expected AckOnly, got {other:?}"),
        }
        assert_eq!(b.flush_session_acks(sid).len(), 0, "taken once");
        assert_eq!(b.poll_timeout(), None);
        assert_eq!(
            b.flush_session_acks(SessionId(99)).len(),
            0,
            "unknown session"
        );
    }

    /// The exchange flags inside `packet`, decrypted with `receiver`'s keys
    /// for session `sid` (a fresh replay window, so `receiver`'s own is
    /// untouched).
    fn exchange_flags_of(
        receiver: &SessionManager,
        sid: SessionId,
        packet: &[u8],
    ) -> crate::protocol_header::ExchangeFlags {
        let session = receiver.get(sid).unwrap();
        let (_, plaintext) = crate::framing::decode_secured(
            packet,
            &session.keys,
            session.role,
            &mut crate::framing::ReplayWindow::new(),
            session.peer_nonce_node_id,
        )
        .unwrap();
        crate::protocol_header::decode_protocol_header(&plaintext)
            .unwrap()
            .0
            .exchange_flags
    }

    /// A session's flush includes its displaced ACKs (a second reliable
    /// message on one exchange forces the first one's ACK out), each with its
    /// own exchange's I flag, in ascending exchange-id order and, within an
    /// exchange, the displaced (older) ACK first. Three exchanges on `b`:
    /// - `0x0100`, started by `a`: one buffered ACK, sent without I;
    /// - `0x0101`, started by `a`: only a displaced ACK left, because `b`'s
    ///   unreliable answer piggybacked the newer one; sent without I;
    /// - `0x0200`, started by `b`: a displaced and a buffered ACK, both with
    ///   I (spec §4.4.3.1; chip matches an inbound to its exchange on it).
    ///
    /// `a` accepts every one, so nothing is left on either side's timer.
    #[test]
    fn flush_session_acks_includes_displaced_acks_with_their_i_flags() {
        use crate::protocol_header::ExchangeFlags;

        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        a.next_exchange_id = 0x0100;
        b.next_exchange_id = 0x0200;
        let reliable = MrpFlags { reliable: true };
        let im = ProtocolId::INTERACTION_MODEL;

        // 0x0100: one reliable message from `a`, its ACK buffered on `b`.
        let y = a
            .encode_outbound(sid, None, 0x06, im, b"write", reliable, now)
            .unwrap();
        b.decode_inbound(&y.wire_bytes, now).unwrap();

        // 0x0101: two reliable messages from `a` displace the first one's
        // ACK; `b`'s unreliable answer piggybacks the second one's.
        let z1 = a
            .encode_outbound(sid, None, 0x02, im, b"z1", reliable, now)
            .unwrap();
        let z2 = a
            .encode_outbound(sid, Some(z1.exchange_id), 0x02, im, b"z2", reliable, now)
            .unwrap();
        b.decode_inbound(&z1.wire_bytes, now).unwrap();
        b.decode_inbound(&z2.wire_bytes, now).unwrap();
        let answer = b
            .encode_outbound(
                sid,
                Some(z1.exchange_id),
                0x01,
                im,
                b"status",
                MrpFlags::default(),
                now,
            )
            .unwrap();
        a.decode_inbound(&answer.wire_bytes, now).unwrap();

        // 0x0200: `b` starts it; `a`'s first reply piggybacks the ACK for
        // `b`'s request, then two reliable messages displace the first ACK.
        let x = b
            .encode_outbound(sid, None, 0x02, im, b"req", reliable, now)
            .unwrap();
        a.decode_inbound(&x.wire_bytes, now).unwrap();
        let m1 = a
            .encode_outbound(sid, Some(x.exchange_id), 0x05, im, b"m1", reliable, now)
            .unwrap();
        let m2 = a
            .encode_outbound(sid, Some(x.exchange_id), 0x05, im, b"m2", reliable, now)
            .unwrap();
        b.decode_inbound(&m1.wire_bytes, now).unwrap();
        b.decode_inbound(&m2.wire_bytes, now).unwrap();
        assert_eq!(
            (y.exchange_id, z1.exchange_id, x.exchange_id),
            (0x0100, 0x0101, 0x0200)
        );

        let events = b.flush_session_acks(sid);

        let got: Vec<(u16, MessageCounter, bool)> = events
            .iter()
            .map(|e| match e {
                MrpEvent::SendStandaloneAck {
                    session_id,
                    exchange_id,
                    packet,
                } => {
                    assert_eq!(*session_id, sid);
                    let flags = exchange_flags_of(&a, sid, packet);
                    assert!(flags.contains(ExchangeFlags::ACK), "{flags:?}");
                    let initiator = flags.contains(ExchangeFlags::INITIATOR);
                    let acked = match a.decode_inbound(packet, now).unwrap() {
                        DecodeInboundOutput::AckOnly { acked_counter, .. } => acked_counter,
                        other => panic!("expected AckOnly, got {other:?}"),
                    };
                    (*exchange_id, acked, initiator)
                }
                other => panic!("expected only standalone ACKs, got {other:?}"),
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (0x0100, y.message_counter, false),
                (0x0101, z1.message_counter, false),
                (0x0200, m1.message_counter, true),
                (0x0200, m2.message_counter, true),
            ]
        );
        assert_eq!(b.flush_session_acks(sid).len(), 0, "taken once");
        assert_eq!(b.poll_timeout(), None);
        assert_eq!(a.poll_timeout(), None, "a's retransmits are all ACKed");
    }

    /// A retransmit of a message whose ACK is still buffered gets an ACK at
    /// once from the duplicate path; the buffered one must then go, or a later
    /// finish or timer sends a second ACK for the same counter. chip clears its
    /// pending flag the same way (ReliableMessageContext.cpp:120-153, at chip
    /// master and v1.4.2.0).
    #[test]
    fn a_duplicate_does_not_earn_a_second_ack() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        let out = a
            .encode_outbound(
                sid,
                None,
                0x05,
                ProtocolId::INTERACTION_MODEL,
                b"report",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        b.decode_inbound(&out.wire_bytes, now).unwrap();
        assert!(matches!(
            b.decode_inbound(&out.wire_bytes, now).unwrap(),
            DecodeInboundOutput::DuplicateReliableAckResent { .. }
        ));
        assert_eq!(
            b.finish_exchange(sid, out.exchange_id).len(),
            0,
            "the duplicate path already ACKed it"
        );
        assert_eq!(
            b.handle_timeout(now + std::time::Duration::from_secs(1))
                .len(),
            0,
            "nor does the timer"
        );
    }

    /// A `transport_reliable` session must never set the protocol header's
    /// R (reliable) bit, even when the caller passes `mrp_flags.reliable =
    /// true` — the underlying transport (BTP) already guarantees ordered,
    /// reliable delivery, so MRP's own reliability layer must stay off.
    #[test]
    fn transport_reliable_suppresses_r_bit() {
        let (mut a, b) = paired_sessions();
        a.get_mut(SessionId(1)).unwrap().transport_reliable = true;
        let now = Instant::now();
        let out = a
            .encode_outbound(
                SessionId(1),
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"payload",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();

        let b_session = b.get(SessionId(1)).unwrap();
        let mut replay_window = ReplayWindow::new();
        let (_, plaintext) = crate::framing::decode_secured(
            &out.wire_bytes,
            &b_session.keys,
            SessionRole::Responder,
            &mut replay_window,
            0,
        )
        .unwrap();
        let (header, _) = crate::protocol_header::decode_protocol_header(&plaintext).unwrap();
        assert!(
            !header
                .exchange_flags
                .contains(crate::protocol_header::ExchangeFlags::RELIABLE),
            "transport_reliable session must suppress the R bit, got flags {:?}",
            header.exchange_flags
        );
    }

    /// A `transport_reliable` outbound must not register a retransmit entry
    /// even when `mrp_flags.reliable` is true — no pending deadline
    /// afterwards.
    #[test]
    fn transport_reliable_skips_retransmit_registration() {
        let (mut a, _b) = paired_sessions();
        a.get_mut(SessionId(1)).unwrap().transport_reliable = true;
        let now = Instant::now();
        a.encode_outbound(
            SessionId(1),
            None,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"payload",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();
        assert_eq!(
            a.poll_timeout(),
            None,
            "a transport_reliable session must not register a retransmit even when mrp_flags.reliable is set"
        );
    }

    /// Defensive check: even if a (buggy) peer sets R=1 on an inbound
    /// message, a `transport_reliable` receiving session must not arm a
    /// standalone-ack timer for it.
    #[test]
    fn transport_reliable_skips_inbound_ack_scheduling() {
        let (mut a, mut b) = paired_sessions();
        b.get_mut(SessionId(1)).unwrap().transport_reliable = true;
        let now = Instant::now();
        let out = a
            .encode_outbound(
                SessionId(1),
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"payload",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        b.decode_inbound(&out.wire_bytes, now).unwrap();
        assert_eq!(
            b.poll_timeout(),
            None,
            "a transport_reliable session must not arm a standalone-ack timer even for a reliable-flagged inbound"
        );
    }

    #[test]
    fn set_transport_reliable_unknown_session_errors() {
        let mut a = SessionManager::new();
        let err = a.set_transport_reliable(SessionId(42), true).unwrap_err();
        assert!(matches!(err, Error::UnknownSession(42)));
    }

    #[test]
    fn is_transport_reliable_reflects_flag_and_unknown_session() {
        let (mut a, _b) = paired_sessions();
        assert_eq!(a.is_transport_reliable(SessionId(1)), Some(false));
        a.set_transport_reliable(SessionId(1), true).unwrap();
        assert_eq!(a.is_transport_reliable(SessionId(1)), Some(true));
        assert_eq!(a.is_transport_reliable(SessionId(99)), None);
    }

    /// An application payload over the framing cap (1024 bytes, including
    /// the protocol header), so `encode_outbound` fails with
    /// `PayloadTooLarge`.
    const OVERSIZED: [u8; 2000] = [0u8; 2000];

    /// A send that fails with `PayloadTooLarge` leaves the exchange table as
    /// it was, whether it asked for a fresh exchange (`None`) or named one the
    /// table has never seen. It used to record the exchange before framing
    /// failed, and nothing ever removed it: more than
    /// `MAX_EXCHANGES_PER_SESSION` such failures filled the table, and the
    /// session then refused every exchange the device started
    /// (`ExchangeTableFull`), so a subscription's reports stopped arriving.
    #[test]
    fn oversized_sends_leak_no_exchange_slots() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        let reliable = MrpFlags { reliable: true };
        let im = ProtocolId::INTERACTION_MODEL;
        let tries = crate::mrp::MAX_EXCHANGES_PER_SESSION + 44;
        let before = a.get(sid).unwrap().mrp.exchange_count();

        for _ in 0..tries {
            let result = a.encode_outbound(sid, None, 0x08, im, &OVERSIZED, reliable, now);
            assert!(
                matches!(result, Err(Error::PayloadTooLarge { .. })),
                "got {:?}",
                result.map(|out| out.exchange_id)
            );
        }
        assert_eq!(
            a.get(sid).unwrap().mrp.exchange_count(),
            before,
            "a fresh exchange whose first send failed must not stay in the table"
        );

        for i in 0..tries {
            let unseen = 0x8000 + u16::try_from(i).unwrap();
            let result = a.encode_outbound(sid, Some(unseen), 0x08, im, &OVERSIZED, reliable, now);
            assert!(matches!(result, Err(Error::PayloadTooLarge { .. })));
        }
        assert_eq!(
            a.get(sid).unwrap().mrp.exchange_count(),
            before,
            "a named exchange the table had not seen must not be recorded by a failed send"
        );

        // The device can still start an exchange (a subscription report).
        let report = b
            .encode_outbound(sid, None, 0x05, im, b"report", reliable, now)
            .unwrap();
        assert!(
            matches!(
                a.decode_inbound(&report.wire_bytes, now),
                Ok(DecodeInboundOutput::AppMessage { .. })
            ),
            "a device-initiated reliable message must still be accepted"
        );
    }

    /// Every message we send on an exchange we started carries `I=1`, not
    /// only the first: a timed action after its `TimedRequest`, a chunked
    /// write's later chunks, the `StatusResponse` that asks for the next read
    /// chunk. The first send records the exchange as ours, and the next send
    /// on it reads the flag from there (the build only forces `I=1` for a
    /// freshly allocated id). A chip responder matches an inbound to its
    /// exchange on the id AND the `I` flag, so a follow-up with `I=0` would
    /// not reach the exchange the device is answering. Checked with the peer
    /// replying in between and without it.
    #[test]
    fn follow_ups_on_our_own_exchange_keep_the_initiator_flag() {
        use crate::protocol_header::ExchangeFlags;

        let sid = SessionId(1);
        let now = Instant::now();
        let im = ProtocolId::INTERACTION_MODEL;
        let reliable = MrpFlags { reliable: true };
        for peer_replies in [false, true] {
            let (mut a, mut b) = paired_sessions();
            let first = a
                .encode_outbound(sid, None, 0x0A, im, b"timed", reliable, now)
                .unwrap();
            assert!(first.is_local_initiator);
            if peer_replies {
                b.decode_inbound(&first.wire_bytes, now).unwrap();
                let reply = b
                    .encode_outbound(sid, Some(first.exchange_id), 0x01, im, b"ok", reliable, now)
                    .unwrap();
                assert!(!reply.is_local_initiator, "b is responding");
                a.decode_inbound(&reply.wire_bytes, now).unwrap();
            }

            let follow_up = a
                .encode_outbound(
                    sid,
                    Some(first.exchange_id),
                    0x08,
                    im,
                    b"action",
                    reliable,
                    now,
                )
                .unwrap();

            assert!(
                follow_up.is_local_initiator,
                "peer_replies={peer_replies}: the follow-up must report I=1"
            );
            assert!(
                exchange_flags_of(&b, sid, &follow_up.wire_bytes)
                    .contains(ExchangeFlags::INITIATOR),
                "peer_replies={peer_replies}: the follow-up's header must carry I=1"
            );
        }
    }

    /// `b` starts a reliable exchange, so `a` owes it an ACK, and `a`'s
    /// first answer fails with `PayloadTooLarge`. Returns the request.
    fn owe_an_ack_then_fail_to_answer(
        a: &mut SessionManager,
        b: &mut SessionManager,
        now: Instant,
    ) -> EncodeOutboundOutput {
        let sid = SessionId(1);
        let im = ProtocolId::INTERACTION_MODEL;
        let reliable = MrpFlags { reliable: true };
        let request = b
            .encode_outbound(sid, None, 0x02, im, b"request", reliable, now)
            .unwrap();
        a.decode_inbound(&request.wire_bytes, now).unwrap();
        let result = a.encode_outbound(
            sid,
            Some(request.exchange_id),
            0x05,
            im,
            &OVERSIZED,
            reliable,
            now,
        );
        assert!(matches!(result, Err(Error::PayloadTooLarge { .. })));
        request
    }

    /// The ACK an exchange owes survives a failed answer and rides the next
    /// one. It used to be taken for the failed message and lost, so the
    /// device kept retransmitting until the duplicate path re-ACKed it.
    /// (chip loses it too: `ExchangeMessageDispatch::SendMessage` takes the
    /// pending ACK before `PrepareMessage`'s length check, at master 5cd2917a
    /// and v1.4.2.0.)
    #[test]
    fn an_oversized_send_keeps_the_owed_ack_for_the_next_message() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        let request = owe_an_ack_then_fail_to_answer(&mut a, &mut b, now);

        let answer = a
            .encode_outbound(
                sid,
                Some(request.exchange_id),
                0x05,
                ProtocolId::INTERACTION_MODEL,
                b"answer",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        assert!(answer.piggyback_acked, "the owed ACK rides the answer");

        // `b` takes it as the ACK of its request: no retransmit is left.
        b.decode_inbound(&answer.wire_bytes, now).unwrap();
        let later = b.handle_timeout(now + std::time::Duration::from_secs(60));
        assert!(
            !later.iter().any(|e| matches!(
                e,
                MrpEvent::Retransmit { counter, .. } | MrpEvent::Expired { counter, .. }
                    if *counter == request.message_counter
            )),
            "the request was ACKed, so it is not retransmitted: {later:?}"
        );
    }

    /// The same ACK, after the failed answer, is still there for
    /// `finish_exchange` to send, and the device accepts it.
    #[test]
    fn an_oversized_send_leaves_the_owed_ack_for_the_flush() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        let request = owe_an_ack_then_fail_to_answer(&mut a, &mut b, now);

        let events = a.finish_exchange(sid, request.exchange_id);
        let [MrpEvent::SendStandaloneAck { packet, .. }] = events.as_slice() else {
            panic!("expected the owed standalone ACK, got {events:?}");
        };
        match b.decode_inbound(packet, now).unwrap() {
            DecodeInboundOutput::AckOnly { acked_counter, .. } => {
                assert_eq!(acked_counter, request.message_counter);
            }
            other => panic!("expected AckOnly, got {other:?}"),
        }
    }

    /// A failed send spends neither a message counter nor an exchange id:
    /// nothing reached the wire, so the next send uses both. Reusing a
    /// counter is forbidden only once it has been sent (its AES-CCM nonce
    /// used); the framing cap is checked before anything is encrypted.
    #[test]
    fn a_failed_send_spends_no_counter_and_no_exchange_id() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        let im = ProtocolId::INTERACTION_MODEL;
        let reliable = MrpFlags { reliable: true };
        let counter_before = a.get(sid).unwrap().outbound_counter;
        let next_id_before = a.next_exchange_id;

        let result = a.encode_outbound(sid, None, 0x08, im, &OVERSIZED, reliable, now);
        assert!(matches!(result, Err(Error::PayloadTooLarge { .. })));
        assert_eq!(a.get(sid).unwrap().outbound_counter, counter_before);
        assert_eq!(a.next_exchange_id, next_id_before);
        assert_eq!(a.poll_timeout(), None, "nothing to retransmit");

        let sent = a
            .encode_outbound(sid, None, 0x08, im, b"fits", reliable, now)
            .unwrap();
        assert_eq!(sent.message_counter, MessageCounter(counter_before));
        assert_eq!(sent.exchange_id, next_id_before);
        assert!(b.decode_inbound(&sent.wire_bytes, now).is_ok());
    }

    /// An exhausted counter (spec §4.6.2: it cannot wrap) or an unknown
    /// session fails before anything changes: no exchange id is spent and an
    /// owed ACK stays owed.
    #[test]
    fn counter_overflow_and_unknown_session_change_nothing() {
        let (mut a, _b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        let im = ProtocolId::INTERACTION_MODEL;
        let reliable = MrpFlags { reliable: true };
        buffer_reliable_inbound(&mut a, sid, 0x4242, MessageCounter(100), now);
        a.sessions.get_mut(&sid).unwrap().outbound_counter = u32::MAX;
        let table_before = a.get(sid).unwrap().mrp.exchange_count();
        let next_id_before = a.next_exchange_id;

        let fresh = a.encode_outbound(sid, None, 0x08, im, b"x", reliable, now);
        assert!(matches!(fresh, Err(Error::CounterOverflow)));
        let answer = a.encode_outbound(sid, Some(0x4242), 0x05, im, b"x", reliable, now);
        assert!(matches!(answer, Err(Error::CounterOverflow)));
        let unknown = a.encode_outbound(SessionId(99), None, 0x08, im, b"x", reliable, now);
        assert!(matches!(unknown, Err(Error::UnknownSession(99))));

        assert_eq!(a.next_exchange_id, next_id_before);
        assert_eq!(a.get(sid).unwrap().mrp.exchange_count(), table_before);
        assert_eq!(a.get(sid).unwrap().outbound_counter, u32::MAX);
        assert!(
            a.get(sid).unwrap().mrp.has_pending(),
            "the ACK owed on 0x4242 is still owed"
        );
    }

    // The duplicate-reliable re-ack decision must be made only AFTER
    // the message authenticates. An attacker who has observed a reliable
    // frame's plaintext header (session id + counter are cleartext) must not
    // be able to replay that header with any ciphertext and make us emit an
    // encrypted standalone ack + burn an outbound counter.
    #[test]
    fn forged_replay_emits_no_ack_and_burns_no_counter() {
        let (mut a, mut b) = paired_sessions();
        let now = Instant::now();
        // a -> b, RELIABLE: b records the counter and owes a reliable ack.
        let out = a
            .encode_outbound(
                SessionId(1),
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"payload",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        let _ = b.decode_inbound(&out.wire_bytes, now).unwrap();

        let before = b.sessions[&SessionId(1)].outbound_counter;

        // Forged: same header (valid session id + already-used reliable
        // counter) but a corrupted AEAD tag — never authenticates.
        let mut forged = out.wire_bytes.clone();
        let n = forged.len();
        forged[n - 1] ^= 0xFF;
        forged[n - 2] ^= 0xFF;

        let result = b.decode_inbound(&forged, now);
        assert!(
            !matches!(
                result,
                Ok(DecodeInboundOutput::DuplicateReliableAckResent { .. })
            ),
            "an unauthenticated replay must not emit a standalone ack; got {result:?}"
        );
        assert!(
            matches!(result, Err(Error::DecryptionFailed)),
            "forged ciphertext must fail authentication; got {result:?}"
        );
        assert_eq!(
            before,
            b.sessions[&SessionId(1)].outbound_counter,
            "no outbound counter may be burned by an unauthenticated replay"
        );
    }

    // Regression guard for the authenticate-before-replay-check fix: a
    // genuine, AUTHENTICATED reliable duplicate (a lost-ack retransmit) must
    // still be re-acked — the fix must not break MRP's legitimate duplicate
    // handling.
    #[test]
    fn authentic_reliable_duplicate_is_still_reacked() {
        let (mut a, mut b) = paired_sessions();
        let now = Instant::now();
        let out = a
            .encode_outbound(
                SessionId(1),
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"payload",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        let first = b.decode_inbound(&out.wire_bytes, now).unwrap();
        assert!(matches!(first, DecodeInboundOutput::AppMessage { .. }));
        // Authentic retransmit of the exact same (authenticated) bytes.
        let second = b.decode_inbound(&out.wire_bytes, now).unwrap();
        assert!(
            matches!(
                second,
                DecodeInboundOutput::DuplicateReliableAckResent { .. }
            ),
            "an authenticated reliable duplicate must be re-acked; got {second:?}"
        );
    }

    // --- Every authenticated reliable duplicate is ACKed (chip) ---
    //
    // chip classifies max, seen-in-window and before-window counters alike as
    // duplicates (SessionManager.cpp:963-1005), and the exchange layer ACKs any
    // duplicate with R set on an MRP session, on its own exchange or on an
    // ephemeral one with role !I (ExchangeMgr.cpp:415-446), without processing
    // its piggybacked ACK or delivering it (ExchangeContext.cpp:543/572;
    // ReliableMessageContext.cpp:116-153).

    /// The protocol header and acked counter of a standalone ACK `packet`
    /// that `b` built, as `a` decodes it.
    fn ack_seen_by(
        a: &mut SessionManager,
        packet: &[u8],
        now: Instant,
    ) -> (crate::protocol_header::ProtocolHeader, MessageCounter) {
        let header = protocol_header_of(a, SessionId(1), packet);
        let acked = match a.decode_inbound(packet, now).unwrap() {
            DecodeInboundOutput::AckOnly { acked_counter, .. } => acked_counter,
            other => panic!("expected AckOnly, got {other:?}"),
        };
        (header, acked)
    }

    /// A reliable message that arrives again after the window has moved
    /// more than 32 past it (`CounterTooOld`) is still acknowledged, once, on its
    /// own exchange with the opposite role. Before the fix only a counter
    /// still in the window and in MRP's recent cache got an ACK.
    #[test]
    fn a_too_old_reliable_duplicate_is_acked_once() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        let m = a
            .encode_outbound(
                sid,
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"req",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        assert!(matches!(
            b.decode_inbound(&m.wire_bytes, now).unwrap(),
            DecodeInboundOutput::AppMessage { .. }
        ));
        // Move b's window 40 past m.
        a.get_mut(sid).unwrap().outbound_counter = m.message_counter.0 + 40;
        let jump = a
            .encode_outbound(
                sid,
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"later",
                MrpFlags::default(),
                now,
            )
            .unwrap();
        b.decode_inbound(&jump.wire_bytes, now).unwrap();

        let ack_packet = match b.decode_inbound(&m.wire_bytes, now).unwrap() {
            DecodeInboundOutput::DuplicateReliableAckResent {
                exchange_id,
                ack_packet,
                ..
            } => {
                assert_eq!(exchange_id, m.exchange_id);
                ack_packet
            }
            other => panic!("expected DuplicateReliableAckResent, got {other:?}"),
        };
        let (header, acked) = ack_seen_by(&mut a, &ack_packet, now);
        assert_eq!(acked, m.message_counter);
        assert_eq!(header.exchange_id, m.exchange_id);
        assert!(
            !header
                .exchange_flags
                .contains(crate::protocol_header::ExchangeFlags::INITIATOR),
            "m had I=1, so its ACK has I=0"
        );
        // One ACK only: the buffered one for m went with it.
        let later = b.handle_timeout(now + std::time::Duration::from_secs(1));
        assert!(standalone_acks(&later).is_empty(), "{later:?}");
    }

    /// A reliable message with counter 0 is a duplicate (never delivered),
    /// but it is acknowledged, as chip acknowledges it; counter 1 is still
    /// accepted after.
    #[test]
    fn a_counter_zero_reliable_message_is_acked_not_delivered() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        a.get_mut(sid).unwrap().outbound_counter = 0;
        let zero = a
            .encode_outbound(
                sid,
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"zero",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        let ack_packet = match b.decode_inbound(&zero.wire_bytes, now).unwrap() {
            DecodeInboundOutput::DuplicateReliableAckResent { ack_packet, .. } => ack_packet,
            other => panic!("expected DuplicateReliableAckResent, got {other:?}"),
        };
        let (_, acked) = ack_seen_by(&mut a, &ack_packet, now);
        assert_eq!(acked, MessageCounter(0));

        let one = a
            .encode_outbound(
                sid,
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"one",
                MrpFlags::default(),
                now,
            )
            .unwrap();
        assert_eq!(one.message_counter, MessageCounter(1));
        assert!(matches!(
            b.decode_inbound(&one.wire_bytes, now).unwrap(),
            DecodeInboundOutput::AppMessage { .. }
        ));
    }

    /// A duplicate without R gets no ACK: the replay error is returned and
    /// no outbound counter is spent.
    #[test]
    fn an_unreliable_duplicate_is_not_acked() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        let m = a
            .encode_outbound(
                sid,
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"x",
                MrpFlags::default(),
                now,
            )
            .unwrap();
        b.decode_inbound(&m.wire_bytes, now).unwrap();
        let before = b.get(sid).unwrap().outbound_counter;
        assert!(matches!(
            b.decode_inbound(&m.wire_bytes, now),
            Err(Error::ReplayedCounter { .. })
        ));
        assert_eq!(b.get(sid).unwrap().outbound_counter, before);
    }

    /// A `transport_reliable` (BTP) session never ACKs, a duplicate
    /// included: chip's `AllowsMRP()` is UDP-only. Before the fix a
    /// duplicate still in MRP's recent cache got an ACK here.
    #[test]
    fn a_transport_reliable_session_does_not_ack_a_duplicate() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        b.get_mut(sid).unwrap().transport_reliable = true;
        let now = Instant::now();
        let m = a
            .encode_outbound(
                sid,
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"x",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        b.decode_inbound(&m.wire_bytes, now).unwrap();
        let before = b.get(sid).unwrap().outbound_counter;
        let result = b.decode_inbound(&m.wire_bytes, now);
        assert!(
            matches!(result, Err(Error::ReplayedCounter { .. })),
            "{result:?}"
        );
        assert_eq!(b.get(sid).unwrap().outbound_counter, before);
    }

    /// A duplicate's piggybacked ACK is not processed (chip
    /// `ExchangeContext::HandleMessage` skips `HandleRcvdAck` for a
    /// duplicate): our retransmit stays pending. The duplicate here is a
    /// counter-0 reply that carries an ACK for our reliable request.
    #[test]
    fn a_duplicates_piggybacked_ack_is_ignored() {
        let (mut a, mut b) = paired_sessions();
        let sid = SessionId(1);
        let now = Instant::now();
        let y = b
            .encode_outbound(
                sid,
                None,
                0x08,
                ProtocolId::INTERACTION_MODEL,
                b"y",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        a.decode_inbound(&y.wire_bytes, now).unwrap();
        a.get_mut(sid).unwrap().outbound_counter = 0;
        let reply = a
            .encode_outbound(
                sid,
                Some(y.exchange_id),
                0x09,
                ProtocolId::INTERACTION_MODEL,
                b"reply",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        assert!(reply.piggyback_acked, "the reply carries the ACK for y");
        assert!(matches!(
            b.decode_inbound(&reply.wire_bytes, now).unwrap(),
            DecodeInboundOutput::DuplicateReliableAckResent { .. }
        ));
        let later = b.handle_timeout(now + std::time::Duration::from_secs(30));
        assert!(
            later.iter().any(|e| matches!(
                e,
                MrpEvent::Retransmit { counter, .. } if *counter == y.message_counter
            )),
            "y's retransmit must still be pending; got {later:?}"
        );
    }

    // --- Exchange identity: (exchange id, role), Matter 1.4 Core §4.10.2-3 ---
    //
    // An exchange we start and one the peer starts may carry the same id: each
    // side allocates from its own counter. These tests force that collision
    // and check that neither exchange takes the other's owed ACK or I flag.

    /// The full protocol header of `packet`, decrypted with `receiver`'s keys
    /// for session `sid`.
    fn protocol_header_of(
        receiver: &SessionManager,
        sid: SessionId,
        packet: &[u8],
    ) -> crate::protocol_header::ProtocolHeader {
        let session = receiver.get(sid).unwrap();
        let (_, plaintext) = crate::framing::decode_secured(
            packet,
            &session.keys,
            session.role,
            &mut crate::framing::ReplayWindow::new(),
            session.peer_nonce_node_id,
        )
        .unwrap();
        crate::protocol_header::decode_protocol_header(&plaintext)
            .unwrap()
            .0
    }

    /// The standalone ACK packets among `events`.
    fn standalone_acks(events: &[MrpEvent]) -> Vec<&[u8]> {
        events
            .iter()
            .filter_map(|e| match e {
                MrpEvent::SendStandaloneAck { packet, .. } => Some(packet.as_slice()),
                _ => None,
            })
            .collect()
    }

    /// Assert that `a` sends exactly one standalone ACK when the standalone-ACK
    /// deadline passes, and that it acknowledges `request` on the PEER's
    /// exchange: same id, `I=0`, `ack_counter` = the request's counter.
    fn assert_owed_ack_goes_to_the_peers_exchange(
        a: &mut SessionManager,
        b: &SessionManager,
        request: &EncodeOutboundOutput,
        now: Instant,
    ) {
        let sid = SessionId(1);
        let deadline = a.mrp_config(sid).unwrap().standalone_ack_deadline;
        let events = a.handle_timeout(now + deadline);
        let acks = standalone_acks(&events);
        assert_eq!(
            acks.len(),
            1,
            "the peer's exchange is still owed its ACK: {events:?}"
        );
        let header = protocol_header_of(b, sid, acks[0]);
        assert_eq!(header.exchange_id, request.exchange_id);
        assert!(
            !header
                .exchange_flags
                .contains(crate::protocol_header::ExchangeFlags::INITIATOR),
            "an ACK on the peer's exchange must carry I=0"
        );
        assert_eq!(header.ack_counter, Some(request.message_counter));
    }

    /// The peer starts exchange X and we owe it an ACK; our next new exchange
    /// is then allocated the same id X. The new message must not carry (and
    /// so drain) the ACK owed on the peer's exchange: it goes out with `I=1`,
    /// so a chip peer matches it to a different exchange
    /// (`ExchangeContext::MatchExchange`) and never sees the ACK.
    #[test]
    fn a_new_local_exchange_does_not_take_a_peer_exchanges_owed_ack() {
        let sid = SessionId(1);
        let now = Instant::now();
        let im = ProtocolId::INTERACTION_MODEL;
        let reliable = MrpFlags { reliable: true };
        let (mut a, mut b) = paired_sessions();
        let request = b
            .encode_outbound(sid, None, 0x02, im, b"request", reliable, now)
            .unwrap();
        a.decode_inbound(&request.wire_bytes, now).unwrap();

        a.next_exchange_id = request.exchange_id;
        let ours = a
            .encode_outbound(sid, None, 0x02, im, b"ours", reliable, now)
            .unwrap();
        assert_eq!(ours.exchange_id, request.exchange_id, "the ids collide");
        assert!(ours.is_local_initiator);
        assert!(
            !ours.piggyback_acked,
            "our new exchange took the ACK owed on the peer's exchange"
        );
        let header = protocol_header_of(&b, sid, &ours.wire_bytes);
        assert!(header
            .exchange_flags
            .contains(crate::protocol_header::ExchangeFlags::INITIATOR));
        assert_eq!(header.ack_counter, None);

        assert_owed_ack_goes_to_the_peers_exchange(&mut a, &b, &request, now);
    }

    /// The other order: we start exchange X, then the peer starts its own
    /// exchange that also has id X. A follow-up on OUR exchange must not
    /// carry the ACK owed on the peer's.
    #[test]
    fn a_peer_exchange_reusing_our_id_keeps_its_ack_off_our_exchange() {
        let sid = SessionId(1);
        let now = Instant::now();
        let im = ProtocolId::INTERACTION_MODEL;
        let reliable = MrpFlags { reliable: true };
        let (mut a, mut b) = paired_sessions();
        let ours = a
            .encode_outbound(sid, None, 0x0A, im, b"timed", reliable, now)
            .unwrap();

        b.next_exchange_id = ours.exchange_id;
        let request = b
            .encode_outbound(sid, None, 0x02, im, b"request", reliable, now)
            .unwrap();
        assert_eq!(request.exchange_id, ours.exchange_id, "the ids collide");
        match a.decode_inbound(&request.wire_bytes, now).unwrap() {
            DecodeInboundOutput::AppMessage { is_initiator, .. } => assert!(is_initiator),
            other => panic!("expected the peer's request, got {other:?}"),
        }

        let follow_up = a
            .encode_outbound(
                sid,
                Some(ours.exchange_id),
                0x08,
                im,
                b"action",
                reliable,
                now,
            )
            .unwrap();
        assert!(follow_up.is_local_initiator);
        assert!(
            !follow_up.piggyback_acked,
            "our follow-up took the ACK owed on the peer's exchange"
        );
        let header = protocol_header_of(&b, sid, &follow_up.wire_bytes);
        assert!(header
            .exchange_flags
            .contains(crate::protocol_header::ExchangeFlags::INITIATOR));
        assert_eq!(header.ack_counter, None);

        assert_owed_ack_goes_to_the_peers_exchange(&mut a, &b, &request, now);
    }

    /// Make `a` hold two exchanges with one id: one it started (`ours`, which
    /// `b` has answered reliably, so `a` owes `b` an ACK there) and one `b`
    /// started with the same id (`request`, also owed an ACK). Returns
    /// `(ours, b's answer on ours, request)`.
    fn collide(
        a: &mut SessionManager,
        b: &mut SessionManager,
        now: Instant,
    ) -> (
        EncodeOutboundOutput,
        EncodeOutboundOutput,
        EncodeOutboundOutput,
    ) {
        let sid = SessionId(1);
        let im = ProtocolId::INTERACTION_MODEL;
        let reliable = MrpFlags { reliable: true };
        let ours = a
            .encode_outbound(sid, None, 0x02, im, b"ours", reliable, now)
            .unwrap();
        b.decode_inbound(&ours.wire_bytes, now).unwrap();
        let answer = b
            .encode_outbound_with_role(
                sid,
                ours.exchange_id,
                false,
                0x05,
                im,
                b"answer",
                reliable,
                now,
            )
            .unwrap();
        a.decode_inbound(&answer.wire_bytes, now).unwrap();

        b.next_exchange_id = ours.exchange_id;
        let request = b
            .encode_outbound(sid, None, 0x02, im, b"request", reliable, now)
            .unwrap();
        assert_eq!(request.exchange_id, ours.exchange_id, "the ids collide");
        a.decode_inbound(&request.wire_bytes, now).unwrap();
        assert_eq!(a.get(sid).unwrap().mrp.exchange_count(), 2);
        (ours, answer, request)
    }

    /// With both exchanges live, the role-aware send reaches each one: our
    /// follow-up carries `I=1` and the ACK for `b`'s answer; our answer to
    /// `b`'s request carries `I=0` and that request's ACK. `b` matches each to
    /// the exchange it belongs to.
    #[test]
    fn encode_outbound_with_role_reaches_either_exchange_after_a_collision() {
        let sid = SessionId(1);
        let now = Instant::now();
        let im = ProtocolId::INTERACTION_MODEL;
        let reliable = MrpFlags { reliable: true };
        let (mut a, mut b) = paired_sessions();
        let (ours, answer, request) = collide(&mut a, &mut b, now);
        let id = ours.exchange_id;

        let reply = a
            .encode_outbound_with_role(sid, id, false, 0x05, im, b"reply", reliable, now)
            .unwrap();
        assert!(!reply.is_local_initiator);
        assert!(reply.piggyback_acked);
        let header = protocol_header_of(&b, sid, &reply.wire_bytes);
        assert!(!header
            .exchange_flags
            .contains(crate::protocol_header::ExchangeFlags::INITIATOR));
        assert_eq!(header.ack_counter, Some(request.message_counter));
        match b.decode_inbound(&reply.wire_bytes, now).unwrap() {
            DecodeInboundOutput::AppMessage { is_initiator, .. } => {
                assert!(
                    !is_initiator,
                    "b must see the reply on the exchange it started"
                );
            }
            other => panic!("expected the reply, got {other:?}"),
        }

        let follow_up = a
            .encode_outbound_with_role(sid, id, true, 0x01, im, b"next", reliable, now)
            .unwrap();
        assert!(follow_up.is_local_initiator);
        assert!(follow_up.piggyback_acked);
        let header = protocol_header_of(&b, sid, &follow_up.wire_bytes);
        assert!(header
            .exchange_flags
            .contains(crate::protocol_header::ExchangeFlags::INITIATOR));
        assert_eq!(header.ack_counter, Some(answer.message_counter));
        match b.decode_inbound(&follow_up.wire_bytes, now).unwrap() {
            DecodeInboundOutput::AppMessage { is_initiator, .. } => {
                assert!(
                    is_initiator,
                    "b must see the follow-up on the exchange a started"
                );
            }
            other => panic!("expected the follow-up, got {other:?}"),
        }
    }

    /// `finish_exchange_with_role` flushes and reclaims only the exchange it
    /// names, in either order; the legacy `finish_exchange(id)` means ours
    /// while ours is live.
    #[test]
    fn finish_exchange_with_role_flushes_only_the_named_exchange() {
        use crate::protocol_header::ExchangeFlags;

        let sid = SessionId(1);
        let now = Instant::now();
        // (use the legacy call for the first finish, finish ours first)
        for (legacy, ours_first) in [(true, true), (false, true), (false, false)] {
            let (mut a, mut b) = paired_sessions();
            let (ours, answer, request) = collide(&mut a, &mut b, now);
            let id = ours.exchange_id;
            let case = format!("legacy={legacy} ours_first={ours_first}");

            for (step, local) in [ours_first, !ours_first].into_iter().enumerate() {
                let events = if legacy && step == 0 {
                    a.finish_exchange(sid, id)
                } else {
                    a.finish_exchange_with_role(sid, id, local)
                };
                let acks = standalone_acks(&events);
                assert_eq!(acks.len(), 1, "{case} step {step}");
                let header = protocol_header_of(&b, sid, acks[0]);
                assert_eq!(
                    header.exchange_flags.contains(ExchangeFlags::INITIATOR),
                    local,
                    "{case} step {step}"
                );
                let owed = if local { &answer } else { &request };
                assert_eq!(
                    header.ack_counter,
                    Some(owed.message_counter),
                    "{case} step {step}"
                );
                assert_eq!(
                    a.get(sid).unwrap().mrp.exchange_count(),
                    1 - step,
                    "{case} step {step}: only the finished exchange is reclaimed"
                );
            }
        }
    }

    /// `abort_exchange_with_role` drops only the named exchange's
    /// retransmits: aborting ours leaves our reliable answer on the peer's
    /// exchange retransmitting, and the other way round.
    #[test]
    fn abort_exchange_with_role_keeps_the_other_exchanges_retransmit() {
        let sid = SessionId(1);
        let now = Instant::now();
        let im = ProtocolId::INTERACTION_MODEL;
        let reliable = MrpFlags { reliable: true };
        for abort_ours in [true, false] {
            let (mut a, mut b) = paired_sessions();
            let (ours, _answer, _request) = collide(&mut a, &mut b, now);
            let id = ours.exchange_id;
            // Our two reliable messages in flight, one on each exchange.
            let on_ours = a
                .encode_outbound_with_role(sid, id, true, 0x01, im, b"next", reliable, now)
                .unwrap();
            let on_peers = a
                .encode_outbound_with_role(sid, id, false, 0x05, im, b"reply", reliable, now)
                .unwrap();

            // The flushed ACKs are not under test here.
            let _ = a.abort_exchange_with_role(sid, id, abort_ours);
            let kept = if abort_ours { &on_peers } else { &on_ours };
            let retransmits: Vec<MessageCounter> = a
                .handle_timeout(now + std::time::Duration::from_secs(10))
                .into_iter()
                .filter_map(|e| match e {
                    MrpEvent::Retransmit { counter, .. } => Some(counter),
                    _ => None,
                })
                .collect();
            assert_eq!(
                retransmits,
                vec![kept.message_counter],
                "abort_ours={abort_ours}: only the other exchange's retransmit survives"
            );
        }
    }

    /// The role-aware send keeps `I=1` on one of our exchanges even after it
    /// has been reclaimed, where `encode_outbound(Some(id))` falls back to a
    /// response (`I=0`), as documented on [`SessionManager::finish_exchange`].
    #[test]
    fn encode_outbound_with_role_keeps_i_on_a_reclaimed_exchange() {
        let sid = SessionId(1);
        let now = Instant::now();
        let im = ProtocolId::INTERACTION_MODEL;
        let (mut a, b) = paired_sessions();
        let first = a
            .encode_outbound(sid, None, 0x02, im, b"first", MrpFlags::default(), now)
            .unwrap();
        assert_eq!(
            a.get(sid).unwrap().mrp.exchange_count(),
            0,
            "an unreliable send reclaims at once"
        );
        let again = a
            .encode_outbound_with_role(
                sid,
                first.exchange_id,
                true,
                0x01,
                im,
                b"again",
                MrpFlags::default(),
                now,
            )
            .unwrap();
        assert!(again.is_local_initiator);
        assert!(protocol_header_of(&b, sid, &again.wire_bytes)
            .exchange_flags
            .contains(crate::protocol_header::ExchangeFlags::INITIATOR));
    }

    #[test]
    fn session_table_is_bounded_and_evicts_oldest() {
        // DoS defense: the table never exceeds its cap; a new registration into
        // a full table evicts the OLDEST session, keeping the newest.
        let keys = PaseSessionKeys {
            ke: [0u8; 16],
            i2r_key: [1u8; 16],
            r2i_key: [2u8; 16],
            attestation_key: [3u8; 16],
        };
        let mut m = SessionManager::new();
        m.set_max_sessions(3);
        let id1 = m.register_pase(keys.clone(), SessionRole::Initiator, 1, PeerHint::default());
        let id2 = m.register_pase(keys.clone(), SessionRole::Initiator, 1, PeerHint::default());
        let id3 = m.register_pase(keys.clone(), SessionRole::Initiator, 1, PeerHint::default());
        assert_eq!(m.session_count(), 3);

        // A 4th registration evicts the oldest (id1) and stays capped.
        let id4 = m.register_pase(keys, SessionRole::Initiator, 1, PeerHint::default());
        assert_eq!(m.session_count(), 3, "table stays capped at 3");
        assert!(m.get(id1).is_none(), "oldest session (id1) was evicted");
        assert!(m.get(id2).is_some());
        assert!(m.get(id3).is_some());
        assert!(m.get(id4).is_some());
    }

    #[test]
    fn session_eviction_prefers_idle_over_busy() {
        use crate::mrp::MrpFlags;
        use crate::protocol_header::ProtocolId;

        // idle-first: a session mid-exchange (in-flight reliable work) is spared
        // in favor of a truly-idle one, even though the busy session is OLDER.
        let keys = PaseSessionKeys {
            ke: [0u8; 16],
            i2r_key: [1u8; 16],
            r2i_key: [2u8; 16],
            attestation_key: [3u8; 16],
        };
        let mut m = SessionManager::new();
        m.set_max_sessions(2);
        let busy = m.register_pase(keys.clone(), SessionRole::Initiator, 1, PeerHint::default());
        let idle = m.register_pase(keys.clone(), SessionRole::Initiator, 1, PeerHint::default());

        // Give the OLDER session in-flight reliable work (a pending retransmit).
        let now = std::time::Instant::now();
        let _ = m
            .encode_outbound(
                busy,
                None,
                0x02,
                ProtocolId::INTERACTION_MODEL,
                b"x",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();

        // A 3rd registration into the full (cap 2) table must evict the IDLE
        // session, NOT the older busy one mid-exchange.
        let fresh = m.register_pase(keys, SessionRole::Initiator, 1, PeerHint::default());
        assert_eq!(m.session_count(), 2, "table stays capped at 2");
        assert!(
            m.get(busy).is_some(),
            "the busy (mid-exchange) session is spared despite being older",
        );
        assert!(m.get(idle).is_none(), "the idle session is evicted first");
        assert!(m.get(fresh).is_some());
    }

    // ---- Session keys stay out of `Debug` --------------------------------
    //
    // `SessionKeys` holds the session's AES-CCM keys and attestation
    // challenge, and `Session` (which derives `Debug`) prints it, so a
    // `{:?}` log of either must show `<redacted>` and none of the bytes. No
    // assertion below prints a rendering or a key on failure: a failing
    // redaction would print the very bytes it failed to hide.

    use crate::debug_redaction_tests::{assert_hidden, renderings, synthetic_bytes};

    /// Sixteen distinct bytes starting at `first`, so each key's decimal and
    /// hex renderings are unique within a `Session`'s `Debug` output.
    fn synthetic_key(first: u8) -> [u8; 16] {
        synthetic_bytes(first)
    }

    /// Assert that `shown` carries `"<redacted>"` and none of `key`'s
    /// renderings (see `debug_redaction_tests::leak`: byte lists in either
    /// layout, hex runs, and lone bytes on their own `{:#?}` line). Never
    /// prints `shown` or `key`.
    fn assert_key_absent(shown: &str, key: &[u8]) {
        assert!(
            shown.contains("\"<redacted>\""),
            "the placeholder is missing"
        );
        assert_hidden(shown, key);
    }

    /// Every `Debug` flavour of `value` (plain, pretty and the four hex
    /// flag combinations) hides all three keys.
    fn assert_keys_absent<T: core::fmt::Debug>(value: &T, keys: &[[u8; 16]; 3]) {
        for shown in renderings(value) {
            for key in keys {
                assert_key_absent(&shown, key);
            }
        }
    }

    #[test]
    fn session_keys_debug_redacts_every_key() {
        let keys = [
            synthetic_key(0xC0),
            synthetic_key(0xD0),
            synthetic_key(0xE0),
        ];
        let session_keys = SessionKeys {
            i2r_key: keys[0],
            r2i_key: keys[1],
            attestation_key: keys[2],
        };

        // Same struct name, field names and order as the old derive.
        assert!(
            format!("{session_keys:?}")
                == "SessionKeys { i2r_key: \"<redacted>\", r2i_key: \"<redacted>\", \
                    attestation_key: \"<redacted>\" }",
            "unexpected SessionKeys rendering"
        );
        assert_keys_absent(&session_keys, &keys);
        // Only the rendering is hidden; the keys themselves are untouched.
        assert!(
            session_keys.i2r_key == keys[0]
                && session_keys.r2i_key == keys[1]
                && session_keys.attestation_key == keys[2],
            "a session key was altered"
        );
    }

    #[test]
    fn session_debug_does_not_leak_its_keys() {
        // `Session` derives `Debug` and prints its `keys` field, so the leak
        // reached every `{:?}` of a live session, not just of `SessionKeys`.
        let keys = [
            synthetic_key(0xC0),
            synthetic_key(0xD0),
            synthetic_key(0xE0),
        ];
        let mut mgr = SessionManager::new();
        let id = mgr.register_pase(
            PaseSessionKeys {
                ke: [0u8; 16],
                i2r_key: keys[0],
                r2i_key: keys[1],
                attestation_key: keys[2],
            },
            SessionRole::Initiator,
            1,
            PeerHint::default(),
        );
        let session = mgr.get(id).expect("just registered");

        assert!(
            format!("{session:?}").contains(
                "keys: SessionKeys { i2r_key: \"<redacted>\", \
                 r2i_key: \"<redacted>\", attestation_key: \"<redacted>\" }"
            ),
            "Session does not print its keys through the redacting Debug"
        );
        assert_keys_absent(session, &keys);

        // The manager derives `Debug` and prints its whole session table, so
        // a `{:?}` of the manager reached the same keys.
        assert!(
            format!("{mgr:?}").contains(
                "keys: SessionKeys { i2r_key: \"<redacted>\", \
                 r2i_key: \"<redacted>\", attestation_key: \"<redacted>\" }"
            ),
            "SessionManager does not print its sessions through the redacting Debug"
        );
        assert_keys_absent(&mgr, &keys);
    }
}
