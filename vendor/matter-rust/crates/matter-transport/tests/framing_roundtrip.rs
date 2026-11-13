//! Local encode → decode roundtrips for `encode_secured` / `decode_secured`.
//!
//! These confirm the two functions are inverses of each other; matter.js
//! byte-parity is verified separately in `framing_byte_parity.rs`.

#![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use matter_transport::session::{SessionKeys, SessionRole};
use matter_transport::{
    decode_secured, encode_secured, MessageCounter, NodeId, ReplayWindow, SecuredMessageFlags,
    SecuredMessageHeader, SecurityFlags, SessionId,
};

fn fake_keys() -> SessionKeys {
    SessionKeys {
        i2r_key: [0x11; 16],
        r2i_key: [0x22; 16],
        attestation_key: [0x33; 16],
    }
}

#[test]
fn initiator_to_responder_roundtrip() {
    let keys = fake_keys();
    let header = SecuredMessageHeader {
        flags: SecuredMessageFlags::empty(),
        session_id: SessionId(0x4242),
        security_flags: SecurityFlags::empty(),
        message_counter: MessageCounter(1),
        source_node_id: None,
        destination_node_id: None,
    };
    let payload = b"hello matter";

    // Initiator encodes (uses i2r_key).
    let wire = encode_secured(&header, payload, &keys, SessionRole::Initiator, 0).unwrap();
    assert!(wire.len() > payload.len() + 8, "header + ciphertext + tag");

    // Responder decodes (the matching i2r_key for inbound — it's the
    // initiator's outbound key from the responder's point of view).
    let mut window = ReplayWindow::new();
    let (decoded_header, decoded_payload) =
        decode_secured(&wire, &keys, SessionRole::Responder, &mut window, 0).unwrap();
    assert_eq!(decoded_header, header);
    assert_eq!(decoded_payload, payload);
}

#[test]
fn responder_to_initiator_roundtrip() {
    let keys = fake_keys();
    let header = SecuredMessageHeader {
        flags: SecuredMessageFlags::SOURCE_PRESENT,
        session_id: SessionId(0x1234),
        security_flags: SecurityFlags::CONTROL,
        message_counter: MessageCounter(0x8000_0001),
        source_node_id: Some(NodeId(0xDEAD_BEEF_CAFE_BABE)),
        destination_node_id: None,
    };
    let payload = vec![0x42u8; 100];

    let wire = encode_secured(&header, &payload, &keys, SessionRole::Responder, 0).unwrap();
    let mut window = ReplayWindow::new();
    let (decoded_header, decoded_payload) =
        decode_secured(&wire, &keys, SessionRole::Initiator, &mut window, 0).unwrap();
    assert_eq!(decoded_header, header);
    assert_eq!(decoded_payload, payload);
}

#[test]
fn case_nonce_binds_sender_operational_node_id() {
    // CASE sessions compute the AES-CCM nonce over the SENDER's operational
    // node id even though the wire header omits it (spec §4.8.2; chip
    // `CryptoContext::BuildNonce`). PASE sessions use 0. Encrypting with a
    // zero nonce-node-id makes real devices silently drop every secured
    // CASE-session frame (observed: Tapo P110M, M6.6.5 validation).
    let keys = fake_keys();
    let header = SecuredMessageHeader {
        flags: SecuredMessageFlags::empty(),
        session_id: SessionId(0x2d26),
        security_flags: SecurityFlags::empty(),
        message_counter: MessageCounter(7),
        source_node_id: None, // wire header omits it on secured sessions
        destination_node_id: None,
    };
    let payload = b"commissioning-complete";
    let sender_node_id: u64 = 0x1234_5678_9ABC_DEF0;

    let wire = encode_secured(
        &header,
        payload,
        &keys,
        SessionRole::Initiator,
        sender_node_id,
    )
    .unwrap();

    // Decrypting with the wrong nonce node id (0, the PASE convention) fails.
    let mut window = ReplayWindow::new();
    assert!(
        decode_secured(&wire, &keys, SessionRole::Responder, &mut window, 0).is_err(),
        "zero nonce-node-id must not decrypt a CASE frame"
    );

    // Decrypting with the sender's operational node id succeeds.
    let mut window = ReplayWindow::new();
    let (decoded_header, decoded_payload) = decode_secured(
        &wire,
        &keys,
        SessionRole::Responder,
        &mut window,
        sender_node_id,
    )
    .unwrap();
    assert_eq!(decoded_header, header);
    assert_eq!(decoded_payload, payload);
}

#[test]
fn wrong_role_fails_decryption() {
    let keys = fake_keys();
    let header = SecuredMessageHeader {
        flags: SecuredMessageFlags::empty(),
        session_id: SessionId(1),
        security_flags: SecurityFlags::empty(),
        message_counter: MessageCounter(1),
        source_node_id: None,
        destination_node_id: None,
    };
    let payload = b"oops";

    // Initiator encodes…
    let wire = encode_secured(&header, payload, &keys, SessionRole::Initiator, 0).unwrap();

    // …but decoder uses the SAME role (so wrong key direction).
    let mut window = ReplayWindow::new();
    let err = decode_secured(&wire, &keys, SessionRole::Initiator, &mut window, 0).unwrap_err();
    assert!(matches!(err, matter_transport::Error::DecryptionFailed));
}

#[test]
fn replay_detected_on_decode() {
    let keys = fake_keys();
    let header = SecuredMessageHeader {
        flags: SecuredMessageFlags::empty(),
        session_id: SessionId(1),
        security_flags: SecurityFlags::empty(),
        message_counter: MessageCounter(7),
        source_node_id: None,
        destination_node_id: None,
    };
    let payload = b"x";

    let wire = encode_secured(&header, payload, &keys, SessionRole::Initiator, 0).unwrap();
    let mut window = ReplayWindow::new();
    decode_secured(&wire, &keys, SessionRole::Responder, &mut window, 0).unwrap();
    let err = decode_secured(&wire, &keys, SessionRole::Responder, &mut window, 0).unwrap_err();
    assert!(matches!(
        err,
        matter_transport::Error::ReplayedCounter { counter: 7 }
    ));
}

#[test]
fn tampered_ciphertext_rejected() {
    let keys = fake_keys();
    let header = SecuredMessageHeader {
        flags: SecuredMessageFlags::empty(),
        session_id: SessionId(1),
        security_flags: SecurityFlags::empty(),
        message_counter: MessageCounter(1),
        source_node_id: None,
        destination_node_id: None,
    };
    let payload = b"matter";

    let mut wire = encode_secured(&header, payload, &keys, SessionRole::Initiator, 0).unwrap();
    // Flip a bit in the ciphertext (byte 8 onward is the encrypted payload).
    wire[8] ^= 1;
    let mut window = ReplayWindow::new();
    let err = decode_secured(&wire, &keys, SessionRole::Responder, &mut window, 0).unwrap_err();
    assert!(matches!(err, matter_transport::Error::DecryptionFailed));
}

#[test]
fn tampered_header_rejected_via_aad() {
    let keys = fake_keys();
    let header = SecuredMessageHeader {
        flags: SecuredMessageFlags::empty(),
        session_id: SessionId(1),
        security_flags: SecurityFlags::empty(),
        message_counter: MessageCounter(1),
        source_node_id: None,
        destination_node_id: None,
    };
    let payload = b"matter";

    let mut wire = encode_secured(&header, payload, &keys, SessionRole::Initiator, 0).unwrap();
    // Flip a bit in the security_flags byte (offset 3) — this should
    // both fail AES-CCM tag verification (because AAD changed) AND change
    // the nonce (since SecurityFlags is the first byte of the nonce).
    wire[3] ^= SecurityFlags::CONTROL.bits();
    let mut window = ReplayWindow::new();
    let err = decode_secured(&wire, &keys, SessionRole::Responder, &mut window, 0).unwrap_err();
    assert!(matches!(err, matter_transport::Error::DecryptionFailed));
}
