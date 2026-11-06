//! Key material and setup codes stay out of `Debug` output.
//!
//! [`GroupKeySet`](crate::group::GroupKeySet),
//! [`GroupKeySetConfig`](crate::state::GroupKeySetConfig) and
//! [`IcdRegistration`](crate::icd::IcdRegistration) each hold a 16-byte
//! symmetric key (a group epoch key, or the ICD Check-In key). A `{:?}` log of
//! one must not leak it, so their `Debug` prints `<redacted>` for the key and
//! everything else exactly as the old derive did.
//!
//! [`CommissioningWindow`](crate::admin::CommissioningWindow) holds a freshly
//! generated setup passcode and the manual and QR codes that encode it:
//! anyone holding any of the three can commission the device while the window
//! is open. All three print as `<redacted>`; the discriminator, iterations
//! and salt (public, advertised or sent in the clear) still print.
//!
//! [`InvokeResult`](crate::node::InvokeResult)'s `Data` carries the decoded
//! response fields, which for some commands hold a Wi-Fi passphrase or a
//! Thread dataset in a byte string. Its `Debug` keeps the `Value` tree's
//! shape and prints each byte string as `Bytes("<N bytes>")`. Two responses
//! carry a PIN in a character string (`AccountLogin` `GetSetupPINResponse`,
//! `ContentControl` `ResetPINResponse`); in those, every string also prints
//! as `Utf8("<redacted>")`, and every other response keeps its strings.
//!
//! No assertion here prints a `{:?}` rendering, a key or a code on failure: a
//! failing redaction would print the very value it failed to hide. Hence
//! `assert!` with static messages throughout, never `assert_eq!`, and a leak
//! detector that names only *which* rendering leaked. The byte detector is the
//! same as matter-transport's (`debug_redaction_tests` there); each crate
//! keeps its own copy so the crates stay independent.

use matter_codec::{Tag, Value};
use matter_interaction::{CommandPath, ImStatus};

use crate::admin::CommissioningWindow;
use crate::group::GroupKeySet;
use crate::icd::IcdRegistration;
use crate::node::InvokeResult;
use crate::state::GroupKeySetConfig;

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

/// A synthetic 16-byte key whose renderings cannot occur by accident: the
/// bytes are `0xC0..=0xCF`, so the decimal list is `192, 193, …, 207` and the
/// hex list is `c0, c1, …, cf`. Nothing else in these structs prints like that.
fn synthetic_key() -> [u8; 16] {
    synthetic_bytes(0xC0)
}

/// Assert that `shown` (a `{:?}`-family rendering) carries the `"<redacted>"`
/// placeholder and none of `key`'s renderings: its byte list in either the
/// compact or the pretty (`{:#?}`) layout and in any integer format, the
/// bytes as one run of hex digits, or any one byte alone on a line. Never
/// prints `shown` or `key`.
fn assert_redacted(shown: &str, key: &[u8]) {
    assert!(
        shown.contains("\"<redacted>\""),
        "the placeholder is missing"
    );
    assert_hidden(shown, key);
}

/// Check every `Debug` flavour of `value`: plain, alternate (`{:#?}`) and the
/// four hex-integer flag combinations (`{:x?}`, `{:X?}`, `{:#x?}`,
/// `{:#X?}`), which the derive would have pushed down into the key's byte
/// list.
fn assert_all_renderings_redacted<T: core::fmt::Debug>(value: &T, key: &[u8]) {
    for shown in renderings(value) {
        assert_redacted(&shown, key);
    }
}

#[test]
fn group_key_set_debug_redacts_the_epoch_key() {
    let key = synthetic_key();
    let set = GroupKeySet::new(0x0042, key.to_vec(), 1234);

    // Same struct name, field names and field order as the old derive; only
    // the key's value changes.
    assert!(
        format!("{set:?}")
            == "GroupKeySet { key_set_id: 66, epoch_key: \"<redacted>\", \
                epoch_start_time: 1234 }",
        "unexpected GroupKeySet rendering"
    );
    assert_all_renderings_redacted(&set, &key);
    // The value itself is untouched: only its rendering is hidden.
    assert!(set.epoch_key == key, "the epoch key was altered");
}

#[test]
fn group_key_set_config_debug_redacts_the_epoch_key() {
    let key = synthetic_key();
    let config = GroupKeySetConfig::new(0x0042, key, 1234);

    assert!(
        format!("{config:?}")
            == "GroupKeySetConfig { key_set_id: 66, epoch_key: \"<redacted>\", \
                epoch_start_time: 1234 }",
        "unexpected GroupKeySetConfig rendering"
    );
    assert_all_renderings_redacted(&config, &key);
    assert!(config.epoch_key == key, "the epoch key was altered");
}

#[test]
fn icd_registration_debug_redacts_the_check_in_key() {
    let key = synthetic_key();
    let registration = IcdRegistration::new(0x1111, 0x2222, 0x3333, key, 7);

    assert!(
        format!("{registration:?}")
            == "IcdRegistration { node_id: 4369, check_in_node_id: 8738, \
                monitored_subject: 13107, key: \"<redacted>\", start_counter: 7 }",
        "unexpected IcdRegistration rendering"
    );
    assert_all_renderings_redacted(&registration, &key);
    assert!(registration.key == key, "the check-in key was altered");
}

#[test]
fn redaction_reaches_through_containers() {
    // A caller is as likely to log a `Vec` or `Option` of these as a single
    // value; the container's derived `Debug` goes through the redacting one.
    let key = synthetic_key();
    let sets = vec![GroupKeySet::new(1, key.to_vec(), 0)];
    assert_all_renderings_redacted(&sets, &key);
    let configs = Some(GroupKeySetConfig::new(1, key, 0));
    assert_all_renderings_redacted(&configs, &key);
    let registrations = vec![IcdRegistration::new(1, 2, 3, key, 0)];
    assert_all_renderings_redacted(&registrations, &key);
}

// ---- `CommissioningWindow`: the setup codes stay out of `Debug` ----------

/// A valid setup passcode, distinctive in every rendering (`34567891`, hex
/// `20f76d3`), and the codes that would carry it.
const WINDOW_PASSCODE: u32 = 34_567_891;
const WINDOW_MANUAL_CODE: &str = "34970112332";
const WINDOW_QR_CODE: &str = "MT:Y.K9042C00KA0648G00";

fn window(qr_code: Option<String>) -> CommissioningWindow {
    CommissioningWindow {
        passcode: WINDOW_PASSCODE,
        discriminator: 0x0F00,
        iterations: 1000,
        salt: b"SPAKE2P Key Salt".to_vec(),
        manual_code: WINDOW_MANUAL_CODE.to_string(),
        qr_code,
    }
}

/// Panic (with a static message) if `shown` carries the passcode (decimal or
/// either hex case), the manual code, or the QR code (whole, or its payload
/// after `MT:`). Never prints `shown`.
fn assert_window_codes_hidden(shown: &str) {
    for rendering in [
        format!("{WINDOW_PASSCODE}"),
        format!("{WINDOW_PASSCODE:x}"),
        format!("{WINDOW_PASSCODE:X}"),
    ] {
        assert!(!shown.contains(&rendering), "the passcode leaked");
    }
    assert!(
        !shown.contains(WINDOW_MANUAL_CODE),
        "the manual code leaked"
    );
    assert!(
        !shown.contains(WINDOW_QR_CODE.trim_start_matches("MT:")),
        "the QR code leaked"
    );
}

#[test]
fn commissioning_window_debug_redacts_the_setup_codes() {
    let window = window(Some(WINDOW_QR_CODE.to_string()));

    // Same struct name, field names and field order as the old derive; the
    // three codes become placeholders, `qr_code` keeps its `Option` shape,
    // and the public PBKDF parameters still print.
    assert!(
        format!("{window:?}")
            == "CommissioningWindow { passcode: \"<redacted>\", discriminator: 3840, \
                iterations: 1000, salt: [83, 80, 65, 75, 69, 50, 80, 32, 75, 101, 121, \
                32, 83, 97, 108, 116], manual_code: \"<redacted>\", \
                qr_code: Some(\"<redacted>\") }",
        "unexpected CommissioningWindow rendering"
    );
    for shown in renderings(&window) {
        assert!(
            shown.contains("\"<redacted>\""),
            "the placeholder is missing"
        );
        assert_window_codes_hidden(&shown);
    }
    // The values themselves are untouched: callers still read the fields.
    assert!(
        window.passcode == WINDOW_PASSCODE
            && window.manual_code == WINDOW_MANUAL_CODE
            && window.qr_code.as_deref() == Some(WINDOW_QR_CODE),
        "a setup code was altered"
    );
}

#[test]
fn commissioning_window_without_a_qr_code_prints_none() {
    let window = window(None);
    assert!(
        format!("{window:?}").ends_with(", manual_code: \"<redacted>\", qr_code: None }"),
        "a missing QR code should print as None"
    );
    for shown in renderings(&window) {
        assert_window_codes_hidden(&shown);
    }
}

// ---- `InvokeResult`: response byte strings stay out of `Debug` ------------

/// The old derive, kept as a mirror so the tests can check that the
/// hand-written impl prints everything except byte strings exactly as it
/// did. Same variant and field names as `InvokeResult`; the enum's own name
/// never appears in a `Debug` rendering.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "a Debug mirror: its fields are only ever read by the derived Debug"
)]
enum DerivedInvokeResult {
    Data { path: CommandPath, fields: Value },
    Status(ImStatus),
}

fn passphrase_path() -> CommandPath {
    // WiFiNetworkManagement (0x0451) NetworkPassphraseResponse (0x01).
    CommandPath {
        endpoint: 1,
        cluster: 0x0451,
        command: 0x01,
    }
}

/// A response tree with a secret byte string at depth 2 (inside a nested
/// structure), another inside an array inside a list, and every scalar kind
/// beside them, so the walk is checked at each container type.
fn nested_fields(secret: &[u8], other: &[u8]) -> Value {
    Value::Structure(vec![
        (Tag::Context(0), Value::Uint(7)),
        (
            Tag::Context(1),
            Value::Structure(vec![
                (Tag::Context(0), Value::Bytes(secret.to_vec())),
                (Tag::Context(1), Value::Utf8("ssid-label".to_string())),
            ]),
        ),
        (
            Tag::Context(2),
            Value::List(vec![(
                Tag::Anonymous,
                Value::Array(vec![Value::Bytes(other.to_vec()), Value::Null]),
            )]),
        ),
        (Tag::Context(3), Value::Bool(true)),
        (Tag::Context(4), Value::Int(-3)),
        (Tag::Context(5), Value::Double(1.5)),
    ])
}

/// The same shape with each byte string replaced by a scalar, so the tree
/// holds no bytes and must print exactly as the derive printed it.
fn nested_fields_without_bytes() -> Value {
    Value::Structure(vec![
        (Tag::Context(0), Value::Uint(7)),
        (
            Tag::Context(1),
            Value::Structure(vec![
                (Tag::Context(0), Value::Null),
                (Tag::Context(1), Value::Utf8("ssid-label".to_string())),
            ]),
        ),
        (
            Tag::Context(2),
            Value::List(vec![(
                Tag::Anonymous,
                Value::Array(vec![Value::Null, Value::Float(0.25)]),
            )]),
        ),
        (Tag::Context(3), Value::Bool(true)),
        (Tag::Context(4), Value::Int(-3)),
        (Tag::Context(5), Value::Double(1.5)),
    ])
}

#[test]
fn invoke_result_debug_prints_byte_strings_as_their_length() {
    let secret = synthetic_bytes(0xC0);
    let other = synthetic_bytes(0xE0);
    let result = InvokeResult::Data {
        path: passphrase_path(),
        fields: nested_fields(&secret, &other),
    };

    assert!(
        format!("{result:?}")
            == "Data { path: CommandPath { endpoint: 1, cluster: 1105, command: 1 }, \
                fields: Structure([(Context(0), Uint(7)), (Context(1), Structure([\
                (Context(0), Bytes(\"<16 bytes>\")), (Context(1), Utf8(\"ssid-label\"))])), \
                (Context(2), List([(Anonymous, Array([Bytes(\"<16 bytes>\"), Null]))])), \
                (Context(3), Bool(true)), (Context(4), Int(-3)), (Context(5), Double(1.5))]) }",
        "unexpected InvokeResult rendering"
    );
    for shown in renderings(&result) {
        assert!(
            shown.contains("\"<16 bytes>\""),
            "the placeholder is missing"
        );
        assert_hidden(&shown, &secret);
        assert_hidden(&shown, &other);
    }
    // A caller logging an `Option` or `Vec` of results goes through it too.
    let wrapped = Some(vec![result.clone()]);
    for shown in renderings(&wrapped) {
        assert_hidden(&shown, &secret);
        assert_hidden(&shown, &other);
    }
    // The value itself is untouched: only its rendering is hidden.
    assert!(
        result
            == InvokeResult::Data {
                path: passphrase_path(),
                fields: nested_fields(&secret, &other),
            },
        "the response fields were altered"
    );
}

#[test]
fn invoke_result_debug_matches_the_derive_without_byte_strings() {
    // With no byte string in the tree, every flavour must print exactly as
    // the old derive did: same names, tags, nesting and pretty layout.
    let fields = nested_fields_without_bytes();
    let ours = InvokeResult::Data {
        path: passphrase_path(),
        fields: fields.clone(),
    };
    let derived = DerivedInvokeResult::Data {
        path: passphrase_path(),
        fields,
    };
    assert!(
        renderings(&ours) == renderings(&derived),
        "a byte-free Data result no longer prints as the derive did"
    );

    let ours = InvokeResult::Status(ImStatus::Failure(0x87));
    let derived = DerivedInvokeResult::Status(ImStatus::Failure(0x87));
    assert!(
        renderings(&ours) == renderings(&derived),
        "a Status result no longer prints as the derive did"
    );
}

#[test]
fn an_empty_byte_string_still_prints_its_length() {
    let result = InvokeResult::Data {
        path: passphrase_path(),
        fields: Value::Bytes(Vec::new()),
    };
    assert!(
        format!("{result:?}").ends_with("fields: Bytes(\"<0 bytes>\") }"),
        "an empty byte string should print as <0 bytes>"
    );
}

// ---- `InvokeResult`: the two string-PIN responses hide every string -------

/// A PIN that nothing else in these trees prints, in any rendering: no
/// integer, tag, path or placeholder contains these digits.
const STRING_PIN: &str = "73914628";

/// `AccountLogin` (0x050E) `GetSetupPINResponse` (0x01) on tv-app's Content
/// App endpoint.
fn setup_pin_path() -> CommandPath {
    CommandPath {
        endpoint: 3,
        cluster: 0x050E,
        command: 0x01,
    }
}

/// `ContentControl` (0x050F) `ResetPINResponse` (0x02).
fn reset_pin_path() -> CommandPath {
    CommandPath {
        endpoint: 1,
        cluster: 0x050F,
        command: 0x02,
    }
}

/// The response as it arrives: one string field, the PIN, at context tag 0.
fn pin_response(pin: &str) -> Value {
    Value::Structure(vec![(Tag::Context(0), Value::Utf8(pin.to_string()))])
}

/// The PIN at every container depth (top level, a nested structure, an array
/// inside a list) beside a byte string and a scalar, so the walk is checked
/// at each container type in string-redacting mode.
fn nested_pin_fields(pin: &str, secret: &[u8]) -> Value {
    Value::Structure(vec![
        (Tag::Context(0), Value::Utf8(pin.to_string())),
        (
            Tag::Context(1),
            Value::Structure(vec![
                (Tag::Context(0), Value::Utf8(pin.to_string())),
                (Tag::Context(1), Value::Uint(7)),
            ]),
        ),
        (
            Tag::Context(2),
            Value::List(vec![(
                Tag::Anonymous,
                Value::Array(vec![
                    Value::Utf8(pin.to_string()),
                    Value::Bytes(secret.to_vec()),
                ]),
            )]),
        ),
    ])
}

#[test]
#[allow(
    clippy::manual_assert_eq,
    reason = "assert_eq! would print the PIN on failure"
)]
fn string_pin_responses_print_every_string_redacted() {
    let secret = synthetic_bytes(0xC0);
    for (path, expected) in [
        (
            setup_pin_path(),
            "Data { path: CommandPath { endpoint: 3, cluster: 1294, command: 1 }, \
             fields: Structure([(Context(0), Utf8(\"<redacted>\"))]) }",
        ),
        (
            reset_pin_path(),
            "Data { path: CommandPath { endpoint: 1, cluster: 1295, command: 2 }, \
             fields: Structure([(Context(0), Utf8(\"<redacted>\"))]) }",
        ),
    ] {
        // The real shape: the tree prints intact, the PIN as a placeholder.
        let result = InvokeResult::Data {
            path,
            fields: pin_response(STRING_PIN),
        };
        assert!(
            format!("{result:?}") == expected,
            "unexpected PIN response rendering"
        );
        for shown in renderings(&result) {
            assert!(
                shown.contains("\"<redacted>\""),
                "the placeholder is missing"
            );
            assert!(!shown.contains(STRING_PIN), "the PIN leaked");
        }

        // Every string at every depth is hidden; byte strings keep their
        // length placeholder and the scalars still print.
        let result = InvokeResult::Data {
            path,
            fields: nested_pin_fields(STRING_PIN, &secret),
        };
        assert!(
            format!("{result:?}").ends_with(
                "fields: Structure([(Context(0), Utf8(\"<redacted>\")), \
                 (Context(1), Structure([(Context(0), Utf8(\"<redacted>\")), \
                 (Context(1), Uint(7))])), (Context(2), List([(Anonymous, \
                 Array([Utf8(\"<redacted>\"), Bytes(\"<16 bytes>\")]))]))]) }"
            ),
            "unexpected nested PIN response rendering"
        );
        for shown in renderings(&result) {
            assert!(!shown.contains(STRING_PIN), "a nested PIN leaked");
            assert!(
                shown.contains("\"<16 bytes>\""),
                "the byte-string placeholder is missing"
            );
            assert_hidden(&shown, &secret);
        }
        // A caller logging an `Option` or `Vec` of results goes through it.
        for shown in renderings(&Some(vec![result.clone()])) {
            assert!(!shown.contains(STRING_PIN), "a wrapped PIN leaked");
        }
        // The value itself is untouched: only its rendering is hidden.
        assert!(
            result
                == InvokeResult::Data {
                    path,
                    fields: nested_pin_fields(STRING_PIN, &secret),
                },
            "the response fields were altered"
        );
    }
}

#[test]
fn other_responses_still_print_their_strings() {
    // Only the exact (cluster, response command) pairs hide strings. Each
    // near miss (the other table entry's command, a neighbouring cluster
    // with the same command, the request the response answers) prints
    // exactly as the old derive did, string included.
    for (cluster, command) in [
        (0x050E, 0x02), // AccountLogin, ContentControl's response id (Login)
        (0x050F, 0x01), // ContentControl, AccountLogin's response id (ResetPIN)
        (0x050E, 0x00), // AccountLogin GetSetupPIN, the request
        (0x050D, 0x01), // the cluster before AccountLogin
        (0x0510, 0x02), // the cluster after ContentControl
        (0x0451, 0x01), // WiFiNetworkManagement NetworkPassphraseResponse
    ] {
        let path = CommandPath {
            endpoint: 1,
            cluster,
            command,
        };
        let fields = pin_response("ssid-label");
        let ours = InvokeResult::Data {
            path,
            fields: fields.clone(),
        };
        let derived = DerivedInvokeResult::Data { path, fields };
        assert!(
            renderings(&ours) == renderings(&derived),
            "a response outside the table no longer prints its strings"
        );
    }
}
