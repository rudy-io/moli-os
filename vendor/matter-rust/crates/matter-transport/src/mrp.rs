//! Matter Message Reliability Protocol (MRP) — Matter Core Spec §4.12
//! (Matter 1.3 renumbered chapter 4; this was §4.11 in 1.0-1.2).
//!
//! Per-session, sans-IO state machine that owns pending retransmits,
//! piggyback ack queues, exchange tracking, and a recent-reliable
//! dedup-resend cache. Never touches crypto. Never builds wire bytes for
//! standalone acks (`SessionManager` does that via
//! [`crate::protocol_header::build_standalone_ack_header`] +
//! [`crate::framing::encode_secured`]).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::framing::{MessageCounter, SessionId};
use crate::protocol_header::{encode_protocol_header, ExchangeFlags, ProtocolHeader, ProtocolId};

/// Upper bound on the number of live exchanges tracked per session.
///
/// `exchange_id` is a peer-controlled 16-bit field, so without a bound a
/// long-lived session could accumulate one table entry per distinct id
/// (up to 65 536) and never reclaim them — a slow memory-exhaustion denial
/// of service. An
/// exchange is reclaimed automatically once it goes idle (no pending
/// retransmit and no ACK it still owes), so this cap only ever bites a
/// peer that holds an abnormal number of exchanges open simultaneously.
///
/// 256 sits far above any realistic concurrent-exchange count for a
/// controller (a handful of in-flight reads/subscriptions per session) while
/// still tightly bounding worst-case memory. The C++ reference
/// (`connectedhomeip`) likewise services exchanges from a small fixed pool.
pub const MAX_EXCHANGES_PER_SESSION: usize = 256;

/// Most ACKs held for reliable messages a newer reliable message on the same
/// exchange displaced. A conformant peer never has two unacknowledged
/// reliable messages on one exchange, so this only bounds a misbehaving one;
/// past it the displaced ACK is dropped, and the duplicate path in
/// [`SessionManager::decode_inbound`](crate::SessionManager::decode_inbound)
/// ACKs the peer's retransmit of that message instead.
const MAX_DISPLACED_ACKS: usize = 32;

/// Spec §4.12.8. Growth factor applied per retransmit beyond the threshold.
pub const MRP_BACKOFF_BASE: f32 = 1.6;
/// Spec §4.12.8 `MRP_BACKOFF_MARGIN` = 1.1, applied to the peer's advertised
/// interval to cover its own processing and radio scheduling slack.
///
/// Held as chip's exact integer ratio (`ReliableMessageMgr.cpp:275-276`) rather
/// than a float `1.1`: the two agree at a 500 ms base but diverge above it — at
/// `SII` = 2000 ms a float gives 2200 ms where chip gives 2201 — and this
/// library cross-verifies against chip.
const MRP_BACKOFF_MARGIN_NUMERATOR: u64 = 1127;
const MRP_BACKOFF_MARGIN_DENOMINATOR: u64 = 1024;
/// Spec §4.12.8. Transmissions before backoff growth starts, so the first
/// **two** transmissions share the base interval.
pub const MRP_BACKOFF_THRESHOLD: u8 = 1;
/// Spec §4.12.8. Maximum proportional jitter added to a retransmit delay.
/// Only ever lengthens a wait; it exists so that N controllers retransmitting
/// to the same peer do not do so in lockstep.
pub const MRP_BACKOFF_JITTER: f32 = 0.25;
/// Spec §4.12.8. Total transmissions of a reliable message, the original
/// included — so five means one send and four retransmits.
pub const MRP_MAX_TRANSMISSIONS: u8 = 5;
/// chip clamps the backoff exponent at 4 (`ReliableMessageMgr.cpp:298-299`).
/// Inert at five transmissions; present so raising [`MRP_MAX_TRANSMISSIONS`]
/// cannot silently diverge from the reference implementation.
const MRP_BACKOFF_MAX_EXPONENT: u8 = 4;

/// Configuration knobs for MRP retransmit + ack timing. Defaults are the
/// Matter 1.4 §4.12.8 MRP parameters and §4.13.1 session intervals.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct MrpConfig {
    /// Base retransmit delay used when the session is "active" (recent
    /// outbound traffic).
    pub initial_active: Duration,
    /// Base retransmit delay used when the session has been idle longer
    /// than `idle_threshold`.
    pub initial_idle: Duration,
    /// Multiplicative growth factor applied per retransmit beyond
    /// [`MRP_BACKOFF_THRESHOLD`] ([`MRP_BACKOFF_BASE`]).
    pub backoff_factor: f32,
    /// Total transmissions of a reliable message before it is declared
    /// expired, **the original send included** ([`MRP_MAX_TRANSMISSIONS`]).
    ///
    /// Renamed from `max_attempts`, which read as "retransmits" and was used as
    /// such: the message was sent once and then retransmitted `max_attempts`
    /// times, i.e. six transmissions where the spec allows five.
    pub max_transmissions: u8,
    /// Proportional jitter added to each retransmit delay
    /// ([`MRP_BACKOFF_JITTER`]). Set to `0.0` for deterministic timing in
    /// tests.
    pub backoff_jitter: f32,
    /// Maximum time a piggyback opportunity is buffered before a
    /// standalone-ack must be emitted (Matter Core Spec §4.12.8
    /// `MRP_STANDALONE_ACK_TIMEOUT`).
    pub standalone_ack_deadline: Duration,
    /// Gap-since-`last_outbound` above which the next outbound uses the
    /// idle base instead of the active base.
    pub idle_threshold: Duration,
}

impl Default for MrpConfig {
    /// The Matter 1.4 §4.13.1 session intervals and §4.12.8 MRP parameters —
    /// what chip's `GetDefaultMRPConfig()` calls out as *"defined by spec, and
    /// shall be same for all implementations"*.
    ///
    /// Identical by construction to `for_peer(None, None, None)`, which
    /// `default_matches_spec_defaults` pins. Written as literals rather than
    /// delegating, because `for_peer` starts from `Self::default()` and
    /// delegating would recurse.
    ///
    /// `initial_idle` was 4200 ms and `idle_threshold` 5 s. Neither came from
    /// the specification: `4200` appears in no version of it, in no part of
    /// connectedhomeip, and in no part of matter.js. It originated in this
    /// project during M5 as a derivation, `300 × 14`, and the
    /// correct values arrived later with `for_peer` without anyone reconciling
    /// the two — leaving the same file asserting 4200 ms twelve lines above
    /// `SPEC_DEFAULT_IDLE: 500 ms`.
    fn default() -> Self {
        Self {
            initial_active: SPEC_DEFAULT_ACTIVE,
            initial_idle: SPEC_DEFAULT_IDLE,
            idle_threshold: SPEC_DEFAULT_ACTIVE_THRESHOLD,
            backoff_factor: MRP_BACKOFF_BASE,
            max_transmissions: MRP_MAX_TRANSMISSIONS,
            backoff_jitter: MRP_BACKOFF_JITTER,
            standalone_ack_deadline: Duration::from_millis(200),
        }
    }
}

/// Matter spec-default MRP parameters used for a peer that advertises no value
/// for a given key (chip `ReliableMessageProtocolConfig` non-Linux defaults):
/// SII (idle) = 500 ms, SAI (active) = 300 ms, SAT (active threshold) = 4000 ms.
const SPEC_DEFAULT_IDLE: Duration = Duration::from_millis(500);
const SPEC_DEFAULT_ACTIVE: Duration = Duration::from_millis(300);
const SPEC_DEFAULT_ACTIVE_THRESHOLD: Duration = Duration::from_secs(4);
/// Spec upper bounds: SII/SAI are 3-byte-ish intervals (Matter caps them at
/// 3600000 ms = 1 h); SAT is a `uint16` ms (≤ 65535 ms). A peer advertising an
/// out-of-range value is clamped rather than trusted or rejected.
const MAX_RETRANS_INTERVAL: Duration = Duration::from_secs(3600);
const MAX_ACTIVE_THRESHOLD: Duration = Duration::from_millis(65_535);

impl MrpConfig {
    /// Build a per-session config from a peer's advertised MRP parameters
    /// (mDNS TXT `SII`/`SAI`/`SAT`, Matter Core Spec §4.3.1.8). Any field the
    /// peer omits falls back to the spec default; each supplied value is clamped
    /// to its spec upper bound. Retransmit shape (`backoff_factor`,
    /// `max_transmissions`) and the local `standalone_ack_deadline` are NOT
    /// peer-controlled and keep their [`Default`] values.
    ///
    /// - `sii` → `initial_idle` (Session Idle Interval)
    /// - `sai` → `initial_active` (Session Active Interval)
    /// - `sat` → `idle_threshold` (Session Active Threshold)
    #[must_use]
    pub fn for_peer(sii: Option<Duration>, sai: Option<Duration>, sat: Option<Duration>) -> Self {
        let base = Self::default();
        Self {
            initial_idle: sii.unwrap_or(SPEC_DEFAULT_IDLE).min(MAX_RETRANS_INTERVAL),
            initial_active: sai.unwrap_or(SPEC_DEFAULT_ACTIVE).min(MAX_RETRANS_INTERVAL),
            idle_threshold: sat
                .unwrap_or(SPEC_DEFAULT_ACTIVE_THRESHOLD)
                .min(MAX_ACTIVE_THRESHOLD),
            ..base
        }
    }

    /// Merge a peer's CASE/PASE-advertised
    /// [`SessionParameters`](matter_crypto::SessionParameters) over this
    /// config, overriding **only the fields the peer actually sent** and
    /// clamping each to its spec bound.
    ///
    /// # Why merge rather than replace
    ///
    /// The two reference implementations disagree here, and the disagreement
    /// is observable. chip decodes field by field into the config it already
    /// holds (`PairingSession::DecodeSessionParametersIfPresent`), so a peer
    /// that sends only `SII` leaves the existing `SAI`/`SAT` untouched.
    /// matter.js's `set timingParameters` replaces the triple wholesale, so
    /// the same message resets `SAI`/`SAT` to matter.js's own defaults.
    ///
    /// We follow chip. On the initiator side `self` is the config derived from
    /// the peer's operational mDNS TXT record — real evidence about that
    /// device — and discarding it because Sigma2 happened to omit one field
    /// would be a strict loss of information.
    ///
    /// On the responder side there is no mDNS record to start from, so the
    /// caller starts from [`MrpConfig::default`] (the spec defaults) and this
    /// reduces to exactly [`for_peer`](Self::for_peer) over the same three
    /// values.
    ///
    /// Retransmit *shape* (`backoff_factor`, `max_transmissions`,
    /// `backoff_jitter`) and the local `standalone_ack_deadline` are never
    /// peer-controlled and are carried through untouched — the same rule
    /// [`for_peer`](Self::for_peer) applies.
    #[must_use]
    pub fn merge_session_params(self, params: &matter_crypto::SessionParameters) -> Self {
        let ms = |v: u32| Duration::from_millis(u64::from(v));
        Self {
            initial_idle: params
                .session_idle_interval_ms
                .map_or(self.initial_idle, |v| ms(v).min(MAX_RETRANS_INTERVAL)),
            initial_active: params
                .session_active_interval_ms
                .map_or(self.initial_active, |v| ms(v).min(MAX_RETRANS_INTERVAL)),
            idle_threshold: params
                .session_active_threshold_ms
                .map_or(self.idle_threshold, |v| {
                    Duration::from_millis(u64::from(v)).min(MAX_ACTIVE_THRESHOLD)
                }),
            ..self
        }
    }

    /// Un-jittered total time a reliable message is given before it expires:
    /// the sum of the full idle-base retransmit schedule.
    ///
    /// Exists so the diagnostic emitted at session registration can state the
    /// number an operator is actually asking about ("why did this time out
    /// after N seconds") rather than making them derive it from four
    /// parameters.
    #[must_use]
    pub fn total_idle_window(&self) -> Duration {
        (0..self.max_transmissions)
            .map(|n| self.retransmit_delay(n, false))
            .sum()
    }

    /// Un-jittered wait AFTER a transmission, given how many sends have already
    /// gone out on this message.
    ///
    /// `sends_done` counts transmissions already made: 0 is the wait following
    /// the original send, 1 the wait after the first retransmit, and so on. It
    /// is **not** an index of the transmission about to happen — reading it that
    /// way would delay the first send by a whole base interval.
    ///
    /// `base × MRP_BACKOFF_MARGIN × backoff_factor^min(4, max(0, sends_done -
    /// MRP_BACKOFF_THRESHOLD))`, truncated to whole milliseconds: fractional
    /// milliseconds are not part of the spec's deadline grid. The base is the
    /// peer's idle or active interval per `peer_active`.
    ///
    /// The threshold is why the first two transmissions share an interval, and
    /// the margin is applied in chip's exact integer form. Jitter is **not**
    /// applied here — it is drawn per scheduling decision, and keeping this
    /// function pure is what makes the schedule table-testable.
    #[must_use]
    pub fn retransmit_delay(&self, sends_done: u8, peer_active: bool) -> Duration {
        let base = if peer_active {
            self.initial_active
        } else {
            self.initial_idle
        };
        // Margin first, in integer milliseconds, matching chip bit for bit.
        // `saturating_mul` guards a pathological advertised interval; the spec
        // ceiling (1 h) is nowhere near it.
        let base_ms = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
        let with_margin_ms =
            base_ms.saturating_mul(MRP_BACKOFF_MARGIN_NUMERATOR) / MRP_BACKOFF_MARGIN_DENOMINATOR;
        let exponent = sends_done
            .saturating_sub(MRP_BACKOFF_THRESHOLD)
            .min(MRP_BACKOFF_MAX_EXPONENT);
        // Inputs are bounded (base ≤ 1 h, factor ≈ 1.6, exponent ≤ 4), so the
        // f32 product stays well below 2^63 and is non-negative.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let scaled_ms =
            (with_margin_ms as f32 * self.backoff_factor.powi(i32::from(exponent))) as u64;
        Duration::from_millis(scaled_ms)
    }
}

/// Where a session's MRP configuration came from.
///
/// Carried purely so the one diagnostic line emitted at session registration
/// can say it. When an operator asks "why did this session time out after N
/// seconds", the answer is almost always which of these three applied, and
/// without it there is nothing in the logs to distinguish them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MrpProvenance {
    /// Parsed from something the peer itself said: the operational mDNS TXT
    /// `SII`/`SAI`/`SAT`, or the `SessionParameters` element it sent during
    /// session establishment (see
    /// [`MrpConfig::merge_session_params`]). The responder role reaches this
    /// through the second route — it has no mDNS record for the initiator, so
    /// Sigma1's element is its only source.
    PeerAdvertised,
    /// The peer advertised nothing for one or more keys, so spec defaults
    /// filled in.
    SpecDefault,
    /// No peer record was available at all — a recovery path with no discovery
    /// result to hand, or a peer that sent no `SessionParameters`.
    ///
    /// This used to name the responder role as a cause. It no longer is: a
    /// responder now sizes itself from the initiator's Sigma1 element.
    Unknown,
}

/// Caller-facing MRP control bits for an outbound message.
#[derive(Debug, Clone, Copy, Default)]
pub struct MrpFlags {
    /// If true, the message requires an MRP acknowledgement and will be
    /// retransmitted on timeout.
    pub reliable: bool,
}

/// Per-session timer event before `SessionManager` folds it into
/// [`MrpEvent`] with the `session_id` attached.
#[derive(Debug)]
#[non_exhaustive]
pub enum MrpTimerEvent {
    /// A pending reliable message hit its retransmit deadline. The cached
    /// encrypted wire bytes are returned so the manager can re-send them
    /// unchanged.
    Retransmit {
        /// Exchange the message belongs to.
        exchange_id: u16,
        /// Counter of the original message being re-sent.
        counter: MessageCounter,
        /// Encrypted wire bytes previously recorded via
        /// [`MrpState::mark_packet_sent`].
        packet: Vec<u8>,
    },
    /// A buffered piggyback ack overflowed its 200ms deadline; the
    /// manager must emit a standalone-ack.
    StandaloneAckDeadlineFired {
        /// Exchange the ack relates to.
        exchange_id: u16,
        /// Peer counter being acknowledged.
        ack_counter: MessageCounter,
        /// Whether the local side originated the exchange (drives the
        /// `I` flag on the standalone-ack).
        is_local_initiator: bool,
    },
    /// A reliable message exhausted its retransmit budget. The exchange
    /// should be torn down by the caller.
    Expired {
        /// Exchange the expired message belongs to.
        exchange_id: u16,
        /// Counter of the expired message.
        counter: MessageCounter,
    },
}

/// Logical work emitted by `SessionManager::handle_timeout`. The
/// `session_id` field disambiguates events across the manager's
/// per-session timer streams. `SessionManager` pre-builds standalone-ack
/// packets before pushing `SendStandaloneAck` here.
#[derive(Debug)]
#[non_exhaustive]
pub enum MrpEvent {
    /// A pending reliable message must be re-sent.
    Retransmit {
        /// Session the packet belongs to.
        session_id: SessionId,
        /// Exchange identifier.
        exchange_id: u16,
        /// Counter of the original message being re-sent.
        counter: MessageCounter,
        /// Encrypted wire bytes to re-transmit unchanged.
        packet: Vec<u8>,
    },
    /// A standalone-ack must be sent; `packet` is the fully-encrypted
    /// wire bytes built by the manager.
    SendStandaloneAck {
        /// Session the packet belongs to.
        session_id: SessionId,
        /// Exchange identifier.
        exchange_id: u16,
        /// Encrypted wire bytes ready to send.
        packet: Vec<u8>,
    },
    /// A reliable message exhausted its retransmit budget.
    Expired {
        /// Session the message belonged to.
        session_id: SessionId,
        /// Exchange identifier.
        exchange_id: u16,
        /// Counter of the expired message.
        counter: MessageCounter,
    },
    /// A standalone-ack was due (the peer's reliable message must be
    /// acknowledged) but could not be built because the session's outbound
    /// message counter is exhausted (`u32::MAX`). Per Matter Core Spec
    /// §4.6.2 a counter cannot wrap; the session must be re-keyed (a new
    /// CASE handshake) before any further traffic — including this ack —
    /// can be sent. Surfaced (rather than silently dropped) so the caller
    /// can tear the session down promptly instead of leaving the peer
    /// un-acked with no signal. The unacked peer counter is included so the
    /// caller can log/trace which message went un-acknowledged.
    SessionCounterExhausted {
        /// Session whose outbound counter is exhausted.
        session_id: SessionId,
        /// Exchange the un-acked peer message belongs to.
        exchange_id: u16,
        /// Peer counter that could not be acknowledged.
        ack_counter: MessageCounter,
    },
}

/// Outcome of processing an inbound decrypted payload through MRP.
///
/// `Debug` is written by hand so that an `AppMessage`'s decrypted `payload`
/// prints as its length only (`"<N bytes>"`): it is whatever plaintext the
/// peer sent, which can be a Wi-Fi passphrase, a Thread dataset or key
/// material, so it must never reach a log through a stray `{:?}`.
#[non_exhaustive]
pub enum InboundOutcome {
    /// A new application message was received. `payload` is the bytes
    /// AFTER the protocol header.
    AppMessage {
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
    /// The inbound message was an ack-only standalone (no app payload).
    AckOnly {
        /// Exchange identifier.
        exchange_id: u16,
        /// Counter that was acknowledged.
        acked_counter: MessageCounter,
    },
    /// The inbound message duplicates a recently-seen reliable message
    /// from the peer.
    DuplicateReliable {
        /// Exchange identifier.
        exchange_id: u16,
        /// Counter that the peer re-sent.
        peer_counter: MessageCounter,
        /// Whether the local side originated the exchange.
        is_local_initiator: bool,
    },
}

impl core::fmt::Debug for InboundOutcome {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Same variant names, fields and order as the derive this replaces;
        // only the decrypted payload's bytes are withheld.
        match self {
            Self::AppMessage {
                exchange_id,
                is_initiator,
                protocol_id,
                opcode,
                payload,
            } => f
                .debug_struct("AppMessage")
                .field("exchange_id", exchange_id)
                .field("is_initiator", is_initiator)
                .field("protocol_id", protocol_id)
                .field("opcode", opcode)
                .field("payload", &format!("<{} bytes>", payload.len()))
                .finish(),
            Self::AckOnly {
                exchange_id,
                acked_counter,
            } => f
                .debug_struct("AckOnly")
                .field("exchange_id", exchange_id)
                .field("acked_counter", acked_counter)
                .finish(),
            Self::DuplicateReliable {
                exchange_id,
                peer_counter,
                is_local_initiator,
            } => f
                .debug_struct("DuplicateReliable")
                .field("exchange_id", exchange_id)
                .field("peer_counter", peer_counter)
                .field("is_local_initiator", is_local_initiator)
                .finish(),
        }
    }
}

/// Output of [`MrpState::prepare_outbound`]: the encoded wire payload
/// (protocol header || application bytes) plus exchange bookkeeping.
pub struct PreparedOutbound {
    /// Encoded protocol header concatenated with the caller's
    /// application payload. The session-manager passes this verbatim to
    /// [`crate::framing::encode_secured`].
    pub wire_payload: Vec<u8>,
    /// Exchange identifier carried in the protocol header.
    pub exchange_id: u16,
    /// Whether the local side initiated the exchange (mirrors the `I`
    /// flag).
    pub is_local_initiator: bool,
    /// Whether a pending peer counter was piggybacked into this message.
    pub piggyback_acked: bool,
}

impl PreparedOutbound {
    /// The exchange this message was built for.
    pub(crate) fn exchange(&self) -> ExchangeKey {
        ExchangeKey {
            id: self.exchange_id,
            is_local_initiator: self.is_local_initiator,
        }
    }
}

/// Snapshot view returned by [`MrpState::check_duplicate_reliable`].
pub struct RecentInboundView {
    /// Exchange the recently-cached reliable message belonged to.
    pub exchange_id: u16,
    /// Whether the local side originated that exchange.
    pub is_local_initiator: bool,
}

/// Which exchange, within one session, a piece of MRP state belongs to: the
/// exchange id together with which side started the exchange.
///
/// The id alone does not name an exchange. Each node allocates ids for the
/// exchanges it starts from its own counter (Matter 1.4 Core §4.10.2), so an
/// exchange we start and one the peer starts can carry the same id at the
/// same time. The spec identifies an exchange by the tuple {Session Context,
/// Exchange ID, Exchange Role} (§4.10.2, §4.10.3); the session is implied
/// here, because each session owns its own [`MrpState`]. chip keys the same
/// way (`ExchangeContext::MatchExchange` compares the id and the initiator
/// role, `ExchangeContext.cpp:364-385` at master 5cd2917a, `:362-383` at
/// v1.4.2.0), and so does matter.js 0.17.1 (`ExchangeManager.ts:136`,
/// `:272-274`: a local exchange is indexed as `exchangeId | 0x10000`).
///
/// The derived `Ord` sorts by id, then the peer's exchange before ours
/// (`false < true`). Every place that emits ACKs in a fixed order uses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ExchangeKey {
    /// The exchange id carried in every message of the exchange.
    pub(crate) id: u16,
    /// Whether we started the exchange. Every message we send on it carries
    /// the `I` flag exactly when this is true (§4.10.4).
    pub(crate) is_local_initiator: bool,
}

impl ExchangeKey {
    /// The exchange an inbound message belongs to: `I=1` from the peer means
    /// the peer started it; `I=0` means we did (§4.10.5.1, criterion 3).
    pub(crate) fn of_inbound(exchange_id: u16, peer_is_initiator: bool) -> Self {
        Self {
            id: exchange_id,
            is_local_initiator: !peer_is_initiator,
        }
    }
}

/// Pending retransmit entry for one outbound reliable message.
#[derive(Debug)]
struct PendingAck {
    packet_bytes: Vec<u8>,
    exchange: ExchangeKey,
    next_attempt: Instant,
    attempts_remaining: u8,
}

/// Buffered piggyback-ack for one exchange. Held in a per-exchange map
/// (keyed by [`ExchangeKey`]) so concurrent reliable inbounds on different
/// exchanges never clobber each other's buffered ack. Each entry is either
/// drained by the next outbound in the same exchange (cheap) or flushed as
/// a standalone-ack when the 200ms deadline expires (fallback). The `I`
/// flag of the standalone ACK comes from the key.
#[derive(Debug)]
struct PendingOutboundAck {
    ack_counter: MessageCounter,
    deadline: Instant,
}

impl PendingOutboundAck {
    /// This ACK, owed on `exchange`, as the timer event `SessionManager`
    /// turns into an encrypted standalone-ACK packet.
    fn fired(self, exchange: ExchangeKey) -> MrpTimerEvent {
        MrpTimerEvent::StandaloneAckDeadlineFired {
            exchange_id: exchange.id,
            ack_counter: self.ack_counter,
            is_local_initiator: exchange.is_local_initiator,
        }
    }
}

/// Cache entry for a recently-seen reliable inbound, kept in a 32-slot
/// ring buffer for duplicate-reliable detection (Matter Core Spec §4.12.6
/// dedup-resend path).
#[derive(Debug)]
struct RecentInbound {
    exchange_id: u16,
    counter: MessageCounter,
    is_local_initiator: bool,
}

/// Per-session MRP state.
#[derive(Debug)]
pub struct MrpState {
    /// Our unacknowledged reliable messages, by message counter. An inbound
    /// ACK clears its entry by counter alone, as §4.12.5.2.1 says ("query
    /// the retransmission table for the Acknowledgement Message Counter"):
    /// our counter is unique within the session, so the counter names one
    /// message whatever exchange the ACK arrives on.
    pending_acks: HashMap<MessageCounter, PendingAck>,
    pending_outbound_acks: HashMap<ExchangeKey, PendingOutboundAck>,
    /// ACKs owed for reliable messages that a newer reliable message on the
    /// same exchange displaced from `pending_outbound_acks`, oldest first.
    /// Each is due at once (its deadline is the instant it was displaced), so
    /// the next [`Self::handle_timeout`] sends it ahead of every other ACK.
    displaced_acks: Vec<(ExchangeKey, PendingOutboundAck)>,
    recent_reliable: [Option<RecentInbound>; 32],
    recent_next_slot: usize,
    /// The live exchanges. Membership is what decides the `I` flag of a send
    /// that names its exchange by id alone (see [`Self::resolve`]).
    exchanges: HashSet<ExchangeKey>,
    last_outbound: Option<Instant>,
    /// Wall-clock of the most recent message RECEIVED from the peer, used to
    /// classify the peer as "active" (within `config.idle_threshold` = the
    /// Session Active Threshold, SAT) vs idle. The retransmit base interval is
    /// selected off THIS, not our own outbound timing — chip
    /// `SecureSession::IsPeerActive` / `GetMRPBaseTimeout`. `None` until
    /// the first inbound: a peer we have never heard from is treated as idle,
    /// so we never hammer a sleepy device with active-interval spacing.
    last_peer_activity: Option<Instant>,
    config: MrpConfig,
    /// State for the retransmit jitter draw. Not a security parameter — jitter
    /// only de-synchronises controllers retransmitting to the same peer — so a
    /// small non-cryptographic PRNG is the right tool, and it keeps this crate
    /// free of an RNG dependency in its sans-IO core.
    jitter: u64,
}

/// Apply [`MrpConfig::backoff_jitter`] to `delay`, advancing `state`.
///
/// Free function rather than a method so `handle_timeout` can call it while
/// holding a `&mut` borrow of `pending_acks`. Jitter only ever LENGTHENS: the
/// multiplier lies in `[1.0, 1.0 + backoff_jitter)`, matching chip's
/// `(1024 + rand_u8) / 1024` (`ReliableMessageMgr.cpp:314-315`). A
/// `backoff_jitter` of `0.0` returns `delay` unchanged, which is how tests pin
/// exact deadlines.
fn jitter_with(state: &mut u64, config: &MrpConfig, delay: Duration) -> Duration {
    if config.backoff_jitter <= 0.0 {
        return delay;
    }
    // xorshift64*: adequate for de-synchronisation, deterministic from a seed.
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    let draw = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
    // Top 8 bits → [0, 1), mirroring chip's rand_u8 granularity.
    #[allow(clippy::cast_precision_loss)]
    let unit = f32::from(u8::try_from(draw >> 56).unwrap_or(0)) / 256.0;
    let ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let jittered = (ms as f32 * (1.0 + config.backoff_jitter * unit)) as u64;
    Duration::from_millis(jittered)
}

impl MrpState {
    /// The retransmit configuration this state was created with.
    ///
    /// `MrpConfig` is `Copy`, and the config is fixed for a session's lifetime
    /// (it is chosen once, at registration, before any traffic), so handing out
    /// a copy cannot desynchronise anything.
    #[must_use]
    pub fn config(&self) -> MrpConfig {
        self.config
    }

    /// Apply this state's jitter to `delay`. See [`jitter_with`].
    fn jittered(&mut self, delay: Duration) -> Duration {
        jitter_with(&mut self.jitter, &self.config, delay)
    }

    /// Create a fresh state with the given config, seeding retransmit jitter
    /// from the system CSPRNG.
    ///
    /// A CSPRNG failure falls back to a per-instance counter rather than
    /// panicking or returning `Result`: this constructor is infallible by design
    /// and CONTRIBUTING.md forbids `unwrap`/`expect` here. The fallback still varies
    /// between sessions — a single fixed seed would make every session on the
    /// node jitter identically, defeating the one thing jitter is for.
    #[must_use]
    pub fn new(config: MrpConfig) -> Self {
        let mut bytes = [0u8; 8];
        let seed = if matter_crypto::random_bytes(&mut bytes).is_ok() {
            u64::from_le_bytes(bytes)
        } else {
            static FALLBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            tracing::warn!(
                target: "matter_transport::mrp",
                "system CSPRNG unavailable seeding MRP jitter; falling back to a counter"
            );
            FALLBACK.fetch_add(0x9E37_79B9_7F4A_7C15, std::sync::atomic::Ordering::Relaxed)
        };
        Self::with_jitter_seed(config, seed)
    }

    /// [`Self::new`] with an explicit jitter seed, for deterministic tests and
    /// trace reproduction.
    #[must_use]
    pub fn with_jitter_seed(config: MrpConfig, seed: u64) -> Self {
        Self {
            // xorshift64* degenerates at zero.
            jitter: if seed == 0 { 1 } else { seed },
            ..Self::bare(config)
        }
    }

    /// Every field except the jitter seed, which the two public constructors
    /// supply.
    fn bare(config: MrpConfig) -> Self {
        Self {
            pending_acks: HashMap::new(),
            pending_outbound_acks: HashMap::new(),
            displaced_acks: Vec::new(),
            recent_reliable: std::array::from_fn(|_| None),
            recent_next_slot: 0,
            exchanges: HashSet::new(),
            last_outbound: None,
            last_peer_activity: None,
            jitter: 1,
            config,
        }
    }

    /// Whether the peer is currently "active" — it has sent us a message within
    /// the Session Active Threshold (`config.idle_threshold`, SAT). Drives the
    /// retransmit base interval (active → `initial_active`/SAI, idle →
    /// `initial_idle`/SII), re-evaluated on every retransmit exactly as chip's
    /// `GetMRPBaseTimeout` does. A peer we have never heard from (`None`) is
    /// idle: sizing the FIRST retransmit on the slower idle interval is the safe
    /// direction (it never over-drives a sleepy device).
    fn is_peer_active(&self, now: Instant) -> bool {
        match self.last_peer_activity {
            Some(t) => now.saturating_duration_since(t) < self.config.idle_threshold,
            None => false,
        }
    }

    /// Record `id` as an exchange **we** initiated, so the next
    /// [`Self::prepare_outbound`] on it sets the `I` flag.
    ///
    /// The id itself comes from [`SessionManager`](crate::SessionManager), which
    /// owns one counter per initiator *node* — Matter Core Spec §4.10.2 requires
    /// exactly that, and a per-session counter (which this type used to keep)
    /// does not satisfy it.
    pub fn note_local_exchange(&mut self, id: u16) {
        self.exchanges.insert(ExchangeKey {
            id,
            is_local_initiator: true,
        });
    }

    /// The exchange a method that takes only an exchange id means.
    ///
    /// An id names one exchange unless both sides started an exchange with
    /// it (see [`ExchangeKey`]). This picks the exchange we started if the
    /// table holds one with this id, and otherwise the peer's. An id the
    /// table does not hold at all is treated as the peer's (we are
    /// responding, `I=0`), which is what these methods did before the table
    /// knew the role. Preferring ours keeps the result they gave after a
    /// collision too: the single entry then always said "ours" once we had
    /// sent on the id.
    ///
    /// [`SessionManager`](crate::SessionManager)'s `*_with_role` methods name
    /// the role explicitly and never consult this.
    pub(crate) fn resolve(&self, id: u16) -> ExchangeKey {
        let ours = ExchangeKey {
            id,
            is_local_initiator: true,
        };
        if self.exchanges.contains(&ours) {
            ours
        } else {
            ExchangeKey {
                id,
                is_local_initiator: false,
            }
        }
    }

    /// Build an outbound wire payload (encoded protocol header concatenated
    /// with `app_payload`) on `exchange_id`.
    ///
    /// `is_local_initiator` comes from the exchange table: an id announced via
    /// [`Self::note_local_exchange`] is ours and sets the `I` flag; any other id
    /// (a peer-assigned one, or one used without a prior inbound) is recorded as
    /// a response with `is_local_initiator = false`. If both an exchange we
    /// started and one the peer started carry `exchange_id`, this sends on
    /// ours; [`SessionManager::encode_outbound_with_role`](crate::SessionManager::encode_outbound_with_role)
    /// can name either.
    ///
    /// When the chosen exchange's buffered piggyback ack is drained, the
    /// outgoing header carries `A=1` and `ack_counter = pending.ack_counter`.
    /// Only that exchange's ACK can ride: an ACK owed on the other exchange
    /// with the same id stays owed.
    ///
    /// This records the exchange and drains the ACK at once, before the
    /// caller has framed anything; a caller whose framing then fails cannot
    /// undo either. [`SessionManager::encode_outbound`](crate::SessionManager::encode_outbound)
    /// therefore does not call it: it builds the payload without changing
    /// anything and commits these two changes only once encryption has
    /// succeeded, so a failed send (`PayloadTooLarge`, for one) leaves the
    /// exchange table and the owed ACK as they were.
    ///
    /// # Errors
    ///
    /// Currently infallible in practice — returns `Result` for forward
    /// compatibility with future exchange-table validation.
    pub fn prepare_outbound(
        &mut self,
        opcode: u8,
        protocol_id: ProtocolId,
        exchange_id: u16,
        app_payload: &[u8],
        mrp_flags: MrpFlags,
        _now: Instant,
    ) -> Result<PreparedOutbound> {
        let prepared = self.build_outbound(
            opcode,
            protocol_id,
            self.resolve(exchange_id),
            app_payload,
            mrp_flags,
        );
        self.commit_outbound(&prepared);
        Ok(prepared)
    }

    /// Build the wire payload for a message on `exchange`, changing nothing:
    /// the exchange is not recorded and its owed ACK, if any, is copied into
    /// the header but stays owed. Pair it with [`Self::commit_outbound`] once
    /// the message is certain to be sent.
    ///
    /// The caller names the exchange in full, so the `I` flag is
    /// `exchange.is_local_initiator` whatever the table holds: for a fresh id
    /// the caller has just allocated for an exchange we start, for a
    /// follow-up on one of ours (even one already reclaimed), or for an
    /// answer on the peer's. [`Self::resolve`] gives the key a method that
    /// takes only an id means.
    pub(crate) fn build_outbound(
        &self,
        opcode: u8,
        protocol_id: ProtocolId,
        exchange: ExchangeKey,
        app_payload: &[u8],
        mrp_flags: MrpFlags,
    ) -> PreparedOutbound {
        let ExchangeKey {
            id: exchange_id,
            is_local_initiator,
        } = exchange;

        // Copy the buffered piggyback for THIS exchange, if any, leaving it
        // buffered: `commit_outbound` drains it once the send is certain.
        // Other exchanges' buffered acks are untouched, including one owed
        // on the other exchange that shares this id.
        let (ack_flag, ack_counter, piggyback_acked) =
            if let Some(p) = self.pending_outbound_acks.get(&exchange) {
                (ExchangeFlags::ACK, Some(p.ack_counter), true)
            } else {
                (ExchangeFlags::empty(), None, false)
            };

        let mut flags = ack_flag;
        if is_local_initiator {
            flags |= ExchangeFlags::INITIATOR;
        }
        if mrp_flags.reliable {
            flags |= ExchangeFlags::RELIABLE;
        }
        let header = ProtocolHeader {
            exchange_flags: flags,
            opcode,
            exchange_id,
            protocol_id,
            ack_counter,
        };
        let mut wire_payload = Vec::with_capacity(app_payload.len() + 16);
        encode_protocol_header(&header, &mut wire_payload);
        wire_payload.extend_from_slice(app_payload);

        PreparedOutbound {
            wire_payload,
            exchange_id,
            is_local_initiator,
            piggyback_acked,
        }
    }

    /// Apply the two changes [`Self::build_outbound`] left out, now that
    /// `prepared` is certain to be sent: record its exchange (with the `I`
    /// flag it was built with) and, if it carries this exchange's owed ACK,
    /// drain that ACK.
    ///
    /// Call it with no other change to this state between the build and the
    /// commit, so the ACK drained is the one the header carries. The caller
    /// then calls [`Self::mark_packet_sent`].
    pub(crate) fn commit_outbound(&mut self, prepared: &PreparedOutbound) {
        // Record the exchange the message was built for: a fresh local one,
        // a reclaimed one we are continuing, or a response. An exchange with
        // the same id and the other role is a different entry and is not
        // touched.
        let exchange = prepared.exchange();
        self.exchanges.insert(exchange);
        if prepared.piggyback_acked {
            self.pending_outbound_acks.remove(&exchange);
        }
    }

    /// Register the encrypted wire bytes of a just-sent message. Caller
    /// passes `reliable` to indicate whether MRP should track this message
    /// for retransmit. Unreliable messages still update `last_outbound` so
    /// idle-vs-active classification remains accurate.
    ///
    /// The first retransmit deadline uses the active base (`initial_active`,
    /// SAI) when the PEER is active — it has sent us something within SAT
    /// (`is_peer_active`) — and the idle base (`initial_idle`, SII) otherwise.
    /// Selection is off the peer's activity, NOT our own outbound timing;
    /// subsequent retransmits re-evaluate it in `handle_timeout`.
    ///
    /// `exchange_id` names the exchange as [`Self::prepare_outbound`] does:
    /// ours if the table holds an exchange we started with this id, and
    /// otherwise the peer's. That is the exchange the preceding
    /// [`Self::prepare_outbound`] on the same id chose.
    pub fn mark_packet_sent(
        &mut self,
        counter: MessageCounter,
        exchange_id: u16,
        packet_bytes: Vec<u8>,
        reliable: bool,
        now: Instant,
    ) {
        let exchange = self.resolve(exchange_id);
        self.mark_packet_sent_on(counter, exchange, packet_bytes, reliable, now);
    }

    /// [`Self::mark_packet_sent`] for the exchange named in full, the one
    /// the message was built for ([`PreparedOutbound::exchange`]).
    pub(crate) fn mark_packet_sent_on(
        &mut self,
        counter: MessageCounter,
        exchange: ExchangeKey,
        packet_bytes: Vec<u8>,
        reliable: bool,
        now: Instant,
    ) {
        // `last_outbound` is retained for callers/telemetry; it no longer drives
        // the active/idle classification (that is peer-activity based now).
        self.last_outbound = Some(now);

        if !reliable {
            // Nothing will ever ack an unreliable message, so this send is the
            // last event the exchange will see from our side: reclaim it now if
            // no other work keeps it live. Without this, every peer-initiated
            // exchange we answer unreliably (a subscription ReportData's
            // StatusResponse) leaked a slot until `MAX_EXCHANGES_PER_SESSION`
            // was reached and the session rejected every new peer exchange.
            // Hanging it off the send, not the ack drain (`commit_outbound`),
            // also covers an answer sent after the standalone-ack deadline
            // already flushed the ack (nothing left to drain, entry re-created).
            self.reclaim_if_idle(exchange);
            return;
        }

        // The caller has ALREADY sent once, so this schedules the wait after
        // transmission 0 and budgets the remaining retransmits. Seeding
        // `attempts_remaining` with the full `max_transmissions` here is what
        // produced six transmissions where the spec allows five: one send plus
        // five retransmits. `saturating_sub` so a pathological config of 0
        // cannot underflow.
        let peer_active = self.is_peer_active(now);
        let initial = self.config.retransmit_delay(0, peer_active);
        let next_attempt = now + self.jittered(initial);
        self.pending_acks.insert(
            counter,
            PendingAck {
                packet_bytes,
                exchange,
                next_attempt,
                attempts_remaining: self.config.max_transmissions.saturating_sub(1),
            },
        );
    }

    /// Whether this session has any in-flight reliable work — an unacked
    /// retransmit or an ACK it still owes. Used by the session table's
    /// idle-first eviction to avoid tearing down a session mid-exchange when a
    /// truly-idle one could be reclaimed instead.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.pending_acks.is_empty()
            || !self.pending_outbound_acks.is_empty()
            || !self.displaced_acks.is_empty()
    }

    /// Earliest pending deadline across retransmits, the piggyback-ack flush
    /// deadline and displaced ACKs (due at once), or `None` if nothing
    /// pending.
    #[must_use]
    pub fn poll_timeout(&self) -> Option<Instant> {
        let retransmit = self.pending_acks.values().map(|p| p.next_attempt).min();
        let standalone = self
            .pending_outbound_acks
            .values()
            .map(|p| p.deadline)
            .min();
        let displaced = self.displaced_acks.iter().map(|(_, p)| p.deadline).min();
        [retransmit, standalone, displaced]
            .into_iter()
            .flatten()
            .min()
    }

    /// Advance retransmit timers. For each pending whose `next_attempt`
    /// has elapsed, emit a `Retransmit` event and schedule the next
    /// attempt (exponential backoff). If `attempts_remaining` reaches 0,
    /// emit `Expired` and drop the entry.
    pub fn handle_timeout(&mut self, now: Instant) -> Vec<MrpTimerEvent> {
        let mut events = Vec::new();
        let mut to_remove = Vec::new();
        // Exchanges whose live work cleared this tick (an expired retransmit
        // or a drained buffered ack); each is reclaimed if it ends up idle.
        let mut reclaim_candidates: Vec<ExchangeKey> = Vec::new();

        // Re-evaluate the peer's active/idle state ONCE for this tick, exactly
        // as chip re-evaluates `IsPeerActive` on every retransmit scheduling:
        // a peer that has gone silent past SAT since the message was queued
        // switches to the slower idle base for its remaining retransmits (the
        // CASE-Sigma3-to-a-sleepy-device failure mode). `last_peer_activity`
        // does not change during this loop, so one read is exact.
        let peer_active = self.is_peer_active(now);

        for (counter, pending) in &mut self.pending_acks {
            if pending.next_attempt > now {
                continue;
            }
            if pending.attempts_remaining == 0 {
                events.push(MrpTimerEvent::Expired {
                    exchange_id: pending.exchange.id,
                    counter: *counter,
                });
                to_remove.push(*counter);
                reclaim_candidates.push(pending.exchange);
                continue;
            }
            events.push(MrpTimerEvent::Retransmit {
                exchange_id: pending.exchange.id,
                counter: *counter,
                packet: pending.packet_bytes.clone(),
            });
            pending.attempts_remaining -= 1;
            // Transmissions made so far: the original send plus the retransmits
            // already emitted, which is what `retransmit_delay` indexes on.
            // `attempts_remaining` was seeded at `max_transmissions - 1` (the
            // original send is not one of the remaining attempts), so that is
            // what we count down from — subtracting from `max_transmissions`
            // would skip `retransmit_delay(1)` and with it the second base
            // interval that MRP_BACKOFF_THRESHOLD exists to produce.
            let sends_done = self
                .config
                .max_transmissions
                .saturating_sub(1)
                .saturating_sub(pending.attempts_remaining);
            let delay = self.config.retransmit_delay(sends_done, peer_active);
            pending.next_attempt = now + jitter_with(&mut self.jitter, &self.config, delay);
        }
        for c in to_remove {
            self.pending_acks.remove(&c);
        }

        // Displaced ACKs first: each is older than the ACK that displaced it.
        let mut still_waiting = Vec::new();
        for (exchange, p) in std::mem::take(&mut self.displaced_acks) {
            if p.deadline <= now {
                events.push(p.fired(exchange));
                reclaim_candidates.push(exchange);
            } else {
                still_waiting.push((exchange, p));
            }
        }
        self.displaced_acks = still_waiting;

        // Standalone-ack deadline: flush every buffered piggyback whose
        // deadline has passed before the next outbound in its exchange,
        // emitting one standalone-ack per due exchange.
        let mut due_exchanges: Vec<ExchangeKey> = self
            .pending_outbound_acks
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(exchange, _)| *exchange)
            .collect();
        // Deterministic emission order — HashMap iteration order is
        // unspecified; sorting (by id, then the peer's exchange before ours)
        // keeps event ordering stable for callers/tests.
        due_exchanges.sort_unstable();
        for exchange in due_exchanges {
            if let Some(p) = self.pending_outbound_acks.remove(&exchange) {
                events.push(p.fired(exchange));
                reclaim_candidates.push(exchange);
            }
        }

        // Reclaim any exchange whose last piece of live work cleared this
        // tick and is now idle (no pending retransmit, no ACK still owed).
        for exchange in reclaim_candidates {
            self.reclaim_if_idle(exchange);
        }

        events
    }

    /// Parse an inbound decrypted payload's protocol header and update MRP
    /// bookkeeping.
    ///
    /// - On `A=1`: clears the matching pending retransmit (`ack_counter`).
    ///   If the payload is a bare `StandaloneAck` (opcode `0x10`, empty app
    ///   payload, `R=0`), returns [`InboundOutcome::AckOnly`].
    /// - On `R=1`: buffers a per-exchange piggyback ack with the
    ///   200ms deadline from [`MrpConfig::standalone_ack_deadline`]. The
    ///   ack will be drained by the next outbound in the same exchange, or
    ///   flushed as a standalone-ack via [`MrpTimerEvent::StandaloneAckDeadlineFired`].
    ///   A second reliable message on the same exchange owes the first one's
    ///   ACK at once (due immediately in [`Self::poll_timeout`]).
    /// - Otherwise: returns [`InboundOutcome::AppMessage`] with the
    ///   post-header bytes.
    ///
    /// # Errors
    ///
    /// Returns the underlying protocol-header decode error if the payload
    /// is malformed.
    pub fn process_inbound(
        &mut self,
        decrypted_payload: Vec<u8>,
        peer_counter: MessageCounter,
        now: Instant,
    ) -> Result<InboundOutcome> {
        // Any inbound is fresh evidence the peer is awake — it feeds the
        // active/idle classification (`is_peer_active`). Record it before
        // any early return so even an ack-only or duplicate frame counts.
        self.last_peer_activity = Some(now);
        // Decode once and use the tail-slice length to split off app bytes.
        let (header, header_len) = {
            let (h, tail) = crate::protocol_header::decode_protocol_header(&decrypted_payload)?;
            let header_len = decrypted_payload.len() - tail.len();
            (h, header_len)
        };
        // Drop the (small) protocol header in place and reuse the existing
        // allocation as the app payload. `drain(..header_len)` shifts the
        // body down within the same buffer (a memmove of the body, no second
        // heap allocation) — cheaper than collecting the tail into a fresh
        // Vec and leaving the original allocation as garbage.
        let mut app_payload = decrypted_payload;
        app_payload.drain(..header_len);

        let exchange_id = header.exchange_id;
        let peer_is_initiator = header.exchange_flags.contains(ExchangeFlags::INITIATOR);
        let we_are_initiator = !peer_is_initiator;
        // Match the message to its exchange by id AND role (§4.10.5.1): `I=1`
        // belongs to an exchange the peer started, `I=0` to one we started.
        // A peer exchange that reuses the id of one of ours is a different
        // exchange, with its own owed ACK.
        let exchange = ExchangeKey::of_inbound(exchange_id, peer_is_initiator);

        // Record the exchange on first sight, bounding the table so a peer
        // cannot grow it without limit via fresh, peer-controlled
        // exchange_ids. If this is a NEW exchange and the table is already
        // at the cap, reject the message rather than insert. Messages on
        // exchanges we already track are always accepted (they cost no new
        // slot), so the cap never interferes with established traffic.
        if !self.exchanges.contains(&exchange) {
            if self.exchanges.len() >= MAX_EXCHANGES_PER_SESSION {
                return Err(Error::ExchangeTableFull);
            }
            self.exchanges.insert(exchange);
        }

        if header.exchange_flags.contains(ExchangeFlags::ACK) {
            if let Some(MessageCounter(c)) = header.ack_counter {
                // By counter alone (§4.12.5.2.1): our counter names one
                // message, whatever exchange the ACK rides. If it was on the
                // other exchange with this id (a peer that piggybacks on node
                // and exchange id only, as §4.12.5.1.1 reads, can do that),
                // that exchange may now have no work left; reclaim it here,
                // since the reclaims below act on the inbound's own exchange.
                if let Some(acked) = self.pending_acks.remove(&MessageCounter(c)) {
                    if acked.exchange != exchange {
                        self.reclaim_if_idle(acked.exchange);
                    }
                }
            }
            // StandaloneAck: opcode 0x10, empty app payload, A=1, R=0.
            // `unwrap_or` here is safe — A=1 guarantees ack_counter is Some,
            // but the lint-clean form keeps clippy::unwrap_used happy.
            if header.opcode == crate::protocol_header::opcode::secure_channel::STANDALONE_ACK
                && app_payload.is_empty()
                && !header.exchange_flags.contains(ExchangeFlags::RELIABLE)
            {
                // A pure ack completes the round-trip from our side — the
                // pending retransmit just cleared above. Reclaim the
                // exchange if nothing else keeps it live.
                self.reclaim_if_idle(exchange);
                return Ok(InboundOutcome::AckOnly {
                    exchange_id,
                    acked_counter: header.ack_counter.unwrap_or(MessageCounter(0)),
                });
            }
        }

        if header.exchange_flags.contains(ExchangeFlags::RELIABLE) {
            // Buffer this exchange's piggyback ack. Keyed by exchange (id and
            // role) so a concurrent reliable inbound on a different exchange,
            // including one that shares this id, never clobbers this one. A second reliable message on the SAME
            // exchange displaces the first one's ack: owe that one at once
            // rather than lose it (chip `ReliableMessageContext.cpp:158-166`,
            // "forcing tx of solitary ack"; matter.js
            // `MessageExchange.ts:426-431`).
            let fresh = PendingOutboundAck {
                ack_counter: peer_counter,
                deadline: now + self.config.standalone_ack_deadline,
            };
            if let Some(older) = self.pending_outbound_acks.insert(exchange, fresh) {
                if self.displaced_acks.len() < MAX_DISPLACED_ACKS {
                    self.displaced_acks.push((
                        exchange,
                        PendingOutboundAck {
                            deadline: now,
                            ..older
                        },
                    ));
                } else {
                    // Only a misbehaving peer gets here (a conformant one
                    // never has two unacknowledged reliable messages on one
                    // exchange); make it diagnosable.
                    tracing::debug!(
                        target: "matter_transport::mrp",
                        exchange_id,
                        is_local_initiator = we_are_initiator,
                        dropped_ack_counter = older.ack_counter.0,
                        "displaced-ACK queue full; dropping the displaced ACK"
                    );
                }
            }
            // Insertion-order eviction into the 32-entry ring buffer.
            self.recent_reliable[self.recent_next_slot] = Some(RecentInbound {
                exchange_id,
                counter: peer_counter,
                is_local_initiator: we_are_initiator,
            });
            self.recent_next_slot = (self.recent_next_slot + 1) % 32;
        }

        // If a piggybacked ack cleared this exchange's last pending
        // retransmit and the inbound was not itself reliable (so it buffered
        // no outbound ack), the exchange is now idle and can be reclaimed.
        self.reclaim_if_idle(exchange);

        Ok(InboundOutcome::AppMessage {
            exchange_id,
            is_initiator: peer_is_initiator,
            protocol_id: header.protocol_id,
            opcode: header.opcode,
            payload: app_payload,
        })
    }

    /// Cancel a pending piggyback/standalone-ack registration for
    /// `exchange`, if any.
    ///
    /// Used by [`crate::session::SessionManager::decode_inbound`] when the
    /// owning session's `transport_reliable` flag is set: the underlying
    /// transport (BTP) already guarantees ordered, reliable delivery, so no
    /// local ack bookkeeping should be armed — defensively, even if a
    /// (possibly buggy) peer sets the R flag on an inbound message.
    pub(crate) fn cancel_pending_outbound_ack(&mut self, exchange: ExchangeKey) {
        self.pending_outbound_acks.remove(&exchange);
        self.displaced_acks
            .retain(|(owed_on, _)| *owed_on != exchange);
    }

    /// Look up `peer_counter` in the 32-entry recent-reliable cache.
    /// Returns `Some(view)` if the counter was seen on a recent reliable
    /// inbound, or `None` if the counter is not cached (either never seen or
    /// already evicted).
    ///
    /// [`SessionManager::decode_inbound`](crate::SessionManager::decode_inbound)
    /// no longer consults it: it ACKs every authenticated duplicate that has
    /// R set, from the duplicate's own protocol header, as chip does, whether
    /// or not the counter is still cached.
    ///
    /// Linear scan over 32 slots — trivial cost.
    #[must_use]
    pub fn check_duplicate_reliable(
        &self,
        peer_counter: MessageCounter,
    ) -> Option<RecentInboundView> {
        self.recent_reliable
            .iter()
            .flatten()
            .find(|r| r.counter == peer_counter)
            .map(|r| RecentInboundView {
                exchange_id: r.exchange_id,
                is_local_initiator: r.is_local_initiator,
            })
    }

    /// Abort the exchange identified by `exchange_id` (chip's `Abort()`
    /// without its ACK flush):
    /// - Removes it from the exchange table.
    /// - Drops any pending retransmits scoped to that exchange.
    /// - **Drops** its buffered piggyback ACK, and any displaced one, unsent.
    ///
    /// To abort the way chip does, sending the ACK first, use
    /// [`SessionManager::abort_exchange`](crate::SessionManager::abort_exchange),
    /// or call [`Self::flush_exchange_acks`] before this.
    ///
    /// `recent_reliable` entries for this exchange are intentionally
    /// preserved: a late peer retransmit of a request from the now-closed
    /// exchange should still trigger the ack-resend path rather than
    /// surfacing as a fresh application message.
    ///
    /// `exchange_id` means the exchange we started if the table holds one
    /// with this id, and otherwise the peer's; an exchange with the same id
    /// and the other role is left alone.
    /// [`SessionManager::abort_exchange_with_role`](crate::SessionManager::abort_exchange_with_role)
    /// can name either.
    pub fn close_exchange(&mut self, exchange_id: u16) {
        let exchange = self.resolve(exchange_id);
        self.close_exchange_on(exchange);
    }

    /// [`Self::close_exchange`] for the exchange named in full.
    pub(crate) fn close_exchange_on(&mut self, exchange: ExchangeKey) {
        self.exchanges.remove(&exchange);
        self.pending_acks.retain(|_, p| p.exchange != exchange);
        self.pending_outbound_acks.remove(&exchange);
        self.displaced_acks
            .retain(|(owed_on, _)| *owed_on != exchange);
    }

    /// Take every ACK `exchange_id` still owes (a displaced one first, then its
    /// buffered piggyback ACK) as [`MrpTimerEvent::StandaloneAckDeadlineFired`]
    /// events to send now, then reclaim the exchange if nothing else keeps it
    /// live.
    ///
    /// The sans-IO half of finishing an exchange. chip's
    /// `ExchangeContext::DoClose` calls `FlushAcks()` on every `Close()` and
    /// `Abort()`, and matter.js's `MessageExchange.close()` sends the pending
    /// ACK before it closes. Without it, an ACK nothing can piggyback
    /// (the next request always opens a new exchange) waits for the
    /// [`MrpConfig::standalone_ack_deadline`] timer, and is lost if the
    /// timer's owner stops first.
    ///
    /// Our own pending retransmits on the exchange keep running (chip's
    /// `Close()`); [`Self::close_exchange`] is the abort.
    ///
    /// Call it only when the exchange will carry nothing more from our side:
    /// once the exchange is reclaimed, [`Self::prepare_outbound`] on the same
    /// id re-creates it as peer-initiated and the message goes out with
    /// `I=0`. With nothing buffered it returns an empty `Vec`, so a second
    /// call is harmless.
    ///
    /// `exchange_id` means the exchange we started if the table holds one
    /// with this id, and otherwise the peer's; an ACK owed on an exchange
    /// with the same id and the other role stays owed.
    /// [`SessionManager::finish_exchange_with_role`](crate::SessionManager::finish_exchange_with_role)
    /// can name either.
    #[must_use = "the returned events carry standalone-ACK packets to send"]
    pub fn flush_exchange_acks(&mut self, exchange_id: u16) -> Vec<MrpTimerEvent> {
        let exchange = self.resolve(exchange_id);
        self.flush_exchange_acks_on(exchange)
    }

    /// [`Self::flush_exchange_acks`] for the exchange named in full.
    pub(crate) fn flush_exchange_acks_on(&mut self, exchange: ExchangeKey) -> Vec<MrpTimerEvent> {
        let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut self.displaced_acks)
            .into_iter()
            .partition(|(owed_on, _)| *owed_on == exchange);
        self.displaced_acks = others;
        let mut events: Vec<MrpTimerEvent> = mine
            .into_iter()
            .map(|(owed_on, p)| p.fired(owed_on))
            .collect();
        if let Some(p) = self.pending_outbound_acks.remove(&exchange) {
            events.push(p.fired(exchange));
        }
        self.reclaim_if_idle(exchange);
        events
    }

    /// [`Self::flush_exchange_acks`] for every exchange that owes an ACK, in
    /// ascending exchange-id order (the order [`Self::handle_timeout`] uses;
    /// where an exchange we started and one the peer started share an id,
    /// the peer's comes first). For a session that is going away.
    #[must_use = "the returned events carry standalone-ACK packets to send"]
    pub fn flush_all_acks(&mut self) -> Vec<MrpTimerEvent> {
        let mut exchanges: Vec<ExchangeKey> = self
            .pending_outbound_acks
            .keys()
            .copied()
            .chain(self.displaced_acks.iter().map(|(owed_on, _)| *owed_on))
            .collect();
        exchanges.sort_unstable();
        exchanges.dedup();
        let mut events = Vec::new();
        for exchange in exchanges {
            events.extend(self.flush_exchange_acks_on(exchange));
        }
        events
    }

    /// Drop the ACK still owed for the peer's message `counter` (buffered or
    /// displaced), if any, and reclaim its exchange if that leaves it idle.
    /// The duplicate path
    /// calls it right after re-sending an ACK for `counter`, so a later close
    /// or timer does not send a second one; chip clears its pending flag the
    /// same way (`ReliableMessageContext.cpp:120-153`).
    ///
    /// The reclaim goes one step past chip, which leaves the exchange open.
    /// It is the reclaim [`Self::handle_timeout`] already does when the
    /// [`MrpConfig::standalone_ack_deadline`] timer sends a buffered ACK, and
    /// it carries the same close-invariant risk (see
    /// [`Self::flush_exchange_acks`]): if a duplicate of a mid-exchange
    /// message arrives before our follow-up on an exchange we started, the
    /// follow-up goes out on a re-created, peer-initiated exchange with
    /// `I=0`. That is acceptable for three reasons:
    /// - This path runs only while the ACK is still buffered, which is
    ///   before the timer would have done the same reclaim. It brings the
    ///   timer's hazard forward rather than adding a new one.
    /// - An IO layer that sends the follow-up in the same turn as it handles
    ///   the message that prompts it never hits this.
    /// - Without the reclaim, a caller that never finishes the exchange
    ///   would leave an idle entry in the exchange table for good.
    pub(crate) fn forget_ack_for(&mut self, counter: MessageCounter) {
        let mut exchanges: Vec<ExchangeKey> = self
            .pending_outbound_acks
            .iter()
            .filter(|(_, p)| p.ack_counter == counter)
            .map(|(exchange, _)| *exchange)
            .collect();
        for exchange in &exchanges {
            self.pending_outbound_acks.remove(exchange);
        }
        exchanges.extend(
            self.displaced_acks
                .iter()
                .filter(|(_, p)| p.ack_counter == counter)
                .map(|(owed_on, _)| *owed_on),
        );
        self.displaced_acks
            .retain(|(_, p)| p.ack_counter != counter);
        for exchange in exchanges {
            self.reclaim_if_idle(exchange);
        }
    }

    /// Returns `true` if `exchange` has no live work attached: no pending
    /// retransmit, no buffered outbound (piggyback) ack and no displaced ACK.
    /// Such an exchange is genuinely complete and its table entry can be
    /// reclaimed. Work on the other exchange with the same id does not count.
    ///
    /// `recent_reliable` entries are intentionally NOT consulted: they are a
    /// fixed-size ring buffer (bounded memory) whose dedup-resend semantics
    /// must outlive the exchange, exactly as [`Self::close_exchange`]
    /// preserves them.
    fn exchange_is_idle(&self, exchange: ExchangeKey) -> bool {
        let has_pending_retransmit = self.pending_acks.values().any(|p| p.exchange == exchange);
        let has_buffered_ack = self.pending_outbound_acks.contains_key(&exchange)
            || self
                .displaced_acks
                .iter()
                .any(|(owed_on, _)| *owed_on == exchange);
        !has_pending_retransmit && !has_buffered_ack
    }

    /// Reclaim `exchange`'s table entry if it is now idle (see
    /// [`Self::exchange_is_idle`]). Called at each point where an exchange's
    /// last piece of live work clears (an ack is received, a retransmit
    /// expires, a buffered or displaced ack is flushed as a standalone ack or
    /// dropped by the duplicate path, or an unreliable message — which
    /// nothing will ack — is sent), so completed exchanges
    /// are evicted automatically rather than depending on a caller invoking
    /// [`Self::close_exchange`].
    fn reclaim_if_idle(&mut self, exchange: ExchangeKey) {
        if self.exchange_is_idle(exchange) {
            self.exchanges.remove(&exchange);
        }
    }

    /// How many exchanges the table holds, for tests outside this module
    /// that check a code path neither leaks nor drops an entry.
    #[cfg(test)]
    pub(crate) fn exchange_count(&self) -> usize {
        self.exchanges.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::error::Error;

    /// The spec backoff shape: margin, threshold, growth, truncation.
    ///
    /// Written against hand-computed values rather than re-derived from the
    /// config, so a later change to the formula has to update it deliberately.
    /// Note the first TWO entries are equal — that is `MRP_BACKOFF_THRESHOLD`,
    /// and it is the easiest part of the formula to drop by accident.
    #[test]
    fn retransmit_delay_follows_the_spec_backoff_shape() {
        let cfg = MrpConfig::default();
        // Idle base 500 ms → 500 × 1127/1024 = 550 ms after margin, then
        // × 1.6^{0,0,1,2,3}, truncated per step. Sums to 5.640 s.
        for (sends_done, want_ms) in [(0, 550), (1, 550), (2, 880), (3, 1408), (4, 2252)] {
            assert_eq!(
                cfg.retransmit_delay(sends_done, false),
                Duration::from_millis(want_ms),
                "idle base, {sends_done} sends done"
            );
        }
        // Active base 300 ms → 330 ms after margin.
        for (sends_done, want_ms) in [(0, 330), (1, 330), (2, 528), (3, 844), (4, 1351)] {
            assert_eq!(
                cfg.retransmit_delay(sends_done, true),
                Duration::from_millis(want_ms),
                "active base, {sends_done} sends done"
            );
        }
    }

    /// `MrpConfig::default()` with jitter switched off, for the many tests that
    /// pin exact retransmit deadlines. Jitter only ever lengthens a delay, and
    /// its magnitude is a spec parameter rather than hidden state, so zeroing it
    /// is an honest way to make timing deterministic — the jitter behaviour
    /// itself is covered by its own tests below.
    fn cfg() -> MrpConfig {
        MrpConfig {
            backoff_jitter: 0.0,
            ..MrpConfig::default()
        }
    }

    /// `Default` and "the peer told us nothing" must be the same thing.
    ///
    /// They were not: `Default` carried a 4200 ms idle base that appears in no
    /// version of the Matter specification, in no part of connectedhomeip and
    /// in no part of matter.js, while `for_peer(None, None, None)` twelve lines
    /// below correctly used the spec's 500 ms. Two sessions to the same device
    /// could therefore have retransmit windows 8.4x apart with nothing in the
    /// logs to say which. This test makes that class of drift impossible.
    #[test]
    fn default_matches_spec_defaults() {
        assert_eq!(MrpConfig::default(), MrpConfig::for_peer(None, None, None));
    }

    /// Merging over `Default` must be indistinguishable from `for_peer` with
    /// the same three values — the responder role's whole use of this method.
    /// If these ever diverge, a responder-role session and an initiator-role
    /// session to the same peer get different windows for no stated reason.
    #[test]
    fn merge_over_default_equals_for_peer() {
        let params = matter_crypto::SessionParameters::new()
            .with_session_idle_interval_ms(3300)
            .with_session_active_interval_ms(1100)
            .with_session_active_threshold_ms(4000);
        assert_eq!(
            MrpConfig::default().merge_session_params(&params),
            MrpConfig::for_peer(
                Some(Duration::from_millis(3300)),
                Some(Duration::from_millis(1100)),
                Some(Duration::from_secs(4)),
            )
        );
    }

    /// An empty advertisement must change nothing. "The peer sent no
    /// parameters" and "the peer sent the spec defaults" are different facts;
    /// only the second should overwrite a discovery-derived config.
    #[test]
    fn merging_an_empty_advertisement_is_a_no_op() {
        let discovered = MrpConfig::for_peer(
            Some(Duration::from_millis(3300)),
            Some(Duration::from_millis(1100)),
            Some(Duration::from_secs(4)),
        );
        assert_eq!(
            discovered.merge_session_params(&matter_crypto::SessionParameters::new()),
            discovered
        );
    }

    /// This is the chip-vs-matter.js divergence, pinned. A peer sending only
    /// `SII` must leave the mDNS-derived `SAI`/`SAT` intact (chip's field-by-
    /// field decode), NOT reset them to the spec defaults (matter.js's
    /// wholesale `set timingParameters`).
    #[test]
    fn a_partial_advertisement_leaves_unsent_fields_alone() {
        let discovered = MrpConfig::for_peer(
            Some(Duration::from_millis(3300)),
            Some(Duration::from_millis(1100)),
            Some(Duration::from_millis(300)),
        );
        let merged = discovered.merge_session_params(
            &matter_crypto::SessionParameters::new().with_session_idle_interval_ms(1800),
        );
        assert_eq!(merged.initial_idle, Duration::from_millis(1800));
        assert_eq!(
            merged.initial_active,
            Duration::from_millis(1100),
            "SAI was not advertised in Sigma2; the mDNS value must survive"
        );
        assert_eq!(
            merged.idle_threshold,
            Duration::from_millis(300),
            "SAT was not advertised in Sigma2; the mDNS value must survive"
        );
    }

    /// Peer-supplied values are clamped to the same spec bounds `for_peer`
    /// applies. Neither chip nor matter.js clamps these (matter.js documents
    /// them as "accepted unbounded"), so this is a deliberate hardening: an
    /// advertised interval is attacker-influencable input on a hostile LAN.
    #[test]
    fn merged_values_are_clamped_to_spec_bounds() {
        let merged = MrpConfig::default().merge_session_params(
            &matter_crypto::SessionParameters::new()
                .with_session_idle_interval_ms(u32::MAX)
                .with_session_active_threshold_ms(u16::MAX),
        );
        assert_eq!(merged.initial_idle, Duration::from_secs(3600));
        assert_eq!(merged.idle_threshold, Duration::from_millis(65_535));
    }

    /// Retransmit shape is never peer-controlled.
    #[test]
    fn merge_never_touches_the_retransmit_shape() {
        let base = MrpConfig::default();
        let merged = base.merge_session_params(
            &matter_crypto::SessionParameters::new().with_session_idle_interval_ms(1800),
        );
        assert_eq!(merged.max_transmissions, base.max_transmissions);
        // Compare the float fields bitwise: they are carried through verbatim,
        // never recomputed, so bit equality is the exact property under test
        // and it keeps `clippy::float_cmp` satisfied.
        assert_eq!(
            merged.backoff_factor.to_bits(),
            base.backoff_factor.to_bits()
        );
        assert_eq!(
            merged.backoff_jitter.to_bits(),
            base.backoff_jitter.to_bits()
        );
        assert_eq!(merged.standalone_ack_deadline, base.standalone_ack_deadline);
    }

    /// State the resulting window somewhere a reader will find it.
    ///
    /// 550 + 550 + 880 + 1408 + 2252 = 5640 ms to `Expired` for a peer that
    /// advertises nothing. It was 13.1 s before the transmission count and the
    /// backoff terms were corrected, and 110.4 s on any path that took the old
    /// `Default`.
    #[test]
    fn the_spec_default_total_window_is_5640ms() {
        let cfg = MrpConfig::default();
        let total: Duration = (0..cfg.max_transmissions)
            .map(|n| cfg.retransmit_delay(n, false))
            .sum();
        assert_eq!(total, Duration::from_millis(5640));
    }

    /// Jitter only ever LENGTHENS, and never past the spec's bound.
    ///
    /// chip's form is `(1024 + rand_u8) / 1024`, i.e. a multiplier in
    /// `[1.0, 1.2490]`. A jitter that could shorten a delay would let us
    /// retransmit at a sleepy device sooner than its advertised interval allows
    /// -- the exact thing the peer-sized config exists to prevent.
    #[test]
    fn jitter_only_lengthens_and_stays_within_the_spec_bound() {
        let config = MrpConfig::default();
        let base = Duration::from_millis(1000);
        let mut state = 0x1234_5678_9ABC_DEF0;
        for _ in 0..1000 {
            let got = jitter_with(&mut state, &config, base);
            assert!(got >= base, "jitter must never shorten: {got:?} < {base:?}");
            assert!(
                got < base.mul_f32(1.0 + MRP_BACKOFF_JITTER),
                "jitter must stay under the spec bound: {got:?}"
            );
        }
    }

    /// A zero `backoff_jitter` must be exactly inert -- this is what lets the
    /// timing tests above pin exact deadlines.
    #[test]
    fn zero_jitter_is_inert() {
        let config = MrpConfig {
            backoff_jitter: 0.0,
            ..MrpConfig::default()
        };
        let base = Duration::from_millis(4622);
        let mut state = 1;
        for _ in 0..100 {
            assert_eq!(jitter_with(&mut state, &config, base), base);
        }
    }

    /// Jitter must actually vary, or it is not de-synchronising anything.
    #[test]
    fn jitter_varies_across_draws() {
        let config = MrpConfig::default();
        let base = Duration::from_millis(1000);
        let mut state = 42;
        let seen: std::collections::HashSet<Duration> = (0..100)
            .map(|_| jitter_with(&mut state, &config, base))
            .collect();
        assert!(
            seen.len() > 10,
            "jitter barely varies: {} distinct",
            seen.len()
        );
    }

    /// A reliable message is transmitted exactly `MRP_MAX_TRANSMISSIONS` times.
    ///
    /// Spec §4.12.8 puts that at 5 -- one original send plus four retransmits.
    /// We used to send six: `mark_packet_sent` seeded `attempts_remaining` with
    /// the full budget even though the caller had already sent once, so the
    /// original transmission was never counted against it.
    #[test]
    fn a_reliable_message_is_sent_exactly_max_transmissions_times() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        let prepared = mrp
            .prepare_outbound_local(
                0x02,
                crate::protocol_header::ProtocolId::INTERACTION_MODEL,
                b"x",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        // Transmission 1: the caller's own send.
        mrp.mark_packet_sent(
            MessageCounter(1),
            prepared.exchange_id,
            vec![1u8; 8],
            true,
            now,
        );

        let mut transmissions = 1;
        let mut expired = false;
        for _ in 0..20 {
            let Some(sim) = mrp.poll_timeout() else { break };
            for e in &mrp.handle_timeout(sim) {
                match e {
                    MrpTimerEvent::Retransmit { .. } => transmissions += 1,
                    MrpTimerEvent::Expired { .. } => expired = true,
                    MrpTimerEvent::StandaloneAckDeadlineFired { .. } => {}
                }
            }
            if expired {
                break;
            }
        }

        assert!(expired, "the message must eventually expire");
        assert_eq!(
            transmissions,
            u32::from(MRP_MAX_TRANSMISSIONS),
            "spec §4.12.8 allows exactly MRP_MAX_TRANSMISSIONS sends, original included"
        );
    }

    /// Exchange id used by tests that would previously have passed `None` and
    /// let `MrpState` allocate. Allocation now lives on `SessionManager` — one
    /// counter per initiator *node*, as Matter Core Spec §4.10.2 requires — so
    /// these tests name the id explicitly. Value 1 preserves the ids the
    /// pre-existing assertions were written against.
    const TEST_LOCAL_EXCHANGE: u16 = 1;

    impl MrpState {
        /// Whether the exchange table holds the exchange `id` with this role.
        fn holds(&self, id: u16, is_local_initiator: bool) -> bool {
            self.exchanges.contains(&ExchangeKey {
                id,
                is_local_initiator,
            })
        }

        /// Test shim reproducing the old `prepare_outbound(.., None, ..)`
        /// behaviour: mark a locally-initiated exchange, then prepare on it.
        ///
        /// It notes the exchange only when the table does not hold it (the
        /// first send, or a send after the exchange was reclaimed). A later
        /// send on a live exchange reads its `I` flag back from the table,
        /// as `encode_outbound` does, so a bug that records the wrong flag
        /// shows up here instead of being overwritten by a fresh note.
        fn prepare_outbound_local(
            &mut self,
            opcode: u8,
            protocol_id: ProtocolId,
            app_payload: &[u8],
            mrp_flags: MrpFlags,
            now: Instant,
        ) -> Result<PreparedOutbound> {
            if !self.holds(TEST_LOCAL_EXCHANGE, true) {
                self.note_local_exchange(TEST_LOCAL_EXCHANGE);
            }
            self.prepare_outbound(
                opcode,
                protocol_id,
                TEST_LOCAL_EXCHANGE,
                app_payload,
                mrp_flags,
                now,
            )
        }
    }
    use crate::protocol_header::{decode_protocol_header, ExchangeFlags};
    use std::time::Duration;

    fn t0() -> Instant {
        // Use a fixed reference Instant for all simulated-clock tests.
        Instant::now()
    }

    #[test]
    fn unreliable_send_no_pending_entry() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        let prepared = mrp
            .prepare_outbound_local(
                0x20,
                crate::protocol_header::ProtocolId::INTERACTION_MODEL,
                b"hello",
                MrpFlags { reliable: false },
                now,
            )
            .unwrap();

        assert!(prepared.is_local_initiator);
        assert_eq!(prepared.exchange_id, 1, "first allocated exchange_id");
        assert!(!prepared.piggyback_acked);
        // No pending_acks even after mark_packet_sent because R=0.
        mrp.mark_packet_sent(
            MessageCounter(1),
            prepared.exchange_id,
            vec![0u8; 16],
            false,
            now,
        );
        assert_eq!(mrp.poll_timeout(), None);
    }

    #[test]
    fn reliable_send_schedules_retransmit() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        // Peer is active (it sent us something just now) → active base (300 ms).
        mrp.last_peer_activity = Some(now);

        let prepared = mrp
            .prepare_outbound_local(
                0x02,
                crate::protocol_header::ProtocolId::INTERACTION_MODEL,
                b"read attr",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        mrp.mark_packet_sent(
            MessageCounter(1),
            prepared.exchange_id,
            vec![0xAAu8; 24],
            true,
            now,
        );

        let deadline = mrp.poll_timeout().expect("retransmit deadline scheduled");
        // Active base 300 ms carries the spec's 1.1 margin: 300 × 1127/1024.
        assert_eq!(deadline, now + Duration::from_millis(330));
    }

    #[test]
    fn handle_timeout_emits_retransmit_at_deadline() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        // Peer active → active base (300 ms) for the first retransmit.
        mrp.last_peer_activity = Some(now);
        let prepared = mrp
            .prepare_outbound_local(
                0x02,
                crate::protocol_header::ProtocolId::INTERACTION_MODEL,
                b"read",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        let packet = vec![0xAAu8; 24];
        mrp.mark_packet_sent(
            MessageCounter(1),
            prepared.exchange_id,
            packet.clone(),
            true,
            now,
        );

        // Before the deadline — no events.
        assert!(mrp
            .handle_timeout(now + Duration::from_millis(200))
            .is_empty());

        // At the deadline (active base 300 ms + margin) — retransmit.
        let events = mrp.handle_timeout(now + Duration::from_millis(330));
        assert_eq!(events.len(), 1);
        match &events[0] {
            MrpTimerEvent::Retransmit {
                exchange_id,
                counter,
                packet: bytes,
            } => {
                assert_eq!(*exchange_id, prepared.exchange_id);
                assert_eq!(*counter, MessageCounter(1));
                assert_eq!(*bytes, packet);
            }
            other => panic!("expected Retransmit, got {other:?}"),
        }

        // Second transmission shares the base interval (MRP_BACKOFF_THRESHOLD),
        // so the next deadline is another 330 ms on, not 330 × 1.6.
        let next = mrp.poll_timeout().unwrap();
        assert_eq!(next, now + Duration::from_millis(330 + 330));
    }

    #[test]
    fn exhausted_retransmit_emits_expired() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        let prepared = mrp
            .prepare_outbound_local(
                0x02,
                crate::protocol_header::ProtocolId::INTERACTION_MODEL,
                b"x",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        mrp.mark_packet_sent(
            MessageCounter(1),
            prepared.exchange_id,
            vec![1u8; 8],
            true,
            now,
        );

        // MRP_MAX_TRANSMISSIONS is 5 and the caller already sent once, so there
        // are FOUR retransmits, at 330, 330, 528, 844 ms after the previous
        // attempt. This used to be five -- one send plus five retransmits, i.e.
        // six transmissions where the spec allows five.
        let mut total_retransmits = 0;
        for _ in 0..4 {
            let sim = mrp.poll_timeout().expect("timer scheduled");
            let events = mrp.handle_timeout(sim);
            for e in &events {
                if matches!(e, MrpTimerEvent::Retransmit { .. }) {
                    total_retransmits += 1;
                }
            }
        }
        assert_eq!(
            total_retransmits,
            i32::from(MRP_MAX_TRANSMISSIONS) - 1,
            "one original send plus max_transmissions-1 retransmits"
        );

        // Fifth firing: Expired.
        let sim = mrp.poll_timeout().expect("expiry timer scheduled");
        let events = mrp.handle_timeout(sim);
        assert!(matches!(
            events[0],
            MrpTimerEvent::Expired { exchange_id, counter }
            if exchange_id == prepared.exchange_id && counter == MessageCounter(1)
        ));
        assert_eq!(mrp.poll_timeout(), None, "no more pending after Expired");
    }

    /// Helper: queue one reliable message and return its scheduled first
    /// retransmit deadline.
    fn schedule_reliable(mrp: &mut MrpState, counter: u32, now: Instant) -> Instant {
        let p = mrp
            .prepare_outbound_local(
                0x02,
                crate::protocol_header::ProtocolId::INTERACTION_MODEL,
                b"x",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        mrp.mark_packet_sent(
            MessageCounter(counter),
            p.exchange_id,
            vec![1u8; 4],
            true,
            now,
        );
        mrp.poll_timeout().expect("retransmit deadline scheduled")
    }

    /// A fresh session (we have never heard from the peer) is IDLE, so
    /// the first retransmit uses the slow idle base (SII) — we never open with
    /// active-interval spacing at a device that might be a sleepy ICD. Once the
    /// peer sends us something (within SAT), the session is active (SAI).
    #[test]
    fn classification_follows_peer_activity_not_our_tx() {
        let now = t0();

        // Fresh session, no peer inbound → idle base (500 ms + 1.1 margin).
        let mut fresh = MrpState::new(cfg());
        assert_eq!(
            schedule_reliable(&mut fresh, 1, now),
            now + Duration::from_millis(550),
            "a peer we have never heard from is idle (SII), regardless of our tx timing",
        );

        // Peer sent us something within SAT → active base (300 ms + margin).
        let mut active = MrpState::new(cfg());
        active.last_peer_activity = Some(now);
        assert_eq!(
            schedule_reliable(&mut active, 1, now),
            now + Duration::from_millis(330),
            "a peer active within SAT uses the active base (SAI)",
        );

        // Peer last spoke longer ago than SAT (idle_threshold, 4 s) → idle base.
        let mut aged = MrpState::new(cfg());
        aged.last_peer_activity = Some(now);
        let later = now + Duration::from_secs(6);
        assert_eq!(
            schedule_reliable(&mut aged, 1, later),
            later + Duration::from_millis(550),
            "a peer silent past SAT is idle (SII) again",
        );
    }

    /// The core rule: the active/idle base is re-evaluated on EVERY retransmit
    /// (chip `GetMRPBaseTimeout`), not fixed at send time. A message queued
    /// while the peer was active must switch to the idle base once the peer
    /// goes silent past SAT — the CASE-Sigma3-to-a-sleepy-device failure mode.
    #[test]
    fn retransmit_reevaluates_peer_active_state() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        // Peer active at send → first retransmit at the active base
        // (300 ms + 1.1 margin).
        mrp.last_peer_activity = Some(now);
        let d0 = schedule_reliable(&mut mrp, 1, now);
        assert_eq!(d0, now + Duration::from_millis(330));

        // Fire the first retransmit while the peer is STILL active (t+330ms,
        // within SAT). The second transmission shares the base interval
        // (MRP_BACKOFF_THRESHOLD), so backoff has not started growing yet.
        let ev = mrp.handle_timeout(d0);
        assert_eq!(ev.len(), 1);
        let d1 = mrp.poll_timeout().unwrap();
        assert_eq!(
            d1,
            d0 + Duration::from_millis(330), // 330 * 1.6^0, threshold not passed
            "still-active peer: active base, growth not yet started",
        );

        // Now the peer has gone silent well past SAT (4 s). The NEXT retransmit
        // must switch to the idle base (500 ms) × its backoff, not stay on the
        // active grid — this is the anti-hammer guarantee.
        let much_later = d1.max(now + Duration::from_secs(7));
        mrp.handle_timeout(much_later);
        let d2 = mrp.poll_timeout().unwrap();
        // sends_done is now 2 → idle base with margin (550) × 1.6^(2-1).
        assert_eq!(
            d2,
            much_later + Duration::from_millis(880),
            "peer silent past SAT: retransmit re-evaluates to the idle base (SII)",
        );
    }

    /// An exchange noted once as ours keeps `I=1` on every message
    /// `prepare_outbound` builds on it, with no `note_local_exchange` before
    /// each send: the first send records the exchange with the flag it was
    /// built with, and later sends read it back from the table.
    #[test]
    fn prepare_outbound_keeps_the_initiator_flag_without_re_noting() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        mrp.note_local_exchange(0x0700);
        for send in 0..3 {
            let prepared = mrp
                .prepare_outbound(
                    0x08,
                    crate::protocol_header::ProtocolId::INTERACTION_MODEL,
                    0x0700,
                    b"data",
                    MrpFlags { reliable: true },
                    now,
                )
                .unwrap();
            assert!(prepared.is_local_initiator, "send {send}");
            let (header, _) = decode_protocol_header(&prepared.wire_payload).unwrap();
            assert!(
                header.exchange_flags.contains(ExchangeFlags::INITIATOR),
                "send {send}: header must carry I=1"
            );
        }
    }

    #[test]
    fn prepared_payload_contains_encoded_header() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        let prepared = mrp
            .prepare_outbound_local(
                0x02,
                crate::protocol_header::ProtocolId::INTERACTION_MODEL,
                b"data",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();

        // The wire_payload starts with the encoded ProtocolHeader.
        let (header, tail) = decode_protocol_header(&prepared.wire_payload).unwrap();
        assert!(header.exchange_flags.contains(ExchangeFlags::INITIATOR));
        assert!(header.exchange_flags.contains(ExchangeFlags::RELIABLE));
        assert_eq!(header.exchange_id, prepared.exchange_id);
        assert_eq!(tail, b"data");
    }

    use crate::protocol_header::opcode;

    fn build_inbound_payload(
        flags: ExchangeFlags,
        opcode_value: u8,
        exchange_id: u16,
        ack_counter: Option<MessageCounter>,
        app_tail: &[u8],
    ) -> Vec<u8> {
        let header = ProtocolHeader {
            exchange_flags: flags,
            opcode: opcode_value,
            exchange_id,
            protocol_id: ProtocolId::INTERACTION_MODEL,
            ack_counter,
        };
        let mut out = Vec::new();
        encode_protocol_header(&header, &mut out);
        out.extend_from_slice(app_tail);
        out
    }

    /// An inbound from the peer marks it active, so a reliable send that
    /// follows uses the active base — proving `process_inbound` feeds the
    /// classification (it is the real hook, not the test-only field poke).
    #[test]
    fn process_inbound_marks_peer_active() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        // Before any inbound: fresh session is idle.
        let mut probe = MrpState::new(cfg());
        assert_eq!(
            schedule_reliable(&mut probe, 1, now),
            now + Duration::from_millis(550),
        );
        // After a real inbound: peer is active.
        let payload = build_inbound_payload(ExchangeFlags::INITIATOR, 0x02, 0x0001, None, b"hi");
        let _ = mrp
            .process_inbound(payload, MessageCounter(50), now)
            .unwrap();
        assert_eq!(
            schedule_reliable(&mut mrp, 1, now),
            now + Duration::from_millis(330),
            "a send right after a peer inbound uses the active base",
        );
    }

    /// `MrpConfig::for_peer` maps advertised `SII`/`SAI`/`SAT`, falls
    /// back to the Matter spec defaults for omitted keys, and clamps
    /// out-of-range values to their spec upper bounds.
    #[test]
    fn for_peer_maps_txt_defaults_and_clamps() {
        // All three supplied, in range.
        let c = MrpConfig::for_peer(
            Some(Duration::from_secs(6)),  // SII
            Some(Duration::from_secs(1)),  // SAI
            Some(Duration::from_secs(30)), // SAT
        );
        assert_eq!(c.initial_idle, Duration::from_secs(6));
        assert_eq!(c.initial_active, Duration::from_secs(1));
        assert_eq!(c.idle_threshold, Duration::from_secs(30));
        // Retransmit shape stays at the local defaults (not peer-controlled).
        assert_eq!(c.max_transmissions, MrpConfig::default().max_transmissions);
        assert!(
            (c.backoff_factor - MrpConfig::default().backoff_factor).abs() < f32::EPSILON,
            "backoff factor stays at the local default",
        );

        // None → spec defaults (SII 500, SAI 300, SAT 4000).
        let d = MrpConfig::for_peer(None, None, None);
        assert_eq!(d.initial_idle, Duration::from_millis(500));
        assert_eq!(d.initial_active, Duration::from_millis(300));
        assert_eq!(d.idle_threshold, Duration::from_secs(4));

        // Over-range → clamped (SII/SAI ≤ 3_600_000 ms, SAT ≤ 65_535 ms).
        let e = MrpConfig::for_peer(
            Some(Duration::from_secs(9999)),
            Some(Duration::from_secs(9999)),
            Some(Duration::from_secs(9999)),
        );
        assert_eq!(e.initial_idle, Duration::from_secs(3600));
        assert_eq!(e.initial_active, Duration::from_secs(3600));
        assert_eq!(e.idle_threshold, Duration::from_millis(65_535));
    }

    #[test]
    fn process_inbound_app_message_delivers_payload() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        let payload = build_inbound_payload(
            ExchangeFlags::INITIATOR, // peer initiated this exchange
            0x02,
            0x4242,
            None,
            b"hello from peer",
        );
        let outcome = mrp
            .process_inbound(payload, MessageCounter(100), now)
            .unwrap();
        match outcome {
            InboundOutcome::AppMessage {
                exchange_id,
                is_initiator,
                protocol_id,
                opcode,
                payload,
            } => {
                assert_eq!(exchange_id, 0x4242);
                assert!(is_initiator, "peer set I=1 → peer is the initiator");
                assert_eq!(protocol_id, ProtocolId::INTERACTION_MODEL);
                assert_eq!(opcode, 0x02);
                assert_eq!(payload, b"hello from peer");
            }
            other => panic!("expected AppMessage, got {other:?}"),
        }
    }

    #[test]
    fn process_inbound_ack_clears_pending() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        // Send a reliable outbound first.
        let prepared = mrp
            .prepare_outbound_local(
                0x02,
                ProtocolId::INTERACTION_MODEL,
                b"x",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        mrp.mark_packet_sent(
            MessageCounter(1),
            prepared.exchange_id,
            vec![0xAAu8; 8],
            true,
            now,
        );
        assert!(mrp.poll_timeout().is_some());

        // Peer sending standalone-ack to us — they did NOT initiate our exchange. I=0 for them.
        let ack_payload = build_inbound_payload(
            ExchangeFlags::ACK,
            opcode::secure_channel::STANDALONE_ACK,
            prepared.exchange_id,
            Some(MessageCounter(1)),
            &[],
        );
        let outcome = mrp
            .process_inbound(ack_payload, MessageCounter(50), now)
            .unwrap();
        match outcome {
            InboundOutcome::AckOnly {
                exchange_id,
                acked_counter,
            } => {
                assert_eq!(exchange_id, prepared.exchange_id);
                assert_eq!(acked_counter, MessageCounter(1));
            }
            other => panic!("expected AckOnly, got {other:?}"),
        }
        assert_eq!(mrp.poll_timeout(), None, "pending_acks cleared");
    }

    #[test]
    fn reliable_inbound_queues_piggyback_ack() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        let payload = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0x4242,
            None,
            b"reliable inbound",
        );
        let _ = mrp
            .process_inbound(payload, MessageCounter(100), now)
            .unwrap();

        // 200ms standalone-ack deadline should be the poll deadline now.
        assert_eq!(
            mrp.poll_timeout().unwrap(),
            now + Duration::from_millis(200),
        );
    }

    #[test]
    fn piggyback_drained_by_next_outbound() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        // Inbound reliable to queue the piggyback.
        let payload = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0x4242,
            None,
            b"in",
        );
        mrp.process_inbound(payload, MessageCounter(100), now)
            .unwrap();
        assert!(mrp.poll_timeout().is_some());

        // Next outbound in the SAME exchange — should piggyback the ack.
        let prepared = mrp
            .prepare_outbound(
                0x10,
                ProtocolId::SECURE_CHANNEL,
                0x4242,
                b"",
                MrpFlags::default(),
                now + Duration::from_millis(50),
            )
            .unwrap();

        assert!(prepared.piggyback_acked);
        // The encoded header should have A=1 with ack_counter = 100.
        let (header, _) =
            crate::protocol_header::decode_protocol_header(&prepared.wire_payload).unwrap();
        assert!(header.exchange_flags.contains(ExchangeFlags::ACK));
        assert_eq!(header.ack_counter, Some(MessageCounter(100)));

        // Piggyback consumed → no standalone deadline pending.
        assert_eq!(mrp.poll_timeout(), None);
    }

    #[test]
    fn standalone_ack_deadline_fires_after_200ms() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        let payload = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0x4242,
            None,
            b"in",
        );
        mrp.process_inbound(payload, MessageCounter(100), now)
            .unwrap();

        // Before deadline.
        assert!(mrp
            .handle_timeout(now + Duration::from_millis(199))
            .is_empty());

        // At deadline.
        let events = mrp.handle_timeout(now + Duration::from_millis(200));
        assert_eq!(events.len(), 1);
        match &events[0] {
            MrpTimerEvent::StandaloneAckDeadlineFired {
                exchange_id,
                ack_counter,
                is_local_initiator,
            } => {
                assert_eq!(*exchange_id, 0x4242);
                assert_eq!(*ack_counter, MessageCounter(100));
                // Peer set I=1, so peer is initiator, we are responder.
                assert!(!*is_local_initiator);
            }
            other => panic!("expected StandaloneAckDeadlineFired, got {other:?}"),
        }
        // Deadline drains; no more pending.
        assert_eq!(mrp.poll_timeout(), None);
    }

    #[test]
    fn caller_provided_exchange_id_responder_side() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        // Peer initiates exchange 0x99 by sending us a reliable message.
        let inbound = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0x99,
            None,
            b"req",
        );
        mrp.process_inbound(inbound, MessageCounter(100), now)
            .unwrap();

        // We respond in the same exchange. We are NOT the initiator.
        let prepared = mrp
            .prepare_outbound(
                0x03,
                ProtocolId::INTERACTION_MODEL,
                0x99,
                b"response",
                MrpFlags { reliable: true },
                now + Duration::from_millis(10),
            )
            .unwrap();
        assert_eq!(prepared.exchange_id, 0x99);
        assert!(!prepared.is_local_initiator);

        let (header, _) =
            crate::protocol_header::decode_protocol_header(&prepared.wire_payload).unwrap();
        assert!(
            !header.exchange_flags.contains(ExchangeFlags::INITIATOR),
            "we are responder for this exchange"
        );
        assert!(header.exchange_flags.contains(ExchangeFlags::RELIABLE));
        assert!(
            header.exchange_flags.contains(ExchangeFlags::ACK),
            "piggyback drained"
        );
        assert_eq!(header.ack_counter, Some(MessageCounter(100)));
    }

    #[test]
    fn close_exchange_drops_pending() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        let prepared = mrp
            .prepare_outbound_local(
                0x02,
                ProtocolId::INTERACTION_MODEL,
                b"x",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        mrp.mark_packet_sent(
            MessageCounter(1),
            prepared.exchange_id,
            vec![1u8; 4],
            true,
            now,
        );
        assert!(mrp.poll_timeout().is_some());

        mrp.close_exchange(prepared.exchange_id);
        assert_eq!(
            mrp.poll_timeout(),
            None,
            "closing exchange dropped pending retransmit"
        );
    }

    #[test]
    fn duplicate_reliable_returns_view() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        // Receive a reliable inbound.
        let inbound = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0x4242,
            None,
            b"first",
        );
        mrp.process_inbound(inbound, MessageCounter(100), now)
            .unwrap();

        // Same counter again — the replay window in the wider system
        // would normally reject; in this test we directly query MRP's
        // recent-reliable cache.
        let view = mrp.check_duplicate_reliable(MessageCounter(100)).unwrap();
        assert_eq!(view.exchange_id, 0x4242);
        assert!(!view.is_local_initiator, "peer was initiator");
    }

    #[test]
    fn concurrent_exchanges_each_retain_their_pending_ack() {
        // Regression guard: a reliable inbound on exchange B must NOT clobber
        // the buffered piggyback ack for exchange A. Both exchanges must
        // drain their own ack on the next outbound in that exchange.
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        // Reliable inbound on exchange A (peer counter 100).
        let in_a = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0xAAAA,
            None,
            b"req-a",
        );
        mrp.process_inbound(in_a, MessageCounter(100), now).unwrap();

        // Reliable inbound on exchange B (peer counter 200) BEFORE any
        // outbound or timeout drains A's ack.
        let in_b = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0xBBBB,
            None,
            b"req-b",
        );
        mrp.process_inbound(in_b, MessageCounter(200), now).unwrap();

        // Respond on exchange A — must piggyback A's ack (counter 100).
        let resp_a = mrp
            .prepare_outbound(
                0x03,
                ProtocolId::INTERACTION_MODEL,
                0xAAAA,
                b"resp-a",
                MrpFlags::default(),
                now + Duration::from_millis(10),
            )
            .unwrap();
        assert!(resp_a.piggyback_acked, "exchange A's ack must drain");
        let (hdr_a, _) =
            crate::protocol_header::decode_protocol_header(&resp_a.wire_payload).unwrap();
        assert!(hdr_a.exchange_flags.contains(ExchangeFlags::ACK));
        assert_eq!(hdr_a.ack_counter, Some(MessageCounter(100)));

        // Respond on exchange B — must piggyback B's ack (counter 200).
        let resp_b = mrp
            .prepare_outbound(
                0x03,
                ProtocolId::INTERACTION_MODEL,
                0xBBBB,
                b"resp-b",
                MrpFlags::default(),
                now + Duration::from_millis(10),
            )
            .unwrap();
        assert!(resp_b.piggyback_acked, "exchange B's ack must drain");
        let (hdr_b, _) =
            crate::protocol_header::decode_protocol_header(&resp_b.wire_payload).unwrap();
        assert!(hdr_b.exchange_flags.contains(ExchangeFlags::ACK));
        assert_eq!(hdr_b.ack_counter, Some(MessageCounter(200)));

        // Both drained → no standalone deadline pending.
        assert_eq!(mrp.poll_timeout(), None);
    }

    #[test]
    fn handle_timeout_flushes_all_due_standalone_acks() {
        // Regression guard: two buffered piggyback acks on different exchanges
        // must BOTH flush as standalone acks once their deadline passes.
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        let in_a = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0xAAAA,
            None,
            b"a",
        );
        mrp.process_inbound(in_a, MessageCounter(100), now).unwrap();

        let in_b = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0xBBBB,
            None,
            b"b",
        );
        mrp.process_inbound(in_b, MessageCounter(200), now).unwrap();

        // Advance past the 200ms standalone-ack deadline.
        let events = mrp.handle_timeout(now + Duration::from_millis(200));

        // Collect the (exchange_id, ack_counter) pairs from every
        // standalone-ack-deadline-fired event.
        let mut flushed: Vec<(u16, MessageCounter)> = events
            .iter()
            .filter_map(|e| match e {
                MrpTimerEvent::StandaloneAckDeadlineFired {
                    exchange_id,
                    ack_counter,
                    ..
                } => Some((*exchange_id, *ack_counter)),
                _ => None,
            })
            .collect();
        flushed.sort_by_key(|(xid, _)| *xid);

        assert_eq!(
            flushed,
            vec![(0xAAAA, MessageCounter(100)), (0xBBBB, MessageCounter(200)),],
            "both exchanges must flush a standalone ack"
        );

        // All drained → nothing pending.
        assert_eq!(mrp.poll_timeout(), None);
    }

    #[test]
    fn inbound_exchange_table_is_capped() {
        // Memory-DoS regression: a peer driving many distinct inbound
        // exchange_ids must not grow the exchange table without bound.
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        // Drive far more distinct exchange_ids than the cap. Use
        // non-reliable inbounds so no pending state lingers — each one is
        // a complete, idle exchange the moment it is processed.
        let n = u32::try_from(MAX_EXCHANGES_PER_SESSION).unwrap() + 500;
        for id in 0..n {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR,
                0x02,
                u16::try_from(id % 0xFFFF).unwrap(),
                None,
                b"x",
            );
            // Keep counters distinct so the replay/dedup paths stay sane.
            let _ = mrp.process_inbound(inbound, MessageCounter(id), now);
        }

        assert!(
            mrp.exchanges.len() <= MAX_EXCHANGES_PER_SESSION,
            "exchange table grew past the cap: {} > {}",
            mrp.exchanges.len(),
            MAX_EXCHANGES_PER_SESSION
        );
    }

    #[test]
    fn capped_new_inbound_exchange_is_rejected() {
        // Once the table is full of LIVE exchanges (each holding a buffered
        // outbound ack so it is not idle), a brand-new exchange_id must be
        // rejected rather than inserted.
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        // Fill the table with reliable inbounds: each leaves a buffered
        // piggyback ack, so the exchange counts as live and won't be
        // reclaimed.
        for id in 0..u32::try_from(MAX_EXCHANGES_PER_SESSION).unwrap() {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                u16::try_from(id).unwrap(),
                None,
                b"x",
            );
            mrp.process_inbound(inbound, MessageCounter(id), now)
                .unwrap();
        }
        assert_eq!(mrp.exchanges.len(), MAX_EXCHANGES_PER_SESSION);

        // A new exchange_id at the cap must be rejected.
        let new_id = u16::try_from(MAX_EXCHANGES_PER_SESSION).unwrap();
        let outcome = mrp.process_inbound(
            build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                new_id,
                None,
                b"x",
            ),
            MessageCounter(0xFFFF),
            now,
        );
        assert!(
            matches!(outcome, Err(Error::ExchangeTableFull)),
            "new exchange past the cap must be rejected, got {outcome:?}"
        );
        assert_eq!(
            mrp.exchanges.len(),
            MAX_EXCHANGES_PER_SESSION,
            "rejected exchange must not be inserted"
        );

        // An inbound on an ALREADY-KNOWN exchange must still be accepted
        // even when the table is full.
        let known = mrp.process_inbound(
            build_inbound_payload(ExchangeFlags::INITIATOR, 0x02, 0, None, b"y"),
            MessageCounter(0xF000),
            now,
        );
        assert!(
            known.is_ok(),
            "inbound on a known exchange must still be accepted at the cap"
        );
    }

    /// Peer-initiated reliable message answered by one UNRELIABLE message on
    /// the same exchange — what the controller does for every steady-state
    /// subscription `ReportData`. The unreliable send drains the buffered ack
    /// and nothing will ever ack it back, so the send itself is the exchange's
    /// last event and must reclaim it. Before the fix the entry leaked, and
    /// report #257 on the session was rejected with `ExchangeTableFull`
    /// (`WeaveHome`: every subscription went deaf after 256 reports).
    #[test]
    fn unreliable_answer_to_peer_exchange_reclaims_it() {
        let mut mrp = MrpState::new(cfg());
        let mut now = t0();
        let n = u32::try_from(MAX_EXCHANGES_PER_SESSION).unwrap() + 44;
        for i in 0..n {
            let exch = 40_000u16.wrapping_add(u16::try_from(i).unwrap());
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x05,
                exch,
                None,
                b"report",
            );
            mrp.process_inbound(inbound, MessageCounter(1000 + i), now)
                .unwrap_or_else(|e| panic!("report #{} rejected: {e:?}", i + 1));
            let p = mrp
                .prepare_outbound(
                    0x01,
                    ProtocolId::INTERACTION_MODEL,
                    exch,
                    b"status",
                    MrpFlags { reliable: false },
                    now,
                )
                .unwrap();
            assert!(p.piggyback_acked, "the answer must carry the buffered ack");
            mrp.mark_packet_sent(MessageCounter(i), exch, Vec::new(), false, now);
            assert_eq!(
                mrp.exchanges.len(),
                0,
                "report #{}: an unreliably-answered exchange must be reclaimed",
                i + 1
            );
            now += Duration::from_secs(600);
            let _ = mrp.handle_timeout(now);
        }
    }

    /// The same leak via the late path: the 200 ms standalone-ack deadline
    /// fires first (flushing the ack and reclaiming the exchange), and only
    /// then does the application send its unreliable answer. `prepare_outbound`
    /// re-creates the entry with nothing to drain, so reclaiming on the drain
    /// alone would still leak — the reclaim has to hang off the unreliable send.
    #[test]
    fn late_unreliable_answer_after_standalone_ack_reclaims_exchange() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        mrp.process_inbound(
            build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x05,
                0x1234,
                None,
                b"report",
            ),
            MessageCounter(7),
            now,
        )
        .unwrap();
        let later = now + Duration::from_secs(1);
        let _ = mrp.handle_timeout(later);
        assert_eq!(mrp.exchanges.len(), 0, "standalone-ack flush reclaims");

        let p = mrp
            .prepare_outbound(
                0x01,
                ProtocolId::INTERACTION_MODEL,
                0x1234,
                b"status",
                MrpFlags { reliable: false },
                later,
            )
            .unwrap();
        assert!(!p.piggyback_acked);
        mrp.mark_packet_sent(MessageCounter(1), 0x1234, Vec::new(), false, later);
        assert_eq!(
            mrp.exchanges.len(),
            0,
            "late unreliable answer must not leak"
        );
    }

    /// Control for the two tests above: a RELIABLE send must keep its exchange
    /// until the peer acks it — reclaiming early would forget which side
    /// initiated the exchange while a retransmit is still in flight.
    #[test]
    fn reliable_send_keeps_exchange_until_acked() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        mrp.process_inbound(
            build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x05,
                0x4321,
                None,
                b"report",
            ),
            MessageCounter(9),
            now,
        )
        .unwrap();
        mrp.prepare_outbound(
            0x01,
            ProtocolId::INTERACTION_MODEL,
            0x4321,
            b"status",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();
        mrp.mark_packet_sent(MessageCounter(3), 0x4321, b"wire".to_vec(), true, now);
        assert_eq!(mrp.exchanges.len(), 1, "live until acked");

        let ack = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::ACK,
            crate::protocol_header::opcode::secure_channel::STANDALONE_ACK,
            0x4321,
            Some(MessageCounter(3)),
            b"",
        );
        mrp.process_inbound(ack, MessageCounter(10), now).unwrap();
        assert_eq!(mrp.exchanges.len(), 0, "reclaimed once acked");
    }

    #[test]
    fn completed_exchange_is_reclaimed() {
        // Reclaim regression: an exchange whose round-trip finishes (its
        // last pending ack clears, no buffered outbound ack) must be
        // evicted from the exchange table automatically.
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        // We initiate a reliable outbound.
        let prepared = mrp
            .prepare_outbound_local(
                0x02,
                ProtocolId::INTERACTION_MODEL,
                b"read",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        mrp.mark_packet_sent(
            MessageCounter(1),
            prepared.exchange_id,
            vec![0xAAu8; 8],
            true,
            now,
        );
        assert!(
            mrp.exchanges.contains(&prepared.exchange()),
            "exchange recorded while round-trip is in flight"
        );

        // Peer acks our message (standalone ack). Round-trip complete.
        let ack = build_inbound_payload(
            ExchangeFlags::ACK,
            opcode::secure_channel::STANDALONE_ACK,
            prepared.exchange_id,
            Some(MessageCounter(1)),
            &[],
        );
        mrp.process_inbound(ack, MessageCounter(50), now).unwrap();

        assert!(
            !mrp.exchanges.contains(&prepared.exchange()),
            "completed exchange must be reclaimed once idle"
        );
        assert!(mrp.exchanges.is_empty(), "exchange table shrank to empty");
    }

    #[test]
    fn active_exchange_is_not_reclaimed() {
        // An exchange with a pending retransmit (round-trip still in
        // flight) must NOT be evicted by the reclaim logic.
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        let prepared = mrp
            .prepare_outbound_local(
                0x02,
                ProtocolId::INTERACTION_MODEL,
                b"read",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        mrp.mark_packet_sent(
            MessageCounter(1),
            prepared.exchange_id,
            vec![0xAAu8; 8],
            true,
            now,
        );

        // A reliable inbound arrives on a DIFFERENT exchange — processing it
        // triggers a reclaim sweep, but our exchange has a pending
        // retransmit and must survive.
        let other = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0x7777,
            None,
            b"other",
        );
        mrp.process_inbound(other, MessageCounter(100), now)
            .unwrap();

        assert!(
            mrp.exchanges.contains(&prepared.exchange()),
            "exchange with a pending retransmit must not be reclaimed"
        );
    }

    #[test]
    fn buffered_outbound_ack_keeps_exchange_live() {
        // An exchange with a buffered outbound (piggyback) ack is NOT idle
        // and must not be reclaimed until the ack drains.
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        // Reliable inbound buffers a piggyback ack for exchange 0x4242.
        let inbound = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0x4242,
            None,
            b"req",
        );
        mrp.process_inbound(inbound, MessageCounter(100), now)
            .unwrap();
        assert!(
            mrp.holds(0x4242, false),
            "exchange with a buffered outbound ack stays live"
        );

        // Drain the ack via a standalone-ack timeout flush. After the ack
        // drains and nothing else is pending, the exchange is reclaimed.
        mrp.handle_timeout(now + Duration::from_millis(200));
        assert!(
            !mrp.holds(0x4242, false),
            "exchange reclaimed once its buffered ack drains and it is idle"
        );
    }

    /// `(exchange, acked counter)` of each event, which must all be ACKs.
    fn acked(events: Vec<MrpTimerEvent>) -> Vec<(u16, MessageCounter)> {
        events
            .into_iter()
            .map(|e| match e {
                MrpTimerEvent::StandaloneAckDeadlineFired {
                    exchange_id,
                    ack_counter,
                    ..
                } => (exchange_id, ack_counter),
                other => panic!("expected only standalone ACKs, got {other:?}"),
            })
            .collect()
    }

    /// A reliable inbound buffers an ACK; closing the exchange hands it out at
    /// once, with the peer's counter and our role, and reclaims the exchange.
    /// No time passes: this is the close path (chip `DoClose` → `FlushAcks`),
    /// not the 200 ms timer.
    #[test]
    fn flush_exchange_acks_emits_the_buffered_ack_without_waiting() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        let inbound = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0x4242,
            None,
            b"req",
        );
        mrp.process_inbound(inbound, MessageCounter(100), now)
            .unwrap();

        let events = mrp.flush_exchange_acks(0x4242);

        assert_eq!(events.len(), 1, "{events:?}");
        match &events[0] {
            MrpTimerEvent::StandaloneAckDeadlineFired {
                exchange_id,
                ack_counter,
                is_local_initiator,
            } => {
                assert_eq!(*exchange_id, 0x4242);
                assert_eq!(*ack_counter, MessageCounter(100));
                assert!(
                    !*is_local_initiator,
                    "the peer set I=1, so we are the responder"
                );
            }
            other => panic!("expected StandaloneAckDeadlineFired, got {other:?}"),
        }
        assert_eq!(mrp.poll_timeout(), None, "nothing left for the timer");
        assert!(
            !mrp.holds(0x4242, false),
            "the closed exchange is reclaimed"
        );
    }

    #[test]
    fn flush_exchange_acks_with_nothing_buffered_is_empty() {
        let mut mrp = MrpState::new(cfg());
        assert_eq!(mrp.flush_exchange_acks(0x4242).len(), 0);
    }

    /// Double ACK: a second close, and the timer after a close, send nothing.
    #[test]
    fn a_flushed_ack_is_never_sent_twice() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        let inbound = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0x4242,
            None,
            b"req",
        );
        mrp.process_inbound(inbound, MessageCounter(100), now)
            .unwrap();
        assert_eq!(mrp.flush_exchange_acks(0x4242).len(), 1);
        assert_eq!(mrp.flush_exchange_acks(0x4242).len(), 0, "second close");
        assert_eq!(
            mrp.handle_timeout(now + Duration::from_secs(1)).len(),
            0,
            "the timer after a close"
        );
    }

    /// Close keeps our own retransmits (chip `Close()`); only the abort
    /// (`close_exchange`) drops them.
    #[test]
    fn flush_exchange_acks_keeps_our_retransmits() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        // We open the exchange with a reliable request...
        let prepared = mrp
            .prepare_outbound_local(
                0x02,
                ProtocolId::INTERACTION_MODEL,
                b"req",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        mrp.mark_packet_sent(
            MessageCounter(1),
            prepared.exchange_id,
            vec![1u8; 4],
            true,
            now,
        );
        // ...and the peer sends a reliable message on it that does not ACK ours.
        let inbound = build_inbound_payload(
            ExchangeFlags::RELIABLE,
            0x05,
            prepared.exchange_id,
            None,
            b"report",
        );
        mrp.process_inbound(inbound, MessageCounter(100), now)
            .unwrap();

        let events = mrp.flush_exchange_acks(prepared.exchange_id);
        // We started this exchange, so our ACK on it must carry I=1 (spec
        // §4.4.3.1); chip matches an inbound to its exchange on that bit
        // (`ExchangeContext::MatchExchange`), so an ACK with I=0 would never
        // reach the peer's exchange and the peer would keep retransmitting.
        assert!(
            matches!(
                events.as_slice(),
                [MrpTimerEvent::StandaloneAckDeadlineFired {
                    ack_counter: MessageCounter(100),
                    is_local_initiator: true,
                    ..
                }]
            ),
            "one ACK for counter 100, as the exchange initiator: {events:?}"
        );
        assert!(
            mrp.poll_timeout().is_some(),
            "our retransmit is still scheduled"
        );
        assert!(
            mrp.exchanges.contains(&prepared.exchange()),
            "an exchange with a pending retransmit stays live"
        );
    }

    #[test]
    fn flush_all_acks_drains_every_exchange_in_ascending_order() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        for (exchange, counter) in [(0x30u16, 3u32), (0x10, 1), (0x20, 2)] {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                exchange,
                None,
                b"",
            );
            mrp.process_inbound(inbound, MessageCounter(counter), now)
                .unwrap();
        }
        assert_eq!(
            acked(mrp.flush_all_acks()),
            vec![
                (0x10, MessageCounter(1)),
                (0x20, MessageCounter(2)),
                (0x30, MessageCounter(3)),
            ]
        );
        assert_eq!(mrp.poll_timeout(), None);
        assert_eq!(mrp.exchanges.len(), 0);
    }

    /// A second reliable message on an exchange that still owes an ACK must
    /// not overwrite it. chip sends the older ACK at once ("Pending ack queue
    /// full; forcing tx of solitary ack", ReliableMessageContext.cpp:158-166
    /// at master, :159-167 at v1.4.2.0); matter.js does too
    /// (MessageExchange.ts:426-431). Before, the first message went unACKed
    /// until the peer retransmitted it.
    #[test]
    fn a_second_reliable_message_acks_the_first_at_once() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        let first = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0x4242,
            None,
            b"first",
        );
        mrp.process_inbound(first, MessageCounter(100), now)
            .unwrap();
        let later = now + Duration::from_millis(10);
        let second = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            0x4242,
            None,
            b"second",
        );
        mrp.process_inbound(second, MessageCounter(101), later)
            .unwrap();

        assert_eq!(
            mrp.poll_timeout(),
            Some(later),
            "the displaced ACK is due at once"
        );
        assert_eq!(
            acked(mrp.handle_timeout(later)),
            vec![(0x4242, MessageCounter(100))]
        );
        assert_eq!(
            acked(mrp.handle_timeout(later + Duration::from_millis(200))),
            vec![(0x4242, MessageCounter(101))],
            "the newer ACK keeps its 200 ms piggyback window"
        );
        assert_eq!(mrp.poll_timeout(), None);
    }

    #[test]
    fn closing_an_exchange_flushes_the_displaced_ack_first() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        for counter in [100u32, 101] {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                0x4242,
                None,
                b"",
            );
            mrp.process_inbound(inbound, MessageCounter(counter), now)
                .unwrap();
        }
        assert_eq!(
            acked(mrp.flush_exchange_acks(0x4242)),
            vec![(0x4242, MessageCounter(100)), (0x4242, MessageCounter(101))]
        );
        assert_eq!(mrp.poll_timeout(), None);
        assert_eq!(mrp.exchanges.len(), 0);
    }

    #[test]
    fn a_piggyback_drains_only_the_newest_ack() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        for counter in [100u32, 101] {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                0x4242,
                None,
                b"",
            );
            mrp.process_inbound(inbound, MessageCounter(counter), now)
                .unwrap();
        }
        let prepared = mrp
            .prepare_outbound(
                0x05,
                ProtocolId::INTERACTION_MODEL,
                0x4242,
                b"resp",
                MrpFlags::default(),
                now,
            )
            .unwrap();
        let (header, _) = decode_protocol_header(&prepared.wire_payload).unwrap();
        assert_eq!(header.ack_counter, Some(MessageCounter(101)));
        assert_eq!(
            acked(mrp.handle_timeout(now)),
            vec![(0x4242, MessageCounter(100))],
            "the displaced ACK still goes out on its own"
        );
    }

    /// A duplicate of a message whose ACK sits in the displaced queue: the
    /// duplicate path re-sends that ACK, so the queued one goes too (one ACK
    /// per counter).
    #[test]
    fn a_duplicate_of_a_displaced_message_drops_its_queued_ack() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        for counter in [100u32, 101] {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                0x4242,
                None,
                b"",
            );
            mrp.process_inbound(inbound, MessageCounter(counter), now)
                .unwrap();
        }
        mrp.forget_ack_for(MessageCounter(100));
        assert_eq!(
            acked(mrp.handle_timeout(now)),
            Vec::<(u16, MessageCounter)>::new()
        );
        assert_eq!(
            acked(mrp.handle_timeout(now + Duration::from_millis(200))),
            vec![(0x4242, MessageCounter(101))]
        );
    }

    /// A peer that floods one exchange with reliable messages cannot grow the
    /// displaced queue without bound; past the cap, the duplicate path in
    /// `SessionManager::decode_inbound` ACKs its retransmits instead.
    #[test]
    fn displaced_acks_are_bounded() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        for n in 0..40u32 {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                0x4242,
                None,
                b"",
            );
            mrp.process_inbound(inbound, MessageCounter(n), now)
                .unwrap();
        }
        assert_eq!(mrp.handle_timeout(now).len(), MAX_DISPLACED_ACKS);
        // Counter 32 was the first displaced past the cap; it is still in the
        // recent-reliable cache, so its retransmit takes the duplicate path.
        assert!(mrp.check_duplicate_reliable(MessageCounter(32)).is_some());
    }

    /// A displaced ACK keeps the I flag of its own exchange: on an exchange
    /// we started it must carry I=1 (spec §4.4.3.1), or chip's
    /// `ExchangeContext::MatchExchange` never routes it to the peer's
    /// exchange and the peer keeps retransmitting.
    #[test]
    fn a_displaced_ack_keeps_its_exchanges_i_flag() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        let prepared = mrp
            .prepare_outbound_local(
                0x02,
                ProtocolId::INTERACTION_MODEL,
                b"req",
                MrpFlags::default(),
                now,
            )
            .unwrap();
        for counter in [100u32, 101] {
            let inbound = build_inbound_payload(
                ExchangeFlags::RELIABLE,
                0x05,
                prepared.exchange_id,
                None,
                b"report",
            );
            mrp.process_inbound(inbound, MessageCounter(counter), now)
                .unwrap();
        }
        let events = mrp.handle_timeout(now);
        assert!(
            matches!(
                events.as_slice(),
                [MrpTimerEvent::StandaloneAckDeadlineFired {
                    ack_counter: MessageCounter(100),
                    is_local_initiator: true,
                    ..
                }]
            ),
            "the displaced ACK, as the exchange initiator: {events:?}"
        );
    }

    /// A piggyback that drains the newest ACK leaves the displaced one owed:
    /// the session still counts as busy, an unreliable answer does not
    /// reclaim the exchange, and flushing every ACK still finds it.
    #[test]
    fn a_displaced_ack_alone_keeps_its_exchange_owed() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        for counter in [100u32, 101] {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                0x4242,
                None,
                b"",
            );
            mrp.process_inbound(inbound, MessageCounter(counter), now)
                .unwrap();
        }
        mrp.prepare_outbound(
            0x05,
            ProtocolId::INTERACTION_MODEL,
            0x4242,
            b"resp",
            MrpFlags::default(),
            now,
        )
        .unwrap();
        mrp.mark_packet_sent(MessageCounter(1), 0x4242, vec![1u8; 4], false, now);

        assert!(mrp.has_pending(), "the displaced ACK is still owed");
        assert!(
            mrp.holds(0x4242, false),
            "an exchange that still owes an ACK stays live"
        );
        assert_eq!(
            acked(mrp.flush_all_acks()),
            vec![(0x4242, MessageCounter(100))]
        );
        assert!(!mrp.has_pending());
        assert_eq!(mrp.exchanges.len(), 0);
    }

    /// On one tick, a displaced ACK goes out ahead of another exchange's ACK
    /// whose 200 ms deadline is due: it is the older of the two.
    #[test]
    fn handle_timeout_sends_displaced_acks_first() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        let due = now + Duration::from_millis(200);
        for (exchange, counter, at) in [(0x10u16, 1u32, now), (0x20, 2, due), (0x20, 3, due)] {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                exchange,
                None,
                b"",
            );
            mrp.process_inbound(inbound, MessageCounter(counter), at)
                .unwrap();
        }
        assert_eq!(
            acked(mrp.handle_timeout(due)),
            vec![(0x20, MessageCounter(2)), (0x10, MessageCounter(1))]
        );
    }

    /// Leave `0x4242` owing only a displaced ACK (for counter 100): two
    /// reliable inbounds, then an unreliable answer that piggybacks the newer
    /// ACK (101).
    fn owe_only_a_displaced_ack(mrp: &mut MrpState, now: Instant) {
        for counter in [100u32, 101] {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                0x4242,
                None,
                b"",
            );
            mrp.process_inbound(inbound, MessageCounter(counter), now)
                .unwrap();
        }
        mrp.prepare_outbound(
            0x05,
            ProtocolId::INTERACTION_MODEL,
            0x4242,
            b"resp",
            MrpFlags::default(),
            now,
        )
        .unwrap();
        mrp.mark_packet_sent(MessageCounter(1), 0x4242, vec![1u8; 4], false, now);
    }

    /// Once the timer sends an exchange's last owed ACK, a displaced one, the
    /// exchange is reclaimed rather than held in the table for good.
    #[test]
    fn a_displaced_ack_sent_by_the_timer_reclaims_its_exchange() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        owe_only_a_displaced_ack(&mut mrp, now);
        assert_eq!(
            acked(mrp.handle_timeout(now)),
            vec![(0x4242, MessageCounter(100))]
        );
        assert_eq!(mrp.exchanges.len(), 0);
    }

    /// Once the duplicate path drops an exchange's last owed ACK, a displaced
    /// one, the exchange is reclaimed rather than held in the table for good.
    #[test]
    fn forgetting_a_displaced_ack_reclaims_its_exchange() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        owe_only_a_displaced_ack(&mut mrp, now);
        mrp.forget_ack_for(MessageCounter(100));
        assert!(!mrp.has_pending());
        assert_eq!(mrp.exchanges.len(), 0);
    }

    /// Finishing one exchange takes only its own displaced ACKs; another
    /// exchange's stay queued, and its exchange stays live until they go.
    #[test]
    fn flushing_one_exchange_leaves_anothers_displaced_ack() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        for (exchange, counter) in [(0x10u16, 1u32), (0x10, 2), (0x20, 3), (0x20, 4)] {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                exchange,
                None,
                b"",
            );
            mrp.process_inbound(inbound, MessageCounter(counter), now)
                .unwrap();
        }
        assert_eq!(
            acked(mrp.flush_exchange_acks(0x10)),
            vec![(0x10, MessageCounter(1)), (0x10, MessageCounter(2))]
        );
        assert!(mrp.holds(0x20, false));
        assert_eq!(
            acked(mrp.handle_timeout(now)),
            vec![(0x20, MessageCounter(3))]
        );
    }

    /// The abort (`close_exchange`) and the transport-reliable cancel
    /// (`cancel_pending_outbound_ack`) drop a displaced ACK as they drop a
    /// buffered one.
    #[test]
    fn close_exchange_and_cancel_drop_displaced_acks() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        for (exchange, counter) in [(0x10u16, 1u32), (0x10, 2), (0x20, 3), (0x20, 4)] {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                exchange,
                None,
                b"",
            );
            mrp.process_inbound(inbound, MessageCounter(counter), now)
                .unwrap();
        }
        mrp.close_exchange(0x10);
        mrp.cancel_pending_outbound_ack(ExchangeKey::of_inbound(0x20, true));

        assert_eq!(mrp.poll_timeout(), None);
        assert_eq!(
            acked(mrp.handle_timeout(now + Duration::from_secs(1))),
            Vec::<(u16, MessageCounter)>::new()
        );
    }

    // --- Exchange identity: (exchange id, role), Matter 1.4 Core §4.10 -----
    //
    // Both sides allocate exchange ids from their own counters, so an exchange
    // we started and one the peer started can share an id. Each test below
    // holds one of each on id 0x30 and checks that the two never share an
    // owed ACK, a retransmit or a table entry.

    const SHARED_ID: u16 = 0x30;

    fn ours(id: u16) -> ExchangeKey {
        ExchangeKey {
            id,
            is_local_initiator: true,
        }
    }

    fn peers(id: u16) -> ExchangeKey {
        ExchangeKey {
            id,
            is_local_initiator: false,
        }
    }

    /// The peer starts exchange `id` with a reliable request (`I=1`), so we
    /// owe it an ACK there.
    fn owe_on_peers_exchange(mrp: &mut MrpState, id: u16, counter: u32, now: Instant) {
        let inbound = build_inbound_payload(
            ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
            0x02,
            id,
            None,
            b"request",
        );
        mrp.process_inbound(inbound, MessageCounter(counter), now)
            .unwrap();
    }

    /// The peer answers on OUR exchange `id` with a reliable response
    /// (`I=0`), so we owe it an ACK there.
    fn owe_on_our_exchange(mrp: &mut MrpState, id: u16, counter: u32, now: Instant) {
        let inbound = build_inbound_payload(ExchangeFlags::RELIABLE, 0x05, id, None, b"response");
        mrp.process_inbound(inbound, MessageCounter(counter), now)
            .unwrap();
    }

    /// Standalone ACKs as `(exchange id, acked counter, I flag)`.
    fn acks_with_role(events: Vec<MrpTimerEvent>) -> Vec<(u16, MessageCounter, bool)> {
        events
            .into_iter()
            .map(|e| match e {
                MrpTimerEvent::StandaloneAckDeadlineFired {
                    exchange_id,
                    ack_counter,
                    is_local_initiator,
                } => (exchange_id, ack_counter, is_local_initiator),
                other => panic!("expected only standalone ACKs, got {other:?}"),
            })
            .collect()
    }

    /// Send a reliable message on `exchange` as `SessionManager` does
    /// (build, commit, mark sent), under our message counter `counter`.
    fn send_reliable_on(mrp: &mut MrpState, exchange: ExchangeKey, counter: u32, now: Instant) {
        let prepared = mrp.build_outbound(
            0x02,
            ProtocolId::INTERACTION_MODEL,
            exchange,
            b"msg",
            MrpFlags { reliable: true },
        );
        mrp.commit_outbound(&prepared);
        mrp.mark_packet_sent_on(MessageCounter(counter), exchange, vec![0xEE], true, now);
    }

    /// Inbound matching (§4.10.5.1): `I=1` belongs to the peer's exchange and
    /// `I=0` to ours, so a reliable message on each, with the same id, makes
    /// two exchanges owing two ACKs. Neither displaces the other (the
    /// displacement rule is per exchange, §4.12.5.2.2), and each ACK fires
    /// with its own exchange's `I` flag, the peer's first.
    #[test]
    fn exchanges_sharing_an_id_owe_separate_acks() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        owe_on_peers_exchange(&mut mrp, SHARED_ID, 100, now);
        owe_on_our_exchange(&mut mrp, SHARED_ID, 101, now);

        assert!(mrp.holds(SHARED_ID, false) && mrp.holds(SHARED_ID, true));
        assert_eq!(mrp.exchange_count(), 2);
        assert!(
            mrp.displaced_acks.is_empty(),
            "a message on the other exchange displaced this one's ACK"
        );
        assert_eq!(
            mrp.poll_timeout(),
            Some(now + Duration::from_millis(200)),
            "nothing is due at once"
        );
        assert_eq!(
            acks_with_role(mrp.handle_timeout(now + Duration::from_millis(200))),
            vec![
                (SHARED_ID, MessageCounter(100), false),
                (SHARED_ID, MessageCounter(101), true),
            ]
        );
        assert_eq!(
            mrp.exchange_count(),
            0,
            "both reclaimed once their ACKs went"
        );
    }

    /// A send carries the ACK owed on its own exchange and never the one owed
    /// on the other exchange with the same id, and its `I` flag follows its
    /// own role.
    #[test]
    fn a_send_carries_only_its_own_exchanges_ack() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        owe_on_peers_exchange(&mut mrp, SHARED_ID, 100, now);
        owe_on_our_exchange(&mut mrp, SHARED_ID, 101, now);

        for (exchange, owed) in [(ours(SHARED_ID), 101), (peers(SHARED_ID), 100)] {
            let prepared = mrp.build_outbound(
                0x08,
                ProtocolId::INTERACTION_MODEL,
                exchange,
                b"msg",
                MrpFlags::default(),
            );
            let (header, _) = decode_protocol_header(&prepared.wire_payload).unwrap();
            assert_eq!(
                header.ack_counter,
                Some(MessageCounter(owed)),
                "{exchange:?}"
            );
            assert_eq!(
                header.exchange_flags.contains(ExchangeFlags::INITIATOR),
                exchange.is_local_initiator,
                "{exchange:?}"
            );
            assert_eq!(prepared.exchange(), exchange);
            mrp.commit_outbound(&prepared);
            mrp.mark_packet_sent_on(MessageCounter(1), exchange, Vec::new(), false, now);
        }
        assert_eq!(
            mrp.poll_timeout(),
            None,
            "both ACKs rode their own messages"
        );
        assert_eq!(mrp.exchange_count(), 0);
    }

    /// Flushing one exchange sends only its ACK and reclaims only it.
    #[test]
    fn flushing_one_exchange_leaves_the_other_with_the_same_id() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        owe_on_peers_exchange(&mut mrp, SHARED_ID, 100, now);
        owe_on_our_exchange(&mut mrp, SHARED_ID, 101, now);

        assert_eq!(
            acks_with_role(mrp.flush_exchange_acks_on(ours(SHARED_ID))),
            vec![(SHARED_ID, MessageCounter(101), true)]
        );
        assert!(!mrp.holds(SHARED_ID, true), "ours is reclaimed");
        assert!(mrp.holds(SHARED_ID, false), "the peer's still owes its ACK");
        assert_eq!(
            acks_with_role(mrp.flush_exchange_acks_on(peers(SHARED_ID))),
            vec![(SHARED_ID, MessageCounter(100), false)]
        );
        assert_eq!(mrp.exchange_count(), 0);
    }

    /// `flush_all_acks` takes every exchange, in key order: by id, the peer's
    /// before ours.
    #[test]
    fn flush_all_acks_orders_exchanges_by_id_then_role() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        owe_on_our_exchange(&mut mrp, SHARED_ID, 101, now);
        owe_on_peers_exchange(&mut mrp, SHARED_ID + 1, 102, now);
        owe_on_peers_exchange(&mut mrp, SHARED_ID, 100, now);
        assert_eq!(
            acks_with_role(mrp.flush_all_acks()),
            vec![
                (SHARED_ID, MessageCounter(100), false),
                (SHARED_ID, MessageCounter(101), true),
                (SHARED_ID + 1, MessageCounter(102), false),
            ]
        );
    }

    /// Aborting one exchange drops only its retransmits; ours and the peer's
    /// each have one, under different counters.
    #[test]
    fn closing_one_exchange_keeps_the_other_exchanges_retransmit() {
        for (closed, kept_counter) in [(ours(SHARED_ID), 2), (peers(SHARED_ID), 1)] {
            let mut mrp = MrpState::new(cfg());
            let now = t0();
            send_reliable_on(&mut mrp, ours(SHARED_ID), 1, now);
            owe_on_peers_exchange(&mut mrp, SHARED_ID, 100, now);
            send_reliable_on(&mut mrp, peers(SHARED_ID), 2, now);

            mrp.close_exchange_on(closed);
            assert!(!mrp.exchanges.contains(&closed), "{closed:?}");
            let kept = ExchangeKey {
                id: SHARED_ID,
                is_local_initiator: !closed.is_local_initiator,
            };
            assert!(mrp.exchanges.contains(&kept), "{closed:?}");
            let retransmits: Vec<MessageCounter> = mrp
                .handle_timeout(now + Duration::from_secs(10))
                .into_iter()
                .filter_map(|e| match e {
                    MrpTimerEvent::Retransmit { counter, .. } => Some(counter),
                    _ => None,
                })
                .collect();
            assert_eq!(
                retransmits,
                vec![MessageCounter(kept_counter)],
                "closing {closed:?} must leave the other exchange's retransmit"
            );
        }
    }

    /// An ACK completes our exchange and reclaims it; the peer's exchange
    /// with the same id still owes an ACK and stays. Our unreliable answer
    /// then drains that ACK and reclaims the peer's exchange too: the
    /// reclaim hangs off the send, for the exchange the send was on.
    #[test]
    fn reclaim_follows_each_exchanges_own_work() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        send_reliable_on(&mut mrp, ours(SHARED_ID), 1, now);
        owe_on_peers_exchange(&mut mrp, SHARED_ID, 100, now);
        assert_eq!(mrp.exchange_count(), 2);

        let ack = build_inbound_payload(
            ExchangeFlags::ACK,
            crate::protocol_header::opcode::secure_channel::STANDALONE_ACK,
            SHARED_ID,
            Some(MessageCounter(1)),
            b"",
        );
        let outcome = mrp.process_inbound(ack, MessageCounter(101), now).unwrap();
        assert!(matches!(outcome, InboundOutcome::AckOnly { .. }));
        assert!(
            !mrp.holds(SHARED_ID, true),
            "our completed exchange is reclaimed"
        );
        assert!(mrp.holds(SHARED_ID, false), "the peer's still owes its ACK");

        let answer = mrp.build_outbound(
            0x05,
            ProtocolId::INTERACTION_MODEL,
            peers(SHARED_ID),
            b"answer",
            MrpFlags::default(),
        );
        assert!(answer.piggyback_acked);
        mrp.commit_outbound(&answer);
        mrp.mark_packet_sent_on(MessageCounter(2), answer.exchange(), Vec::new(), false, now);
        assert_eq!(
            mrp.exchange_count(),
            0,
            "the unreliable answer reclaims the peer's"
        );
    }

    /// The other order: the peer's exchange goes idle first (our unreliable
    /// answer drains its ACK) while our retransmit on the exchange with the
    /// same id is still pending. Only ours has live work, so only ours stays.
    #[test]
    fn a_peers_exchange_is_reclaimed_while_ours_still_retransmits() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        send_reliable_on(&mut mrp, ours(SHARED_ID), 1, now);
        owe_on_peers_exchange(&mut mrp, SHARED_ID, 100, now);

        let answer = mrp.build_outbound(
            0x05,
            ProtocolId::INTERACTION_MODEL,
            peers(SHARED_ID),
            b"answer",
            MrpFlags::default(),
        );
        mrp.commit_outbound(&answer);
        mrp.mark_packet_sent_on(MessageCounter(2), answer.exchange(), Vec::new(), false, now);
        assert!(!mrp.holds(SHARED_ID, false), "the peer's exchange is done");
        assert!(mrp.holds(SHARED_ID, true), "ours still has a retransmit");
        assert_eq!(mrp.exchange_count(), 1);
    }

    /// A displaced ACK belongs to its exchange: flushing ours does not take
    /// the one displaced on the peer's exchange with the same id.
    #[test]
    fn a_displaced_ack_stays_with_its_own_exchange() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        owe_on_peers_exchange(&mut mrp, SHARED_ID, 100, now);
        owe_on_peers_exchange(&mut mrp, SHARED_ID, 101, now);
        owe_on_our_exchange(&mut mrp, SHARED_ID, 102, now);
        assert_eq!(mrp.displaced_acks.len(), 1, "101 displaced 100");

        assert_eq!(
            acks_with_role(mrp.flush_exchange_acks_on(ours(SHARED_ID))),
            vec![(SHARED_ID, MessageCounter(102), true)]
        );
        assert_eq!(
            acks_with_role(mrp.flush_exchange_acks_on(peers(SHARED_ID))),
            vec![
                (SHARED_ID, MessageCounter(100), false),
                (SHARED_ID, MessageCounter(101), false),
            ]
        );
    }

    /// An ACK clears a retransmit by counter alone (§4.12.5.2.1), whatever
    /// exchange it rides. When the ACK for our message on the peer's exchange
    /// rides a message on OURS (a peer that matches piggybacks on node and
    /// exchange id only, as §4.12.5.1.1 reads), the peer's exchange is left
    /// with no work and must be reclaimed, or each such ACK leaks a slot.
    #[test]
    fn a_cross_exchange_ack_reclaims_the_exchange_it_completes() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        send_reliable_on(&mut mrp, peers(SHARED_ID), 1, now);
        mrp.note_local_exchange(SHARED_ID);
        assert_eq!(mrp.exchange_count(), 2);

        let inbound = build_inbound_payload(
            ExchangeFlags::ACK,
            0x05,
            SHARED_ID,
            Some(MessageCounter(1)),
            b"on-ours",
        );
        mrp.process_inbound(inbound, MessageCounter(100), now)
            .unwrap();
        assert!(!mrp.has_pending(), "the retransmit is cleared");
        assert!(
            !mrp.holds(SHARED_ID, false),
            "the peer's exchange is reclaimed"
        );
        assert_eq!(mrp.exchange_count(), 0, "ours is idle too");
    }

    /// A transport-reliable session cancels the ACK the inbound armed, and
    /// only on the inbound's own exchange.
    #[test]
    fn cancelling_an_owed_ack_is_scoped_to_one_exchange() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        owe_on_peers_exchange(&mut mrp, SHARED_ID, 100, now);
        owe_on_our_exchange(&mut mrp, SHARED_ID, 101, now);
        mrp.cancel_pending_outbound_ack(ExchangeKey::of_inbound(SHARED_ID, true));
        assert_eq!(
            acks_with_role(mrp.flush_all_acks()),
            vec![(SHARED_ID, MessageCounter(101), true)]
        );
    }

    /// What the id-only public methods mean: the exchange we started if the
    /// table holds one with the id, otherwise the peer's, including an id
    /// the table does not hold at all (a response, `I=0`, as before roles
    /// were tracked).
    #[test]
    fn an_id_alone_means_our_exchange_then_the_peers() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();
        assert_eq!(mrp.resolve(SHARED_ID), peers(SHARED_ID), "unknown id");
        owe_on_peers_exchange(&mut mrp, SHARED_ID, 100, now);
        assert_eq!(mrp.resolve(SHARED_ID), peers(SHARED_ID));
        mrp.note_local_exchange(SHARED_ID);
        assert_eq!(
            mrp.resolve(SHARED_ID),
            ours(SHARED_ID),
            "ours wins a collision"
        );

        // So the legacy calls act on ours and leave the peer's ACK owed.
        let prepared = mrp
            .prepare_outbound(
                0x02,
                ProtocolId::INTERACTION_MODEL,
                SHARED_ID,
                b"ours",
                MrpFlags::default(),
                now,
            )
            .unwrap();
        assert!(prepared.is_local_initiator);
        assert!(
            !prepared.piggyback_acked,
            "the peer's ACK must not ride ours"
        );
        mrp.mark_packet_sent(MessageCounter(1), SHARED_ID, Vec::new(), false, now);
        assert!(
            !mrp.holds(SHARED_ID, true),
            "the unreliable send reclaims ours"
        );
        assert_eq!(
            acks_with_role(mrp.flush_exchange_acks(SHARED_ID)),
            vec![(SHARED_ID, MessageCounter(100), false)],
            "with ours gone, the id means the peer's"
        );
    }

    #[test]
    fn recent_reliable_cache_evicts_after_32_entries() {
        let mut mrp = MrpState::new(cfg());
        let now = t0();

        // Record 33 distinct reliable inbounds; the first should be evicted.
        for n in 0u32..33 {
            let inbound = build_inbound_payload(
                ExchangeFlags::INITIATOR | ExchangeFlags::RELIABLE,
                0x02,
                u16::try_from(n).unwrap(),
                None,
                b"",
            );
            mrp.process_inbound(inbound, MessageCounter(n), now)
                .unwrap();
        }

        // Oldest (counter=0) was evicted.
        assert!(mrp.check_duplicate_reliable(MessageCounter(0)).is_none());
        // Newest is still there.
        assert!(mrp.check_duplicate_reliable(MessageCounter(32)).is_some());
    }
}
