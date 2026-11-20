//! The messages of Govee's LAN API, and nothing else: no socket here.
//!
//! Every message is one UDP datagram holding one JSON object,
//! `{"msg":{"cmd":…,"data":{…}}}`. A controller sends `scan` to the
//! multicast group (port 4001) or to a light's address; a light answers to
//! port 4002 of whoever asked. Orders and `devStatus` go to port 4003 of the
//! light, whose answer to `devStatus` comes back to port 4002 as well.
//!
//! Written from Govee's public "LAN API user manual". No message carries a
//! request number, and an order has no acknowledgement: what a light did is
//! known only by asking `devStatus` afterwards.

use std::net::Ipv4Addr;

use serde::Deserialize;
use serde_json::{Value, json};

/// The multicast group a `scan` is sent to.
pub const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
/// Where lights listen for a `scan`.
pub const SCAN_PORT: u16 = 4001;
/// Where a controller hears every answer.
pub const LISTEN_PORT: u16 = 4002;
/// Where a light takes orders and `devStatus`.
pub const COMMAND_PORT: u16 = 4003;
/// The colour temperatures the manual gives, in kelvins.
pub const KELVIN: (u16, u16) = (2000, 9000);

/// A colour as the lights take it. The brightness is its own value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// `#rrggbb`.
    #[must_use]
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// From `#rrggbb`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.trim().strip_prefix('#')?;
        if digits.len() != 6 || !digits.is_ascii() {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
        Some(Self {
            r: channel(0)?,
            g: channel(2)?,
            b: channel(4)?,
        })
    }
}

/// A light that answered a `scan`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Its identifier, as hexadecimal digits only (lower case).
    pub device: String,
    /// Its model (`H6163`…), empty if it gave none.
    pub sku: String,
}

/// What a light says of itself in a `devStatus`. A field it left out is
/// `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub on: Option<bool>,
    /// Percent.
    pub brightness: Option<u8>,
    pub color: Option<Rgb>,
    /// Kelvins; 0 when the light shows a colour, not a white.
    pub kelvin: Option<u16>,
}

/// What a light may send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Scan(Found),
    Status(Status),
}

/// An order to a light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Turn(bool),
    /// Percent, 1 to 100.
    Brightness(u8),
    Color(Rgb),
    /// A white of this many kelvins.
    Kelvin(u16),
}

impl Order {
    /// The datagram that carries it.
    #[must_use]
    pub fn message(self) -> Vec<u8> {
        match self {
            Self::Turn(on) => message("turn", &json!({ "value": u8::from(on) })),
            Self::Brightness(percent) => {
                message("brightness", &json!({ "value": percent.clamp(1, 100) }))
            }
            // A kelvin of 0 tells the light to take the colour.
            Self::Color(Rgb { r, g, b }) => message(
                "colorwc",
                &json!({ "color": { "r": r, "g": g, "b": b }, "colorTemInKelvin": 0 }),
            ),
            Self::Kelvin(kelvin) => message(
                "colorwc",
                &json!({
                    "color": { "r": 0, "g": 0, "b": 0 },
                    "colorTemInKelvin": kelvin.clamp(KELVIN.0, KELVIN.1),
                }),
            ),
        }
    }

    /// Whether this reading shows the order carried out.
    #[must_use]
    pub fn done(self, status: &Status) -> bool {
        match self {
            Self::Turn(on) => status.on == Some(on),
            Self::Brightness(percent) => status.brightness == Some(percent),
            Self::Color(color) => {
                status.color == Some(color) && matches!(status.kelvin, None | Some(0))
            }
            Self::Kelvin(kelvin) => status.kelvin == Some(kelvin),
        }
    }
}

fn message(cmd: &str, data: &Value) -> Vec<u8> {
    json!({ "msg": { "cmd": cmd, "data": data } })
        .to_string()
        .into_bytes()
}

/// "Who is there?" The manual gives `reserve` as the only topic.
#[must_use]
pub fn scan() -> Vec<u8> {
    message("scan", &json!({ "account_topic": "reserve" }))
}

/// "What is your state?"
#[must_use]
pub fn status_request() -> Vec<u8> {
    message("devStatus", &json!({}))
}

#[derive(Deserialize)]
struct Envelope {
    msg: Msg,
}

#[derive(Deserialize)]
struct Msg {
    cmd: String,
    #[serde(default)]
    data: Value,
}

/// Reads a datagram. `None` for anything that is not a light's answer to
/// `scan` or `devStatus` (another controller's own `scan` included).
#[must_use]
pub fn decode(datagram: &[u8]) -> Option<Reply> {
    let Envelope { msg } = serde_json::from_slice(datagram).ok()?;
    match msg.cmd.as_str() {
        "scan" => {
            let device = native_id(msg.data.get("device")?.as_str()?)?;
            let sku = msg
                .data
                .get("sku")
                .and_then(Value::as_str)
                .map(model)
                .unwrap_or_default();
            Some(Reply::Scan(Found { device, sku }))
        }
        "devStatus" => {
            let data = msg.data.as_object()?;
            let status = Status {
                on: data.get("onOff").and_then(|v| match v {
                    Value::Bool(on) => Some(*on),
                    other => other.as_f64().map(|n| n != 0.0),
                }),
                brightness: data
                    .get("brightness")
                    .and_then(|v| bounded(v, 100.0))
                    .and_then(|n| u8::try_from(n).ok()),
                color: data.get("color").and_then(|c| {
                    let channel = |name: &str| {
                        bounded(c.get(name)?, 255.0).and_then(|n| u8::try_from(n).ok())
                    };
                    Some(Rgb {
                        r: channel("r")?,
                        g: channel("g")?,
                        b: channel("b")?,
                    })
                }),
                kelvin: data
                    .get("colorTemInKelvin")
                    .and_then(|v| bounded(v, f64::from(u16::MAX))),
            };
            // An object with none of the fields is not a state.
            (status != Status::default()).then_some(Reply::Status(status))
        }
        _ => None,
    }
}

/// A number brought into 0..=`max`, rounded.
fn bounded(value: &Value, max: f64) -> Option<u16> {
    let n = value.as_f64()?;
    if !n.is_finite() {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = n.round().clamp(0.0, max.min(f64::from(u16::MAX))) as u16;
    Some(n)
}

/// A light's identifier (`AA:BB:…`, 8 bytes in the manual) as its
/// hexadecimal digits in lower case; `None` if it holds anything else.
#[must_use]
pub fn native_id(device: &str) -> Option<String> {
    let digits: String = device
        .trim()
        .chars()
        .filter(|c| *c != ':')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let plausible =
        (12..=32).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_hexdigit());
    plausible.then_some(digits)
}

/// A model name kept short and plain: it is shown to people.
fn model(sku: &str) -> String {
    sku.trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .take(24)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    #[test]
    fn requests_are_the_manuals() {
        assert_eq!(
            json(&scan()),
            json!({"msg":{"cmd":"scan","data":{"account_topic":"reserve"}}})
        );
        assert_eq!(
            json(&status_request()),
            json!({"msg":{"cmd":"devStatus","data":{}}})
        );
    }

    #[test]
    fn orders_are_the_manuals() {
        assert_eq!(
            json(&Order::Turn(true).message()),
            json!({"msg":{"cmd":"turn","data":{"value":1}}})
        );
        assert_eq!(
            json(&Order::Turn(false).message()),
            json!({"msg":{"cmd":"turn","data":{"value":0}}})
        );
        assert_eq!(
            json(&Order::Brightness(50).message()),
            json!({"msg":{"cmd":"brightness","data":{"value":50}}})
        );
        // Never 0: the manual's range starts at 1.
        assert_eq!(
            json(&Order::Brightness(0).message())["msg"]["data"]["value"],
            1
        );
        assert_eq!(
            json(
                &Order::Color(Rgb {
                    r: 255,
                    g: 12,
                    b: 8
                })
                .message()
            ),
            json!({"msg":{"cmd":"colorwc","data":{
                "color":{"r":255,"g":12,"b":8},"colorTemInKelvin":0}}})
        );
        assert_eq!(
            json(&Order::Kelvin(7200).message()),
            json!({"msg":{"cmd":"colorwc","data":{
                "color":{"r":0,"g":0,"b":0},"colorTemInKelvin":7200}}})
        );
        assert_eq!(
            json(&Order::Kelvin(12_000).message())["msg"]["data"]["colorTemInKelvin"],
            9000
        );
    }

    // The two fixtures are messages built from the manual, never heard from
    // a real light (see the integration's README).
    const SCAN: &str = include_str!("../../../integrations/govee/fixtures/scan.json");
    const STATUS: &str = include_str!("../../../integrations/govee/fixtures/dev-status.json");

    #[test]
    fn the_scan_fixture_is_read() {
        assert_eq!(
            decode(SCAN.as_bytes()),
            Some(Reply::Scan(Found {
                device: "aabbccddeeff0011".into(),
                sku: "H6163".into(),
            }))
        );
    }

    #[test]
    fn the_status_fixture_is_read() {
        assert_eq!(
            decode(STATUS.as_bytes()),
            Some(Reply::Status(Status {
                on: Some(true),
                brightness: Some(100),
                color: Some(Rgb { r: 255, g: 0, b: 0 }),
                kelvin: Some(7200),
            }))
        );
    }

    #[test]
    fn a_status_is_read_leniently() {
        let read = |text: &str| match decode(text.as_bytes()) {
            Some(Reply::Status(status)) => status,
            other => panic!("not a status: {other:?}"),
        };
        let partial = read(r#"{"msg":{"cmd":"devStatus","data":{"onOff":0}}}"#);
        assert_eq!(
            partial,
            Status {
                on: Some(false),
                ..Status::default()
            }
        );
        let odd = read(
            r#"{"msg":{"cmd":"devStatus","data":{"onOff":true,"brightness":254.6,
                "color":{"r":300,"g":-4,"b":7},"colorTemInKelvin":0}}}"#,
        );
        assert_eq!(odd.on, Some(true));
        assert_eq!(odd.brightness, Some(100));
        assert_eq!(odd.color, Some(Rgb { r: 255, g: 0, b: 7 }));
        assert_eq!(odd.kelvin, Some(0));
        // A colour missing a channel is no colour.
        let holed = read(r#"{"msg":{"cmd":"devStatus","data":{"onOff":1,"color":{"r":1,"g":2}}}}"#);
        assert_eq!(holed.color, None);
    }

    #[test]
    fn what_is_not_an_answer_is_ignored() {
        for text in [
            "",
            "not json",
            "{}",
            r#"{"msg":{"cmd":"devStatus"}}"#,
            r#"{"msg":{"cmd":"devStatus","data":{}}}"#,
            r#"{"msg":{"cmd":"devStatus","data":{"other":1}}}"#,
            // Another controller's own scan.
            r#"{"msg":{"cmd":"scan","data":{"account_topic":"reserve"}}}"#,
            r#"{"msg":{"cmd":"scan","data":{"device":"not an id","sku":"H6163"}}}"#,
            r#"{"msg":{"cmd":"scan","data":{"device":42}}}"#,
            r#"{"msg":{"cmd":"turn","data":{"value":1}}}"#,
        ] {
            assert_eq!(decode(text.as_bytes()), None, "{text}");
        }
    }

    #[test]
    fn identifiers_and_models_are_cleaned() {
        assert_eq!(
            native_id(" AA:BB:CC:DD:EE:FF:00:11 ").as_deref(),
            Some("aabbccddeeff0011")
        );
        assert_eq!(
            native_id("aa:bb:cc:dd:ee:ff").as_deref(),
            Some("aabbccddeeff")
        );
        assert_eq!(native_id("aa:bb"), None);
        assert_eq!(native_id("aa/bb:cc:dd:ee:ff"), None);
        let Some(Reply::Scan(found)) = decode(
            br#"{"msg":{"cmd":"scan","data":{"device":"AA:BB:CC:DD:EE:FF:00:11","sku":" H61<b>63 "}}}"#,
        ) else {
            panic!("not a scan answer");
        };
        assert_eq!(found.sku, "H61b63");
        let Some(Reply::Scan(found)) =
            decode(br#"{"msg":{"cmd":"scan","data":{"device":"AA:BB:CC:DD:EE:FF:00:11"}}}"#)
        else {
            panic!("not a scan answer");
        };
        assert_eq!(found.sku, "");
    }

    #[test]
    fn colours_go_both_ways() {
        let colour = Rgb::parse(" #FF0c08 ").unwrap();
        assert_eq!(
            colour,
            Rgb {
                r: 255,
                g: 12,
                b: 8
            }
        );
        assert_eq!(colour.hex(), "#ff0c08");
        for bad in ["ff0c08", "#ff0c0", "#ff0c0g", "#ééé", "bleu"] {
            assert_eq!(Rgb::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_order_is_done_when_the_light_says_so() {
        let red = Rgb { r: 255, g: 0, b: 0 };
        let status = Status {
            on: Some(true),
            brightness: Some(40),
            color: Some(red),
            kelvin: Some(0),
        };
        assert!(Order::Turn(true).done(&status));
        assert!(!Order::Turn(false).done(&status));
        assert!(Order::Brightness(40).done(&status));
        assert!(!Order::Brightness(41).done(&status));
        assert!(Order::Color(red).done(&status));
        assert!(!Order::Kelvin(2700).done(&status));
        let white = Status {
            kelvin: Some(2700),
            ..status
        };
        assert!(Order::Kelvin(2700).done(&white));
        // Still a white: the colour was not taken.
        assert!(!Order::Color(red).done(&white));
        assert!(!Order::Turn(true).done(&Status::default()));
    }
}
