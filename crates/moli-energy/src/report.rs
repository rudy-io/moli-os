//! Hour slices → days and months in local time, and the headline figures.

use std::collections::BTreeMap;

use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan as _, Zoned};
use serde::{Deserialize, Serialize};

use crate::config::{Config, Role};
use crate::meter::HOUR_MS;
use crate::store::{EnergyError, HourRow};

/// At most this many buckets per report (a year of hours fits).
pub const MAX_BUCKETS: usize = 9_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    Hour,
    Day,
    Month,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Amount {
    pub kwh: f64,
    /// Absent when no price applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

impl Amount {
    fn add(&mut self, kwh: f64, cost: Option<f64>) {
        self.kwh += kwh;
        if let Some(c) = cost {
            *self.cost.get_or_insert(0.0) += c;
        }
    }

    fn sum<'a>(amounts: impl IntoIterator<Item = &'a Self>) -> Self {
        let mut total = Self::default();
        for a in amounts {
            total.add(a.kwh, a.cost);
        }
        total
    }

    /// Rounded for display and agents: Wh and tenths of a cent.
    #[must_use]
    pub fn rounded(&self) -> Self {
        Self {
            kwh: (self.kwh * 1000.0).round() / 1000.0,
            cost: self.cost.map(|c| (c * 1000.0).round() / 1000.0),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Bucket {
    /// Start, ms since epoch (local midnight for days and months).
    pub start: u64,
    pub meters: BTreeMap<String, Amount>,
}

fn zoned(ms: u64, tz: &TimeZone) -> Result<Zoned, EnergyError> {
    let ms = i64::try_from(ms).map_err(|_| EnergyError::Invalid("date out of range".into()))?;
    Timestamp::from_millisecond(ms)
        .map(|t| t.to_zoned(tz.clone()))
        .map_err(|e| EnergyError::Invalid(e.to_string()))
}

fn millis(z: &Zoned) -> u64 {
    u64::try_from(z.timestamp().as_millisecond()).unwrap_or(0)
}

fn bad(e: &jiff::Error) -> EnergyError {
    EnergyError::Invalid(e.to_string())
}

/// Start of the bucket holding `ms`.
pub fn floor(ms: u64, step: Step, tz: &TimeZone) -> Result<u64, EnergyError> {
    match step {
        Step::Hour => Ok(ms - ms % HOUR_MS),
        Step::Day => Ok(millis(&zoned(ms, tz)?.start_of_day().map_err(|e| bad(&e))?)),
        Step::Month => Ok(millis(
            &zoned(ms, tz)?
                .first_of_month()
                .and_then(|z| z.start_of_day())
                .map_err(|e| bad(&e))?,
        )),
    }
}

/// Start of the next bucket (days of 23 or 25 hours across DST). Floored
/// again: where DST skips midnight, a day starts at 01:00 and the next one
/// still starts at its own midnight.
pub fn next(start: u64, step: Step, tz: &TimeZone) -> Result<u64, EnergyError> {
    let later = match step {
        Step::Hour => return Ok(start + HOUR_MS),
        Step::Day => zoned(start, tz)?.checked_add(1.day()),
        Step::Month => zoned(start, tz)?.checked_add(1.month()),
    }
    .map_err(|e| bad(&e))?;
    floor(millis(&later), step, tz)
}

/// Longest series per step: ~a year of hours, ten years of days, fifty
/// years of months.
#[must_use]
pub fn max_count(step: Step) -> usize {
    match step {
        Step::Hour => 24 * 366,
        Step::Day => 3_660,
        Step::Month => 600,
    }
}

/// Buckets covering `[from, to)`, every one present (zeros included).
pub fn buckets(
    rows: &[HourRow],
    from: u64,
    to: u64,
    step: Step,
    tz: &TimeZone,
) -> Result<Vec<Bucket>, EnergyError> {
    let mut starts = Vec::new();
    let mut start = floor(from, step, tz)?;
    while start < to {
        if starts.len() >= MAX_BUCKETS {
            return Err(EnergyError::Invalid(format!(
                "more than {MAX_BUCKETS} buckets: widen the step"
            )));
        }
        starts.push(start);
        start = next(start, step, tz)?;
    }
    let mut out: Vec<Bucket> = starts
        .iter()
        .map(|&start| Bucket {
            start,
            meters: BTreeMap::new(),
        })
        .collect();
    for row in rows {
        // The last bucket starting at or before this hour.
        let i = starts.partition_point(|&s| s <= row.hour);
        if let Some(bucket) = i.checked_sub(1).and_then(|i| out.get_mut(i)) {
            bucket
                .meters
                .entry(row.meter.clone())
                .or_default()
                .add(row.kwh, row.cost);
        }
    }
    for bucket in &mut out {
        for amount in bucket.meters.values_mut() {
            *amount = amount.rounded();
        }
    }
    Ok(out)
}

/// A stretch of time (today, this month…) and what was consumed.
#[derive(Clone, Debug, Serialize)]
pub struct Period {
    pub from: u64,
    pub to: u64,
    /// The headline: the more complete of the billed meters (grid) and the
    /// whole-home measure, else the circuits.
    pub total: Amount,
    pub meters: BTreeMap<String, Amount>,
    /// Whole-home measure minus the circuits: what no circuit explains.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unmeasured: Option<Amount>,
}

impl Period {
    pub(crate) fn of(config: &Config, rows: &[HourRow], from: u64, to: u64) -> Self {
        let mut meters: BTreeMap<String, Amount> = BTreeMap::new();
        for row in rows.iter().filter(|r| from <= r.hour && r.hour < to) {
            meters
                .entry(row.meter.clone())
                .or_default()
                .add(row.kwh, row.cost);
        }
        let of_role = |role: Role| {
            let ids: Vec<&str> = config
                .meters
                .iter()
                .filter(|m| m.role == role)
                .map(|m| m.id.as_str())
                .collect();
            (!ids.is_empty()).then(|| {
                Amount::sum(
                    ids.iter()
                        .filter_map(|id| meters.get(*id))
                        .collect::<Vec<_>>(),
                )
            })
        };
        let (grid, whole, circuits) = (
            of_role(Role::Grid),
            of_role(Role::Total),
            of_role(Role::Circuit),
        );
        let unmeasured = match (&whole, &circuits) {
            (Some(w), Some(c)) => Some(Amount {
                kwh: (w.kwh - c.kwh).max(0.0),
                cost: w.cost.zip(c.cost).map(|(w, c)| (w - c).max(0.0)),
            }),
            _ => None,
        };
        // Both measure the same home; the lower one is missing data (the
        // Linky lags by up to a whole kWh, the clamp has no old history).
        let total = match (grid, whole) {
            (Some(g), Some(w)) => Some(if w.kwh > g.kwh { w } else { g }),
            (g, w) => g.or(w),
        }
        .or(circuits)
        .unwrap_or_default();
        Self {
            from,
            to,
            total: total.rounded(),
            meters: meters.into_iter().map(|(k, a)| (k, a.rounded())).collect(),
            unmeasured: unmeasured.map(|a| a.rounded()),
        }
    }
}

/// This month at the pace so far (after a full day of data).
#[allow(clippy::cast_precision_loss)] // ms spans
pub(crate) fn projection(month: &Period, now: u64, month_end: u64) -> Option<Amount> {
    let elapsed = now.saturating_sub(month.from);
    if elapsed < 24 * HOUR_MS || month_end <= month.from {
        return None;
    }
    let factor = (month_end - month.from) as f64 / elapsed as f64;
    Some(
        Amount {
            kwh: month.total.kwh * factor,
            cost: month.total.cost.map(|c| c * factor),
        }
        .rounded(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paris() -> TimeZone {
        TimeZone::get("Europe/Paris").unwrap()
    }

    fn at(s: &str) -> u64 {
        let z: Zoned = s.parse().unwrap();
        millis(&z)
    }

    #[test]
    fn days_follow_local_midnight_across_dst() {
        let tz = paris();
        // 26 October 2025: clocks go back, the day lasts 25 hours.
        let day = at("2025-10-26T00:00[Europe/Paris]");
        assert_eq!(floor(day + 5 * HOUR_MS, Step::Day, &tz).unwrap(), day);
        assert_eq!(next(day, Step::Day, &tz).unwrap() - day, 25 * HOUR_MS);
        let month = at("2025-10-01T00:00[Europe/Paris]");
        assert_eq!(floor(day, Step::Month, &tz).unwrap(), month);
        assert_eq!(
            next(month, Step::Month, &tz).unwrap(),
            at("2025-11-01T00:00[Europe/Paris]")
        );
    }

    #[test]
    fn rows_land_in_their_local_day_and_empty_days_exist() {
        let tz = paris();
        let d1 = at("2026-10-01T00:00[Europe/Paris]");
        let row = |hour: u64, kwh: f64| HourRow {
            meter: "hp".into(),
            hour,
            kwh,
            cost: Some(kwh * 0.2),
        };
        // 23:00 local on the 1st is 21:00 UTC: still the 1st.
        let rows = [
            row(d1 + 23 * HOUR_MS, 1.0),
            row(d1 + 2 * HOUR_MS, 0.5),
            row(d1 + 49 * HOUR_MS, 2.0),
        ];
        let b = buckets(&rows, d1, d1 + 72 * HOUR_MS, Step::Day, &tz).unwrap();
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].meters["hp"].kwh, 1.5);
        assert_eq!(b[0].meters["hp"].cost, Some(0.3));
        assert!(b[1].meters.is_empty());
        assert_eq!(b[2].meters["hp"].kwh, 2.0);
        assert!(buckets(&rows, 0, u64::MAX / 4, Step::Hour, &tz).is_err());
    }

    #[test]
    fn headline_prefers_the_bill_and_explains_the_rest() {
        let config: Config = toml::from_str(
            r#"
            [[meter]]
            id = "hc"
            name = "HC"
            point = "z2m:l/t1"
            role = "grid"
            [[meter]]
            id = "hp"
            name = "HP"
            point = "z2m:l/t2"
            role = "grid"
            [[meter]]
            id = "general"
            name = "Général"
            point = "p:em/e1"
            role = "total"
            [[meter]]
            id = "ce"
            name = "Chauffe-eau"
            point = "p:em/e4"
            role = "circuit"
            "#,
        )
        .unwrap();
        let row = |meter: &str, kwh: f64, cost: Option<f64>| HourRow {
            meter: meter.into(),
            hour: 0,
            kwh,
            cost,
        };
        let rows = [
            row("hc", 4.0, Some(0.5)),
            row("hp", 6.0, Some(1.0)),
            row("general", 9.5, Some(1.4)),
            row("ce", 3.0, Some(0.4)),
        ];
        let p = Period::of(&config, &rows, 0, HOUR_MS);
        assert_eq!(
            p.total,
            Amount {
                kwh: 10.0,
                cost: Some(1.5)
            }
        );
        assert_eq!(
            p.unmeasured,
            Some(Amount {
                kwh: 6.5,
                cost: Some(1.0)
            })
        );
        assert_eq!(p.meters.len(), 4);
        // The Linky lagging behind the clamp: the clamp wins.
        let lagging = [row("hc", 1.0, Some(0.1)), row("general", 2.5, Some(0.4))];
        let p = Period::of(&config, &lagging, 0, HOUR_MS);
        assert_eq!(
            p.total,
            Amount {
                kwh: 2.5,
                cost: Some(0.4)
            }
        );

        let month = Period::of(&config, &rows, 0, 2 * 24 * HOUR_MS);
        assert_eq!(projection(&month, HOUR_MS, 30 * 24 * HOUR_MS), None);
        let pace = projection(&month, 2 * 24 * HOUR_MS, 30 * 24 * HOUR_MS).unwrap();
        assert_eq!(pace.kwh, 150.0);
    }
}
