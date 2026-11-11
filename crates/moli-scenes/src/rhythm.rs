//! The rhythm of the day: the white and the level a light takes when it is
//! switched on, by the hour. Cool and bright in the morning, warm and low at
//! night, sliding from one to the next. Pure: no IO.
//!
//! ```toml
//! [rhythm]
//! lights = ["lumieres:suspensions", "hue:…"]
//! # Optional: the house's own curve (local time, in order).
//! curve = [
//!   { at = "07:00", white = 2700, brightness = 60 },
//!   { at = "09:00", white = 4300, brightness = 100 },
//!   { at = "21:30", white = 2400, brightness = 50 },
//! ]
//! ```

use serde::Deserialize;

const DAY: u32 = 24 * 60;

/// Which lights follow the day, and the curve they follow.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rhythm {
    /// Device ids (a fixture counts as one light).
    pub lights: Vec<String>,
    #[serde(default = "default_curve")]
    pub curve: Vec<Stop>,
}

/// One point of the curve.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Stop {
    /// Local time, `HH:MM`.
    pub at: String,
    /// Kelvins.
    pub white: u32,
    /// Percent.
    pub brightness: f64,
}

impl Stop {
    fn new(at: &str, white: u32, brightness: f64) -> Self {
        Self {
            at: at.into(),
            white,
            brightness,
        }
    }

    /// Minutes since midnight.
    fn minute(&self) -> Option<u32> {
        let (hours, minutes) = self.at.split_once(':')?;
        let (hours, minutes): (u32, u32) = (hours.parse().ok()?, minutes.parse().ok()?);
        (hours < 24 && minutes < 60 && self.at.len() == 5).then_some(hours * 60 + minutes)
    }
}

/// A day at home: a soft wake-up, daylight white, then warmer and lower
/// through the evening.
#[must_use]
pub fn default_curve() -> Vec<Stop> {
    vec![
        Stop::new("05:00", 2200, 30.0),
        Stop::new("07:00", 2700, 60.0),
        Stop::new("09:00", 4300, 100.0),
        Stop::new("16:00", 4000, 100.0),
        Stop::new("19:00", 3000, 80.0),
        Stop::new("21:30", 2400, 50.0),
        Stop::new("23:30", 2200, 30.0),
    ]
}

impl Rhythm {
    /// `[rhythm]` checked: lights, times in order, values a lamp can take.
    pub fn check(&self) -> Result<(), String> {
        if self.lights.is_empty() {
            return Err("rhythm: at least one light".into());
        }
        for light in &self.lights {
            if !light.contains(':') || light.contains('/') {
                return Err(format!("rhythm: {light:?} is not a device id"));
            }
        }
        if self.curve.is_empty() {
            return Err("rhythm: an empty curve".into());
        }
        let mut last = None;
        for stop in &self.curve {
            let minute = stop
                .minute()
                .ok_or_else(|| format!("rhythm: {:?} is not a time (HH:MM)", stop.at))?;
            if last.is_some_and(|l| minute <= l) {
                return Err(format!("rhythm: {:?} is out of order", stop.at));
            }
            last = Some(minute);
            if !(1500..=6500).contains(&stop.white) {
                return Err(format!(
                    "rhythm: {} K at {:?} (1500 to 6500)",
                    stop.white, stop.at
                ));
            }
            if !(1.0..=100.0).contains(&stop.brightness) {
                return Err(format!("rhythm: brightness at {:?} (1 to 100)", stop.at));
            }
        }
        Ok(())
    }

    /// The white (kelvins, to the nearest 50) and the level (percent) at this
    /// minute of the day, between the stop before and the stop after.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn at(&self, minute: u32) -> (u32, f64) {
        let minute = minute % DAY;
        let stops: Vec<(u32, &Stop)> = self
            .curve
            .iter()
            .filter_map(|s| Some((s.minute()?, s)))
            .collect();
        let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
            return (2700, 100.0);
        };
        // The stop before (yesterday's last one, early in the morning) and
        // the stop after (tomorrow's first one, late at night).
        let before = stops
            .iter()
            .rev()
            .find(|(m, _)| *m <= minute)
            .map_or((f64::from(last.0) - f64::from(DAY), last.1), |(m, s)| {
                (f64::from(*m), *s)
            });
        let after = stops
            .iter()
            .find(|(m, _)| *m > minute)
            .map_or((f64::from(first.0) + f64::from(DAY), first.1), |(m, s)| {
                (f64::from(*m), *s)
            });
        let span = after.0 - before.0;
        let t = if span > 0.0 {
            (f64::from(minute) - before.0) / span
        } else {
            0.0
        };
        let slide = |a: f64, b: f64| a + (b - a) * t;
        let white = slide(f64::from(before.1.white), f64::from(after.1.white));
        let brightness = slide(before.1.brightness, after.1.brightness);
        (((white / 50.0).round() * 50.0) as u32, brightness.round())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> Rhythm {
        Rhythm {
            lights: vec!["hue:a".into()],
            curve: default_curve(),
        }
    }

    #[test]
    fn the_day_slides_from_one_stop_to_the_next() {
        let day = day();
        day.check().unwrap();
        assert_eq!(day.at(9 * 60), (4300, 100.0));
        assert_eq!(day.at(12 * 60), (4150, 100.0));
        // Half way between 19:00 (3000 K, 80 %) and 21:30 (2400 K, 50 %).
        assert_eq!(day.at(20 * 60 + 15), (2700, 65.0));
        // The night is flat: 23:30 and 05:00 say the same.
        assert_eq!(day.at(0), (2200, 30.0));
        assert_eq!(day.at(3 * 60), (2200, 30.0));
        assert_eq!(day.at(23 * 60 + 59), (2200, 30.0));
    }

    #[test]
    fn midnight_is_crossed_between_the_last_stop_and_the_first() {
        let day = Rhythm {
            lights: vec!["hue:a".into()],
            curve: vec![
                Stop::new("08:00", 4000, 100.0),
                Stop::new("20:00", 2000, 20.0),
            ],
        };
        // 02:00 is half way from 20:00 to 08:00.
        assert_eq!(day.at(2 * 60), (3000, 60.0));
        let one = Rhythm {
            lights: vec!["hue:a".into()],
            curve: vec![Stop::new("08:00", 3000, 70.0)],
        };
        assert_eq!(one.at(0), (3000, 70.0));
        assert_eq!(one.at(15 * 60), (3000, 70.0));
    }

    #[test]
    fn a_wrong_curve_is_refused() {
        let with = |curve: Vec<Stop>| Rhythm {
            lights: vec!["hue:a".into()],
            curve,
        };
        assert!(with(vec![]).check().is_err());
        assert!(with(vec![Stop::new("7:00", 3000, 50.0)]).check().is_err());
        assert!(with(vec![Stop::new("25:00", 3000, 50.0)]).check().is_err());
        assert!(with(vec![Stop::new("07:00", 900, 50.0)]).check().is_err());
        assert!(with(vec![Stop::new("07:00", 3000, 0.0)]).check().is_err());
        let unordered = vec![
            Stop::new("09:00", 3000, 50.0),
            Stop::new("07:00", 3000, 50.0),
        ];
        assert!(with(unordered).check().is_err());
        let mut nobody = day();
        nobody.lights.clear();
        assert!(nobody.check().is_err());
        nobody.lights = vec!["not-an-id".into()];
        assert!(nobody.check().is_err());
    }

    #[test]
    fn the_curve_is_optional_in_the_file() {
        let rhythm: Rhythm = toml::from_str(r#"lights = ["hue:a"]"#).unwrap();
        assert_eq!(rhythm.curve, default_curve());
    }
}
