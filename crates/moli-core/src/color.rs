//! Light colours: « #rrggbb » (what people and pages pick) and CIE xy (what
//! Hue and Zigbee lamps take). Wide-gamut D65 conversion, as Philips
//! documents it; each lamp clamps to its own gamut.

/// `#rrggbb` → CIE xy (four decimals). `None` for anything else, black
/// included (it has no colour).
#[must_use]
pub fn hex_to_xy(hex: &str) -> Option<(f64, f64)> {
    let [red, green, blue] = parse_hex(hex)?.map(|c| linear(f64::from(c) / 255.0));
    let cx = red * 0.664_511 + green * 0.154_324 + blue * 0.162_028;
    let cy = red * 0.283_881 + green * 0.668_433 + blue * 0.047_685;
    let cz = red * 0.000_088 + green * 0.072_310 + blue * 0.986_039;
    let sum = cx + cy + cz;
    (sum > 0.0).then(|| (round4(cx / sum), round4(cy / sum)))
}

/// CIE xy → `#rrggbb`, at full brightness. `None` outside the diagram.
#[must_use]
pub fn xy_to_hex(x: f64, y: f64) -> Option<String> {
    if !(x.is_finite() && y.is_finite()) || x < 0.0 || y <= 0.0 || x + y > 1.0 {
        return None;
    }
    let (cx, cz) = (x / y, (1.0 - x - y) / y);
    let red = cx * 1.656_492 - 0.354_851 - cz * 0.255_038;
    let green = -cx * 0.707_196 + 1.655_397 + cz * 0.036_152;
    let blue = cx * 0.051_713 - 0.121_364 + cz * 1.011_530;
    let [red, green, blue] = [red, green, blue].map(|c| c.max(0.0));
    let top = red.max(green).max(blue);
    if top <= 0.0 {
        return None;
    }
    let [red, green, blue] = [red, green, blue].map(|c| byte(gamma(c / top)));
    Some(format!("#{red:02x}{green:02x}{blue:02x}"))
}

fn parse_hex(hex: &str) -> Option<[u8; 3]> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let at = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    Some([at(0)?, at(2)?, at(4)?])
}

fn linear(c: f64) -> f64 {
    if c > 0.040_45 {
        ((c + 0.055) / 1.055).powf(2.4)
    } else {
        c / 12.92
    }
}

fn gamma(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn byte(c: f64) -> u8 {
    // Clamped to [0, 255] first: the cast cannot truncate.
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primaries_and_white_go_both_ways() {
        assert_eq!(hex_to_xy("#ff0000"), Some((0.7006, 0.2993)));
        assert_eq!(hex_to_xy("#ffffff"), Some((0.3227, 0.329)));
        for hex in ["#ff0000", "#00ff00", "#0000ff", "#ffffff", "#ff8000"] {
            let (x, y) = hex_to_xy(hex).unwrap();
            let back = xy_to_hex(x, y).unwrap();
            let delta = parse_hex(&back)
                .unwrap()
                .iter()
                .zip(parse_hex(hex).unwrap())
                .map(|(a, b)| a.abs_diff(b))
                .max()
                .unwrap();
            assert!(delta <= 3, "{hex} → ({x}, {y}) → {back}");
        }
    }

    #[test]
    fn nonsense_is_refused() {
        for bad in ["ff0000", "#ff00", "#gg0000", "#000000", "", "#ff00001"] {
            assert_eq!(hex_to_xy(bad), None, "{bad}");
        }
        assert_eq!(xy_to_hex(0.0, 0.0), None);
        assert_eq!(xy_to_hex(0.8, 0.5), None);
        assert_eq!(xy_to_hex(f64::NAN, 0.3), None);
    }
}
