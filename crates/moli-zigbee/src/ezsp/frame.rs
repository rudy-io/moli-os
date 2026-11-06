//! EZSP frame layout (UG100): the extended format every command uses, and
//! the legacy format of the very first `version` exchange.
//!
//! Extended: `seq`, frame control low byte, frame control high byte, frame
//! ID (16 bits, little-endian), parameters. Legacy: `seq`, frame control,
//! frame ID (8 bits), parameters.

use std::fmt;

use super::ids;

// Frame control bits, transcribed from zigbee-herdsman (MIT),
// `src/adapter/ember/ezsp/consts.ts`.
/// Low byte: set in a frame from the NCP that answers a command or carries a callback.
const RESPONSE: u8 = 0x80;
/// Low byte: the NCP ran out of memory since the previous response.
const OVERFLOW: u8 = 0x01;
/// Low byte: the response did not fit and was cut.
const TRUNCATED: u8 = 0x02;
/// Low byte: callback type, synchronous (`0x08`) or asynchronous (`0x10`).
const CALLBACK_TYPE: u8 = 0x18;
/// High byte: frame format version; `0x01` is the extended format.
const FORMAT_VERSION_MASK: u8 = 0x03;
const FORMAT_VERSION: u8 = 0x01;
/// High byte: bits a host must reject (neither security, padding nor version).
const RESERVED_MASK: u8 = 0x3c;

/// Why an EZSP frame or its parameters could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EzspError {
    /// The frame, or one of its parameters, ends early.
    TooShort,
    /// The extended frame control high byte uses reserved bits.
    UnsupportedControl(u8),
}

impl fmt::Display for EzspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => f.write_str("EZSP frame too short"),
            Self::UnsupportedControl(byte) => {
                write!(f, "unsupported EZSP frame control {byte:#04x}")
            }
        }
    }
}

impl std::error::Error for EzspError {}

/// What `decode` reads before the parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // independent frame control bits
pub struct Header {
    pub seq: u8,
    /// From the NCP (a response or a callback), not a command.
    pub is_response: bool,
    /// A callback rather than the answer to a command.
    pub callback: bool,
    /// The NCP ran out of memory; the frame itself is still valid.
    pub overflow: bool,
    /// The response was cut: its parameters are incomplete.
    pub truncated: bool,
    pub id: u16,
}

/// The `version` command in the legacy format, the only one an NCP of
/// unknown version is sure to understand.
#[must_use]
pub fn legacy_version_request(seq: u8, desired: u8) -> Vec<u8> {
    let [id, _] = ids::VERSION.to_le_bytes();
    vec![seq, 0x00, id, desired]
}

/// A command in the extended format.
#[must_use]
pub fn encode(seq: u8, id: u16, params: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + params.len());
    frame.extend_from_slice(&[seq, 0x00, FORMAT_VERSION]);
    frame.extend_from_slice(&id.to_le_bytes());
    frame.extend_from_slice(params);
    frame
}

/// Splits a frame from the NCP into its header and parameters. Takes both
/// formats, told apart by the third byte as herdsman does: the extended one,
/// and the legacy one of the first `version` response (frame ID `0x00`).
pub fn decode(bytes: &[u8]) -> Result<(Header, &[u8]), EzspError> {
    let [seq, control, third, rest @ ..] = bytes else {
        return Err(EzspError::TooShort);
    };
    let (id, params) = if third & FORMAT_VERSION_MASK == FORMAT_VERSION {
        if third & RESERVED_MASK != 0 {
            return Err(EzspError::UnsupportedControl(*third));
        }
        let [low, high, params @ ..] = rest else {
            return Err(EzspError::TooShort);
        };
        (u16::from_le_bytes([*low, *high]), params)
    } else {
        (u16::from(*third), rest)
    };
    let header = Header {
        seq: *seq,
        is_response: control & RESPONSE != 0,
        callback: control & CALLBACK_TYPE != 0,
        overflow: control & OVERFLOW != 0,
        truncated: control & TRUNCATED != 0,
        id,
    };
    Ok((header, params))
}

/// The parameters of a `version` response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    /// EZSP protocol version the NCP speaks.
    pub protocol: u8,
    /// Stack type (2 for a Zigbee stack).
    pub stack_type: u8,
    /// Firmware version, one nibble per component.
    pub stack_version: u16,
}

impl Version {
    pub fn parse(params: &[u8]) -> Result<Self, EzspError> {
        let [protocol, stack_type, low, high, ..] = *params else {
            return Err(EzspError::TooShort);
        };
        Ok(Self {
            protocol,
            stack_type,
            stack_version: u16::from_le_bytes([low, high]),
        })
    }

    /// The firmware version as Silicon Labs writes it (`0x7440` -> `"7.4.4.0"`).
    #[must_use]
    pub fn display(&self) -> String {
        let v = self.stack_version;
        format!(
            "{}.{}.{}.{}",
            v >> 12,
            (v >> 8) & 0xF,
            (v >> 4) & 0xF,
            v & 0xF
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_version_bytes() {
        assert_eq!(legacy_version_request(0, 13), [0x00, 0x00, 0x00, 0x0D]);
        assert_eq!(legacy_version_request(7, 14), [0x07, 0x00, 0x00, 0x0E]);
    }

    #[test]
    fn extended_header() {
        assert_eq!(
            encode(5, 0x0052, &[0x01]),
            [5, 0x00, 0x01, 0x52, 0x00, 0x01]
        );
        // A 16-bit ID goes out little-endian; no parameters is fine.
        assert_eq!(encode(9, ids::EXPORT_KEY, &[]), [9, 0x00, 0x01, 0x14, 0x01]);
    }

    #[test]
    fn decode_response_flags() {
        let frame = [5, 0x80, 0x01, 0x52, 0x00, 0x00, 0x34, 0x12];
        let (header, params) = decode(&frame).unwrap();
        assert_eq!(
            header,
            Header {
                seq: 5,
                is_response: true,
                callback: false,
                overflow: false,
                truncated: false,
                id: ids::GET_CONFIGURATION_VALUE,
            }
        );
        assert_eq!(params, [0x00, 0x34, 0x12]);

        // Our own command reads back as a command.
        let command = encode(5, 0x0052, &[0x01]);
        let (header, params) = decode(&command).unwrap();
        assert!(!header.is_response);
        assert_eq!(params, [0x01]);

        let (header, _) = decode(&[1, 0x80 | 0x01 | 0x02, 0x01, 0x28, 0x00]).unwrap();
        assert!(header.overflow);
        assert!(header.truncated);
        assert!(!header.callback);
    }

    #[test]
    fn decode_callback() {
        // stackStatusHandler(NETWORK_UP), asynchronous callback (0x10).
        let frame = [3, 0x90, 0x01, 0x19, 0x00, 0x90];
        let (header, params) = decode(&frame).unwrap();
        assert!(header.is_response);
        assert!(header.callback);
        assert_eq!(header.id, ids::STACK_STATUS_HANDLER);
        assert_eq!(params, [0x90]);
        // Synchronous callback type (0x08) is a callback too.
        assert!(decode(&[3, 0x88, 0x01, 0x19, 0x00]).unwrap().0.callback);
    }

    #[test]
    fn decode_legacy_version_response() {
        let frame = [0, 0x80, 0x00, 0x0D, 0x02, 0x40, 0x74];
        let (header, params) = decode(&frame).unwrap();
        assert_eq!(header.id, ids::VERSION);
        assert!(header.is_response);
        assert_eq!(params, [0x0D, 0x02, 0x40, 0x74]);
    }

    #[test]
    fn decode_rejects_short_and_reserved() {
        assert_eq!(decode(&[]), Err(EzspError::TooShort));
        assert_eq!(decode(&[0, 0x80]), Err(EzspError::TooShort));
        // Extended format announced, frame ID missing its high byte.
        assert_eq!(decode(&[0, 0x80, 0x01, 0x52]), Err(EzspError::TooShort));
        assert_eq!(
            decode(&[0, 0x80, 0x05, 0x52, 0x00]),
            Err(EzspError::UnsupportedControl(0x05))
        );
        // Security and padding bits are not reserved.
        assert!(decode(&[0, 0x80, 0xC1, 0x52, 0x00]).is_ok());
    }

    #[test]
    fn version_parse() {
        let v = Version::parse(&[0x0D, 0x02, 0x40, 0x74]).unwrap();
        assert_eq!(
            v,
            Version {
                protocol: 13,
                stack_type: 2,
                stack_version: 0x7440,
            }
        );
        assert_eq!(v.display(), "7.4.4.0");
        assert_eq!(
            Version::parse(&[0x0D, 0x02, 0x40]),
            Err(EzspError::TooShort)
        );
    }
}
