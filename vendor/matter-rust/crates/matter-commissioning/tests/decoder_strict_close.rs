//! The command-response decoders require one complete struct and find its
//! real end.
//!
//! Each command-response decoder takes a command-fields blob. In a
//! commissioning run that blob comes from `parse_invoke_response`, which
//! re-encodes it from a closed container, so it is already well-formed there.
//! (`decode_basic_commissioning_info` takes an attribute value re-encoded from
//! the `ReportData`.) The decoders are public, though, and take arbitrary bytes.
//! Two gaps are closed:
//!
//! - **Strict close.** Every strict prefix and any trailing bytes are
//!   rejected, as chip's `MessageParser::ExitContainer` rejects them on the
//!   message itself. The documented error is checked:
//!   `NocError::ClusterCodec(UnclosedContainer | TrailingData)` for the NOC
//!   decoders, and `CommissioningError::MalformedResponse(stage)`, which
//!   carries no codec detail, for the cluster decoders.
//! - **Nested containers.** Several decoders ignored a `ContainerStart`
//!   without skipping its body. The members of an unknown container field
//!   were then read as top-level fields, and its end-of-container ended the
//!   struct early. That path *is* reachable from a device: a response
//!   carrying a container-typed field from a newer revision either failed
//!   to decode or had its fields misattributed. Each test puts a decoy
//!   container, three levels deep, ahead of the real fields. The decoy reuses
//!   the real fields' tags, and the decoded values must be the real ones. The
//!   decoy message must also be well-formed-strict: truncation inside the
//!   decoy is rejected too.

#![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).

use matter_codec::{Error as CodecError, Tag, TlvWriter};
use matter_commissioning::clusters::general_commissioning::{
    decode_arm_fail_safe_response, decode_basic_commissioning_info,
    decode_set_regulatory_config_response,
};
use matter_commissioning::clusters::network_commissioning::{
    decode_connect_network_response, decode_network_config_response,
};
use matter_commissioning::noc::commands::{
    decode_attestation_response, decode_certificate_chain_response, decode_csr_response,
    decode_noc_response,
};
use matter_commissioning::noc::NocError;
use matter_commissioning::{CommissioningError, Stage};
use std::fmt::Debug;

const SUFFIXES: &[&[u8]] = &[&[0x00], &[0x04, 0x00], &[0x15, 0x18], &[0x18], &[0xFF]];

/// The errors one decoder must return: `unclosed` for end of input inside the
/// struct, `trailing` for bytes after it.
struct Expect<E> {
    unclosed: fn(&E) -> bool,
    trailing: fn(&E) -> bool,
}

const NOC: Expect<NocError> = Expect {
    unclosed: |e| matches!(e, NocError::ClusterCodec(CodecError::UnclosedContainer)),
    trailing: |e| matches!(e, NocError::ClusterCodec(CodecError::TrailingData)),
};

/// `CommissioningError` maps every malformation to `MalformedResponse(stage)`.
fn malformed(e: &CommissioningError, stage: Stage) -> bool {
    matches!(e, CommissioningError::MalformedResponse(s) if *s == stage)
}

const ARM: Expect<CommissioningError> = Expect {
    unclosed: |e| malformed(e, Stage::ArmFailsafe),
    trailing: |e| malformed(e, Stage::ArmFailsafe),
};

const REGULATORY: Expect<CommissioningError> = Expect {
    unclosed: |e| malformed(e, Stage::ConfigRegulatory),
    trailing: |e| malformed(e, Stage::ConfigRegulatory),
};

const NETWORK: Expect<CommissioningError> = Expect {
    unclosed: |e| malformed(e, Stage::NetworkSetup),
    trailing: |e| malformed(e, Stage::NetworkSetup),
};

/// `decode_basic_commissioning_info` returns `Option`: `None` is its only
/// error.
const OPTION: Expect<()> = Expect {
    unclosed: |()| true,
    trailing: |()| true,
};

fn assert_strict<E: Debug>(
    name: &str,
    full: &[u8],
    parse: impl Fn(&[u8]) -> Result<(), E>,
    expect: &Expect<E>,
) {
    if let Err(e) = parse(full) {
        panic!("{name}: the full struct must decode, got {e:?}");
    }
    for suffix in SUFFIXES {
        let mut tailed = full.to_vec();
        tailed.extend_from_slice(suffix);
        match parse(&tailed) {
            Err(e) if (expect.trailing)(&e) => {}
            got => panic!("{name}: trailing bytes {suffix:02x?} gave {got:?}"),
        }
    }
    // One byte short: exactly the final end-of-container is missing.
    match parse(&full[..full.len() - 1]) {
        Err(e) if (expect.unclosed)(&e) => {}
        got => panic!("{name}: missing final end-of-container gave {got:?}"),
    }
    for n in 0..full.len() {
        assert!(
            parse(&full[..n]).is_err(),
            "{name}: prefix of {n} / {} bytes accepted (truncated struct)",
            full.len()
        );
    }
}

/// Encode an anonymous struct. With `decoy`, it first writes an unknown
/// context-9 struct, three levels deep, holding `decoy`'s fields at every
/// level under the *real* fields' tags:
/// `9: { decoy.., 20: { decoy.., 21: [ { decoy.. } ] } }`. That is the shape a
/// newer revision's container field could take. Then `fields` writes the real
/// members.
fn encode(
    decoy: Option<&dyn Fn(&mut TlvWriter<'_>)>,
    fields: &dyn Fn(&mut TlvWriter<'_>),
) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous).unwrap();
    if let Some(decoy) = decoy {
        w.start_structure(Tag::Context(9)).unwrap();
        decoy(&mut w);
        w.start_structure(Tag::Context(20)).unwrap();
        decoy(&mut w);
        w.start_array(Tag::Context(21)).unwrap();
        w.start_structure(Tag::Anonymous).unwrap();
        decoy(&mut w);
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
        w.end_container().unwrap();
    }
    fields(&mut w);
    w.end_container().unwrap();
    buf
}

/// Run `assert_strict` on both the plain and the decoy encodings, and check
/// that a cut just inside the decoy (`0x15 0x35 0x09`, the decoy opened and
/// nothing more) is a truncation error.
fn check<E: Debug>(
    name: &str,
    decoy: &dyn Fn(&mut TlvWriter<'_>),
    real: &dyn Fn(&mut TlvWriter<'_>),
    parse: impl Fn(&[u8]) -> Result<(), E>,
    expect: &Expect<E>,
) -> Vec<u8> {
    let plain = encode(None, real);
    let decoyed = encode(Some(decoy), real);
    assert_strict(name, &plain, &parse, expect);
    assert_strict(&format!("{name} with decoy"), &decoyed, &parse, expect);
    assert_eq!(&decoyed[..3], &[0x15, 0x35, 0x09]);
    match parse(&decoyed[..3]) {
        Err(e) if (expect.unclosed)(&e) => {}
        got => panic!("{name}: cut inside the decoy gave {got:?}"),
    }
    decoyed
}

fn sig(fill: u8) -> [u8; 64] {
    [fill; 64]
}

#[test]
fn attestation_response() {
    let real = |w: &mut TlvWriter<'_>| {
        w.put_bytes(Tag::Context(0), b"elements").unwrap();
        w.put_bytes(Tag::Context(1), &sig(0xAA)).unwrap();
    };
    let decoy = |w: &mut TlvWriter<'_>| {
        w.put_bytes(Tag::Context(0), b"decoy").unwrap();
    };
    let decoyed = check(
        "AttestationResponse",
        &decoy,
        &real,
        |b| decode_attestation_response(b).map(drop),
        &NOC,
    );
    let got = decode_attestation_response(&decoyed).unwrap();
    assert_eq!(got.attestation_elements, b"elements");
    assert_eq!(got.signature, sig(0xAA));
}

#[test]
fn certificate_chain_response() {
    let real = |w: &mut TlvWriter<'_>| {
        w.put_bytes(Tag::Context(0), b"der").unwrap();
    };
    let decoy = |w: &mut TlvWriter<'_>| {
        w.put_bytes(Tag::Context(0), b"decoy").unwrap();
    };
    let decoyed = check(
        "CertificateChainResponse",
        &decoy,
        &real,
        |b| decode_certificate_chain_response(b).map(drop),
        &NOC,
    );
    let got = decode_certificate_chain_response(&decoyed).unwrap();
    assert_eq!(got.certificate, b"der");
}

#[test]
fn csr_response() {
    let real = |w: &mut TlvWriter<'_>| {
        w.put_bytes(Tag::Context(0), b"nocsr").unwrap();
        w.put_bytes(Tag::Context(1), &sig(0xBB)).unwrap();
    };
    let decoy = |w: &mut TlvWriter<'_>| {
        w.put_bytes(Tag::Context(0), b"decoy").unwrap();
    };
    let decoyed = check(
        "CSRResponse",
        &decoy,
        &real,
        |b| decode_csr_response(b).map(drop),
        &NOC,
    );
    let got = decode_csr_response(&decoyed).unwrap();
    assert_eq!(got.nocsr_elements, b"nocsr");
    assert_eq!(got.attestation_signature, sig(0xBB));
}

#[test]
fn noc_response() {
    let real = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_uint(Tag::Context(1), 3).unwrap();
        w.put_utf8(Tag::Context(2), "ok").unwrap();
    };
    let decoy = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 9).unwrap();
        w.put_uint(Tag::Context(1), 9).unwrap();
    };
    let decoyed = check(
        "NOCResponse",
        &decoy,
        &real,
        |b| decode_noc_response(b).map(drop),
        &NOC,
    );
    let got = decode_noc_response(&decoyed).unwrap();
    assert_eq!((got.status, got.fabric_index), (0, Some(3)));
    assert_eq!(got.debug_text.as_deref(), Some("ok"));
}

#[test]
fn arm_fail_safe_and_set_regulatory_config_responses() {
    let real = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_utf8(Tag::Context(1), "").unwrap();
    };
    let decoy = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 4).unwrap();
    };
    let decoyed = check(
        "ArmFailSafeResponse",
        &decoy,
        &real,
        |b| decode_arm_fail_safe_response(b).map(drop),
        &ARM,
    );
    check(
        "SetRegulatoryConfigResponse",
        &decoy,
        &real,
        |b| decode_set_regulatory_config_response(b).map(drop),
        &REGULATORY,
    );
    let got = decode_arm_fail_safe_response(&decoyed).unwrap();
    assert_eq!(got.error_code, 0);
}

#[test]
fn basic_commissioning_info() {
    let real = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 60).unwrap();
        w.put_uint(Tag::Context(1), 900).unwrap();
    };
    let decoy = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_uint(Tag::Context(1), 1).unwrap();
    };
    let decoyed = check(
        "BasicCommissioningInfo",
        &decoy,
        &real,
        |b| decode_basic_commissioning_info(b).map(drop).ok_or(()),
        &OPTION,
    );
    let got = decode_basic_commissioning_info(&decoyed).unwrap();
    assert_eq!(got.failsafe_expiry_length_seconds, 60);
    assert_eq!(got.max_cumulative_failsafe_seconds, 900);
}

#[test]
fn network_config_response() {
    let real = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_utf8(Tag::Context(1), "ok").unwrap();
        w.put_uint(Tag::Context(2), 0).unwrap();
    };
    let decoy = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 7).unwrap();
    };
    let decoyed = check(
        "NetworkConfigResponse",
        &decoy,
        &real,
        |b| decode_network_config_response(Stage::NetworkSetup, b).map(drop),
        &NETWORK,
    );
    let got = decode_network_config_response(Stage::NetworkSetup, &decoyed).unwrap();
    assert_eq!(got.networking_status, 0);
}

#[test]
fn connect_network_response() {
    let real = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 0).unwrap();
        w.put_utf8(Tag::Context(1), "ok").unwrap();
        w.put_int(Tag::Context(2), -1).unwrap();
    };
    let decoy = |w: &mut TlvWriter<'_>| {
        w.put_uint(Tag::Context(0), 7).unwrap();
    };
    let decoyed = check(
        "ConnectNetworkResponse",
        &decoy,
        &real,
        |b| decode_connect_network_response(Stage::NetworkSetup, b).map(drop),
        &NETWORK,
    );
    let got = decode_connect_network_response(Stage::NetworkSetup, &decoyed).unwrap();
    assert_eq!((got.networking_status, got.error_value), (0, Some(-1)));
}
