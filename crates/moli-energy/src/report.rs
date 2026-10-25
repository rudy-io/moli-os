//! Hour slices → days and months in local time, and the headline figures.

use std::collections::BTreeMap;

use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan as _, Zoned};
use serde::{Deserialize, Serialize};

use crate::config::{Config, Role};
use crate::meter::HOUR_MS;
use crate::store::{EnergyError, HourRow};
use crate::tariff::Contracts;

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
    /// The house's consumption by tariff period (« HC », « HP »), when
    /// dated contracts say the periods.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub by_period: BTreeMap<String, Amount>,
    /// Each meter's consumption by tariff period, when dated contracts say
    /// the periods (a radiator's off-peak and peak kWh).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub meter_periods: BTreeMap<String, BTreeMap<String, Amount>>,
}

/// The meters that make the house's figure, for the split by period: the
/// whole-home measure, else the billed meters, else the circuits.
fn headline(config: &Config) -> Vec<&str> {
    [Role::Total, Role::Grid, Role::Circuit]
        .into_iter()
        .map(|role| {
            config
                .meters
                .iter()
                .filter(|m| m.role == role)
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>()
        })
        .find(|ids| !ids.is_empty())
        .unwrap_or_default()
}

/// Adds a row to the split by period, priced period by period.
fn split_into(by: &mut BTreeMap<String, Amount>, row: &HourRow, contracts: &Contracts) {
    for share in contracts.shares(row.hour, row.hour + HOUR_MS) {
        let kwh = row.kwh * share.part;
        by.entry(share.period.to_owned())
            .or_default()
            .add(kwh, share.price.map(|p| p * kwh));
    }
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
    config: &Config,
    contracts: &Contracts,
    rows: &[HourRow],
    from: u64,
    to: u64,
    step: Step,
    tz: &TimeZone,
) -> Result<Vec<Bucket>, EnergyError> {
    let house = headline(config);
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
            by_period: BTreeMap::new(),
            meter_periods: BTreeMap::new(),
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
            if house.contains(&row.meter.as_str()) {
                split_into(&mut bucket.by_period, row, contracts);
            }
            split_into(
                bucket.meter_periods.entry(row.meter.clone()).or_default(),
                row,
                contracts,
            );
        }
    }
    for bucket in &mut out {
        bucket.meter_periods.retain(|_, by| !by.is_empty());
        for amount in bucket
            .meters
            .values_mut()
            .chain(bucket.by_period.values_mut())
            .chain(
                bucket
                    .meter_periods
                    .values_mut()
                    .flat_map(BTreeMap::values_mut),
            )
        {
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
    /// The house's consumption by tariff period (« HC », « HP »), when
    /// dated contracts say the periods.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub by_period: BTreeMap<String, Amount>,
    /// Each meter's consumption by tariff period, when dated contracts say
    /// the periods.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub meter_periods: BTreeMap<String, BTreeMap<String, Amount>>,
}

impl Period {
    pub(crate) fn of(
        config: &Config,
        contracts: &Contracts,
        rows: &[HourRow],
        from: u64,
        to: u64,
    ) -> Self {
        let house = headline(config);
        let mut by_period = BTreeMap::new();
        let mut meters: BTreeMap<String, Amount> = BTreeMap::new();
        let mut meter_periods: BTreeMap<String, BTreeMap<String, Amount>> = BTreeMap::new();
        for row in rows.iter().filter(|r| from <= r.hour && r.hour < to) {
            if house.contains(&row.meter.as_str()) {
                split_into(&mut by_period, row, contracts);
            }
            split_into(
                meter_periods.entry(row.meter.clone()).or_default(),
                row,
                contracts,
            );
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
            by_period: by_period
                .into_iter()
                .map(|(k, a)| (k, a.rounded()))
                .collect(),
            meter_periods: meter_periods
                .into_iter()
                .filter(|(_, by)| !by.is_empty())
                .map(|(meter, by)| {
                    let by = by.into_iter().map(|(k, a)| (k, a.rounded())).collect();
                    (meter, by)
                })
                .collect(),
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

    fn empty() -> Config {
        toml::from_str("").unwrap()
    }

    fn no_contracts() -> Contracts {
        Contracts::new(&[], paris())
    }

    #[test]
    fn contracts_split_the_house_by_period() {
        let config: Config = toml::from_str(
            r#"
            [[meter]]
            id = "salon"
            name = "Radiateur"
            point = "tuya:r/energy_all"
            role = "circuit"
            [[meter]]
            id = "chambre"
            name = "Chambre"
            point = "tuya:c/energy_all"
            role = "circuit"
            [[contract]]
            from = "2026-01-01"
            offpeak = ["01:38-07:08"]
            prices = { HP = 0.2, HC = 0.1 }
            "#,
        )
        .unwrap();
        let contracts = Contracts::new(&config.contracts, paris());
        let d = at("2026-10-01T00:00[Europe/Paris]");
        let row = |h: u64| HourRow {
            meter: "salon".into(),
            hour: d + h * HOUR_MS,
            kwh: 1.0,
            cost: None,
        };
        // 01:00 (22 min off-peak), 03:00 (off-peak), 12:00 (peak); the
        // bedroom at 04:00 (off-peak) only.
        let night = HourRow {
            meter: "chambre".into(),
            hour: d + 4 * HOUR_MS,
            kwh: 2.0,
            cost: None,
        };
        let rows = [row(1), row(3), night, row(12)];
        let p = Period::of(&config, &contracts, &rows, d, d + 24 * HOUR_MS);
        let (hc, hp) = (&p.by_period["HC"], &p.by_period["HP"]);
        assert!((hc.kwh - (3.0 + 22.0 / 60.0)).abs() < 1e-3);
        assert!((hp.kwh - (1.0 + 38.0 / 60.0)).abs() < 1e-3);
        // Each meter its own split: the bedroom all off-peak.
        let salon = &p.meter_periods["salon"];
        assert!((salon["HC"].kwh - (1.0 + 22.0 / 60.0)).abs() < 1e-3);
        assert!((salon["HP"].kwh - (1.0 + 38.0 / 60.0)).abs() < 1e-3);
        assert_eq!(p.meter_periods["chambre"]["HC"].kwh, 2.0);
        assert!(!p.meter_periods["chambre"].contains_key("HP"));
        assert!((hc.cost.unwrap() - hc.kwh * 0.1).abs() < 1e-3);
        let b = buckets(
            &config,
            &contracts,
            &rows,
            d,
            d + 24 * HOUR_MS,
            Step::Day,
            &paris(),
        )
        .unwrap();
        assert_eq!(b[0].by_period["HC"].kwh, hc.kwh);
        assert_eq!(b[0].meter_periods["salon"], p.meter_periods["salon"]);
        assert!((b[0].meter_periods["chambre"]["HC"].cost.unwrap() - 0.2).abs() < 1e-9);
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
        let b = buckets(
            &empty(),
            &no_contracts(),
            &rows,
            d1,
            d1 + 72 * HOUR_MS,
            Step::Day,
            &tz,
        )
        .unwrap();
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].meters["hp"].kwh, 1.5);
        assert_eq!(b[0].meters["hp"].cost, Some(0.3));
        assert!(b[1].meters.is_empty());
        assert_eq!(b[2].meters["hp"].kwh, 2.0);
        assert!(
            buckets(
                &empty(),
                &no_contracts(),
                &rows,
                0,
                u64::MAX / 4,
                Step::Hour,
                &tz
            )
            .is_err()
        );
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
        let p = Period::of(&config, &no_contracts(), &rows, 0, HOUR_MS);
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
        let p = Period::of(&config, &no_contracts(), &lagging, 0, HOUR_MS);
        assert_eq!(
            p.total,
            Amount {
                kwh: 2.5,
                cost: Some(0.4)
            }
        );

        let month = Period::of(&config, &no_contracts(), &rows, 0, 2 * 24 * HOUR_MS);
        assert_eq!(projection(&month, HOUR_MS, 30 * 24 * HOUR_MS), None);
        let pace = projection(&month, 2 * 24 * HOUR_MS, 30 * 24 * HOUR_MS).unwrap();
        assert_eq!(pace.kwh, 150.0);
    }
}
