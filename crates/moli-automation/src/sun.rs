//! Sunrise and sunset, computed here (NOAA's approximation, about a minute
//! off): the house knows the sun without asking the internet.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

/// Sunrise and sunset of a (local) day, `None` during polar days and nights.
#[allow(clippy::many_single_char_names)]
pub fn times(date: Date, latitude: f64, longitude: f64) -> Option<(Timestamp, Timestamp)> {
    let n = f64::from(date.day_of_year());
    let g = 2.0 * std::f64::consts::PI / 365.0 * (n - 1.0);
    let eqtime = 229.18
        * (0.000_075 + 0.001_868 * g.cos()
            - 0.032_077 * g.sin()
            - 0.014_615 * (2.0 * g).cos()
            - 0.040_849 * (2.0 * g).sin());
    let decl = 0.006_918 - 0.399_912 * g.cos() + 0.070_257 * g.sin() - 0.006_758 * (2.0 * g).cos()
        + 0.000_907 * (2.0 * g).sin()
        - 0.002_697 * (3.0 * g).cos()
        + 0.001_48 * (3.0 * g).sin();
    let lat = latitude.to_radians();
    let cos_ha = 90.833_f64.to_radians().cos() / (lat.cos() * decl.cos()) - lat.tan() * decl.tan();
    if !(-1.0..=1.0).contains(&cos_ha) {
        return None;
    }
    let ha = cos_ha.acos().to_degrees();
    let midnight = date.to_zoned(TimeZone::UTC).ok()?.timestamp();
    let at = |minutes: f64| {
        #[allow(clippy::cast_possible_truncation)]
        let secs = (minutes * 60.0).round() as i64;
        midnight.checked_add(SignedDuration::from_secs(secs)).ok()
    };
    Some((
        at(720.0 - 4.0 * (longitude + ha) - eqtime)?,
        at(720.0 - 4.0 * (longitude - ha) - eqtime)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paris_matches_the_almanac() {
        // Open-Meteo for 48.85 N, 2.35 E on 3 Oct 2026: 07:52 and 19:25 (UTC+2).
        let (rise, set) = times(Date::new(2026, 10, 3).unwrap(), 48.85, 2.35).unwrap();
        let paris = TimeZone::get("Europe/Paris").unwrap();
        let minutes = |t: Timestamp| {
            let z = t.to_zoned(paris.clone());
            i32::from(z.hour()) * 60 + i32::from(z.minute())
        };
        assert!(
            (minutes(rise) - (7 * 60 + 52)).abs() <= 3,
            "rise {}",
            minutes(rise)
        );
        assert!(
            (minutes(set) - (19 * 60 + 25)).abs() <= 3,
            "set {}",
            minutes(set)
        );
    }

    #[test]
    fn polar_night_has_no_sun() {
        assert!(times(Date::new(2026, 12, 21).unwrap(), 80.0, 0.0).is_none());
    }
}
