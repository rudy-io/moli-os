//! BTP handshake request/response codec (Matter spec 4.18.3.1/4.18.3.2).
//!
//! Byte-grounded against connectedhomeip `src/ble/BleLayer.cpp` (encode/decode
//! of `BleTransportCapabilitiesRequestMessage` / `...ResponseMessage`) and
//! `BLEEndPoint.cpp` (central-side handling of the peripheral's response).
//! Test vectors: `test-vectors/btp/handshake.json`.

use crate::BtpError;

/// The only BTP version we implement (chip: BleLayer.h:87-97).
pub const BTP_VERSION: u8 = 4;
/// Max fragment size we accept (chip: `BtpEngine` `kMaxFragmentSize` = 244).
pub const MAX_SEGMENT_SIZE: u16 = 244;
/// Min legal fragment size (BTP header + 1).
pub const MIN_SEGMENT_SIZE: u16 = 6;
/// Our receive window (chip: `BLE_MAX_RECEIVE_WINDOW_SIZE` = 6).
pub const WINDOW_SIZE: u8 = 6;

const CHECK_BYTE_1: u8 = 0x65;
const CHECK_BYTE_2: u8 = 0x6c;

/// The handshake request we (as BTP central) send to the peripheral.
///
/// Only a single supported version (`BTP_VERSION`) is ever advertised, so the
/// nibble-packed version array always carries that one value at index 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandshakeRequest {
    /// Our ATT MTU, or 0 if unknown/unobserved (BLEEndPoint.cpp:991-998 falls
    /// back to the platform delegate when 0).
    pub mtu: u16,
    /// Our receive window size.
    pub window: u8,
}

impl HandshakeRequest {
    /// Encode the 9-byte `BleTransportCapabilitiesRequestMessage`.
    ///
    /// Layout (chip: BleLayer.cpp:135-178): check bytes `0x65 0x6c`, then the
    /// nibble-packed supported-version array (4 bytes; even index -> low
    /// nibble of `slot[index/2]`, odd index -> high nibble — with a single
    /// supported version, index 0's low nibble carries `BTP_VERSION` and the
    /// rest are zero), then MTU as u16 LE, then the window byte.
    #[must_use]
    pub fn encode(&self) -> [u8; 9] {
        let mtu = self.mtu.to_le_bytes();
        [
            CHECK_BYTE_1,
            CHECK_BYTE_2,
            BTP_VERSION, // index 0 low nibble = BTP_VERSION, high nibble 0
            0x00,        // indices 2,3
            0x00,        // indices 4,5
            0x00,        // indices 6,7 (only 8 version slots representable; unused)
            mtu[0],
            mtu[1],
            self.window,
        ]
    }
}

/// The handshake response we (as BTP central) receive from the peripheral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandshakeResponse {
    /// The BTP version the peripheral selected. Always `BTP_VERSION` on
    /// success — chip aborts the connection for anything else
    /// (BLEEndPoint.cpp:1064-1068).
    pub version: u8,
    /// The negotiated fragment size, clamped to `MAX_SEGMENT_SIZE`.
    pub segment_size: u16,
    /// The negotiated receive window, clamped to `WINDOW_SIZE`.
    pub window: u8,
}

impl HandshakeResponse {
    /// Parse the 6-byte `BleTransportCapabilitiesResponseMessage`
    /// (chip: BleLayer.cpp:206-223, `kCapabilitiesResponseLength`).
    ///
    /// # Errors
    /// - [`BtpError::MalformedHandshake`]: length != 6, check bytes wrong, or
    ///   window is 0.
    /// - [`BtpError::UnsupportedVersion`]: selected version != `BTP_VERSION`
    ///   (chip's central aborts rather than adapting, BLEEndPoint.cpp:1064-1068).
    /// - [`BtpError::FragmentTooSmall`]: segment size below `MIN_SEGMENT_SIZE`.
    ///
    /// Segment size above `MAX_SEGMENT_SIZE` is clamped down (our defensive
    /// choice; the chip central applies the equivalent clamp at
    /// BLEEndPoint.cpp:1071). Window above `WINDOW_SIZE` is likewise clamped
    /// down to our own minimum — this is OUR defensive choice, not chip's:
    /// the real chip central adopts the peripheral's response verbatim
    /// (BLEEndPoint.cpp:1080).
    pub fn parse(packet: &[u8]) -> Result<Self, BtpError> {
        if packet.len() != 6 {
            return Err(BtpError::MalformedHandshake);
        }
        if packet[0] != CHECK_BYTE_1 || packet[1] != CHECK_BYTE_2 {
            return Err(BtpError::MalformedHandshake);
        }
        let version = packet[2];
        if version != BTP_VERSION {
            return Err(BtpError::UnsupportedVersion(version));
        }
        let segment_size = u16::from_le_bytes([packet[3], packet[4]]);
        if segment_size < MIN_SEGMENT_SIZE {
            return Err(BtpError::FragmentTooSmall(segment_size));
        }
        let segment_size = segment_size.min(MAX_SEGMENT_SIZE);
        let window = packet[5];
        if window == 0 {
            return Err(BtpError::MalformedHandshake);
        }
        let window = window.min(WINDOW_SIZE);
        Ok(Self {
            version,
            segment_size,
            window,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;

    // Vector: test-vectors/btp/handshake.json "our_canonical_request"
    #[test]
    fn encodes_canonical_request() {
        let req = HandshakeRequest { mtu: 0, window: 6 };
        assert_eq!(hex_str(&req.encode()), "656c04000000000006");
    }

    // Vector: test-vectors/btp/handshake.json "expected_chip_peripheral_response"
    #[test]
    fn parses_expected_peripheral_response() {
        let resp = HandshakeResponse::parse(&hex("656c04f40006")).unwrap();
        assert_eq!(
            resp,
            HandshakeResponse {
                version: 4,
                segment_size: 244,
                window: 6,
            }
        );
    }

    #[test]
    fn rejects_unsupported_version() {
        assert_eq!(
            HandshakeResponse::parse(&hex("656c05f40006")),
            Err(BtpError::UnsupportedVersion(5))
        );
    }

    #[test]
    fn rejects_undersized_fragment() {
        assert_eq!(
            HandshakeResponse::parse(&hex("656c04050006")),
            Err(BtpError::FragmentTooSmall(5))
        );
    }

    #[test]
    fn clamps_oversize_fragment() {
        let resp = HandshakeResponse::parse(&hex("656c042c0106")).unwrap();
        assert_eq!(resp.segment_size, MAX_SEGMENT_SIZE);
    }

    #[test]
    fn clamps_oversize_window() {
        let resp = HandshakeResponse::parse(&hex("656c04f40008")).unwrap();
        assert_eq!(resp.window, WINDOW_SIZE);
    }

    #[test]
    fn rejects_wrong_check_bytes() {
        assert_eq!(
            HandshakeResponse::parse(&hex("656d04f40006")),
            Err(BtpError::MalformedHandshake)
        );
    }

    #[test]
    fn rejects_wrong_length() {
        assert_eq!(
            HandshakeResponse::parse(&hex("656c04f400")),
            Err(BtpError::MalformedHandshake)
        );
    }

    #[test]
    fn rejects_zero_window() {
        assert_eq!(
            HandshakeResponse::parse(&hex("656c04f40000")),
            Err(BtpError::MalformedHandshake)
        );
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn hex_str(bytes: &[u8]) -> String {
        use std::fmt::Write;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }
}
