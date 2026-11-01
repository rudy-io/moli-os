//! A Tuya bulb's settings as Moli names them for any lamp (Hue,
//! Zigbee2MQTT…): `brightness` in %, `color_temp` in mired, `color` as
//! `#rrggbb`. The dashboard, ambiances and Moli find a lamp's controls by
//! these keys; the bulb keeps its own codes and scales.

use serde_json::{Map, Value as Json};

use moli_core::Value;

/// The warmest and coolest whites of a Tuya bulb's `temp_value` scale (its
/// 0 and its maximum), in kelvin: usual for these bulbs, not measured.
const WARM: f64 = 2700.0;
const COOL: f64 = 6500.0;

/// The categories of lights and dimmers (bulb, strip, ceiling, ambient,
/// string, dimmers). Only their DPs take a lamp's names: anything else
/// keeps its code and meaning, so a gate module that happened to have a
/// `bright_value` is still seen for what it is by the guard (`opener`).
pub const LAMPS: [&str; 7] = ["dj", "dd", "xdd", "fwd", "dc", "tgq", "tgkg"];

/// Which setting of a lamp a DP holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    Brightness,
    White,
    Colour,
}

impl Light {
    #[must_use]
    pub fn of(code: &str) -> Option<Self> {
        match code {
            "bright_value" | "bright_value_v2" => Some(Self::Brightness),
            "temp_value" | "temp_value_v2" => Some(Self::White),
            "colour_data_v2" => Some(Self::Colour),
            _ => None,
        }
    }

    /// The key every lamp of Moli has for it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Brightness => "brightness",
            Self::White => "color_temp",
            Self::Colour => "color",
        }
    }
}

/// Brightness in % from the bulb's scale (`bright_value_v2`: 10–1000). A
/// lamp that is on is never said to be at 0 %.
#[must_use]
pub fn percent(raw: f64, max: f64) -> f64 {
    (raw / scale(max) * 100.0).round().clamp(1.0, 100.0)
}

/// A maximum a bulb could say wrong (zero, negative): Tuya's usual one.
#[must_use]
pub fn scale(max: f64) -> f64 {
    if max > 0.0 { max } else { 1000.0 }
}

/// % → the bulb's scale, within its bounds.
#[must_use]
pub fn from_percent(pct: f64, min: f64, max: f64) -> f64 {
    let max = scale(max);
    (pct / 100.0 * max).round().clamp(min.min(max), max)
}

/// The mired bounds of the whites: the coolest is the smallest.
#[must_use]
pub fn mired_bounds() -> (f64, f64) {
    ((1e6 / COOL).round(), (1e6 / WARM).round())
}

/// The bulb's white (0 warm … max cool) → mired, evenly between the two.
#[must_use]
pub fn mired(raw: f64, max: f64) -> f64 {
    let (cool, warm) = mired_bounds();
    (warm - (raw / scale(max)).clamp(0.0, 1.0) * (warm - cool)).round()
}

/// Mired → the bulb's white scale.
#[must_use]
pub fn from_mired(mired: f64, max: f64) -> f64 {
    let (cool, warm) = mired_bounds();
    ((warm - mired.clamp(cool, warm)) / (warm - cool) * scale(max)).round()
}

/// A colour as Tuya holds it: hue 0–360, saturation and value 0–1000.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsv {
    pub h: f64,
    pub s: f64,
    pub v: f64,
}

impl Hsv {
    /// `000003e803e8` (four hex digits each, as it is written back) or
    /// `{"h":0,"s":1000,"v":1000}` (the cloud's form, or as a string).
    #[must_use]
    pub fn parse(raw: &Json) -> Option<Self> {
        match raw {
            Json::String(s) if s.len() == 12 && s.bytes().all(|b| b.is_ascii_hexdigit()) => {
                let part = |i: usize| u16::from_str_radix(&s[i..i + 4], 16).ok().map(f64::from);
                Some(Self {
                    h: part(0)?,
                    s: part(4)?,
                    v: part(8)?,
                })
            }
            Json::String(s) => Self::parse(&serde_json::from_str(s).ok()?),
            Json::Object(o) => Some(Self {
                h: o.get("h")?.as_f64()?,
                s: o.get("s")?.as_f64()?,
                v: o.get("v")?.as_f64()?,
            }),
            _ => None,
        }
    }

    /// As the bulb takes it on the LAN.
    #[must_use]
    pub fn tuya(self) -> String {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = |x: f64, max: f64| x.round().clamp(0.0, max) as u16;
        format!(
            "{:04x}{:04x}{:04x}",
            n(self.h, 360.0),
            n(self.s, 1000.0),
            n(self.v, 1000.0)
        )
    }

    /// `#rrggbb` at full value: the colour itself, its brightness apart.
    #[must_use]
    pub fn hex(self) -> String {
        let sector = self.h.rem_euclid(360.0) / 60.0;
        let sat = (self.s / 1000.0).clamp(0.0, 1.0);
        let mid = sat * (1.0 - (sector % 2.0 - 1.0).abs());
        let (red, green, blue) = match sector {
            x if x < 1.0 => (sat, mid, 0.0),
            x if x < 2.0 => (mid, sat, 0.0),
            x if x < 3.0 => (0.0, sat, mid),
            x if x < 4.0 => (0.0, mid, sat),
            x if x < 5.0 => (mid, 0.0, sat),
            _ => (sat, 0.0, mid),
        };
        let base = 1.0 - sat;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let byte = |c: f64| ((c + base) * 255.0).round().clamp(0.0, 255.0) as u8;
        format!("#{:02x}{:02x}{:02x}", byte(red), byte(green), byte(blue))
    }

    /// `#rrggbb` → hue and saturation, at the value given (0–1000).
    #[must_use]
    pub fn from_hex(hex: &str, value: f64) -> Option<Self> {
        let hex = hex.strip_prefix('#')?;
        if hex.len() != 6 {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        let (r8, g8, b8) = (byte(0)?, byte(2)?, byte(4)?);
        let top = r8.max(g8).max(b8);
        let unit = |b: u8| f64::from(b) / 255.0;
        let (red, green, blue, max) = (unit(r8), unit(g8), unit(b8), unit(top));
        let delta = max - unit(r8.min(g8).min(b8));
        // Which channel leads, compared as the bytes they are.
        let hue = if top == r8.min(g8).min(b8) {
            0.0
        } else if top == r8 {
            60.0 * ((green - blue) / delta).rem_euclid(6.0)
        } else if top == g8 {
            60.0 * ((blue - red) / delta + 2.0)
        } else {
            60.0 * ((red - green) / delta + 4.0)
        };
        let sat = if top == 0 { 0.0 } else { delta / max };
        Some(Self {
            h: hue.round(),
            s: (sat * 1000.0).round(),
            v: value.clamp(10.0, 1000.0),
        })
    }
}

fn text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(t) => Some(t),
        _ => None,
    }
}

/// The value (0–1000) for a brightness in %.
#[must_use]
pub fn value_of(pct: f64) -> f64 {
    (pct * 10.0).clamp(10.0, 1000.0)
}

/// What to send for an order on a lamp's setting, given the DP ids of its
/// mode, brightness and colour, and what the lamp shows now (`current`, by
/// Moli key). Choosing a colour puts the bulb in colour mode, a white in
/// white mode; a brightness while in colour is the colour's own value,
/// else the bulb would dim its white, unseen.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn order(
    light: Light,
    value: &Value,
    dp: u32,
    raw: Json,
    mode: Option<u32>,
    colour: Option<u32>,
    current: &dyn Fn(&str) -> Option<Value>,
) -> Option<Map<String, Json>> {
    let mut dps = Map::new();
    let in_colour = current("work_mode").is_some_and(|m| m == Value::from("colour"));
    match light {
        Light::Colour => {
            let pct = current("brightness")
                .and_then(|b| b.as_f64())
                .unwrap_or(100.0);
            let hsv = Hsv::from_hex(text(value)?, value_of(pct))?;
            dps.insert(dp.to_string(), Json::String(hsv.tuya()));
            if let Some(mode) = mode {
                dps.insert(mode.to_string(), Json::from("colour"));
            }
        }
        Light::White => {
            dps.insert(dp.to_string(), raw);
            if let Some(mode) = mode {
                dps.insert(mode.to_string(), Json::from("white"));
            }
        }
        Light::Brightness => match (in_colour, colour, current("color")) {
            (true, Some(colour), Some(hex)) => {
                let hsv = Hsv::from_hex(text(&hex)?, value_of(value.as_f64()?))?;
                dps.insert(colour.to_string(), Json::String(hsv.tuya()));
            }
            _ => {
                dps.insert(dp.to_string(), raw);
            }
        },
    }
    Some(dps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_reads_from_the_lan_and_the_cloud() {
        let red = Hsv {
            h: 0.0,
            s: 1000.0,
            v: 1000.0,
        };
        assert_eq!(Hsv::parse(&Json::from("000003e803e8")), Some(red));
        assert_eq!(
            Hsv::parse(&serde_json::json!({"h": 0, "s": 1000, "v": 1000})),
            Some(red)
        );
        assert_eq!(
            Hsv::parse(&Json::from(r#"{"h":0,"s":1000,"v":1000}"#)),
            Some(red)
        );
        assert_eq!(red.tuya(), "000003e803e8");
        assert_eq!(red.hex(), "#ff0000");
        assert_eq!(Hsv::parse(&Json::from("pas une couleur")), None);
    }

    #[test]
    fn colours_go_both_ways() {
        for hex in [
            "#ff0000", "#00ff00", "#0000ff", "#ffffff", "#ff8000", "#8000ff",
        ] {
            let hsv = Hsv::from_hex(hex, 1000.0).unwrap();
            assert_eq!(hsv.hex(), hex, "{hsv:?}");
        }
        let blue = Hsv::from_hex("#0000ff", 500.0).unwrap();
        assert_eq!(blue.tuya(), "00f003e801f4");
    }

    #[test]
    fn brightness_and_white_take_moli_scales() {
        assert_eq!(percent(1000.0, 1000.0), 100.0);
        assert_eq!(percent(10.0, 1000.0), 1.0);
        assert_eq!(from_percent(50.0, 10.0, 1000.0), 500.0);
        assert_eq!(from_percent(0.0, 10.0, 1000.0), 10.0);
        // 0 warm (2700 K = 370 mired), max cool (6500 K = 154 mired).
        assert_eq!(mired_bounds(), (154.0, 370.0));
        assert_eq!(mired(0.0, 1000.0), 370.0);
        assert_eq!(mired(1000.0, 1000.0), 154.0);
        assert_eq!(from_mired(370.0, 1000.0), 0.0);
        assert_eq!(from_mired(154.0, 1000.0), 1000.0);
        // Whole mireds: back within a few steps of the bulb's 1000.
        let back = from_mired(mired(571.0, 1000.0), 1000.0);
        assert!((back - 571.0).abs() <= 3.0, "{back}");
    }

    /// A bulb that says `max = 0` (or less) is read on Tuya's usual 1000,
    /// never divided by zero.
    #[test]
    fn a_maximum_of_zero_is_not_one() {
        assert_eq!(percent(500.0, 0.0), 50.0);
        assert_eq!(percent(500.0, -1.0), 50.0);
        assert_eq!(from_percent(50.0, 10.0, 0.0), 500.0);
        assert_eq!(mired(1000.0, 0.0), 154.0);
        assert_eq!(from_mired(154.0, 0.0), 1000.0);
    }

    #[test]
    fn a_colour_puts_the_bulb_in_colour_and_a_white_in_white() {
        let now = |k: &str| match k {
            "brightness" => Some(Value::Float(40.0)),
            "work_mode" => Some(Value::from("white")),
            _ => None,
        };
        let dps = order(
            Light::Colour,
            &Value::from("#0000ff"),
            24,
            Json::Null,
            Some(21),
            Some(24),
            &now,
        )
        .unwrap();
        assert_eq!(dps["21"], "colour");
        assert_eq!(dps["24"], "00f003e80190", "blue at 40 %");
        let dps = order(
            Light::White,
            &Value::Float(370.0),
            23,
            Json::from(0),
            Some(21),
            Some(24),
            &now,
        )
        .unwrap();
        assert_eq!(
            (dps["21"].as_str(), dps["23"].as_i64()),
            (Some("white"), Some(0))
        );
    }

    #[test]
    fn brightness_in_colour_is_the_colours_own() {
        let in_colour = |k: &str| match k {
            "work_mode" => Some(Value::from("colour")),
            "color" => Some(Value::from("#ff0000")),
            _ => None,
        };
        let dps = order(
            Light::Brightness,
            &Value::Float(25.0),
            22,
            Json::from(250),
            Some(21),
            Some(24),
            &in_colour,
        )
        .unwrap();
        assert_eq!(dps.len(), 1);
        assert_eq!(dps["24"], "000003e800fa", "red at 25 %");
        let in_white = |k: &str| (k == "work_mode").then(|| Value::from("white"));
        let dps = order(
            Light::Brightness,
            &Value::Float(25.0),
            22,
            Json::from(250),
            Some(21),
            Some(24),
            &in_white,
        )
        .unwrap();
        assert_eq!(dps["22"], 250);
    }
}
