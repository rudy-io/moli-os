//! ZCL (Zigbee Cluster Library): frame headers, data types, the general
//! commands and the cluster commands the generic devices need. No IO.
//!
//! Layouts from the ZCL specification (CSA 07-5123) as zigbee-herdsman (MIT)
//! implements them: `src/zspec/zcl/zclHeader.ts`, `zclFrame.ts`,
//! `buffaloZcl.ts`, `definition/cluster.ts`; integers little-endian.

use std::fmt;

/// `ZclFrame`'s profile for every request: Home Automation, also what ZLL
/// lamps (profile `0xC05E` in their descriptors) take.
pub const HA_PROFILE: u16 = 0x0104;
/// Zigbee Light Link: a profile lamps declare, spoken in ZCL as Home Automation.
pub const ZLL_PROFILE: u16 = 0xC05E;

pub const BASIC: u16 = 0x0000;
pub const POWER_CONFIG: u16 = 0x0001;
pub const ON_OFF: u16 = 0x0006;
pub const LEVEL: u16 = 0x0008;
pub const THERMOSTAT: u16 = 0x0201;
pub const COLOR: u16 = 0x0300;
pub const ILLUMINANCE: u16 = 0x0400;
pub const TEMPERATURE: u16 = 0x0402;
pub const PRESSURE: u16 = 0x0403;
pub const HUMIDITY: u16 = 0x0405;
pub const OCCUPANCY: u16 = 0x0406;
pub const IAS_ZONE: u16 = 0x0500;
pub const METERING: u16 = 0x0702;
pub const ELECTRICAL: u16 = 0x0B04;

// General commands.
const READ_ATTRIBUTES: u8 = 0x00;
pub const READ_RESPONSE: u8 = 0x01;
const WRITE_ATTRIBUTES: u8 = 0x02;
pub const WRITE_RESPONSE: u8 = 0x04;
const CONFIGURE_REPORTING: u8 = 0x06;
pub const CONFIGURE_RESPONSE: u8 = 0x07;
pub const REPORT_ATTRIBUTES: u8 = 0x0A;
pub const DEFAULT_RESPONSE: u8 = 0x0B;

// Cluster commands of the IAS Zone server, towards the coordinator.
pub const ZONE_STATUS_CHANGE: u8 = 0x00;
pub const ZONE_ENROLL_REQUEST: u8 = 0x01;

pub const STATUS_SUCCESS: u8 = 0x00;

// Data types the requests write.
pub const BITMAP8: u8 = 0x18;
pub const BOOLEAN: u8 = 0x10;
pub const UINT8: u8 = 0x20;
pub const UINT16: u8 = 0x21;
pub const INT16: u8 = 0x29;
pub const INT24: u8 = 0x2A;
pub const UINT48: u8 = 0x25;
pub const ENUM8: u8 = 0x30;
pub const IEEE_ADDR: u8 = 0xF0;

// Frame control bits.
const TYPE_SPECIFIC: u8 = 0x01;
const MANUFACTURER_SPECIFIC: u8 = 0x04;
const SERVER_TO_CLIENT: u8 = 0x08;
const DISABLE_DEFAULT_RESPONSE: u8 = 0x10;

/// Why a ZCL frame could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZclError {
    TooShort,
}

impl fmt::Display for ZclError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ZCL frame too short")
    }
}

impl std::error::Error for ZclError {}

/// What starts every ZCL frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// A command of the cluster, not a general one.
    pub specific: bool,
    pub manufacturer: Option<u16>,
    /// Sent by the cluster's server (a report, a response).
    pub from_server: bool,
    /// The sender wants no default response.
    pub no_default_response: bool,
    pub seq: u8,
    pub command: u8,
}

impl Header {
    /// Reads the header at the start of `frame`; returns what follows.
    ///
    /// # Errors
    ///
    /// The frame is too short.
    pub fn parse(frame: &[u8]) -> Result<(Self, &[u8]), ZclError> {
        let (&control, rest) = frame.split_first().ok_or(ZclError::TooShort)?;
        let (manufacturer, rest) = if control & MANUFACTURER_SPECIFIC == 0 {
            (None, rest)
        } else {
            let (code, rest) = rest.split_first_chunk::<2>().ok_or(ZclError::TooShort)?;
            (Some(u16::from_le_bytes(*code)), rest)
        };
        let (&[seq, command], rest) = rest.split_first_chunk::<2>().ok_or(ZclError::TooShort)?;
        Ok((
            Self {
                specific: control & 0x03 == TYPE_SPECIFIC,
                manufacturer,
                from_server: control & SERVER_TO_CLIENT != 0,
                no_default_response: control & DISABLE_DEFAULT_RESPONSE != 0,
                seq,
                command,
            },
            rest,
        ))
    }
}

/// An attribute's value. ZCL's « no value » of a type (`0xFF` for a `uint8`,
/// `0x8000` for an `int16`…) is [`Data::Invalid`]: a sensor saying it has
/// nothing to say, not a reading.
#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    Bool(bool),
    Uint(u64),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Invalid,
}

impl Data {
    /// As an unsigned integer (bitmaps and enums read as such).
    #[must_use]
    pub fn as_uint(&self) -> Option<u64> {
        match *self {
            Self::Uint(value) => Some(value),
            Self::Int(value) => u64::try_from(value).ok(),
            Self::Bool(value) => Some(u64::from(value)),
            _ => None,
        }
    }

    /// As a number.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // ZCL readings stay far below 2^52.
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::Uint(value) => Some(value as f64),
            Self::Int(value) => Some(value as f64),
            Self::Float(value) => Some(value),
            _ => None,
        }
    }
}

/// How many bytes a fixed-size type takes.
fn width(data_type: u8) -> Option<usize> {
    let bytes = match data_type {
        0x08..=0x0F => data_type - 0x07,
        0x10 | 0x30 => 1,
        0x18..=0x1F => data_type - 0x17,
        0x20..=0x27 => data_type - 0x1F,
        0x28..=0x2F => data_type - 0x27,
        0x31 | 0x38 | 0xE8 | 0xE9 => 2,
        0x39 | 0xE0 | 0xE1 | 0xE2 | 0xEA => 4,
        0x3A | 0xF0 => 8,
        0xF1 => 16,
        _ => return None,
    };
    Some(usize::from(bytes))
}

/// Up to 8 little-endian bytes.
fn little_endian(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .rev()
        .fold(0, |value, &byte| (value << 8) | u64::from(byte))
}

/// Reads one value of `data_type`; returns what follows. `None` when it does
/// not fit or the type is not read (arrays, structures, sets: nothing the
/// generic devices use, and no way to step over them cheaply).
#[must_use]
pub fn read_value(data_type: u8, bytes: &[u8]) -> Option<(Data, &[u8])> {
    match data_type {
        0x00 => return Some((Data::Invalid, bytes)),
        0x41..=0x44 => return read_string(data_type, bytes),
        _ => {}
    }
    let len = width(data_type)?;
    let (raw, rest) = bytes.split_at_checked(len)?;
    let data = match data_type {
        0x10 => match raw[0] {
            0xFF => Data::Invalid,
            value => Data::Bool(value != 0),
        },
        0x20..=0x27 => {
            let value = little_endian(raw);
            if value == u64::MAX >> (64 - 8 * len) {
                Data::Invalid
            } else {
                Data::Uint(value)
            }
        }
        0x28..=0x2F => {
            let shift = 64 - 8 * len;
            #[allow(clippy::cast_possible_wrap)] // Sign-extended by the shift back.
            let value = ((little_endian(raw) << shift) as i64) >> shift;
            if value == i64::MIN >> shift {
                Data::Invalid
            } else {
                Data::Int(value)
            }
        }
        0x38 => Data::Invalid,
        0x39 => float(f64::from(f32::from_le_bytes(raw.try_into().ok()?))),
        0x3A => float(f64::from_le_bytes(raw.try_into().ok()?)),
        0xF1 => Data::Bytes(raw.to_vec()),
        _ => Data::Uint(little_endian(raw)),
    };
    Some((data, rest))
}

fn float(value: f64) -> Data {
    if value.is_nan() {
        Data::Invalid
    } else {
        Data::Float(value)
    }
}

/// An octet or character string, short (`0x41`, `0x42`) or long (`0x43`,
/// `0x44`): its length first, all ones when it has no value.
fn read_string(data_type: u8, bytes: &[u8]) -> Option<(Data, &[u8])> {
    let (len, rest) = if data_type <= 0x42 {
        let (&len, rest) = bytes.split_first()?;
        (usize::from(len), rest)
    } else {
        let (len, rest) = bytes.split_first_chunk::<2>()?;
        (usize::from(u16::from_le_bytes(*len)), rest)
    };
    let none = if data_type <= 0x42 { 0xFF } else { 0xFFFF };
    if len == none {
        return Some((Data::Invalid, rest));
    }
    let (raw, rest) = rest.split_at_checked(len)?;
    let data = if matches!(data_type, 0x41 | 0x43) {
        Data::Bytes(raw.to_vec())
    } else {
        Data::Text(
            String::from_utf8_lossy(raw)
                .trim_end_matches('\0')
                .to_owned(),
        )
    };
    Some((data, rest))
}

/// One attribute of a report.
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    pub id: u16,
    pub value: Data,
}

/// What a report says: the records up to the first one that cannot be read.
#[must_use]
pub fn report_attributes(mut payload: &[u8]) -> Vec<Attribute> {
    let mut attributes = Vec::new();
    while let Some((&[id0, id1, data_type], rest)) = payload.split_first_chunk::<3>() {
        let Some((value, rest)) = read_value(data_type, rest) else {
            break;
        };
        attributes.push(Attribute {
            id: u16::from_le_bytes([id0, id1]),
            value,
        });
        payload = rest;
    }
    attributes
}

/// What a read attributes response says: the attributes that answered (up to
/// the first record that cannot be read), and how many were refused.
#[must_use]
pub fn read_response(mut payload: &[u8]) -> (Vec<Attribute>, usize) {
    let mut attributes = Vec::new();
    let mut refused = 0;
    while let Some((&[id0, id1, status], rest)) = payload.split_first_chunk::<3>() {
        if status != STATUS_SUCCESS {
            refused += 1;
            payload = rest;
            continue;
        }
        let Some((&data_type, rest)) = rest.split_first() else {
            break;
        };
        let Some((value, rest)) = read_value(data_type, rest) else {
            break;
        };
        attributes.push(Attribute {
            id: u16::from_le_bytes([id0, id1]),
            value,
        });
        payload = rest;
    }
    (attributes, refused)
}

/// A default response: the command it answers, and its status.
#[must_use]
pub fn default_response(payload: &[u8]) -> Option<(u8, u8)> {
    match *payload {
        [command, status, ..] => Some((command, status)),
        _ => None,
    }
}

/// Whether a configure reporting or write attributes response is all good:
/// one `SUCCESS`, or one record per failed attribute otherwise.
#[must_use]
pub fn all_succeeded(payload: &[u8]) -> bool {
    payload.first() == Some(&STATUS_SUCCESS)
}

/// A zone status change notification: the zone status.
#[must_use]
pub fn zone_status_change(payload: &[u8]) -> Option<u16> {
    payload
        .first_chunk::<2>()
        .map(|raw| u16::from_le_bytes(*raw))
}

/// A zone enroll request: the zone type.
#[must_use]
pub fn enroll_request(payload: &[u8]) -> Option<u16> {
    payload
        .first_chunk::<2>()
        .map(|raw| u16::from_le_bytes(*raw))
}

/// A frame to send, but for its transaction number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub specific: bool,
    pub no_default_response: bool,
    pub command: u8,
    pub payload: Vec<u8>,
}

impl Request {
    /// The frame, client to server, with transaction number `seq`.
    #[must_use]
    pub fn frame(&self, seq: u8) -> Vec<u8> {
        let mut control = 0;
        if self.specific {
            control |= TYPE_SPECIFIC;
        }
        if self.no_default_response {
            control |= DISABLE_DEFAULT_RESPONSE;
        }
        let mut frame = Vec::with_capacity(3 + self.payload.len());
        frame.extend_from_slice(&[control, seq, self.command]);
        frame.extend_from_slice(&self.payload);
        frame
    }

    fn general(command: u8, payload: Vec<u8>) -> Self {
        Self {
            specific: false,
            no_default_response: false,
            command,
            payload,
        }
    }

    fn cluster(command: u8, payload: Vec<u8>) -> Self {
        Self {
            specific: true,
            no_default_response: false,
            command,
            payload,
        }
    }
}

/// Read attributes.
#[must_use]
pub fn read_attributes(attributes: &[u16]) -> Request {
    let payload = attributes.iter().flat_map(|id| id.to_le_bytes()).collect();
    Request::general(READ_ATTRIBUTES, payload)
}

/// Write one attribute; `value` as it travels, little-endian.
#[must_use]
pub fn write_attribute(attribute: u16, data_type: u8, value: &[u8]) -> Request {
    let mut payload = attribute.to_le_bytes().to_vec();
    payload.push(data_type);
    payload.extend_from_slice(value);
    Request::general(WRITE_ATTRIBUTES, payload)
}

/// One attribute's reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub attribute: u16,
    pub data_type: u8,
    pub min: u16,
    pub max: u16,
    /// The change that triggers a report, for an analog type; discrete types
    /// (booleans, bitmaps, enums) have none on the wire.
    pub change: Option<u64>,
}

/// Configure reporting, towards the coordinator (direction 0).
#[must_use]
pub fn configure_reporting(reports: &[Report]) -> Request {
    let mut payload = Vec::with_capacity(reports.len() * 12);
    for report in reports {
        payload.push(0);
        payload.extend_from_slice(&report.attribute.to_le_bytes());
        payload.push(report.data_type);
        payload.extend_from_slice(&report.min.to_le_bytes());
        payload.extend_from_slice(&report.max.to_le_bytes());
        if let Some(change) = report.change {
            let len = width(report.data_type).unwrap_or(0).min(8);
            payload.extend_from_slice(&change.to_le_bytes()[..len]);
        }
    }
    Request::general(CONFIGURE_REPORTING, payload)
}

/// On/Off cluster: `Off` (0) or `On` (1).
#[must_use]
pub fn on_off(on: bool) -> Request {
    Request::cluster(u8::from(on), Vec::new())
}

/// Level Control: `moveToLevelWithOnOff`, options left to the device.
#[must_use]
pub fn move_to_level_with_on_off(level: u8, tenths: u16) -> Request {
    let [t0, t1] = tenths.to_le_bytes();
    Request::cluster(0x04, vec![level, t0, t1, 0, 0])
}

/// Color Control: `moveToColor` (xy, each as 0..=65279 of 1), options left to
/// the device.
#[must_use]
pub fn move_to_color(x: u16, y: u16, tenths: u16) -> Request {
    let mut payload = x.to_le_bytes().to_vec();
    payload.extend_from_slice(&y.to_le_bytes());
    payload.extend_from_slice(&tenths.to_le_bytes());
    payload.extend_from_slice(&[0, 0]);
    Request::cluster(0x07, payload)
}

/// Color Control: `moveToColorTemp`, in mired, options left to the device.
#[must_use]
pub fn move_to_color_temp(mired: u16, tenths: u16) -> Request {
    let mut payload = mired.to_le_bytes().to_vec();
    payload.extend_from_slice(&tenths.to_le_bytes());
    payload.extend_from_slice(&[0, 0]);
    Request::cluster(0x0A, payload)
}

/// IAS Zone: enroll response, success; no default response (herdsman's).
#[must_use]
pub fn enroll_response(zone: u8) -> Request {
    Request {
        no_default_response: true,
        ..Request::cluster(0x00, vec![0x00, zone])
    }
}

/// The default response to a frame received from a device: the other way
/// than it came, same transaction number, none expected in return.
#[must_use]
pub fn default_response_frame(received: &Header, status: u8) -> Vec<u8> {
    let mut control = DISABLE_DEFAULT_RESPONSE;
    if !received.from_server {
        control |= SERVER_TO_CLIENT;
    }
    vec![
        control,
        received.seq,
        DEFAULT_RESPONSE,
        received.command,
        status,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers() {
        // A report from the server: general, no default response wanted.
        let (header, rest) = Header::parse(&[0x18, 0x2A, 0x0A, 0xAB]).unwrap();
        assert_eq!(
            header,
            Header {
                specific: false,
                manufacturer: None,
                from_server: true,
                no_default_response: true,
                seq: 0x2A,
                command: REPORT_ATTRIBUTES,
            }
        );
        assert_eq!(rest, [0xAB]);
        // A manufacturer's cluster command.
        let (header, rest) = Header::parse(&[0x05, 0x34, 0x12, 0x07, 0x01]).unwrap();
        assert!(header.specific);
        assert_eq!(header.manufacturer, Some(0x1234));
        assert!(!header.from_server);
        assert_eq!((header.seq, header.command), (0x07, 0x01));
        assert!(rest.is_empty());
    }

    #[test]
    fn truncated_frames_are_refused() {
        assert_eq!(Header::parse(&[]), Err(ZclError::TooShort));
        assert_eq!(Header::parse(&[0x18, 0x01]), Err(ZclError::TooShort));
        // Manufacturer specific, its code cut.
        assert_eq!(Header::parse(&[0x04, 0x34]), Err(ZclError::TooShort));
        // A record cut in the middle: what came before stays.
        let mut report = vec![0x00, 0x00, 0x10, 0x01];
        report.extend_from_slice(&[0x01, 0x00, 0x21, 0x34]);
        assert_eq!(
            report_attributes(&report),
            [Attribute {
                id: 0,
                value: Data::Bool(true)
            }]
        );
        assert_eq!(read_response(&[0x00]), (Vec::new(), 0));
        assert_eq!(default_response(&[0x01]), None);
        assert_eq!(zone_status_change(&[0x01]), None);
        assert_eq!(enroll_request(&[]), None);
    }

    fn value(data_type: u8, bytes: &[u8]) -> Data {
        let (data, rest) = read_value(data_type, bytes).unwrap();
        assert!(rest.is_empty(), "{data_type:#x}: {rest:?} left");
        data
    }

    #[test]
    fn data_types() {
        assert_eq!(value(BOOLEAN, &[0x01]), Data::Bool(true));
        assert_eq!(value(BOOLEAN, &[0x00]), Data::Bool(false));
        assert_eq!(value(UINT8, &[0xFE]), Data::Uint(254));
        assert_eq!(value(UINT16, &[0x34, 0x12]), Data::Uint(0x1234));
        assert_eq!(
            value(UINT48, &[1, 0, 0, 0, 0, 1]),
            Data::Uint(0x0100_0000_0001)
        );
        assert_eq!(value(INT16, &[0x2E, 0x08]), Data::Int(2094));
        assert_eq!(value(INT16, &[0xF6, 0xFF]), Data::Int(-10));
        assert_eq!(value(INT24, &[0xFF, 0xFF, 0xFF]), Data::Int(-1));
        assert_eq!(
            value(BITMAP8, &[0xFF]),
            Data::Uint(0xFF),
            "no invalid bitmap"
        );
        assert_eq!(
            value(ENUM8, &[0xFF]),
            Data::Uint(0xFF),
            "startUpOnOff: previous"
        );
        assert_eq!(value(0x39, &1.5f32.to_le_bytes()), Data::Float(1.5));
        assert_eq!(value(0x42, b"\x03abc"), Data::Text("abc".into()));
        assert_eq!(value(0x42, b"\x04abc\0"), Data::Text("abc".into()));
        assert_eq!(
            value(0x41, &[0x02, 0xAA, 0xBB]),
            Data::Bytes(vec![0xAA, 0xBB])
        );
        assert_eq!(value(0x44, b"\x02\x00ab"), Data::Text("ab".into()));
        assert_eq!(value(IEEE_ADDR, &[1, 0, 0, 0, 0, 0, 0, 0]), Data::Uint(1));
    }

    #[test]
    fn no_value_is_invalid() {
        assert_eq!(value(BOOLEAN, &[0xFF]), Data::Invalid);
        assert_eq!(value(UINT8, &[0xFF]), Data::Invalid);
        assert_eq!(value(UINT16, &[0xFF, 0xFF]), Data::Invalid);
        assert_eq!(value(INT16, &[0x00, 0x80]), Data::Invalid);
        assert_eq!(value(INT24, &[0x00, 0x00, 0x80]), Data::Invalid);
        assert_eq!(value(0x42, &[0xFF]), Data::Invalid);
        assert_eq!(value(0x39, &f32::NAN.to_le_bytes()), Data::Invalid);
        // Not read at all: too short, or a list.
        assert_eq!(read_value(UINT16, &[0x01]), None);
        assert_eq!(read_value(0x42, &[0x05, 0x61]), None);
        assert_eq!(read_value(0x48, &[0x20, 0x00, 0x00]), None);
    }

    #[test]
    fn reports_and_read_responses() {
        // Two attributes: on/off true, level 254.
        let report = [0x00, 0x00, 0x10, 0x01, 0x11, 0x00, 0x20, 0xFE];
        assert_eq!(
            report_attributes(&report),
            [
                Attribute {
                    id: 0x0000,
                    value: Data::Bool(true)
                },
                Attribute {
                    id: 0x0011,
                    value: Data::Uint(254)
                },
            ]
        );
        // The model, then an attribute refused, then the manufacturer.
        let mut response = vec![0x05, 0x00, 0x00, 0x42, 0x01, b'M'];
        response.extend_from_slice(&[0x07, 0x00, 0x86]);
        response.extend_from_slice(&[0x04, 0x00, 0x00, 0x42, 0x01, b'F']);
        let (attributes, refused) = read_response(&response);
        assert_eq!(refused, 1);
        assert_eq!(
            attributes,
            [
                Attribute {
                    id: 5,
                    value: Data::Text("M".into())
                },
                Attribute {
                    id: 4,
                    value: Data::Text("F".into())
                },
            ]
        );
    }

    #[test]
    fn responses() {
        assert_eq!(default_response(&[0x01, 0x86]), Some((0x01, 0x86)));
        assert!(all_succeeded(&[0x00]));
        assert!(!all_succeeded(&[0x86, 0x00, 0x00, 0x00]));
        assert!(!all_succeeded(&[]));
        assert_eq!(zone_status_change(&[0x31, 0x00, 0x00, 0x01]), Some(0x0031));
        assert_eq!(enroll_request(&[0x15, 0x00, 0x34, 0x12]), Some(0x0015));
    }

    #[test]
    fn requests() {
        assert_eq!(
            read_attributes(&[0x0005, 0x4000]).frame(7),
            [0x00, 7, 0x00, 0x05, 0x00, 0x00, 0x40]
        );
        assert_eq!(on_off(true).frame(1), [0x01, 1, 0x01]);
        assert_eq!(on_off(false).frame(2), [0x01, 2, 0x00]);
        assert_eq!(
            move_to_level_with_on_off(127, 0).frame(3),
            [0x01, 3, 0x04, 127, 0, 0, 0, 0]
        );
        assert_eq!(
            move_to_color(0x1234, 0x5678, 2).frame(4),
            [0x01, 4, 0x07, 0x34, 0x12, 0x78, 0x56, 2, 0, 0, 0]
        );
        assert_eq!(
            move_to_color_temp(370, 0).frame(5),
            [0x01, 5, 0x0A, 0x72, 0x01, 0, 0, 0, 0]
        );
        assert_eq!(enroll_response(23).frame(6), [0x11, 6, 0x00, 0x00, 23]);
        assert_eq!(
            write_attribute(0x0010, IEEE_ADDR, &[1, 2, 3, 4, 5, 6, 7, 8]).frame(9),
            [0x00, 9, 0x02, 0x10, 0x00, 0xF0, 1, 2, 3, 4, 5, 6, 7, 8]
        );
    }

    #[test]
    fn reporting_requests() {
        let request = configure_reporting(&[
            Report {
                attribute: 0x0000,
                data_type: BOOLEAN,
                min: 0,
                max: 3600,
                change: None,
            },
            Report {
                attribute: 0x0000,
                data_type: INT16,
                min: 10,
                max: 3600,
                change: Some(100),
            },
            Report {
                attribute: 0x0000,
                data_type: UINT48,
                min: 5,
                max: 3600,
                change: Some(257),
            },
        ]);
        assert_eq!(
            request.frame(8),
            [
                0x00, 8, 0x06, // header
                0, 0x00, 0x00, 0x10, 0x00, 0x00, 0x10, 0x0E, // boolean: no change
                0, 0x00, 0x00, 0x29, 0x0A, 0x00, 0x10, 0x0E, 0x64, 0x00, // int16
                0, 0x00, 0x00, 0x25, 0x05, 0x00, 0x10, 0x0E, 0x01, 0x01, 0, 0, 0, 0, // uint48
            ]
        );
    }

    #[test]
    fn default_responses_go_the_other_way() {
        let (report, _) = Header::parse(&[0x08, 0x2A, 0x0A]).unwrap();
        assert_eq!(
            default_response_frame(&report, STATUS_SUCCESS),
            [0x10, 0x2A, 0x0B, 0x0A, 0x00]
        );
        let (command, _) = Header::parse(&[0x01, 0x2B, 0x02]).unwrap();
        assert_eq!(
            default_response_frame(&command, 0x81),
            [0x18, 0x2B, 0x0B, 0x02, 0x81]
        );
    }
}
