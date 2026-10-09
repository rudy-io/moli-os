//! A demo house's past: a year of hourly consumption, so its energy pages
//! show months, seasons and habits from the first visit (`moli-os energy
//! demo`, run before the first start: Moli files history only before it
//! records live).
//!
//! Shapes are a family villa's, recognised from the meter's id: a water
//! heater at night, air conditioning in summer, a pool pump on sunny days,
//! a car charging some nights, cooking at meal times. The grid's two Linky
//! indices (`…-hc`, `…-hp`) take the whole house, by tariff period; the
//! total (`general`) is the sum of its circuits.

use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};

/// One hour of one meter: `(meter id, start of the hour in ms UTC, kWh)`.
pub type Row = (String, u64, f64);

/// Same numbers at every run for a given hour (no random source needed).
fn noise(meter: &str, hour_ms: u64) -> f64 {
    let mut x = hour_ms ^ 0x9E37_79B9_7F4A_7C15;
    for b in meter.bytes() {
        x = x.rotate_left(5) ^ u64::from(b);
        x = x.wrapping_mul(0x0100_0000_01b3);
    }
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    #[allow(clippy::cast_precision_loss)]
    let f = (x >> 11) as f64 / (1u64 << 53) as f64;
    f
}

/// 0 in winter, 1 in high summer (day of year).
fn summer(day: i16) -> f64 {
    let x = (f64::from(day) - 200.0) / 365.0 * std::f64::consts::TAU;
    (0.5 + 0.5 * x.cos()).powi(2)
}

/// Off-peak hours (heures creuses): 22:00–06:00.
fn off_peak(hour: i8) -> bool {
    !(6..22).contains(&hour)
}

/// kWh for one hour of a circuit or an appliance, by what its id says.
fn kwh(meter: &str, hour: i8, day: i16, weekday: i8, n: f64) -> f64 {
    let s = summer(day);
    let winter = 1.0 - s;
    let h = f64::from(hour);
    let at = |center: f64, width: f64| (-((h - center) / width).powi(2)).exp();
    match meter {
        m if m.contains("chauffe") => {
            if (2..5).contains(&hour) {
                2.2 + 0.4 * n
            } else {
                0.0
            }
        }
        m if m.contains("clim") => {
            // Cooling on summer afternoons and evenings, heating on winter
            // mornings and evenings; the pool pump on sunny days.
            let cooling = s * (1.4 * at(16.0, 3.5) + 0.6 * at(22.0, 2.0));
            let heating = winter * (0.9 * at(7.5, 1.5) + 0.8 * at(20.0, 2.5));
            let pump = if (10..18).contains(&hour) {
                0.75 * (0.3 + 0.7 * s)
            } else {
                0.0
            };
            (cooling + heating) * (0.8 + 0.4 * n) + pump
        }
        m if m.contains("voiture") => {
            // Two or three nights a week, a few hours each.
            let night = n < 0.42 && (off_peak(hour) && !(4..6).contains(&hour));
            if night { 3.6 } else { 0.0 }
        }
        m if m.contains("cuisine") => {
            0.08 + 1.1 * at(12.5, 1.0) + 1.5 * at(19.75, 1.0) * (0.7 + 0.6 * n)
        }
        m if m.contains("prise") => 0.12 + 0.25 * at(9.0, 2.0) + 0.35 * at(21.0, 2.5) + 0.05 * n,
        m if m.contains("cinema") => {
            let evening = if weekday >= 5 { 0.16 } else { 0.11 };
            evening * at(21.5, 1.5) + 0.008
        }
        m if m.contains("vaisselle") => {
            if hour == 21 && n < 0.8 {
                1.15
            } else {
                0.0
            }
        }
        m if m.contains("atelier") => {
            if (9..19).contains(&hour) && n < 0.5 {
                0.18
            } else {
                0.004
            }
        }
        m if m.contains("cave") => 0.05 + 0.04 * s,
        _ => 0.0,
    }
}

/// The past `days` days, up to the hour before `now`, for the given meters
/// (`id`). Meters the shapes do not know get nothing.
#[must_use]
pub fn history(meters: &[String], tz: &TimeZone, now: Timestamp, days: u32) -> Vec<Row> {
    let circuits: Vec<&String> = meters
        .iter()
        .filter(|m| !m.ends_with("-hc") && !m.ends_with("-hp") && m.as_str() != "general")
        .collect();
    let mut rows = Vec::new();
    let Ok(start) = now.checked_sub((i64::from(days) * 24).hours()) else {
        return rows;
    };
    #[allow(clippy::cast_sign_loss)]
    let first = (start.as_millisecond() as u64) / 3_600_000 * 3_600_000;
    #[allow(clippy::cast_sign_loss)]
    let last = (now.as_millisecond() as u64) / 3_600_000 * 3_600_000;
    let mut hour_ms = first;
    while hour_ms < last {
        #[allow(clippy::cast_possible_wrap)]
        let Ok(ts) = Timestamp::from_millisecond(hour_ms as i64) else {
            break;
        };
        let local = ts.to_zoned(tz.clone());
        let (hour, day, weekday) = (
            local.hour(),
            local.day_of_year(),
            local.weekday().to_monday_zero_offset(),
        );
        // The circuits the shapes know (appliances are inside them: they
        // are not added to the house).
        let mut house = 0.0;
        for m in &circuits {
            let v = kwh(m, hour, day, weekday, noise(m, hour_ms));
            if v > 0.0 {
                rows.push(((*m).clone(), hour_ms, (v * 1000.0).round() / 1000.0));
                if !["cinema", "vaisselle", "atelier", "cave"]
                    .iter()
                    .any(|a| m.contains(a))
                {
                    house += v;
                }
            }
        }
        // What no circuit measures: fridges, the network, standby.
        house += 0.18 + 0.05 * noise("reste", hour_ms);
        let house = (house * 1000.0).round() / 1000.0;
        if meters.iter().any(|m| m == "general") {
            rows.push(("general".to_owned(), hour_ms, house));
        }
        let grid = if off_peak(hour) { "-hc" } else { "-hp" };
        if let Some(m) = meters.iter().find(|m| m.ends_with(grid)) {
            rows.push((m.clone(), hour_ms, house));
        }
        hour_ms += 3_600_000;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_year_looks_like_a_family_villa() {
        let tz = TimeZone::get("Europe/Paris").unwrap();
        let now: Timestamp = "2026-10-09T12:00:00Z".parse().unwrap();
        let meters: Vec<String> = [
            "linky-hc",
            "linky-hp",
            "general",
            "chauffe-eau",
            "clims-piscine",
            "voiture",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let rows = history(&meters, &tz, now, 365);
        let total: f64 = rows.iter().filter(|r| r.0 == "general").map(|r| r.2).sum();
        let grid: f64 = rows
            .iter()
            .filter(|r| r.0.starts_with("linky"))
            .map(|r| r.2)
            .sum();
        assert!((grid - total).abs() < 1.0, "the grid takes the whole house");
        assert!(
            (5_000.0..20_000.0).contains(&total),
            "a villa's year: {total} kWh"
        );
        // Same hours, same numbers.
        assert_eq!(rows, history(&meters, &tz, now, 365));
        assert!(rows.iter().all(|r| r.1 % 3_600_000 == 0 && r.2 >= 0.0));
    }
}
