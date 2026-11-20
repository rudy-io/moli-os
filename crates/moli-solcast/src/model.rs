//! Solcast's rooftop forecasts, made the house's: half-hours of expected
//! production (kW) with a low (10th percentile) and a high (90th) estimate,
//! kept across answers and summed across sites, then said as the day's
//! totals and a curve. Pure: no clock, no network.
//!
//! A half-hour keeps the last forecast made for it before it passed: the
//! morning's answer draws the whole day, the later ones only redraw what is
//! still ahead. The past part of today's curve thus costs no call.

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Solcast's step (`PT30M`): the only one its rooftop forecasts use.
pub const STEP: i64 = 1800;
/// Two refreshes are never closer than this (seconds).
const MIN_GAP: i64 = 1800;
/// Half-hours kept: two days back is enough for yesterday's curve.
const KEEP_BACK: i64 = 2 * 86_400;

/// One half-hour, in kW (mean power over the half-hour).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    pub kw: f64,
    pub lo: f64,
    pub hi: f64,
}

#[derive(Deserialize)]
struct Answer {
    forecasts: Vec<Forecast>,
}

#[derive(Deserialize)]
struct Forecast {
    pv_estimate: f64,
    pv_estimate10: Option<f64>,
    pv_estimate90: Option<f64>,
    period_end: String,
    #[serde(default)]
    period: Option<String>,
}

/// One answer of `/rooftop_sites/{id}/forecasts`: half-hours by their end
/// (Unix seconds). Another step than 30 minutes, or an unreadable time, is
/// left out rather than misplaced.
pub fn parse(body: &[u8]) -> Result<BTreeMap<i64, Slot>, serde_json::Error> {
    let answer: Answer = serde_json::from_slice(body)?;
    let mut out = BTreeMap::new();
    for f in answer.forecasts {
        if f.period.as_deref().is_some_and(|p| p != "PT30M") {
            continue;
        }
        let Ok(end) = f.period_end.parse::<Timestamp>() else {
            continue;
        };
        let kw = clean(f.pv_estimate);
        let slot = Slot {
            kw,
            lo: f.pv_estimate10.map_or(kw, clean).min(kw),
            hi: f.pv_estimate90.map_or(kw, clean).max(kw),
        };
        out.insert(end.as_second(), slot);
    }
    Ok(out)
}

/// A power as Solcast may send it: never negative, never absurd.
fn clean(kw: f64) -> f64 {
    if kw.is_finite() {
        kw.clamp(0.0, 1_000.0)
    } else {
        0.0
    }
}

/// What the driver keeps (on disk when it can): the forecasts by site, and
/// the calls made in Solcast's day (UTC).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    /// Per site, the half-hours by their end.
    pub sites: BTreeMap<String, BTreeMap<i64, Slot>>,
    /// Solcast's day the count belongs to (`2026-11-20`, UTC).
    pub day: String,
    /// Calls made that day (each site asked is one).
    pub calls: u32,
    /// Solcast said the day's calls are used up.
    pub exhausted: bool,
    /// The last answer received (Unix seconds).
    pub fetched: Option<i64>,
}

impl Cache {
    /// An answer for `site` at `now`: what is still ahead is redrawn, what
    /// has passed stays as it was forecast; old half-hours go.
    pub fn merge(&mut self, site: &str, answer: BTreeMap<i64, Slot>, now: i64) {
        let kept = self.sites.entry(site.to_owned()).or_default();
        for (end, slot) in answer {
            if end > now || !kept.contains_key(&end) {
                kept.insert(end, slot);
            }
        }
        kept.retain(|end, _| *end > now - KEEP_BACK);
        self.fetched = Some(now);
    }

    /// Only the configured sites are kept (a site removed from moli.toml no
    /// longer counts).
    pub fn keep_sites(&mut self, sites: &[String]) {
        self.sites.retain(|s, _| sites.contains(s));
    }

    /// The house's forecast: the sites summed, half-hour by half-hour.
    pub fn total(&self) -> BTreeMap<i64, Slot> {
        let mut out: BTreeMap<i64, Slot> = BTreeMap::new();
        for slots in self.sites.values() {
            for (end, s) in slots {
                let t = out.entry(*end).or_default();
                t.kw += s.kw;
                t.lo += s.lo;
                t.hi += s.hi;
            }
        }
        out
    }

    /// The count of calls, for Solcast's day of `now`.
    pub fn calls_today(&self, now: i64) -> (u32, bool) {
        if self.day == utc_day(now) {
            (self.calls, self.exhausted)
        } else {
            (0, false)
        }
    }

    /// One call made at `now` (counted whatever its answer: Solcast counts it).
    pub fn count_call(&mut self, now: i64) {
        let day = utc_day(now);
        if self.day != day {
            self.day = day;
            self.calls = 0;
            self.exhausted = false;
        }
        self.calls += 1;
    }

    /// Solcast refused for the day (HTTP 429).
    pub fn mark_exhausted(&mut self, now: i64) {
        self.count_call(now);
        self.exhausted = true;
    }
}

/// Solcast's quota day: UTC.
pub fn utc_day(now: i64) -> String {
    Timestamp::from_second(now)
        .map_or_else(|_| String::new(), |t| t.strftime("%Y-%m-%d").to_string())
}

/// Seconds since the epoch of the local midnight `days` after the day of `now`.
fn midnight(now: i64, days: i64, tz: &TimeZone) -> Option<i64> {
    let at = Timestamp::from_second(now).ok()?.to_zoned(tz.clone());
    let date = at.date().checked_add(jiff::Span::new().days(days)).ok()?;
    Some(date.to_zoned(tz.clone()).ok()?.timestamp().as_second())
}

/// kWh of the half-hours (or the part of them) between `from` and `to`.
fn energy(total: &BTreeMap<i64, Slot>, from: i64, to: i64) -> Option<Slot> {
    let mut sum = Slot::default();
    let mut any = false;
    for (end, s) in total.range(from + 1..) {
        let start = end - STEP;
        if start >= to {
            break;
        }
        let secs = i32::try_from(end.min(&to) - start.max(from)).unwrap_or(0);
        let part = f64::from(secs) / 3600.0;
        if part <= 0.0 {
            continue;
        }
        any = true;
        sum.kw += s.kw * part;
        sum.lo += s.lo * part;
        sum.hi += s.hi * part;
    }
    any.then_some(sum)
}

/// What the device says, at `now`.
#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    /// The power expected now (W).
    pub power_now: Option<f64>,
    /// Today's whole day (kWh): expected, low, high.
    pub today: Option<Slot>,
    /// What today still has to give, from now on (kWh).
    pub remaining: Option<Slot>,
    pub tomorrow: Option<Slot>,
    /// The next hour (kWh).
    pub next_hour: Option<f64>,
}

pub fn summary(total: &BTreeMap<i64, Slot>, now: i64, tz: &TimeZone) -> Summary {
    let (Some(today), Some(tomorrow), Some(after)) = (
        midnight(now, 0, tz),
        midnight(now, 1, tz),
        midnight(now, 2, tz),
    ) else {
        return Summary::default();
    };
    let power_now = total
        .range(now + 1..)
        .next()
        .filter(|(end, _)| *end - STEP <= now)
        .map(|(_, s)| (s.kw * 1000.0).round());
    Summary {
        power_now,
        today: energy(total, today, tomorrow),
        remaining: energy(total, now.max(today), tomorrow),
        tomorrow: energy(total, tomorrow, after),
        next_hour: energy(total, now, now + 3600).map(|s| s.kw),
    }
}

/// The curve from yesterday's midnight to tomorrow's end, half-hour by
/// half-hour, as compact as a text point needs (4,000 characters at most):
/// `{"start": ms, "step": 1800, "kw": [...], "lo": [...], "hi": [...]}`, kW
/// to the hundredth, `null` where nothing is known. Yesterday goes first
/// when it would not fit.
pub fn curve(total: &BTreeMap<i64, Slot>, now: i64, tz: &TimeZone) -> Option<String> {
    let end = midnight(now, 2, tz)?;
    for back in [-1, 0] {
        let start = midnight(now, back, tz)?;
        let text = curve_from(total, start, end);
        if text.chars().count() <= moli_core::MAX_TEXT {
            return Some(text);
        }
    }
    None
}

fn curve_from(total: &BTreeMap<i64, Slot>, start: i64, end: i64) -> String {
    let n = ((end - start) / STEP).max(0);
    let cap = usize::try_from(n).unwrap_or(0);
    let (mut kw, mut lo, mut hi) = (
        Vec::with_capacity(cap),
        Vec::with_capacity(cap),
        Vec::with_capacity(cap),
    );
    let hundredth = |v: f64| serde_json::Value::from((v * 100.0).round() / 100.0);
    for i in 0..n {
        if let Some(s) = total.get(&(start + (i + 1) * STEP)) {
            kw.push(hundredth(s.kw));
            lo.push(hundredth(s.lo));
            hi.push(hundredth(s.hi));
        } else {
            kw.push(serde_json::Value::Null);
            lo.push(serde_json::Value::Null);
            hi.push(serde_json::Value::Null);
        }
    }
    serde_json::json!({ "start": start * 1000, "step": STEP, "kw": kw, "lo": lo, "hi": hi })
        .to_string()
}

/// When to ask Solcast next, at `now`. The day's calls (`per_day`, each
/// site asked costing one) are spread over the hours of light, read from
/// the forecast itself (the first and last half-hour expected to produce),
/// 06:00 to 20:00 before any is known; the first refresh at the first light,
/// the last at the last (it also brings tomorrow's forecast).
///
/// Returns `now` when a refresh is due, else the time it will be.
pub fn next_refresh(cache: &Cache, sites: usize, per_day: u32, now: i64, tz: &TimeZone) -> i64 {
    let sites = u32::try_from(sites.max(1)).unwrap_or(u32::MAX);
    let next_utc_day = Timestamp::from_second(now)
        .ok()
        .and_then(|t| t.to_zoned(TimeZone::UTC).date().tomorrow().ok())
        .and_then(|d| d.to_zoned(TimeZone::UTC).ok())
        .map_or(now + 3600, |z| z.timestamp().as_second() + 60);
    let (used, exhausted) = cache.calls_today(now);
    if exhausted || used + sites > per_day {
        return next_utc_day;
    }
    // Nothing known yet: at once.
    let Some(fetched) = cache.fetched.filter(|_| !cache.sites.is_empty()) else {
        return now;
    };
    let refreshes = (per_day / sites).max(1);
    let total = cache.total();
    // The hours of light of a day, from the first to the last half-hour
    // expected to produce, and whether that day's dawn is known (an answer
    // received in the afternoon does not tell when the day began).
    let lit = |day: i64| -> Option<(i64, i64, bool)> {
        let from = midnight(now, day, tz)?;
        let to = midnight(now, day + 1, tz)?;
        let mut slots = total.range(from + 1..=to).peekable();
        let dawn_known = slots.peek().is_some_and(|(_, s)| s.kw <= 0.0);
        let ends: Vec<i64> = slots
            .filter(|(_, s)| s.kw > 0.0)
            .map(|(end, _)| *end)
            .collect();
        Some((*ends.first()? - STEP, *ends.last()?, dawn_known))
    };
    let window = |day: i64| -> Option<(i64, i64)> {
        let from = midnight(now, day, tz)?;
        match lit(day) {
            Some((start, end, true)) => Some((start, end)),
            // Dawn unknown: the next day's light, a day earlier (a 48-hour
            // answer always holds the whole next day); else 06:00 to 20:00.
            _ => match lit(day + 1) {
                Some((start, end, true)) => {
                    let shift = midnight(now, day + 1, tz)? - from;
                    Some((start - shift, end - shift))
                }
                _ => Some((from + 6 * 3600, from + 20 * 3600)),
            },
        }
    };
    for day in [0, 1] {
        let Some((start, end)) = window(day) else {
            continue;
        };
        let times: Vec<i64> = if refreshes == 1 {
            vec![start + (end - start) / 2]
        } else {
            (0..refreshes)
                .map(|i| start + (end - start) * i64::from(i) / i64::from(refreshes - 1))
                .collect()
        };
        if let Some(at) = times.into_iter().find(|at| *at > fetched) {
            // Never two refreshes closer than half an hour.
            return at.max(fetched + MIN_GAP).max(now);
        }
    }
    // Past tomorrow's window (should not happen): in six hours.
    now + 6 * 3600
}
