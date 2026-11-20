//! `GeneralCommissioning` cluster (id `0x0030`) command + response
//! codecs.
//!
//! Spec §11.10. M6.4 uses `ArmFailSafe`, `SetRegulatoryConfig`, and
//! `CommissioningComplete` plus their responses.

#![forbid(unsafe_code)]

use crate::clusters::network_commissioning::truncate_utf8;
use crate::state_machine::{CommissioningError, Stage};

/// Cluster ID: `0x0030`.
pub const CLUSTER_ID: u32 = 0x0030;

/// Command IDs (Matter Core Spec §11.10.6).
pub mod command_id {
    /// `ArmFailSafe` request.
    pub const ARM_FAIL_SAFE: u32 = 0x00;
    /// `SetRegulatoryConfig` request.
    pub const SET_REGULATORY_CONFIG: u32 = 0x02;
    /// `CommissioningComplete` request.
    pub const COMMISSIONING_COMPLETE: u32 = 0x04;
}

/// Decoded `ArmFailSafeResponse` (spec §11.10.6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmFailSafeResponse {
    /// `CommissioningErrorEnum` (spec §11.10.5.1). 0 = OK.
    pub error_code: u8,
    /// Optional human-readable debug text (≤128 chars), capped at the
    /// spec's 512-octet bound at decode. **Device-controlled free text** —
    /// log deliberately.
    pub debug_text: Option<String>,
}

/// Decoded `SetRegulatoryConfigResponse` (spec §11.10.6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetRegulatoryConfigResponse {
    /// `CommissioningErrorEnum`. 0 = OK.
    pub error_code: u8,
    /// Optional debug text, capped at the spec's 512-octet bound at
    /// decode. **Device-controlled free text** — log deliberately.
    pub debug_text: Option<String>,
}

/// `RegulatoryLocationTypeEnum` (spec §11.10.5.2).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RegulatoryLocation {
    /// Indoor only.
    Indoor = 0,
    /// Outdoor only.
    Outdoor = 1,
    /// Indoor + outdoor.
    IndoorOutdoor = 2,
}

/// Encode `ArmFailSafe` (spec §11.10.6.1).
///
/// `expiry_length_seconds == 0` disarms the failsafe.
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_arm_fail_safe(expiry_length_seconds: u16, breadcrumb: u64) -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(0), u64::from(expiry_length_seconds))
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(1), breadcrumb)
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Encode `SetRegulatoryConfig` (spec §11.10.6.3).
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_set_regulatory_config(
    new_regulatory_config: RegulatoryLocation,
    country_code: &str,
    breadcrumb: u64,
) -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(0), u64::from(new_regulatory_config as u8))
        .expect("infallible: vec writer");
    w.put_utf8(Tag::Context(1), country_code)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(2), breadcrumb)
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Encode `CommissioningComplete` (spec §11.10.6.5).
///
/// `CommissioningComplete` carries no payload fields — just an empty
/// anonymous structure. Sent over the CASE session at
/// [`crate::state_machine::Stage::SendComplete`].
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_commissioning_complete() -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Decode the shared `(error_code, debug_text)` shape used by
/// `ArmFailSafeResponse`, `SetRegulatoryConfigResponse`, and
/// `CommissioningCompleteResponse`.
///
/// `stage` is plumbed through so any error includes the right cursor
/// position in the `CommissioningError::MalformedResponse(...)` variant.
#[allow(clippy::match_same_arms)] // truncation vs malformed-shape are conceptually distinct, both surface as MalformedResponse.
pub(crate) fn decode_commissioning_error_response(
    stage: Stage,
    tlv: &[u8],
) -> Result<(u8, Option<String>), CommissioningError> {
    use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};
    let mut reader = TlvReader::new(tlv);
    match reader
        .next()
        .map_err(|_| CommissioningError::MalformedResponse(stage))?
    {
        Some(Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        }) => {}
        _ => return Err(CommissioningError::MalformedResponse(stage)),
    }
    let mut error_code: Option<u8> = None;
    let mut debug_text: Option<String> = None;
    loop {
        match reader
            .next()
            .map_err(|_| CommissioningError::MalformedResponse(stage))?
        {
            None => return Err(CommissioningError::MalformedResponse(stage)),
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(v),
            }) => {
                if error_code.is_some() {
                    return Err(CommissioningError::MalformedResponse(stage));
                }
                let n =
                    u8::try_from(v).map_err(|_| CommissioningError::MalformedResponse(stage))?;
                error_code = Some(n);
            }
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Utf8(s),
            }) => {
                if debug_text.is_some() {
                    return Err(CommissioningError::MalformedResponse(stage));
                }
                // Spec bound (§11.10): DebugText ≤ 512 octets. Device-echoed free text — cap defensively.
                debug_text = Some(truncate_utf8(s, 512));
            }
            // Forward-compat: skip an unknown container whole. Ignoring only its
            // start would read its members as top-level fields and end the
            // struct at its end-of-container.
            Some(Element::ContainerStart { .. }) => reader
                .skip_container()
                .map_err(|_| CommissioningError::MalformedResponse(stage))?,
            // Forward-compat: ignore future scalar tags.
            Some(Element::Scalar { .. }) => {}
            Some(_) => return Err(CommissioningError::MalformedResponse(stage)),
        }
    }
    // Nothing may follow the struct's end-of-container.
    reader
        .finish()
        .map_err(|_| CommissioningError::MalformedResponse(stage))?;
    let error_code = error_code.ok_or(CommissioningError::MalformedResponse(stage))?;
    Ok((error_code, debug_text))
}

/// Decode `ArmFailSafeResponse` (spec §11.10.6.2).
///
/// # Errors
///
/// Returns `CommissioningError::MalformedResponse(Stage::ArmFailsafe)`
/// on malformed input.
pub fn decode_arm_fail_safe_response(
    tlv: &[u8],
) -> Result<ArmFailSafeResponse, CommissioningError> {
    let (error_code, debug_text) = decode_commissioning_error_response(Stage::ArmFailsafe, tlv)?;
    Ok(ArmFailSafeResponse {
        error_code,
        debug_text,
    })
}

/// Decode `SetRegulatoryConfigResponse` (spec §11.10.6.4).
///
/// # Errors
///
/// Returns `CommissioningError::MalformedResponse(Stage::ConfigRegulatory)`
/// on malformed input.
pub fn decode_set_regulatory_config_response(
    tlv: &[u8],
) -> Result<SetRegulatoryConfigResponse, CommissioningError> {
    let (error_code, debug_text) =
        decode_commissioning_error_response(Stage::ConfigRegulatory, tlv)?;
    Ok(SetRegulatoryConfigResponse {
        error_code,
        debug_text,
    })
}

/// Decoded `BasicCommissioningInfo` struct (spec §11.10.5.5).
///
/// The commissioner reads `failsafe_expiry_length_seconds` as the requested
/// `ArmFailSafe` expiry and caps it against `max_cumulative_failsafe_seconds`
/// so it never sends a value the device is guaranteed to reject with
/// `BoundsExceeded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasicCommissioningInfo {
    /// Maximum failsafe expiry the device will honour, in seconds.
    /// Context tag 0 inside the `BasicCommissioningInfo` struct.
    pub failsafe_expiry_length_seconds: u16,
    /// Spec §11.10.5.5 — context tag 1. Optional in the spec; if
    /// absent in the wire payload this field is `0`.
    pub max_cumulative_failsafe_seconds: u16,
}

/// Best-effort decode of a `BasicCommissioningInfo` struct from its
/// TLV bytes.
///
/// Returns `None` on any parse failure — the state machine treats a
/// missing value as "use the M6.4 fallback of 60 seconds." This keeps
/// the state machine moving even against malformed devices.
#[must_use]
pub fn decode_basic_commissioning_info(tlv: &[u8]) -> Option<BasicCommissioningInfo> {
    use matter_codec::{ContainerKind, Element, Tag, TlvReader, Value};
    let mut reader = TlvReader::new(tlv);
    match reader.next().ok().flatten()? {
        Element::ContainerStart {
            tag: Tag::Anonymous,
            kind: ContainerKind::Structure,
        } => {}
        _ => return None,
    }
    let mut failsafe: Option<u16> = None;
    let mut max_cumulative: u16 = 0;
    loop {
        match reader.next().ok().flatten() {
            None => return None,
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(v),
            }) => {
                failsafe = u16::try_from(v).ok();
            }
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Uint(v),
            }) => {
                if let Ok(n) = u16::try_from(v) {
                    max_cumulative = n;
                }
            }
            // Forward-compat: skip an unknown container whole (see
            // `decode_commissioning_error_response`).
            Some(Element::ContainerStart { .. }) => reader.skip_container().ok()?,
            // Forward-compat: ignore unrecognised scalar tags.
            Some(_) => {}
        }
    }
    // Nothing may follow the struct's end-of-container.
    reader.finish().ok()?;
    failsafe.map(|fs| BasicCommissioningInfo {
        failsafe_expiry_length_seconds: fs,
        max_cumulative_failsafe_seconds: max_cumulative,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::items_after_statements
)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    #[test]
    fn arm_fail_safe_60_0_matches_spec_bytes() {
        let bytes = encode_arm_fail_safe(60, 0);
        assert_eq!(
            bytes,
            vec![0x15, 0x24, 0x00, 0x3C, 0x24, 0x01, 0x00, 0x18],
            "encoded bytes: {bytes:02x?}"
        );
    }

    #[test]
    fn commissioning_complete_is_empty_anonymous_struct() {
        let bytes = encode_commissioning_complete();
        assert_eq!(bytes, vec![0x15, 0x18]);
    }

    #[test]
    fn arm_fail_safe_response_ok_round_trips() {
        // Encode an ok response by hand: { 0: 0_u8 } — debug_text omitted.
        let tlv = vec![0x15, 0x24, 0x00, 0x00, 0x18];
        let decoded = decode_arm_fail_safe_response(&tlv).expect("happy path decodes");
        assert_eq!(decoded.error_code, 0);
        assert_eq!(decoded.debug_text, None);
    }

    #[test]
    fn arm_fail_safe_response_with_debug_text_round_trips() {
        // { 0: 1_u8, 1: "busy" }
        let tlv = vec![
            0x15, 0x24, 0x00, 0x01, // error_code = 1 (busy)
            0x2C, 0x01, 0x04, b'b', b'u', b's', b'y', // debug_text = "busy"
            0x18, // end
        ];
        let decoded = decode_arm_fail_safe_response(&tlv).expect("happy path decodes");
        assert_eq!(decoded.error_code, 1);
        assert_eq!(decoded.debug_text.as_deref(), Some("busy"));
    }

    #[test]
    fn malformed_response_returns_error() {
        // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
        let err = decode_arm_fail_safe_response(&[0xFF]).expect_err("should fail");
        assert!(
            matches!(
                err,
                crate::state_machine::CommissioningError::MalformedResponse(
                    crate::state_machine::Stage::ArmFailsafe
                )
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn basic_commissioning_info_120_decodes() {
        // { 0: 120_u16, 1: 900_u16 }
        let tlv = vec![
            0x15, 0x25, 0x00, 0x78, 0x00, // u16 = 120
            0x25, 0x01, 0x84, 0x03, // u16 = 900
            0x18,
        ];
        let info = decode_basic_commissioning_info(&tlv).expect("decodes");
        assert_eq!(info.failsafe_expiry_length_seconds, 120);
        assert_eq!(info.max_cumulative_failsafe_seconds, 900);
    }

    #[test]
    fn basic_commissioning_info_malformed_returns_none() {
        assert!(decode_basic_commissioning_info(&[0xFF]).is_none());
        assert!(decode_basic_commissioning_info(&[]).is_none());
    }

    #[test]
    fn arm_fail_safe_response_caps_debug_text_at_512_bytes() {
        use matter_codec::{Tag, TlvWriter};
        // ArmFailSafeResponse TLV: anonymous struct { [0]=1u, [1]=600x'x' }.
        let long_text = "x".repeat(600);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.put_utf8(Tag::Context(1), &long_text).unwrap();
        w.end_container().unwrap();

        let decoded = decode_arm_fail_safe_response(&buf).expect("happy path decodes");
        assert_eq!(decoded.error_code, 1);
        assert_eq!(
            decoded.debug_text.unwrap().len(),
            512,
            "capped at spec bound"
        );
    }
}
