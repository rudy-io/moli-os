//! matter.js byte-parity for the Secure Channel `CloseSession` message
//! (Matter 1.4 Core §4.11.1.4).
//!
//! The fixture, `test-vectors/transport/close-session.json`, is captured by
//! `cargo xtask capture-close-session`, which runs matter.js 0.17.1's
//! `SecureChannelMessenger.sendCloseSession` on a freshly initiated exchange
//! and records its payload header and payload. The exchange id is the
//! capture's input; ours comes from the node-wide counter, so the end-to-end
//! test substitutes it before comparing.

#![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use matter_crypto::pase::PaseSessionKeys;
use matter_transport::protocol_header::{
    encode_protocol_header, opcode, ExchangeFlags, ProtocolHeader, ProtocolId,
};
use matter_transport::secure_channel::{
    encode_close_session, is_close_session, CLOSE_SESSION_PAYLOAD,
};
use matter_transport::session::{PeerHint, SessionManager, SessionRole};
use matter_transport::{decode_secured, ReplayWindow};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    inputs: Inputs,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct Inputs {
    exchange_id: u16,
}

#[derive(Debug, Deserialize)]
struct Expected {
    payload_header_hex: String,
    payload_hex: String,
}

fn load() -> Fixture {
    let path = PathBuf::from("../../test-vectors/transport/close-session.json");
    let text = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "fixture {} not found — run `cargo xtask capture-close-session`",
            path.display()
        )
    });
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("malformed fixture {}: {e}", path.display()))
}

/// Our `CloseSession` payload is matter.js's, and our recogniser accepts it.
#[test]
fn close_session_payload_matches_matter_js() {
    let fx = load();
    assert_eq!(hex::encode(CLOSE_SESSION_PAYLOAD), fx.expected.payload_hex);
    let theirs = hex::decode(&fx.expected.payload_hex).unwrap();
    assert!(is_close_session(
        ProtocolId::SECURE_CHANNEL,
        opcode::secure_channel::STATUS_REPORT,
        &theirs
    ));
}

/// For the capture's exchange id, the header we build for a `CloseSession`
/// (I set, R and A clear, opcode 0x40, protocol `SecureChannel`) is matter.js's.
#[test]
fn close_session_header_matches_matter_js() {
    let fx = load();
    let header = ProtocolHeader {
        exchange_flags: ExchangeFlags::INITIATOR,
        opcode: opcode::secure_channel::STATUS_REPORT,
        exchange_id: fx.inputs.exchange_id,
        protocol_id: ProtocolId::SECURE_CHANNEL,
        ack_counter: None,
    };
    let mut ours = Vec::new();
    encode_protocol_header(&header, &mut ours);
    assert_eq!(hex::encode(ours), fx.expected.payload_header_hex);
}

fn pase_keys() -> PaseSessionKeys {
    PaseSessionKeys {
        ke: [0xAAu8; 16],
        i2r_key: [0x11u8; 16],
        r2i_key: [0x22u8; 16],
        attestation_key: [0x33u8; 16],
    }
}

/// End to end: what `encode_close_session` puts on the wire decrypts, with
/// the peer's keys, to matter.js's header (with our exchange id) followed by
/// matter.js's payload.
#[test]
fn an_encoded_close_session_decrypts_to_matter_js_bytes() {
    let fx = load();
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

    let out = encode_close_session(&mut alice, alice_sid, Instant::now()).unwrap();
    assert!(
        out.is_local_initiator,
        "a CloseSession opens a new exchange"
    );
    assert!(!out.piggyback_acked, "nothing is owed on a new exchange");

    let session = bob.get(bob_sid).unwrap();
    let (_, plaintext) = decode_secured(
        &out.wire_bytes,
        &session.keys,
        session.role,
        &mut ReplayWindow::new(),
        session.peer_nonce_node_id,
    )
    .unwrap();
    let mut expected = hex::decode(&fx.expected.payload_header_hex).unwrap();
    expected[2..4].copy_from_slice(&out.exchange_id.to_le_bytes());
    expected.extend(hex::decode(&fx.expected.payload_hex).unwrap());
    assert_eq!(hex::encode(plaintext), hex::encode(expected));
}
