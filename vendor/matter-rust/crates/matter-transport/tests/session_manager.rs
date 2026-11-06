//! `SessionManager` integration tests.
//!
//! Six tests are inherited from M5.1 (registration + counter bump +
//! roundtrip + unknown-session error), updated for the M5.2 signatures
//! that thread the protocol header and MRP state through the manager.
//! Six new tests cover M5.2 behaviour: reliable+piggyback roundtrip,
//! cross-session `poll_timeout`, duplicate-reliable resend, expired-event
//! draining, concurrent-exchange isolation, and the payload-too-large
//! guard with the protocol header included.

#![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use std::time::{Duration, Instant};

use matter_crypto::pase::PaseSessionKeys;
use matter_transport::mrp::MrpFlags;
use matter_transport::protocol_header::ProtocolId;
use matter_transport::session::{
    DecodeInboundOutput, PeerHint, SessionKeys, SessionManager, SessionRole,
};
use matter_transport::{
    decode_protocol_header, decode_secured, encode_secured, ExchangeFlags, MessageCounter,
    MrpEvent, ReplayWindow, SecuredMessageFlags, SecuredMessageHeader, SecurityFlags, SessionId,
};

fn pase_keys() -> PaseSessionKeys {
    PaseSessionKeys {
        ke: [0xAAu8; 16],
        i2r_key: [0x11u8; 16],
        r2i_key: [0x22u8; 16],
        attestation_key: [0x33u8; 16],
    }
}

#[test]
fn register_pase_allocates_session_id() {
    let mut mgr = SessionManager::new();
    let sid = mgr.register_pase(
        pase_keys(),
        SessionRole::Initiator,
        /* peer_session_id */ 0xBEEF,
        PeerHint::default(),
    );
    let s = mgr.get(sid).unwrap();
    assert_eq!(s.peer_id.0, 0xBEEF);
    assert_eq!(s.role, SessionRole::Initiator);
    assert_eq!(s.keys.i2r_key, [0x11u8; 16]);
    assert_eq!(s.keys.r2i_key, [0x22u8; 16]);
}

#[test]
fn allocated_session_ids_are_unique() {
    let mut mgr = SessionManager::new();
    let s1 = mgr.register_pase(pase_keys(), SessionRole::Initiator, 1, PeerHint::default());
    let s2 = mgr.register_pase(pase_keys(), SessionRole::Initiator, 2, PeerHint::default());
    assert_ne!(s1, s2);
}

#[test]
fn remove_drops_the_session() {
    let mut mgr = SessionManager::new();
    let sid = mgr.register_pase(pase_keys(), SessionRole::Initiator, 1, PeerHint::default());
    assert!(mgr.get(sid).is_some());
    assert!(mgr.remove(sid).is_some());
    assert!(mgr.get(sid).is_none());
    assert!(mgr.remove(sid).is_none());
}

#[test]
fn encode_outbound_bumps_counter() {
    let mut mgr = SessionManager::new();
    let sid = mgr.register_pase(
        pase_keys(),
        SessionRole::Initiator,
        0xBEEF,
        PeerHint::default(),
    );
    let initial_counter = mgr.get(sid).unwrap().outbound_counter;

    let _ = mgr
        .encode_outbound(
            sid,
            None,
            0x02,
            matter_transport::protocol_header::ProtocolId::INTERACTION_MODEL,
            b"hello",
            MrpFlags::default(),
            std::time::Instant::now(),
        )
        .unwrap();

    let after = mgr.get(sid).unwrap().outbound_counter;
    assert_eq!(after, initial_counter.wrapping_add(1));
}

#[test]
fn encode_outbound_then_decode_inbound_roundtrip() {
    // Two managers, each side of the same session.
    let mut alice = SessionManager::new();
    let mut bob = SessionManager::new();
    let keys = pase_keys();
    let now = std::time::Instant::now();

    let alice_sid = alice.register_pase(
        keys.clone(),
        SessionRole::Initiator,
        /* peer_session_id (bob's local id, faked here) */ 1,
        PeerHint::default(),
    );
    let bob_sid = bob.register_pase(
        keys,
        SessionRole::Responder,
        /* peer_session_id (alice's local id) */ alice_sid.0,
        PeerHint::default(),
    );

    // encode_outbound sets header.session_id to peer_id from alice's
    // session record. Alice's peer_id == 1 == bob's local id, so bob
    // can demux on receipt.
    let out = alice
        .encode_outbound(
            alice_sid,
            None,
            0x02,
            matter_transport::protocol_header::ProtocolId::INTERACTION_MODEL,
            b"ping",
            MrpFlags::default(),
            now,
        )
        .unwrap();

    let decoded = bob.decode_inbound(&out.wire_bytes, now).unwrap();
    match decoded {
        matter_transport::session::DecodeInboundOutput::AppMessage {
            session_id,
            payload,
            ..
        } => {
            assert_eq!(session_id, bob_sid);
            assert_eq!(payload, b"ping");
        }
        other => panic!("expected AppMessage, got {other:?}"),
    }
}

/// A secure session rejects a peer message counter of 0, and still accepts
/// counter 1. Secure-session counters start at 1 or higher (Matter 1.4
/// §4.6.1.1), the spec sets the peer's synchronized `max_message_counter`
/// to 0 when a session is established (§4.14.1.2 for PASE, §4.14.2.3 for
/// CASE), and chip starts each secure session's reception state synced
/// at 0 (`PairingSession::ActivateSecureSession` →
/// `SetCounter(kInitialSyncValue)`), so its `VerifyEncryptedUnicast` treats
/// counter 0 as a duplicate. Before the fix the first counter, 0 included,
/// was trusted.
#[test]
fn decode_inbound_rejects_peer_counter_zero_on_a_secure_session() {
    let mut alice = SessionManager::new();
    let mut bob = SessionManager::new();
    let keys = pase_keys();
    let now = std::time::Instant::now();
    let alice_sid =
        alice.register_pase(keys.clone(), SessionRole::Initiator, 1, PeerHint::default());
    let _bob_sid = bob.register_pase(
        keys,
        SessionRole::Responder,
        alice_sid.0,
        PeerHint::default(),
    );

    let mut send_with_counter = |counter: u32| {
        alice.get_mut(alice_sid).unwrap().outbound_counter = counter;
        alice
            .encode_outbound(
                alice_sid,
                None,
                0x02,
                ProtocolId::INTERACTION_MODEL,
                b"ping",
                MrpFlags::default(),
                now,
            )
            .unwrap()
            .wire_bytes
    };

    let zero = send_with_counter(0);
    let err = bob.decode_inbound(&zero, now).unwrap_err();
    assert!(
        matches!(err, matter_transport::Error::ReplayedCounter { counter: 0 }),
        "counter 0 must be rejected as a duplicate; got {err:?}",
    );

    let one = send_with_counter(1);
    assert!(matches!(
        bob.decode_inbound(&one, now).unwrap(),
        DecodeInboundOutput::AppMessage { .. }
    ));
}

#[test]
fn decode_inbound_unknown_session_errors() {
    let mut mgr = SessionManager::new();
    // Craft a wire packet with a session ID we never registered.
    let keys = SessionKeys {
        i2r_key: [0x11u8; 16],
        r2i_key: [0x22u8; 16],
        attestation_key: [0x33u8; 16],
    };
    let header = SecuredMessageHeader {
        flags: SecuredMessageFlags::empty(),
        session_id: SessionId(0xDEAD),
        security_flags: SecurityFlags::empty(),
        message_counter: MessageCounter(1),
        source_node_id: None,
        destination_node_id: None,
    };
    let wire = encode_secured(&header, b"x", &keys, SessionRole::Initiator, 0).unwrap();

    let err = mgr
        .decode_inbound(&wire, std::time::Instant::now())
        .unwrap_err();
    assert!(matches!(
        err,
        matter_transport::Error::UnknownSession(0xDEAD)
    ));
}

#[test]
fn encode_outbound_reliable_then_decode_inbound_ack() {
    // Alice (Initiator) sends reliable; Bob (Responder) receives. Bob's
    // process_inbound queues a piggyback. Bob then sends a response in
    // the same exchange; the response carries A=1 + ack_counter back to
    // Alice. Alice's pending_acks clears.
    use matter_transport::protocol_header::ProtocolId;
    use matter_transport::session::DecodeInboundOutput;

    let mut alice = SessionManager::new();
    let mut bob = SessionManager::new();
    let keys = pase_keys();
    let now = std::time::Instant::now();

    let alice_sid =
        alice.register_pase(keys.clone(), SessionRole::Initiator, 1, PeerHint::default());
    let bob_sid = bob.register_pase(
        keys,
        SessionRole::Responder,
        alice_sid.0,
        PeerHint::default(),
    );

    let out1 = alice
        .encode_outbound(
            alice_sid,
            None,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"req",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();
    let exchange_id = out1.exchange_id;
    // Alice has a pending retransmit.
    assert!(alice.get(alice_sid).unwrap().mrp.poll_timeout().is_some());

    let received_at_bob = bob.decode_inbound(&out1.wire_bytes, now).unwrap();
    let (_, _) = match received_at_bob {
        DecodeInboundOutput::AppMessage {
            exchange_id: ex,
            payload,
            ..
        } => (ex, payload),
        other => panic!("expected AppMessage, got {other:?}"),
    };

    // Bob sends a response in the same exchange — piggyback drains.
    let out2 = bob
        .encode_outbound(
            bob_sid,
            Some(exchange_id),
            0x03,
            ProtocolId::INTERACTION_MODEL,
            b"resp",
            MrpFlags::default(),
            now,
        )
        .unwrap();
    assert!(out2.piggyback_acked, "Bob piggybacked Alice's ack");

    // Alice decodes Bob's response — the embedded ack clears her pending.
    let received_at_alice = alice.decode_inbound(&out2.wire_bytes, now).unwrap();
    match received_at_alice {
        DecodeInboundOutput::AppMessage { payload, .. } => {
            assert_eq!(payload, b"resp");
        }
        other => panic!("expected AppMessage at Alice, got {other:?}"),
    }
    assert!(
        alice.get(alice_sid).unwrap().mrp.poll_timeout().is_none(),
        "Alice's pending retransmit cleared by piggyback ack",
    );
}

#[test]
fn poll_timeout_min_across_sessions() {
    use matter_transport::protocol_header::ProtocolId;
    use std::time::Duration;

    let mut mgr = SessionManager::new();
    let s1 = mgr.register_pase(pase_keys(), SessionRole::Initiator, 1, PeerHint::default());
    let s2 = mgr.register_pase(pase_keys(), SessionRole::Initiator, 2, PeerHint::default());

    let now = std::time::Instant::now();
    let _ = mgr
        .encode_outbound(
            s1,
            None,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"x",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();
    let later = now + Duration::from_millis(100);
    let _ = mgr
        .encode_outbound(
            s2,
            None,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"y",
            MrpFlags { reliable: true },
            later,
        )
        .unwrap();

    let deadline = mgr.poll_timeout().unwrap();
    // Both sessions are fresh with no recorded peer activity, so they classify
    // as idle and use the idle base plus the spec's 1.1 margin. s1 was
    // sent first, so its deadline is the earliest across sessions.
    //
    // A RANGE, not an equality: retransmits carry MRP_BACKOFF_JITTER, and a
    // `SessionManager` builds its `MrpState` internally so there is no seed to
    // inject. The bound is the jitter bound itself, which makes this a stricter
    // check than the equality it replaces — it would catch a jitter draw that
    // shortened a delay or exceeded its spec limit.
    let base = Duration::from_millis(550);
    let max = base.mul_f32(1.0 + 0.25);
    assert!(
        deadline >= now + base && deadline < now + max,
        "earliest is s1's idle-base deadline, jittered within [base, base * 1.25): \
         got {:?} past `now`, expected within [{base:?}, {max:?})",
        deadline.saturating_duration_since(now),
    );
}

#[test]
fn decode_inbound_duplicate_reliable_emits_resend_packet() {
    use matter_transport::protocol_header::ProtocolId;
    use matter_transport::session::DecodeInboundOutput;

    let mut alice = SessionManager::new();
    let mut bob = SessionManager::new();
    let keys = pase_keys();
    let now = std::time::Instant::now();

    let alice_sid =
        alice.register_pase(keys.clone(), SessionRole::Initiator, 1, PeerHint::default());
    let bob_sid = bob.register_pase(
        keys,
        SessionRole::Responder,
        alice_sid.0,
        PeerHint::default(),
    );

    let out = alice
        .encode_outbound(
            alice_sid,
            None,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"req",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();
    bob.decode_inbound(&out.wire_bytes, now).unwrap();

    // Alice retransmits the same wire bytes (peer's ack was lost).
    let outcome = bob.decode_inbound(&out.wire_bytes, now).unwrap();
    match outcome {
        DecodeInboundOutput::DuplicateReliableAckResent {
            session_id,
            exchange_id,
            ack_packet,
        } => {
            assert_eq!(session_id, bob_sid);
            assert_eq!(exchange_id, out.exchange_id);
            assert_ne!(ack_packet, b"");
        }
        other => panic!("expected DuplicateReliableAckResent, got {other:?}"),
    }
}

#[test]
fn handle_timeout_drains_expired_sessions() {
    use matter_transport::protocol_header::ProtocolId;
    use matter_transport::MrpEvent;
    use std::time::Duration;

    let mut mgr = SessionManager::new();
    let sid = mgr.register_pase(pase_keys(), SessionRole::Initiator, 1, PeerHint::default());
    let now = std::time::Instant::now();

    let _ = mgr
        .encode_outbound(
            sid,
            None,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"x",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();

    // Advance well past every retransmit. The session is idle (no peer activity
    // recorded), so retransmits use the idle base (500 ms + margin) with
    // exponential backoff past MRP_BACKOFF_THRESHOLD — the budget below
    // comfortably exceeds the resulting schedule (chip's idle spacing is
    // deliberately long for sleepy devices).
    let mut t = now;
    let mut saw_expired = false;
    for _ in 0..30 {
        t += Duration::from_secs(10);
        let events = mgr.handle_timeout(t);
        if events.iter().any(|e| matches!(e, MrpEvent::Expired { .. })) {
            saw_expired = true;
            break;
        }
    }
    assert!(
        saw_expired,
        "expected at least one Expired event after exhausting retransmits"
    );
}

#[test]
fn concurrent_exchanges_isolated() {
    use matter_transport::protocol_header::ProtocolId;

    let mut mgr = SessionManager::new();
    let sid = mgr.register_pase(pase_keys(), SessionRole::Initiator, 1, PeerHint::default());
    let now = std::time::Instant::now();

    let out1 = mgr
        .encode_outbound(
            sid,
            None,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"a",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();
    let out2 = mgr
        .encode_outbound(
            sid,
            None,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"b",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();

    assert_ne!(out1.exchange_id, out2.exchange_id);
    assert_eq!(out1.message_counter.0 + 1, out2.message_counter.0);
}

#[test]
fn allocate_then_register_pase_under_chosen_local_id() {
    let mut mgr = SessionManager::new();
    let local = mgr.allocate_session_id();
    assert_ne!(local.0, 0, "allocated id must be non-zero");
    let local2 = mgr.allocate_session_id();
    assert_ne!(local, local2);

    let keys = matter_crypto::pase::PaseSessionKeys {
        ke: [0u8; 16],
        i2r_key: [1u8; 16],
        r2i_key: [2u8; 16],
        attestation_key: [3u8; 16],
    };
    mgr.register_pase_with_local_id(
        local,
        keys,
        SessionRole::Initiator,
        0x00BB,
        PeerHint::default(),
    );

    let s = mgr.get(local).unwrap();
    assert_eq!(s.local_id, local);
    assert_eq!(s.peer_id, SessionId(0x00BB));
}

#[test]
fn payload_too_large_with_protocol_header() {
    use matter_transport::protocol_header::ProtocolId;

    let mut mgr = SessionManager::new();
    let sid = mgr.register_pase(pase_keys(), SessionRole::Initiator, 1, PeerHint::default());
    let now = std::time::Instant::now();

    // MAX_PAYLOAD_LEN = 1024 includes the protocol header (~6 bytes).
    // 1025 bytes of app payload pushes the wire payload (header + app)
    // past the cap and the framing layer rejects it.
    let oversized = vec![0u8; 1025];
    let err = mgr
        .encode_outbound(
            sid,
            None,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            &oversized,
            MrpFlags::default(),
            now,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        matter_transport::Error::PayloadTooLarge { .. }
    ));
}

/// Alice (initiator) and Bob (responder) sharing one PASE key set, each
/// registered with the other's session id.
fn paired() -> (SessionManager, SessionId, SessionManager, SessionId) {
    let mut alice = SessionManager::new();
    let mut bob = SessionManager::new();
    let alice_sid =
        alice.register_pase(pase_keys(), SessionRole::Initiator, 1, PeerHint::default());
    let bob_sid = bob.register_pase(
        pase_keys(),
        SessionRole::Responder,
        alice_sid.0,
        PeerHint::default(),
    );
    (alice, alice_sid, bob, bob_sid)
}

/// The exchange flags inside `packet`, decrypted with `receiver`'s keys for
/// session `sid` (a fresh replay window, so `receiver`'s own is untouched).
fn exchange_flags_of(receiver: &SessionManager, sid: SessionId, packet: &[u8]) -> ExchangeFlags {
    let session = receiver.get(sid).unwrap();
    let (_, plaintext) = decode_secured(
        packet,
        &session.keys,
        session.role,
        &mut ReplayWindow::new(),
        session.peer_nonce_node_id,
    )
    .unwrap();
    decode_protocol_header(&plaintext).unwrap().0.exchange_flags
}

/// Alice sends a reliable request, Bob answers reliably, Alice finishes the
/// exchange. The ACK she sends at once is one Bob accepts for his response,
/// and it clears his retransmit, so nothing is left on either side's timer.
/// Alice started the exchange, so her ACK carries I=1 (spec §4.4.3.1), the bit
/// chip matches an inbound to its exchange on (`ExchangeContext::MatchExchange`).
#[test]
fn finish_exchange_sends_an_ack_the_peer_accepts() {
    let (mut alice, alice_sid, mut bob, bob_sid) = paired();
    let now = Instant::now();
    let req = alice
        .encode_outbound(
            alice_sid,
            None,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();
    bob.decode_inbound(&req.wire_bytes, now).unwrap();
    let resp = bob
        .encode_outbound(
            bob_sid,
            Some(req.exchange_id),
            0x09,
            ProtocolId::INTERACTION_MODEL,
            b"resp",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();
    alice.decode_inbound(&resp.wire_bytes, now).unwrap();

    let events = alice.finish_exchange(alice_sid, req.exchange_id);

    let [MrpEvent::SendStandaloneAck {
        session_id,
        exchange_id,
        packet,
    }] = events.as_slice()
    else {
        panic!("expected one standalone ACK, got {events:?}");
    };
    assert_eq!((*session_id, *exchange_id), (alice_sid, req.exchange_id));
    let flags = exchange_flags_of(&bob, bob_sid, packet);
    assert!(
        flags.contains(ExchangeFlags::INITIATOR | ExchangeFlags::ACK),
        "the exchange initiator's ACK carries I and A: {flags:?}"
    );
    assert!(
        !flags.contains(ExchangeFlags::RELIABLE),
        "an ACK is never reliable"
    );
    match bob.decode_inbound(packet, now).unwrap() {
        DecodeInboundOutput::AckOnly { acked_counter, .. } => {
            assert_eq!(acked_counter, resp.message_counter);
        }
        other => panic!("expected AckOnly at Bob, got {other:?}"),
    }
    assert!(bob.poll_timeout().is_none(), "Bob's retransmit is cleared");
    assert!(
        alice.poll_timeout().is_none(),
        "nothing left on Alice's timer"
    );
}

/// The converse: Bob finishes an exchange Alice started without answering on
/// it, so his ACK must NOT carry I, or Alice would not match it to her
/// exchange.
#[test]
fn finish_exchange_on_a_peer_initiated_exchange_sends_an_ack_without_i() {
    let (mut alice, alice_sid, mut bob, bob_sid) = paired();
    let now = Instant::now();
    let req = alice
        .encode_outbound(
            alice_sid,
            None,
            0x08,
            ProtocolId::INTERACTION_MODEL,
            b"req",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();
    bob.decode_inbound(&req.wire_bytes, now).unwrap();

    let events = bob.finish_exchange(bob_sid, req.exchange_id);

    let [MrpEvent::SendStandaloneAck { packet, .. }] = events.as_slice() else {
        panic!("expected one standalone ACK, got {events:?}");
    };
    let flags = exchange_flags_of(&alice, alice_sid, packet);
    assert!(flags.contains(ExchangeFlags::ACK), "{flags:?}");
    assert!(
        !flags.contains(ExchangeFlags::INITIATOR),
        "the responder's ACK must not carry I: {flags:?}"
    );
    match alice.decode_inbound(packet, now).unwrap() {
        DecodeInboundOutput::AckOnly { acked_counter, .. } => {
            assert_eq!(acked_counter, req.message_counter);
        }
        other => panic!("expected AckOnly at Alice, got {other:?}"),
    }
}

/// Abort flushes the ACK first (chip `Abort()` → `DoClose(true)` →
/// `FlushAcks`), then drops our own retransmits.
#[test]
fn abort_exchange_flushes_the_ack_then_drops_our_retransmits() {
    let (mut alice, alice_sid, mut bob, bob_sid) = paired();
    let now = Instant::now();
    let req = alice
        .encode_outbound(
            alice_sid,
            None,
            0x03,
            ProtocolId::INTERACTION_MODEL,
            b"subscribe",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();
    // Bob sends a reliable message on Alice's exchange WITHOUT having decoded
    // her request, so it carries no ACK for it: Alice now holds both her own
    // retransmit and an ACK owed to Bob.
    let report = bob
        .encode_outbound(
            bob_sid,
            Some(req.exchange_id),
            0x05,
            ProtocolId::INTERACTION_MODEL,
            b"report",
            MrpFlags { reliable: true },
            now,
        )
        .unwrap();
    alice.decode_inbound(&report.wire_bytes, now).unwrap();

    let events = alice.abort_exchange(alice_sid, req.exchange_id);

    assert!(
        matches!(events.as_slice(), [MrpEvent::SendStandaloneAck { .. }]),
        "the ACK goes out before the abort: {events:?}"
    );
    let later = alice.handle_timeout(now + Duration::from_secs(60));
    assert!(
        !later.iter().any(|e| matches!(
            e,
            MrpEvent::Retransmit { exchange_id, .. } | MrpEvent::Expired { exchange_id, .. }
                if *exchange_id == req.exchange_id
        )),
        "abort drops our retransmits: {later:?}"
    );
}

/// Teardown: one ACK per session that owes one, in session-id order. Eight
/// sessions, so a `HashMap` walk without the sort does not land in ascending
/// order by chance (with the sort removed this failed 100 runs out of 100;
/// with five it still passed 2 runs out of 20).
#[test]
fn flush_pending_acks_covers_every_session() {
    const PEERS: u8 = 8;
    let keys_for = |n: u8| PaseSessionKeys {
        ke: [0xB0 + n; 16],
        i2r_key: [0x40 + n; 16],
        r2i_key: [0x50 + n; 16],
        attestation_key: [0x60 + n; 16],
    };
    let mut alice = SessionManager::new();
    let now = Instant::now();
    let mut expected = Vec::new();
    for n in 0..PEERS {
        let alice_sid =
            alice.register_pase(keys_for(n), SessionRole::Initiator, 1, PeerHint::default());
        let mut peer = SessionManager::new();
        let peer_sid = peer.register_pase(
            keys_for(n),
            SessionRole::Responder,
            alice_sid.0,
            PeerHint::default(),
        );
        let out = peer
            .encode_outbound(
                peer_sid,
                None,
                0x05,
                ProtocolId::INTERACTION_MODEL,
                b"report",
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        alice.decode_inbound(&out.wire_bytes, now).unwrap();
        expected.push(alice_sid);
    }
    expected.sort_unstable_by_key(|sid| sid.0);

    let sessions: Vec<SessionId> = alice
        .flush_pending_acks()
        .iter()
        .map(|e| match e {
            MrpEvent::SendStandaloneAck { session_id, .. } => *session_id,
            other => panic!("expected only standalone ACKs, got {other:?}"),
        })
        .collect();

    assert_eq!(sessions, expected);
    assert!(alice.poll_timeout().is_none());
}
