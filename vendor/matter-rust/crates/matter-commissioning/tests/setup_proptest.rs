//! Property-based roundtrip tests for `matter_commissioning::setup`.
//!
//! For every valid `SetupPayload`, the QR and manual-code encoders produce
//! strings that the corresponding decoders return back to the same value.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use matter_commissioning::setup::{
    encode_manual_code, encode_qr, parse_manual_code, parse_qr, CommissioningFlow,
    DiscoveryCapabilities, Discriminator, Passcode, SetupPayload, DISALLOWED_PASSCODES,
    MAX_PASSCODE,
};
use proptest::prelude::*;

fn arb_passcode() -> impl Strategy<Value = Passcode> {
    // Valid passcode range is 1..=MAX_PASSCODE (spec §5.1.7.1); values above the
    // max are 27-bit-representable but not valid passcodes.
    (1u32..=MAX_PASSCODE)
        .prop_filter("disallowed-trivial", |v| !DISALLOWED_PASSCODES.contains(v))
        .prop_map(|v| Passcode::new(v).expect("filtered"))
}

fn arb_discriminator() -> impl Strategy<Value = Discriminator> {
    (0u16..=0x0FFF).prop_map(|v| Discriminator::new(v).expect("range-checked"))
}

fn arb_flow_qr() -> impl Strategy<Value = CommissioningFlow> {
    // Custom is encodable per the spec but matter-rust rejects it on
    // encode (Error::CustomFlowUnsupported). Restrict QR proptests to
    // the two we can roundtrip.
    prop_oneof![
        Just(CommissioningFlow::Standard),
        Just(CommissioningFlow::UserIntent),
    ]
}

fn arb_caps() -> impl Strategy<Value = DiscoveryCapabilities> {
    // Only the three defined bits; reserved bits are tested separately
    // in setup_byte_parity.rs via the captured fixtures.
    (0u8..=0b111).prop_map(DiscoveryCapabilities::from_bits_retain)
}

fn arb_payload_qr() -> impl Strategy<Value = SetupPayload> {
    (
        any::<u16>(),
        any::<u16>(),
        arb_flow_qr(),
        arb_caps(),
        arb_discriminator(),
        arb_passcode(),
    )
        .prop_map(|(vid, pid, flow, caps, disc, pass)| SetupPayload {
            version: 0,
            vendor_id: Some(vid),
            product_id: Some(pid),
            commissioning_flow: flow,
            discovery_capabilities: caps,
            discriminator: disc,
            passcode: pass,
        })
}

fn arb_payload_manual_11() -> impl Strategy<Value = SetupPayload> {
    // A manual code carries only the short discriminator, so a payload that
    // can round-trip through one has a short discriminator to begin with.
    // Building this with a *long* value that merely looks short is what made
    // this suite assert a false identity before provenance was tracked (#120).
    ((0u8..=0x0F), arb_passcode()).prop_map(|(short, passcode)| SetupPayload {
        version: 0,
        vendor_id: None,
        product_id: None,
        commissioning_flow: CommissioningFlow::Standard,
        discovery_capabilities: DiscoveryCapabilities::empty(),
        discriminator: Discriminator::from_short(short).expect("4-bit"),
        passcode,
    })
}

/// Like `arb_payload_manual_11`, but over the *full* 12-bit discriminator
/// range rather than the short-aligned subset. Used for the lossy-roundtrip
/// property below.
fn arb_payload_manual_11_long_disc() -> impl Strategy<Value = SetupPayload> {
    (arb_discriminator(), arb_passcode()).prop_map(|(discriminator, passcode)| SetupPayload {
        version: 0,
        vendor_id: None,
        product_id: None,
        commissioning_flow: CommissioningFlow::Standard,
        discovery_capabilities: DiscoveryCapabilities::empty(),
        discriminator,
        passcode,
    })
}

fn arb_payload_manual_21() -> impl Strategy<Value = SetupPayload> {
    ((0u8..=0x0F), any::<u16>(), any::<u16>(), arb_passcode()).prop_map(
        |(short, vid, pid, passcode)| SetupPayload {
            version: 0,
            vendor_id: Some(vid),
            product_id: Some(pid),
            commissioning_flow: CommissioningFlow::Standard,
            discovery_capabilities: DiscoveryCapabilities::empty(),
            discriminator: Discriminator::from_short(short).expect("4-bit"),
            passcode,
        },
    )
}

proptest! {
    #[test]
    fn qr_roundtrip(payload in arb_payload_qr()) {
        let s = encode_qr(&payload).expect("valid QR payload");
        let back = parse_qr(&s).expect("parse the encoded string");
        prop_assert_eq!(payload, back);
    }

    #[test]
    fn manual_11_roundtrip(payload in arb_payload_manual_11()) {
        let s = encode_manual_code(&payload);
        let back = parse_manual_code(&s).expect("parse the encoded code");
        prop_assert_eq!(payload, back);
    }

    /// The honest manual-code property for an *arbitrary* 12-bit
    /// discriminator: the passcode survives exactly, the discriminator
    /// survives only down to its short form (Matter Core Spec §5.1.4).
    ///
    /// `manual_11_roundtrip` above asserts full equality, which holds only
    /// because its payloads carry a *short* discriminator to begin with. This
    /// states what is true when a long one is fed in. See issue #120.
    #[test]
    fn manual_11_preserves_passcode_and_short_discriminator(
        payload in arb_payload_manual_11_long_disc()
    ) {
        let s = encode_manual_code(&payload);
        let back = parse_manual_code(&s).expect("parse the encoded code");

        prop_assert_eq!(back.passcode, payload.passcode);
        prop_assert_eq!(back.discriminator.short(), payload.discriminator.short());
        // Zero-extended: the low 8 bits are dropped, never invented.
        prop_assert_eq!(back.discriminator.as_u16(), payload.discriminator.as_u16() & 0x0F00);
    }

    #[test]
    fn manual_21_roundtrip(payload in arb_payload_manual_21()) {
        let s = encode_manual_code(&payload);
        let back = parse_manual_code(&s).expect("parse the encoded code");
        prop_assert_eq!(payload, back);
    }
}
