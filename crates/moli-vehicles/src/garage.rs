//! The garage book: what each vehicle has had done (oil change, filters,
//! inspection…), at which date and mileage, and when the next one is due,
//! by distance or by time, whichever comes first.

use serde::{Deserialize, Serialize};

const DAY_MS: f64 = 86_400_000.0;
/// An average month (365.25 / 12 days).
const MONTH_DAYS: f64 = 30.4375;

/// A kind of maintenance and how often it is due.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Item {
    /// Its id (`vidange`, `controle_technique`): the points' prefix.
    pub id: String,
    pub name: String,
    /// Due every so many km (none: by time only).
    #[serde(default)]
    pub every_km: Option<f64>,
    /// Due every so many months (none: by distance only).
    #[serde(default)]
    pub every_months: Option<u32>,
}

/// Something done to the vehicle.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Record {
    /// The maintenance item's id.
    pub item: String,
    /// When (ms since the epoch).
    pub at_ms: u64,
    /// The odometer then, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub km: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Where an item stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Due {
    /// Km before it is due (negative: overdue). None: not known.
    pub km_left: Option<f64>,
    /// Days before it is due (negative: overdue). None: not known.
    pub days_left: Option<f64>,
}

impl Due {
    /// Overdue by distance or by time.
    #[must_use]
    pub fn is_due(&self) -> bool {
        self.km_left.is_some_and(|k| k <= 0.0) || self.days_left.is_some_and(|d| d <= 0.0)
    }
}

/// Where `item` stands, from its last record, the odometer and the time.
#[must_use]
pub fn due(item: &Item, records: &[Record], odometer: Option<f64>, now_ms: u64) -> Due {
    let Some(last) = records
        .iter()
        .filter(|r| r.item == item.id)
        .max_by_key(|r| r.at_ms)
    else {
        return Due::default();
    };
    let km_left = match (item.every_km, last.km, odometer) {
        (Some(every), Some(at), Some(now)) => Some(at + every - now),
        _ => None,
    };
    #[allow(clippy::cast_precision_loss)] // ms since 1970 fit a f64 exactly
    let days_left = item.every_months.map(|months| {
        let since = (now_ms as f64 - last.at_ms as f64) / DAY_MS;
        f64::from(months) * MONTH_DAYS - since
    });
    Due { km_left, days_left }
}

/// A `maintenance_done` order: `vidange`, `vidange 123456`, or JSON
/// `{"item": "vidange", "km": 123456, "date": "2026-11-24", "note": "…"}`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Done {
    pub item: String,
    #[serde(default)]
    pub km: Option<f64>,
    /// `YYYY-MM-DD` (that day, noon UTC); none: now.
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

impl Done {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let text = text.trim();
        if text.starts_with('{') {
            return Ok(serde_json::from_str(text)?);
        }
        let mut words = text.split_whitespace();
        let item = words.next().unwrap_or_default().to_owned();
        let km = match words.next() {
            Some(km) => Some(km.replace(['.', '_'], "").parse::<f64>()?),
            None => None,
        };
        anyhow::ensure!(words.next().is_none(), "« item » or « item km », or JSON");
        Ok(Self {
            item,
            km,
            date: None,
            note: None,
        })
    }

    /// The moment it was done (ms).
    pub fn at_ms(&self, now_ms: u64) -> anyhow::Result<u64> {
        let Some(date) = &self.date else {
            return Ok(now_ms);
        };
        let day: jiff::civil::Date = date.parse()?;
        let ts = day
            .to_datetime(jiff::civil::time(12, 0, 0, 0))
            .to_zoned(jiff::tz::TimeZone::UTC)?
            .timestamp();
        let ms = u64::try_from(ts.as_millisecond())?;
        anyhow::ensure!(ms <= now_ms, "date: not in the future");
        Ok(ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn oil() -> Item {
        Item {
            id: "vidange".into(),
            name: "Vidange".into(),
            every_km: Some(15_000.0),
            every_months: Some(12),
        }
    }

    #[test]
    fn never_done_is_unknown_not_due() {
        let d = due(&oil(), &[], Some(100_000.0), NOW);
        assert_eq!(d, Due::default());
        assert!(!d.is_due());
    }

    #[test]
    fn due_by_distance_or_by_time_whichever_first() {
        let rec = |days_ago: u64, km| Record {
            item: "vidange".into(),
            at_ms: NOW - days_ago * 86_400_000,
            km: Some(km),
            note: None,
        };
        let d = due(&oil(), &[rec(30, 100_000.0)], Some(110_000.0), NOW);
        assert_eq!(d.km_left, Some(5_000.0));
        assert!((d.days_left.unwrap() - (365.25 - 30.0)).abs() < 0.01);
        assert!(!d.is_due());
        // Far by distance, late by time.
        assert!(due(&oil(), &[rec(400, 100_000.0)], Some(101_000.0), NOW).is_due());
        // Recent, but driven past it; the latest record counts.
        let recs = [rec(400, 80_000.0), rec(10, 100_000.0)];
        assert!(due(&oil(), &recs, Some(115_100.0), NOW).is_due());
        assert!(!due(&oil(), &recs, Some(114_000.0), NOW).is_due());
        // No odometer: by time only.
        assert_eq!(due(&oil(), &recs, None, NOW).km_left, None);
    }

    #[test]
    fn orders_are_read() {
        assert_eq!(Done::parse("vidange").unwrap().item, "vidange");
        assert_eq!(
            Done::parse(" vidange 123.456 ").unwrap().km,
            Some(123_456.0)
        );
        let j = Done::parse(r#"{"item":"ct","date":"2026-01-15","note":"OK"}"#).unwrap();
        assert_eq!(j.item, "ct");
        assert!(j.at_ms(NOW).unwrap() < NOW);
        assert!(Done::parse("vidange 12 x").is_err());
        assert!(Done::parse("vidange douze").is_err());
        let future = Done {
            date: Some("2099-01-01".into()),
            ..Done::default()
        };
        assert!(future.at_ms(NOW).is_err());
    }
}
