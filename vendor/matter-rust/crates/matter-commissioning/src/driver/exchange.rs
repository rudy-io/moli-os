//! Secured-exchange round-trip over `SessionManager` + MRP.
//!
//! This is the single place where async IO and MRP retransmit timers meet for
//! *encrypted* exchanges. The sans-IO `SessionManager` produces wire bytes and
//! consumes inbound packets; this helper sends, then drives a
//! recv-or-timer-fire loop until the matching application response arrives,
//! silently absorbing acks, duplicate-reliable resends, and retransmits so the
//! policy layer above never sees MRP mechanics.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use matter_transport::{
    DecodeInboundOutput, MrpEvent, MrpFlags, ProtocolId, SessionId, SessionManager,
};

use crate::driver::datagram::AsyncDatagram;
use crate::driver::error::DriverError;

/// The application response decoded from a completed secured round-trip.
///
/// `Debug` is written by hand so that `payload` prints as its length only
/// (`"<N bytes>"`): it is the device's decrypted response, which can carry a
/// secret, so it must never reach a log through a stray `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub struct SecuredResponse {
    /// The exchange id the request/response pair ran on.
    pub exchange_id: u16,
    /// The decoded application payload (post-protocol-header bytes).
    /// Plaintext: this struct's `Debug` output shows only its length.
    pub payload: Vec<u8>,
}

impl core::fmt::Debug for SecuredResponse {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Same name, fields and order as the derive this replaces; only the
        // payload's bytes are withheld.
        f.debug_struct("SecuredResponse")
            .field("exchange_id", &self.exchange_id)
            .field("payload", &format!("<{} bytes>", self.payload.len()))
            .finish()
    }
}

/// Sentinel sleep when no MRP deadline is pending: the recv arm of the
/// `select!` should win, so we park on a long timer rather than busy-loop.
const IDLE_SLEEP: Duration = Duration::from_secs(3600);

/// Send `app_payload` as a *reliable* secured message on `session_id` to
/// `peer`, then drive MRP until the matching application response arrives.
///
/// `AckOnly` and `DuplicateReliableAckResent` outcomes are absorbed; retransmit
/// timers are honoured. `peer` is used only as the send destination — inbound
/// datagrams are matched by session id and exchange id (the source address is
/// not filtered), consistent with the single-peer commissioning flow. A frame
/// for an unknown session, one that fails authentication, and an
/// authenticated duplicate that earns no ACK (no R) are skipped, as chip
/// drops a duplicate that needs no ACK. Any other decode failure (a
/// malformed header, for example) is propagated as an error and aborts the
/// round-trip; that is acceptable under the single-peer assumption but is a
/// place to harden if this is ever used where unrelated traffic can reach the
/// socket.
///
/// The response ends the exchange, so before returning this sends the
/// standalone ACK MRP buffered for it ([`SessionManager::finish_exchange`];
/// chip's `ExchangeContext::DoClose` flushes the same way). Nothing else
/// could send it: the next exchange uses a fresh id, so the ACK cannot
/// piggyback, and once this returns nothing pumps the MRP timer until the
/// next call — after `commission()`'s last exchange, never. The send is best
/// effort and never fails a round-trip whose response arrived. If the ACK is
/// lost, the device retransmits the response: while a later exchange runs on
/// this session, its duplicate path ACKs the retransmit; after the last
/// exchange nothing does, and the device retransmits until it gives up (chip
/// ignores a failed `FlushAcks` the same way).
///
/// # Errors
///
/// - [`DriverError::Transport`] if framing/session/MRP rejects a frame.
/// - [`DriverError::Io`] if the datagram send/recv fails.
/// - [`DriverError::Timeout`] if MRP exhausts its retransmit budget.
/// - [`DriverError::SessionClosed`] if the device closes `session_id` with a
///   `CloseSession` before answering (the session is removed). A
///   `CloseSession` for another session removes that session and the wait
///   continues.
pub async fn secured_round_trip<T: AsyncDatagram>(
    transport: &T,
    sessions: &mut SessionManager,
    session_id: SessionId,
    peer: SocketAddr,
    opcode: u8,
    protocol_id: ProtocolId,
    app_payload: &[u8],
) -> Result<SecuredResponse, DriverError> {
    // 1. Encode + send the request (reliable → MRP tracks it for retransmit).
    let out = sessions.encode_outbound(
        session_id,
        None, // new exchange; MRP allocates the id and marks us initiator
        opcode,
        protocol_id,
        app_payload,
        MrpFlags { reliable: true },
        Instant::now(),
    )?;
    let our_exchange = out.exchange_id;
    transport.send_to(&out.wire_bytes, peer).await?;
    // M6 wire-trace capture: feeds JsonlLayer / cargo xtask trace-diff.
    #[cfg(feature = "tracing")]
    tracing::debug!(
        target: "matter_wire",
        dir = "tx",
        session_id = u64::from(session_id.0),
        exchange_id = u64::from(our_exchange),
        protocol = u64::from(protocol_id.protocol),
        opcode = u64::from(opcode),
        payload = crate::hexdump::payload(app_payload),
        "wire"
    );

    // 2. recv-or-timer loop.
    loop {
        let now = Instant::now();
        let sleep_for = sessions.poll_timeout().map_or(IDLE_SLEEP, |deadline| {
            deadline.saturating_duration_since(now)
        });

        tokio::select! {
            biased;
            recv = transport.recv_from() => {
                // Single-peer flow: the source address is not trust-checked here;
                // `decode_inbound` authenticates by session id + AES-CCM tag.
                let (packet, _from) = recv?;
                // Unsecured stragglers (session id 0 — e.g. a PASE/CASE
                // StatusReport retransmit whose standalone ack was lost) are
                // not addressed to any secured session; skip, don't abort.
                if packet.len() >= 3 && packet[1] == 0 && packet[2] == 0 {
                    continue;
                }
                let decoded = match sessions.decode_inbound(&packet, Instant::now()) {
                    Ok(d) => d,
                    // A secured packet we cannot attribute to one of THIS
                    // manager's sessions, or cannot decrypt, is not our
                    // response — skip it, don't abort the exchange. Seen right
                    // after `commission()` hands off to a fresh operational
                    // `SessionManager`: the device keeps MRP-retransmitting its
                    // final commissioning-session frame (addressed to the old,
                    // now-unknown session id) into the new session's recv loop.
                    //
                    // An authenticated duplicate that earns no ACK (no R, or a
                    // `transport_reliable` session) is skipped too, as chip
                    // drops it (`SessionManager.cpp:996` at master 5cd2917a,
                    // `:1001` at v1.4.2.0) and as the controller actor does. A
                    // duplicate with R set comes back as
                    // `DuplicateReliableAckResent` below and is ACKed once.
                    Err(
                        matter_transport::Error::UnknownSession(_)
                        | matter_transport::Error::DecryptionFailed
                        | matter_transport::Error::ReplayedCounter { .. }
                        | matter_transport::Error::CounterTooOld { .. },
                    ) => continue,
                    Err(e) => return Err(e.into()),
                };
                if let Some(closed) = take_close_session(sessions, &decoded) {
                    if closed == session_id {
                        return Err(DriverError::SessionClosed { session_id: closed.0 });
                    }
                    // Another of our sessions: it is gone, keep waiting here.
                    continue;
                }
                match decoded {
                    // Our response arrives on our exchange: our session, our
                    // id, and `I` clear because we are its initiator
                    // (Matter 1.4 Core §4.10.5.1; chip
                    // `ExchangeContext::MatchExchange`). A message the device
                    // starts carries `I=1`, and one on another session belongs
                    // to that session: either is a different exchange even
                    // when its id equals ours.
                    DecodeInboundOutput::AppMessage {
                        session_id: msg_session,
                        exchange_id,
                        is_initiator: false,
                        payload,
                        protocol_id: msg_protocol_id,
                        opcode: msg_opcode,
                        ..
                    } if msg_session == session_id && exchange_id == our_exchange => {
                        // M6 wire-trace capture: feeds JsonlLayer / cargo xtask trace-diff.
                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            target: "matter_wire",
                            dir = "rx",
                            session_id = u64::from(session_id.0),
                            exchange_id = u64::from(exchange_id),
                            protocol = u64::from(msg_protocol_id.protocol),
                            opcode = u64::from(msg_opcode),
                            payload = crate::hexdump::payload(&payload),
                            "wire"
                        );
                        #[cfg(not(feature = "tracing"))]
                        let _ = (&msg_protocol_id, &msg_opcode);
                        flush_exchange_ack(transport, sessions, session_id, exchange_id, peer)
                            .await;
                        return Ok(SecuredResponse { exchange_id, payload });
                    }
                    // Peer re-sent a reliable frame; bounce its standalone ack.
                    DecodeInboundOutput::DuplicateReliableAckResent { ack_packet, .. } => {
                        transport.send_to(&ack_packet, peer).await?;
                    }
                    // Wait through everything else: an app message for a
                    // *different* exchange (not expected in the strictly
                    // sequential commissioning flow), an `AckOnly` for our request
                    // (response still pending), and — `DecodeInboundOutput` being
                    // `#[non_exhaustive]` — any future outcome.
                    _ => {}
                }
            }
            () = tokio::time::sleep(sleep_for) => {
                drive_timer_tick(transport, sessions, session_id, our_exchange, peer).await?;
            }
        }
    }
}

/// Maximum number of `ReportData` chunks a single read may span before
/// [`secured_read`] aborts. A conformant wildcard read of a typical device is a
/// handful of chunks; 64 is far above that and bounds a buggy/hostile peer.
pub const MAX_READ_CHUNKS: usize = 64;

/// Maximum total decoded payload bytes a single read may accumulate before
/// [`secured_read`] aborts (256 `KiB`).
pub const MAX_READ_BYTES: usize = 256 * 1024;

/// Maximum decoded payload bytes a *single* `ReportData` chunk may carry
/// before [`secured_read`] aborts (64 `KiB`).
///
/// A Matter chunk rides inside one secured UDP datagram, so a single chunk is
/// far smaller than this in practice. Bounding each chunk *before* it is parsed
/// stops a buggy or hostile peer from forcing a parse of one oversized blob
/// (the cumulative [`MAX_READ_BYTES`] cap alone would let a single chunk up to
/// that size through to the parser first).
pub const MAX_READ_CHUNK_BYTES: usize = 64 * 1024;

/// IM `StatusResponse` opcode (Matter §8.7) — the per-chunk ack the reader
/// sends to solicit the next chunk.
const OP_STATUS_RESPONSE: u8 = 0x01;

/// Enforce the three chunked-read safety caps for one inbound chunk, returning
/// the updated cumulative byte total on success.
///
/// Checked in order, all *before* the chunk is parsed:
///
/// 1. **Per-chunk** ([`MAX_READ_CHUNK_BYTES`]): reject a single oversized blob
///    up front, so it is never handed to the parser.
/// 2. **Chunk count** ([`MAX_READ_CHUNKS`]): bound the number of chunks.
/// 3. **Cumulative bytes** ([`MAX_READ_BYTES`]): bound the running total.
///
/// `existing_chunks` is the number of chunks already accepted, `chunk_len` the
/// length of the new chunk's decoded payload, and `total_bytes` the running
/// cumulative total before this chunk.
///
/// # Errors
///
/// - [`DriverError::ReadTooLarge`] if any cap is exceeded, naming the cap hit.
fn enforce_read_caps(
    existing_chunks: usize,
    chunk_len: usize,
    total_bytes: usize,
) -> Result<usize, DriverError> {
    if chunk_len > MAX_READ_CHUNK_BYTES {
        return Err(DriverError::ReadTooLarge {
            limit: "MAX_READ_CHUNK_BYTES",
        });
    }
    if existing_chunks + 1 > MAX_READ_CHUNKS {
        return Err(DriverError::ReadTooLarge {
            limit: "MAX_READ_CHUNKS",
        });
    }
    let total = total_bytes.saturating_add(chunk_len);
    if total > MAX_READ_BYTES {
        return Err(DriverError::ReadTooLarge {
            limit: "MAX_READ_BYTES",
        });
    }
    Ok(total)
}

/// Check one inbound `ReportData` chunk: the three read caps
/// ([`enforce_read_caps`]) first, so an oversized chunk is never parsed, then
/// the parse. Returns the updated cumulative byte total and the chunk's
/// `MoreChunkedMessages` flag.
///
/// # Errors
///
/// - [`DriverError::ReadTooLarge`] if a cap is exceeded.
/// - [`DriverError::Im`] if the chunk is not a parseable `ReportData`.
fn check_chunk(
    existing_chunks: usize,
    payload: &[u8],
    total_bytes: usize,
) -> Result<(usize, bool), DriverError> {
    let total = enforce_read_caps(existing_chunks, payload.len(), total_bytes)?;
    let more = crate::im::parse_report_data(payload)?.more_chunked_messages;
    Ok((total, more))
}

/// Send a `ReadRequest` (`app_payload`) as a reliable secured message, then
/// drive the Matter chunked-read transaction: for every inbound `ReportData`
/// chunk that sets `MoreChunkedMessages`, reply with `StatusResponse(SUCCESS)`
/// on the same exchange — which piggybacks the chunk's MRP ack and solicits the
/// next chunk — stopping at the final chunk. Returns every chunk payload in
/// order.
///
/// A non-chunked read returns a single-element `Vec` (the loop runs once). The
/// final chunk ends the exchange: its standalone ACK is sent before this
/// returns, as [`secured_round_trip`] does. Inbound demultiplexing (exchange
/// match, unsecured-straggler skip, `UnknownSession`/`DecryptionFailed` skip,
/// unacknowledged-duplicate skip, duplicate-ack bounce, MRP timer) is
/// identical to [`secured_round_trip`].
///
/// # Errors
///
/// - [`DriverError::ReadTooLarge`] if the read exceeds [`MAX_READ_CHUNKS`],
///   [`MAX_READ_BYTES`], or any single chunk exceeds [`MAX_READ_CHUNK_BYTES`].
/// - [`DriverError::Im`] if a chunk is not a parseable `ReportData`.
/// - [`DriverError::Transport`] / [`DriverError::Io`] / [`DriverError::Timeout`]
///   as for [`secured_round_trip`].
/// - [`DriverError::SessionClosed`] if the device closes `session_id` with a
///   `CloseSession` before answering (the session is removed). A
///   `CloseSession` for another session removes that session and the wait
///   continues.
pub async fn secured_read<T: AsyncDatagram>(
    transport: &T,
    sessions: &mut SessionManager,
    session_id: SessionId,
    peer: SocketAddr,
    opcode: u8,
    protocol_id: ProtocolId,
    app_payload: &[u8],
) -> Result<Vec<Vec<u8>>, DriverError> {
    // 1. Encode + send the ReadRequest (reliable → MRP tracks it).
    let out = sessions.encode_outbound(
        session_id,
        None,
        opcode,
        protocol_id,
        app_payload,
        MrpFlags { reliable: true },
        Instant::now(),
    )?;
    let our_exchange = out.exchange_id;
    transport.send_to(&out.wire_bytes, peer).await?;

    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut total_bytes = 0usize;

    // 2. recv-or-timer loop, acking each non-final chunk to solicit the next.
    loop {
        let now = Instant::now();
        let sleep_for = sessions.poll_timeout().map_or(IDLE_SLEEP, |deadline| {
            deadline.saturating_duration_since(now)
        });

        tokio::select! {
            biased;
            recv = transport.recv_from() => {
                let (packet, _from) = recv?;
                // Unsecured stragglers (session id 0) are not ours — skip.
                if packet.len() >= 3 && packet[1] == 0 && packet[2] == 0 {
                    continue;
                }
                let decoded = match sessions.decode_inbound(&packet, Instant::now()) {
                    Ok(d) => d,
                    // As in `secured_round_trip`: an unknown session, a failed
                    // tag, or an authenticated duplicate that earns no ACK.
                    Err(
                        matter_transport::Error::UnknownSession(_)
                        | matter_transport::Error::DecryptionFailed
                        | matter_transport::Error::ReplayedCounter { .. }
                        | matter_transport::Error::CounterTooOld { .. },
                    ) => continue,
                    Err(e) => return Err(e.into()),
                };
                if let Some(closed) = take_close_session(sessions, &decoded) {
                    if closed == session_id {
                        return Err(DriverError::SessionClosed { session_id: closed.0 });
                    }
                    // Another of our sessions: it is gone, keep waiting here.
                    continue;
                }
                match decoded {
                    // Our exchange only: our session and id with `I` clear,
                    // as in `secured_round_trip`.
                    DecodeInboundOutput::AppMessage {
                        session_id: msg_session,
                        exchange_id,
                        is_initiator: false,
                        payload,
                        ..
                    } if msg_session == session_id && exchange_id == our_exchange => {
                        // Enforce all three read caps BEFORE parsing the chunk —
                        // in particular the per-chunk cap bounds a lone oversized
                        // blob up front rather than parsing it first.
                        let (total, more) = match check_chunk(chunks.len(), &payload, total_bytes) {
                            Ok(checked) => checked,
                            Err(e) => {
                                // The read is over: chip closes an exchange on
                                // error, and the close flushes its ACK.
                                flush_exchange_ack(
                                    transport,
                                    sessions,
                                    session_id,
                                    our_exchange,
                                    peer,
                                )
                                .await;
                                return Err(e);
                            }
                        };
                        total_bytes = total;
                        chunks.push(payload);
                        if !more {
                            flush_exchange_ack(transport, sessions, session_id, our_exchange, peer)
                                .await;
                            return Ok(chunks);
                        }
                        // Ack this chunk + solicit the next on the same exchange.
                        let status = crate::im::build_status_response(0);
                        let ack = sessions.encode_outbound_with_role(
                            session_id,
                            our_exchange,
                            true,
                            OP_STATUS_RESPONSE,
                            ProtocolId::INTERACTION_MODEL,
                            &status,
                            MrpFlags { reliable: true },
                            Instant::now(),
                        )?;
                        transport.send_to(&ack.wire_bytes, peer).await?;
                    }
                    // Peer re-sent a reliable frame; bounce its standalone ack.
                    DecodeInboundOutput::DuplicateReliableAckResent { ack_packet, .. } => {
                        transport.send_to(&ack_packet, peer).await?;
                    }
                    // Keep waiting through everything else: an app message on
                    // another exchange, an `AckOnly` for our request, and —
                    // `DecodeInboundOutput` being `#[non_exhaustive]` — any
                    // future outcome.
                    _ => {}
                }
            }
            () = tokio::time::sleep(sleep_for) => {
                drive_timer_tick(transport, sessions, session_id, our_exchange, peer).await?;
            }
        }
    }
}

/// Run one MRP timer tick for a helper waiting on `our_exchange`: send every
/// retransmit and standalone ACK the tick produced, then, if a retransmit
/// budget ran out, end the exchange ([`flush_exchange_ack`]) and fail with
/// [`DriverError::Timeout`].
///
/// The whole batch is sent before returning on `Expired`:
/// `MrpState::handle_timeout` emits `Expired` ahead of the ACKs that fell due
/// on the same tick, and it has already drained those ACKs, so stopping at
/// `Expired` would lose them for good.
///
/// # Errors
///
/// - [`DriverError::Io`] if a send fails.
/// - [`DriverError::Timeout`] naming the first exchange whose retransmit
///   budget ran out.
async fn drive_timer_tick<T: AsyncDatagram>(
    transport: &T,
    sessions: &mut SessionManager,
    session_id: SessionId,
    our_exchange: u16,
    peer: SocketAddr,
) -> Result<(), DriverError> {
    let mut expired = None;
    for event in sessions.handle_timeout(Instant::now()) {
        match event {
            MrpEvent::Retransmit { packet, .. } | MrpEvent::SendStandaloneAck { packet, .. } => {
                transport.send_to(&packet, peer).await?;
            }
            MrpEvent::Expired { exchange_id, .. } => {
                expired.get_or_insert(exchange_id);
            }
            // `MrpEvent` is `#[non_exhaustive]`; ignore future timer events.
            _ => {}
        }
    }
    if let Some(exchange_id) = expired {
        // The exchange is over; send anything it still owes.
        flush_exchange_ack(transport, sessions, session_id, our_exchange, peer).await;
        return Err(DriverError::Timeout { exchange_id });
    }
    Ok(())
}

/// Send the standalone ACK still owed on `exchange_id`, an exchange we
/// started, now that the exchange is over
/// ([`SessionManager::finish_exchange_with_role`]; a device exchange that
/// reuses the id keeps its own ACK). Best effort: a failed send
/// or an exhausted counter is not an error for the caller (its response
/// already arrived, or it is already failing); chip ignores `FlushAcks`
/// failures the same way (`ExchangeContext::DoClose`). An exhausted counter
/// is logged under the `tracing` feature. Sends nothing on a
/// reliable-transport (BTP) session, which never buffers an MRP ACK.
async fn flush_exchange_ack<T: AsyncDatagram>(
    transport: &T,
    sessions: &mut SessionManager,
    session_id: SessionId,
    exchange_id: u16,
    peer: SocketAddr,
) {
    for event in sessions.finish_exchange_with_role(session_id, exchange_id, true) {
        match event {
            MrpEvent::SendStandaloneAck { packet, .. } => {
                let _ = transport.send_to(&packet, peer).await;
            }
            #[cfg(feature = "tracing")]
            MrpEvent::SessionCounterExhausted { exchange_id, .. } => {
                tracing::debug!(
                    exchange_id = u64::from(exchange_id),
                    "standalone ACK not sent: the session's message counter is exhausted"
                );
            }
            // `MrpEvent` is `#[non_exhaustive]`.
            _ => {}
        }
    }
}

/// Honour a `CloseSession` from the device (Matter 1.4 Core §4.11.1.4) if
/// `decoded` is one. That means an application message `decode_inbound`
/// authenticated on one of `sessions`' PASE or CASE sessions, that starts an
/// exchange the device opened, and that
/// `matter_transport::secure_channel::is_close_session` recognises. If so,
/// remove that session and return its id. Either kind counts: §4.11.1.4
/// sends a `CloseSession` "within an exchange associated with a PASE or CASE
/// session", and chip evicts any secure session that receives one.
///
/// A node that receives a `CloseSession` SHALL remove the session's state.
/// Its owed ACKs and our retransmits on it go too: the device has dropped
/// the session and could not decrypt them. A `CloseSession` on an exchange we
/// started (I clear from the device) is not one, as in chip, which honours
/// only a status report that starts an exchange
/// (`UnsolicitedStatusHandler.cpp:28-56`).
fn take_close_session(
    sessions: &mut SessionManager,
    decoded: &DecodeInboundOutput,
) -> Option<SessionId> {
    let DecodeInboundOutput::AppMessage {
        session_id,
        is_initiator: true,
        protocol_id,
        opcode,
        payload,
        ..
    } = decoded
    else {
        return None;
    };
    if !matter_transport::secure_channel::is_close_session(*protocol_id, *opcode, payload) {
        return None;
    }
    sessions.remove(*session_id);
    Some(*session_id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use std::time::Instant;

    use matter_crypto::pase::PaseSessionKeys;
    use matter_transport::{
        DecodeInboundOutput, MrpFlags, PeerHint, ProtocolId, SessionManager, SessionRole,
    };

    use super::*;
    use crate::driver::datagram::{AsyncDatagram, InMemoryDatagram};

    /// Two `SessionManager`s sharing one symmetric key set, cross-registered as
    /// Initiator/Responder. Both allocate local id 1 (allocator starts at 1),
    /// so each side's `peer_session_id` is 1.
    fn paired_pase_sessions() -> (SessionManager, SessionManager) {
        let keys = PaseSessionKeys {
            ke: [0u8; 16],
            i2r_key: [1u8; 16],
            r2i_key: [2u8; 16],
            attestation_key: [3u8; 16],
        };
        let mut ctrl = SessionManager::new();
        let mut dev = SessionManager::new();
        let _ctrl_sid =
            ctrl.register_pase(keys.clone(), SessionRole::Initiator, 1, PeerHint::default());
        let _dev_sid = dev.register_pase(keys, SessionRole::Responder, 1, PeerHint::default());
        (ctrl, dev)
    }

    /// An authenticated duplicate that arrives while the driver waits is
    /// acknowledged, not taken as the response, and the round trip still
    /// completes.
    /// The duplicate here is a reliable message with counter 0, which every
    /// secure session treats as a duplicate (chip ACKs it, `ExchangeMgr.cpp:
    /// 415-446`). Before the transport fix it surfaced as `ReplayedCounter`
    /// and aborted the exchange.
    #[tokio::test]
    async fn secured_round_trip_acks_an_authenticated_duplicate() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
        );

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected an application message");
            };
            // A reliable duplicate (counter 0) on the device's own exchange.
            let saved = dev.get(session).unwrap().outbound_counter;
            dev.get_mut(session).unwrap().outbound_counter = 0;
            let dup = dev
                .encode_outbound(
                    session,
                    None,
                    0x05,
                    ProtocolId::INTERACTION_MODEL,
                    b"dup",
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev.get_mut(session).unwrap().outbound_counter = saved;
            dev_io.send_to(&dup.wire_bytes, ctrl_addr).await.unwrap();
            // The driver ACKs it, naming counter 0. Bounded, so a driver
            // that sends nothing fails here instead of hanging the join.
            let (ack, _) =
                tokio::time::timeout(std::time::Duration::from_secs(5), dev_io.recv_from())
                    .await
                    .expect("the driver must ACK the duplicate")
                    .unwrap();
            match dev.decode_inbound(&ack, Instant::now()).unwrap() {
                DecodeInboundOutput::AckOnly {
                    exchange_id: acked_on,
                    acked_counter,
                    ..
                } => {
                    assert_eq!(acked_on, dup.exchange_id);
                    assert_eq!(acked_counter, matter_transport::MessageCounter(0));
                }
                other => panic!("expected the duplicate's ACK, got {other:?}"),
            }
            let out = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x09,
                    ProtocolId::INTERACTION_MODEL,
                    b"resp",
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, b"resp");
    }

    /// Send `dev` a message without R on a fresh device-started exchange,
    /// twice, as a network that duplicates a datagram delivers it. The second
    /// copy is an authenticated duplicate that earns no ACK.
    async fn send_duplicated_unreliable(
        dev: &mut SessionManager,
        dev_io: &InMemoryDatagram,
        to: std::net::SocketAddr,
    ) {
        let session = matter_transport::SessionId(1);
        let msg = dev
            .encode_outbound(
                session,
                None,
                0x05,
                ProtocolId::INTERACTION_MODEL,
                b"unreliable",
                MrpFlags::default(),
                Instant::now(),
            )
            .unwrap();
        dev_io.send_to(&msg.wire_bytes, to).await.unwrap();
        dev_io.send_to(&msg.wire_bytes, to).await.unwrap();
    }

    /// A network-duplicated message without R, arriving mid-step, is dropped
    /// and the round trip completes, as chip drops a duplicate that needs no
    /// ACK (`SessionManager.cpp:996`). Before the fix its `ReplayedCounter`
    /// ended the commissioning step.
    #[tokio::test]
    async fn secured_round_trip_drops_an_unreliable_duplicate() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected an application message");
            };
            send_duplicated_unreliable(&mut dev, &dev_io, ctrl_addr).await;
            let out = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x09,
                    ProtocolId::INTERACTION_MODEL,
                    b"resp",
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
        };
        let (got, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(controller, device)
        })
        .await
        .expect("the step must finish");
        assert_eq!(
            got.expect("the duplicate must not end the step").payload,
            b"resp"
        );
    }

    /// [`secured_read`] drops a network-duplicated message without R too.
    #[tokio::test]
    async fn secured_read_drops_an_unreliable_duplicate() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let controller = secured_read(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"read",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected the ReadRequest");
            };
            send_duplicated_unreliable(&mut dev, &dev_io, ctrl_addr).await;
            let out = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x05,
                    ProtocolId::INTERACTION_MODEL,
                    &report_data(1, 0x06, 0x0000, 1, false),
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
        };
        let (got, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(controller, device)
        })
        .await
        .expect("the read must finish");
        assert_eq!(got.expect("the duplicate must not end the read").len(), 1);
    }

    #[tokio::test]
    async fn secured_round_trip_retransmits_dropped_request() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        // Drop the controller's FIRST outbound datagram (the original request).
        // MRP's ~300 ms active retransmit then re-sends it; the resend lands.
        ctrl_io.set_drops(1);

        let request = b"req".as_slice();
        let response = b"resp".as_slice();

        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            request,
        );

        let device = async {
            // The device's single recv sees the retransmit (original was dropped).
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage {
                exchange_id,
                payload,
                ..
            } = dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected an application message");
            };
            assert_eq!(payload, request);
            let out = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x09,
                    ProtocolId::INTERACTION_MODEL,
                    response,
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, response);
    }

    #[tokio::test]
    async fn secured_round_trip_skips_unsecured_frames() {
        // A PASE/CASE StatusReport retransmit (session id 0) can straggle into
        // the secured IM phase if the device missed our standalone ack
        // (observed: Tapo P110M, M6.6.5 validation). It is not addressed to
        // any secured session — skip it rather than abort the round-trip.
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let request = b"req".as_slice();
        let response = b"resp".as_slice();

        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            request,
        );

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected an application message");
            };
            // Straggler: an unsecured (session-id 0) StatusReport retransmit.
            let stray = crate::driver::unsecured::encode_unsecured(
                7,
                1,
                0x40,
                ProtocolId::SECURE_CHANNEL,
                false,
                true,
                None,
                None,
                &[0u8; 8],
            );
            dev_io.send_to(&stray, ctrl_addr).await.unwrap();
            // Then the real secured response.
            let out = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x09,
                    ProtocolId::INTERACTION_MODEL,
                    response,
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, response);
    }

    #[tokio::test]
    async fn secured_round_trip_skips_unknown_session_frames() {
        // After commission() hands off to a fresh operational SessionManager,
        // the device can keep MRP-retransmitting its final commissioning-session
        // frame, addressed to a (now-unknown) session id the new manager never
        // registered (observed: Tapo P110M, M7.5 validation). Such a secured
        // frame must be skipped, not abort the round-trip.
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        // A "stale" peer whose frames are addressed to session id 99 — an id the
        // controller's manager does not have.
        let stale_keys = PaseSessionKeys {
            ke: [9u8; 16],
            i2r_key: [9u8; 16],
            r2i_key: [9u8; 16],
            attestation_key: [9u8; 16],
        };
        let mut stale = SessionManager::new();
        let stale_sid =
            stale.register_pase(stale_keys, SessionRole::Initiator, 99, PeerHint::default());

        let request = b"req".as_slice();
        let response = b"resp".as_slice();

        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            request,
        );

        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected an application message");
            };
            // Straggler: a secured frame addressed to unknown session id 99.
            let stray = stale
                .encode_outbound(
                    stale_sid,
                    None,
                    0x09,
                    ProtocolId::INTERACTION_MODEL,
                    b"stale",
                    MrpFlags { reliable: false },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&stray.wire_bytes, ctrl_addr).await.unwrap();
            // Then the real secured response on the live session.
            let out = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x09,
                    ProtocolId::INTERACTION_MODEL,
                    response,
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, response);
    }

    #[tokio::test]
    async fn secured_round_trip_returns_response_payload() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let request = b"invoke-request-tlv".as_slice();
        let response = b"invoke-response-tlv".as_slice();

        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08, // InvokeRequest opcode (opaque to the transport here)
            ProtocolId::INTERACTION_MODEL,
            request,
        );

        let device = async {
            loop {
                let (pkt, _) = dev_io.recv_from().await.unwrap();
                // Wait for the request; ignore any ack-only / other frames.
                if let DecodeInboundOutput::AppMessage {
                    exchange_id,
                    payload,
                    ..
                } = dev.decode_inbound(&pkt, Instant::now()).unwrap()
                {
                    assert_eq!(payload, request);
                    let out = dev
                        .encode_outbound(
                            session,
                            Some(exchange_id),
                            0x09, // InvokeResponse opcode
                            ProtocolId::INTERACTION_MODEL,
                            response,
                            MrpFlags { reliable: true },
                            Instant::now(),
                        )
                        .unwrap();
                    dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
                    break;
                }
            }
        };

        let (got, ()) = tokio::join!(controller, device);
        let got = got.unwrap();
        assert_eq!(got.payload, response);
    }

    /// The next standalone ACK the device receives within `within`, or `None`.
    async fn next_ack(
        dev_io: &InMemoryDatagram,
        dev: &mut SessionManager,
        within: std::time::Duration,
    ) -> Option<matter_transport::MessageCounter> {
        let Ok(Ok((pkt, _))) = tokio::time::timeout(within, dev_io.recv_from()).await else {
            return None;
        };
        match dev.decode_inbound(&pkt, Instant::now()).unwrap() {
            DecodeInboundOutput::AckOnly { acked_counter, .. } => Some(acked_counter),
            other => panic!("expected a standalone ACK, got {other:?}"),
        }
    }

    /// The response ends the exchange, so its standalone ACK leaves before
    /// `secured_round_trip` returns (chip `ExchangeContext::DoClose` →
    /// `FlushAcks`). Before, nothing sent it: once the call returns, nothing
    /// pumps this manager's MRP timer.
    #[tokio::test]
    async fn secured_round_trip_acks_the_response_before_returning() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected the request");
            };
            let out = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x09,
                    ProtocolId::INTERACTION_MODEL,
                    b"resp",
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
            out.message_counter
        };
        let (got, response_counter) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, b"resp");
        assert_eq!(
            next_ack(&dev_io, &mut dev, std::time::Duration::from_secs(1)).await,
            Some(response_counter)
        );
        assert_eq!(ctrl.poll_timeout(), None, "no ACK left buffered");
    }

    /// A controller session whose request expires on the first timer tick
    /// (one transmission, zero backoff, no jitter) and whose standalone-ACK
    /// deadline is immediate, already owing an ACK for a device-initiated
    /// reliable message on another exchange (0x30). The helper's first tick
    /// therefore yields `Expired` for its request and that ACK in ONE batch,
    /// with `Expired` first (`MrpState::handle_timeout` emits retransmit
    /// events before ACK events). Returns the owed message's counter.
    fn expiring_ctrl_owing_an_ack() -> (
        SessionManager,
        SessionManager,
        matter_transport::MessageCounter,
    ) {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let mut cfg = matter_transport::MrpConfig::default();
        cfg.max_transmissions = 1;
        cfg.initial_active = Duration::ZERO;
        cfg.initial_idle = Duration::ZERO;
        cfg.backoff_jitter = 0.0;
        cfg.standalone_ack_deadline = Duration::ZERO;
        ctrl.get_mut(session).unwrap().mrp = matter_transport::MrpState::new(cfg);
        let owed = dev
            .encode_outbound(
                session,
                Some(0x30),
                0x05,
                ProtocolId::INTERACTION_MODEL,
                b"x",
                MrpFlags { reliable: true },
                Instant::now(),
            )
            .unwrap();
        ctrl.decode_inbound(&owed.wire_bytes, Instant::now())
            .unwrap();
        (ctrl, dev, owed.message_counter)
    }

    /// The device receives the helper's request, then waits up to 1 s for the
    /// next standalone ACK and returns its counter.
    async fn request_then_ack(
        dev_io: &InMemoryDatagram,
        dev: &mut SessionManager,
    ) -> Option<matter_transport::MessageCounter> {
        let (req, _) = dev_io.recv_from().await.unwrap();
        assert!(
            matches!(
                dev.decode_inbound(&req, Instant::now()).unwrap(),
                DecodeInboundOutput::AppMessage { .. }
            ),
            "the first datagram must be the request"
        );
        next_ack(dev_io, dev, std::time::Duration::from_secs(1)).await
    }

    /// An `Expired` in a timer batch must not cut the batch short: a
    /// standalone ACK that fell due on the same tick (here for another
    /// exchange) was already drained from MRP, so if the helper returned at
    /// `Expired` nothing would ever send it.
    #[tokio::test]
    async fn secured_round_trip_sends_an_ack_due_in_its_expiry_batch() {
        let (mut ctrl, mut dev, owed) = expiring_ctrl_owing_an_ack();
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let got = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            matter_transport::SessionId(1),
            dev_io.local_addr(),
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
        )
        .await;
        assert!(
            matches!(got, Err(DriverError::Timeout { .. })),
            "got {got:?}"
        );
        assert_eq!(request_then_ack(&dev_io, &mut dev).await, Some(owed));
        assert_eq!(ctrl.poll_timeout(), None);
    }

    #[tokio::test]
    async fn secured_read_sends_an_ack_due_in_its_expiry_batch() {
        let (mut ctrl, mut dev, owed) = expiring_ctrl_owing_an_ack();
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let got = secured_read(
            &ctrl_io,
            &mut ctrl,
            matter_transport::SessionId(1),
            dev_io.local_addr(),
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"read",
        )
        .await;
        assert!(
            matches!(got, Err(DriverError::Timeout { .. })),
            "got {got:?}"
        );
        assert_eq!(request_then_ack(&dev_io, &mut dev).await, Some(owed));
        assert_eq!(ctrl.poll_timeout(), None);
    }

    #[tokio::test]
    async fn secured_read_acks_the_final_chunk_before_returning() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let controller = secured_read(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"read",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected the ReadRequest");
            };
            let out = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x05,
                    ProtocolId::INTERACTION_MODEL,
                    &report_data(1, 0x06, 0x0000, 1, false),
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
            out.message_counter
        };
        let (got, chunk_counter) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().len(), 1);
        assert_eq!(
            next_ack(&dev_io, &mut dev, std::time::Duration::from_secs(1)).await,
            Some(chunk_counter)
        );
        assert_eq!(ctrl.poll_timeout(), None);
    }

    /// An error return ends the exchange too: a chunk that is not a
    /// `ReportData` fails the read with `DriverError::Im`, and its ACK still
    /// leaves before the error does (chip closes an exchange on error, and
    /// the close flushes).
    #[tokio::test]
    async fn secured_read_acks_a_malformed_chunk_before_failing() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let controller = secured_read(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"read",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected the ReadRequest");
            };
            let out = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x05,
                    ProtocolId::INTERACTION_MODEL,
                    &[0x04, 0x2A], // an anonymous uint, not a structure: not a ReportData
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
            out.message_counter
        };
        let (got, chunk_counter) = tokio::join!(controller, device);
        assert!(matches!(got, Err(DriverError::Im(_))), "got {got:?}");
        assert_eq!(
            next_ack(&dev_io, &mut dev, std::time::Duration::from_secs(1)).await,
            Some(chunk_counter)
        );
        assert_eq!(ctrl.poll_timeout(), None);
    }

    /// Build a `ReportData` carrying one attribute `(ep,cl,at)=val` with the
    /// given `MoreChunkedMessages` flag (CR.1 wire shape).
    fn report_data(ep: u16, cl: u32, at: u32, val: u64, more: bool) -> Vec<u8> {
        use matter_codec::{Tag, TlvWriter};
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.start_array(Tag::Context(1)).unwrap(); // AttributeReports
        w.start_structure(Tag::Anonymous).unwrap(); // AttributeReportIB
        w.start_structure(Tag::Context(1)).unwrap(); // AttributeData
        w.start_list(Tag::Context(1)).unwrap(); // Path
        w.put_uint(Tag::Context(2), u64::from(ep)).unwrap();
        w.put_uint(Tag::Context(3), u64::from(cl)).unwrap();
        w.put_uint(Tag::Context(4), u64::from(at)).unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(2), val).unwrap(); // Data
        w.end_container().unwrap(); // AttributeData
        w.end_container().unwrap(); // AttributeReportIB
        w.end_container().unwrap(); // array
        if more {
            w.put_bool(Tag::Context(3), true).unwrap(); // MoreChunkedMessages
        }
        w.put_uint(Tag::Context(0xFF), 11).unwrap();
        w.end_container().unwrap();
        buf
    }

    #[test]
    fn enforce_read_caps_rejects_oversized_single_chunk() {
        // A lone chunk larger than the per-chunk cap is rejected up front
        // (before any parse), even though it alone is under the cumulative cap.
        let err = enforce_read_caps(0, MAX_READ_CHUNK_BYTES + 1, 0).unwrap_err();
        match err {
            DriverError::ReadTooLarge { limit } => assert_eq!(limit, "MAX_READ_CHUNK_BYTES"),
            other => panic!("expected ReadTooLarge(MAX_READ_CHUNK_BYTES), got {other:?}"),
        }
    }

    #[test]
    fn enforce_read_caps_accepts_normal_chunk() {
        // A normally sized chunk passes and returns the updated cumulative total.
        let total = enforce_read_caps(0, 1024, 0).unwrap();
        assert_eq!(total, 1024);
        // A second normal chunk accumulates.
        let total = enforce_read_caps(1, 512, total).unwrap();
        assert_eq!(total, 1536);
    }

    #[test]
    fn enforce_read_caps_rejects_too_many_chunks() {
        let err = enforce_read_caps(MAX_READ_CHUNKS, 1, 0).unwrap_err();
        match err {
            DriverError::ReadTooLarge { limit } => assert_eq!(limit, "MAX_READ_CHUNKS"),
            other => panic!("expected MAX_READ_CHUNKS, got {other:?}"),
        }
    }

    #[test]
    fn enforce_read_caps_rejects_cumulative_overflow() {
        // Each chunk is within the per-chunk cap, but together they exceed the
        // cumulative cap.
        let err = enforce_read_caps(1, 1, MAX_READ_BYTES).unwrap_err();
        match err {
            DriverError::ReadTooLarge { limit } => assert_eq!(limit, "MAX_READ_BYTES"),
            other => panic!("expected MAX_READ_BYTES, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn secured_read_reassembles_two_chunks() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let controller = secured_read(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x02, // ReadRequest
            ProtocolId::INTERACTION_MODEL,
            b"readreq",
        );

        let device = async {
            // 1. Receive the ReadRequest.
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected ReadRequest");
            };
            // 2. Send chunk 0 (MoreChunkedMessages=true): ep0/0x28/0x0002 = 5010.
            let c0 = report_data(0, 0x28, 0x0002, 5010, true);
            let out = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x05, // ReportData
                    ProtocolId::INTERACTION_MODEL,
                    &c0,
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
            // 3. Receive the controller's StatusResponse ack (opcode 0x01). It
            //    must ride the SAME exchange as the read — that is what
            //    piggybacks chunk 0's MRP ack and solicits the next chunk.
            let (ack, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage {
                opcode,
                exchange_id: ack_exchange,
                ..
            } = dev.decode_inbound(&ack, Instant::now()).unwrap()
            else {
                panic!("expected StatusResponse");
            };
            assert_eq!(
                opcode, 0x01,
                "controller must ack the chunk with StatusResponse"
            );
            assert_eq!(
                ack_exchange, exchange_id,
                "StatusResponse must ride the read exchange (enables the chunk-ack piggyback)"
            );
            // 4. Send the final chunk (no MoreChunkedMessages): ep1/0x06/0x0000 = 1.
            let c1 = report_data(1, 0x06, 0x0000, 1, false);
            let out = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x05,
                    ProtocolId::INTERACTION_MODEL,
                    &c1,
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
        };

        let (got, ()) = tokio::join!(controller, device);
        let chunks = got.unwrap();
        assert_eq!(chunks.len(), 2, "both chunks returned");
        // Reassemble through the CR.1 accumulator.
        let mut acc = crate::im::ReportAccumulator::new();
        for c in &chunks {
            acc.push(crate::im::parse_report_data(c).unwrap()).unwrap();
        }
        let attrs = acc.finish();
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].0.endpoint, 0);
        assert_eq!(attrs[1].0.endpoint, 1);
    }

    /// Two session pairs with distinct keys: controller ids 1 and 2, device
    /// ids 1 and 2, cross-registered (stand-ins for PASE and CASE).
    fn two_paired_sessions() -> (SessionManager, SessionManager) {
        let k = |a: u8| PaseSessionKeys {
            ke: [a; 16],
            i2r_key: [a.wrapping_add(1); 16],
            r2i_key: [a.wrapping_add(2); 16],
            attestation_key: [a.wrapping_add(3); 16],
        };
        let mut ctrl = SessionManager::new();
        let mut dev = SessionManager::new();
        ctrl.register_pase(k(0x10), SessionRole::Initiator, 1, PeerHint::default());
        ctrl.register_pase(k(0x20), SessionRole::Initiator, 2, PeerHint::default());
        dev.register_pase(k(0x10), SessionRole::Responder, 1, PeerHint::default());
        dev.register_pase(k(0x20), SessionRole::Responder, 2, PeerHint::default());
        (ctrl, dev)
    }

    /// A message with our exchange id and `I` clear, but on ANOTHER of our
    /// sessions, is not our response: an exchange belongs to one session
    /// (Matter 1.4 Core §4.10.5.1 criterion 1; chip `MatchExchange` checks
    /// `mSession.Contains(session)`). The response is the one on our session.
    #[tokio::test]
    async fn secured_round_trip_ignores_our_exchange_id_on_another_session() {
        let (mut ctrl, mut dev) = two_paired_sessions();
        let ours = matter_transport::SessionId(1);
        let other = matter_transport::SessionId(2);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let (dev_addr, ctrl_addr) = (dev_io.local_addr(), ctrl_io.local_addr());
        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            ours,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected the request");
            };
            for (session, payload) in [(other, b"wrong-session".as_slice()), (ours, b"resp")] {
                let out = dev
                    .encode_outbound_with_role(
                        session,
                        exchange_id,
                        false,
                        0x09,
                        ProtocolId::INTERACTION_MODEL,
                        payload,
                        MrpFlags { reliable: false },
                        Instant::now(),
                    )
                    .unwrap();
                dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
            }
        };
        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, b"resp");
    }

    /// [`secured_read`] applies the same session check to its chunks.
    #[tokio::test]
    async fn secured_read_ignores_our_exchange_id_on_another_session() {
        let (mut ctrl, mut dev) = two_paired_sessions();
        let ours = matter_transport::SessionId(1);
        let other = matter_transport::SessionId(2);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let (dev_addr, ctrl_addr) = (dev_io.local_addr(), ctrl_io.local_addr());
        let wrong = report_data(1, 6, 0, 0, false);
        let right = report_data(1, 6, 0, 1, false);
        let controller = secured_read(
            &ctrl_io,
            &mut ctrl,
            ours,
            dev_addr,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"read",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected the read");
            };
            for (session, payload) in [(other, &wrong), (ours, &right)] {
                let out = dev
                    .encode_outbound_with_role(
                        session,
                        exchange_id,
                        false,
                        0x05,
                        ProtocolId::INTERACTION_MODEL,
                        payload,
                        MrpFlags { reliable: false },
                        Instant::now(),
                    )
                    .unwrap();
                dev_io.send_to(&out.wire_bytes, ctrl_addr).await.unwrap();
            }
        };
        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap(), vec![right.clone()]);
    }

    /// The device closes the session instead of answering: the helper
    /// removes it and fails at once with `SessionClosed`. Before, it waited
    /// for a response that could never come.
    #[tokio::test]
    async fn secured_round_trip_fails_at_once_when_the_device_closes_its_session() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let (dev_addr, ctrl_addr) = (dev_io.local_addr(), ctrl_io.local_addr());
        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
        );
        let device = async {
            let _ = dev_io.recv_from().await.unwrap(); // the request
            let close = matter_transport::secure_channel::encode_close_session(
                &mut dev,
                session,
                Instant::now(),
            )
            .unwrap();
            dev_io.send_to(&close.wire_bytes, ctrl_addr).await.unwrap();
        };
        let (got, ()) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(controller, device)
        })
        .await
        .expect("fails at once, without waiting on MRP");
        assert!(
            matches!(got, Err(DriverError::SessionClosed { session_id: 1 })),
            "{got:?}"
        );
        assert!(ctrl.get(session).is_none(), "the closed session is removed");
    }

    /// The device closes ANOTHER session while we wait on this one: that
    /// session is removed and the round trip still completes.
    #[tokio::test]
    async fn secured_round_trip_forgets_another_closed_session_and_keeps_waiting() {
        let (mut ctrl, mut dev) = two_paired_sessions();
        let (waiting, other) = (
            matter_transport::SessionId(1),
            matter_transport::SessionId(2),
        );
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let (dev_addr, ctrl_addr) = (dev_io.local_addr(), ctrl_io.local_addr());
        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            waiting,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected the request");
            };
            let close = matter_transport::secure_channel::encode_close_session(
                &mut dev,
                other,
                Instant::now(),
            )
            .unwrap();
            dev_io.send_to(&close.wire_bytes, ctrl_addr).await.unwrap();
            let resp = dev
                .encode_outbound(
                    waiting,
                    Some(exchange_id),
                    0x09,
                    ProtocolId::INTERACTION_MODEL,
                    b"resp",
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&resp.wire_bytes, ctrl_addr).await.unwrap();
        };
        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, b"resp");
        assert!(ctrl.get(other).is_none(), "the other session is removed");
        assert!(ctrl.get(waiting).is_some(), "the waiting session is kept");
    }

    /// Guard: a Secure Channel `StatusReport` that is not a `CloseSession`, on a
    /// new exchange, closes nothing.
    #[tokio::test]
    async fn secured_round_trip_ignores_a_status_report_that_is_not_close_session() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let (dev_addr, ctrl_addr) = (dev_io.local_addr(), ctrl_io.local_addr());
        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected the request");
            };
            let not_close = dev
                .encode_outbound(
                    session,
                    None,
                    0x40,
                    ProtocolId::SECURE_CHANNEL,
                    &[0, 0, 0, 0, 0, 0, 0, 0],
                    MrpFlags { reliable: false },
                    Instant::now(),
                )
                .unwrap();
            dev_io
                .send_to(&not_close.wire_bytes, ctrl_addr)
                .await
                .unwrap();
            let resp = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x09,
                    ProtocolId::INTERACTION_MODEL,
                    b"resp",
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&resp.wire_bytes, ctrl_addr).await.unwrap();
        };
        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, b"resp");
        assert!(ctrl.get(session).is_some());
    }

    /// Guard (spoofing): an UNSECURED frame carrying the `CloseSession` payload
    /// closes nothing; only an encrypted one counts.
    #[tokio::test]
    async fn secured_round_trip_ignores_an_unsecured_close_session() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let (dev_addr, ctrl_addr) = (dev_io.local_addr(), ctrl_io.local_addr());
        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected the request");
            };
            let spoof = crate::driver::encode_unsecured(
                1,
                0x1234,
                0x40,
                ProtocolId::SECURE_CHANNEL,
                true,
                false,
                None,
                None,
                &matter_transport::secure_channel::CLOSE_SESSION_PAYLOAD,
            );
            dev_io.send_to(&spoof, ctrl_addr).await.unwrap();
            let resp = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x09,
                    ProtocolId::INTERACTION_MODEL,
                    b"resp",
                    MrpFlags { reliable: true },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&resp.wire_bytes, ctrl_addr).await.unwrap();
        };
        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, b"resp");
        assert!(ctrl.get(session).is_some());
    }

    /// `secured_read` honours it the same way.
    #[tokio::test]
    async fn secured_read_fails_at_once_when_the_device_closes_its_session() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let (dev_addr, ctrl_addr) = (dev_io.local_addr(), ctrl_io.local_addr());
        let controller = secured_read(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"read",
        );
        let device = async {
            let _ = dev_io.recv_from().await.unwrap();
            let close = matter_transport::secure_channel::encode_close_session(
                &mut dev,
                session,
                Instant::now(),
            )
            .unwrap();
            dev_io.send_to(&close.wire_bytes, ctrl_addr).await.unwrap();
        };
        let (got, ()) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(controller, device)
        })
        .await
        .expect("fails at once");
        assert!(
            matches!(got, Err(DriverError::SessionClosed { session_id: 1 })),
            "{got:?}"
        );
        assert!(ctrl.get(session).is_none());
    }

    /// A device that starts its own exchange with the id of ours (legal:
    /// each side allocates its own ids, Matter 1.4 Core §4.10.2) sends with
    /// `I=1`, so its message is not our response; the response is the `I=0`
    /// one that follows.
    #[tokio::test]
    async fn secured_round_trip_ignores_a_device_exchange_that_reuses_our_id() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let (dev_addr, ctrl_addr) = (dev_io.local_addr(), ctrl_io.local_addr());
        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected the request");
            };
            // The device's own exchange, same id: I=1.
            let unsolicited = dev
                .encode_outbound_with_role(
                    session,
                    exchange_id,
                    true,
                    0x05,
                    ProtocolId::INTERACTION_MODEL,
                    b"device-initiated",
                    MrpFlags { reliable: false },
                    Instant::now(),
                )
                .unwrap();
            dev_io
                .send_to(&unsolicited.wire_bytes, ctrl_addr)
                .await
                .unwrap();
            let resp = dev
                .encode_outbound_with_role(
                    session,
                    exchange_id,
                    false,
                    0x09,
                    ProtocolId::INTERACTION_MODEL,
                    b"resp",
                    MrpFlags { reliable: false },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&resp.wire_bytes, ctrl_addr).await.unwrap();
        };
        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(got.unwrap().payload, b"resp");
    }

    /// Guard: a `CloseSession`-shaped report on the exchange WE started (I
    /// clear from the device) is a response, not a close (§4.11.1.4 requires a
    /// new exchange; chip routes only initiator messages to its
    /// `UnsolicitedStatusHandler`, `ExchangeMgr.cpp:319-322`).
    #[tokio::test]
    async fn secured_round_trip_treats_a_close_session_on_our_exchange_as_a_response() {
        let (mut ctrl, mut dev) = paired_pase_sessions();
        let session = matter_transport::SessionId(1);
        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let (dev_addr, ctrl_addr) = (dev_io.local_addr(), ctrl_io.local_addr());
        let controller = secured_round_trip(
            &ctrl_io,
            &mut ctrl,
            session,
            dev_addr,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
        );
        let device = async {
            let (pkt, _) = dev_io.recv_from().await.unwrap();
            let DecodeInboundOutput::AppMessage { exchange_id, .. } =
                dev.decode_inbound(&pkt, Instant::now()).unwrap()
            else {
                panic!("expected the request");
            };
            let resp = dev
                .encode_outbound(
                    session,
                    Some(exchange_id),
                    0x40,
                    ProtocolId::SECURE_CHANNEL,
                    &matter_transport::secure_channel::CLOSE_SESSION_PAYLOAD,
                    MrpFlags { reliable: false },
                    Instant::now(),
                )
                .unwrap();
            dev_io.send_to(&resp.wire_bytes, ctrl_addr).await.unwrap();
        };
        let (got, ()) = tokio::join!(controller, device);
        assert_eq!(
            got.unwrap().payload,
            matter_transport::secure_channel::CLOSE_SESSION_PAYLOAD
        );
        assert!(ctrl.get(session).is_some());
    }
}
