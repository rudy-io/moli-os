//! Unsecured (session-id 0) message framing for the PASE handshake.
//!
//! `matter-transport`'s `SessionManager` only handles *encrypted* sessions —
//! it always encrypts and explicitly skips session id 0. The PASE handshake
//! runs UNSECURED (session id 0, plaintext, `SecureChannel` protocol, opcodes
//! `PBKDFParamRequest 0x20` … `PASE_Pake3 0x24`). connectedhomeip models this
//! as a first-class unauthenticated session, so it is a necessary path, not a
//! hack. This module builds it directly on the transport's header primitives:
//! the wire layout is `secured-message-header (session id 0) || protocol-header
//! || plaintext app payload` — no AES-CCM.
//!
//! Header conventions (resolved during M6.6.5 real-device validation, Tapo
//! P110M): session-establishment messages SHALL set the `S` bit and carry a
//! random *ephemeral source node id* (Matter Core Spec §4.13.2.1) — devices
//! key their unsecured session context on it and **silently drop** frames
//! without it. The unsecured message counter SHALL be seeded as a random
//! 28-bit value + 1 (§4.6.1.1). Security flags stay empty and the destination
//! node id stays absent on the initiator side; the responder echoes our
//! ephemeral id back as the destination.

use std::net::SocketAddr;
use std::time::Duration;

use matter_transport::{
    decode_header, decode_protocol_header, encode_header, encode_protocol_header, DestNodeId,
    ExchangeFlags, MessageCounter, MrpConfig, NodeId, ProtocolHeader, ProtocolId,
    SecuredMessageFlags, SecuredMessageHeader, SecurityFlags, SessionId,
};

use crate::driver::datagram::AsyncDatagram;
use crate::driver::error::DriverError;
use crate::driver::TransportReliability;

/// Response deadline used by the [`TransportReliability::TransportProvides`]
/// path: after the single send, wait this long for the peer's reply before
/// surfacing a [`DriverError::Timeout`]. There is no stop-and-wait retransmit
/// on a reliable transport, so this is the *only* deadline (unlike the MRP
/// path, where it is the shorter post-ack response window). 30 s matches the
/// MRP path's post-ack response timeout.
const UNSECURED_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on the total pre-ack retransmit waiting one handshake may do, across
/// **all** of its steps.
///
/// A peer may advertise `SII` up to the spec ceiling of one hour. Sizing the
/// handshake to the peer without a bound would let one such advertisement turn
/// a connect into a multi-hour operation holding a `pending_connects` slot and
/// every caller parked behind it.
///
/// This is a budget for the whole handshake, shared across every
/// [`UnsecuredExchange::send_and_recv`] call, rather than a per-step allowance.
///
/// # This is NOT a bound on connect latency
///
/// It bounds **pre-ack retransmit waiting only**. Do not size a caller-side
/// timeout against it, and do not read "we hit our own bound" as "the peer has
/// had its whole window" — a connect legitimately spends, in addition:
///
/// - the mDNS operational resolve, up to the controller's `RESOLVE_DEADLINE`
///   (30 s in `matter-controller`);
/// - up to the 30 s post-ack response deadline (`UNSECURED_RESPONSE_TIMEOUT`)
///   *per round trip* once the peer acknowledges, and [`run_case_establish`]
///   has two round trips
///   (Sigma1 → Sigma2, Sigma3 → `StatusReport`).
///
/// A peer that acks Sigma1 promptly and then sleeps, or a slow resolve, burns a
/// caller's deadline with this budget barely touched. Worst case is therefore
/// around 2.5x this constant, not equal to it. Reported by a downstream
/// consumer (`WeaveHome`, 2026-09-08) who reached for this constant when sizing
/// its own bound, which is the natural and wrong thing to do — an earlier
/// version of this comment invited it by claiming this was "the only thing
/// bounding total connect latency". It is not.
///
/// 60 s is the largest value an operator still reads as slow rather than hung.
/// Neither chip nor matter.js imposes such a cap; we do, because a hub must
/// fail in bounded time and let the caller retry.
///
/// [`run_case_establish`]: crate::driver::run_case_establish
///
/// Known limitation: this is wrong for a LIT ICD whose advertised `SII` is
/// minutes long. Those are reached through the check-in protocol, not a plain
/// CASE connect; if plain connects to one are ever wanted, this is what blocks
/// them.
pub const MAX_HANDSHAKE_RETRANSMIT_WINDOW: Duration = Duration::from_secs(60);

/// The pre-ack retransmit schedule for one handshake: the un-jittered wait
/// after each transmission, in order.
///
/// Uses the **idle** base for every entry. A peer we are about to hand a Sigma1
/// has by definition sent us nothing, so it is not in active mode -- Matter
/// 1.4 §4.12.2.1, chip's `mLastPeerActivityTime(kZero)` "Start at zero to
/// default to IDLE state", and matter.js's "when we are the initiator we assume
/// the node is in idle mode". Growth, margin and threshold come from
/// [`MrpConfig::retransmit_delay`], so the handshake and the operational layer
/// cannot drift apart.
///
/// Cap rule, in this order:
/// 1. each individual delay is clamped to `cap`, which bounds even the FIRST
///    wait -- an `SII` at the spec ceiling would otherwise blow past it alone;
/// 2. an entry is dropped, and iteration stops, if adding it would take the
///    running total past `cap` -- unless it would be the only entry.
///
/// Invariants: never empty; at most `config.max_transmissions` entries; the sum
/// never exceeds `cap`.
///
/// # The window can be much LONGER than it used to be
///
/// Before this was peer-sized, every handshake got a flat 300 ms x 5 = 1.5 s
/// regardless of what the device advertised. For a genuinely sleepy peer the
/// window now *grows*, substantially — and the direction is the opposite of the
/// operational MRP change that shipped alongside it, which shortened windows.
///
/// Measured in the field (`WeaveHome`, 2026-09-08): an Eve Door & Window Thread
/// SED advertising `SII=3300` gets **37.2 s** — roughly 25x the old flat value.
/// A eufy E31 lock advertising `SII=1800` gets 20.3 s. Mains Wi-Fi peers
/// advertising nothing keep 5.6 s.
///
/// **A caller whose timeout was chosen against the old 1.5 s will now cut a
/// sleepy device off mid-handshake.** That is exactly what happened downstream:
/// a 15 s consumer bound had always sat comfortably above 1.5 s and silently
/// became 40% of the window the device had just earned.
fn handshake_schedule(config: &MrpConfig, cap: Duration) -> Vec<Duration> {
    let mut out = Vec::new();
    let mut total = Duration::ZERO;
    for n in 0..config.max_transmissions {
        let d = config.retransmit_delay(n, false).min(cap);
        if !out.is_empty() && total + d > cap {
            break;
        }
        total += d;
        out.push(d);
    }
    out
}

/// `SecureChannel` `MRP Standalone Acknowledgement` opcode (spec §4.12.8).
/// Devices send one when a reliable message's response is not immediately
/// ready; it acknowledges delivery but is NOT the exchange's response.
const OPCODE_MRP_STANDALONE_ACK: u8 = 0x10;

/// `SecureChannel` `StatusReport` opcode (spec §4.10.1.1). Devices close a
/// PASE/CASE handshake with one (`SessionEstablishmentSuccess` or a failure
/// code) after the terminal message (Pake3 / Sigma3).
const OPCODE_STATUS_REPORT: u8 = 0x40;

/// Parsed `SecureChannel` `StatusReport` body (spec §4.10.1.1): three fixed
/// little-endian fields, no TLV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecureChannelStatus {
    /// General code: `0` SUCCESS, `1` FAILURE, … (spec Table 22).
    pub general_code: u16,
    /// Protocol id the protocol code belongs to (`0x0000` = `SecureChannel`).
    pub protocol_id: u32,
    /// Protocol-specific code (`0x0000` = `SessionEstablishmentSuccess`).
    pub protocol_code: u16,
}

impl SecureChannelStatus {
    /// `true` for the handshake-closing success report: general SUCCESS and
    /// `SecureChannel` `SessionEstablishmentSuccess`.
    #[must_use]
    pub fn is_session_establishment_success(self) -> bool {
        self.general_code == 0 && self.protocol_id == 0 && self.protocol_code == 0
    }
}

/// Parse `msg` as a `SecureChannel` `StatusReport`, the message that closes a
/// PASE/CASE handshake.
///
/// # Errors
///
/// - [`DriverError::Handshake`] if the opcode is not `StatusReport 0x40` or
///   the body is shorter than the fixed 8-byte layout.
pub fn parse_status_report(msg: &UnsecuredMessage) -> Result<SecureChannelStatus, DriverError> {
    if msg.opcode != OPCODE_STATUS_REPORT {
        return Err(DriverError::Handshake(
            "expected a SecureChannel StatusReport to close the handshake",
        ));
    }
    let b = &msg.payload;
    if b.len() < 8 {
        return Err(DriverError::Handshake("StatusReport body truncated"));
    }
    Ok(SecureChannelStatus {
        general_code: u16::from_le_bytes([b[0], b[1]]),
        protocol_id: u32::from_le_bytes([b[2], b[3], b[4], b[5]]),
        protocol_code: u16::from_le_bytes([b[6], b[7]]),
    })
}

/// Require `msg` to be the `SecureChannel` handshake message `opcode`.
///
/// A `StatusReport` in its place is a *rejection* (e.g. `NoSharedTrustRoots`
/// when Sigma1's destination id matches no fabric, or `InvalidParameter` on
/// a failed PASE proof) and is surfaced as
/// [`DriverError::SessionEstablishmentFailed`] carrying the device's codes —
/// never fed into the next handshake parser (observed misparse on a real
/// device: Tapo P110M, M6.6.5 validation).
///
/// # Errors
///
/// - [`DriverError::SessionEstablishmentFailed`] for a `StatusReport`.
/// - [`DriverError::Handshake`] for any other unexpected opcode or a
///   malformed `StatusReport` body.
pub fn require_handshake_opcode(msg: &UnsecuredMessage, opcode: u8) -> Result<(), DriverError> {
    require_handshake_opcode_any(msg, &[opcode])
}

/// [`require_handshake_opcode`] for a step that admits more than one opcode.
///
/// The CASE resumption path needs it: after a Sigma1 carrying resumption
/// fields the responder may answer with `Sigma2_Resume` (0x33) **or** decline
/// and send a plain `Sigma2` (0x31), and both are legitimate. Which one arrived
/// is what tells the caller which handler to route to.
///
/// # Errors
///
/// - [`DriverError::SessionEstablishmentFailed`] if the peer sent a terminal
///   `StatusReport` instead (its codes are surfaced, not swallowed).
/// - [`DriverError::Handshake`] for any other opcode.
pub fn require_handshake_opcode_any(
    msg: &UnsecuredMessage,
    opcodes: &[u8],
) -> Result<(), DriverError> {
    if msg.protocol_id == ProtocolId::SECURE_CHANNEL && opcodes.contains(&msg.opcode) {
        return Ok(());
    }
    if msg.protocol_id == ProtocolId::SECURE_CHANNEL && msg.opcode == OPCODE_STATUS_REPORT {
        let status = parse_status_report(msg)?;
        return Err(DriverError::SessionEstablishmentFailed {
            general_code: status.general_code,
            protocol_code: status.protocol_code,
        });
    }
    Err(DriverError::Handshake(
        "unexpected opcode in session-establishment exchange",
    ))
}

/// A decoded unsecured message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsecuredMessage {
    /// The secured-message-header message counter (the peer's outbound counter).
    pub message_counter: u32,
    /// Exchange id from the protocol header.
    pub exchange_id: u16,
    /// Protocol opcode (e.g. `PBKDFParamResponse 0x21`).
    pub opcode: u8,
    /// Protocol id (always `SECURE_CHANNEL` for PASE).
    pub protocol_id: ProtocolId,
    /// Whether the sender set the initiator (`I`) flag.
    pub is_initiator: bool,
    /// Acknowledged counter, if the `A` flag was set.
    pub ack_counter: Option<u32>,
    /// Sender's source node id, if the `S` flag was set (for session
    /// establishment this is the peer's random ephemeral node id).
    pub source_node_id: Option<u64>,
    /// Destination address from the secured-message header, if `DSIZ` was set.
    ///
    /// A responder conforming to Matter 1.3+ echoes the initiator's ephemeral
    /// *source* node id here (§4.13.2.1; see [`encode_unsecured_reply`]), which
    /// is what lets an initiator tell its own replies from another attempt's.
    /// `None` is legitimate: Matter 1.0-1.2 required PASE/CASE messages to carry
    /// neither node id (S=0, DSIZ=0), corrected only in 1.3.
    ///
    /// Kept as the full [`DestNodeId`] rather than a bare `u64` so a
    /// group-addressed frame -- malformed on an unsecured session-establishment
    /// exchange -- stays distinguishable from an absent destination.
    pub destination_node_id: Option<DestNodeId>,
    /// Plaintext application payload (post-protocol-header bytes).
    pub payload: Vec<u8>,
}

/// Encode an unsecured (session-id 0, plaintext) message: secured-message
/// header || protocol header || `app_payload`. The `ACK` and `VENDOR` exchange
/// flags are auto-derived by `encode_protocol_header` from `ack`/`protocol_id`.
/// `source_node_id` sets the header's `S` flag and 8-byte field — required on
/// every session-establishment message (spec §4.13.2.1); devices drop frames
/// without it.
#[allow(clippy::too_many_arguments)] // Threaded header inputs; matches encode_outbound's shape.
#[must_use]
pub fn encode_unsecured(
    message_counter: u32,
    exchange_id: u16,
    opcode: u8,
    protocol_id: ProtocolId,
    initiator: bool,
    reliable: bool,
    ack: Option<u32>,
    source_node_id: Option<u64>,
    app_payload: &[u8],
) -> Vec<u8> {
    let mut flags = SecuredMessageFlags::empty();
    if source_node_id.is_some() {
        flags |= SecuredMessageFlags::SOURCE_PRESENT;
    }
    let header = SecuredMessageHeader {
        flags,
        session_id: SessionId(0),
        security_flags: SecurityFlags::empty(),
        message_counter: MessageCounter(message_counter),
        source_node_id: source_node_id.map(NodeId),
        destination_node_id: None,
    };
    let mut buf = encode_header(&header);

    let mut exchange_flags = ExchangeFlags::empty();
    if initiator {
        exchange_flags |= ExchangeFlags::INITIATOR;
    }
    if reliable {
        exchange_flags |= ExchangeFlags::RELIABLE;
    }
    let protocol_header = ProtocolHeader {
        exchange_flags,
        opcode,
        exchange_id,
        protocol_id,
        ack_counter: ack.map(MessageCounter),
    };
    encode_protocol_header(&protocol_header, &mut buf);
    buf.extend_from_slice(app_payload);
    buf
}

/// Encode an unsecured **responder-side reply**: like [`encode_unsecured`]
/// (initiator flag clear) but carrying the DESTINATION node id instead of a
/// source node id.
///
/// Matter Core §4.4.1 / chip `SessionManager::UnauthenticatedMessageDispatch`
/// require an unsecured message to carry **exactly one** of {source,
/// destination} node id: the session-establishment initiator stamps a random
/// ephemeral *source* node id on its messages, and every responder reply must
/// echo that value as the *destination* node id — chip drops replies without
/// it as "malformed unsecure packet". Pass the initiator's
/// [`UnsecuredMessage::source_node_id`] as `destination_node_id`; `None`
/// degrades to a bare header for loopback peers that sent no source id.
#[allow(clippy::too_many_arguments)] // Threaded header inputs; matches encode_unsecured's shape.
#[must_use]
pub fn encode_unsecured_reply(
    message_counter: u32,
    exchange_id: u16,
    opcode: u8,
    protocol_id: ProtocolId,
    reliable: bool,
    ack: Option<u32>,
    destination_node_id: Option<u64>,
    app_payload: &[u8],
) -> Vec<u8> {
    let mut flags = SecuredMessageFlags::empty();
    if destination_node_id.is_some() {
        flags |= SecuredMessageFlags::DEST_UNICAST;
    }
    let header = SecuredMessageHeader {
        flags,
        session_id: SessionId(0),
        security_flags: SecurityFlags::empty(),
        message_counter: MessageCounter(message_counter),
        source_node_id: None,
        destination_node_id: destination_node_id.map(|n| DestNodeId::Node(NodeId(n))),
    };
    let mut buf = encode_header(&header);

    let mut exchange_flags = ExchangeFlags::empty();
    if reliable {
        exchange_flags |= ExchangeFlags::RELIABLE;
    }
    let protocol_header = ProtocolHeader {
        exchange_flags,
        opcode,
        exchange_id,
        protocol_id,
        ack_counter: ack.map(MessageCounter),
    };
    encode_protocol_header(&protocol_header, &mut buf);
    buf.extend_from_slice(app_payload);
    buf
}

/// Decode an unsecured message. Rejects any frame whose session id is non-zero
/// (those are encrypted and must go through `SessionManager`).
///
/// # Errors
///
/// - [`DriverError::UnexpectedSecuredMessage`] if the session id is non-zero.
/// - [`DriverError::Transport`] if the secured-message or protocol header is
///   malformed.
pub fn decode_unsecured(bytes: &[u8]) -> Result<UnsecuredMessage, DriverError> {
    let (msg_header, rest) = decode_header(bytes)?;
    if msg_header.session_id.0 != 0 {
        return Err(DriverError::UnexpectedSecuredMessage(
            msg_header.session_id.0,
        ));
    }
    let (protocol_header, app) = decode_protocol_header(rest)?;
    Ok(UnsecuredMessage {
        message_counter: msg_header.message_counter.0,
        exchange_id: protocol_header.exchange_id,
        opcode: protocol_header.opcode,
        protocol_id: protocol_header.protocol_id,
        is_initiator: protocol_header
            .exchange_flags
            .contains(ExchangeFlags::INITIATOR),
        ack_counter: protocol_header.ack_counter.map(|c| c.0),
        source_node_id: msg_header.source_node_id.map(|n| n.0),
        destination_node_id: msg_header.destination_node_id,
        payload: app.to_vec(),
    })
}

/// Minimal stop-and-wait reliable sender for the unsecured PASE handshake.
///
/// Owns the unsecured message counter and the handshake's exchange id. Each
/// `send_and_recv` transmits one message (initiator, reliable) and awaits the
/// next inbound unsecured message, retransmitting on timeout up to
/// `max_attempts`. This is intentionally simpler than full MRP — the 5 PASE
/// messages are strictly ordered request→response, so stop-and-wait suffices;
/// hardening to MRP-over-unsecured can follow.
pub struct UnsecuredExchange {
    counter: u32,
    exchange_id: u16,
    source_node_id: u64,
    /// Un-jittered wait after each transmission, sized to the peer's advertised
    /// `SII` (see [`handshake_schedule`]).
    schedule: Vec<Duration>,
    /// Pre-ack retransmit budget remaining for the WHOLE handshake, shared
    /// across every `send_and_recv` call on this exchange.
    budget: Duration,
    /// Pre-ack retransmit time already spent on this exchange.
    ///
    /// Tracked separately from `budget` so [`Self::resize_peer_mrp`] can
    /// recompute the remaining budget against a NEW schedule without refunding
    /// time already waited. Deriving it from the initial total instead would
    /// mean re-deriving that total after every resize.
    spent: Duration,
    response_timeout: Duration,
    /// The peer's highest message counter we have already consumed as a real
    /// response on this exchange, if any. Used by `send_and_recv` to drop a
    /// retransmitted prior-step frame (stop-and-wait dedup): the unsecured path
    /// has no `ReplayWindow` (that lives only in the secured `SessionManager`),
    /// so we track it here. `None` until the first response is consumed.
    last_consumed_peer_counter: Option<u32>,
    /// How reliability is provided under this exchange. Under
    /// [`TransportReliability::Mrp`] (UDP) the sender sets the R-flag,
    /// retransmits with stop-and-wait, and emits standalone acks. Under
    /// [`TransportReliability::TransportProvides`] (BTP/in-memory) all three
    /// are suppressed and the sender does a single send + long response wait.
    reliability: TransportReliability,
}

/// Draw a CSPRNG exchange id for one session-establishment handshake.
///
/// Both PASE and CASE used a compile-time `1`, so every handshake this
/// controller ever ran shared an exchange id. A late reply from a previous
/// attempt to the same peer therefore matched the current attempt's filter —
/// see the destination-node-id guard in [`UnsecuredExchange::send_and_recv`],
/// which closes the same window from the other side. Randomising here is
/// defence in depth: unlike the guard it depends on nothing the peer does, and
/// unlike the guard it is probabilistic (1 in 65536 per retry).
///
/// Any `u16` is legal. Matter Core Spec §4.10.2 makes the first exchange id of
/// an initiator a random integer and reserves no value; chip seeds its
/// allocator the same way (`ExchangeMgr.cpp:67`).
///
/// # Errors
///
/// - [`DriverError::Handshake`] if the system CSPRNG fails (ring reports no
///   detail; effectively unreachable on supported platforms).
pub fn random_exchange_id() -> Result<u16, DriverError> {
    let rng = ring::rand::SystemRandom::new();
    let mut bytes = [0u8; 2];
    ring::rand::SecureRandom::fill(&rng, &mut bytes)
        .map_err(|_| DriverError::Handshake("system CSPRNG failure drawing exchange id"))?;
    Ok(u16::from_le_bytes(bytes))
}

impl UnsecuredExchange {
    /// Create an exchange with the given initial message counter, exchange id,
    /// and ephemeral source node id. Deterministic — intended for tests and
    /// trace reproduction; production callers use [`Self::new_ephemeral`].
    ///
    /// Retransmits are sized to the Matter spec defaults (as if the peer
    /// advertised nothing); 30 s post-ack response timeout. Use
    /// [`Self::new_with_mrp`] to size them to a peer's advertised `SII`.
    #[must_use]
    pub fn new(initial_counter: u32, exchange_id: u16, source_node_id: u64) -> Self {
        Self::new_with_mrp(
            initial_counter,
            exchange_id,
            source_node_id,
            MrpConfig::for_peer(None, None, None),
        )
    }

    /// [`Self::new`] with the peer's advertised MRP configuration, sizing the
    /// handshake's retransmits to the device.
    ///
    /// This is the whole point of the type's timing: a Thread sleepy end device
    /// advertising `SII` in the seconds range needs a handshake window
    /// proportional to it. The operational layer has always honoured `SII`; the
    /// handshake that gates the operational layer did not, so the careful sizing
    /// never got a chance to apply and a device that was merely slow to wake
    /// looked unreachable.
    #[must_use]
    pub fn new_with_mrp(
        initial_counter: u32,
        exchange_id: u16,
        source_node_id: u64,
        peer_mrp: MrpConfig,
    ) -> Self {
        let schedule = handshake_schedule(&peer_mrp, MAX_HANDSHAKE_RETRANSMIT_WINDOW);
        Self {
            counter: initial_counter,
            exchange_id,
            source_node_id,
            budget: schedule.iter().copied().sum(),
            spent: Duration::ZERO,
            schedule,
            response_timeout: UNSECURED_RESPONSE_TIMEOUT,
            last_consumed_peer_counter: None,
            // Default to the historical UDP/MRP behavior; the BTP path opts in
            // via `new_ephemeral_with` / `run_pase_with`.
            reliability: TransportReliability::Mrp,
        }
    }

    /// Create an exchange with a CSPRNG-seeded message counter and ephemeral
    /// source node id — the production constructor.
    ///
    /// - Counter: random 28-bit value + 1 (Matter Core Spec §4.6.1.1, matching
    ///   matter.js's `MessageCounter` initialisation).
    /// - Source node id: random, masked to the low 60 bits (keeps it inside
    ///   the operational node-id range `1..=0xFFFF_FFEF_FFFF_FFFF` required
    ///   for ephemeral ids, §4.13.2.1) and clamped nonzero.
    ///
    /// # Errors
    ///
    /// - [`DriverError::Handshake`] if the system CSPRNG fails (ring reports
    ///   no detail; this is effectively unreachable on supported platforms).
    pub fn new_ephemeral(exchange_id: u16) -> Result<Self, DriverError> {
        Self::new_ephemeral_with(
            exchange_id,
            TransportReliability::Mrp,
            MrpConfig::for_peer(None, None, None),
        )
    }

    /// Like [`Self::new_ephemeral`], but with an explicit
    /// [`TransportReliability`]. Under [`TransportReliability::TransportProvides`]
    /// the exchange does a single send + 30 s response wait (no 300 ms × 5
    /// stop-and-wait), never sets the R-flag, never attaches an ack, and
    /// [`Self::send_standalone_ack`] becomes a no-op — the reliability the
    /// suppressed MRP would have provided is supplied by the transport (Matter
    /// spec §4.12: MRP off over BLE/BTP).
    ///
    /// # Errors
    ///
    /// - [`DriverError::Handshake`] if the system CSPRNG fails (ring reports
    ///   no detail; this is effectively unreachable on supported platforms).
    pub fn new_ephemeral_with(
        exchange_id: u16,
        reliability: TransportReliability,
        peer_mrp: MrpConfig,
    ) -> Result<Self, DriverError> {
        let rng = ring::rand::SystemRandom::new();
        let mut bytes = [0u8; 12];
        ring::rand::SecureRandom::fill(&rng, &mut bytes).map_err(|_| {
            DriverError::Handshake("system CSPRNG failure seeding unsecured session")
        })?;
        // First 4 bytes → counter, remaining 8 → ephemeral node id.
        let counter_seed: [u8; 4] = [bytes[0], bytes[1], bytes[2], bytes[3]];
        let node_seed: [u8; 8] = [
            bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11],
        ];
        let counter = (u32::from_le_bytes(counter_seed) & 0x0FFF_FFFF) + 1;
        let source_node_id = (u64::from_le_bytes(node_seed) & 0x0FFF_FFFF_FFFF_FFFF).max(1);
        let mut exch = Self::new_with_mrp(counter, exchange_id, source_node_id, peer_mrp);
        exch.reliability = reliability;
        Ok(exch)
    }

    /// Send a **terminal** handshake message reliably and make a bounded effort
    /// to see it acknowledged. Returns whether an ack was observed.
    ///
    /// This is the shape of the CASE resumption path's closing message: after
    /// `Sigma2_Resume` the initiator has no further message to exchange, only a
    /// success `StatusReport` to deliver. [`Self::send_and_recv`] cannot express
    /// that — it waits for a *response*, and a standalone ack merely flips it to
    /// the long response timeout, so the call would end in
    /// [`DriverError::Timeout`] on a handshake that in fact succeeded.
    ///
    /// **A missing ack is not a failure.** By the time this is called both sides
    /// have derived the session; the peer processed our `Sigma2_Resume` reply
    /// and is waiting only for confirmation. Failing the connect because the
    /// final ack was lost would discard a working session. chip does not wait
    /// at all here — it calls `SendStatusReport` and immediately reports the
    /// session established, leaving retransmission to the MRP layer. We
    /// retransmit within the exchange's remaining budget, which is strictly
    /// more robust, and then return `Ok(false)` rather than erroring.
    ///
    /// Under [`TransportReliability::TransportProvides`] the transport is
    /// already reliable, so this sends exactly once and returns `Ok(false)`
    /// without waiting.
    ///
    /// # Errors
    ///
    /// [`DriverError::Io`] if the datagram cannot be sent.
    pub async fn send_and_await_ack<T: AsyncDatagram>(
        &mut self,
        transport: &T,
        peer: SocketAddr,
        opcode: u8,
        app_payload: &[u8],
        ack: Option<u32>,
    ) -> Result<bool, DriverError> {
        let counter = self.counter;
        self.counter = self.counter.wrapping_add(1);
        let transport_provides = self.reliability == TransportReliability::TransportProvides;
        let wire = encode_unsecured(
            counter,
            self.exchange_id,
            opcode,
            ProtocolId::SECURE_CHANNEL,
            true,
            !transport_provides,
            if transport_provides { None } else { ack },
            Some(self.source_node_id),
            app_payload,
        );

        transport.send_to(&wire, peer).await?;
        if transport_provides {
            return Ok(false);
        }

        let mut attempts: u8 = 0;
        loop {
            let wait = match self.schedule.get(usize::from(attempts)) {
                Some(d) => (*d).min(self.budget),
                None => return Ok(false),
            };
            match tokio::time::timeout(wait, transport.recv_from()).await {
                Ok(recv) => {
                    let (packet, _from) = recv?;
                    // Same skip rules as `send_and_recv_any`: secured
                    // stragglers, foreign exchanges, and frames addressed to a
                    // previous attempt's ephemeral node id are not ours.
                    if packet.len() >= 3 && (packet[1] != 0 || packet[2] != 0) {
                        continue;
                    }
                    let Ok(msg) = decode_unsecured(&packet) else {
                        continue;
                    };
                    if msg.exchange_id != self.exchange_id {
                        continue;
                    }
                    if let Some(dest) = msg.destination_node_id {
                        if dest != DestNodeId::Node(NodeId(self.source_node_id)) {
                            continue;
                        }
                    }
                    if msg.protocol_id == ProtocolId::SECURE_CHANNEL
                        && msg.opcode == OPCODE_MRP_STANDALONE_ACK
                    {
                        return Ok(true);
                    }
                    // Anything else (a retransmit of the peer's last message,
                    // because our report has not reached it) is ignored; the
                    // retransmit below covers it.
                }
                Err(_elapsed) => {
                    self.budget = self.budget.saturating_sub(wait);
                    self.spent += wait;
                    attempts += 1;
                    if usize::from(attempts) >= self.schedule.len() || self.budget.is_zero() {
                        return Ok(false);
                    }
                    transport.send_to(&wire, peer).await?;
                }
            }
        }
    }

    /// Re-size this exchange's retransmit schedule to `peer_mrp`, mid-handshake.
    ///
    /// Used when the peer states its own MRP timings during the handshake — the
    /// `SessionParameters` element in Sigma2 / `Sigma2_Resume` — which is more
    /// trustworthy than the mDNS TXT record the exchange started from: the
    /// advertisement can be stale, cached, or missing entirely.
    ///
    /// chip does exactly this, applying Sigma2's parameters after validating it
    /// and before sending Sigma3 (`CASESession::HandleSigma2`), so Sigma3's
    /// retransmits are sized to what the device just said rather than to what
    /// mDNS said earlier. Without it a device with no operational mDNS record
    /// gets spec-default timings for the rest of the handshake no matter what
    /// it advertises in-band.
    ///
    /// **Time already waited is never refunded.** The new budget is the new
    /// schedule's total minus what this exchange has already spent, so a peer
    /// cannot extend a handshake indefinitely by advertising a large `SII`
    /// after we have already burned most of the window. A resize that shrinks
    /// the total below what is already spent leaves zero budget, ending the
    /// handshake at the next timeout rather than retroactively failing it.
    pub fn resize_peer_mrp(&mut self, peer_mrp: MrpConfig) {
        let schedule = handshake_schedule(&peer_mrp, MAX_HANDSHAKE_RETRANSMIT_WINDOW);
        let total: Duration = schedule.iter().copied().sum();
        self.budget = total.saturating_sub(self.spent);
        self.schedule = schedule;
    }

    /// Send an MRP standalone acknowledgement (`SecureChannel 0x10`) for the
    /// peer's reliable message `peer_counter` — used to ack the handshake's
    /// closing `StatusReport` so the device stops retransmitting it. Fire and
    /// forget (an ack is itself never acked); advances the message counter.
    ///
    /// Under [`TransportReliability::TransportProvides`] MRP is off (spec
    /// §4.12), so this is a no-op: it sends nothing and leaves the message
    /// counter unchanged.
    ///
    /// # Errors
    ///
    /// - [`DriverError::Io`] if the datagram send fails.
    pub async fn send_standalone_ack<T: AsyncDatagram>(
        &mut self,
        transport: &T,
        peer: SocketAddr,
        peer_counter: u32,
    ) -> Result<(), DriverError> {
        // Under a reliable transport MRP is off (spec §4.12): there is nothing
        // to acknowledge at the exchange layer, so this is a no-op that sends
        // nothing and does not advance the message counter.
        if self.reliability == TransportReliability::TransportProvides {
            return Ok(());
        }
        let counter = self.counter;
        self.counter = self.counter.wrapping_add(1);
        let wire = encode_unsecured(
            counter,
            self.exchange_id,
            OPCODE_MRP_STANDALONE_ACK,
            ProtocolId::SECURE_CHANNEL,
            true,
            false,
            Some(peer_counter),
            Some(self.source_node_id),
            &[],
        );
        transport.send_to(&wire, peer).await?;
        // M6 wire-trace capture: feeds JsonlLayer / cargo xtask trace-diff.
        #[cfg(feature = "tracing")]
        tracing::debug!(
            target: "matter_wire",
            dir = "tx",
            session_id = 0_u64,
            exchange_id = u64::from(self.exchange_id),
            protocol = u64::from(ProtocolId::SECURE_CHANNEL.protocol),
            opcode = u64::from(OPCODE_MRP_STANDALONE_ACK),
            payload = crate::hexdump::payload(&[]),
            "wire"
        );
        Ok(())
    }

    /// Send one unsecured message (initiator, reliable) and await the
    /// exchange's response, retransmitting on timeout. `ack` piggybacks the
    /// previous message's counter when the caller has one to acknowledge.
    ///
    /// `opcode` is the opcode being sent; `expected_opcode` is the
    /// `SecureChannel` opcode of the awaited next-step response (e.g.
    /// `PBKDFParamResponse 0x21` after sending `PBKDFParamRequest 0x20`, or
    /// `StatusReport 0x40` after the terminal `Pake3`/`Sigma3`).
    ///
    /// Frames that are NOT the awaited next step are skipped rather than
    /// returned, so the handshake waits for the real frame instead of aborting:
    ///
    /// - MRP standalone acks (`SecureChannel 0x10`) — a standalone ack stops
    ///   further retransmission and extends the wait to the response timeout.
    /// - Frames from other exchanges (mismatched exchange id).
    /// - **Stale prior-step responses**: an in-flight MRP retransmit of a
    ///   previous step's response (the peer resends reliable frames until it
    ///   sees our next message) matches the exchange id but is not the awaited
    ///   opcode. The unsecured path has no `ReplayWindow`, so these are dropped
    ///   here both by opcode (not `expected_opcode`) and, as a backstop, by
    ///   message-counter dedup (counter `<=` the last consumed response).
    ///
    /// A terminal `StatusReport 0x40` is always surfaced (it carries a possible
    /// rejection); a genuinely unexpected frame that never resolves still fails
    /// via the response deadline rather than hanging.
    ///
    /// # Errors
    ///
    /// - [`DriverError::Io`] if a datagram send/recv fails.
    /// - [`DriverError::Transport`] / [`DriverError::UnexpectedSecuredMessage`]
    ///   if the reply does not decode as an unsecured message.
    /// - [`DriverError::Timeout`] if no reply arrives within `max_attempts`.
    pub async fn send_and_recv<T: AsyncDatagram>(
        &mut self,
        transport: &T,
        peer: SocketAddr,
        opcode: u8,
        expected_opcode: u8,
        app_payload: &[u8],
        ack: Option<u32>,
    ) -> Result<UnsecuredMessage, DriverError> {
        self.send_and_recv_any(
            transport,
            peer,
            opcode,
            &[expected_opcode],
            app_payload,
            ack,
        )
        .await
    }

    /// [`Self::send_and_recv`] for a step whose response may legitimately be
    /// one of several opcodes.
    ///
    /// The single-opcode gate is not merely inconvenient for CASE resumption,
    /// it is silently wrong: a frame that is neither the awaited opcode nor a
    /// `StatusReport` is *dropped* by the loop below, so a `Sigma2_Resume`
    /// arriving where only `Sigma2` was expected never reaches the caller and
    /// the exchange ends in [`DriverError::Timeout`] — with nothing to say the
    /// response had in fact arrived.
    ///
    /// # Errors
    ///
    /// Same as [`Self::send_and_recv`].
    // Cohesive single retransmit/recv loop: the length comes from the
    // cfg-gated wire-tracing blocks and the documented per-frame skip cases
    // (secured straggler, foreign exchange, standalone-ack, counter dedup,
    // opcode gate). Splitting it would force threading the loop state
    // (acked/attempts/counter) through a helper for no readability gain.
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::too_many_arguments)]
    pub async fn send_and_recv_any<T: AsyncDatagram>(
        &mut self,
        transport: &T,
        peer: SocketAddr,
        opcode: u8,
        expected_opcodes: &[u8],
        app_payload: &[u8],
        ack: Option<u32>,
    ) -> Result<UnsecuredMessage, DriverError> {
        let counter = self.counter;
        self.counter = self.counter.wrapping_add(1);
        // Under TransportProvides (BTP) the transport carries reliability, so
        // MRP is off (spec §4.12): the R-flag is never set and no ack is
        // piggybacked. Under Mrp both are used exactly as before.
        let transport_provides = self.reliability == TransportReliability::TransportProvides;
        let reliable_bit = !transport_provides;
        let ack_field = if transport_provides { None } else { ack };
        let wire = encode_unsecured(
            counter,
            self.exchange_id,
            opcode,
            ProtocolId::SECURE_CHANNEL,
            true,
            reliable_bit,
            ack_field,
            Some(self.source_node_id),
            app_payload,
        );

        #[cfg(feature = "tracing")]
        tracing::debug!(
            opcode = format_args!("{opcode:#04x}"),
            exchange_id = self.exchange_id,
            len = wire.len(),
            wire = crate::hexdump::payload(&wire),
            "unsecured send"
        );
        // M6 wire-trace capture: feeds JsonlLayer / cargo xtask trace-diff.
        #[cfg(feature = "tracing")]
        tracing::debug!(
            target: "matter_wire",
            dir = "tx",
            session_id = 0_u64,
            exchange_id = u64::from(self.exchange_id),
            protocol = u64::from(ProtocolId::SECURE_CHANNEL.protocol),
            opcode = u64::from(opcode),
            payload = crate::hexdump::payload(app_payload),
            "wire"
        );
        let mut attempts: u8 = 0;
        // Once the peer acknowledges delivery (standalone ack), stop
        // retransmitting and switch to the longer response timeout — the
        // device has the message and is computing its reply (observed: real
        // devices standalone-ack a PASE message before the SPAKE2+ response).
        let mut acked = false;
        // TransportProvides sends exactly once (the transport is reliable), so
        // track whether the single send has gone out.
        let mut sent = false;
        loop {
            // Send decision: MRP retransmits the same bytes until the peer
            // acks; TransportProvides sends once and never retransmits.
            let should_send = if transport_provides { !sent } else { !acked };
            if should_send {
                transport.send_to(&wire, peer).await?;
                sent = true;
            }
            // Wait decision: TransportProvides always waits the full response
            // deadline (no retransmit). MRP waits the short retransmit interval
            // until the peer acks, then the response deadline.
            let wait = if transport_provides || acked {
                self.response_timeout
            } else {
                // Peer-sized: the wait after the transmissions made so far,
                // clamped to whatever pre-ack budget this handshake has left.
                match self.schedule.get(usize::from(attempts)) {
                    Some(d) => (*d).min(self.budget),
                    None => {
                        return Err(DriverError::Timeout {
                            exchange_id: self.exchange_id,
                        })
                    }
                }
            };
            match tokio::time::timeout(wait, transport.recv_from()).await {
                Ok(recv) => {
                    let (packet, _from) = recv?;
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        len = packet.len(),
                        head = crate::hexdump::payload(&packet[..packet.len().min(24)]),
                        "unsecured recv"
                    );
                    // Secured-session stragglers (session id ≠ 0 at header
                    // offset 1, e.g. an MRP retransmit of the last PASE-
                    // session response whose standalone ack the device has
                    // not seen yet) are not part of this unsecured exchange —
                    // skip, don't abort (observed: Tapo P110M, M6.6.5).
                    if packet.len() >= 3 && (packet[1] != 0 || packet[2] != 0) {
                        continue;
                    }
                    let msg = decode_unsecured(&packet)?;
                    if msg.exchange_id != self.exchange_id {
                        // Stray frame from another exchange — not ours.
                        continue;
                    }
                    // Cross-attempt guard. A responder conforming to Matter 1.3+
                    // echoes our ephemeral source node id as the destination, so
                    // a frame addressed to a DIFFERENT ephemeral id is a late
                    // reply to an earlier handshake attempt from this socket to
                    // this peer. Consuming it would decrypt attempt N-1's Sigma2
                    // with attempt N's ephemeral secret — an AEAD failure,
                    // deterministically, on every retry, for as long as the
                    // device stays slow enough to lose the first race. A group
                    // destination is malformed here and is rejected the same way.
                    //
                    // MUST precede the standalone-ack arm below: a stray *ack*
                    // from the previous attempt would otherwise set `acked`,
                    // stopping this attempt's retransmits and switching it to the
                    // long response wait — the same bug, quieter.
                    //
                    // An ABSENT destination is ACCEPTED. Matter 1.0-1.2 required
                    // PASE/CASE messages to carry neither node id (S=0, DSIZ=0);
                    // that was corrected only in 1.3 (connectedhomeip#33518), so
                    // a pre-1.3 device legitimately sends nothing to match on.
                    if let Some(dest) = msg.destination_node_id {
                        if dest != DestNodeId::Node(NodeId(self.source_node_id)) {
                            continue;
                        }
                    }
                    if msg.protocol_id == ProtocolId::SECURE_CHANNEL
                        && msg.opcode == OPCODE_MRP_STANDALONE_ACK
                    {
                        acked = true;
                        continue;
                    }
                    // Counter dedup backstop: a retransmit of a prior-step
                    // response carries a counter we have already consumed on
                    // this exchange. The unsecured path has no ReplayWindow, so
                    // drop it here (stop-and-wait: each step's response counter
                    // is strictly greater than the last).
                    if let Some(last) = self.last_consumed_peer_counter {
                        if msg.message_counter <= last {
                            continue;
                        }
                    }
                    // Opcode gate: skip any frame that is neither the awaited
                    // next-step opcode nor a terminal StatusReport (a rejection
                    // we must surface). A retransmitted previous-step response
                    // (e.g. PBKDFParamResponse 0x21 while awaiting Pake2 0x23)
                    // is dropped here rather than returned — returning it would
                    // trip the require_handshake_opcode gate and abort the
                    // handshake (observed cause of intermittent commissioning
                    // failures on lossy/duplicating networks).
                    if msg.protocol_id == ProtocolId::SECURE_CHANNEL
                        && !expected_opcodes.contains(&msg.opcode)
                        && msg.opcode != OPCODE_STATUS_REPORT
                    {
                        continue;
                    }
                    // This is the awaited response (or a terminal StatusReport):
                    // record its counter so any later retransmit of it is
                    // deduped on the next step.
                    self.last_consumed_peer_counter = Some(msg.message_counter);
                    // M6 wire-trace capture: feeds JsonlLayer / cargo xtask trace-diff.
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        target: "matter_wire",
                        dir = "rx",
                        session_id = 0_u64,
                        exchange_id = u64::from(msg.exchange_id),
                        protocol = u64::from(msg.protocol_id.protocol),
                        opcode = u64::from(msg.opcode),
                        payload = crate::hexdump::payload(&msg.payload),
                        "wire"
                    );
                    return Ok(msg);
                }
                Err(_elapsed) => {
                    if transport_provides || acked {
                        // TransportProvides: the single send's response never
                        // arrived. MRP-acked: delivery was acknowledged but no
                        // response came (the peer abandoned the exchange, and a
                        // retransmit would only be deduplicated). Either way,
                        // surface a timeout.
                        return Err(DriverError::Timeout {
                            exchange_id: self.exchange_id,
                        });
                    }
                    // Spend the wait we just served from the handshake-wide
                    // budget, so a peer advertising a huge SII cannot stretch
                    // the connect step by step past the cap.
                    self.budget = self.budget.saturating_sub(wait);
                    self.spent += wait;
                    attempts += 1;
                    if usize::from(attempts) >= self.schedule.len() || self.budget.is_zero() {
                        return Err(DriverError::Timeout {
                            exchange_id: self.exchange_id,
                        });
                    }
                    // loop → retransmit the same bytes.
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use matter_transport::ProtocolId;

    use crate::driver::datagram::{AsyncDatagram, InMemoryDatagram};

    use super::*;

    #[test]
    fn unsecured_roundtrip_preserves_fields() {
        // PASE_Pake1 (0x22), initiator, reliable, acking counter 7.
        let wire = encode_unsecured(
            0x8000_0001,
            42,
            0x22,
            ProtocolId::SECURE_CHANNEL,
            true,
            true,
            Some(7),
            None,
            b"pake1-bytes",
        );
        let msg = decode_unsecured(&wire).unwrap();
        assert_eq!(msg.message_counter, 0x8000_0001);
        assert_eq!(msg.exchange_id, 42);
        assert_eq!(msg.opcode, 0x22);
        assert_eq!(msg.protocol_id, ProtocolId::SECURE_CHANNEL);
        assert!(msg.is_initiator);
        assert_eq!(msg.ack_counter, Some(7));
        assert_eq!(msg.payload, b"pake1-bytes");
    }

    #[test]
    fn unsecured_rejects_secured_session_id() {
        // Hand-build a frame with a non-zero session id via the transport
        // primitive, then assert decode rejects it.
        use matter_transport::{
            encode_header, MessageCounter, SecuredMessageFlags, SecuredMessageHeader,
            SecurityFlags, SessionId,
        };
        let hdr = SecuredMessageHeader {
            flags: SecuredMessageFlags::empty(),
            session_id: SessionId(5), // non-zero ⇒ secured
            security_flags: SecurityFlags::empty(),
            message_counter: MessageCounter(1),
            source_node_id: None,
            destination_node_id: None,
        };
        let mut wire = encode_header(&hdr);
        wire.extend_from_slice(&[0u8; 6]); // minimal protocol-header bytes
        let err = decode_unsecured(&wire).unwrap_err();
        assert!(matches!(err, DriverError::UnexpectedSecuredMessage(5)));
    }

    #[tokio::test]
    async fn unsecured_send_and_recv_roundtrips() {
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new(1, 7, 0xE0E0);

        let controller = exch.send_and_recv(
            &ctrl_io, dev_addr, 0x20, /* PBKDFParamRequest */
            0x21, /* awaiting PBKDFParamResponse */
            b"req", None,
        );

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let msg = decode_unsecured(&pkt).unwrap();
            assert_eq!(msg.opcode, 0x20);
            assert_eq!(msg.payload, b"req");
            // Reply PBKDFParamResponse (0x21), acking the request's counter.
            let reply = encode_unsecured(
                100,
                msg.exchange_id,
                0x21,
                ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(msg.message_counter),
                None,
                b"resp",
            );
            dev_io.send_to(&reply, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        let got = got.unwrap();
        assert_eq!(got.opcode, 0x21);
        assert_eq!(got.payload, b"resp");
    }

    #[tokio::test]
    async fn unsecured_standalone_ack_has_expected_shape() {
        let (a, b) = InMemoryDatagram::pair();
        let b_addr = b.local_addr();
        let mut exch = UnsecuredExchange::new(5, 9, 0xE0E0);
        exch.send_standalone_ack(&a, b_addr, 7).await.unwrap();
        let (pkt, _) = b.recv_from().await.unwrap();
        let msg = decode_unsecured(&pkt).unwrap();
        assert_eq!(msg.opcode, 0x10);
        assert_eq!(msg.payload, b"");
        assert_eq!(msg.message_counter, 5);
        assert_eq!(msg.ack_counter, Some(7));
        assert!(msg.is_initiator);
        assert_eq!(msg.source_node_id, Some(0xE0E0));
    }

    #[test]
    fn status_report_parses_success_and_failure() {
        let mk = |general: u16, code: u16| UnsecuredMessage {
            message_counter: 1,
            exchange_id: 1,
            opcode: 0x40,
            protocol_id: ProtocolId::SECURE_CHANNEL,
            is_initiator: false,
            ack_counter: None,
            source_node_id: None,
            destination_node_id: None,
            payload: {
                let mut b = Vec::new();
                b.extend_from_slice(&general.to_le_bytes());
                b.extend_from_slice(&0u32.to_le_bytes());
                b.extend_from_slice(&code.to_le_bytes());
                b
            },
        };
        let ok = parse_status_report(&mk(0, 0)).unwrap();
        assert!(ok.is_session_establishment_success());
        let no = parse_status_report(&mk(1, 0x0002)).unwrap();
        assert!(!no.is_session_establishment_success());
        assert_eq!(no.general_code, 1);
        assert_eq!(no.protocol_code, 0x0002);
        // Wrong opcode is rejected.
        let mut wrong = mk(0, 0);
        wrong.opcode = 0x21;
        assert!(parse_status_report(&wrong).is_err());
    }

    #[test]
    fn unsecured_encode_carries_source_node_id() {
        // Matter Core Spec §4.13.2.1: session-establishment messages SHALL set
        // the S flag and carry a random ephemeral source node id. Real devices
        // (observed: Tapo P110M, M6.6.5 validation) silently drop unsecured
        // frames without it.
        let wire = encode_unsecured(
            1,
            7,
            0x20,
            ProtocolId::SECURE_CHANNEL,
            true,
            true,
            None,
            Some(0x1122_3344_5566_7788),
            b"req",
        );
        // Byte 0 message flags: S bit (0b100) set.
        assert_eq!(wire[0] & 0b0000_0100, 0b0000_0100, "S flag must be set");
        // Bytes 8..16: the source node id, little-endian.
        assert_eq!(wire[8..16], 0x1122_3344_5566_7788u64.to_le_bytes());
        let msg = decode_unsecured(&wire).unwrap();
        assert_eq!(msg.source_node_id, Some(0x1122_3344_5566_7788));
    }

    #[tokio::test]
    async fn unsecured_exchange_frames_carry_source_node_id() {
        let (a, b) = InMemoryDatagram::pair();
        let b_addr = b.local_addr();
        let mut exch = UnsecuredExchange::new(5, 9, 0xABCD);
        exch.send_standalone_ack(&a, b_addr, 3).await.unwrap();
        let (pkt, _) = b.recv_from().await.unwrap();
        let msg = decode_unsecured(&pkt).unwrap();
        assert_eq!(msg.source_node_id, Some(0xABCD));
    }

    #[tokio::test]
    async fn unsecured_new_ephemeral_seeds_counter_and_node_id() {
        // The production constructor must seed both the unsecured message
        // counter (spec §4.6.1.1: random 28-bit + 1) and the ephemeral source
        // node id from the CSPRNG.
        let (a, b) = InMemoryDatagram::pair();
        let b_addr = b.local_addr();
        let mut exch = UnsecuredExchange::new_ephemeral(9).unwrap();
        exch.send_standalone_ack(&a, b_addr, 3).await.unwrap();
        let (pkt, _) = b.recv_from().await.unwrap();
        let msg = decode_unsecured(&pkt).unwrap();
        let node_id = msg.source_node_id.expect("ephemeral node id present");
        assert_ne!(node_id, 0, "node id must be nonzero (operational range)");
        assert_ne!(msg.message_counter, 0, "counter must be nonzero");
        assert!(
            msg.message_counter <= 0x0FFF_FFFF + 1,
            "counter seeded as random 28-bit + 1"
        );
    }

    #[tokio::test]
    async fn unsecured_send_and_recv_skips_standalone_ack() {
        // Real devices (observed: Tapo P110M, M6.6.5 validation) ack a reliable
        // PASE message with an MRP *standalone ack* (SecureChannel 0x10) first,
        // then send the actual response as a separate message. send_and_recv
        // must not surface the ack as if it were the response.
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new(1, 7, 0xE0E0);

        let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x20, 0x21, b"req", None);

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let msg = decode_unsecured(&pkt).unwrap();
            // Standalone ack: SecureChannel 0x10, no payload, acks the request.
            let ack = encode_unsecured(
                100,
                msg.exchange_id,
                0x10,
                ProtocolId::SECURE_CHANNEL,
                false,
                false,
                Some(msg.message_counter),
                None,
                b"",
            );
            dev_io.send_to(&ack, ctrl_addr).await.unwrap();
            // Then the real PBKDFParamResponse on the same exchange.
            let reply = encode_unsecured(
                101,
                msg.exchange_id,
                0x21,
                ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(msg.message_counter),
                None,
                b"resp",
            );
            dev_io.send_to(&reply, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        let got = got.unwrap();
        assert_eq!(got.opcode, 0x21, "standalone ack must be skipped");
        assert_eq!(got.payload, b"resp");
    }

    /// A late reply from a PREVIOUS handshake attempt must not be consumed by
    /// the current one.
    ///
    /// Regression guard for the reported failure: two CASE attempts to the same
    /// sleepy peer from the same socket share an exchange id (it was a
    /// compile-time `1`), so attempt 1's Sigma2 -- retransmitted reliably under
    /// MRP after attempt 1 had already timed out -- matched attempt 2's filter
    /// on session id, exchange id, opcode, and the counter dedup (attempt 2's
    /// `last_consumed_peer_counter` is `None`). It was then decrypted with
    /// attempt 2's ephemeral secret, which cannot work: AEAD failure,
    /// deterministically, on every retry. The destination node id is what
    /// distinguishes them -- a conformant responder echoes the initiator's
    /// ephemeral source node id, and attempt 1's carries attempt 1's.
    #[tokio::test]
    async fn send_and_recv_rejects_a_reply_addressed_to_another_attempt() {
        const OUR_EPHEMERAL: u64 = 0xE0E0;
        const PRIOR_ATTEMPT_EPHEMERAL: u64 = 0xB00B;

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new(1, 7, OUR_EPHEMERAL);

        let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x30, 0x31, b"sigma1", None);

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let msg = decode_unsecured(&pkt).unwrap();
            // The previous attempt's Sigma2, arriving late. Right opcode, right
            // exchange, fresh counter -- only the destination betrays it.
            let stale = encode_unsecured_reply(
                100,
                msg.exchange_id,
                0x31,
                ProtocolId::SECURE_CHANNEL,
                true,
                Some(msg.message_counter),
                Some(PRIOR_ATTEMPT_EPHEMERAL),
                b"stale-sigma2",
            );
            dev_io.send_to(&stale, ctrl_addr).await.unwrap();
            // This attempt's real Sigma2.
            let ours = encode_unsecured_reply(
                101,
                msg.exchange_id,
                0x31,
                ProtocolId::SECURE_CHANNEL,
                true,
                Some(msg.message_counter),
                Some(OUR_EPHEMERAL),
                b"our-sigma2",
            );
            dev_io.send_to(&ours, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        let got = got.unwrap();
        assert_eq!(
            got.payload, b"our-sigma2",
            "a reply addressed to a prior attempt's ephemeral id must be dropped"
        );
    }

    /// The guard must sit BEFORE the standalone-ack arm.
    ///
    /// A stray *ack* from the previous attempt is the same bug, quieter: it
    /// would set `acked`, which stops this attempt retransmitting and switches
    /// it to the long post-ack response wait. The device here refuses to answer
    /// until it has seen a retransmit, so if the misaddressed ack were consumed
    /// this test would hang rather than fail.
    #[tokio::test]
    async fn send_and_recv_rejects_a_standalone_ack_addressed_to_another_attempt() {
        const OUR_EPHEMERAL: u64 = 0xE0E0;
        const PRIOR_ATTEMPT_EPHEMERAL: u64 = 0xB00B;

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new(1, 7, OUR_EPHEMERAL);
        // Keep the retransmit the test waits on short.
        exch.schedule = vec![Duration::from_millis(50); 5];
        exch.budget = Duration::from_millis(250);

        let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x30, 0x31, b"sigma1", None);

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let msg = decode_unsecured(&pkt).unwrap();
            let stale_ack = encode_unsecured_reply(
                100,
                msg.exchange_id,
                OPCODE_MRP_STANDALONE_ACK,
                ProtocolId::SECURE_CHANNEL,
                false,
                Some(msg.message_counter),
                Some(PRIOR_ATTEMPT_EPHEMERAL),
                b"",
            );
            dev_io.send_to(&stale_ack, ctrl_addr).await.unwrap();
            // Only answer once the controller has proved it is still
            // retransmitting -- i.e. the misaddressed ack did not land.
            //
            // Bounded, and deliberately so: if the guard were missing the ack
            // would set `acked`, no retransmit would ever come, and an unbounded
            // wait here would hang the suite instead of failing it. The bound is
            // ~20x the 50 ms retransmit interval, so it cannot flake on a loaded
            // machine while still failing fast when the guard regresses.
            let retx = tokio::time::timeout(Duration::from_secs(1), dev_io.recv_from())
                .await
                .expect("controller must keep retransmitting: a standalone ack addressed to another attempt must not be consumed")
                .unwrap()
                .0;
            let retx = decode_unsecured(&retx).unwrap();
            let ours = encode_unsecured_reply(
                101,
                retx.exchange_id,
                0x31,
                ProtocolId::SECURE_CHANNEL,
                true,
                Some(retx.message_counter),
                Some(OUR_EPHEMERAL),
                b"our-sigma2",
            );
            dev_io.send_to(&ours, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, b"our-sigma2");
    }

    /// The handshake schedule, sized to what the peer advertises.
    ///
    /// A silent peer gets the spec defaults; a Thread sleepy end device
    /// advertising `SII=2000` gets a window proportional to it; and a peer
    /// advertising the spec ceiling (1 h) is bounded by the cap rather than
    /// wedging the connect for hours. Hand-computed, so a change to the formula
    /// has to update this deliberately.
    #[test]
    fn handshake_schedule_is_sized_to_the_peer() {
        let cap = MAX_HANDSHAKE_RETRANSMIT_WINDOW;

        // Peer advertises nothing → 500 ms idle base, margin 550.
        let silent = handshake_schedule(&MrpConfig::for_peer(None, None, None), cap);
        assert_eq!(
            silent,
            [550, 550, 880, 1408, 2252].map(Duration::from_millis),
            "silent peer gets the spec-default schedule"
        );
        assert_eq!(silent.iter().sum::<Duration>(), Duration::from_millis(5640));

        // Thread SED advertising SII=2000 → margin 2201.
        let sleepy = handshake_schedule(
            &MrpConfig::for_peer(Some(Duration::from_millis(2000)), None, None),
            cap,
        );
        assert_eq!(
            sleepy,
            [2201, 2201, 3521, 5634, 9015].map(Duration::from_millis),
            "a sleepy peer gets a window proportional to its advertised SII"
        );
        assert_eq!(
            sleepy.iter().sum::<Duration>(),
            Duration::from_millis(22572),
            "22.6 s, versus the 1.5 s every device used to get"
        );

        // Spec ceiling: one entry, clamped, and the connect still ends.
        let ceiling = handshake_schedule(
            &MrpConfig::for_peer(Some(Duration::from_secs(3600)), None, None),
            cap,
        );
        assert_eq!(ceiling, [cap], "a ceiling SII is clamped to a single wait");
        assert_eq!(ceiling.iter().sum::<Duration>(), cap);
    }

    /// Invariants that must hold for ANY advertised `SII`, including the
    /// degenerate ones: the schedule is never empty (a handshake always gets at
    /// least one retry), never longer than the transmission budget, and never
    /// sums past the cap.
    #[test]
    fn handshake_schedule_invariants_hold_for_any_sii() {
        let cap = MAX_HANDSHAKE_RETRANSMIT_WINDOW;
        // A coarse sweep across the whole legal range, plus the boundaries.
        let mut sii_ms = vec![0u64, 1, 2, 499, 500, 501, 2000, 59_999, 60_000, 60_001];
        sii_ms.extend((0..=3_600_000u64).step_by(97_297));
        for ms in sii_ms {
            let cfg = MrpConfig::for_peer(Some(Duration::from_millis(ms)), None, None);
            let sched = handshake_schedule(&cfg, cap);
            assert!(!sched.is_empty(), "SII={ms}: schedule must never be empty");
            assert!(
                sched.len() <= usize::from(cfg.max_transmissions),
                "SII={ms}: {} entries exceeds max_transmissions",
                sched.len()
            );
            let total: Duration = sched.iter().sum();
            assert!(total <= cap, "SII={ms}: total {total:?} exceeds the cap");
        }
    }

    /// The cap is a budget for the WHOLE handshake, not per step.
    ///
    /// Sigma1→Sigma2 and Sigma3→StatusReport each call `send_and_recv`, so a
    /// per-step allowance would permit two full windows plus two 30 s response
    /// deadlines -- around three minutes holding a connect slot.
    #[test]
    fn the_retransmit_budget_is_shared_across_the_whole_handshake() {
        let exch = UnsecuredExchange::new_with_mrp(
            1,
            7,
            0xE0E0,
            MrpConfig::for_peer(Some(Duration::from_secs(30)), None, None),
        );
        assert!(
            exch.budget <= MAX_HANDSHAKE_RETRANSMIT_WINDOW,
            "the budget starts at the capped schedule total"
        );
        assert_eq!(
            exch.budget,
            exch.schedule.iter().copied().sum::<Duration>(),
            "budget and schedule must agree at construction"
        );
    }

    /// The point of the whole change: a device that takes 10 s to wake.
    ///
    /// With `SII=2000` the handshake window is 22.5 s, so the peer is still
    /// being retransmitted to when it finally answers. Advertising nothing it
    /// gets the 5.6 s spec-default window and times out -- which is the correct
    /// outcome for a device that told us it was fast.
    ///
    /// Before this change EVERY peer got a flat 1.5 s regardless of what it
    /// advertised, so both halves timed out.
    #[tokio::test(start_paused = true)]
    async fn a_slow_to_wake_peer_is_reached_when_it_advertises_a_long_sii() {
        async fn attempt(peer_mrp: MrpConfig) -> Result<UnsecuredMessage, DriverError> {
            let (ctrl_io, dev_io) = InMemoryDatagram::pair();
            let dev_addr = dev_io.local_addr();
            let ctrl_addr = ctrl_io.local_addr();
            let mut exch = UnsecuredExchange::new_with_mrp(1, 7, 0xE0E0, peer_mrp);

            let device = async {
                // Asleep. The controller retransmits into the void meanwhile.
                let (pkt, _) = dev_io.recv_from().await.unwrap();
                let msg = decode_unsecured(&pkt).unwrap();
                tokio::time::sleep(Duration::from_secs(10)).await;
                // Retransmits piled up in the device's inbox meanwhile; they go
                // unread, exactly as a sleeping radio would leave them.
                let reply = encode_unsecured_reply(
                    100,
                    msg.exchange_id,
                    0x31,
                    ProtocolId::SECURE_CHANNEL,
                    true,
                    Some(msg.message_counter),
                    Some(0xE0E0),
                    b"sigma2",
                );
                dev_io.send_to(&reply, ctrl_addr).await.unwrap();
            };
            let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x30, 0x31, b"sigma1", None);
            let (got, ()) = tokio::join!(controller, device);
            got
        }

        let sleepy = attempt(MrpConfig::for_peer(
            Some(Duration::from_millis(2000)),
            None,
            None,
        ))
        .await;
        assert_eq!(
            sleepy
                .expect("SII=2000 gives a 22.5 s window, well past the device's 10 s")
                .payload,
            b"sigma2"
        );

        let silent = attempt(MrpConfig::for_peer(None, None, None)).await;
        assert!(
            matches!(silent, Err(DriverError::Timeout { .. })),
            "a peer that advertises nothing keeps the 5.6 s spec-default window"
        );
    }

    /// Exchange ids must actually vary. A constant here is what let a late
    /// reply from a previous handshake match the current one's filter.
    #[test]
    fn random_exchange_id_draws_vary() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            seen.insert(random_exchange_id().unwrap());
        }
        // 1000 draws from 65536 values collide often (birthday), but landing on
        // fewer than 100 distinct values would mean the source is not random.
        assert!(
            seen.len() > 100,
            "expected a wide spread of exchange ids, got {} distinct",
            seen.len()
        );
        assert!(
            seen.len() > 1,
            "a constant exchange id is the defect this replaces"
        );
    }

    /// A handshake must work on an exchange id that is not the old constant `1`
    /// -- nothing in the framing or the receive filter may assume it.
    #[tokio::test]
    async fn send_and_recv_works_on_a_non_one_exchange_id() {
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new(1, 0xBEEF, 0xE0E0);

        let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x30, 0x31, b"sigma1", None);
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let msg = decode_unsecured(&pkt).unwrap();
            assert_eq!(msg.exchange_id, 0xBEEF, "exchange id must survive framing");
            let reply = encode_unsecured_reply(
                100,
                msg.exchange_id,
                0x31,
                ProtocolId::SECURE_CHANNEL,
                true,
                Some(msg.message_counter),
                Some(0xE0E0),
                b"sigma2",
            );
            dev_io.send_to(&reply, ctrl_addr).await.unwrap();
        };
        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, b"sigma2");
    }

    /// A reply carrying NO destination node id must still be accepted.
    ///
    /// Matter 1.0-1.2 required PASE/CASE messages to set S=0 and DSIZ=0, i.e. to
    /// carry neither node id; only 1.3 corrected this
    /// (connectedhomeip#33518). A pre-1.3 device therefore has nothing to echo,
    /// and a guard that demanded a match would make it permanently unreachable.
    #[tokio::test]
    async fn send_and_recv_accepts_a_reply_with_no_destination_node_id() {
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new(1, 7, 0xE0E0);

        let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x30, 0x31, b"sigma1", None);

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let msg = decode_unsecured(&pkt).unwrap();
            let reply = encode_unsecured_reply(
                100,
                msg.exchange_id,
                0x31,
                ProtocolId::SECURE_CHANNEL,
                true,
                Some(msg.message_counter),
                None,
                b"pre-1.3-sigma2",
            );
            dev_io.send_to(&reply, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, b"pre-1.3-sigma2");
    }

    /// A group-addressed frame is malformed on an unsecured session-establishment
    /// exchange and must be dropped rather than parsed as our reply.
    #[tokio::test]
    async fn send_and_recv_rejects_a_group_addressed_reply() {
        const OUR_EPHEMERAL: u64 = 0xE0E0;

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new(1, 7, OUR_EPHEMERAL);

        let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x30, 0x31, b"sigma1", None);

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let msg = decode_unsecured(&pkt).unwrap();
            // Group destination, hand-framed: encode_unsecured_reply only emits
            // unicast, so build the header directly.
            let header = SecuredMessageHeader {
                flags: SecuredMessageFlags::DEST_GROUP,
                session_id: SessionId(0),
                security_flags: SecurityFlags::empty(),
                message_counter: MessageCounter(100),
                source_node_id: None,
                destination_node_id: Some(DestNodeId::Group(0xABCD)),
            };
            let mut group = encode_header(&header);
            encode_protocol_header(
                &ProtocolHeader {
                    exchange_flags: ExchangeFlags::empty(),
                    opcode: 0x31,
                    exchange_id: msg.exchange_id,
                    protocol_id: ProtocolId::SECURE_CHANNEL,
                    ack_counter: None,
                },
                &mut group,
            );
            group.extend_from_slice(b"group-sigma2");
            dev_io.send_to(&group, ctrl_addr).await.unwrap();

            let ours = encode_unsecured_reply(
                101,
                msg.exchange_id,
                0x31,
                ProtocolId::SECURE_CHANNEL,
                true,
                Some(msg.message_counter),
                Some(OUR_EPHEMERAL),
                b"our-sigma2",
            );
            dev_io.send_to(&ours, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, b"our-sigma2");
    }

    #[tokio::test]
    async fn unsecured_send_and_recv_skips_secured_frames() {
        // An MRP retransmit of the last PASE-session (secured, session id ≠ 0)
        // response can straggle into the CASE handshake if its standalone ack
        // was still pending when the unsecured exchange started (observed:
        // Tapo P110M, M6.6.5 validation). Skip it — it is not part of this
        // exchange.
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new(1, 7, 0xE0E0);

        let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x30, 0x31, b"sigma1", None);

        let device = async {
            use matter_transport::{
                encode_header, MessageCounter, SecuredMessageFlags, SecuredMessageHeader,
                SecurityFlags, SessionId,
            };
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&pkt).unwrap();
            // Straggler: a secured frame (session id 1) — header built via the
            // transport primitive, payload irrelevant (it would be encrypted).
            let hdr = SecuredMessageHeader {
                flags: SecuredMessageFlags::empty(),
                session_id: SessionId(1),
                security_flags: SecurityFlags::empty(),
                message_counter: MessageCounter(99),
                source_node_id: None,
                destination_node_id: None,
            };
            let mut stray = encode_header(&hdr);
            stray.extend_from_slice(&[0xAA; 24]); // opaque ciphertext
            dev_io.send_to(&stray, ctrl_addr).await.unwrap();
            // Then the real response.
            let reply = encode_unsecured(
                100,
                m.exchange_id,
                0x31,
                ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                b"sigma2",
            );
            dev_io.send_to(&reply, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        let got = got.unwrap();
        assert_eq!(got.opcode, 0x31, "secured straggler must be skipped");
        assert_eq!(got.payload, b"sigma2");
    }

    #[tokio::test]
    async fn unsecured_send_and_recv_skips_stale_prior_step_response() {
        // After step N's response is consumed and we send step N+1's request,
        // an in-flight MRP retransmit of step N's response (the device resends
        // reliable frames until it sees our next message) still matches the
        // exchange id. send_and_recv must skip it and wait for the real
        // next-step frame, not return it (which would abort the handshake at
        // the require_handshake_opcode gate). Here: awaiting Pake2 (0x23), the
        // device first re-emits the previous PBKDFParamResponse (0x21), then
        // sends the real Pake2.
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new(1, 7, 0xE0E0);

        let controller = exch.send_and_recv(
            &ctrl_io, dev_addr, 0x22, /* Pake1 */
            0x23, /* awaiting Pake2 */
            b"pake1", None,
        );

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&pkt).unwrap();
            // Stale retransmit of the PREVIOUS step's response (0x21).
            let stale = encode_unsecured(
                100,
                m.exchange_id,
                0x21, /* PBKDFParamResponse — stale */
                ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                b"stale-pbkdf-response",
            );
            dev_io.send_to(&stale, ctrl_addr).await.unwrap();
            // Then the real next-step frame, Pake2 (0x23).
            let reply = encode_unsecured(
                101,
                m.exchange_id,
                0x23,
                ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                b"pake2",
            );
            dev_io.send_to(&reply, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        let got = got.unwrap();
        assert_eq!(
            got.opcode, 0x23,
            "stale prior-step response must be skipped"
        );
        assert_eq!(got.payload, b"pake2");
    }

    #[tokio::test]
    async fn unsecured_send_and_recv_unexpected_frame_times_out() {
        // A frame that is neither the awaited next-step opcode, a standalone
        // ack, nor a terminal StatusReport (and that never resolves) must not
        // hang forever — the response deadline must still fire and surface a
        // Timeout, so a misbehaving peer fails cleanly.
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new(1, 7, 0xE0E0);
        // Shorten the deadlines so the test is fast.
        exch.schedule = vec![Duration::from_millis(30); 2];
        exch.budget = Duration::from_millis(60);
        exch.response_timeout = Duration::from_millis(60);

        let controller = exch.send_and_recv(
            &ctrl_io, dev_addr, 0x22, /* Pake1 */
            0x23, /* awaiting Pake2 */
            b"pake1", None,
        );

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&pkt).unwrap();
            // Standalone ack to stop retransmission and switch to the response
            // deadline, then a wrong-opcode frame that is never followed by the
            // real next-step frame.
            let ack = encode_unsecured(
                100,
                m.exchange_id,
                OPCODE_MRP_STANDALONE_ACK,
                ProtocolId::SECURE_CHANNEL,
                false,
                false,
                Some(m.message_counter),
                None,
                b"",
            );
            dev_io.send_to(&ack, ctrl_addr).await.unwrap();
            let stale = encode_unsecured(
                101,
                m.exchange_id,
                0x21, /* wrong opcode, never resolves */
                ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                b"stale",
            );
            dev_io.send_to(&stale, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        assert!(
            matches!(got, Err(DriverError::Timeout { exchange_id: 7 })),
            "unexpected unresolving frame must time out, got: {got:?}"
        );
    }

    #[tokio::test]
    async fn unsecured_send_and_recv_retransmits_dropped_send() {
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new(1, 7, 0xE0E0);

        ctrl_io.set_drops(1); // drop the first send; the retransmit must land

        let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x20, 0x21, b"req", None);

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap(); // sees the retransmit
            let msg = decode_unsecured(&pkt).unwrap();
            let reply = encode_unsecured(
                100,
                msg.exchange_id,
                0x21,
                ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(msg.message_counter),
                None,
                b"resp",
            );
            dev_io.send_to(&reply, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().opcode, 0x21);
    }

    /// Under `TransportProvides` the sent frame must NOT set the R-flag and
    /// must NOT attach an ack — the transport carries reliability, so MRP is
    /// off (spec §4.12). The `Mrp` case (next test) guards the default.
    #[tokio::test]
    async fn transport_provides_sets_no_r_bit() {
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new_ephemeral_with(
            7,
            TransportReliability::TransportProvides,
            MrpConfig::for_peer(None, None, None),
        )
        .unwrap();

        // Pass an ack the MRP path would normally attach, to prove it is stripped.
        let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x20, 0x21, b"req", Some(5));

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            // Parse the exchange flags with the transport's own header parser.
            let (_hdr, rest) = decode_header(&pkt).unwrap();
            let (ph, _) = decode_protocol_header(rest).unwrap();
            assert!(
                !ph.exchange_flags.contains(ExchangeFlags::RELIABLE),
                "R-flag must be clear under TransportProvides"
            );
            assert!(
                ph.ack_counter.is_none(),
                "ack must not be attached under TransportProvides"
            );
            let m = decode_unsecured(&pkt).unwrap();
            let reply = encode_unsecured(
                100,
                m.exchange_id,
                0x21,
                ProtocolId::SECURE_CHANNEL,
                false,
                false,
                None,
                None,
                b"resp",
            );
            dev_io.send_to(&reply, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().opcode, 0x21);
    }

    /// Guard against regressing the default: under `Mrp` the sent frame SHALL
    /// set the R-flag and carry the piggybacked ack.
    #[tokio::test]
    async fn mrp_sets_r_bit_and_ack() {
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new_ephemeral_with(
            7,
            TransportReliability::Mrp,
            MrpConfig::for_peer(None, None, None),
        )
        .unwrap();

        let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x20, 0x21, b"req", Some(5));

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let (_hdr, rest) = decode_header(&pkt).unwrap();
            let (ph, _) = decode_protocol_header(rest).unwrap();
            assert!(
                ph.exchange_flags.contains(ExchangeFlags::RELIABLE),
                "R-flag must be set under Mrp"
            );
            assert_eq!(
                ph.ack_counter.map(|c| c.0),
                Some(5),
                "ack must be attached under Mrp"
            );
            let m = decode_unsecured(&pkt).unwrap();
            let reply = encode_unsecured(
                100,
                m.exchange_id,
                0x21,
                ProtocolId::SECURE_CHANNEL,
                false,
                false,
                None,
                None,
                b"resp",
            );
            dev_io.send_to(&reply, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().opcode, 0x21);
    }

    /// Under `TransportProvides` there is no stop-and-wait retransmit: even
    /// when the responder delays well past the 300 ms MRP retransmit interval,
    /// exactly one packet is sent. Virtual time (`start_paused`) keeps the test
    /// instant.
    #[tokio::test(start_paused = true)]
    async fn transport_provides_sends_exactly_once() {
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut exch = UnsecuredExchange::new_ephemeral_with(
            7,
            TransportReliability::TransportProvides,
            MrpConfig::for_peer(None, None, None),
        )
        .unwrap();

        let controller = exch.send_and_recv(&ctrl_io, dev_addr, 0x20, 0x21, b"req", None);

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&pkt).unwrap();
            // Delay past the MRP 300 ms retransmit interval; an MRP sender would
            // retransmit by now, a TransportProvides sender must not.
            tokio::time::sleep(Duration::from_millis(450)).await;
            let reply = encode_unsecured(
                100,
                m.exchange_id,
                0x21,
                ProtocolId::SECURE_CHANNEL,
                false,
                false,
                None,
                None,
                b"resp",
            );
            dev_io.send_to(&reply, ctrl_addr).await.unwrap();
            // No retransmit must ever arrive on the wire.
            let extra = tokio::time::timeout(Duration::from_millis(500), dev_io.recv_from()).await;
            assert!(
                extra.is_err(),
                "TransportProvides must send exactly one packet (no retransmit)"
            );
        };

        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().opcode, 0x21);
    }

    /// Under `TransportProvides` a non-answering peer must time out at the
    /// 30 s response deadline (`UNSECURED_RESPONSE_TIMEOUT`), not at the shorter
    /// MRP retransmit budget. Virtual time verifies the 30 s constant without a
    /// real 30 s wait.
    #[tokio::test(start_paused = true)]
    async fn transport_provides_times_out_at_30s() {
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = ctrl_io.local_addr(); // peer arg is ignored by InMemoryDatagram
        let _keep_peer_alive = dev_io; // keep the channel open so recv stays pending
        let mut exch = UnsecuredExchange::new_ephemeral_with(
            7,
            TransportReliability::TransportProvides,
            MrpConfig::for_peer(None, None, None),
        )
        .unwrap();

        let start = tokio::time::Instant::now();
        let res = exch
            .send_and_recv(&ctrl_io, dev_addr, 0x20, 0x21, b"req", None)
            .await;
        let elapsed = start.elapsed();

        assert!(
            matches!(res, Err(DriverError::Timeout { exchange_id: 7 })),
            "expected a Timeout, got: {res:?}"
        );
        assert!(
            elapsed >= Duration::from_secs(30),
            "TransportProvides must wait the full 30 s response deadline, waited {elapsed:?}"
        );
    }

    /// Under `TransportProvides` `send_standalone_ack` is a no-op: nothing is
    /// put on the wire and the message counter does not advance.
    #[tokio::test(start_paused = true)]
    async fn standalone_ack_noop_under_transport_provides() {
        let (a, b) = InMemoryDatagram::pair();
        let b_addr = b.local_addr();
        let mut exch = UnsecuredExchange::new_ephemeral_with(
            9,
            TransportReliability::TransportProvides,
            MrpConfig::for_peer(None, None, None),
        )
        .unwrap();
        let counter_before = exch.counter;

        exch.send_standalone_ack(&a, b_addr, 7).await.unwrap();

        // Nothing may arrive at the peer.
        let got = tokio::time::timeout(Duration::from_millis(50), b.recv_from()).await;
        assert!(
            got.is_err(),
            "send_standalone_ack must be a no-op under TransportProvides"
        );
        assert_eq!(
            exch.counter, counter_before,
            "the no-op must not advance the message counter"
        );
    }
}
