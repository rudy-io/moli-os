//! matter.js byte-parity tests for `protocol_header`.
//!
//! Each test loads a JSON fixture captured by
//! `cargo xtask capture-protocol-header` (which drives matter.js's
//! `MessageCodec.encodePayloadHeader` with fixed inputs) and asserts:
//!
//! 1. `encode_protocol_header` produces byte-identical output for the
//!    same logical inputs.
//! 2. `decode_protocol_header` parses matter.js's bytes back into the
//!    exact same `ProtocolHeader` value.
//!
//! These are cross-verification artifacts — matter.js is the
//! oracle for the on-wire format until we have a real Matter device
//! to test against.

#![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use std::fs;
use std::path::PathBuf;

use matter_transport::framing::MessageCounter;
use matter_transport::protocol_header::{
    build_standalone_ack_header, decode_protocol_header, encode_protocol_header, opcode,
    ExchangeFlags, ProtocolHeader, ProtocolId,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    inputs: FixtureInputs,
    expected: FixtureExpected,
}

#[derive(Debug, Deserialize)]
struct FixtureInputs {
    exchange_id: u16,
    protocol_id: FixtureProtocolId,
    opcode: u8,
    is_initiator: bool,
    requires_ack: bool,
    #[serde(default)]
    ack_counter: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct FixtureProtocolId {
    vendor: u16,
    protocol: u16,
}

#[derive(Debug, Deserialize)]
struct FixtureExpected {
    wire_hex: String,
}

fn load_fixture(scenario: &str) -> Fixture {
    let path = PathBuf::from("../../test-vectors/transport").join(format!("{scenario}.json"));
    let bytes = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "fixture {} not found — run `cargo xtask capture-protocol-header`",
            path.display()
        )
    });
    serde_json::from_str(&bytes)
        .unwrap_or_else(|e| panic!("malformed fixture {}: {}", path.display(), e))
}

fn assemble(fx: &Fixture) -> ProtocolHeader {
    // Build the input header the way a real caller would: set I/R bits
    // directly; the encoder will auto-derive A and V from `ack_counter`
    // and `vendor`.
    let mut flags = ExchangeFlags::empty();
    if fx.inputs.is_initiator {
        flags |= ExchangeFlags::INITIATOR;
    }
    if fx.inputs.requires_ack {
        flags |= ExchangeFlags::RELIABLE;
    }
    if fx.inputs.ack_counter.is_some() {
        flags |= ExchangeFlags::ACK;
    }
    if fx.inputs.protocol_id.vendor != 0 {
        flags |= ExchangeFlags::VENDOR;
    }

    ProtocolHeader {
        exchange_flags: flags,
        opcode: fx.inputs.opcode,
        exchange_id: fx.inputs.exchange_id,
        protocol_id: ProtocolId {
            vendor: fx.inputs.protocol_id.vendor,
            protocol: fx.inputs.protocol_id.protocol,
        },
        ack_counter: fx.inputs.ack_counter.map(MessageCounter),
    }
}

fn run(scenario: &str) {
    let fx = load_fixture(scenario);
    let header = assemble(&fx);

    // Encode side: our bytes must equal matter.js's bytes.
    let mut ours = Vec::new();
    encode_protocol_header(&header, &mut ours);
    assert_eq!(
        hex::encode(&ours),
        fx.expected.wire_hex,
        "wire bytes diverged in scenario {scenario}",
    );

    // Decode side: matter.js's bytes parse back into the same header.
    let wire = hex::decode(&fx.expected.wire_hex).unwrap();
    let (parsed, rest) = decode_protocol_header(&wire).unwrap();
    assert_eq!(parsed, header);
    assert!(rest.is_empty(), "decoder must consume the full header");
}

#[test]
fn matter_js_byte_parity_initiator_reliable() {
    run("protocol-header-initiator-reliable");
}

#[test]
fn matter_js_byte_parity_responder_ack() {
    run("protocol-header-responder-ack");
}

#[test]
fn matter_js_byte_parity_standalone_ack() {
    run("protocol-header-standalone-ack");
}

#[test]
fn standalone_ack_helper_matches_fixture() {
    let fx = load_fixture("protocol-header-standalone-ack");
    let h = build_standalone_ack_header(
        fx.inputs.exchange_id,
        MessageCounter(fx.inputs.ack_counter.unwrap()),
        fx.inputs.is_initiator,
    );
    assert_eq!(h.opcode, opcode::secure_channel::STANDALONE_ACK);
    let mut bytes = Vec::new();
    encode_protocol_header(&h, &mut bytes);
    assert_eq!(hex::encode(&bytes), fx.expected.wire_hex);
}
