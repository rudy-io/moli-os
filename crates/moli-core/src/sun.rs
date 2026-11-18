//! The sun over the house, computed here: no internet, no dependency.
//!
//! One set of formulas (NOAA's approximation) for both what the
//! automations know (sunrise and sunset, day or night) and where the sun is
//! (`dehors:soleil`): so « the sun is up » and the elevation never disagree.
//! About a minute off for the times, about half a degree for the position:
//! plenty for shutters and lights.
//!
//! Pure numbers in, numbers out: the day of the year and the minutes since
//! midnight are UTC's; each caller turns its own clock into them.

use std::f64::consts::PI;

/// The elevation of the sun's centre at sunrise and sunset (degrees): the
/// disc's upper edge on the horizon, refraction included. The elevation
/// [`position`] gives is geometric (no refraction), so it is about this at
/// [`rise_set`]'s times, and crosses 0° a few minutes after sunrise and
/// before sunset.
pub const HORIZON: f64 = -0.833;

/// Where the sun is, in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    /// Height above the horizon, negative at night (-90..=90).
    pub elevation: f64,
    /// Direction, clockwise from north: 90 = east, 180 = south (0..360).
    pub azimuth: f64,
}

/// The fractional year (radians), `day_of_year` from 1, at `minutes` UTC.
fn year_angle(day_of_year: u16, minutes: f64) -> f64 {
    2.0 * PI / 365.0 * (f64::from(day_of_year) - 1.0 + (minutes - 720.0) / 1440.0)
}

/// The equation of time (minutes) and the sun's declination (radians).
fn orbit(g: f64) -> (f64, f64) {
    let eqtime = 229.18
        * (0.000_075 + 0.001_868 * g.cos()
            - 0.032_077 * g.sin()
            - 0.014_615 * (2.0 * g).cos()
            - 0.040_849 * (2.0 * g).sin());
    let decl = 0.006_918 - 0.399_912 * g.cos() + 0.070_257 * g.sin() - 0.006_758 * (2.0 * g).cos()
        + 0.000_907 * (2.0 * g).sin()
        - 0.002_697 * (3.0 * g).cos()
        + 0.001_48 * (3.0 * g).sin();
    (eqtime, decl)
}

/// Sunrise and sunset of a day, in minutes after its midnight UTC (they
/// may fall outside 0..1440 far from Greenwich); `None` during polar days
/// and nights.
#[must_use]
pub fn rise_set(day_of_year: u16, latitude: f64, longitude: f64) -> Option<(f64, f64)> {
    let (eqtime, decl) = orbit(year_angle(day_of_year, 720.0));
    let lat = latitude.to_radians();
    let cos_ha =
        (90.0 - HORIZON).to_radians().cos() / (lat.cos() * decl.cos()) - lat.tan() * decl.tan();
    if !(-1.0..=1.0).contains(&cos_ha) {
        return None;
    }
    let ha = cos_ha.acos().to_degrees();
    Some((
        720.0 - 4.0 * (longitude + ha) - eqtime,
        720.0 - 4.0 * (longitude - ha) - eqtime,
    ))
}

/// Where the sun is on a day, at `minutes` after its midnight UTC.
#[must_use]
pub fn position(day_of_year: u16, minutes: f64, latitude: f64, longitude: f64) -> Position {
    let (eqtime, decl) = orbit(year_angle(day_of_year, minutes));
    // True solar time, then the hour angle (0 at solar noon, west positive).
    let solar = minutes + eqtime + 4.0 * longitude;
    let ha = (solar / 4.0 - 180.0).to_radians();
    let lat = latitude.to_radians();
    let sin_elevation = lat.sin() * decl.sin() + lat.cos() * decl.cos() * ha.cos();
    let elevation = sin_elevation.clamp(-1.0, 1.0).asin().to_degrees();
    let azimuth = (ha
        .sin()
        .atan2(ha.cos() * lat.sin() - decl.tan() * lat.cos())
        .to_degrees()
        + 180.0)
        .rem_euclid(360.0);
    Position { elevation, azimuth }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An independent formula, the Astronomical Almanac's low-precision
    /// one (about 0.01°): the reference [`position`] is checked against.
    fn almanac(day_of_year: u16, minutes: f64, latitude: f64, longitude: f64) -> Position {
        // Days since J2000.0 (2000-01-01 12:00 UTC), for a day of 2026.
        let days_to_2026 = 9496.5; // 2026-01-01 00:00 UTC
        let n = days_to_2026 + f64::from(day_of_year) - 1.0 + minutes / 1440.0;
        let l = (280.460 + 0.985_647_4 * n).rem_euclid(360.0);
        let g = (357.528 + 0.985_600_3 * n).rem_euclid(360.0).to_radians();
        let lambda = (l + 1.915 * g.sin() + 0.020 * (2.0 * g).sin()).to_radians();
        let epsilon = (23.439 - 0.000_000_4 * n).to_radians();
        let ra = (epsilon.cos() * lambda.sin()).atan2(lambda.cos());
        let decl = (epsilon.sin() * lambda.sin()).asin();
        let gmst = (18.697_374_558 + 24.065_709_824_419_08 * n).rem_euclid(24.0);
        let ha = (gmst * 15.0 + longitude).to_radians() - ra;
        let lat = latitude.to_radians();
        let elevation = (lat.sin() * decl.sin() + lat.cos() * decl.cos() * ha.cos())
            .asin()
            .to_degrees();
        let azimuth = (ha
            .sin()
            .atan2(ha.cos() * lat.sin() - decl.tan() * lat.cos())
            .to_degrees()
            + 180.0)
            .rem_euclid(360.0);
        Position { elevation, azimuth }
    }

    fn angle_gap(a: f64, b: f64) -> f64 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    /// Paris, Quito, Sydney, Tromsø, Los Angeles: both hemispheres, the
    /// equator, the polar circle, west of Greenwich.
    const PLACES: [(f64, f64); 5] = [
        (48.85, 2.35),
        (-0.18, -78.47),
        (-33.87, 151.21),
        (69.65, 18.96),
        (34.05, -118.24),
    ];

    #[test]
    fn the_position_matches_the_almanac_all_year_round() {
        let mut worst: (f64, f64) = (0.0, 0.0);
        for (lat, lon) in PLACES {
            for day in (1..=365).step_by(7) {
                for minutes in (0..1440).step_by(20) {
                    let m = f64::from(minutes);
                    let ours = position(day, m, lat, lon);
                    let reference = almanac(day, m, lat, lon);
                    let de = (ours.elevation - reference.elevation).abs();
                    worst.0 = worst.0.max(de);
                    assert!(
                        de < 0.5,
                        "elevation {lat},{lon} day {day} {m}: {ours:?} vs {reference:?}"
                    );
                    // The azimuth is meaningless right under the sun (and
                    // right under the feet): checked where it is of use.
                    if (-10.0..85.0).contains(&reference.elevation) {
                        // As an angle on the sky: a degree of azimuth is
                        // less and less of the sky as the sun climbs.
                        let da = angle_gap(ours.azimuth, reference.azimuth)
                            * reference.elevation.to_radians().cos();
                        worst.1 = worst.1.max(da);
                        assert!(
                            da < 0.5,
                            "azimuth {lat},{lon} day {day} {m}: {ours:?} vs {reference:?}"
                        );
                    }
                }
            }
        }
        assert!(worst.0 > 0.0 && worst.1 > 0.0, "two formulas, not one");
    }

    #[test]
    fn noon_on_the_solstices() {
        // At solar noon the sun stands at 90° - latitude + declination
        // (±23.44° on the solstices), due south north of the tropics and
        // due north south of them.
        let noon = |day: u16, lat: f64, lon: f64| {
            (0..1440)
                .map(|m| position(day, f64::from(m), lat, lon))
                .max_by(|a, b| a.elevation.total_cmp(&b.elevation))
                .unwrap()
        };
        let cases = [
            // (day, latitude, longitude, elevation, azimuth)
            (172, 48.85, 2.35, 64.59, 180.0),  // Paris, 21 June
            (355, 48.85, 2.35, 17.71, 180.0),  // Paris, 21 December
            (172, -33.87, 151.21, 32.69, 0.0), // Sydney, 21 June (its winter)
            (355, -33.87, 151.21, 79.57, 0.0), // Sydney, 21 December
        ];
        for (day, lat, lon, elevation, azimuth) in cases {
            let p = noon(day, lat, lon);
            assert!(
                (p.elevation - elevation).abs() < 0.3,
                "{lat} day {day}: {p:?}"
            );
            assert!(
                angle_gap(p.azimuth, azimuth) < 2.0,
                "{lat} day {day}: {p:?}"
            );
        }
    }

    #[test]
    fn paris_sunrise_points_north_east_in_june_south_east_in_december() {
        // The Almanac's: 51.6° on 21 June, 126.0° on 21 December.
        for (day, azimuth) in [(172, 51.6), (355, 126.0)] {
            let (rise, _) = rise_set(day, 48.85, 2.35).unwrap();
            let p = position(day, rise, 48.85, 2.35);
            assert!((p.azimuth - azimuth).abs() < 1.0, "day {day}: {p:?}");
        }
    }

    #[test]
    fn sunrise_and_sunset_are_where_the_elevation_says() {
        // The same formulas: at the times rise_set gives, the sun's centre
        // is at the horizon of sunrise, and the elevation climbs after
        // sunrise and drops after sunset.
        for (lat, lon) in PLACES {
            for day in (1..=365).step_by(5) {
                let Some((rise, set)) = rise_set(day, lat, lon) else {
                    continue;
                };
                for at in [rise, set] {
                    let e = position(day, at, lat, lon).elevation;
                    assert!(
                        (e - HORIZON).abs() < 0.2,
                        "{lat},{lon} day {day} at {at}: {e}"
                    );
                }
                let up = |m: f64| position(day, m, lat, lon).elevation;
                assert!(up(rise + 10.0) > up(rise), "{lat} day {day}: rising");
                assert!(up(set + 10.0) < up(set), "{lat} day {day}: setting");
            }
        }
    }

    #[test]
    fn polar_night_and_polar_day_have_no_sunrise() {
        assert!(rise_set(355, 80.0, 0.0).is_none());
        assert!(rise_set(172, 80.0, 0.0).is_none());
        assert!(
            position(355, 720.0, 80.0, 0.0).elevation < 0.0,
            "night all day"
        );
        assert!(
            position(172, 0.0, 80.0, 0.0).elevation > 0.0,
            "midnight sun"
        );
    }
}
