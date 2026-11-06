//! `NetworkCommissioning` cluster (id `0x0031`) command + response
//! codecs.
//!
//! Spec §11.9. Only what commissioning sends and reads:
//! `AddOrUpdateWiFiNetwork`, `AddOrUpdateThreadNetwork` (M9-C2),
//! `ConnectNetwork`, plus the `FeatureMap` and `ConnectMaxTimeSeconds`
//! attribute reads for Wi-Fi/Ethernet/Thread branching. `ScanNetworks` and
//! `RemoveNetwork`/`ReorderNetworks` are not implemented: commissioning
//! never sends them.

#![forbid(unsafe_code)]

use crate::state_machine::{CommissioningError, RemediationHint, Stage};

/// Cluster ID: `0x0031`.
pub const CLUSTER_ID: u32 = 0x0031;

/// Command IDs (Matter Core Spec §11.9.6).
pub mod command_id {
    /// `AddOrUpdateWiFiNetwork` request.
    pub const ADD_OR_UPDATE_WIFI_NETWORK: u32 = 0x02;
    /// `AddOrUpdateThreadNetwork` request.
    pub const ADD_OR_UPDATE_THREAD_NETWORK: u32 = 0x03;
    /// `ConnectNetwork` request.
    pub const CONNECT_NETWORK: u32 = 0x06;
}

/// Response IDs (Matter Core Spec §11.9.6).
pub mod response_id {
    /// `NetworkConfigResponse` — emitted for
    /// `AddOrUpdateWiFiNetwork` / `RemoveNetwork` / `ReorderNetworks`.
    pub const NETWORK_CONFIG_RESPONSE: u32 = 0x05;
    /// `ConnectNetworkResponse`.
    pub const CONNECT_NETWORK_RESPONSE: u32 = 0x07;
}

/// Attribute IDs.
pub mod attribute_id {
    /// Universal Matter cluster meta-attribute. Spec §7.13.
    pub const FEATURE_MAP: u32 = 0xFFFC;
    /// `ConnectMaxTimeSeconds` (spec §11.9.5.4; chip
    /// `network-commissioning-cluster.xml` `code="0x0003"`). Type
    /// `int8u` — maximum time, in seconds (0-255), the device may take
    /// to connect to an operational network after `ConnectNetwork`.
    /// Used to size the failsafe extension before `ConnectNetwork`
    /// (Thread attach is slower than Wi-Fi association).
    pub const CONNECT_MAX_TIME_SECONDS: u32 = 0x0003;
}

bitflags::bitflags! {
    /// Bits from `NetworkCommissioning::FeatureMap` (spec §11.9.4).
    ///
    /// Bit 0 = `WiFiNetworkInterface`, bit 1 = `ThreadNetworkInterface`,
    /// bit 2 = `EthernetNetworkInterface`. Higher bits reserved.
    #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
    pub struct NetworkCommissioningFeature: u32 {
        /// Device exposes a Wi-Fi network interface.
        const WIFI     = 1 << 0;
        /// Device exposes a Thread network interface.
        const THREAD   = 1 << 1;
        /// Device exposes an Ethernet network interface.
        const ETHERNET = 1 << 2;
    }
}

/// Encode `AddOrUpdateWiFiNetwork` (spec §11.9.6.3).
///
/// `ssid` must be 1–32 bytes; `credentials` 0–64 bytes. The encoder
/// does NOT validate lengths — callers (state machine `Commissioner`)
/// validate at config-load time.
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_add_or_update_wifi_network(
    ssid: &[u8],
    credentials: &[u8],
    breadcrumb: u64,
) -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bytes(Tag::Context(0), ssid)
        .expect("infallible: vec writer");
    w.put_bytes(Tag::Context(1), credentials)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(2), breadcrumb)
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Encode `AddOrUpdateThreadNetwork` (spec §11.9.6.4).
///
/// `operational_dataset` is the opaque Thread Operational Dataset TLV, in
/// Thread's own TLV format (type-length-value, distinct from Matter TLV) —
/// captured verbatim from the Thread Border Router (e.g. `ot-ctl dataset
/// active -x`). The encoder does not parse or validate the dataset; it is
/// carried as an octet string. Unlike `AddOrUpdateWiFiNetwork`, there is no
/// credentials field, so `breadcrumb` lands at `Tag::Context(1)` here
/// rather than `Tag::Context(2)`.
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_add_or_update_thread_network(operational_dataset: &[u8], breadcrumb: u64) -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bytes(Tag::Context(0), operational_dataset)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(1), breadcrumb)
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Encode `ConnectNetwork` (spec §11.9.6.6).
///
/// `network_id` is the SSID bytes for Wi-Fi networks (re-uses the
/// `ssid` field as the network identity per spec §11.9.5.2). For
/// Wi-Fi commissioning the value is identical to the SSID supplied
/// to `encode_add_or_update_wifi_network`.
#[must_use]
#[allow(clippy::expect_used, clippy::missing_panics_doc)] // Vec-backed TlvWriter is infallible.
pub fn encode_connect_network(network_id: &[u8], breadcrumb: u64) -> Vec<u8> {
    use matter_codec::{Tag, TlvWriter};
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.start_structure(Tag::Anonymous)
        .expect("infallible: vec writer");
    w.put_bytes(Tag::Context(0), network_id)
        .expect("infallible: vec writer");
    w.put_uint(Tag::Context(1), breadcrumb)
        .expect("infallible: vec writer");
    w.end_container().expect("infallible: vec writer");
    buf
}

/// Decode the `NetworkCommissioning::FeatureMap` attribute value.
///
/// Expects the bare TLV encoding of the u32 attribute value (no
/// `AttributeReportIB` envelope). The state machine's M6.6 driver
/// unwraps the Interaction Model envelope and delivers just the value
/// TLV to `Commissioner::on_response`.
///
/// # Errors
///
/// Returns `CommissioningError::MalformedResponse(Stage::ReadNetworkCommissioningInfo)`
/// if the bytes are not a well-formed unsigned-integer TLV element.
pub fn decode_feature_map(tlv: &[u8]) -> Result<NetworkCommissioningFeature, CommissioningError> {
    use matter_codec::{Element, TlvReader, Value};
    let mut reader = TlvReader::new(tlv);
    match reader
        .next()
        .map_err(|_| CommissioningError::MalformedResponse(Stage::ReadNetworkCommissioningInfo))?
    {
        Some(Element::Scalar {
            value: Value::Uint(raw),
            ..
        }) => {
            let truncated = u32::try_from(raw).map_err(|_| {
                CommissioningError::MalformedResponse(Stage::ReadNetworkCommissioningInfo)
            })?;
            Ok(NetworkCommissioningFeature::from_bits_truncate(truncated))
        }
        _ => Err(CommissioningError::MalformedResponse(
            Stage::ReadNetworkCommissioningInfo,
        )),
    }
}

/// Decode the `NetworkCommissioning::ConnectMaxTimeSeconds` attribute
/// value (spec §11.9.5.4).
///
/// Expects the bare TLV encoding of the unsigned attribute value (no
/// `AttributeReportIB` envelope) — the same single-attribute shape
/// [`decode_feature_map`] parses. The state machine's driver unwraps the
/// Interaction Model report and delivers just the value TLV.
///
/// The attribute is a `u8` (`int8u`) seconds count on the wire; the
/// value is widened into a `u16` return type for decoder headroom, and
/// any value that does not fit `u16` (which a spec-conformant `u8`
/// device will never send) is clamped to [`u16::MAX`] rather than
/// rejected, since the value only sizes a local timeout (over-large is
/// harmless, and a hostile device cannot use it to reject a
/// well-formed read).
///
/// # Errors
///
/// Returns `CommissioningError::MalformedResponse(Stage::ReadNetworkCommissioningInfo)`
/// if the bytes are not a well-formed unsigned-integer TLV element.
pub fn decode_connect_max_time_seconds(tlv: &[u8]) -> Result<u16, CommissioningError> {
    use matter_codec::{Element, TlvReader, Value};
    let mut reader = TlvReader::new(tlv);
    match reader
        .next()
        .map_err(|_| CommissioningError::MalformedResponse(Stage::ReadNetworkCommissioningInfo))?
    {
        Some(Element::Scalar {
            value: Value::Uint(raw),
            ..
        }) => Ok(u16::try_from(raw).unwrap_or(u16::MAX)),
        _ => Err(CommissioningError::MalformedResponse(
            Stage::ReadNetworkCommissioningInfo,
        )),
    }
}

/// Cap `s` at `max_bytes` bytes, flooring to a UTF-8 char boundary (Matter
/// string bounds are octet counts; `String::truncate` panics mid-char, and
/// `str::floor_char_boundary` is unstable at MSRV 1.88).
pub(crate) fn truncate_utf8(mut s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s
}

/// Decoded `NetworkConfigResponse` (spec §11.9.6.5). Emitted by
/// `AddOrUpdateWiFiNetwork`, `RemoveNetwork`, `ReorderNetworks`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NetworkConfigResponse {
    /// `NetworkCommissioningStatusEnum` (spec §11.9.5.1). 0 = OK.
    pub networking_status: u8,
    /// Optional human-readable debug text echoed by the device, capped at
    /// the spec's 512-octet bound at decode. **Device-controlled free
    /// text** — it may name networks (e.g. an SSID); log deliberately.
    pub debug_text: Option<String>,
    // `network_index` deliberately omitted — only meaningful on the
    // scan path, which M6.5 does not ship.
}

/// Decoded `ConnectNetworkResponse` (spec §11.9.6.6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectNetworkResponse {
    /// `NetworkCommissioningStatusEnum`. 0 = OK.
    pub networking_status: u8,
    /// Optional human-readable debug text echoed by the device, capped at
    /// the spec's 512-octet bound at decode. **Device-controlled free
    /// text** — it may name networks (e.g. an SSID); log deliberately.
    pub debug_text: Option<String>,
    /// Platform-specific Wi-Fi error code (spec §11.9.6.6.3). Optional.
    pub error_value: Option<i32>,
}

/// Decode `NetworkConfigResponse` (spec §11.9.6.5).
///
/// `stage` is plumbed through so any error includes the right cursor
/// position in `CommissioningError::MalformedResponse(_)`. Callers
/// pass `Stage::NetworkSetup` in production.
///
/// # Errors
///
/// Returns `CommissioningError::MalformedResponse(stage)` on garbled
/// TLV. A `networking_status != 0` is a *successful* decode whose
/// non-OK value is mapped to
/// `CommissioningError::NetworkRejected { remediation_hint, .. }` by
/// the state-machine dispatch layer (M6.5.2).
pub fn decode_network_config_response(
    stage: Stage,
    tlv: &[u8],
) -> Result<NetworkConfigResponse, CommissioningError> {
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
    let mut networking_status: Option<u8> = None;
    let mut debug_text: Option<String> = None;
    loop {
        match reader
            .next()
            .map_err(|_| CommissioningError::MalformedResponse(stage))?
        {
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(v),
            }) => {
                if networking_status.is_some() {
                    return Err(CommissioningError::MalformedResponse(stage));
                }
                networking_status = Some(
                    u8::try_from(v).map_err(|_| CommissioningError::MalformedResponse(stage))?,
                );
            }
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Utf8(s),
            }) => {
                if debug_text.is_some() {
                    return Err(CommissioningError::MalformedResponse(stage));
                }
                // Spec bound (§11.9): DebugText ≤ 512 octets. Device-echoed free text — cap defensively.
                debug_text = Some(truncate_utf8(s, 512));
            }
            // Forward-compat: ignore tag 2 (network_index) on NetworkConfigResponse
            // and all other unknown scalar tags.
            Some(Element::Scalar { .. }) => {}
            // Forward-compat: skip an unknown container whole. Ignoring only its
            // start would read its members as top-level fields and end the
            // struct at its end-of-container.
            Some(Element::ContainerStart { .. }) => reader
                .skip_container()
                .map_err(|_| CommissioningError::MalformedResponse(stage))?,
            None | Some(_) => return Err(CommissioningError::MalformedResponse(stage)),
        }
    }
    // Nothing may follow the struct's end-of-container.
    reader
        .finish()
        .map_err(|_| CommissioningError::MalformedResponse(stage))?;
    let networking_status =
        networking_status.ok_or(CommissioningError::MalformedResponse(stage))?;
    Ok(NetworkConfigResponse {
        networking_status,
        debug_text,
    })
}

/// Decode `ConnectNetworkResponse` (spec §11.9.6.6.2).
///
/// # Errors
///
/// Returns `CommissioningError::MalformedResponse(stage)` on garbled
/// TLV. A `networking_status != 0` is a *successful* decode whose
/// non-OK value is mapped to
/// `CommissioningError::NetworkRejected { remediation_hint, .. }` by
/// the state-machine dispatch layer (M6.5.2).
pub fn decode_connect_network_response(
    stage: Stage,
    tlv: &[u8],
) -> Result<ConnectNetworkResponse, CommissioningError> {
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
    let mut networking_status: Option<u8> = None;
    let mut debug_text: Option<String> = None;
    let mut error_value: Option<i32> = None;
    loop {
        match reader
            .next()
            .map_err(|_| CommissioningError::MalformedResponse(stage))?
        {
            Some(Element::ContainerEnd) => break,
            Some(Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(v),
            }) => {
                if networking_status.is_some() {
                    return Err(CommissioningError::MalformedResponse(stage));
                }
                networking_status = Some(
                    u8::try_from(v).map_err(|_| CommissioningError::MalformedResponse(stage))?,
                );
            }
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Utf8(s),
            }) => {
                if debug_text.is_some() {
                    return Err(CommissioningError::MalformedResponse(stage));
                }
                // Spec bound (§11.9): DebugText ≤ 512 octets. Device-echoed free text — cap defensively.
                debug_text = Some(truncate_utf8(s, 512));
            }
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Int(v),
            }) => {
                if error_value.is_some() {
                    return Err(CommissioningError::MalformedResponse(stage));
                }
                error_value = Some(
                    i32::try_from(v).map_err(|_| CommissioningError::MalformedResponse(stage))?,
                );
            }
            // Forward-compat: ignore unknown scalar tags.
            Some(Element::Scalar { .. }) => {}
            // Forward-compat: skip an unknown container whole. Ignoring only its
            // start would read its members as top-level fields and end the
            // struct at its end-of-container.
            Some(Element::ContainerStart { .. }) => reader
                .skip_container()
                .map_err(|_| CommissioningError::MalformedResponse(stage))?,
            None | Some(_) => return Err(CommissioningError::MalformedResponse(stage)),
        }
    }
    // Nothing may follow the struct's end-of-container.
    reader
        .finish()
        .map_err(|_| CommissioningError::MalformedResponse(stage))?;
    let networking_status =
        networking_status.ok_or(CommissioningError::MalformedResponse(stage))?;
    Ok(ConnectNetworkResponse {
        networking_status,
        debug_text,
        error_value,
    })
}

/// Map a Matter `NetworkCommissioningStatusEnum` value (spec §11.9.5.1)
/// to its [`RemediationHint`] category. Used by the M6.5.2 dispatch
/// layer when constructing
/// `CommissioningError::NetworkRejected` (lands in M6.5.2).
///
/// Any unmapped value (including values outside the defined enum
/// range) returns [`RemediationHint::None`].
#[must_use]
pub const fn remediation_for(networking_status: u8) -> RemediationHint {
    match networking_status {
        2 => RemediationHint::DeviceNetworkSlotsFull,
        3 | 5 => RemediationHint::CheckSsid,
        6 => RemediationHint::CheckRegulatoryRegion,
        7 => RemediationHint::CheckPassphrase,
        8 => RemediationHint::UpgradeSecurityMode,
        10 | 11 => RemediationHint::DeviceIpStackFailure,
        _ => RemediationHint::None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    #[test]
    fn feature_bits_disjoint() {
        assert_eq!(NetworkCommissioningFeature::WIFI.bits(), 0b001);
        assert_eq!(NetworkCommissioningFeature::THREAD.bits(), 0b010);
        assert_eq!(NetworkCommissioningFeature::ETHERNET.bits(), 0b100);
    }

    #[test]
    fn cluster_id_is_0x0031() {
        assert_eq!(CLUSTER_ID, 0x0031);
    }

    #[test]
    fn add_or_update_wifi_network_matter_no_creds_matches_spec_bytes() {
        let bytes = encode_add_or_update_wifi_network(b"matter", b"", 0);
        assert_eq!(
            bytes,
            vec![
                0x15, 0x30, 0x00, 0x06, b'm', b'a', b't', b't', b'e', b'r', 0x30, 0x01, 0x00, 0x24,
                0x02, 0x00, 0x18,
            ],
            "encoded bytes: {bytes:02x?}",
        );
    }

    #[test]
    fn add_or_update_wifi_network_with_creds_includes_passphrase_bytes() {
        let bytes = encode_add_or_update_wifi_network(b"matter", b"hunter22", 1);
        // Check structural invariants without hand-computing the full byte string.
        assert_eq!(bytes.first(), Some(&0x15));
        assert_eq!(bytes.last(), Some(&0x18));
        let window = b"hunter22";
        assert!(
            bytes.windows(window.len()).any(|w| w == window),
            "credentials should appear in the payload literal",
        );
    }

    #[test]
    fn add_or_update_thread_network_bytes_match_vector() {
        // Vector: test-vectors/thread/network_commissioning.json
        // "add_or_update_thread_network" — reference dataset from
        // "reference_operational_dataset" in the same file (111 bytes,
        // OTBR `ot-ctl dataset active -x` capture).
        let ds = hex::decode(
            "0e08000000000001000000030000184a0300001235060004001fffe0020878\
             96217f787f6ebe0708fdec3f34f3cd2020051071dccee3f164f15da92254e0\
             b9c8a3a5030f4f70656e5468726561642d3839643701\
             0289d70410dc4b544c7a58671a2ce4f876f5d6dcd90c0402a0f7f8",
        )
        .expect("valid hex literal");
        assert_eq!(ds.len(), 111, "reference dataset must be 111 bytes");
        let got = encode_add_or_update_thread_network(&ds, 1);
        assert_eq!(
            hex::encode(&got),
            "1530006f0e08000000000001000000030000184a0300001235060004001fff\
             e002087896217f787f6ebe0708fdec3f34f3cd2020051071dccee3f164f15d\
             a92254e0b9c8a3a5030f4f70656e5468726561642d383964370102\
             89d70410dc4b544c7a58671a2ce4f876f5d6dcd90c0402a0f7f824010118",
            "encoded bytes: {got:02x?}",
        );
    }

    #[test]
    fn connect_network_thread_bytes_match_vector() {
        // Vector: test-vectors/thread/network_commissioning.json
        // "connect_network_thread" — network_id is the Extended PAN ID
        // (8 bytes) extracted from the reference dataset, not an SSID.
        let ext_pan_id = [0x78, 0x96, 0x21, 0x7f, 0x78, 0x7f, 0x6e, 0xbe];
        let got = encode_connect_network(&ext_pan_id, 1);
        assert_eq!(
            hex::encode(&got),
            "153000087896217f787f6ebe24010118",
            "encoded bytes: {got:02x?}",
        );
    }

    #[test]
    fn connect_network_matter_matches_spec_bytes() {
        let bytes = encode_connect_network(b"matter", 0);
        assert_eq!(
            bytes,
            vec![
                0x15, 0x30, 0x00, 0x06, b'm', b'a', b't', b't', b'e', b'r', 0x24, 0x01, 0x00, 0x18,
            ],
            "encoded bytes: {bytes:02x?}",
        );
    }

    #[test]
    fn decode_feature_map_round_trips_all_8_combinations() {
        // TLV encoding for u32 value `v` (anonymous tag, minimum width):
        // - 0 ..= 0xFF        → 0x04 0xVV               (uint-1B)
        // - 0x100 ..= 0xFFFF  → 0x05 0xLO 0xHI          (uint-2B)
        // For all 3-bit values (0..=7) we hit the uint-1B branch.
        for raw in 0u8..8 {
            let tlv = vec![0x04, raw];
            let decoded = decode_feature_map(&tlv).expect("happy path decodes");
            assert_eq!(decoded.bits(), u32::from(raw));
        }
    }

    #[test]
    fn decode_feature_map_rejects_non_uint_tlv() {
        // Octet-string TLV — wrong element type.
        let tlv = vec![0x10, 0x00];
        let err = decode_feature_map(&tlv).expect_err("should fail");
        assert!(
            matches!(err, CommissioningError::MalformedResponse(_)),
            "got {err:?}",
        );
    }

    #[test]
    fn decode_feature_map_truncates_high_bits_safely() {
        // Reserved bits ignored — only WIFI|THREAD|ETHERNET (bits 0-2) recognised.
        // raw value 0x0F = WIFI|THREAD|ETHERNET + bit 3 (reserved). Bit 3 dropped
        // by from_bits_truncate.
        let tlv = vec![0x04, 0x0F];
        let decoded = decode_feature_map(&tlv).expect("decodes");
        assert_eq!(
            decoded,
            NetworkCommissioningFeature::WIFI
                | NetworkCommissioningFeature::THREAD
                | NetworkCommissioningFeature::ETHERNET,
        );
    }

    #[test]
    fn decode_connect_max_time_seconds_round_trips() {
        // 1-byte uint: { 0x04, 30 } → 30 seconds.
        assert_eq!(decode_connect_max_time_seconds(&[0x04, 30]).unwrap(), 30);
        // 2-byte uint: 0x05 <lo> <hi> → 300 seconds (0x012C).
        assert_eq!(
            decode_connect_max_time_seconds(&[0x05, 0x2C, 0x01]).unwrap(),
            300
        );
    }

    #[test]
    fn decode_connect_max_time_seconds_clamps_oversize_to_u16_max() {
        // 4-byte uint 0x0001_0000 (65536) exceeds u16 → clamped, not rejected.
        let tlv = vec![0x06, 0x00, 0x00, 0x01, 0x00];
        assert_eq!(decode_connect_max_time_seconds(&tlv).unwrap(), u16::MAX);
    }

    #[test]
    fn decode_connect_max_time_seconds_rejects_non_uint() {
        // Octet-string TLV — wrong element type.
        let err = decode_connect_max_time_seconds(&[0x10, 0x00]).expect_err("should fail");
        assert!(
            matches!(err, CommissioningError::MalformedResponse(_)),
            "got {err:?}",
        );
    }

    #[test]
    fn network_config_response_ok_round_trips() {
        // { 0: 0_u8 }
        let tlv = vec![0x15, 0x24, 0x00, 0x00, 0x18];
        let decoded =
            decode_network_config_response(Stage::NetworkSetup, &tlv).expect("happy path decodes");
        assert_eq!(decoded.networking_status, 0);
        assert_eq!(decoded.debug_text, None);
    }

    #[test]
    fn network_config_response_auth_failure_with_debug_text() {
        // { 0: 7_u8, 1: "wrong-pw" }
        let tlv = vec![
            0x15, 0x24, 0x00, 0x07, 0x2C, 0x01, 0x08, b'w', b'r', b'o', b'n', b'g', b'-', b'p',
            b'w', 0x18,
        ];
        let decoded =
            decode_network_config_response(Stage::NetworkSetup, &tlv).expect("happy path decodes");
        assert_eq!(decoded.networking_status, 7);
        assert_eq!(decoded.debug_text.as_deref(), Some("wrong-pw"));
    }

    #[test]
    fn network_config_response_malformed_returns_error() {
        let err =
            decode_network_config_response(Stage::NetworkSetup, &[0xFF]).expect_err("should fail");
        assert!(
            matches!(err, CommissioningError::MalformedResponse(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn connect_network_response_ok_round_trips() {
        let tlv = vec![0x15, 0x24, 0x00, 0x00, 0x18];
        let decoded = decode_connect_network_response(Stage::NetworkEnable, &tlv)
            .expect("happy path decodes");
        assert_eq!(decoded.networking_status, 0);
        assert_eq!(decoded.debug_text, None);
        assert_eq!(decoded.error_value, None);
    }

    #[test]
    fn connect_network_response_carries_error_value() {
        // { 0: 9_u8, 2: i32 +10 }  — signed-int control byte 0x20, 1-byte value
        let tlv = vec![
            0x15, 0x24, 0x00, 0x09, // networking_status = 9
            0x20, 0x02, 0x0A, // signed-int 1B, value +10
            0x18,
        ];
        let decoded = decode_connect_network_response(Stage::NetworkEnable, &tlv).expect("decodes");
        assert_eq!(decoded.networking_status, 9);
        assert_eq!(decoded.error_value, Some(10));
    }

    #[test]
    fn connect_network_response_malformed_returns_error() {
        let err = decode_connect_network_response(Stage::NetworkEnable, &[0xFF])
            .expect_err("should fail");
        assert!(
            matches!(err, CommissioningError::MalformedResponse(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn truncate_utf8_caps_bytes_without_splitting_chars() {
        // ASCII: exact byte cap.
        let s = "a".repeat(600);
        assert_eq!(truncate_utf8(s, 512).len(), 512);

        // Under the cap: untouched.
        assert_eq!(truncate_utf8("short".to_string(), 512), "short");

        // Multi-byte char straddling the boundary: floor to the previous char
        // boundary — never panic, never emit invalid UTF-8. 'é' is 2 bytes;
        // 511 ASCII bytes + 'é' puts the boundary mid-char at 512.
        let mut s = "a".repeat(511);
        s.push('é');
        let t = truncate_utf8(s, 512);
        assert_eq!(t.len(), 511, "must floor to the char boundary");
        assert!(t.is_char_boundary(t.len()));
    }

    #[test]
    fn decode_caps_debug_text_at_512_bytes() {
        use matter_codec::{Tag, TlvWriter};
        // NetworkConfigResponse TLV: anonymous struct { [0]=5u, [1]=600x'x' }.
        let long_text = "x".repeat(600);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 5).unwrap();
        w.put_utf8(Tag::Context(1), &long_text).unwrap();
        w.end_container().unwrap();

        let resp = decode_network_config_response(Stage::NetworkSetup, &buf).unwrap();
        assert_eq!(resp.networking_status, 5);
        assert_eq!(resp.debug_text.unwrap().len(), 512, "capped at spec bound");
    }

    #[test]
    fn decode_connect_network_response_caps_debug_text_at_512_bytes() {
        use matter_codec::{Tag, TlvWriter};
        // ConnectNetworkResponse TLV: anonymous struct { [0]=5u, [1]=600x'x' }.
        let long_text = "x".repeat(600);
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 5).unwrap();
        w.put_utf8(Tag::Context(1), &long_text).unwrap();
        w.end_container().unwrap();

        let resp = decode_connect_network_response(Stage::NetworkEnable, &buf).unwrap();
        assert_eq!(resp.networking_status, 5);
        assert_eq!(resp.debug_text.unwrap().len(), 512, "capped at spec bound");
    }

    #[test]
    fn remediation_for_table_matches_spec() {
        use RemediationHint::*;
        let table: &[(u8, RemediationHint)] = &[
            (0, None),                   // Success — non-error path, mapping is best-effort.
            (1, None),                   // OutOfRange
            (2, DeviceNetworkSlotsFull), // BoundsExceeded
            (3, CheckSsid),              // NetworkIDNotFound
            (4, None),                   // DuplicateNetworkID
            (5, CheckSsid),              // NetworkNotFound
            (6, CheckRegulatoryRegion),  // RegulatoryError
            (7, CheckPassphrase),        // AuthFailure
            (8, UpgradeSecurityMode),    // UnsupportedSecurity
            (9, None),                   // OtherConnectionFailure
            (10, DeviceIpStackFailure),  // IPV6Failed
            (11, DeviceIpStackFailure),  // IPBindFailed
            (12, None),                  // UnknownError
        ];
        for (code, expected) in table {
            assert_eq!(
                remediation_for(*code),
                *expected,
                "remediation_for({code}) mismatch",
            );
        }
        // Unknown values (above the defined enum range) fall through to None.
        assert_eq!(remediation_for(99), RemediationHint::None);
        assert_eq!(remediation_for(u8::MAX), RemediationHint::None);
    }
}
