//! EZSP types of the startup sequence: statuses, network state and
//! network parameters, with what differs between protocol versions.

use super::EzspError;

/// First EZSP version whose statuses are a 32-bit `SlStatus`; before it they
/// are one byte (zigbee-herdsman, `EzspBuffalo.readStatus`: `version < 0x0e`).
pub const SL_STATUS_VERSION: u8 = 0x0e;

/// Silicon Labs status (`sl_status_t`), the status of every EZSP v14+
/// response. Values transcribed from zigbee-herdsman (MIT),
/// `src/adapter/ember/enums.ts`, enum `SLStatus`; only the codes the
/// driver looks at are named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlStatus {
    Ok,
    Fail,
    InvalidState,
    NotReady,
    Busy,
    NetworkUp,
    NetworkDown,
    NotJoined,
    InvalidParameter,
    /// An EZSP-level error (unknown ID, bad value…), however the NCP said it.
    ZigbeeEzspError,
    Other(u32),
}

/// Codes of the named `SlStatus` variants (zigbee-herdsman, `enums.ts`, `SLStatus`).
const SL_CODES: [(SlStatus, u32); 10] = [
    (SlStatus::Ok, 0x0000),
    (SlStatus::Fail, 0x0001),
    (SlStatus::InvalidState, 0x0002),
    (SlStatus::NotReady, 0x0003),
    (SlStatus::Busy, 0x0004),
    (SlStatus::NetworkUp, 0x0015),
    (SlStatus::NetworkDown, 0x0016),
    (SlStatus::NotJoined, 0x0017),
    (SlStatus::InvalidParameter, 0x0021),
    (SlStatus::ZigbeeEzspError, 0x0c1e),
];

impl SlStatus {
    #[must_use]
    pub fn from_u32(code: u32) -> Self {
        SL_CODES
            .iter()
            .find(|&&(_, c)| c == code)
            .map_or(Self::Other(code), |&(status, _)| status)
    }

    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            Self::Other(code) => code,
            named => SL_CODES
                .iter()
                .find(|&&(status, _)| status == named)
                .map_or(0, |&(_, code)| code),
        }
    }
}

/// EmberStatus, the one-byte status of EZSP v13 responses. Values from
/// zigbee-herdsman (MIT), `src/adapter/ember/ezsp/buffalo.ts`,
/// `EMBER_TO_SL_STATUS_MAP` (whose comments name them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmberStatus {
    Success,
    BadArgument,
    InvalidCall,
    NetworkUp,
    NetworkDown,
    NotJoined,
    NetworkBusy,
    Other(u8),
}

/// Codes of the named `EmberStatus` variants.
const EMBER_CODES: [(EmberStatus, u8); 7] = [
    (EmberStatus::Success, 0x00),
    (EmberStatus::BadArgument, 0x02),
    (EmberStatus::InvalidCall, 0x70),
    (EmberStatus::NetworkUp, 0x90),
    (EmberStatus::NetworkDown, 0x91),
    (EmberStatus::NotJoined, 0x93),
    (EmberStatus::NetworkBusy, 0xa1),
];

/// EmberStatus code -> SlStatus code, for v13 NCPs. Transcribed from
/// zigbee-herdsman (MIT), `src/adapter/ember/ezsp/buffalo.ts`,
/// `EMBER_TO_SL_STATUS_MAP`, values resolved through `SLStatus` in
/// `src/adapter/ember/enums.ts`.
const EMBER_TO_SL: [(u8, u32); 38] = [
    (0x02, 0x0021), // BAD_ARGUMENT -> INVALID_PARAMETER
    (0x18, 0x0019), // NO_BUFFERS -> ALLOCATION_FAILED
    (0x19, 0x0c01), // PACKET_HANDOFF_DROP_PACKET -> ZIGBEE_PACKET_HANDOFF_DROPPED
    (0x66, 0x0c02), // DELIVERY_FAILED -> ZIGBEE_DELIVERY_FAILED
    (0x70, 0x0002), // INVALID_CALL -> INVALID_STATE
    (0x72, 0x0c03), // MAX_MESSAGE_LIMIT_REACHED -> ZIGBEE_MAX_MESSAGE_LIMIT_REACHED
    (0x74, 0x0038), // MESSAGE_TOO_LONG -> MESSAGE_TOO_LONG
    (0x75, 0x0c04), // BINDING_IS_ACTIVE -> ZIGBEE_BINDING_IS_ACTIVE
    (0x76, 0x0c05), // ADDRESS_TABLE_ENTRY_IS_ACTIVE -> ZIGBEE_ADDRESS_TABLE_ENTRY_IS_ACTIVE
    (0x90, 0x0015), // NETWORK_UP -> NETWORK_UP
    (0x91, 0x0016), // NETWORK_DOWN -> NETWORK_DOWN
    (0x93, 0x0017), // NOT_JOINED -> NOT_JOINED
    (0x95, 0x0c08), // INVALID_SECURITY_LEVEL -> ZIGBEE_INVALID_SECURITY_LEVEL
    (0x96, 0x0c06), // MOVE_FAILED -> ZIGBEE_MOVE_FAILED
    (0x99, 0x0c07), // NODE_ID_CHANGED -> ZIGBEE_NODE_ID_CHANGED
    (0x9a, 0x0c16), // PAN_ID_CHANGED -> ZIGBEE_PAN_ID_CHANGED
    (0x9b, 0x0c17), // CHANNEL_CHANGED -> ZIGBEE_CHANNEL_CHANGED
    (0x9c, 0x0c18), // NETWORK_OPENED -> ZIGBEE_NETWORK_OPENED
    (0x9d, 0x0c19), // NETWORK_CLOSED -> ZIGBEE_NETWORK_CLOSED
    (0xa1, 0x0004), // NETWORK_BUSY -> BUSY
    (0xa4, 0x0c0f), // BINDING_HAS_CHANGED -> ZIGBEE_BINDING_HAS_CHANGED
    (0xa5, 0x0c12), // INSUFFICIENT_RANDOM_DATA -> ZIGBEE_INSUFFICIENT_RANDOM_DATA
    (0xa6, 0x0c0a), // APS_ENCRYPTION_ERROR -> ZIGBEE_APS_ENCRYPTION_ERROR
    (0xa9, 0x0c13), // SOURCE_ROUTE_FAILURE -> ZIGBEE_SOURCE_ROUTE_FAILURE
    (0xa8, 0x0c0b), // SECURITY_STATE_NOT_SET -> ZIGBEE_SECURITY_STATE_NOT_SET
    (0xaa, 0x0c14), // MANY_TO_ONE_ROUTE_FAILURE -> ZIGBEE_MANY_TO_ONE_ROUTE_FAILURE
    (0xac, 0x0c1a), // RECEIVED_KEY_IN_THE_CLEAR -> ZIGBEE_RECEIVED_KEY_IN_THE_CLEAR
    (0xad, 0x0c1b), // NO_NETWORK_KEY_RECEIVED -> ZIGBEE_NO_NETWORK_KEY_RECEIVED
    (0xae, 0x0c1c), // NO_LINK_KEY_RECEIVED -> ZIGBEE_NO_LINK_KEY_RECEIVED
    (0xaf, 0x0c1d), // PRECONFIGURED_KEY_REQUIRED -> ZIGBEE_PRECONFIGURED_KEY_REQUIRED
    (0xb0, 0x0c15), // STACK_AND_HARDWARE_MISMATCH -> ZIGBEE_STACK_AND_HARDWARE_MISMATCH
    (0xb5, 0x000e), // LIBRARY_NOT_PRESENT -> NOT_AVAILABLE
    (0xb8, 0x0c0c), // TOO_SOON_FOR_SWITCH_KEY -> ZIGBEE_TOO_SOON_FOR_SWITCH_KEY
    (0xb9, 0x0c0d), // SIGNATURE_VERIFY_FAILURE -> ZIGBEE_SIGNATURE_VERIFY_FAILURE
    (0xbb, 0x0c0e), // KEY_NOT_AUTHORIZED -> ZIGBEE_KEY_NOT_AUTHORIZED
    (0xbc, 0x0c10), // TRUST_CENTER_EUI_HAS_CHANGED -> ZIGBEE_TRUST_CENTER_SWAP_EUI_HAS_CHANGED
    (0xbe, 0x0c09), // IEEE_ADDRESS_DISCOVERY_IN_PROGRESS -> ZIGBEE_IEEE_ADDRESS_DISCOVERY_IN_PROGRESS
    (0xbf, 0x0c11), // TRUST_CENTER_SWAPPED_OUT_EUI_HAS_NOT_CHANGED -> ZIGBEE_TRUST_CENTER_SWAP_EUI_HAS_NOT_CHANGED
];

impl EmberStatus {
    #[must_use]
    pub fn from_u8(code: u8) -> Self {
        EMBER_CODES
            .iter()
            .find(|&&(_, c)| c == code)
            .map_or(Self::Other(code), |&(status, _)| status)
    }

    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Other(code) => code,
            named => EMBER_CODES
                .iter()
                .find(|&&(status, _)| status == named)
                .map_or(0, |&(_, code)| code),
        }
    }

    /// The `SlStatus` a v14+ NCP would have answered; an unmapped code is
    /// passed through as is, like herdsman does.
    #[must_use]
    pub fn to_sl(self) -> SlStatus {
        let code = self.code();
        if code == 0 {
            return SlStatus::Ok;
        }
        let sl = EMBER_TO_SL
            .iter()
            .find(|&&(ember, _)| ember == code)
            .map_or(u32::from(code), |&(_, sl)| sl);
        SlStatus::from_u32(sl)
    }
}

/// Reads the status that starts most responses: an `EmberStatus` byte
/// before v14, a 32-bit `SlStatus` from v14 on.
pub fn read_status(params: &[u8], version: u8) -> Result<(SlStatus, &[u8]), EzspError> {
    read_any_status(params, version, |code| EmberStatus::from_u8(code).to_sl())
}

/// Same as `read_status`, for the commands whose v13 status is an
/// `EzspStatus` (configuration, policy and value commands, `addEndpoint`):
/// any failure there reads as `ZigbeeEzspError`, as in herdsman.
pub fn read_ezsp_status(params: &[u8], version: u8) -> Result<(SlStatus, &[u8]), EzspError> {
    read_any_status(params, version, |code| {
        if code == 0 {
            SlStatus::Ok
        } else {
            SlStatus::ZigbeeEzspError
        }
    })
}

/// The one place where the status width depends on the version.
fn read_any_status(
    params: &[u8],
    version: u8,
    legacy: impl FnOnce(u8) -> SlStatus,
) -> Result<(SlStatus, &[u8]), EzspError> {
    if version < SL_STATUS_VERSION {
        let (&code, rest) = params.split_first().ok_or(EzspError::TooShort)?;
        Ok((legacy(code), rest))
    } else {
        let (code, rest) = params.split_first_chunk::<4>().ok_or(EzspError::TooShort)?;
        Ok((SlStatus::from_u32(u32::from_le_bytes(*code)), rest))
    }
}

/// `EmberNetworkStatus`, the answer to `networkState`. Values transcribed
/// from zigbee-herdsman (MIT), `src/adapter/ember/enums.ts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkStatus {
    NoNetwork,
    JoiningNetwork,
    JoinedNetwork,
    JoinedNetworkNoParent,
    JoinedNetworkS2sInitiator,
    JoinedNetworkS2sTarget,
    LeavingNetwork,
    Other(u8),
}

impl NetworkStatus {
    #[must_use]
    pub fn from_u8(code: u8) -> Self {
        match code {
            0 => Self::NoNetwork,
            1 => Self::JoiningNetwork,
            2 => Self::JoinedNetwork,
            3 => Self::JoinedNetworkNoParent,
            4 => Self::JoinedNetworkS2sInitiator,
            5 => Self::JoinedNetworkS2sTarget,
            6 => Self::LeavingNetwork,
            other => Self::Other(other),
        }
    }
}

/// `EmberNetworkParameters`, as `formNetwork` takes them and
/// `getNetworkParameters` returns them. Layout from zigbee-herdsman (MIT),
/// `src/adapter/ember/ezsp/buffalo.ts`, `writeEmberNetworkParameters`;
/// integers little-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkParameters {
    /// As the stack stores it (herdsman copies these 8 bytes unchanged).
    pub extended_pan_id: [u8; 8],
    pub pan_id: u16,
    /// dBm.
    pub radio_tx_power: i8,
    pub radio_channel: u8,
    /// `EmberJoinMethod`; ignored when forming.
    pub join_method: u8,
    pub nwk_manager_id: u16,
    pub nwk_update_id: u8,
    /// Channel mask (bit n = channel n).
    pub channels: u32,
}

impl NetworkParameters {
    /// Bytes on the wire.
    pub const LEN: usize = 20;

    /// Appends the parameters to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.extended_pan_id);
        out.extend_from_slice(&self.pan_id.to_le_bytes());
        out.extend_from_slice(&self.radio_tx_power.to_le_bytes());
        out.push(self.radio_channel);
        out.push(self.join_method);
        out.extend_from_slice(&self.nwk_manager_id.to_le_bytes());
        out.push(self.nwk_update_id);
        out.extend_from_slice(&self.channels.to_le_bytes());
    }

    /// Reads the parameters at the start of `bytes`; returns what follows.
    pub fn decode(bytes: &[u8]) -> Result<(Self, &[u8]), EzspError> {
        let (raw, rest) = bytes
            .split_first_chunk::<{ Self::LEN }>()
            .ok_or(EzspError::TooShort)?;
        let [
            e0,
            e1,
            e2,
            e3,
            e4,
            e5,
            e6,
            e7,
            pan0,
            pan1,
            tx_power,
            radio_channel,
            join_method,
            manager0,
            manager1,
            nwk_update_id,
            c0,
            c1,
            c2,
            c3,
        ] = *raw;
        let params = Self {
            extended_pan_id: [e0, e1, e2, e3, e4, e5, e6, e7],
            pan_id: u16::from_le_bytes([pan0, pan1]),
            radio_tx_power: i8::from_le_bytes([tx_power]),
            radio_channel,
            join_method,
            nwk_manager_id: u16::from_le_bytes([manager0, manager1]),
            nwk_update_id,
            channels: u32::from_le_bytes([c0, c1, c2, c3]),
        };
        Ok((params, rest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ezsp::{decode, ids};

    fn params() -> NetworkParameters {
        NetworkParameters {
            extended_pan_id: [0xDD; 8],
            pan_id: 0x1a62,
            radio_tx_power: 5,
            radio_channel: 15,
            join_method: 0,
            nwk_manager_id: 0x0000,
            nwk_update_id: 0,
            channels: 1 << 15,
        }
    }

    #[test]
    fn network_parameters_round_trip() {
        let mut out = vec![0xAA];
        params().encode(&mut out);
        assert_eq!(
            out,
            [
                0xAA, // what was already there stays
                0xDD, 0xDD, 0xDD, 0xDD, 0xDD, 0xDD, 0xDD, 0xDD, // extended PAN ID
                0x62, 0x1a, // PAN ID
                0x05, // tx power
                0x0F, // channel
                0x00, // join method
                0x00, 0x00, // network manager
                0x00, // update ID
                0x00, 0x80, 0x00, 0x00, // channel mask
            ]
        );
        let mut tail = out[1..].to_vec();
        tail.push(0x42);
        let (decoded, rest) = NetworkParameters::decode(&tail).unwrap();
        assert_eq!(decoded, params());
        assert_eq!(rest, [0x42]);
        assert_eq!(
            NetworkParameters::decode(&out[1..NetworkParameters::LEN]),
            Err(EzspError::TooShort)
        );
    }

    #[test]
    fn negative_tx_power_round_trips() {
        let p = NetworkParameters {
            radio_tx_power: -3,
            ..params()
        };
        let mut out = Vec::new();
        p.encode(&mut out);
        assert_eq!(out[10], 0xFD);
        assert_eq!(NetworkParameters::decode(&out).unwrap().0, p);
    }

    #[test]
    fn status_width_by_version() {
        // The same `networkInit` answer (not joined), as v13 and v14 NCPs send it.
        let v13 = [1, 0x80, 0x01, 0x17, 0x00, 0x93];
        let v14 = [1, 0x80, 0x01, 0x17, 0x00, 0x17, 0x00, 0x00, 0x00];
        for (frame, version) in [(&v13[..], 13), (&v14[..], 14)] {
            let (header, params) = decode(frame).unwrap();
            assert_eq!(header.id, ids::NETWORK_INIT);
            let (status, rest) = read_status(params, version).unwrap();
            assert_eq!(status, SlStatus::NotJoined, "v{version}");
            assert!(rest.is_empty(), "v{version}");
        }
        // Read with the wrong rule, a v13 answer is too short for v14.
        assert_eq!(read_status(&v13[5..], 14), Err(EzspError::TooShort));
        assert_eq!(read_status(&[], 13), Err(EzspError::TooShort));
        // Success is 0 in both.
        assert_eq!(read_status(&[0], 13).unwrap().0, SlStatus::Ok);
        assert_eq!(read_status(&[0; 4], 14).unwrap().0, SlStatus::Ok);
    }

    #[test]
    fn ember_statuses_map_to_sl() {
        assert_eq!(EmberStatus::from_u8(0x90).to_sl(), SlStatus::NetworkUp);
        assert_eq!(EmberStatus::from_u8(0x91).to_sl(), SlStatus::NetworkDown);
        assert_eq!(EmberStatus::from_u8(0x70).to_sl(), SlStatus::InvalidState);
        assert_eq!(
            EmberStatus::from_u8(0x02).to_sl(),
            SlStatus::InvalidParameter
        );
        assert_eq!(EmberStatus::from_u8(0xA1).to_sl(), SlStatus::Busy);
        // Mapped, but not named here.
        assert_eq!(EmberStatus::from_u8(0x9C).to_sl(), SlStatus::Other(0x0c18));
        // Not mapped: the number is passed through, as herdsman does.
        assert_eq!(EmberStatus::from_u8(0x42), EmberStatus::Other(0x42));
        assert_eq!(EmberStatus::from_u8(0x42).to_sl(), SlStatus::Other(0x42));
        assert_eq!(EmberStatus::from_u8(0x01).to_sl(), SlStatus::Fail);
        for code in [0x00, 0x02, 0x70, 0x90, 0x91, 0x93, 0xA1, 0x42] {
            assert_eq!(EmberStatus::from_u8(code).code(), code);
        }
    }

    #[test]
    fn sl_status_codes() {
        for code in [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x15, 0x16, 0x17, 0x21, 0x0c1e, 0x0c18,
        ] {
            assert_eq!(SlStatus::from_u32(code).code(), code);
        }
        assert_eq!(SlStatus::from_u32(0x0c1e), SlStatus::ZigbeeEzspError);
        assert_eq!(SlStatus::from_u32(0x0c18), SlStatus::Other(0x0c18));
    }

    #[test]
    fn ezsp_status_by_version() {
        // `setConfigurationValue` refused: an EzspStatus in v13, an SlStatus in v14.
        assert_eq!(
            read_ezsp_status(&[0x36], 13).unwrap().0,
            SlStatus::ZigbeeEzspError
        );
        assert_eq!(read_ezsp_status(&[0x00], 13).unwrap().0, SlStatus::Ok);
        assert_eq!(
            read_ezsp_status(&[0x1e, 0x0c, 0, 0], 14).unwrap().0,
            SlStatus::ZigbeeEzspError
        );
    }

    #[test]
    fn network_status_values() {
        assert_eq!(NetworkStatus::from_u8(0), NetworkStatus::NoNetwork);
        assert_eq!(NetworkStatus::from_u8(2), NetworkStatus::JoinedNetwork);
        assert_eq!(NetworkStatus::from_u8(6), NetworkStatus::LeavingNetwork);
        assert_eq!(NetworkStatus::from_u8(9), NetworkStatus::Other(9));
    }
}
