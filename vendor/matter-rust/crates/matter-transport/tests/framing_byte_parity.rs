//! matter.js byte-parity tests for the M5.1 framing layer.
//!
//! Loads JSON fixtures captured by `cargo xtask capture-framing`,
//! replays each through `encode_secured`, and asserts byte-identical
//! output. Also confirms `decode_secured` round-trips matter.js's
//! ciphertext back to the same plaintext.

#![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use std::fs;
use std::path::PathBuf;

use matter_transport::session::{SessionKeys, SessionRole};
use matter_transport::{
    decode_secured, encode_secured, DestNodeId, MessageCounter, NodeId, ReplayWindow,
    SecuredMessageFlags, SecuredMessageHeader, SecurityFlags, SessionId,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    inputs: FixtureInputs,
    expected: FixtureExpected,
}

#[derive(Debug, Deserialize)]
struct FixtureInputs {
    i2r_key: String,
    r2i_key: String,
    session_id: u16,
    message_counter: u32,
    #[serde(default)]
    source_node_id: Option<String>,
    #[serde(default)]
    destination_node_id: Option<String>,
    role: String,
    payload_hex: String,
}

#[derive(Debug, Deserialize)]
struct FixtureExpected {
    wire_hex: String,
}

fn load_fixture(scenario: &str) -> Fixture {
    let path = PathBuf::from("../../test-vectors/transport").join(format!("{scenario}.json"));
    let bytes = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "fixture {} not found — run `cargo xtask capture-framing`",
            path.display()
        )
    });
    serde_json::from_str(&bytes)
        .unwrap_or_else(|e| panic!("malformed fixture {}: {}", path.display(), e))
}

fn hex_to_key(s: &str) -> [u8; 16] {
    let bytes = hex::decode(s).unwrap();
    assert_eq!(bytes.len(), 16, "key must be 16 bytes (hex)");
    bytes.try_into().unwrap()
}

/// Parse a `NodeId` from a big-endian hex string. The capture script
/// (`xtask/scripts/capture-framing/index.js`) documents that fixtures
/// store node IDs as human-readable BE hex; the wire layout is LE,
/// converted inside `encode_secured`.
fn hex_to_node_id(s: &str) -> NodeId {
    let bytes = hex::decode(s).unwrap();
    assert_eq!(bytes.len(), 8, "node id must be 8 bytes (hex)");
    NodeId(u64::from_be_bytes(bytes.try_into().unwrap()))
}

fn assemble(fx: &Fixture) -> (SessionKeys, SessionRole, SecuredMessageHeader, Vec<u8>) {
    let keys = SessionKeys {
        i2r_key: hex_to_key(&fx.inputs.i2r_key),
        r2i_key: hex_to_key(&fx.inputs.r2i_key),
        attestation_key: [0u8; 16], // not used by framing
    };
    let role = match fx.inputs.role.as_str() {
        "Initiator" => SessionRole::Initiator,
        "Responder" => SessionRole::Responder,
        other => panic!("unexpected role {other}"),
    };

    let source_node_id = fx.inputs.source_node_id.as_deref().map(hex_to_node_id);
    let destination_node_id = fx
        .inputs
        .destination_node_id
        .as_deref()
        .map(|s| DestNodeId::Node(hex_to_node_id(s)));

    let mut flags = SecuredMessageFlags::empty();
    if source_node_id.is_some() {
        flags |= SecuredMessageFlags::SOURCE_PRESENT;
    }
    if destination_node_id.is_some() {
        flags |= SecuredMessageFlags::DEST_UNICAST;
    }

    let header = SecuredMessageHeader {
        flags,
        session_id: SessionId(fx.inputs.session_id),
        security_flags: SecurityFlags::empty(),
        message_counter: MessageCounter(fx.inputs.message_counter),
        source_node_id,
        destination_node_id,
    };
    let payload = hex::decode(&fx.inputs.payload_hex).unwrap();
    (keys, role, header, payload)
}

fn run(scenario: &str) {
    let fx = load_fixture(scenario);
    let (keys, role, header, payload) = assemble(&fx);

    // Encode side: our wire bytes must match matter.js's byte-for-byte.
    let ours = encode_secured(
        &header,
        &payload,
        &keys,
        role,
        header.source_node_id.map_or(0, |n| n.0),
    )
    .unwrap();
    assert_eq!(
        hex::encode(&ours),
        fx.expected.wire_hex,
        "wire bytes diverged in scenario {scenario}",
    );

    // Decode side: matter.js's wire bytes must roundtrip back to the
    // same plaintext through our decoder. Use the opposite role (the
    // peer's perspective).
    let peer_role = match role {
        SessionRole::Initiator => SessionRole::Responder,
        SessionRole::Responder => SessionRole::Initiator,
    };
    let wire = hex::decode(&fx.expected.wire_hex).unwrap();
    let mut window = ReplayWindow::new();
    let (header_back, payload_back) = decode_secured(
        &wire,
        &keys,
        peer_role,
        &mut window,
        header.source_node_id.map_or(0, |n| n.0),
    )
    .unwrap();
    assert_eq!(header_back, header);
    assert_eq!(payload_back, payload);
}

#[test]
fn matter_js_byte_parity_pase_session() {
    run("framing-pase-session");
}

#[test]
fn matter_js_byte_parity_case_session() {
    run("framing-case-session");
}

#[test]
fn matter_js_byte_parity_with_mrp_ack() {
    run("framing-with-mrp-ack");
}
