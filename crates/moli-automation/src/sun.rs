//! Sunrise and sunset for the automations: the house's one sun
//! ([`moli_core::sun`], also behind `dehors:soleil`), on its calendar.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

/// Sunrise and sunset of a (local) day, `None` during polar days and nights.
pub fn times(date: Date, latitude: f64, longitude: f64) -> Option<(Timestamp, Timestamp)> {
    let day = u16::try_from(date.day_of_year()).ok()?;
    let (rise, set) = moli_core::sun::rise_set(day, latitude, longitude)?;
    let midnight = date.to_zoned(TimeZone::UTC).ok()?.timestamp();
    let at = |minutes: f64| {
        #[allow(clippy::cast_possible_truncation)]
        let secs = (minutes * 60.0).round() as i64;
        midnight.checked_add(SignedDuration::from_secs(secs)).ok()
    };
    Some((at(rise)?, at(set)?))
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
