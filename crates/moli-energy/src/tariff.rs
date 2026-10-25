//! Dated contracts (`[[energy.contract]]`): the off-peak hours of each day,
//! the price of a kWh in each period and the subscription, each from its
//! own day on (a new price, new off-peak hours). What an hour cost is read
//! from the contract of its day when a report is made, so a contract filed
//! later prices the past too. For a home without a tariff point (a Linky
//! that says « HP.. » / « HC.. »).

use std::collections::BTreeMap;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::config::period_key;

/// The period names: off-peak, peak, or the one price of a contract without
/// off-peak hours.
pub const OFF_PEAK: &str = "HC";
pub const PEAK: &str = "HP";
pub const BASE: &str = "Base";

const DAY_MIN: f64 = 1440.0;

/// A contract as written in `moli.toml`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    /// First day it applies, local (« 2026-01-01 »).
    pub from: String,
    /// The subscription per month, taxes included.
    #[serde(default)]
    pub monthly_fee: Option<f64>,
    /// Off-peak hours of each day, « 01:38-07:08 » (an end before its start
    /// runs past midnight). None: one price all day (`Base`).
    #[serde(default)]
    pub offpeak: Vec<String>,
    /// Price of a kWh by period: « HC » and « HP », or « Base ».
    pub prices: BTreeMap<String, f64>,
}

impl Contract {
    /// Why it cannot be used, if it cannot.
    pub(crate) fn check(&self) -> Result<(), String> {
        let at = || format!("[[energy.contract]] from {:?}", self.from);
        self.from
            .parse::<Date>()
            .map_err(|e| format!("{}: {e}", at()))?;
        for range in &self.offpeak {
            window(range)
                .ok_or_else(|| format!("{}: offpeak {range:?} is not « HH:MM-HH:MM »", at()))?;
        }
        let names: &[&str] = if self.offpeak.is_empty() {
            &[BASE]
        } else {
            &[OFF_PEAK, PEAK]
        };
        for name in names {
            if price_in(&self.prices, name).is_none() {
                return Err(format!("{}: no price for {name}", at()));
            }
        }
        for (name, price) in self
            .prices
            .iter()
            .map(|(n, p)| (n.as_str(), *p))
            .chain(self.monthly_fee.map(|f| ("monthly_fee", f)))
        {
            if !(price.is_finite() && price >= 0.0) {
                return Err(format!(
                    "{}: {name} price {price} is not a positive number",
                    at()
                ));
            }
        }
        Ok(())
    }
}

/// « 01:38-07:08 » → minutes of the day (01:38 = 98).
fn window(range: &str) -> Option<(f64, f64)> {
    let minutes = |hhmm: &str| -> Option<u16> {
        let (h, m) = hhmm.trim().split_once(':')?;
        let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
        (h <= 24 && m < 60 && h * 60 + m <= 1440).then_some(h * 60 + m)
    };
    let (a, b) = range.split_once('-')?;
    let (a, b) = (minutes(a)?, minutes(b)?);
    (a != b).then_some((f64::from(a), f64::from(b)))
}

fn price_in(prices: &BTreeMap<String, f64>, period: &str) -> Option<f64> {
    let wanted = period_key(period);
    prices
        .iter()
        .find(|(name, _)| period_key(name) == wanted)
        .map(|(_, p)| *p)
}

/// An off-peak window, in minutes of the day (`to` before `from`: past midnight).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Window {
    pub from: f64,
    pub to: f64,
}

/// A part of a stretch of time in one period, at that period's price.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Share {
    pub period: &'static str,
    /// Of the stretch, 0 to 1.
    pub part: f64,
    pub price: Option<f64>,
}

#[derive(Clone, Debug)]
struct Dated {
    from: Date,
    fee: Option<f64>,
    offpeak: Vec<Window>,
    prices: BTreeMap<String, f64>,
}

/// The contracts, in the house's time zone, oldest first.
#[derive(Clone, Debug)]
pub struct Contracts {
    tz: TimeZone,
    list: Vec<Dated>,
}

impl Contracts {
    /// From checked contracts (one that does not parse is left out).
    #[must_use]
    pub fn new(contracts: &[Contract], tz: TimeZone) -> Self {
        let mut list: Vec<Dated> = contracts
            .iter()
            .filter_map(|c| {
                Some(Dated {
                    from: c.from.parse().ok()?,
                    fee: c.monthly_fee,
                    offpeak: c
                        .offpeak
                        .iter()
                        .filter_map(|r| window(r))
                        .map(|(from, to)| Window { from, to })
                        .collect(),
                    prices: c.prices.clone(),
                })
            })
            .collect();
        list.sort_by_key(|d| d.from);
        Self { tz, list }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The contract in force at `ms`, and that moment in minutes of its day.
    fn at(&self, ms: u64) -> Option<(&Dated, f64)> {
        let ts = Timestamp::from_millisecond(i64::try_from(ms).ok()?).ok()?;
        let local = ts.to_zoned(self.tz.clone());
        let day = local.date();
        let contract = self.list.iter().rev().find(|c| c.from <= day)?;
        let minute = f64::from(local.hour()) * 60.0
            + f64::from(local.minute())
            + f64::from(local.second()) / 60.0;
        Some((contract, minute))
    }

    /// How `[a, b)` (an hour at most) splits between the periods, by the
    /// contract of its start. Empty before the first contract.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // ms within an hour
    pub fn shares(&self, a: u64, b: u64) -> Vec<Share> {
        let Some((c, start)) = self.at(a) else {
            return Vec::new();
        };
        if c.offpeak.is_empty() {
            return vec![Share {
                period: BASE,
                part: 1.0,
                price: price_in(&c.prices, BASE),
            }];
        }
        let len = b.saturating_sub(a) as f64 / 60_000.0;
        let off = if len > 0.0 {
            (c.offpeak
                .iter()
                .map(|w| overlap(start, start + len, *w))
                .sum::<f64>()
                / len)
                .clamp(0.0, 1.0)
        } else {
            f64::from(u8::from(
                c.offpeak
                    .iter()
                    .any(|w| overlap(start, start + 1e-6, *w) > 0.0),
            ))
        };
        [(OFF_PEAK, off), (PEAK, 1.0 - off)]
            .into_iter()
            .filter(|(_, part)| *part > 0.0)
            .map(|(period, part)| Share {
                period,
                part,
                price: price_in(&c.prices, period),
            })
            .collect()
    }

    /// The price of a kWh drawn evenly over `[a, b)`; None before the
    /// first contract or for a period without a price.
    #[must_use]
    pub fn price(&self, a: u64, b: u64) -> Option<f64> {
        let shares = self.shares(a, b);
        if shares.is_empty() {
            return None;
        }
        shares.iter().map(|s| s.price.map(|p| p * s.part)).sum()
    }

    /// The period now and its price.
    #[must_use]
    pub fn now(&self, ms: u64) -> Option<(&'static str, Option<f64>)> {
        let share = *self.shares(ms, ms).first()?;
        Some((share.period, share.price))
    }

    /// The subscription in force at `ms`.
    #[must_use]
    pub fn monthly_fee(&self, ms: u64) -> Option<f64> {
        self.at(ms)?.0.fee
    }

    /// The off-peak windows in force at `ms`.
    #[must_use]
    pub fn offpeak(&self, ms: u64) -> Vec<Window> {
        self.at(ms)
            .map(|(c, _)| c.offpeak.clone())
            .unwrap_or_default()
    }
}

/// Minutes of `[x0, x1)` inside a window (one past midnight counts on both
/// sides of it; a stretch may run into the next day).
fn overlap(x0: f64, x1: f64, w: Window) -> f64 {
    let pieces = if w.from < w.to {
        vec![(w.from, w.to)]
    } else {
        vec![(w.from, DAY_MIN), (0.0, w.to)]
    };
    pieces
        .into_iter()
        .flat_map(|(a, b)| [(a, b), (a + DAY_MIN, b + DAY_MIN)])
        .map(|(a, b)| (x1.min(b) - x0.max(a)).max(0.0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paris() -> TimeZone {
        TimeZone::get("Europe/Paris").unwrap()
    }

    fn at(s: &str) -> u64 {
        let z: jiff::Zoned = format!("{s}[Europe/Paris]").parse().unwrap();
        u64::try_from(z.timestamp().as_millisecond()).unwrap()
    }

    fn contract(from: &str, offpeak: &[&str], hp: f64, hc: f64) -> Contract {
        Contract {
            from: from.into(),
            monthly_fee: Some(17.96),
            offpeak: offpeak.iter().map(|s| (*s).to_owned()).collect(),
            prices: BTreeMap::from([("HP".into(), hp), ("HC".into(), hc)]),
        }
    }

    #[test]
    fn an_hour_splits_at_the_off_peak_minute() {
        let c = Contracts::new(
            &[contract(
                "2026-01-01",
                &["01:38-07:08", "14:38-17:08"],
                0.2210,
                0.1747,
            )],
            paris(),
        );
        // 01:00–02:00: 38 min peak, 22 min off-peak.
        let shares = c.shares(at("2026-03-02T01:00"), at("2026-03-02T02:00"));
        assert_eq!(shares.len(), 2);
        assert_eq!(shares[0].period, OFF_PEAK);
        assert!((shares[0].part - 22.0 / 60.0).abs() < 1e-9);
        let price = c
            .price(at("2026-03-02T01:00"), at("2026-03-02T02:00"))
            .unwrap();
        assert!((price - (0.1747 * 22.0 + 0.2210 * 38.0) / 60.0).abs() < 1e-9);
        // Wholly off-peak, wholly peak.
        assert_eq!(
            c.now(at("2026-03-02T03:00")),
            Some((OFF_PEAK, Some(0.1747)))
        );
        assert_eq!(c.now(at("2026-03-02T12:00")), Some((PEAK, Some(0.2210))));
        assert_eq!(
            c.now(at("2026-03-02T15:00")),
            Some((OFF_PEAK, Some(0.1747)))
        );
        // Summer time: local hours all the same.
        assert_eq!(
            c.now(at("2026-07-14T07:00")),
            Some((OFF_PEAK, Some(0.1747)))
        );
        assert_eq!(c.now(at("2026-07-14T07:10")), Some((PEAK, Some(0.2210))));
    }

    #[test]
    fn each_day_takes_the_contract_begun_by_then() {
        let c = Contracts::new(
            &[
                contract("2026-08-01", &["22:00-06:00"], 0.25, 0.20),
                contract("2026-01-01", &["01:38-07:08"], 0.2210, 0.1747),
            ],
            paris(),
        );
        assert_eq!(c.now(at("2025-12-31T12:00")), None, "before any contract");
        assert_eq!(c.now(at("2026-07-31T23:00")), Some((PEAK, Some(0.2210))));
        // From 1 August: off-peak past midnight.
        assert_eq!(c.now(at("2026-08-01T23:00")), Some((OFF_PEAK, Some(0.20))));
        assert_eq!(c.now(at("2026-08-02T05:59")), Some((OFF_PEAK, Some(0.20))));
        assert_eq!(
            c.offpeak(at("2026-09-01T12:00")),
            vec![Window {
                from: 1320.0,
                to: 360.0
            }]
        );
        assert_eq!(c.monthly_fee(at("2026-09-01T12:00")), Some(17.96));
    }

    #[test]
    fn a_contract_is_checked() {
        assert!(
            contract("2026-01-01", &["01:38-07:08"], 0.2, 0.1)
                .check()
                .is_ok()
        );
        assert!(
            contract("2026-13-01", &[], 0.2, 0.1).check().is_err(),
            "date"
        );
        assert!(
            contract("2026-01-01", &["1h-7h"], 0.2, 0.1)
                .check()
                .is_err(),
            "window"
        );
        assert!(
            contract("2026-01-01", &[], 0.2, 0.1).check().is_err(),
            "no Base price"
        );
        let mut base = contract("2026-01-01", &[], 0.2, 0.1);
        base.prices = BTreeMap::from([("base".into(), 0.19)]);
        assert!(base.check().is_ok());
        assert!(
            contract("2026-01-01", &["01:38-07:08"], -0.2, 0.1)
                .check()
                .is_err(),
            "price"
        );
    }
}
