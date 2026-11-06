//! Setup passcodes and command payloads stay out of `Debug` output.
//!
//! - [`Passcode`] is the device's setup PIN: anyone holding it can
//!   commission the device while it is commissionable. Its `Debug` prints
//!   `Passcode("<redacted>")`, so [`SetupPayload`]'s derived `Debug` hides it
//!   too.
//! - [`Action::Invoke`]'s `payload` is the plaintext of an outbound command:
//!   `AddNOC` carries the IPK, `AddOrUpdateWiFiNetwork` the Wi-Fi passphrase,
//!   `AddOrUpdateThreadNetwork` the Thread dataset. Every `Invoke` prints its
//!   payload as its length only (`"<N bytes>"`), whatever the command.
//! - [`SecuredResponse`] (driver) carries a decrypted response payload, and
//!   prints it the same way.
//!
//! Every other field prints exactly as the old derive did.
//!
//! No assertion here prints a rendering, a payload or a passcode on failure:
//! a failing redaction would print the very value it failed to hide. Hence
//! `assert!` with static messages throughout, and leak detectors that name
//! only *which* rendering leaked. The byte detector is the same as
//! matter-transport's (`debug_redaction_tests` there); each crate keeps its
//! own copy so the crates stay independent.

// Test code: unwrap is allowed in tests (CONTRIBUTING.md). Every `unwrap`
// here is on a constructor fed known-valid input (the spec's test passcode, a
// fresh fabric).
#![allow(clippy::unwrap_used)]

use crate::setup::{
    CommissioningFlow, DiscoveryCapabilities, Discriminator, Passcode, SetupPayload,
};
use crate::state_machine::{Action, Expectation, SessionContext};

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

/// Panic (with a static message) if `shown` carries `passcode` in decimal or
/// in either hex case (the `{:x?}`/`{:X?}` flags reach a derived `u32`).
fn assert_passcode_hidden(shown: &str, passcode: u32) {
    for rendering in [
        format!("{passcode}"),
        format!("{passcode:x}"),
        format!("{passcode:X}"),
    ] {
        assert!(!shown.contains(&rendering), "the passcode leaked");
    }
}

/// The spec's standard test passcode: valid, and distinctive in every
/// rendering (`20202021`, `0x1344425`).
const TEST_PASSCODE: u32 = 20_202_021;

#[test]
fn passcode_debug_is_redacted() {
    let passcode = Passcode::new(TEST_PASSCODE).unwrap();

    // Same tuple-struct shape as the old derive; only the value changes.
    assert!(
        format!("{passcode:?}") == "Passcode(\"<redacted>\")",
        "unexpected Passcode rendering"
    );
    for shown in renderings(&passcode) {
        assert!(
            shown.contains("\"<redacted>\""),
            "the placeholder is missing"
        );
        assert_passcode_hidden(&shown, TEST_PASSCODE);
    }
    // The value itself is untouched: only its rendering is hidden.
    assert!(
        passcode.as_u32() == TEST_PASSCODE,
        "the passcode was altered"
    );
}

#[test]
fn setup_payload_debug_redacts_its_passcode() {
    // `SetupPayload` still derives `Debug`; it prints its passcode through
    // `Passcode`'s redacting impl, so a `{:?}` of a parsed QR code is safe.
    let payload = SetupPayload {
        version: 0,
        vendor_id: Some(0xFFF1),
        product_id: Some(0x8000),
        commissioning_flow: CommissioningFlow::Standard,
        discovery_capabilities: DiscoveryCapabilities::ON_NETWORK,
        discriminator: Discriminator::new(0x0F00).unwrap(),
        passcode: Passcode::new(TEST_PASSCODE).unwrap(),
    };

    assert!(
        format!("{payload:?}")
            == "SetupPayload { version: 0, vendor_id: Some(65521), product_id: Some(32768), \
                commissioning_flow: Standard, discovery_capabilities: \
                DiscoveryCapabilities(ON_NETWORK), discriminator: Discriminator { value: 3840, \
                is_short: false }, passcode: Passcode(\"<redacted>\") }",
        "unexpected SetupPayload rendering"
    );
    for shown in renderings(&payload) {
        assert!(
            shown.contains("\"<redacted>\""),
            "the placeholder is missing"
        );
        assert_passcode_hidden(&shown, TEST_PASSCODE);
    }
}

#[test]
#[allow(
    clippy::manual_assert_eq,
    reason = "assert_eq! would print the payload bytes on failure"
)]
fn invoke_action_debug_shows_only_the_payload_length() {
    let payload = synthetic_bytes(0xC0);
    let action = Action::Invoke {
        session: SessionContext::Pase,
        endpoint: 0,
        cluster: 0x003E,
        command: 0x06,
        payload: payload.to_vec(),
        expect: Expectation::NocResponse,
    };

    // Same variant name, field names and field order as the old derive; only
    // the payload's value changes.
    assert!(
        format!("{action:?}")
            == "Invoke { session: Pase, endpoint: 0, cluster: 62, command: 6, \
                payload: \"<16 bytes>\", expect: NocResponse }",
        "unexpected Action::Invoke rendering"
    );
    for shown in renderings(&action) {
        assert!(
            shown.contains("\"<16 bytes>\""),
            "the placeholder is missing"
        );
        assert_hidden(&shown, &payload);
    }
    // The value itself is untouched: only its rendering is hidden.
    let Action::Invoke { payload: kept, .. } = action else {
        panic!("not an Invoke");
    };
    // `assert!(a == b)`, not `assert_eq!`, so a failure never prints it.
    assert!(kept == payload, "the payload was altered");
}

#[test]
fn every_invoke_is_redacted_whatever_the_command() {
    // No allow-list: a harmless-looking command still prints only a length,
    // so a future command that carries a secret cannot be forgotten.
    let payload = synthetic_bytes(0xD0);
    for (cluster, command) in [(0x0030, 0x00), (0x0031, 0x02), (0x003E, 0x0B)] {
        for session in [SessionContext::Pase, SessionContext::Case] {
            let action = Action::Invoke {
                session,
                endpoint: 0,
                cluster,
                command,
                payload: payload.to_vec(),
                expect: Expectation::ArmFailsafeResponse,
            };
            for shown in renderings(&action) {
                assert!(
                    shown.contains("\"<16 bytes>\""),
                    "the placeholder is missing"
                );
                assert_hidden(&shown, &payload);
            }
        }
    }
}

#[test]
fn other_actions_print_as_derived() {
    let read = Action::ReadAttribute {
        session: SessionContext::Case,
        endpoint: 0,
        cluster: 0x0030,
        attributes: &[0, 1],
        expect: Expectation::CommissioningInfo,
    };
    assert!(
        format!("{read:?}")
            == "ReadAttribute { session: Case, endpoint: 0, cluster: 48, attributes: [0, 1], \
                expect: CommissioningInfo }",
        "unexpected Action::ReadAttribute rendering"
    );
    let evict = Action::EvictCase {
        fabric_id: 1,
        peer_node_id: 2,
    };
    assert!(
        format!("{evict:?}") == "EvictCase { fabric_id: 1, peer_node_id: 2 }",
        "unexpected Action::EvictCase rendering"
    );
    let establish = Action::EstablishCase {
        fabric_id: 1,
        peer_node_id: 2,
    };
    assert!(
        format!("{establish:?}") == "EstablishCase { fabric_id: 1, peer_node_id: 2 }",
        "unexpected Action::EstablishCase rendering"
    );
    let abort = Action::Abort {
        send_disarm_failsafe: true,
        reason: "synthetic failure".to_string(),
    };
    assert!(
        format!("{abort:?}")
            == "Abort { send_disarm_failsafe: true, reason: \"synthetic failure\" }",
        "unexpected Action::Abort rendering"
    );
}

#[test]
fn done_action_prints_its_fabric_as_derived() {
    use std::sync::Arc;

    use matter_cert::time::MatterTime;
    use matter_crypto::{RingSigner, Signer};

    use crate::noc::{FabricRecord, SystemNocRng};
    use crate::state_machine::{CommissionedFabric, Stage};

    let (signer, _pkcs8) = RingSigner::generate().unwrap();
    let signer: Arc<dyn Signer> = Arc::new(signer);
    let fabric = FabricRecord::new_root_only(
        1,
        signer,
        MatterTime::from_unix_secs(1_704_067_200),
        MatterTime::from_unix_secs(1_735_689_600),
        42,
        &SystemNocRng,
    )
    .unwrap();
    let done = CommissionedFabric {
        fabric,
        peer_node_id: 2,
        peer_root_public_key: [0x04; 65],
        terminated_at: Stage::Cleanup,
    };
    // A tuple variant, printed through `CommissionedFabric`'s own `Debug`
    // (whose `FabricRecord` already withholds the IPK and signer).
    let expected = format!("Done({done:?})");
    let expected_pretty = format!("{:#?}", (&done,)).replacen('(', "Done(", 1);
    let action = Action::Done(done);
    assert!(
        format!("{action:?}") == expected,
        "unexpected Action::Done rendering"
    );
    assert!(
        format!("{action:#?}") == expected_pretty,
        "unexpected Action::Done pretty rendering"
    );
}

#[cfg(feature = "driver")]
#[test]
#[allow(
    clippy::manual_assert_eq,
    reason = "assert_eq! would print the payload bytes on failure"
)]
fn secured_response_debug_shows_only_the_payload_length() {
    use crate::driver::SecuredResponse;

    let payload = synthetic_bytes(0xC0);
    let response = SecuredResponse {
        exchange_id: 0x1234,
        payload: payload.to_vec(),
    };

    assert!(
        format!("{response:?}") == "SecuredResponse { exchange_id: 4660, payload: \"<16 bytes>\" }",
        "unexpected SecuredResponse rendering"
    );
    for shown in renderings(&response) {
        assert!(
            shown.contains("\"<16 bytes>\""),
            "the placeholder is missing"
        );
        assert_hidden(&shown, &payload);
    }
    assert!(response.payload == payload, "the payload was altered");
}
