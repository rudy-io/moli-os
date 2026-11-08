//! The LIFX LAN protocol: a 36-byte header (frame, frame address,
//! protocol header, all little-endian) followed by a message's payload.
//! Written from LIFX's public LAN protocol documentation.

/// Where every LIFX device listens.
pub const PORT: u16 = 56700;
const HEADER: usize = 36;
/// Protocol number, `addressable` bit set: what every message carries.
const PROTOCOL: u16 = 1024 | (1 << 12);
const TAGGED: u16 = 1 << 13;

pub const GET_SERVICE: u16 = 2;
pub const STATE_SERVICE: u16 = 3;
pub const GET_HOST_FIRMWARE: u16 = 14;
pub const STATE_HOST_FIRMWARE: u16 = 15;
pub const GET_WIFI_INFO: u16 = 16;
pub const STATE_WIFI_INFO: u16 = 17;
pub const GET_VERSION: u16 = 32;
pub const STATE_VERSION: u16 = 33;
pub const ACKNOWLEDGEMENT: u16 = 45;
pub const GET_GROUP: u16 = 51;
pub const STATE_GROUP: u16 = 53;
pub const LIGHT_GET: u16 = 101;
pub const LIGHT_SET_COLOR: u16 = 102;
pub const LIGHT_STATE: u16 = 107;
pub const LIGHT_SET_POWER: u16 = 117;

/// A device's serial number: the six first bytes of a frame's target (its
/// Wi-Fi MAC minus one).
pub type Serial = [u8; 6];

/// One message, header decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub source: u32,
    pub target: Serial,
    pub sequence: u8,
    pub kind: u16,
    pub payload: Vec<u8>,
}

/// What an outgoing message asks of the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// A `State…` answer.
    Response,
    /// An `Acknowledgement` (orders).
    Ack,
}

/// Encodes a message. `target`: `None` reaches every device (tagged), as
/// discovery does.
#[must_use]
pub fn encode(
    source: u32,
    target: Option<Serial>,
    sequence: u8,
    kind: u16,
    payload: &[u8],
    reply: Reply,
) -> Vec<u8> {
    let size = u16::try_from(HEADER + payload.len()).unwrap_or(u16::MAX);
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(&size.to_le_bytes());
    let flags = PROTOCOL | if target.is_none() { TAGGED } else { 0 };
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&source.to_le_bytes());
    let mut address = [0u8; 8];
    if let Some(serial) = target {
        address[..6].copy_from_slice(&serial);
    }
    out.extend_from_slice(&address);
    out.extend_from_slice(&[0u8; 6]);
    out.push(match reply {
        Reply::Response => 0b01,
        Reply::Ack => 0b10,
    });
    out.push(sequence);
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&[0u8; 2]);
    out.extend_from_slice(payload);
    out
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Decodes a datagram; `None` if it is not a whole LIFX message.
#[must_use]
pub fn decode(bytes: &[u8]) -> Option<Message> {
    let size = usize::from(u16_at(bytes, 0)?);
    if size < HEADER || size > bytes.len() {
        return None;
    }
    if u16_at(bytes, 2)? & 0x0FFF != 1024 {
        return None;
    }
    let mut target = [0u8; 6];
    target.copy_from_slice(bytes.get(8..14)?);
    Some(Message {
        source: u32_at(bytes, 4)?,
        target,
        sequence: *bytes.get(23)?,
        kind: u16_at(bytes, 32)?,
        payload: bytes.get(HEADER..size)?.to_vec(),
    })
}

/// A 32-byte, NUL-padded UTF-8 label.
fn label(bytes: &[u8]) -> Option<String> {
    let raw = bytes.get(..32)?;
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let text = String::from_utf8_lossy(&raw[..end]).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// `StateService`: the UDP port, if the device offers the UDP service.
#[must_use]
pub fn udp_port(payload: &[u8]) -> Option<u16> {
    if *payload.first()? != 1 {
        return None;
    }
    u16::try_from(u32_at(payload, 1)?).ok()
}

/// `StateHostFirmware`: (major, minor).
#[must_use]
pub fn firmware(payload: &[u8]) -> Option<(u16, u16)> {
    Some((u16_at(payload, 18)?, u16_at(payload, 16)?))
}

/// `StateVersion`: (vendor, product).
#[must_use]
pub fn version(payload: &[u8]) -> Option<(u32, u32)> {
    Some((u32_at(payload, 0)?, u32_at(payload, 4)?))
}

/// `StateGroup`: the group's name (the room in the LIFX app).
#[must_use]
pub fn group(payload: &[u8]) -> Option<String> {
    label(payload.get(16..)?)
}

/// `StateWifiInfo`: the signal in dBm, as LIFX documents it
/// (`floor(10·log10(signal) + 0.5)`); `None` without a signal.
#[must_use]
pub fn rssi(payload: &[u8]) -> Option<i64> {
    let signal = f32::from_le_bytes(payload.get(..4)?.try_into().ok()?);
    if !signal.is_finite() || signal <= 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some((10.0 * f64::from(signal).log10() + 0.5).floor() as i64)
}

/// A light's colour as LIFX keeps it: hue, saturation, brightness on
/// 0-65535, temperature in kelvins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hsbk {
    pub hue: u16,
    pub saturation: u16,
    pub brightness: u16,
    pub kelvin: u16,
}

/// `LightState`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightState {
    pub color: Hsbk,
    pub on: bool,
    pub label: Option<String>,
}

#[must_use]
pub fn light_state(payload: &[u8]) -> Option<LightState> {
    if payload.len() < 44 {
        return None;
    }
    Some(LightState {
        color: Hsbk {
            hue: u16_at(payload, 0)?,
            saturation: u16_at(payload, 2)?,
            brightness: u16_at(payload, 4)?,
            kelvin: u16_at(payload, 6)?,
        },
        on: u16_at(payload, 10)? != 0,
        label: label(payload.get(12..)?),
    })
}

/// `LightSetColor`'s payload.
#[must_use]
pub fn set_color(color: Hsbk, duration_ms: u32) -> Vec<u8> {
    let mut out = vec![0u8];
    for v in [color.hue, color.saturation, color.brightness, color.kelvin] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&duration_ms.to_le_bytes());
    out
}

/// `LightSetPower`'s payload.
#[must_use]
pub fn set_power(on: bool, duration_ms: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(6);
    out.extend_from_slice(&(if on { u16::MAX } else { 0 }).to_le_bytes());
    out.extend_from_slice(&duration_ms.to_le_bytes());
    out
}

/// 0-65535 → whole percent.
#[must_use]
pub fn percent(level: u16) -> i64 {
    (i64::from(level) * 100 + 32_767) / 65_535
}

/// Percent (clamped to 0-100) → 0-65535.
#[must_use]
pub fn level(percent: f64) -> u16 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let level = (percent.clamp(0.0, 100.0) * 655.35).round() as u16;
    level
}

/// Hue and saturation → `#rrggbb` (full value: brightness is its own point).
#[must_use]
pub fn hex(color: Hsbk) -> String {
    let sixths = f64::from(color.hue) / 65_535.0 * 6.0;
    let sat = f64::from(color.saturation) / 65_535.0;
    let sector = sixths.floor();
    let frac = sixths - sector;
    let (low, fall, rise) = (1.0 - sat, 1.0 - sat * frac, 1.0 - sat * (1.0 - frac));
    #[allow(clippy::cast_possible_truncation)]
    let (red, green, blue) = match (sector as i64).rem_euclid(6) {
        0 => (1.0, rise, low),
        1 => (fall, 1.0, low),
        2 => (low, 1.0, rise),
        3 => (low, fall, 1.0),
        4 => (rise, low, 1.0),
        _ => (1.0, low, fall),
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("#{:02x}{:02x}{:02x}", byte(red), byte(green), byte(blue))
}

/// `#rrggbb` → (hue, saturation) on 0-65535; the value is left to the
/// brightness point.
#[must_use]
pub fn hue_saturation(text: &str) -> Option<(u16, u16)> {
    let digits = text.trim().strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let channel = |i: usize| {
        u8::from_str_radix(&digits[i..i + 2], 16)
            .ok()
            .map(|v| f64::from(v) / 255.0)
    };
    let (r, g, b) = (channel(0)?, channel(2)?, channel(4)?);
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    let hue = if delta == 0.0 {
        0.0
    } else if (max - r).abs() < f64::EPSILON {
        ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < f64::EPSILON {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    let saturation = if max == 0.0 { 0.0 } else { delta / max };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let scale = |v: f64| (v * 65_535.0).round().clamp(0.0, 65_535.0) as u16;
    Some((scale(hue / 6.0), scale(saturation)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn get_service_broadcast_frame() {
        // Size 36, protocol 1024 + addressable + tagged (`00 34`), source
        // 2, every device, a response asked (byte 22), type 2.
        let frame = encode(2, None, 0, GET_SERVICE, &[], Reply::Response);
        assert_eq!(
            frame,
            bytes("240000340200000000000000000000000000000000000100000000000000000002000000")
        );
    }

    #[test]
    fn set_color_to_one_bulb() {
        let target = [0x02, 0, 0, 0, 0, 0x01];
        let color = Hsbk {
            hue: 21_845,
            saturation: 65_535,
            brightness: 65_535,
            kelvin: 3500,
        };
        let frame = encode(
            7,
            Some(target),
            9,
            LIGHT_SET_COLOR,
            &set_color(color, 1024),
            Reply::Ack,
        );
        assert_eq!(frame.len(), 49);
        assert_eq!(&frame[..4], &[49, 0, 0x00, 0x14]);
        assert_eq!(&frame[8..16], &[0x02, 0, 0, 0, 0, 1, 0, 0]);
        assert_eq!(frame[22], 0b10);
        assert_eq!(frame[23], 9);
        assert_eq!(u16_at(&frame, 32), Some(LIGHT_SET_COLOR));
        assert_eq!(&frame[36..], &bytes("005555ffffffffac0d00040000")[..]);
        let back = decode(&frame).unwrap();
        assert_eq!(back.source, 7);
        assert_eq!(back.target, target);
        assert_eq!(back.sequence, 9);
        assert_eq!(back.payload.len(), 13);
    }

    #[test]
    fn light_state_reads_power_brightness_and_label() {
        let mut payload = Vec::new();
        for v in [0u16, 0, 32_768, 2700, 0, 65_535] {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        let mut name = [0u8; 32];
        name[..8].copy_from_slice(b"Lumi\xc3\xa8re");
        payload.extend_from_slice(&name);
        payload.extend_from_slice(&[0u8; 8]);
        let state = light_state(&payload).unwrap();
        assert!(state.on);
        assert_eq!(percent(state.color.brightness), 50);
        assert_eq!(state.color.kelvin, 2700);
        assert_eq!(state.label.as_deref(), Some("Lumière"));
    }

    #[test]
    fn short_or_foreign_datagrams_are_refused() {
        assert!(decode(&[0; 10]).is_none());
        let mut frame = encode(2, None, 0, GET_SERVICE, &[], Reply::Response);
        frame[2] = 0;
        frame[3] = 0;
        assert!(decode(&frame).is_none());
        assert!(light_state(&[0; 20]).is_none());
    }

    #[test]
    fn service_firmware_version_group_and_signal() {
        assert_eq!(udp_port(&bytes("017cdd0000")), Some(PORT));
        assert_eq!(udp_port(&bytes("057cdd0000")), None);
        let mut fw = vec![0u8; 16];
        fw.extend_from_slice(&80u16.to_le_bytes());
        fw.extend_from_slice(&3u16.to_le_bytes());
        assert_eq!(firmware(&fw), Some((3, 80)));
        assert_eq!(version(&bytes("0100000057000000")), Some((1, 87)));
        let mut g = vec![0u8; 16];
        let mut name = [0u8; 32];
        name[..6].copy_from_slice(b"Bureau");
        g.extend_from_slice(&name);
        g.extend_from_slice(&[0u8; 8]);
        assert_eq!(group(&g).as_deref(), Some("Bureau"));
        // 1e-6 mW → -60 dBm.
        let mut wifi = 1e-6f32.to_le_bytes().to_vec();
        wifi.extend_from_slice(&[0u8; 10]);
        assert_eq!(rssi(&wifi), Some(-60));
        assert_eq!(rssi(&[0u8; 14]), None);
    }

    #[test]
    fn percent_and_level_round_trip() {
        assert_eq!(percent(0), 0);
        assert_eq!(percent(u16::MAX), 100);
        for p in 0..=100 {
            assert_eq!(percent(level(f64::from(p))), i64::from(p));
        }
        assert_eq!(level(150.0), u16::MAX);
    }

    #[test]
    fn colours_round_trip_through_hex() {
        for text in [
            "#ff0000", "#00ff00", "#0000ff", "#ffffff", "#ff8000", "#8000ff",
        ] {
            let (hue, saturation) = hue_saturation(text).unwrap();
            let color = Hsbk {
                hue,
                saturation,
                brightness: u16::MAX,
                kelvin: 3500,
            };
            assert_eq!(hex(color), text);
        }
        assert!(hue_saturation("red").is_none());
        assert!(hue_saturation("#12345").is_none());
    }
}
