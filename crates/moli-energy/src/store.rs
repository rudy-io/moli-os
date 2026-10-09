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

    /// Adds `slices` and sets the meter's last reading, atomically.
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
