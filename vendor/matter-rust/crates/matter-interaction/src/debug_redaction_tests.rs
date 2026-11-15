//! Command and attribute payloads stay out of `Debug` output.
//!
//! [`InvokeResponse::Command`]'s `fields_tlv`, [`InvokedCommand`]'s
//! `fields_tlv` and [`AttributeWriteRequest`]'s `value_tlv` carry a command's
//! or attribute's plaintext TLV. Depending on the command that can be a Wi-Fi
//! passphrase, a Thread dataset, a group key or an `AddNOC` IPK, so their
//! `Debug` prints the byte field's length only (`"<N bytes>"`) and every
//! other field exactly as the old derive did.
//!
//! No assertion here prints a rendering or a payload on failure: a failing
//! redaction would print the very bytes it failed to hide. Hence `assert!`
//! with static messages throughout, and a leak detector that names only
//! *which* rendering leaked. The detector is the same as matter-transport's
//! (`debug_redaction_tests` there); each crate keeps its own copy so the
//! crates stay independent.

use crate::invoke::{InvokeResponse, InvokeResponseEntry};
use crate::invoke_server::{InvokedCommand, ParsedInvokeRequest};
use crate::path::{AttributePath, CommandPath};
use crate::status::ImStatus;
use crate::write::AttributeWriteRequest;

/// Sixteen distinct bytes starting at `first` (`first..first + 16`). With
/// `first >= 0xC0` every byte renders as a three-digit decimal or a
/// `c0`..`ff` hex pair, which nothing else in these structs prints.
fn synthetic_bytes(first: u8) -> [u8; 16] {
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
fn leak(shown: &str, secret: &[u8]) -> Option<&'static str> {
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
fn assert_hidden(shown: &str, secret: &[u8]) {
    if let Some(what) = leak(shown, secret) {
        panic!("{what}");
    }
}

/// Every `Debug` flavour of `value`: plain, pretty, and the four hex-integer
/// flag combinations, which a derive pushes down into a byte list.
fn renderings<T: core::fmt::Debug>(value: &T) -> [String; 6] {
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

const PATH: CommandPath = CommandPath {
    endpoint: 0,
    cluster: 0x0031,
    command: 0x05,
};

#[test]
#[allow(
    clippy::manual_assert_eq,
    reason = "assert_eq! would print the payload bytes on failure"
)]
fn invoke_response_command_debug_shows_only_the_fields_length() {
    let fields = synthetic_bytes(0xC0);
    let response = InvokeResponse::Command {
        path: PATH,
        fields_tlv: fields.to_vec(),
    };

    // Same variant name, field names and field order as the old derive; only
    // the payload's value changes.
    assert!(
        format!("{response:?}")
            == "Command { path: CommandPath { endpoint: 0, cluster: 49, command: 5 }, \
                fields_tlv: \"<16 bytes>\" }",
        "unexpected InvokeResponse::Command rendering"
    );
    for shown in renderings(&response) {
        assert!(
            shown.contains("\"<16 bytes>\""),
            "the placeholder is missing"
        );
        assert_hidden(&shown, &fields);
    }
    // A batched response's entries print through the same `Debug`.
    let entry = InvokeResponseEntry {
        command_ref: Some(1),
        response: response.clone(),
    };
    for shown in renderings(&entry) {
        assert_hidden(&shown, &fields);
    }
    // The value itself is untouched: only its rendering is hidden.
    let InvokeResponse::Command { fields_tlv, .. } = response else {
        panic!("not a Command");
    };
    // `assert!(a == b)`, not `assert_eq!`, so a failure never prints it.
    assert!(fields_tlv == fields, "the fields were altered");
}

#[test]
fn invoke_response_status_prints_as_derived() {
    assert!(
        format!("{:?}", InvokeResponse::Status(ImStatus::Failure(0x88))) == "Status(Failure(136))",
        "unexpected InvokeResponse::Status rendering"
    );
    assert!(
        format!("{:?}", InvokeResponse::Status(ImStatus::Success)) == "Status(Success)",
        "unexpected InvokeResponse::Status rendering"
    );
}

#[test]
#[allow(
    clippy::manual_assert_eq,
    reason = "assert_eq! would print the payload bytes on failure"
)]
fn invoked_command_debug_shows_only_the_fields_length() {
    let fields = synthetic_bytes(0xC0);
    let command = InvokedCommand {
        path: PATH,
        fields_tlv: fields.to_vec(),
        command_ref: Some(3),
    };

    assert!(
        format!("{command:?}")
            == "InvokedCommand { path: CommandPath { endpoint: 0, cluster: 49, command: 5 }, \
                fields_tlv: \"<16 bytes>\", command_ref: Some(3) }",
        "unexpected InvokedCommand rendering"
    );
    for shown in renderings(&command) {
        assert!(
            shown.contains("\"<16 bytes>\""),
            "the placeholder is missing"
        );
        assert_hidden(&shown, &fields);
    }
    // A parsed request prints its commands through the same `Debug`.
    let request = ParsedInvokeRequest {
        suppress_response: false,
        timed: false,
        commands: vec![command.clone()],
    };
    for shown in renderings(&request) {
        assert_hidden(&shown, &fields);
    }
    assert!(command.fields_tlv == fields, "the fields were altered");
}

#[test]
#[allow(
    clippy::manual_assert_eq,
    reason = "assert_eq! would print the payload bytes on failure"
)]
fn attribute_write_request_debug_shows_only_the_value_length() {
    let value = synthetic_bytes(0xC0);
    let write = AttributeWriteRequest {
        path: AttributePath {
            endpoint: 1,
            cluster: 0x0006,
            attribute: 0x4003,
        },
        value_tlv: value.to_vec(),
    };

    assert!(
        format!("{write:?}")
            == "AttributeWriteRequest { path: AttributePath { endpoint: 1, cluster: 6, \
                attribute: 16387 }, value_tlv: \"<16 bytes>\" }",
        "unexpected AttributeWriteRequest rendering"
    );
    for shown in renderings(&write) {
        assert!(
            shown.contains("\"<16 bytes>\""),
            "the placeholder is missing"
        );
        assert_hidden(&shown, &value);
    }
    // Callers pass slices of these; the slice's `Debug` goes through ours.
    for shown in renderings(&vec![write.clone()]) {
        assert_hidden(&shown, &value);
    }
    assert!(write.value_tlv == value, "the value was altered");
}
