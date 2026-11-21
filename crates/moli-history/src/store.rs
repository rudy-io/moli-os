//! SQLite storage. Synchronous: always called from blocking threads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use moli_core::{PointId, Value};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::limits::Limit;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value as Json;

use crate::hourly::{self, HOUR_MS, Hour, hour_of};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS points (
    id    INTEGER PRIMARY KEY,
    point TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS samples (
    point_id INTEGER NOT NULL REFERENCES points(id),
    ts       INTEGER NOT NULL,           -- ms since the Unix epoch
    kind     TEXT    NOT NULL,           -- b(ool) i(nt) f(loat) t(ext) z(null)
    num      REAL,                       -- bool as 0/1, int, float
    txt      TEXT,
    PRIMARY KEY (point_id, ts)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS samples_ts ON samples(ts);
-- What agents query: one row per recorded change.
CREATE VIEW IF NOT EXISTS history AS
    SELECT p.point AS point, s.ts AS ts, s.kind AS kind, s.num AS num, s.txt AS txt
    FROM samples s JOIN points p ON p.id = s.point_id;
-- Hourly statistics of the points kept for ever ([server] history_hourly):
-- time-weighted average, lowest and highest value. Never pruned.
CREATE TABLE IF NOT EXISTS hourly_stats (
    point_id INTEGER NOT NULL REFERENCES points(id),
    hour     INTEGER NOT NULL,           -- ms, start of the hour
    avg      REAL    NOT NULL,
    min      REAL    NOT NULL,
    max      REAL    NOT NULL,
    PRIMARY KEY (point_id, hour)
) WITHOUT ROWID;
-- When each kept point's own statistics began (its oldest change then):
-- imported hours stop there, so no hour is told twice.
CREATE TABLE IF NOT EXISTS hourly_since (
    point_id INTEGER PRIMARY KEY REFERENCES points(id),
    ts       INTEGER NOT NULL
);
-- What agents query: one row per hour of a kept point.
CREATE VIEW IF NOT EXISTS hourly AS
    SELECT p.point AS point, h.hour AS hour, h.avg AS avg, h.min AS min, h.max AS max
    FROM hourly_stats h JOIN points p ON p.id = h.point_id;
";

/// Agents' SQL: time budget per query.
const SQL_BUDGET: Duration = Duration::from_secs(2);
/// Agents' SQL: largest string or blob any expression may build.
const AGENT_MAX_VALUE: i32 = 64 * 1024;
/// Agents' SQL: rough size cap of one answer.
const AGENT_MAX_ANSWER: usize = 1024 * 1024;
/// Same-millisecond samples of one point kept (then the rest is dropped).
const MAX_SAME_MS: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("only a single read-only statement is accepted")]
    NotReadOnly,
    #[error("query took longer than {}s", SQL_BUDGET.as_secs())]
    TooSlow,
    #[error("{0}")]
    Internal(String),
}

/// One bucket of a downsampled numeric series.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Bucket {
    /// First sample's time in the bucket.
    pub ts: u64,
    pub avg: f64,
    pub min: f64,
    pub max: f64,
    /// Changes in the bucket (hours, for a point read hour by hour).
    pub count: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Series {
    pub point: String,
    pub from: u64,
    pub to: u64,
    /// Number of changes recorded in the window.
    pub total: u64,
    /// Last value before the window (a step chart starts from it).
    pub before: Option<(u64, Value)>,
    /// First and last changes inside the window (whatever the shape below).
    pub first: Option<(u64, Value)>,
    pub last: Option<(u64, Value)>,
    /// Every change, when there are at most `max_points` of them.
    pub raw: Vec<(u64, Value)>,
    /// Otherwise, numeric points are bucketed…
    pub buckets: Vec<Bucket>,
    /// …and non-numeric ones keep their most recent changes.
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SqlResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Json>>,
    pub truncated: bool,
}

pub struct Store {
    path: PathBuf,
    writer: Mutex<Connection>,
    reader: Mutex<Connection>,
    agents: Mutex<Connection>,
    ids: Mutex<HashMap<String, i64>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// (kind, num, txt) for storage.
fn encode(value: &Value) -> (&'static str, Option<f64>, Option<&str>) {
    #[allow(clippy::cast_precision_loss)]
    match value {
        Value::Null => ("z", None, None),
        Value::Bool(b) => ("b", Some(f64::from(u8::from(*b))), None),
        Value::Int(i) => ("i", Some(*i as f64), None),
        Value::Float(f) => ("f", Some(*f), None),
        Value::Text(t) => ("t", None, Some(t)),
    }
}

fn decode(kind: &str, num: Option<f64>, txt: Option<String>) -> Value {
    #[allow(clippy::cast_possible_truncation)]
    match (kind, num, txt) {
        ("b", Some(n), _) => Value::Bool(n != 0.0),
        ("i", Some(n), _) => Value::Int(n as i64),
        ("f", Some(n), _) => Value::Float(n),
        ("t", _, Some(t)) => Value::Text(t.into()),
        _ => Value::Null,
    }
}

#[allow(clippy::cast_sign_loss)]
fn ms(ts: i64) -> u64 {
    ts.max(0) as u64
}

fn ts(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, HistoryError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| HistoryError::Internal(e.to_string()))?;
        }
        let writer = Connection::open(path)?;
        writer.pragma_update(None, "journal_mode", "WAL")?;
        writer.pragma_update(None, "synchronous", "NORMAL")?;
        writer.busy_timeout(Duration::from_secs(5))?;
        writer.execute_batch(SCHEMA)?;

        let read_only = || -> Result<Connection, HistoryError> {
            let conn = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.busy_timeout(Duration::from_secs(5))?;
            Ok(conn)
        };
        let reader = read_only()?;
        let mut agents = read_only()?;
        agents.pragma_update(None, "query_only", true)?;
        // Agents read data; they never attach files, change settings, or
        // hold transactions open (a pinned read snapshot would stall the WAL).
        agents.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Attach { .. }
            | AuthAction::Detach { .. }
            | AuthAction::Pragma { .. }
            | AuthAction::Transaction { .. }
            | AuthAction::Savepoint { .. } => Authorization::Deny,
            _ => Authorization::Allow,
        }))?;
        // No giant strings or blobs (printf('%.*c', 1e9, 'x'), zeroblob…).
        agents.set_limit(Limit::SQLITE_LIMIT_LENGTH, AGENT_MAX_VALUE)?;
        Ok(Self {
            path: path.to_path_buf(),
            writer: Mutex::new(writer),
            reader: Mutex::new(reader),
            agents: Mutex::new(agents),
            ids: Mutex::new(HashMap::new()),
        })
    }

    /// Records changes. `restate`: these rows re-state values already known
    /// (current state after a restart or a lag) — an identical sample at the
    /// same instant is then skipped instead of counted twice.
    pub fn insert(
        &self,
        rows: &[(PointId, Value, u64)],
        restate: bool,
    ) -> Result<(), HistoryError> {
        let mut conn = lock(&self.writer);
        let tx = conn.transaction()?;
        {
            let mut ids = lock(&self.ids);
            let mut upsert = tx.prepare(
                "INSERT INTO points(point) VALUES (?1)
                 ON CONFLICT(point) DO UPDATE SET point = excluded.point RETURNING id",
            )?;
            // Two changes in the same millisecond (two button presses…) are
            // two samples: a collision moves the later one by 1 ms.
            let mut sample = tx.prepare(
                "INSERT INTO samples(point_id, ts, kind, num, txt) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(point_id, ts) DO NOTHING",
            )?;
            for (point, value, at) in rows {
                let id = if let Some(id) = ids.get(point.as_str()) {
                    *id
                } else {
                    let id: i64 = upsert.query_row([point.as_str()], |r| r.get(0))?;
                    ids.insert(point.as_str().to_owned(), id);
                    id
                };
                let (kind, num, txt) = encode(value);
                for at in (ts(*at)..).take(MAX_SAME_MS) {
                    if sample.execute(params![id, at, kind, num, txt])? == 1 || restate {
                        break;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Deletes samples older than `cutoff_ms`, except the latest of each
    /// point (a value stable for months must still be known). A point whose
    /// hours are kept (`kept`) also keeps the changes of the hours not filed
    /// yet, and the one before them (where the next hour starts from): a
    /// rollup failing for days loses nothing, and everything else is pruned
    /// all the same. Returns how many.
    pub fn prune(&self, cutoff_ms: u64, kept: &[String]) -> Result<usize, HistoryError> {
        let mut conn = lock(&self.writer);
        let tx = conn.transaction()?;
        // Each kept point: the oldest change its next hours still need.
        let mut needed: Vec<(i64, i64)> = Vec::new();
        for point in kept {
            let Some(id) = point_id(&tx, point)? else {
                continue;
            };
            let from = match own_since(&tx, id)? {
                // Not begun (no rollup went through yet): every change is needed.
                None => 0,
                Some(since) => {
                    let first = hour_of(since);
                    let filed: Option<i64> = tx.query_row(
                        "SELECT MAX(hour) FROM hourly_stats WHERE point_id = ?1 AND hour >= ?2",
                        params![id, ts(first)],
                        |r| r.get(0),
                    )?;
                    filed.map_or(first, |h| ms(h) + HOUR_MS)
                }
            };
            let before: Option<i64> = tx.query_row(
                "SELECT MAX(ts) FROM samples WHERE point_id = ?1 AND ts < ?2",
                params![id, ts(from)],
                |r| r.get(0),
            )?;
            needed.push((id, before.unwrap_or_else(|| ts(from))));
        }
        let latest = "ts < (SELECT MAX(s.ts) FROM samples s WHERE s.point_id = samples.point_id)";
        // Ids read from the database: integers only in the statement.
        let kept_ids = needed
            .iter()
            .map(|(id, _)| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut removed = tx.execute(
            &format!(
                "DELETE FROM samples WHERE ts < ?1 AND {latest} AND point_id NOT IN ({kept_ids})"
            ),
            [ts(cutoff_ms)],
        )?;
        for (id, keep_from) in needed {
            removed += tx.execute(
                &format!("DELETE FROM samples WHERE point_id = ?1 AND ts < ?2 AND {latest}"),
                params![id, ts(cutoff_ms).min(keep_from)],
            )?;
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Files the hourly statistics of `points` for every whole hour before
    /// `now` not filed yet (from where their own statistics began). Run it
    /// before pruning: it reads the changes the pruning removes. Returns
    /// how many hours were filed.
    pub fn rollup(&self, points: &[String], now: u64) -> Result<usize, HistoryError> {
        let mut conn = lock(&self.writer);
        let tx = conn.transaction()?;
        let mut filed = 0;
        for point in points {
            let Some(id) = point_id(&tx, point)? else {
                continue; // never recorded yet
            };
            let Some(since) = begin(&tx, id)? else {
                continue; // no change kept
            };
            let first = hour_of(since);
            let filed_up_to: Option<i64> = tx.query_row(
                "SELECT MAX(hour) FROM hourly_stats WHERE point_id = ?1 AND hour >= ?2",
                params![id, ts(first)],
                |r| r.get(0),
            )?;
            let from = filed_up_to.map_or(first, |h| ms(h) + HOUR_MS);
            let to = hour_of(now);
            if from >= to {
                continue;
            }
            let (before, changes) = numeric_changes(&tx, id, from, to)?;
            let mut add = tx.prepare(
                "INSERT OR REPLACE INTO hourly_stats (point_id, hour, avg, min, max)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for h in hourly::hours(before, &changes, from, to) {
                add.execute(params![id, ts(h.hour), h.avg, h.min, h.max])?;
                filed += 1;
            }
        }
        tx.commit()?;
        Ok(filed)
    }

    /// Adds hours recorded elsewhere (Home Assistant's statistics, a
    /// weather archive) to a point, only before its own statistics began,
    /// where no hour is filed yet, and only hours already over at `now` (a
    /// later one would stop the filing until then: it goes on from the last
    /// hour filed). Returns (inserted, skipped).
    pub fn import_hourly(
        &self,
        point: &str,
        rows: &[Hour],
        now: u64,
    ) -> Result<(usize, usize), HistoryError> {
        let mut conn = lock(&self.writer);
        let tx = conn.transaction()?;
        let id: i64 = tx.query_row(
            "INSERT INTO points(point) VALUES (?1)
             ON CONFLICT(point) DO UPDATE SET point = excluded.point RETURNING id",
            [point],
            |r| r.get(0),
        )?;
        // Not begun yet: its oldest change is where it will begin.
        let own = match own_since(&tx, id)? {
            Some(since) => Some(since),
            None => oldest_change(&tx, id)?,
        };
        let (mut inserted, mut skipped) = (0, 0);
        {
            let mut add = tx.prepare(
                "INSERT INTO hourly_stats (point_id, hour, avg, min, max) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (point_id, hour) DO NOTHING",
            )?;
            for h in rows {
                if h.hour.saturating_add(HOUR_MS) > now
                    || own.is_some_and(|own| h.hour + HOUR_MS > own)
                {
                    skipped += 1;
                } else if add.execute(params![id, ts(h.hour), h.avg, h.min, h.max])? == 1 {
                    inserted += 1;
                } else {
                    skipped += 1;
                }
            }
        }
        tx.commit()?;
        Ok((inserted, skipped))
    }

    pub fn series(
        &self,
        point: &str,
        from: u64,
        to: u64,
        max_points: usize,
    ) -> Result<Series, HistoryError> {
        let conn = lock(&self.reader);
        let mut series = Series {
            point: point.to_owned(),
            from,
            to,
            ..Series::default()
        };
        let Some(id) = conn
            .query_row("SELECT id FROM points WHERE point = ?1", [point], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
        else {
            return Ok(series);
        };
        let (from_ts, to_ts) = (ts(from), ts(to));
        let row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(u64, Value)> {
            Ok((
                ms(r.get(0)?),
                decode(&r.get::<_, String>(1)?, r.get(2)?, r.get(3)?),
            ))
        };
        series.before = conn
            .query_row(
                "SELECT ts, kind, num, txt FROM samples WHERE point_id = ?1 AND ts < ?2 ORDER BY ts DESC LIMIT 1",
                params![id, from_ts],
                row,
            )
            .optional()?;
        let (total, numeric): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COUNT(num) FROM samples WHERE point_id = ?1 AND ts BETWEEN ?2 AND ?3",
            params![id, from_ts, to_ts],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        series.total = u64::try_from(total).unwrap_or(0);
        let max = i64::try_from(max_points.max(1)).unwrap_or(i64::MAX);
        let edge = |order: &str| {
            conn.query_row(
                &format!(
                    "SELECT ts, kind, num, txt FROM samples WHERE point_id = ?1 AND ts BETWEEN ?2 AND ?3 ORDER BY ts {order} LIMIT 1"
                ),
                params![id, from_ts, to_ts],
                row,
            )
            .optional()
        };
        series.first = edge("ASC")?;
        series.last = edge("DESC")?;

        if read_hourly(&conn, id, max_points, &mut series)? {
            return Ok(series);
        }

        if total <= max {
            let mut stmt = conn.prepare(
                "SELECT ts, kind, num, txt FROM samples WHERE point_id = ?1 AND ts BETWEEN ?2 AND ?3 ORDER BY ts",
            )?;
            series.raw = stmt
                .query_map(params![id, from_ts, to_ts], row)?
                .collect::<Result<_, _>>()?;
        } else if numeric > 0 {
            // Numeric points are bucketed even if some readings were null
            // (device offline a moment): nulls simply do not count.
            let width = ((to_ts - from_ts) / max).max(1);
            let mut stmt = conn.prepare(
                "SELECT MIN(ts), AVG(num), MIN(num), MAX(num), COUNT(*) FROM samples
                 WHERE point_id = ?1 AND ts BETWEEN ?2 AND ?3 AND num IS NOT NULL
                 GROUP BY (ts - ?2) / ?4 ORDER BY 1",
            )?;
            series.buckets = stmt
                .query_map(params![id, from_ts, to_ts, width], |r| {
                    Ok(Bucket {
                        ts: ms(r.get(0)?),
                        avg: r.get(1)?,
                        min: r.get(2)?,
                        max: r.get(3)?,
                        count: u64::try_from(r.get::<_, i64>(4)?).unwrap_or(0),
                    })
                })?
                .collect::<Result<_, _>>()?;
        } else {
            let mut stmt = conn.prepare(
                "SELECT ts, kind, num, txt FROM samples WHERE point_id = ?1 AND ts BETWEEN ?2 AND ?3
                 ORDER BY ts DESC LIMIT ?4",
            )?;
            let mut raw: Vec<_> = stmt
                .query_map(params![id, from_ts, to_ts, max], row)?
                .collect::<Result<_, _>>()?;
            raw.reverse();
            series.raw = raw;
            series.truncated = true;
        }
        Ok(series)
    }

    /// Runs an agent's query: one read-only statement, bounded in time and rows.
    pub fn query_sql(&self, sql: &str, max_rows: usize) -> Result<SqlResult, HistoryError> {
        let mut conn = lock(&self.agents);
        let started = Instant::now();
        conn.progress_handler(10_000, Some(move || started.elapsed() > SQL_BUDGET))?;
        let result = run_sql(&conn, sql, max_rows);
        conn.progress_handler(0, None::<fn() -> bool>)?;
        // Belt and braces: never leave a transaction (and its snapshot) open.
        if !conn.is_autocommit() {
            let _ = conn.execute_batch("ROLLBACK");
        }
        result.map_err(|e| match e {
            HistoryError::Db(rusqlite::Error::SqliteFailure(f, _))
                if f.code == rusqlite::ErrorCode::OperationInterrupted =>
            {
                HistoryError::TooSlow
            }
            other => other,
        })
    }
}

fn point_id(conn: &Connection, point: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row("SELECT id FROM points WHERE point = ?1", [point], |r| {
        r.get(0)
    })
    .optional()
}

/// When a kept point's own hourly statistics began.
fn own_since(conn: &Connection, id: i64) -> rusqlite::Result<Option<u64>> {
    Ok(conn
        .query_row(
            "SELECT ts FROM hourly_since WHERE point_id = ?1",
            [id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(ms))
}

fn oldest_change(conn: &Connection, id: i64) -> rusqlite::Result<Option<u64>> {
    Ok(conn
        .query_row(
            "SELECT MIN(ts) FROM samples WHERE point_id = ?1",
            [id],
            |r| r.get::<_, Option<i64>>(0),
        )?
        .map(ms))
}

/// The numeric value held when `from` begins, and the changes in
/// `[from, to)` (a non-numeric one is a gap: `None`).
type Changes = (Option<f64>, Vec<(u64, Option<f64>)>);

fn numeric_changes(conn: &Connection, id: i64, from: u64, to: u64) -> rusqlite::Result<Changes> {
    let before = conn
        .query_row(
            "SELECT num FROM samples WHERE point_id = ?1 AND ts < ?2 ORDER BY ts DESC LIMIT 1",
            params![id, ts(from)],
            |r| r.get::<_, Option<f64>>(0),
        )
        .optional()?
        .flatten();
    let mut stmt = conn.prepare(
        "SELECT ts, num FROM samples WHERE point_id = ?1 AND ts >= ?2 AND ts < ?3 ORDER BY ts",
    )?;
    let changes = stmt
        .query_map(params![id, ts(from), ts(to)], |r| {
            Ok((ms(r.get(0)?), r.get::<_, Option<f64>>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok((before, changes))
}

/// When a kept point's own hourly statistics began: noted the first time,
/// at its oldest change kept (none: nothing to file yet).
fn begin(conn: &Connection, id: i64) -> rusqlite::Result<Option<u64>> {
    if let Some(since) = own_since(conn, id)? {
        return Ok(Some(since));
    }
    let oldest = oldest_change(conn, id)?;
    if let Some(oldest) = oldest {
        conn.execute(
            "INSERT INTO hourly_since (point_id, ts) VALUES (?1, ?2)",
            params![id, ts(oldest)],
        )?;
    }
    Ok(oldest)
}

/// Further back than the changes kept, a point kept for ever is read hour
/// by hour ([server] history_hourly), always bucketed. False: not such a
/// window, or not such a point.
fn read_hourly(
    conn: &Connection,
    id: i64,
    max_points: usize,
    series: &mut Series,
) -> rusqlite::Result<bool> {
    let (from, to) = (series.from, series.to);
    let oldest = oldest_change(conn, id)?;
    if oldest.is_some_and(|oldest| from >= oldest) {
        return Ok(false);
    }
    let hours = hours_in(conn, id, from, to)?;
    let Some(first) = hours.first().filter(|h| oldest.is_none_or(|o| h.hour < o)) else {
        return Ok(false);
    };
    series.first = Some((first.hour, Value::Float(first.avg)));
    if series.last.is_none() {
        series.last = hours.last().map(|h| (h.hour, Value::Float(h.avg)));
    }
    series.raw.clear();
    series.buckets = hourly_buckets(&hours, from, to, max_points);
    Ok(true)
}

/// The hours of a point in `[from, to)`: those filed, then those not
/// filed yet, from its changes (whole hours only).
fn hours_in(conn: &Connection, id: i64, from: u64, to: u64) -> rusqlite::Result<Vec<Hour>> {
    let mut stmt = conn.prepare(
        "SELECT hour, avg, min, max FROM hourly_stats
         WHERE point_id = ?1 AND hour >= ?2 AND hour < ?3 ORDER BY hour",
    )?;
    let mut hours: Vec<Hour> = stmt
        .query_map(params![id, ts(hour_of(from)), ts(to)], |r| {
            Ok(Hour {
                hour: ms(r.get(0)?),
                avg: r.get(1)?,
                min: r.get(2)?,
                max: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let Some(last) = hours.last() else {
        return Ok(hours);
    };
    let (rest, end) = (last.hour + HOUR_MS, hour_of(to));
    if rest < end {
        let (before, changes) = numeric_changes(conn, id, rest, end)?;
        hours.extend(hourly::hours(before, &changes, rest, end));
    }
    Ok(hours)
}

/// Hours as buckets: one per hour when they fit, else `max_points` slices
/// of the window, each hour weighing the same.
#[allow(clippy::cast_precision_loss)] // hours in a bucket
fn hourly_buckets(hours: &[Hour], from: u64, to: u64, max_points: usize) -> Vec<Bucket> {
    let max = u64::try_from(max_points.max(1)).unwrap_or(u64::MAX);
    let width = if hours.len() <= max_points {
        HOUR_MS
    } else {
        to.saturating_sub(from).div_ceil(max).max(HOUR_MS)
    };
    let mut out: Vec<(u64, Bucket)> = Vec::new();
    for h in hours {
        let slot = h.hour.saturating_sub(from) / width;
        match out.last_mut() {
            Some((s, b)) if *s == slot => {
                b.avg += h.avg;
                b.min = b.min.min(h.min);
                b.max = b.max.max(h.max);
                b.count += 1;
            }
            _ => out.push((
                slot,
                Bucket {
                    ts: h.hour,
                    avg: h.avg,
                    min: h.min,
                    max: h.max,
                    count: 1,
                },
            )),
        }
    }
    out.into_iter()
        .map(|(_, mut b)| {
            b.avg /= b.count as f64;
            b
        })
        .collect()
}

fn run_sql(conn: &Connection, sql: &str, max_rows: usize) -> Result<SqlResult, HistoryError> {
    let mut stmt = conn.prepare(sql.trim().trim_end_matches(';'))?;
    if !stmt.readonly() {
        return Err(HistoryError::NotReadOnly);
    }
    let columns: Vec<String> = stmt.column_names().map(str::to_owned).collect();
    let width = columns.len();
    let mut rows = Vec::new();
    let mut truncated = false;
    let mut bytes = 0usize;
    let mut cursor = stmt.query([])?;
    while let Some(row) = cursor.next()? {
        if rows.len() == max_rows || bytes > AGENT_MAX_ANSWER {
            truncated = true;
            break;
        }
        let mut out = Vec::with_capacity(width);
        for i in 0..width {
            out.push(match row.get_ref(i)? {
                ValueRef::Null => Json::Null,
                ValueRef::Integer(n) => n.into(),
                ValueRef::Real(f) => {
                    serde_json::Number::from_f64(f).map_or(Json::Null, Json::Number)
                }
                ValueRef::Text(t) => {
                    bytes += t.len();
                    String::from_utf8_lossy(t).into_owned().into()
                }
                ValueRef::Blob(b) => format!("<{} bytes>", b.len()).into(),
            });
            bytes += 8;
        }
        rows.push(out);
    }
    Ok(SqlResult {
        columns,
        rows,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("moli-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("history.db");
        (Store::open(&path).unwrap(), dir)
    }

    fn p(s: &str) -> PointId {
        PointId::from(s)
    }

    #[test]
    fn values_round_trip_exactly() {
        let (store, dir) = store("types");
        let rows = vec![
            (p("d/on"), Value::Bool(true), 1_000),
            (p("d/on"), Value::Bool(false), 2_000),
            (p("d/n"), Value::Int(42), 1_000),
            (p("d/f"), Value::Float(21.5), 1_000),
            (p("d/t"), Value::from("short_release"), 1_000),
            (p("d/z"), Value::Null, 1_000),
        ];
        store.insert(&rows, false).unwrap();
        let s = store.series("d/on", 0, 10_000, 100).unwrap();
        assert_eq!(
            s.raw,
            vec![(1_000, Value::Bool(true)), (2_000, Value::Bool(false))]
        );
        assert_eq!(
            store.series("d/n", 0, 10_000, 100).unwrap().raw[0].1,
            Value::Int(42)
        );
        assert_eq!(
            store.series("d/t", 0, 10_000, 100).unwrap().raw[0].1,
            Value::from("short_release")
        );
        assert_eq!(
            store.series("d/z", 0, 10_000, 100).unwrap().raw[0].1,
            Value::Null
        );
        assert_eq!(store.series("unknown", 0, 10_000, 100).unwrap().total, 0);
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn long_numeric_series_are_bucketed_with_a_starting_value() {
        let (store, dir) = store("buckets");
        let rows: Vec<_> = (0..1_000u32)
            .map(|i| {
                (
                    p("d/power"),
                    Value::Float(f64::from(i)),
                    10_000 + u64::from(i) * 10,
                )
            })
            .collect();
        store.insert(&rows, false).unwrap();
        store
            .insert(&[(p("d/power"), Value::Float(-1.0), 5_000)], false)
            .unwrap();
        let s = store.series("d/power", 10_000, 20_000, 100).unwrap();
        assert_eq!(s.total, 1_000);
        assert!(s.raw.is_empty());
        assert!(
            s.buckets.len() <= 101 && s.buckets.len() >= 99,
            "{}",
            s.buckets.len()
        );
        assert!(s.buckets[0].min.abs() < 1e-9, "min={}", s.buckets[0].min);
        assert_eq!(s.before, Some((5_000, Value::Float(-1.0))));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn same_millisecond_changes_are_all_kept_and_restatements_are_not() {
        let (store, dir) = store("same-ms");
        let press = || (p("remote/button1"), Value::from("short_release"), 5_000);
        store.insert(&[press(), press()], false).unwrap();
        assert_eq!(
            store.series("remote/button1", 0, 10_000, 10).unwrap().total,
            2
        );
        // Re-recording the current state (after a restart or a lag) adds nothing.
        store.insert(&[press()], true).unwrap();
        assert_eq!(
            store.series("remote/button1", 0, 10_000, 10).unwrap().total,
            2
        );
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nulls_do_not_prevent_bucketing() {
        let (store, dir) = store("nulls");
        let mut rows: Vec<_> = (0..500u32)
            .map(|i| (p("d/t"), Value::Float(f64::from(i)), 1_000 + u64::from(i)))
            .collect();
        rows.push((p("d/t"), Value::Null, 1_250));
        store.insert(&rows, false).unwrap();
        let s = store.series("d/t", 0, 10_000, 50).unwrap();
        assert!(!s.buckets.is_empty());
        assert_eq!(s.first.map(|f| f.1), Some(Value::Float(0.0)));
        assert_eq!(s.last.map(|l| l.1), Some(Value::Float(499.0)));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prune_removes_old_samples() {
        let (store, dir) = store("prune");
        let rows = [
            (p("d/x"), Value::Int(1), 1_000),
            (p("d/x"), Value::Int(2), 9_000),
            (p("d/stable"), Value::Int(7), 1_000),
        ];
        store.insert(&rows, false).unwrap();
        assert_eq!(store.prune(5_000, &[]).unwrap(), 1);
        assert_eq!(store.series("d/x", 0, 10_000, 10).unwrap().total, 1);
        // A value that never changed is still known, however old.
        let stable = store.series("d/stable", 6_000, 10_000, 10).unwrap();
        assert_eq!(stable.before, Some((1_000, Value::Int(7))));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn kept_points_are_filed_hour_by_hour_and_outlive_the_pruning() {
        const H: u64 = HOUR_MS;
        let (store, dir) = store("hourly");
        let kept = vec!["meteo:m/temperature".to_owned()];
        let rows = [
            (p("meteo:m/temperature"), Value::Float(4.0), 100 * H + H / 2),
            (p("meteo:m/temperature"), Value::Float(6.0), 101 * H + H / 2),
            (p("meteo:m/temperature"), Value::Float(8.0), 103 * H),
            (p("other:x/temperature"), Value::Float(20.0), 100 * H),
        ];
        store.insert(&rows, false).unwrap();
        // Whole hours only, from the first change on; others are not kept.
        assert_eq!(store.rollup(&kept, 103 * H + H / 2).unwrap(), 3);
        assert_eq!(store.rollup(&kept, 103 * H + H / 2).unwrap(), 0);
        assert_eq!(store.rollup(&kept, 104 * H).unwrap(), 1);
        let sql = store
            .query_sql(
                "SELECT point, hour, avg, min, max FROM hourly ORDER BY hour",
                10,
            )
            .unwrap();
        assert_eq!(sql.rows.len(), 4);
        assert_eq!(sql.rows[1][2], serde_json::json!(5.0)); // 4 then 6, half and half
        assert_eq!(sql.rows[3][2], serde_json::json!(8.0));

        // Long after: the changes are pruned, the hours stay and are read back.
        assert!(store.prune(103 * H, &kept).unwrap() > 0);
        let s = store
            .series("meteo:m/temperature", 90 * H, 104 * H, 100)
            .unwrap();
        assert!(s.raw.is_empty());
        let hours: Vec<(u64, f64, u64)> =
            s.buckets.iter().map(|b| (b.ts, b.avg, b.count)).collect();
        assert_eq!(
            hours,
            vec![
                (100 * H, 4.0, 1),
                (101 * H, 5.0, 1),
                (102 * H, 6.0, 1),
                (103 * H, 8.0, 1)
            ]
        );
        assert_eq!(s.first, Some((100 * H, Value::Float(4.0))));
        // Too many hours for the points asked: equal slices of the window.
        let s = store
            .series("meteo:m/temperature", 100 * H, 104 * H, 2)
            .unwrap();
        let slices: Vec<(u64, f64, f64, u64)> = s
            .buckets
            .iter()
            .map(|b| (b.ts, b.avg, b.max, b.count))
            .collect();
        assert_eq!(slices, vec![(100 * H, 4.5, 6.0, 2), (102 * H, 7.0, 8.0, 2)]);
        // A recent window still reads the changes themselves.
        let recent = store
            .series("meteo:m/temperature", 103 * H, 104 * H, 100)
            .unwrap();
        assert_eq!(recent.raw, vec![(103 * H, Value::Float(8.0))]);
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failing_rollup_never_stops_the_pruning() {
        const H: u64 = HOUR_MS;
        let (store, dir) = store("hourly-failing");
        let kept = vec!["meteo:m/temperature".to_owned()];
        let at = |v: f64, t: u64| (p("meteo:m/temperature"), Value::Float(v), t);
        store
            .insert(
                &[
                    at(4.0, 100 * H + H / 2),
                    at(6.0, 101 * H + H / 2),
                    (p("other:x/t"), Value::Int(1), 100 * H),
                    (p("other:x/t"), Value::Int(2), 101 * H),
                    (p("other:x/t"), Value::Int(3), 102 * H),
                ],
                false,
            )
            .unwrap();
        // No rollup ever went through: the other points are pruned, the kept
        // one keeps every change.
        assert_eq!(store.prune(200 * H, &kept).unwrap(), 2);
        assert_eq!(store.series("other:x/t", 0, 200 * H, 10).unwrap().total, 1);
        assert_eq!(
            store
                .series("meteo:m/temperature", 0, 200 * H, 10)
                .unwrap()
                .total,
            2
        );

        // Filed up to 102 h, then more changes, then the rollup fails again:
        // only what the next hours need is left, the change before them included.
        assert_eq!(store.rollup(&kept, 102 * H).unwrap(), 2);
        store
            .insert(&[at(8.0, 103 * H), at(9.0, 104 * H)], false)
            .unwrap();
        // 4 at 100:30 is no longer needed: 6 at 101:30 is the value 102 h starts from.
        assert_eq!(store.prune(200 * H, &kept).unwrap(), 1);
        assert_eq!(
            store
                .series("meteo:m/temperature", 0, 200 * H, 10)
                .unwrap()
                .total,
            3
        );
        // Filed later on, from where it stopped, with the right value before.
        assert_eq!(store.rollup(&kept, 105 * H).unwrap(), 3);
        let sql = store
            .query_sql(
                "SELECT hour, avg FROM hourly WHERE hour >= 102 * 3600000 ORDER BY hour",
                10,
            )
            .unwrap();
        assert_eq!(sql.rows.len(), 3);
        assert_eq!(sql.rows[0][1], serde_json::json!(6.0)); // 102 h: nothing changed, 6 all along
        assert_eq!(sql.rows[1][1], serde_json::json!(8.0));
        // Now the old changes can go (the latest before 105 h stays).
        assert_eq!(store.prune(200 * H, &kept).unwrap(), 2);
        assert_eq!(
            store
                .series("meteo:m/temperature", 0, 200 * H, 10)
                .unwrap()
                .total,
            1
        );
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_hour_not_over_is_never_imported() {
        const H: u64 = HOUR_MS;
        let (store, dir) = store("hourly-future");
        let hour = |h: u64| Hour {
            hour: h * H,
            avg: 1.0,
            min: 1.0,
            max: 1.0,
        };
        let point = "pac:x/outside_temperature";
        // At 10:30: 9 h is over, 10 h is not, 50 h is to come.
        assert_eq!(
            store
                .import_hourly(point, &[hour(9), hour(10), hour(50)], 10 * H + H / 2)
                .unwrap(),
            (1, 2)
        );
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn imported_hours_stop_where_moli_began() {
        const H: u64 = HOUR_MS;
        let (store, dir) = store("hourly-import");
        let hour = |h: u64, v: f64| Hour {
            hour: h * H,
            avg: v,
            min: v,
            max: v,
        };
        // Nothing recorded yet: everything is taken, once.
        let point = "pac:x/outside_temperature";
        assert_eq!(
            store
                .import_hourly(point, &[hour(1, 1.0), hour(2, 2.0)], 100 * H)
                .unwrap(),
            (2, 0)
        );
        assert_eq!(
            store
                .import_hourly(point, &[hour(2, 9.0)], 100 * H)
                .unwrap(),
            (0, 1)
        );
        // Moli began at 10:30: the hour it began in and later ones are its own.
        store
            .insert(&[(p(point), Value::Float(5.0), 10 * H + H / 2)], false)
            .unwrap();
        store.rollup(&[point.to_owned()], 12 * H).unwrap();
        assert_eq!(
            store
                .import_hourly(
                    point,
                    &[hour(9, 3.0), hour(10, 3.0), hour(11, 3.0)],
                    100 * H
                )
                .unwrap(),
            (1, 2)
        );
        let s = store.series(point, 0, 12 * H, 100).unwrap();
        let hours: Vec<(u64, f64)> = s.buckets.iter().map(|b| (b.ts, b.avg)).collect();
        assert_eq!(
            hours,
            vec![
                (H, 1.0),
                (2 * H, 2.0),
                (9 * H, 3.0),
                (10 * H, 5.0),
                (11 * H, 5.0)
            ]
        );
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn agents_read_but_never_write() {
        let (store, dir) = store("sql");
        store
            .insert(
                &[
                    (p("door/contact"), Value::Bool(false), 1_000),
                    (p("door/contact"), Value::Bool(true), 2_000),
                ],
                false,
            )
            .unwrap();
        let r = store
            .query_sql(
                "SELECT point, COUNT(*) AS changes FROM history GROUP BY point;",
                10,
            )
            .unwrap();
        assert_eq!(r.columns, ["point", "changes"]);
        assert_eq!(
            r.rows,
            vec![vec![Json::from("door/contact"), Json::from(2)]]
        );

        for forbidden in [
            "DELETE FROM samples",
            "DROP VIEW history",
            "INSERT INTO points(point) VALUES ('x')",
            "ATTACH DATABASE '/etc/passwd' AS x",
            "PRAGMA query_only = 0",
            "SELECT 1; DELETE FROM samples",
            "BEGIN",
            "SAVEPOINT pin",
            "SELECT printf('%.*c', 100000000, 'x')",
            "SELECT zeroblob(100000000)",
        ] {
            assert!(
                store.query_sql(forbidden, 10).is_err(),
                "{forbidden} must be refused"
            );
        }
        assert_eq!(
            store.series("door/contact", 0, 10_000, 10).unwrap().total,
            2,
            "nothing changed"
        );

        let capped = store.query_sql("SELECT ts FROM history", 1).unwrap();
        assert!(capped.truncated);
        let slow = store.query_sql(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT COUNT(*) FROM c",
            10,
        );
        assert!(matches!(slow, Err(HistoryError::TooSlow)), "{slow:?}");
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }
}
