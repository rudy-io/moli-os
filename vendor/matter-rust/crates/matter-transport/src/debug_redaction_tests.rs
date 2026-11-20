//! Secrets and decrypted payloads stay out of `Debug` output.
//!
//! [`SessionKeys`](crate::session::SessionKeys) holds a session's AES-CCM keys
//! (its tests live beside it in `session.rs`). The two decrypted-payload
//! carriers, [`DecodeInboundOutput::AppMessage`] and
//! [`InboundOutcome::AppMessage`], hold the plaintext of whatever the peer
//! sent: a Wi-Fi passphrase, a Thread dataset, an `AddNOC` IPK echoed in a
//! response. Their `Debug` prints the payload's length only, as
//! `"<N bytes>"`, and every other field exactly as the old derive did.
//!
//! No assertion here prints a rendering or a payload on failure: a failing
//! redaction would print the very bytes it failed to hide. Hence `assert!`
//! with static messages throughout, and a leak detector that names only
//! *which* rendering leaked.

use crate::framing::{MessageCounter, SessionId};
use crate::mrp::InboundOutcome;
use crate::protocol_header::ProtocolId;
use crate::session::DecodeInboundOutput;

/// Sixteen distinct bytes starting at `first` (`first..first + 16`). With
/// `first >= 0xC0` every byte renders as a three-digit decimal or a
/// `c0`..`ff` hex pair, which nothing else in these structs prints.
pub(crate) fn synthetic_bytes(first: u8) -> [u8; 16] {
    core::array::from_fn(|i| {
        // `i < 16` and every `first` used is at most 0xE0, so the cast is
        // lossless and the sum cannot overflow.
        #[allow(clippy::cast_possible_truncation)]
        let offset = i as u8;
        first + offset
    })
}

/// Every way `Debug` can print one byte: decimal (`{:?}`), hex (`{:x?}`,
/// `{:X?}`) and prefixed hex (`{:#x?}`, `{:#X?}`).
fn byte_tokens(b: u8) -> [String; 5] {
    [
        format!("{b}"),
        format!("{b:x}"),
        format!("{b:X}"),
        format!("{b:#x}"),
        format!("{b:#X}"),
    ]
}

/// Which rendering of `secret` appears in `shown`, if any.
///
/// Returns a static description, never the bytes, so a caller can put it in
/// a panic message. Three checks:
///
/// 1. **Byte lists, whitespace-insensitive.** `shown` and each list are
///    compared with all whitespace removed, so the compact layout
///    (`192, 193`) and the pretty one (`192,\n    193`) both match.
/// 2. **Hex runs.** The bytes as one run of hex digits, either case.
/// 3. **A lone byte.** `{:#?}` prints each list element on its own line.
///    Any line that is nothing but one of `secret`'s byte tokens (and an
///    optional trailing comma) is a leak, even if only one byte got out.
///    Matching whole lines, not substrings, is what keeps this free of false
///    positives: a named numeric field prints as `name: 200,`, never as a
///    bare `200,`, so a random exchange id or timestamp elsewhere in the
///    struct cannot trip it.
pub(crate) fn leak(shown: &str, secret: &[u8]) -> Option<&'static str> {
    use core::fmt::Write as _;

    let squeezed: String = shown.chars().filter(|c| !c.is_whitespace()).collect();
    for which in 0..5 {
        let list = secret
            .iter()
            .map(|&b| byte_tokens(b)[which].clone())
            .collect::<Vec<_>>()
            .join(",");
        if squeezed.contains(&list) {
            return Some("a secret's Debug byte list leaked");
        }
    }

    let run = secret.iter().fold(String::new(), |mut run, b| {
        // Writing into a `String` cannot fail, so the `fmt::Result` is moot.
        let _ = write!(run, "{b:02x}");
        run
    });
    if shown.contains(&run) || shown.contains(&run.to_uppercase()) {
        return Some("a secret leaked as a run of hex digits");
    }

    for line in shown.lines() {
        let line = line.trim();
        let line = line.strip_suffix(',').unwrap_or(line);
        if secret
            .iter()
            .any(|&b| byte_tokens(b).iter().any(|t| t == line))
        {
            return Some("a secret byte leaked on its own line");
        }
    }
    None
}

/// Panic (with a static message) if `shown` carries any rendering of
/// `secret`. Never prints `shown` or `secret`.
pub(crate) fn assert_hidden(shown: &str, secret: &[u8]) {
    if let Some(what) = leak(shown, secret) {
        panic!("{what}");
    }
}

/// Every `Debug` flavour of `value`: plain, pretty, and the four hex-integer
/// flag combinations, which a derive pushes down into a byte list.
pub(crate) fn renderings<T: core::fmt::Debug>(value: &T) -> [String; 6] {
    [
        format!("{value:?}"),
        format!("{value:#?}"),
        format!("{value:x?}"),
        format!("{value:X?}"),
        format!("{value:#x?}"),
        format!("{value:#X?}"),
    ]
}

#[test]
fn leak_detector_catches_every_layout() {
    // The detector is only worth something if it fires. Feed it the raw
    // secret in every layout `Debug` has, and check it notices each one.
    let secret = synthetic_bytes(0xC0);
    for shown in renderings(&secret)
        .iter()
        .chain(&renderings(&secret.to_vec()))
    {
        assert!(leak(shown, &secret).is_some(), "a layout went unnoticed");
    }

    // The gap this detector closes: the pretty layout puts every element on
    // its own line, so a plain substring search for `192, 193` misses it.
    let pretty = format!("{secret:#?}");
    assert!(!pretty.contains("192, 193"), "the pretty layout changed");
    assert!(leak(&pretty, &secret).is_some(), "the pretty layout leaked");

    // A single escaped byte is still a leak.
    let one = format!("{:#?}", vec![secret[5]]);
    assert!(leak(&one, &secret).is_some(), "a lone byte went unnoticed");
    let one_hex = format!("{:#x?}", vec![secret[5]]);
    assert!(leak(&one_hex, &secret).is_some(), "a lone hex byte");

    // No false positives from a length placeholder or a named field that
    // happens to hold one of the byte values.
    assert!(
        leak("payload: \"<16 bytes>\"", &secret).is_none(),
        "placeholder"
    );
    assert!(
        leak("Foo {\n    exchange_id: 192,\n}", &secret).is_none(),
        "a named field is not a leaked byte"
    );
}

#[test]
#[allow(
    clippy::manual_assert_eq,
    reason = "assert_eq! would print the payload bytes on failure"
)]
fn decode_inbound_app_message_debug_shows_only_the_payload_length() {
    let payload = synthetic_bytes(0xC0);
    let out = DecodeInboundOutput::AppMessage {
        session_id: SessionId(7),
        exchange_id: 0x1234,
        is_initiator: true,
        protocol_id: ProtocolId {
            vendor: 0,
            protocol: 1,
        },
        opcode: 8,
        payload: payload.to_vec(),
    };

    // Same variant name, field names and field order as the old derive; only
    // the payload's value changes.
    assert!(
        format!("{out:?}")
            == "AppMessage { session_id: SessionId(7), exchange_id: 4660, \
                is_initiator: true, protocol_id: ProtocolId { vendor: 0, protocol: 1 }, \
                opcode: 8, payload: \"<16 bytes>\" }",
        "unexpected AppMessage rendering"
    );
    for shown in renderings(&out) {
        assert!(
            shown.contains("\"<16 bytes>\""),
            "the placeholder is missing"
        );
        assert_hidden(&shown, &payload);
    }
    // The value itself is untouched: only its rendering is hidden.
    let DecodeInboundOutput::AppMessage { payload: kept, .. } = out else {
        panic!("not an AppMessage");
    };
    // `assert!(a == b)`, not `assert_eq!`, so a failure never prints it.
    assert!(kept == payload, "the payload was altered");
}

#[test]
fn decode_inbound_other_variants_print_as_derived() {
    // The two variants without plaintext print exactly as the derive did,
    // including the (encrypted) standalone-ack packet.
    let ack_only = DecodeInboundOutput::AckOnly {
        session_id: SessionId(7),
        exchange_id: 3,
        acked_counter: MessageCounter(9),
    };
    assert!(
        format!("{ack_only:?}")
            == "AckOnly { session_id: SessionId(7), exchange_id: 3, \
                acked_counter: MessageCounter(9) }",
        "unexpected AckOnly rendering"
    );
    let resent = DecodeInboundOutput::DuplicateReliableAckResent {
        session_id: SessionId(7),
        exchange_id: 3,
        ack_packet: vec![1, 2, 3],
    };
    assert!(
        format!("{resent:?}")
            == "DuplicateReliableAckResent { session_id: SessionId(7), exchange_id: 3, \
                ack_packet: [1, 2, 3] }",
        "unexpected DuplicateReliableAckResent rendering"
    );
}

#[test]
#[allow(
    clippy::manual_assert_eq,
    reason = "assert_eq! would print the payload bytes on failure"
)]
fn inbound_outcome_app_message_debug_shows_only_the_payload_length() {
    let payload = synthetic_bytes(0xC0);
    let out = InboundOutcome::AppMessage {
        exchange_id: 0x1234,
        is_initiator: false,
        protocol_id: ProtocolId {
            vendor: 0,
            protocol: 1,
        },
        opcode: 8,
        payload: payload.to_vec(),
    };

    assert!(
        format!("{out:?}")
            == "AppMessage { exchange_id: 4660, is_initiator: false, \
                protocol_id: ProtocolId { vendor: 0, protocol: 1 }, opcode: 8, \
                payload: \"<16 bytes>\" }",
        "unexpected InboundOutcome::AppMessage rendering"
    );
    for shown in renderings(&out) {
        assert!(
            shown.contains("\"<16 bytes>\""),
            "the placeholder is missing"
        );
        assert_hidden(&shown, &payload);
    }
    let InboundOutcome::AppMessage { payload: kept, .. } = out else {
        panic!("not an AppMessage");
    };
    // `assert!(a == b)`, not `assert_eq!`, so a failure never prints it.
    assert!(kept == payload, "the payload was altered");
}

#[test]
fn inbound_outcome_other_variants_print_as_derived() {
    let ack_only = InboundOutcome::AckOnly {
        exchange_id: 3,
        acked_counter: MessageCounter(9),
    };
    assert!(
        format!("{ack_only:?}") == "AckOnly { exchange_id: 3, acked_counter: MessageCounter(9) }",
        "unexpected AckOnly rendering"
    );
    let dup = InboundOutcome::DuplicateReliable {
        exchange_id: 3,
        peer_counter: MessageCounter(9),
        is_local_initiator: true,
    };
    assert!(
        format!("{dup:?}")
            == "DuplicateReliable { exchange_id: 3, peer_counter: MessageCounter(9), \
                is_local_initiator: true }",
        "unexpected DuplicateReliable rendering"
    );
}

#[test]
fn an_empty_payload_still_prints_its_length() {
    let out = InboundOutcome::AppMessage {
        exchange_id: 1,
        is_initiator: false,
        protocol_id: ProtocolId {
            vendor: 0,
            protocol: 1,
        },
        opcode: 0x10,
        payload: Vec::new(),
    };
    assert!(
        format!("{out:?}").contains("payload: \"<0 bytes>\""),
        "an empty payload should print as <0 bytes>"
    );
}
