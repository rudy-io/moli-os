//! For a house whose meters report a day late (a Linky through Enedis, a
//! Gazpar through GRDF): the last day known and the days before it, instead
//! of a « today » that stays empty until tomorrow; and how far the month's
//! figures reach, for an honest projection.

use std::collections::{BTreeMap, BTreeSet};

use jiff::tz::TimeZone;
use serde::Serialize;

use crate::EnergyError;
use crate::config::{Config, Role};
use crate::meter::HOUR_MS;
use crate::report::{self, Amount, Period, Step};
use crate::store::HourRow;

/// Days averaged for « usual ».
const USUAL_DAYS: usize = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Energy {
    Electricity,
    Gas,
}

impl Energy {
    fn roles(self) -> &'static [Role] {
        match self {
            Self::Electricity => &[Role::Grid, Role::Total],
            Self::Gas => &[Role::Gas],
        }
    }

    fn of(self, period: &Period) -> Option<Amount> {
        match self {
            Self::Electricity => Some(period.total.clone()),
            Self::Gas => period.gas.clone(),
        }
    }
}

/// The last day an energy's late meters told, and its usual day.
#[derive(Clone, Debug, Serialize)]
pub struct LastDay {
    pub energy: Energy,
    /// Start of that day (local midnight), ms.
    pub day: u64,
    pub amount: Amount,
    /// The days with readings before it (seven at most), on average.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usual: Option<Amount>,
    /// How many days that average holds.
    pub usual_days: usize,
}

/// The meters an energy's headline comes from, when every one of them reports
/// a day late (`history`); `None` with a meter read live, or none at all.
pub(crate) fn late_meters(config: &Config, energy: Energy) -> Option<Vec<&str>> {
    let mut main = config
        .meters
        .iter()
        .filter(|m| energy.roles().contains(&m.role))
        .peekable();
    if main.peek().is_none() || !main.clone().all(|m| m.history) {
        return None;
    }
    Some(main.map(|m| m.id.as_str()).collect())
}

/// The last day known of each energy read a day late, oldest rows first or not.
pub(crate) fn last_days(
    config: &Config,
    rows: &[HourRow],
    tz: &TimeZone,
) -> Result<Vec<LastDay>, EnergyError> {
    let mut out = Vec::new();
    for energy in [Energy::Electricity, Energy::Gas] {
        let Some(ids) = late_meters(config, energy) else {
            continue;
        };
        // Only whole days: a gas day runs from 6:00 to 6:00 (GRDF), so the
        // last calendar day holds a few of its hours, not the day.
        let mut hours: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
        for row in rows.iter().filter(|r| ids.contains(&r.meter.as_str())) {
            let day = report::floor(row.hour, Step::Day, tz)?;
            hours.entry(day).or_default().insert(row.hour);
        }
        let mut days = BTreeSet::new();
        for (day, told) in hours {
            let length = report::next(day, Step::Day, tz)? - day;
            if told.len() as u64 * HOUR_MS >= length {
                days.insert(day);
            }
        }
        let Some(&day) = days.last() else { continue };
        let amount_of = |from: u64| -> Result<Option<Amount>, EnergyError> {
            let to = report::next(from, Step::Day, tz)?;
            Ok(energy.of(&Period::of(config, rows, from, to)))
        };
        let Some(amount) = amount_of(day)? else {
            continue;
        };
        let mut before = Vec::new();
        for &d in days.iter().rev().skip(1).take(USUAL_DAYS) {
            if let Some(a) = amount_of(d)? {
                before.push(a);
            }
        }
        out.push(LastDay {
            energy,
            day,
            amount,
            usual: average(&before),
            usual_days: before.len(),
        });
    }
    Ok(out)
}

#[allow(clippy::cast_precision_loss)] // a handful of days
fn average(days: &[Amount]) -> Option<Amount> {
    if days.is_empty() {
        return None;
    }
    let n = days.len() as f64;
    let cost = days
        .iter()
        .map(|a| a.cost)
        .collect::<Option<Vec<f64>>>()
        .map(|c| c.iter().sum::<f64>() / n);
    Some(
        Amount {
            kwh: days.iter().map(|a| a.kwh).sum::<f64>() / n,
            cost,
        }
        .rounded(),
    )
}

/// How far the month's figures of an energy reach, as a time: `now` for
/// meters read live; for meters a day late, the month's start plus the hours
/// they actually told. A day missing (a service down, a gap) then neither
/// counts as empty nor drags the pace down.
pub(crate) fn reach(
    config: &Config,
    rows: &[HourRow],
    month: u64,
    now: u64,
    energy: Energy,
) -> u64 {
    let Some(ids) = late_meters(config, energy) else {
        return now;
    };
    let hours: BTreeSet<u64> = rows
        .iter()
        .filter(|r| r.hour >= month && r.hour < now && ids.contains(&r.meter.as_str()))
        .map(|r| r.hour)
        .collect();
    month + hours.len() as u64 * HOUR_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paris() -> TimeZone {
        TimeZone::get("Europe/Paris").unwrap()
    }

    /// 2026-11-01 00:00 Paris.
    const NOV: u64 = 1_793_487_600_000;
    const DAY: u64 = 24 * HOUR_MS;

    fn config(extra: &str) -> Config {
        toml::from_str(&format!(
            "[[meter]]\nid = \"linky\"\nname = \"Linky\"\npoint = \"enedis:linky/hours\"\nrole = \"grid\"\nhistory = true\nprice = 0.2\n\
             [[meter]]\nid = \"gaz\"\nname = \"Gaz\"\npoint = \"grdf:gazpar/hours\"\nrole = \"gas\"\nhistory = true\n{extra}"
        ))
        .unwrap()
    }

    fn day_of(meter: &str, day: u64, kwh_per_hour: f64, cost: Option<f64>) -> Vec<HourRow> {
        (0..24)
            .map(|h| HourRow {
                meter: meter.into(),
                hour: NOV + day * DAY + h * HOUR_MS,
                kwh: kwh_per_hour,
                cost,
            })
            .collect()
    }

    #[test]
    fn the_last_day_and_the_days_before_it_with_readings() {
        let mut rows = Vec::new();
        // Days 0 to 9, with a gap on days 5 and 6; gas up to day 8.
        for d in (0..10).filter(|d| ![5, 6].contains(d)) {
            rows.extend(day_of("linky", d, 0.5, Some(0.1)));
        }
        for d in 0..9 {
            rows.extend(day_of("gaz", d, 2.0, None));
        }
        let days = last_days(&config(""), &rows, &paris()).unwrap();
        let elec = days
            .iter()
            .find(|d| d.energy == Energy::Electricity)
            .unwrap();
        assert_eq!(elec.day, NOV + 9 * DAY);
        assert!((elec.amount.kwh - 12.0).abs() < 1e-9);
        // Seven days with readings before day 9: 8, 7, 4, 3, 2, 1, 0.
        assert_eq!(elec.usual_days, 7);
        assert!((elec.usual.as_ref().unwrap().kwh - 12.0).abs() < 1e-9);
        assert!((elec.usual.as_ref().unwrap().cost.unwrap() - 2.4).abs() < 1e-9);
        let gas = days.iter().find(|d| d.energy == Energy::Gas).unwrap();
        assert_eq!(gas.day, NOV + 8 * DAY);
        assert!((gas.amount.kwh - 48.0).abs() < 1e-9);
        assert_eq!(gas.amount.cost, None);
    }

    #[test]
    fn a_day_known_only_in_part_is_not_the_last_day() {
        // A gas day from 6:00 to 6:00: the next calendar day holds 6 of its hours.
        let mut rows = day_of("gaz", 0, 2.0, None);
        rows.extend(day_of("gaz", 1, 2.0, None).into_iter().take(6));
        let days = last_days(&config(""), &rows, &paris()).unwrap();
        let gas = days.iter().find(|d| d.energy == Energy::Gas).unwrap();
        assert_eq!(gas.day, NOV);
        assert!((gas.amount.kwh - 48.0).abs() < 1e-9);
    }

    #[test]
    fn a_meter_read_live_says_today_itself() {
        let config = config(
            "[[meter]]\nid = \"pince\"\nname = \"Pince\"\npoint = \"z2m:x/energy\"\nrole = \"total\"\n",
        );
        let rows = day_of("linky", 0, 1.0, None);
        let days = last_days(&config, &rows, &paris()).unwrap();
        assert!(days.iter().all(|d| d.energy == Energy::Gas));
        assert_eq!(
            reach(&config, &rows, NOV, NOV + 3 * DAY, Energy::Electricity),
            NOV + 3 * DAY
        );
    }

    #[test]
    fn the_pace_counts_the_hours_told_not_the_gaps() {
        let mut rows = Vec::new();
        for d in [0, 1, 4] {
            rows.extend(day_of("linky", d, 1.0, None));
        }
        let now = NOV + 6 * DAY;
        assert_eq!(
            reach(&config(""), &rows, NOV, now, Energy::Electricity),
            NOV + 3 * DAY
        );
        // Nothing yet for gas this month.
        assert_eq!(reach(&config(""), &rows, NOV, now, Energy::Gas), NOV);
    }
}
