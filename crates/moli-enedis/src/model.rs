//! The Conso API's answers and when to ask. Pure: no network, no clock.

use std::collections::BTreeMap;

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned};
use serde::{Deserialize, Serialize};

const HOUR_MS: i64 = 3_600_000;
/// Seconds in the morning window, from 06:00 (the service asks for one request
/// a day between 6 and 10, at a time that is not round).
const MORNING_WINDOW: u32 = 4 * 3600;
/// Seconds in the second chance's window, from 14:00.
const AFTERNOON_WINDOW: u32 = 3600;

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    grandeur: Vec<Quantity>,
}

#[derive(Deserialize)]
struct Quantity {
    #[serde(rename = "grandeurPhysique")]
    physical: String,
    unite: String,
    #[serde(default)]
    points: Vec<Point>,
}

#[derive(Deserialize)]
struct Point {
    v: Option<String>,
    d: String,
    p: String,
}

/// One whole hour of consumption: what the energy recorder reads.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Hour {
    /// Start of the hour, ms since epoch.
    pub hour: u64,
    pub kwh: f64,
}

/// The last day with every measure.
#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    pub date: Date,
    pub kwh: f64,
    /// Highest average power over one measure.
    pub peak_w: f64,
}

/// What an answer says.
#[derive(Debug, Default, PartialEq)]
pub struct Curve {
    pub hours: Vec<Hour>,
    pub last_day: Option<Day>,
}

fn minutes(step: &str) -> Option<i64> {
    let minutes = match step {
        "PT1H" => 60,
        _ => step.strip_prefix("PT")?.strip_suffix('M')?.parse().ok()?,
    };
    (minutes > 0 && minutes <= 60 && 60 % minutes == 0).then_some(minutes)
}

/// Times are local and name the end of a measure. When the clocks go back the
/// same hour comes twice: the second one follows the first.
fn end_of_measure(text: &str, tz: &TimeZone, previous: Option<Timestamp>) -> Option<Timestamp> {
    let local = DateTime::strptime("%Y-%m-%d %H:%M:%S", text).ok()?;
    let ambiguous = tz.to_ambiguous_timestamp(local);
    let earlier = ambiguous.earlier().ok()?;
    if previous.is_some_and(|seen| earlier <= seen) {
        ambiguous.later().ok()
    } else {
        Some(earlier)
    }
}

#[derive(Default)]
struct Sum {
    kwh: f64,
    minutes: i64,
    peak_w: f64,
}

impl Sum {
    #[allow(clippy::cast_precision_loss)] // minutes: at most 60
    fn add(&mut self, watts: f64, minutes: i64) {
        self.kwh += watts * minutes as f64 / 60.0 / 1000.0;
        self.minutes += minutes;
        self.peak_w = self.peak_w.max(watts);
    }
}

fn day_minutes(date: Date, tz: &TimeZone) -> Option<i64> {
    let start = date.to_zoned(tz.clone()).ok()?.timestamp();
    let end = date.tomorrow().ok()?.to_zoned(tz.clone()).ok()?.timestamp();
    Some((end.as_second() - start.as_second()) / 60)
}

/// A measure missing leaves its hour and day out, for a later answer to fill:
/// a partial hour, once filed, would stay wrong.
pub fn curve(body: &[u8], tz: &TimeZone) -> Result<Curve, serde_json::Error> {
    let answer: Answer = serde_json::from_slice(body)?;
    let mut hours: BTreeMap<i64, Sum> = BTreeMap::new();
    let mut days: BTreeMap<Date, Sum> = BTreeMap::new();
    let measures = answer
        .grandeur
        .iter()
        .filter(|q| q.physical == "PA" && q.unite == "W")
        .flat_map(|q| &q.points);
    let mut previous = None;
    for point in measures {
        let Some(end) = end_of_measure(&point.d, tz, previous) else {
            continue;
        };
        previous = Some(end);
        let watts = point.v.as_deref().and_then(|v| v.parse::<f64>().ok());
        let (Some(watts), Some(step)) = (
            watts.filter(|w| w.is_finite() && *w >= 0.0),
            minutes(&point.p),
        ) else {
            continue;
        };
        let Ok(start) = end.checked_sub((step * 60).seconds()) else {
            continue;
        };
        let hour = start.as_millisecond().div_euclid(HOUR_MS) * HOUR_MS;
        hours.entry(hour).or_default().add(watts, step);
        days.entry(start.to_zoned(tz.clone()).date())
            .or_default()
            .add(watts, step);
    }
    Ok(Curve {
        hours: hours
            .into_iter()
            .filter(|(_, sum)| sum.minutes == 60)
            .filter_map(|(hour, sum)| {
                Some(Hour {
                    hour: u64::try_from(hour).ok()?,
                    kwh: (sum.kwh * 1000.0).round() / 1000.0,
                })
            })
            .collect(),
        last_day: days
            .into_iter()
            .rev()
            .find(|(date, sum)| day_minutes(*date, tz) == Some(sum.minutes))
            .map(|(date, sum)| Day {
                date,
                kwh: (sum.kwh * 1000.0).round() / 1000.0,
                peak_w: sum.peak_w,
            }),
    })
}

/// A number proper to this meter: homes do not all ask at the same second.
#[must_use]
pub fn spread(prm: &str) -> u32 {
    prm.bytes().fold(2_166_136_261_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(16_777_619)
    })
}

fn at(date: Date, hour: i8, seconds: u32, tz: &TimeZone) -> Option<Zoned> {
    let start = date.at(hour, 0, 0, 0).to_zoned(tz.clone()).ok()?;
    start.checked_add(i64::from(seconds).seconds()).ok()
}

/// When to ask next. Each day has two moments, fixed for this meter, so that a
/// restart never adds a request: the morning, and the afternoon if the day
/// before is still missing. `done` is the last day received.
#[must_use]
pub fn next_try(now: &Zoned, done: Option<Date>, spread: u32) -> Option<Zoned> {
    let tz = now.time_zone();
    let today = now.date();
    let morning = |date| at(date, 6, spread % MORNING_WINDOW, tz);
    let afternoon = at(today, 14, spread % AFTERNOON_WINDOW, tz)?;
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

    fn paris() -> TimeZone {
        TimeZone::get("Europe/Paris").unwrap()
    }

    fn answer(points: &[(&str, &str)]) -> Vec<u8> {
        let points: Vec<_> = points
            .iter()
            .map(|(d, v)| serde_json::json!({ "v": v, "d": d, "p": "PT15M" }))
            .collect();
        serde_json::json!({
            "grandeur": [{ "grandeurPhysique": "PA", "unite": "W", "points": points }]
        })
        .to_string()
        .into_bytes()
    }

    fn ms(text: &str) -> u64 {
        u64::try_from(text.parse::<Timestamp>().unwrap().as_millisecond()).unwrap()
    }

    #[test]
    fn reads_the_captured_answer_and_leaves_incomplete_hours_out() {
        let body = include_bytes!("../../../integrations/enedis/fixtures/courbe_de_charge.json");
        let curve = curve(body, &paris()).unwrap();
        // 500, 620, 410, 300 W for 15 minutes each; the second hour lacks a measure.
        assert_eq!(
            curve.hours,
            vec![Hour {
                hour: ms("2000-01-02T23:00:00Z"),
                kwh: 0.458
            }]
        );
        assert_eq!(curve.last_day, None);
    }

    #[test]
    fn an_empty_or_foreign_answer_says_nothing() {
        assert_eq!(curve(b"{}", &paris()).unwrap(), Curve::default());
        let other = br#"{"grandeur":[{"grandeurPhysique":"E","unite":"Wh","points":[{"v":"9","d":"2026-01-01 00:15:00","p":"PT15M"}]}]}"#;
        assert_eq!(curve(other, &paris()).unwrap(), Curve::default());
        assert!(curve(b"nope", &paris()).is_err());
    }

    #[test]
    fn a_bad_measure_never_counts() {
        let body = answer(&[
            ("2026-01-01 00:15:00", "-5"),
            ("2026-01-01 00:30:00", "100"),
            ("2026-01-01 00:45:00", "100"),
            ("2026-01-01 01:00:00", "100"),
        ]);
        assert_eq!(curve(&body, &paris()).unwrap().hours, vec![]);
    }

    fn whole_day(start: &str, count: usize) -> Curve {
        let tz = paris();
        let mut t = format!("{start}T00:00:00+00:00[UTC]")
            .parse::<Zoned>()
            .unwrap();
        t = t.date().to_zoned(tz.clone()).unwrap();
        let mut texts = Vec::new();
        for _ in 0..count {
            t = t.checked_add(15.minutes()).unwrap();
            texts.push(t.strftime("%Y-%m-%d %H:%M:%S").to_string());
        }
        let points: Vec<(&str, &str)> = texts.iter().map(|d| (d.as_str(), "1000")).collect();
        curve(&answer(&points), &tz).unwrap()
    }

    #[test]
    fn a_whole_day_ends_at_midnight() {
        let curve = whole_day("2026-11-02", 96);
        let day = curve.last_day.unwrap();
        assert_eq!(day.date, date(2026, 11, 2));
        assert!((day.kwh - 24.0).abs() < 1e-9);
        assert!((day.peak_w - 1000.0).abs() < 1e-9);
        assert_eq!(curve.hours.len(), 24);
        assert_eq!(curve.hours[0].hour, ms("2026-11-01T23:00:00Z"));
    }

    #[test]
    fn days_of_clock_change_have_their_true_length() {
        // Clocks forward: 23 hours. Back: 25, the repeated hour kept apart.
        let short = whole_day("2026-03-29", 92);
        assert_eq!(short.last_day.unwrap().date, date(2026, 3, 29));
        assert_eq!(short.hours.len(), 23);
        let long = whole_day("2026-10-25", 100);
        let day = long.last_day.unwrap();
        assert_eq!(day.date, date(2026, 10, 25));
        assert!((day.kwh - 25.0).abs() < 1e-9);
        assert_eq!(long.hours.len(), 25);
        assert!(long.hours.iter().all(|h| (h.kwh - 1.0).abs() < 1e-9));
    }

    #[test]
    fn steps_are_read_or_refused() {
        assert_eq!(minutes("PT15M"), Some(15));
        assert_eq!(minutes("PT30M"), Some(30));
        assert_eq!(minutes("PT1H"), Some(60));
        assert_eq!(minutes("PT7M"), None);
        assert_eq!(minutes("P1D"), None);
    }

    fn paris_at(text: &str) -> Zoned {
        format!("{text}[Europe/Paris]").parse().unwrap()
    }

    #[test]
    fn asks_in_the_morning_then_in_the_afternoon_then_the_next_day() {
        let spread = 3661; // 07:01:01 and 15:01:01
        let early = paris_at("2026-11-03T05:00:00+01:00");
        let next = next_try(&early, Some(date(2026, 11, 1)), spread).unwrap();
        assert_eq!(next, paris_at("2026-11-03T07:01:01+01:00"));
        let after = next_try(&next, Some(date(2026, 11, 1)), spread).unwrap();
        assert_eq!(after, paris_at("2026-11-03T14:01:01+01:00"));
        let late = next_try(&after, Some(date(2026, 11, 1)), spread).unwrap();
        assert_eq!(late, paris_at("2026-11-04T07:01:01+01:00"));
    }

    #[test]
    fn received_means_nothing_more_today_even_after_a_restart() {
        let now = paris_at("2026-11-03T05:00:00+01:00");
        let next = next_try(&now, Some(date(2026, 11, 2)), 0).unwrap();
        assert_eq!(next, paris_at("2026-11-04T06:00:00+01:00"));
    }

    #[test]
    fn the_time_stays_in_the_windows_the_service_asks_for() {
        let now = paris_at("2026-11-03T00:00:00+01:00");
        for prm in ["00000000000000", "12345678901234", "99999999999999"] {
            let next = next_try(&now, None, spread(prm)).unwrap();
            assert!((6..10).contains(&next.hour()), "{next}");
        }
    }
}
