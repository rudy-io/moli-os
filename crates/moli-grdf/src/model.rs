//! GRDF's daily readings and when to ask. Pure: no network, no clock.
//!
//! A reading covers one « journée gazière » (06:00 to 06:00 the next day). Its
//! energy (kWh) is GRDF's: the volume (m³) times the conversion factor of that
//! day. The meter sends nothing finer: for the energy page, a day's kWh are
//! spread evenly over its hours.

use std::collections::BTreeMap;

use jiff::civil::Date;
use jiff::{Timestamp, ToSpan, Zoned};
use serde::{Deserialize, Serialize};

const HOUR_MS: i64 = 3_600_000;
/// Seconds in the morning window, from 08:00 (GRDF publishes the day before
/// during the night or the morning, often later).
const MORNING_WINDOW: u32 = 3 * 3600;
/// Seconds in the second chance's window, from 16:00.
const AFTERNOON_WINDOW: u32 = 3600;

#[derive(Deserialize)]
struct Meter {
    #[serde(default)]
    releves: Vec<Record>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    journee_gaziere: Option<String>,
    date_debut_releve: Option<String>,
    date_fin_releve: Option<String>,
    index_fin: Option<f64>,
    volume_brut_consomme: Option<f64>,
    energie_consomme: Option<f64>,
    coeff_conversion: Option<f64>,
}

/// One gas day with its consumption.
#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    pub date: Date,
    pub kwh: f64,
    pub m3: Option<f64>,
    /// The meter's index at the end of the day, in m³.
    pub index: Option<f64>,
    /// kWh per m³ that day.
    pub factor: Option<f64>,
    pub start: Timestamp,
    pub end: Timestamp,
}

/// One hour of a day's even share: what the energy recorder reads.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Hour {
    /// Start of the hour, ms since epoch.
    pub hour: u64,
    pub kwh: f64,
}

fn number(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && *v >= 0.0)
}

/// The days of an answer, oldest first. GRDF answers `[]` when it has none;
/// a day not yet measured (no energy) is left out, for a later answer to fill.
pub fn days(body: &[u8], pce: &str) -> Result<Vec<Day>, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_slice(body)?;
    if value.as_array().is_some_and(Vec::is_empty) {
        return Ok(Vec::new());
    }
    let mut meters: BTreeMap<String, Meter> = serde_json::from_value(value)?;
    let Some(meter) = meters.remove(pce) else {
        return Ok(Vec::new());
    };
    let mut days: BTreeMap<Date, Day> = BTreeMap::new();
    for record in meter.releves {
        let date = record
            .journee_gaziere
            .as_deref()
            .and_then(|d| d.parse::<Date>().ok());
        let (Some(date), Some(kwh)) = (date, number(record.energie_consomme)) else {
            continue;
        };
        let instant = |text: Option<&str>| text.and_then(|t| t.parse::<Timestamp>().ok());
        // The day's bounds, or the usual ones: 06:00 UTC, 24 hours.
        let usual = date
            .at(6, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .map(|z| z.timestamp());
        let Some(start) = instant(record.date_debut_releve.as_deref()).or_else(|| usual.ok())
        else {
            continue;
        };
        let end = instant(record.date_fin_releve.as_deref())
            .or_else(|| start.checked_add(24.hours()).ok());
        let Some(end) = end.filter(|end| {
            let hours = (end.as_millisecond() - start.as_millisecond()) / HOUR_MS;
            (20..=28).contains(&hours) && start.as_millisecond() % HOUR_MS == 0
        }) else {
            continue;
        };
        days.insert(
            date,
            Day {
                date,
                kwh,
                m3: number(record.volume_brut_consomme),
                index: number(record.index_fin),
                factor: number(record.coeff_conversion).filter(|f| *f > 0.0),
                start,
                end,
            },
        );
    }
    Ok(days.into_values().collect())
}

/// A day's kWh, the same share each hour, rounded to the Wh; the last hour
/// takes what rounding left, so the day adds up exactly.
#[must_use]
pub fn hours(day: &Day) -> Vec<Hour> {
    let first = day.start.as_millisecond();
    let count = (day.end.as_millisecond() - first) / HOUR_MS;
    let Ok(n) = u32::try_from(count) else {
        return Vec::new();
    };
    if n == 0 {
        return Vec::new();
    }
    let share = (day.kwh / f64::from(n) * 1000.0).round() / 1000.0;
    let mut left = day.kwh;
    (0..n)
        .filter_map(|i| {
            let kwh = if i + 1 == n {
                (left * 1000.0).round() / 1000.0
            } else {
                left -= share;
                share
            };
            Some(Hour {
                hour: u64::try_from(first + i64::from(i) * HOUR_MS).ok()?,
                kwh: kwh.max(0.0),
            })
        })
        .collect()
}

/// A number proper to this meter: homes do not all ask at the same second.
#[must_use]
pub fn spread(pce: &str) -> u32 {
    pce.bytes().fold(2_166_136_261_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(16_777_619)
    })
}

fn at(date: Date, hour: i8, seconds: u32, tz: &jiff::tz::TimeZone) -> Option<Zoned> {
    let start = date.at(hour, 0, 0, 0).to_zoned(tz.clone()).ok()?;
    start.checked_add(i64::from(seconds).seconds()).ok()
}

/// When to ask next. Each day has two moments, fixed for this meter, so that a
/// restart never adds a sign-in: the morning, and the afternoon if the day
/// before is still missing. `done` is the last day received.
#[must_use]
pub fn next_try(now: &Zoned, done: Option<Date>, spread: u32) -> Option<Zoned> {
    let tz = now.time_zone();
    let today = now.date();
    let morning = |date| at(date, 8, spread % MORNING_WINDOW, tz);
    let afternoon = at(today, 16, spread % AFTERNOON_WINDOW, tz)?;
    let tomorrow = today.tomorrow().ok()?;
    if done.is_some_and(|day| day >= today.yesterday().unwrap_or(today)) {
        return morning(tomorrow);
    }
    let first = morning(today)?;
    if *now < first {
        Some(first)
    } else if *now < afternoon {
        Some(afternoon)
    } else {
        morning(tomorrow)
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    const PCE: &str = "00000000000000";

    fn fixture() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../integrations/grdf/fixtures/informatives.json"
        ))
        .unwrap()
    }

    #[test]
    fn reads_the_captured_answer_and_leaves_unmeasured_days_out() {
        let days = days(&fixture(), PCE).unwrap();
        let dates: Vec<_> = days.iter().map(|d| d.date).collect();
        // The last day has no energy yet: GRDF has not measured it.
        assert_eq!(
            dates,
            vec![date(2000, 1, 1), date(2000, 1, 2), date(2000, 1, 3)]
        );
        let last = &days[2];
        assert!((last.kwh - 102.0).abs() < 1e-9);
        assert_eq!(last.m3, Some(9.0));
        assert_eq!(last.index, Some(1029.0));
        assert_eq!(last.factor, Some(11.31));
        assert_eq!(
            last.start,
            "2000-01-03T05:00:00Z".parse::<Timestamp>().unwrap()
        );
    }

    #[test]
    fn another_meter_or_nothing_says_nothing() {
        assert_eq!(days(&fixture(), "99999999999999").unwrap(), vec![]);
        assert_eq!(days(b"[]", PCE).unwrap(), vec![]);
        assert!(days(b"nope", PCE).is_err());
    }

    #[test]
    fn absurd_values_never_count() {
        let body = serde_json::json!({ PCE: { "releves": [
            { "journeeGaziere": "2026-01-01", "energieConsomme": -3 },
            { "journeeGaziere": "2026-01-02", "energieConsomme": 10,
              "dateDebutReleve": "2026-01-02T05:00:00Z", "dateFinReleve": "2026-01-02T06:00:00Z" },
            { "journeeGaziere": "2026-01-03", "energieConsomme": 24 }
        ] } })
        .to_string();
        let days = days(body.as_bytes(), PCE).unwrap();
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].date, date(2026, 1, 3));
        // Without bounds: 06:00 UTC, 24 hours.
        assert_eq!(
            days[0].start,
            "2026-01-03T06:00:00Z".parse::<Timestamp>().unwrap()
        );
    }

    #[test]
    fn a_day_is_spread_evenly_and_adds_up() {
        let day = &days(&fixture(), PCE).unwrap()[2];
        let hours = hours(day);
        assert_eq!(hours.len(), 24);
        assert_eq!(
            hours[0].hour,
            u64::try_from(day.start.as_millisecond()).unwrap()
        );
        assert!((hours[0].kwh - 4.25).abs() < 1e-9);
        let total: f64 = hours.iter().map(|h| h.kwh).sum();
        assert!((total - 102.0).abs() < 1e-6, "{total}");
        // An awkward amount: rounding goes to the last hour.
        let odd = Day {
            kwh: 10.0,
            ..day.clone()
        };
        let hours = super::hours(&odd);
        let total: f64 = hours.iter().map(|h| h.kwh).sum();
        assert!((total - 10.0).abs() < 1e-6, "{total}");
        assert!(hours.iter().all(|h| h.kwh >= 0.0));
    }

    fn paris_at(text: &str) -> Zoned {
        format!("{text}[Europe/Paris]").parse().unwrap()
    }

    #[test]
    fn asks_in_the_morning_then_in_the_afternoon_then_the_next_day() {
        let spread = 3661; // 09:01:01 and 16:01:01
        let early = paris_at("2026-11-03T05:00:00+01:00");
        let next = next_try(&early, Some(date(2026, 11, 1)), spread).unwrap();
        assert_eq!(next, paris_at("2026-11-03T09:01:01+01:00"));
        let after = next_try(&next, Some(date(2026, 11, 1)), spread).unwrap();
        assert_eq!(after, paris_at("2026-11-03T16:01:01+01:00"));
        let late = next_try(&after, Some(date(2026, 11, 1)), spread).unwrap();
        assert_eq!(late, paris_at("2026-11-04T09:01:01+01:00"));
    }

    #[test]
    fn received_means_nothing_more_today_even_after_a_restart() {
        let now = paris_at("2026-11-03T05:00:00+01:00");
        let next = next_try(&now, Some(date(2026, 11, 2)), 0).unwrap();
        assert_eq!(next, paris_at("2026-11-04T08:00:00+01:00"));
    }

    #[test]
    fn the_time_stays_in_the_morning_window() {
        let now = paris_at("2026-11-03T00:00:00+01:00");
        for pce in ["00000000000000", "12345678901234", "99999999999999"] {
            let next = next_try(&now, None, spread(pce)).unwrap();
            assert!((8..11).contains(&next.hour()), "{next}");
        }
    }
}
