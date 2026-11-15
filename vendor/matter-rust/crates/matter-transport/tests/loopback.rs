//! Full-stack loopback integration test for M5.3.
//!
//! Two `TokioUdpTransport` instances on `[::1]:0` exchange one reliable
//! Matter message + one piggyback-acked response through their
//! respective `SessionManager`s. Validates that every M5 layer
//! (framing AES-CCM + protocol header + MRP + session manager + Tokio
//! UDP) works end-to-end on real sockets.
//!
//! No MRP timing assertions — M5.2's simulated-clock tests cover that.

#![cfg(feature = "tokio")]
#![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use matter_crypto::pase::PaseSessionKeys;
use matter_transport::{
    protocol_header::ProtocolId,
    session::{DecodeInboundOutput, PeerHint, SessionManager, SessionRole},
    MrpFlags, PeerAddress, TokioUdpTransport, Transport,
};

fn pase_keys() -> PaseSessionKeys {
    PaseSessionKeys {
        ke: [0xAAu8; 16],
        i2r_key: [0x11u8; 16],
        r2i_key: [0x22u8; 16],
        attestation_key: [0x33u8; 16],
    }
}

/// Poll `transport.poll_recv` until a packet arrives or `deadline` elapses.
async fn recv_with_deadline(
    transport: &mut TokioUdpTransport,
    deadline: Instant,
) -> Option<(PeerAddress, Vec<u8>)> {
    loop {
        if let Some(pair) = transport.poll_recv().unwrap() {
            return Some(pair);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn full_session_roundtrip_via_real_udp() {
    let now0 = Instant::now();

    // 1. Bind two transports on loopback.
    let mut alice_tx = TokioUdpTransport::bind_addr("[::1]:0".parse::<SocketAddr>().unwrap())
        .await
        .unwrap();
    let mut bob_tx = TokioUdpTransport::bind_addr("[::1]:0".parse::<SocketAddr>().unwrap())
        .await
        .unwrap();
    let alice_addr = PeerAddress(alice_tx.local_address());
    let bob_addr = PeerAddress(bob_tx.local_address());

    // 2. Create two session managers and register matching PASE sessions.
    let mut alice_mgr = SessionManager::new();
    let mut bob_mgr = SessionManager::new();
    let keys = pase_keys();
    let alice_sid = alice_mgr.register_pase(
        keys.clone(),
        SessionRole::Initiator,
        /* peer_session_id */ 1,
        PeerHint::default(),
    );
    let bob_sid = bob_mgr.register_pase(
        keys,
        SessionRole::Responder,
        /* peer_session_id */ alice_sid.0,
        PeerHint::default(),
    );

    // 3. Alice encodes a reliable outbound app message.
    let out1 = alice_mgr
        .encode_outbound(
            alice_sid,
            None,
            0x02,
            ProtocolId::INTERACTION_MODEL,
            b"ping",
            MrpFlags { reliable: true },
            now0,
        )
        .unwrap();
    let exchange_id = out1.exchange_id;

    // 4. Alice ships via transport. Prime write-readiness first — Tokio's
    //    try_send_to returns WouldBlock on a freshly-bound UDP socket
    //    before the runtime has registered write readiness.
    alice_tx.socket().writable().await.unwrap();
    alice_tx.send(bob_addr, out1.wire_bytes).unwrap();

    // 5. Bob receives.
    let deadline = Instant::now() + Duration::from_secs(1);
    let (from_alice, packet1) = recv_with_deadline(&mut bob_tx, deadline)
        .await
        .expect("Bob did not receive Alice's packet within 1s");
    assert_eq!(from_alice.0.port(), alice_addr.0.port());

    // 6. Bob decodes.
    let decoded = bob_mgr.decode_inbound(&packet1, Instant::now()).unwrap();
    let payload = match decoded {
        DecodeInboundOutput::AppMessage { payload, .. } => payload,
        other => panic!("expected AppMessage at Bob, got {other:?}"),
    };
    assert_eq!(payload, b"ping");

    // 7. Bob sends an unreliable response in the same exchange. The
    //    pending piggyback drains — Bob's outbound carries A=1.
    let out2 = bob_mgr
        .encode_outbound(
            bob_sid,
            Some(exchange_id),
            0x03,
            ProtocolId::INTERACTION_MODEL,
            b"pong",
            MrpFlags::default(),
            Instant::now(),
        )
        .unwrap();
    assert!(
        out2.piggyback_acked,
        "Bob's response must piggyback Alice's ack"
    );

    bob_tx.socket().writable().await.unwrap();
    bob_tx.send(alice_addr, out2.wire_bytes).unwrap();

    // 8. Alice receives Bob's response.
    let deadline = Instant::now() + Duration::from_secs(1);
    let (_, packet2) = recv_with_deadline(&mut alice_tx, deadline)
        .await
        .expect("Alice did not receive Bob's response within 1s");

    // 9. Alice decodes; the embedded ack clears her pending retransmit.
    let decoded = alice_mgr.decode_inbound(&packet2, Instant::now()).unwrap();
    let payload = match decoded {
        DecodeInboundOutput::AppMessage { payload, .. } => payload,
        other => panic!("expected AppMessage at Alice, got {other:?}"),
    };
    assert_eq!(payload, b"pong");
    assert!(
        alice_mgr
            .get(alice_sid)
            .unwrap()
            .mrp
            .poll_timeout()
            .is_none(),
        "Alice's pending retransmit must be cleared by Bob's piggyback ack",
    );
}
