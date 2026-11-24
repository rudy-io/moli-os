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

#[derive(Deserialize)]
struct DailyAnswer {
    #[serde(default)]
    grandeur: Vec<DailyQuantity>,
}

#[derive(Deserialize)]
struct DailyQuantity {
    #[serde(rename = "grandeurPhysique")]
    physical: String,
    unite: String,
    #[serde(default)]
    points: Vec<DailyPoint>,
}

#[derive(Deserialize)]
struct DailyPoint {
    v: Option<String>,
    d: String,
}

/// One day's consumption, from the daily totals: no peak, no hours.
#[derive(Clone, Debug, PartialEq)]
pub struct Total {
    pub date: Date,
    pub kwh: f64,
}

/// The days of a `consommation_quotidienne` answer (energy, in Wh, one point
/// a day), oldest first: what is left when Enedis does not serve the hourly
/// curve.
pub fn totals(body: &[u8]) -> Result<Vec<Total>, serde_json::Error> {
    let answer: DailyAnswer = serde_json::from_slice(body)?;
    let days: BTreeMap<Date, f64> = answer
        .grandeur
        .iter()
        .filter(|q| q.physical == "EA" && q.unite == "Wh")
        .flat_map(|q| &q.points)
        .filter_map(|point| {
            let date: Date = point.d.parse().ok()?;
            let wh = point.v.as_deref()?.parse::<f64>().ok()?;
            (wh.is_finite() && wh >= 0.0).then_some((date, wh.round() / 1000.0))
        })
        .collect();
    Ok(days
        .into_iter()
        .map(|(date, kwh)| Total { date, kwh })
        .collect())
}

/// The last day of a daily answer.
#[cfg(test)]
fn last_total(body: &[u8]) -> Result<Option<Total>, serde_json::Error> {
    Ok(totals(body)?.pop())
}

/// A day's total as the energy recorder takes it: from local midnight to
/// the next (23, 24 or 25 hours), spread over its hours there, and only if
/// none of them is filed yet.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WholeDay {
    /// Start of the day, ms since epoch.
    pub day: u64,
    /// Start of the next day, ms since epoch.
    pub to: u64,
    pub kwh: f64,
}

#[must_use]
pub fn whole_day(total: &Total, tz: &TimeZone) -> Option<WholeDay> {
    let ms = |date: Date| -> Option<u64> {
        let start = date.to_zoned(tz.clone()).ok()?;
        u64::try_from(start.timestamp().as_millisecond()).ok()
    };
    Some(WholeDay {
        day: ms(total.date)?,
        to: ms(total.date.tomorrow().ok()?)?,
        kwh: total.kwh,
    })
}

/// Why Enedis refused, as the Conso API relays it (`error.error_description`,
/// or `error.description`): one short line, without any meter number, to be
/// shown in a warning.
#[must_use]
pub fn reason(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct Refusal {
        error: Option<Detail>,
    }
    #[derive(Deserialize)]
    struct Detail {
        error_description: Option<String>,
        description: Option<String>,
        error: Option<String>,
    }
    let detail = serde_json::from_slice::<Refusal>(body).ok()?.error?;
    let text = detail
        .error_description
        .or(detail.description)
        .or(detail.error)?;
    let mut line = String::new();
    let mut digits = 0;
    for c in text.chars().filter(|c| !c.is_control()) {
        // A run of digits as long as a meter number is no reason: dropped.
        digits = if c.is_ascii_digit() { digits + 1 } else { 0 };
        line.push(c);
        if digits == 10 {
            line.truncate(line.len() - 10);
            line.push('…');
        } else if digits > 10 {
            line.pop();
        }
    }
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line.trim_end_matches(['.', ' ']);
    if line.is_empty() {
        return None;
    }
    Some(if line.chars().count() > 160 {
        line.chars().take(159).chain(['…']).collect()
    } else {
        line.to_owned()
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

    #[test]
    fn a_day_without_its_curve_goes_whole_from_midnight_to_midnight() {
        let tz = TimeZone::get("Europe/Paris").unwrap();
        let day = |d, kwh| super::whole_day(&Total { date: d, kwh }, &tz).unwrap();
        let normal = day(date(2026, 11, 21), 10.0);
        assert_eq!(
            normal.day,
            u64::try_from(
                paris_at("2026-11-21T00:00:00+01:00")
                    .timestamp()
                    .as_millisecond()
            )
            .unwrap()
        );
        let hours = |d: WholeDay| (d.to - d.day) / 3_600_000;
        assert_eq!(hours(normal.clone()), 24);
        assert!((normal.kwh - 10.0).abs() < 1e-9);
        // The clocks change: 25 hours that day, 23 in spring.
        assert_eq!(hours(day(date(2026, 10, 25), 5.0)), 25);
        assert_eq!(hours(day(date(2026, 3, 29), 5.0)), 23);
    }

    #[test]
    fn a_daily_answer_gives_its_days_in_order() {
        let body = br#"{"grandeur":[{"grandeurPhysique":"EA","unite":"Wh","points":[{"v":"2000","d":"2026-11-20"},{"v":"1000","d":"2026-11-18"}]}]}"#;
        let days = totals(body).unwrap();
        assert_eq!(
            days.iter().map(|d| d.date).collect::<Vec<_>>(),
            vec![date(2026, 11, 18), date(2026, 11, 20)]
        );
    }

    #[test]
    fn a_daily_answer_gives_its_last_day() {
        let body = br#"{"pas":"P1D","grandeur":[{"grandeurMetier":"CONS","grandeurPhysique":"EA","unite":"Wh","points":[{"v":"13838","d":"2026-11-15"},{"v":"11789","d":"2026-11-21"},{"v":"9237","d":"2026-11-19"}],"calendrier":[]}],"contexte":[]}"#;
        assert_eq!(
            last_total(body).unwrap(),
            Some(Total {
                date: date(2026, 11, 21),
                kwh: 11.789
            })
        );
        assert_eq!(last_total(b"{}").unwrap(), None);
        let bad = br#"{"grandeur":[{"grandeurPhysique":"EA","unite":"Wh","points":[{"v":"-1","d":"2026-11-21"},{"v":null,"d":"2026-11-22"},{"v":"5","d":"hier"}]}]}"#;
        assert_eq!(last_total(bad).unwrap(), None);
        let power = br#"{"grandeur":[{"grandeurPhysique":"PMA","unite":"VA","points":[{"v":"5000","d":"2026-11-21"}]}]}"#;
        assert_eq!(last_total(power).unwrap(), None);
        assert!(last_total(b"nope").is_err());
    }

    #[test]
    fn a_refusal_says_why_in_one_short_line_without_a_meter_number() {
        let missing = br#"{"status":404,"message":"The Enedis API returned an error","error":{"error":"Not Found","error_description":"La sous-ressource demand\u00e9e n existe pas."}}"#;
        assert_eq!(
            reason(missing).as_deref(),
            Some("La sous-ressource demandée n existe pas")
        );
        let quota = br#"{"status":429,"error":{"code":"900804","message":"Message throttled out","description":"You have exceeded your quota .You can access API after 2026-Jul-20 05:00:00+0000 UTC"}}"#;
        assert!(
            reason(quota)
                .unwrap()
                .starts_with("You have exceeded your quota")
        );
        let bare = br#"{"error":{"error":"no_data_found"}}"#;
        assert_eq!(reason(bare).as_deref(), Some("no_data_found"));
        let prm = br#"{"error":{"error_description":"Point 12345678901234 inconnu\n"}}"#;
        assert_eq!(reason(prm).as_deref(), Some("Point … inconnu"));
        let long = format!(
            r#"{{"error":{{"error_description":"{}"}}}}"#,
            "a".repeat(400)
        );
        assert_eq!(reason(long.as_bytes()).unwrap().chars().count(), 160);
        assert_eq!(reason(b"{}"), None);
        assert_eq!(reason(b"<html>"), None);
        assert_eq!(reason(br#"{"error":"text"}"#), None);
    }
}
