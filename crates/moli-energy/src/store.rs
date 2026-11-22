//! `energy.db`: kWh and cost per meter and hour, plus each meter's last
//! reading. Never pruned: a year of ten meters is ~90 000 small rows.
//!
//! A reading's hours and the new « last reading » are written in one
//! transaction: after a crash, nothing is counted twice or lost (the next
//! reading is compared with the last one that was really written).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use rusqlite::{Connection, OptionalExtension as _, params};

use crate::meter::Reading;
use crate::price::HousePrice;

#[derive(Debug, thiserror::Error)]
pub enum EnergyError {
    #[error("energy database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("internal: {0}")]
    Internal(String),
}

/// One slice of consumption: a meter, an hour (ms, UTC), kWh and cost.
#[derive(Clone, Debug, PartialEq)]
pub struct HourRow {
    pub meter: String,
    pub hour: u64,
    pub kwh: f64,
    pub cost: Option<f64>,
}

#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    CREATE TABLE IF NOT EXISTS meter_last (
        meter TEXT PRIMARY KEY,
        kwh   REAL NOT NULL,
        ts    INTEGER NOT NULL
    ) STRICT;
    CREATE TABLE IF NOT EXISTS energy_hour (
        meter TEXT NOT NULL,
        hour  INTEGER NOT NULL,
        kwh   REAL NOT NULL,
        cost  REAL,
        PRIMARY KEY (meter, hour)
    ) STRICT, WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS energy_hour_by_hour ON energy_hour (hour);
    -- When live recording of each meter began (its first reference
    -- reading): imported history stops there, so nothing counts twice.
    CREATE TABLE IF NOT EXISTS meter_first (
        meter TEXT PRIMARY KEY,
        ts    INTEGER NOT NULL
    ) STRICT;
    -- The house's price of a kWh, chosen from the dashboard, from `since`
    -- on (meters without a price of their own).
    CREATE TABLE IF NOT EXISTS house_price (
        since INTEGER PRIMARY KEY,
        price REAL NOT NULL
    ) STRICT;
    -- The point each meter read last time: a counter swapped for another
    -- one starts from its own first reading (see `follow_sources`).
    CREATE TABLE IF NOT EXISTS meter_source (
        meter TEXT PRIMARY KEY,
        point TEXT NOT NULL
    ) STRICT;
    -- A reading held as suspect (a jump, a fall) and how many readings
    -- back it: a restart does not forget it (see `Tracker::resumed`).
    CREATE TABLE IF NOT EXISTS meter_suspect (
        meter TEXT PRIMARY KEY,
        kwh   REAL NOT NULL,
        ts    INTEGER NOT NULL,
        seen  INTEGER NOT NULL
    ) STRICT;
    -- Databases from before meter_first: the older of the last reading
    -- and the first recorded hour.
    INSERT OR IGNORE INTO meter_first (meter, ts)
        SELECT l.meter, min(l.ts, coalesce(
            (SELECT min(h.hour) FROM energy_hour h WHERE h.meter = l.meter), l.ts))
        FROM meter_last l;
";

fn ms(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn unms(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, EnergyError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| EnergyError::Internal(format!("{}: {e}", dir.display())))?;
        }
        let conn = Connection::open(path)?;
        // The import tool writes while the hub runs.
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    #[cfg(test)]
    pub fn memory() -> Self {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        Self {
            conn: Mutex::new(conn),
        }
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn last_readings(&self) -> Result<HashMap<String, Reading>, EnergyError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT meter, kwh, ts FROM meter_last")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Reading {
                    kwh: row.get(1)?,
                    ts: unms(row.get(2)?),
                },
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Each meter's suspect reading, as it was left before a restart.
    pub fn suspects(&self) -> Result<HashMap<String, (Reading, u32)>, EnergyError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT meter, kwh, ts, seen FROM meter_suspect")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    Reading {
                        kwh: row.get(1)?,
                        ts: unms(row.get(2)?),
                    },
                    u32::try_from(row.get::<_, i64>(3)?).unwrap_or(1),
                ),
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Keeps (or forgets, with `None`) a meter's suspect reading.
    pub fn hold(&self, meter: &str, suspect: Option<(Reading, u32)>) -> Result<(), EnergyError> {
        let conn = self.conn();
        match suspect {
            Some((reading, seen)) => conn.execute(
                "INSERT INTO meter_suspect (meter, kwh, ts, seen) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (meter) DO UPDATE SET
                    kwh = excluded.kwh, ts = excluded.ts, seen = excluded.seen",
                params![meter, reading.kwh, ms(reading.ts), i64::from(seen)],
            )?,
            None => conn.execute("DELETE FROM meter_suspect WHERE meter = ?1", params![meter])?,
        };
        Ok(())
    }

    /// Notes the point each meter reads. A meter whose point changed since
    /// the last run forgets its last reading: the new device's lifetime
    /// total is not the old one's, and the difference was never used by the
    /// home. Its history stays. Returns those meters.
    pub fn follow_sources(&self, meters: &[(String, String)]) -> Result<Vec<String>, EnergyError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut changed = Vec::new();
        for (meter, point) in meters {
            let before: Option<String> = tx
                .query_row(
                    "SELECT point FROM meter_source WHERE meter = ?1",
                    params![meter],
                    |row| row.get(0),
                )
                .optional()?;
            if before.as_ref().is_some_and(|b| b != point) {
                tx.execute("DELETE FROM meter_last WHERE meter = ?1", params![meter])?;
                tx.execute("DELETE FROM meter_suspect WHERE meter = ?1", params![meter])?;
                changed.push(meter.clone());
            }
            tx.execute(
                "INSERT INTO meter_source (meter, point) VALUES (?1, ?2)
                 ON CONFLICT (meter) DO UPDATE SET point = excluded.point",
                params![meter, point],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Adds `slices` and sets the meter's last reading, atomically (a
    /// suspect reading is settled by it).
    pub fn record(
        &self,
        meter: &str,
        last: Reading,
        slices: &[HourRow],
    ) -> Result<(), EnergyError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut add = tx.prepare(
                "INSERT INTO energy_hour (meter, hour, kwh, cost) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (meter, hour) DO UPDATE SET
                    kwh = kwh + excluded.kwh,
                    cost = CASE WHEN excluded.cost IS NULL THEN cost
                                ELSE coalesce(cost, 0) + excluded.cost END",
            )?;
            for s in slices {
                add.execute(params![s.meter, ms(s.hour), s.kwh, s.cost])?;
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO meter_first (meter, ts) VALUES (?1, ?2)",
            params![meter, ms(last.ts)],
        )?;
        tx.execute(
            "INSERT INTO meter_last (meter, kwh, ts) VALUES (?1, ?2, ?3)
             ON CONFLICT (meter) DO UPDATE SET kwh = excluded.kwh, ts = excluded.ts",
            params![meter, last.kwh, ms(last.ts)],
        )?;
        tx.execute("DELETE FROM meter_suspect WHERE meter = ?1", params![meter])?;
        tx.commit()?;
        Ok(())
    }

    /// Adds history recorded elsewhere, only for hours that ended before
    /// the meter's live recording began and that hold nothing yet; an
    /// imported hour that had no cost gets one. Returns (inserted,
    /// repriced, skipped).
    pub fn import(&self, rows: &[HourRow]) -> Result<(usize, usize, usize), EnergyError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (mut inserted, mut repriced, mut skipped) = (0, 0, 0);
        {
            let mut first = tx.prepare("SELECT ts FROM meter_first WHERE meter = ?1")?;
            let mut add = tx.prepare(
                "INSERT INTO energy_hour (meter, hour, kwh, cost) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (meter, hour) DO NOTHING",
            )?;
            let mut price = tx.prepare(
                "UPDATE energy_hour SET cost = ?3
                 WHERE meter = ?1 AND hour = ?2 AND cost IS NULL AND ?3 IS NOT NULL",
            )?;
            let mut cutoffs: HashMap<String, Option<u64>> = HashMap::new();
            for row in rows {
                if !cutoffs.contains_key(&row.meter) {
                    let ts: Option<i64> = first
                        .query_row(params![row.meter], |r| r.get(0))
                        .optional()?;
                    cutoffs.insert(row.meter.clone(), ts.map(unms));
                }
                let live = cutoffs[&row.meter];
                if live.is_some_and(|live| row.hour + crate::meter::HOUR_MS > live) {
                    skipped += 1;
                } else if add.execute(params![row.meter, ms(row.hour), row.kwh, row.cost])? == 1 {
                    inserted += 1;
                } else if price.execute(params![row.meter, ms(row.hour), row.cost])? == 1 {
                    repriced += 1;
                } else {
                    skipped += 1;
                }
            }
        }
        tx.commit()?;
        Ok((inserted, repriced, skipped))
    }

    /// Every slice with `from <= hour < to`.
    pub fn hours(&self, from: u64, to: u64) -> Result<Vec<HourRow>, EnergyError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT meter, hour, kwh, cost FROM energy_hour
             WHERE hour >= ?1 AND hour < ?2 ORDER BY hour",
        )?;
        let rows = stmt.query_map(params![ms(from), ms(to)], |row| {
            Ok(HourRow {
                meter: row.get(0)?,
                hour: unms(row.get(1)?),
                kwh: row.get(2)?,
                cost: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every price of the house, oldest first.
    pub fn house_prices(&self) -> Result<Vec<HousePrice>, EnergyError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT since, price FROM house_price ORDER BY since")?;
        let rows = stmt.query_map([], |row| {
            Ok(HousePrice {
                since: unms(row.get(0)?),
                price: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// A new price of the house, from `price.since` on.
    pub fn add_house_price(&self, price: HousePrice) -> Result<(), EnergyError> {
        self.conn().execute(
            "INSERT INTO house_price (since, price) VALUES (?1, ?2)
             ON CONFLICT (since) DO UPDATE SET price = excluded.price",
            params![ms(price.since), price.price],
        )?;
        Ok(())
    }

    /// Every price of the house at once (the list edited from the
    /// dashboard), and the hours of `meters` from `from` on priced again
    /// with it, in one go: an hour takes the price in force in its middle,
    /// none before the first price. Returns how many hours were priced again.
    pub fn replace_house_prices(
        &self,
        prices: &[HousePrice],
        meters: &[String],
        from: u64,
    ) -> Result<usize, EnergyError> {
        let mut sorted = prices.to_vec();
        sorted.sort_by_key(|p| p.since);
        // Slices of time from `from` on, each with its price (none before the first).
        let mut slices: Vec<(u64, u64, Option<f64>)> = Vec::new();
        let first = sorted.first().map_or(u64::MAX, |p| p.since);
        if from < first {
            slices.push((from, first, None));
        }
        for (i, p) in sorted.iter().enumerate() {
            let end = sorted.get(i + 1).map_or(u64::MAX, |n| n.since);
            if end > from {
                slices.push((p.since.max(from), end, Some(p.price)));
            }
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM house_price", [])?;
        let mut repriced = 0;
        {
            let mut add = tx.prepare("INSERT INTO house_price (since, price) VALUES (?1, ?2)")?;
            for p in &sorted {
                add.execute(params![ms(p.since), p.price])?;
            }
            // The middle of an hour decides its price: `hour + half` in [a, b).
            let mut set = tx.prepare(
                "UPDATE energy_hour SET cost = kwh * ?4
                 WHERE meter = ?1 AND hour + ?5 >= ?2 AND hour + ?5 < ?3",
            )?;
            let half = ms(crate::meter::HOUR_MS / 2);
            for meter in meters {
                for &(a, b, price) in &slices {
                    repriced += set.execute(params![meter, ms(a), ms(b), price, half])?;
                }
            }
        }
        tx.commit()?;
        Ok(repriced)
    }

    /// The first hour on record (for « depuis le … »).
    pub fn first_hour(&self) -> Result<Option<u64>, EnergyError> {
        let first: Option<Option<i64>> = self
            .conn()
            .query_row("SELECT min(hour) FROM energy_hour", [], |row| row.get(0))
            .optional()?;
        Ok(first.flatten().map(unms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(meter: &str, hour: u64, kwh: f64, cost: Option<f64>) -> HourRow {
        HourRow {
            meter: meter.into(),
            hour,
            kwh,
            cost,
        }
    }

    #[test]
    fn slices_add_up_and_last_reading_follows() {
        let store = Store::memory();
        let last = Reading { kwh: 10.0, ts: 5 };
        store
            .record("hc", last, &[row("hc", 0, 1.0, Some(0.1))])
            .unwrap();
        store
            .record(
                "hc",
                Reading { kwh: 12.0, ts: 9 },
                &[
                    row("hc", 0, 0.5, Some(0.05)),
                    row("hc", 3_600_000, 0.5, None),
                ],
            )
            .unwrap();
        let hours = store.hours(0, u64::MAX / 2).unwrap();
        assert_eq!(hours.len(), 2);
        assert!((hours[0].kwh - 1.5).abs() < 1e-9);
        assert!((hours[0].cost.unwrap() - 0.15).abs() < 1e-9);
        assert_eq!(hours[1].cost, None);
        assert_eq!(
            store.last_readings().unwrap()["hc"],
            Reading { kwh: 12.0, ts: 9 }
        );
        assert_eq!(store.first_hour().unwrap(), Some(0));
        assert_eq!(Store::memory().first_hour().unwrap(), None);
    }

    #[test]
    fn a_suspect_reading_is_kept_until_a_reading_settles_it() {
        let store = Store::memory();
        let suspect = (
            Reading {
                kwh: 2_545.57,
                ts: 7,
            },
            2,
        );
        store.hold("borne", Some(suspect)).unwrap();
        assert_eq!(store.suspects().unwrap()["borne"], suspect);
        store.hold("borne", None).unwrap();
        assert!(store.suspects().unwrap().is_empty());
        // A filed reading (counted or rebased) settles it.
        store.hold("borne", Some(suspect)).unwrap();
        store
            .record(
                "borne",
                Reading {
                    kwh: 2_545.6,
                    ts: 9,
                },
                &[],
            )
            .unwrap();
        assert!(store.suspects().unwrap().is_empty());
        // So does another point behind the meter.
        store.hold("borne", Some(suspect)).unwrap();
        let sources = |point: &str| vec![("borne".to_owned(), point.to_owned())];
        store.follow_sources(&sources("a:b/energy")).unwrap();
        store.follow_sources(&sources("c:b/energy")).unwrap();
        assert!(store.suspects().unwrap().is_empty());
    }

    #[test]
    fn a_meter_moved_to_another_point_starts_from_its_first_reading() {
        let store = Store::memory();
        let sources = |point: &str| vec![("grid".to_owned(), point.to_owned())];
        // First run, then the same point again: nothing forgotten.
        assert!(
            store
                .follow_sources(&sources("a:m/energy"))
                .unwrap()
                .is_empty()
        );
        store
            .record(
                "grid",
                Reading { kwh: 3.0, ts: 5 },
                &[row("grid", 0, 1.0, None)],
            )
            .unwrap();
        assert!(
            store
                .follow_sources(&sources("a:m/energy"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.last_readings().unwrap().len(), 1);
        // Another device behind the same meter: its total is not compared
        // with the old one, the hours already filed stay.
        assert_eq!(
            store.follow_sources(&sources("b:m/energy")).unwrap(),
            vec!["grid".to_owned()]
        );
        assert!(store.last_readings().unwrap().is_empty());
        assert_eq!(store.hours(0, 10).unwrap().len(), 1);
        assert!(
            store
                .follow_sources(&sources("b:m/energy"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_house_prices_are_kept_in_order() {
        let store = Store::memory();
        assert!(store.house_prices().unwrap().is_empty());
        let (late, early) = (
            HousePrice {
                since: 20,
                price: 0.25,
            },
            HousePrice {
                since: 10,
                price: 0.2,
            },
        );
        store.add_house_price(late).unwrap();
        store.add_house_price(early).unwrap();
        assert_eq!(store.house_prices().unwrap(), vec![early, late]);
    }

    #[test]
    fn a_new_list_of_prices_prices_the_hours_again_from_where_it_changed() {
        const H: u64 = 3_600_000;
        let store = Store::memory();
        // Three hours of the grid and of the panels, 2 kWh each.
        let rows: Vec<HourRow> = (0..3)
            .flat_map(|i| {
                [
                    row("grid", i * H, 2.0, Some(0.4)),
                    row("pv", i * H, 2.0, None),
                ]
            })
            .collect();
        store.import(&rows).unwrap();
        let cost = |meter: &str, hour: u64| {
            store
                .hours(0, 3 * H)
                .unwrap()
                .into_iter()
                .find(|r| r.meter == meter && r.hour == hour)
                .and_then(|r| r.cost)
        };
        // 0.10 for the first hour, 0.30 from the second one on: only the
        // grid's hours from the second one are priced again.
        let prices = [
            HousePrice {
                since: 0,
                price: 0.10,
            },
            HousePrice {
                since: H,
                price: 0.30,
            },
        ];
        let repriced = store
            .replace_house_prices(&prices, &["grid".to_owned()], H)
            .unwrap();
        assert_eq!(repriced, 2);
        assert_eq!(store.house_prices().unwrap(), prices.to_vec());
        assert_eq!(cost("grid", 0), Some(0.4));
        assert!((cost("grid", H).unwrap() - 0.6).abs() < 1e-9);
        assert!((cost("grid", 2 * H).unwrap() - 0.6).abs() < 1e-9);
        assert_eq!(cost("pv", H), None);
        // Before the first price, an hour has none.
        let later = [HousePrice {
            since: 2 * H,
            price: 0.5,
        }];
        store
            .replace_house_prices(&later, &["grid".to_owned()], 0)
            .unwrap();
        assert_eq!(store.house_prices().unwrap(), later.to_vec());
        assert_eq!(cost("grid", 0), None);
        assert_eq!(cost("grid", H), None);
        assert!((cost("grid", 2 * H).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn imports_stop_where_live_recording_began() {
        const H: u64 = 3_600_000;
        let store = Store::memory();
        // Live recording of « hc » began at 10:30.
        store
            .record(
                "hc",
                Reading {
                    kwh: 5.0,
                    ts: 10 * H + H / 2,
                },
                &[],
            )
            .unwrap();
        store
            .record(
                "hc",
                Reading {
                    kwh: 6.0,
                    ts: 12 * H,
                },
                &[row("hc", 10 * H, 0.2, None), row("hc", 11 * H, 0.8, None)],
            )
            .unwrap();
        let (inserted, repriced, skipped) = store
            .import(&[
                row("hc", 8 * H, 1.0, Some(0.1)),
                row("hc", 9 * H, 1.0, Some(0.1)),
                row("hc", 10 * H, 9.0, None), // overlaps live: refused
                row("hc", 11 * H, 9.0, None),
                row("other", 11 * H, 2.0, None), // never live: taken
            ])
            .unwrap();
        assert_eq!((inserted, skipped), (3, 2), "{repriced}");
        let total: f64 = store.hours(0, 24 * H).unwrap().iter().map(|r| r.kwh).sum();
        assert!((total - 5.0).abs() < 1e-9, "{total}");
        // Running it again changes nothing, except pricing what had no price.
        assert_eq!(
            store.import(&[row("hc", 8 * H, 1.0, None)]).unwrap(),
            (0, 0, 1)
        );
        assert_eq!(
            store
                .import(&[row("other", 11 * H, 2.0, Some(0.3))])
                .unwrap(),
            (0, 1, 0)
        );
    }
}
