//! SQLite storage. Synchronous: always called from blocking threads.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::model::{Flexibility, Item, Offer, OfferStatus, Product, ProviderSettings, Reading};
use crate::providers::PROVIDERS;

/// The whole schema for a new database. The offers, readings, baskets and
/// purchases tables are filled by later features but exist from day one.
/// A column added after a database was first created is also declared here,
/// and added to older databases by [`migrate`]. Money is `INTEGER` cents,
/// times are milliseconds since the Unix epoch.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS products (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    detail         TEXT NOT NULL DEFAULT '',
    flexibility    TEXT NOT NULL DEFAULT 'strict'
                   CHECK (flexibility IN ('strict', 'format', 'brand', 'cheapest')),
    fixed_provider TEXT,
    aisle          TEXT
);
-- Several candidate sheets per (product, provider), at most one 'confirmed'; a sheet
-- stays 'pending' until a human confirms it.
CREATE TABLE IF NOT EXISTS offers (
    id           INTEGER PRIMARY KEY,
    product      TEXT NOT NULL,
    provider     TEXT NOT NULL,
    reference    TEXT NOT NULL,
    label        TEXT NOT NULL DEFAULT '',
    ean          TEXT,
    unit_qty     REAL,
    unit         TEXT,
    status       TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'confirmed')),
    confirmed_by TEXT,
    confirmed_at INTEGER,
    UNIQUE (product, provider, reference)
);
CREATE TABLE IF NOT EXISTS items (
    id         INTEGER PRIMARY KEY,
    product    TEXT,
    text       TEXT NOT NULL,
    qty        REAL NOT NULL,
    added_by   TEXT NOT NULL,
    channel    TEXT NOT NULL,
    added_at   INTEGER NOT NULL,
    removed_at INTEGER,
    removed_by TEXT,
    done_at    INTEGER,
    basket     INTEGER
);
CREATE INDEX IF NOT EXISTS items_open ON items(product) WHERE removed_at IS NULL AND done_at IS NULL;
-- A price seen in a store at a given time.
CREATE TABLE IF NOT EXISTS readings (
    id               INTEGER PRIMARY KEY,
    offer            INTEGER NOT NULL REFERENCES offers(id),
    store            TEXT NOT NULL,
    price_cents      INTEGER NOT NULL,
    unit_price_cents INTEGER,
    promo            TEXT,
    available        INTEGER NOT NULL DEFAULT 1,
    read_at          INTEGER NOT NULL,
    read_by          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS readings_offer ON readings(offer, store, read_at);
CREATE TABLE IF NOT EXISTS providers (
    id              TEXT PRIMARY KEY,
    enabled         INTEGER NOT NULL DEFAULT 1,
    store           TEXT NOT NULL DEFAULT '',
    min_order_cents INTEGER NOT NULL DEFAULT 0,
    fee_cents       INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS baskets (
    id         INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    status     TEXT NOT NULL,
    options    TEXT NOT NULL DEFAULT '[]',   -- JSON
    deviations TEXT NOT NULL DEFAULT '[]',   -- JSON
    chosen     TEXT,
    accepted   TEXT NOT NULL DEFAULT '[]',   -- JSON
    chosen_by  TEXT,
    chosen_at  INTEGER,
    used       TEXT NOT NULL DEFAULT '[]'    -- JSON: [item, provider, offer] priced in the options
);
-- How many times a deviation of a kind was refused for a product.
CREATE TABLE IF NOT EXISTS refusals (
    product TEXT NOT NULL,
    kind    TEXT NOT NULL,
    count   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (product, kind)
);
CREATE TABLE IF NOT EXISTS purchases (
    id          INTEGER PRIMARY KEY,
    product     TEXT NOT NULL,
    offer       INTEGER,
    provider    TEXT NOT NULL,
    store       TEXT NOT NULL,
    qty         REAL NOT NULL,
    price_cents INTEGER NOT NULL,
    at          INTEGER NOT NULL,
    source      TEXT NOT NULL,
    basket      INTEGER,
    status      TEXT NOT NULL DEFAULT 'confirmed',
    receipt        INTEGER,
    label          TEXT NOT NULL DEFAULT '',
    line_cents     INTEGER,
    discount_cents INTEGER NOT NULL DEFAULT 0,
    unit           TEXT
);
";

/// Columns added to `purchases` after the first version: (name, definition).
const PURCHASE_ADDED_COLUMNS: [(&str, &str); 5] = [
    ("receipt", "INTEGER"),
    ("label", "TEXT NOT NULL DEFAULT ''"),
    ("line_cents", "INTEGER"),
    ("discount_cents", "INTEGER NOT NULL DEFAULT 0"),
    ("unit", "TEXT"),
];

/// Brings an older database up to the current schema: adds the missing
/// columns, then fills the cost of the lines that predate them. Safe to run
/// on every open.
fn migrate(conn: &Connection) -> Result<(), PantryError> {
    let tx = conn.unchecked_transaction()?;
    let present: Vec<String> = tx
        .prepare("SELECT name FROM pragma_table_info('purchases')")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for (name, definition) in PURCHASE_ADDED_COLUMNS {
        if !present.iter().any(|c| c == name) {
            tx.execute(
                &format!("ALTER TABLE purchases ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    tx.execute(
        "UPDATE purchases
         SET line_cents = CAST(ROUND(qty * price_cents) AS INTEGER) - discount_cents
         WHERE line_cents IS NULL",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

const ITEM_COLUMNS: &str = "id, product, text, qty, added_by, channel, added_at, basket";
const OFFER_COLUMNS: &str = "id, product, provider, reference, label, ean, unit_qty, unit, status";
const SPLIT_THRESHOLD: &str = "split_threshold";
const DEFAULT_SPLIT_THRESHOLD: f64 = 0.30;
const SPLIT_RANGE: std::ops::RangeInclusive<f64> = 0.0..=0.9;
/// An item is on the list while neither removed nor bought.
const OPEN: &str = "removed_at IS NULL AND done_at IS NULL";
/// An item not yet in a drive's cart: tied to no basket, or to one that is
/// only proposed or chosen. A line in a `filled` or `ordered` basket is being
/// bought: it is neither proposed again nor merged with a new request.
pub(crate) const NOT_IN_A_CART: &str = "(basket IS NULL OR basket NOT IN \
     (SELECT id FROM baskets WHERE status IN ('filled', 'ordered')))";

#[derive(Debug, thiserror::Error)]
pub enum PantryError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    /// A blocking task that did not finish (it panicked).
    #[error("internal: {0}")]
    Internal(String),
}

pub struct Store {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub(crate) fn ts(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

#[allow(clippy::cast_sign_loss)]
pub(crate) fn ms(ts: i64) -> u64 {
    ts.max(0) as u64
}

/// Two free-text lines are the same need when their words match once trimmed
/// and lowercased.
fn same_words(text: &str) -> String {
    text.trim().to_lowercase()
}

fn item_row(r: &Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        id: r.get(0)?,
        product: r.get(1)?,
        text: r.get(2)?,
        qty: r.get(3)?,
        added_by: r.get(4)?,
        channel: r.get(5)?,
        added_at: ms(r.get(6)?),
        basket: r.get(7)?,
    })
}

fn product_row(r: &Row<'_>) -> rusqlite::Result<Product> {
    let flexibility: String = r.get(3)?;
    Ok(Product {
        id: r.get(0)?,
        name: r.get(1)?,
        detail: r.get(2)?,
        flexibility: Flexibility::parse(&flexibility).unwrap_or_default(),
        fixed_provider: r.get(4)?,
        aisle: r.get(5)?,
    })
}

fn offer_row(r: &Row<'_>) -> rusqlite::Result<Offer> {
    let status: String = r.get(8)?;
    Ok(Offer {
        id: r.get(0)?,
        product: r.get(1)?,
        provider: r.get(2)?,
        reference: r.get(3)?,
        label: r.get(4)?,
        ean: r.get(5)?,
        unit_qty: r.get(6)?,
        unit: r.get(7)?,
        status: OfferStatus::parse(&status).unwrap_or_default(),
    })
}

fn reading_row(r: &Row<'_>) -> rusqlite::Result<Reading> {
    Ok(Reading {
        offer: r.get(0)?,
        store: r.get(1)?,
        price_cents: r.get(2)?,
        unit_price_cents: r.get(3)?,
        promo: r.get(4)?,
        available: r.get(5)?,
        read_at: ms(r.get(6)?),
        read_by: r.get(7)?,
    })
}

fn check_qty(qty: f64) -> Result<(), PantryError> {
    if qty.is_finite() && qty > 0.0 {
        Ok(())
    } else {
        Err(PantryError::Invalid(
            "the quantity must be a number above zero".into(),
        ))
    }
}

/// Confirms a sheet inside a transaction already open (see [`Store::confirm_offer`]).
pub(crate) fn confirm_offer_in(conn: &Connection, id: i64, by: &str) -> Result<(), PantryError> {
    let key: Option<(String, String)> = conn
        .query_row(
            "SELECT product, provider FROM offers WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((product, provider)) = key else {
        return Err(PantryError::NotFound(format!("offer {id}")));
    };
    conn.execute(
        "UPDATE offers SET status = 'pending', confirmed_by = NULL, confirmed_at = NULL
         WHERE product = ?1 AND provider = ?2 AND id <> ?3",
        params![product, provider, id],
    )?;
    conn.execute(
        "UPDATE offers SET status = 'confirmed', confirmed_by = ?2, confirmed_at = ?3
         WHERE id = ?1",
        params![id, by, ts(now_ms())],
    )?;
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, PantryError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                PantryError::Invalid(format!("cannot create {}: {e}", dir.display()))
            })?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self {
            path: path.to_path_buf(),
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds a need to the list. When the same need is already open (same
    /// product, or same words for a free-text line) its quantity grows and the
    /// first requester stays the author: two people asking for diapers at the
    /// same time make one line. A line already in a filled or ordered cart is
    /// not that need any more: the new request makes a new line, so the
    /// pick-up does not close it.
    pub fn add_item(
        &self,
        text: &str,
        product: Option<&str>,
        qty: f64,
        by: &str,
        channel: &str,
    ) -> Result<Item, PantryError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(PantryError::Invalid("the item needs a text".into()));
        }
        check_qty(qty)?;
        // One writer at a time: the lookup and the write cannot be interleaved
        // with another request for the same need.
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let existing: Option<i64> = if let Some(product) = product {
            tx.query_row(
                &format!(
                    "SELECT id FROM items WHERE product = ?1 AND {OPEN} AND {NOT_IN_A_CART}
                     ORDER BY id LIMIT 1"
                ),
                [product],
                |r| r.get(0),
            )
            .optional()?
        } else {
            let wanted = same_words(text);
            let mut stmt = tx.prepare(&format!(
                "SELECT id, text FROM items WHERE product IS NULL AND {OPEN} AND {NOT_IN_A_CART}
                 ORDER BY id"
            ))?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            let mut found = None;
            for row in rows {
                let (id, other) = row?;
                if same_words(&other) == wanted {
                    found = Some(id);
                    break;
                }
            }
            found
        };
        let id = if let Some(id) = existing {
            tx.execute(
                "UPDATE items SET qty = qty + ?2 WHERE id = ?1",
                params![id, qty],
            )?;
            id
        } else {
            tx.execute(
                "INSERT INTO items(product, text, qty, added_by, channel, added_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![product, text, qty, by, channel, ts(now_ms())],
            )?;
            tx.last_insert_rowid()
        };
        let item = tx.query_row(
            &format!("SELECT {ITEM_COLUMNS} FROM items WHERE id = ?1"),
            [id],
            item_row,
        )?;
        tx.commit()?;
        Ok(item)
    }

    /// Takes a line off the list. It stays in the database (statistics).
    pub fn remove_item(&self, id: i64, by: &str) -> Result<(), PantryError> {
        let n = self.lock().execute(
            &format!("UPDATE items SET removed_at = ?2, removed_by = ?3 WHERE id = ?1 AND {OPEN}"),
            params![id, ts(now_ms()), by],
        )?;
        if n == 0 {
            return Err(PantryError::NotFound(format!("item {id}")));
        }
        Ok(())
    }

    pub fn set_qty(&self, id: i64, qty: f64) -> Result<Item, PantryError> {
        check_qty(qty)?;
        let conn = self.lock();
        let n = conn.execute(
            &format!("UPDATE items SET qty = ?2 WHERE id = ?1 AND {OPEN}"),
            params![id, qty],
        )?;
        if n == 0 {
            return Err(PantryError::NotFound(format!("item {id}")));
        }
        Ok(conn.query_row(
            &format!("SELECT {ITEM_COLUMNS} FROM items WHERE id = ?1"),
            [id],
            item_row,
        )?)
    }

    /// The shopping list: lines neither removed nor bought, oldest first.
    pub fn open_items(&self) -> Result<Vec<Item>, PantryError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM items WHERE {OPEN} ORDER BY id"
        ))?;
        Ok(stmt
            .query_map([], item_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// The open lines a new basket may buy: those not already in a filled or
    /// ordered cart, oldest first.
    pub(crate) fn items_to_buy(&self) -> Result<Vec<Item>, PantryError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM items WHERE {OPEN} AND {NOT_IN_A_CART} ORDER BY id"
        ))?;
        Ok(stmt
            .query_map([], item_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Creates a product, or replaces the one with the same id.
    pub fn save_product(&self, p: &Product) -> Result<(), PantryError> {
        if p.id.trim().is_empty() || p.name.trim().is_empty() {
            return Err(PantryError::Invalid(
                "a product needs an id and a name".into(),
            ));
        }
        self.lock().execute(
            "INSERT INTO products(id, name, detail, flexibility, fixed_provider, aisle)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, detail = excluded.detail,
                 flexibility = excluded.flexibility, fixed_provider = excluded.fixed_provider,
                 aisle = excluded.aisle",
            params![
                p.id,
                p.name,
                p.detail,
                p.flexibility.as_str(),
                p.fixed_provider,
                p.aisle
            ],
        )?;
        Ok(())
    }

    pub fn products(&self) -> Result<Vec<Product>, PantryError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, detail, flexibility, fixed_provider, aisle FROM products
             ORDER BY name COLLATE NOCASE, id",
        )?;
        Ok(stmt
            .query_map([], product_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Creates a sheet, or updates the one with the same product, chain and
    /// reference. A new sheet starts `Pending` (the `status` field is
    /// ignored): only [`Store::confirm_offer`] confirms. An update that leaves
    /// the label, EAN, pack size and unit as they were keeps the status; one
    /// that changes any of them sends the sheet back to `Pending`, because the
    /// human checked what was described, not what it now says.
    pub fn save_offer(&self, o: &Offer) -> Result<i64, PantryError> {
        if o.product.trim().is_empty()
            || o.provider.trim().is_empty()
            || o.reference.trim().is_empty()
        {
            return Err(PantryError::Invalid(
                "an offer needs a product, a provider and a reference".into(),
            ));
        }
        let conn = self.lock();
        Ok(conn.query_row(
            "INSERT INTO offers(product, provider, reference, label, ean, unit_qty, unit)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(product, provider, reference) DO UPDATE SET
                 status = CASE WHEN label IS excluded.label AND ean IS excluded.ean
                     AND unit_qty IS excluded.unit_qty AND unit IS excluded.unit
                     THEN status ELSE 'pending' END,
                 confirmed_by = CASE WHEN label IS excluded.label AND ean IS excluded.ean
                     AND unit_qty IS excluded.unit_qty AND unit IS excluded.unit
                     THEN confirmed_by ELSE NULL END,
                 confirmed_at = CASE WHEN label IS excluded.label AND ean IS excluded.ean
                     AND unit_qty IS excluded.unit_qty AND unit IS excluded.unit
                     THEN confirmed_at ELSE NULL END,
                 label = excluded.label, ean = excluded.ean,
                 unit_qty = excluded.unit_qty, unit = excluded.unit
             RETURNING id",
            params![
                o.product,
                o.provider,
                o.reference,
                o.label,
                o.ean,
                o.unit_qty,
                o.unit
            ],
            |r| r.get(0),
        )?)
    }

    /// A human validates a sheet: it becomes `Confirmed` and the other sheets
    /// of the same product in the same chain go back to `Pending`.
    pub fn confirm_offer(&self, id: i64, by: &str) -> Result<(), PantryError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        confirm_offer_in(&tx, id, by)?;
        tx.commit()?;
        Ok(())
    }

    /// The sheets, of one product or of all, oldest first.
    pub fn offers(&self, product: Option<&str>) -> Result<Vec<Offer>, PantryError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {OFFER_COLUMNS} FROM offers WHERE ?1 IS NULL OR product = ?1 ORDER BY id"
        ))?;
        Ok(stmt
            .query_map([product], offer_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Stores price readings, all or none: an unknown sheet or a negative price
    /// refuses the whole batch.
    pub fn record_readings(&self, readings: &[Reading]) -> Result<(), PantryError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for r in readings {
            if r.price_cents < 0 || r.unit_price_cents.is_some_and(|p| p < 0) {
                return Err(PantryError::Invalid("a price cannot be negative".into()));
            }
            let known: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM offers WHERE id = ?1)",
                [r.offer],
                |row| row.get(0),
            )?;
            if !known {
                return Err(PantryError::NotFound(format!("offer {}", r.offer)));
            }
            tx.execute(
                "INSERT INTO readings(offer, store, price_cents, unit_price_cents, promo,
                                      available, read_at, read_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    r.offer,
                    r.store,
                    r.price_cents,
                    r.unit_price_cents,
                    r.promo,
                    r.available,
                    ts(r.read_at),
                    r.read_by
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The most recent reading of each sheet in each store, kept only when it
    /// was taken at `newer_than` or later (ms): a store whose latest price is
    /// too old yields nothing, never an even older price.
    pub fn latest_readings(&self, newer_than: u64) -> Result<Vec<Reading>, PantryError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT r.offer, r.store, r.price_cents, r.unit_price_cents, r.promo, r.available,
                    r.read_at, r.read_by
             FROM readings r
             WHERE r.read_at >= ?1
               AND r.id = (SELECT id FROM readings
                           WHERE offer = r.offer AND store = r.store
                           ORDER BY read_at DESC, id DESC LIMIT 1)
             ORDER BY r.offer, r.store",
        )?;
        Ok(stmt
            .query_map([ts(newer_than)], reading_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// One line per known chain, in the order of [`PROVIDERS`]; a chain nobody
    /// has set up yet is disabled.
    pub fn provider_settings(&self) -> Result<Vec<ProviderSettings>, PantryError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT enabled, store, min_order_cents, fee_cents FROM providers WHERE id = ?1",
        )?;
        PROVIDERS
            .iter()
            .map(|p| {
                let saved = stmt
                    .query_row([p.id], |r| {
                        Ok(ProviderSettings {
                            id: p.id.into(),
                            enabled: r.get(0)?,
                            store: r.get(1)?,
                            min_order_cents: r.get(2)?,
                            fee_cents: r.get(3)?,
                        })
                    })
                    .optional()?;
                Ok(saved.unwrap_or_else(|| ProviderSettings {
                    id: p.id.into(),
                    enabled: false,
                    store: String::new(),
                    min_order_cents: 0,
                    fee_cents: 0,
                }))
            })
            .collect()
    }

    pub fn save_provider_settings(&self, s: &ProviderSettings) -> Result<(), PantryError> {
        if !PROVIDERS.iter().any(|p| p.id == s.id) {
            return Err(PantryError::Invalid(format!("unknown provider: {}", s.id)));
        }
        if s.min_order_cents < 0 || s.fee_cents < 0 {
            return Err(PantryError::Invalid(
                "the minimum order and the fee cannot be negative".into(),
            ));
        }
        self.lock().execute(
            "INSERT INTO providers(id, enabled, store, min_order_cents, fee_cents)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET enabled = excluded.enabled, store = excluded.store,
                 min_order_cents = excluded.min_order_cents, fee_cents = excluded.fee_cents",
            params![s.id, s.enabled, s.store, s.min_order_cents, s.fee_cents],
        )?;
        Ok(())
    }

    /// How much cheaper (0.30 = 30 %) a split of the list over two drives must
    /// be than the best single drive before it is recommended.
    pub fn split_threshold(&self) -> Result<f64, PantryError> {
        let saved: Option<String> = self
            .lock()
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [SPLIT_THRESHOLD],
                |r| r.get(0),
            )
            .optional()?;
        Ok(saved
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| SPLIT_RANGE.contains(v))
            .unwrap_or(DEFAULT_SPLIT_THRESHOLD))
    }

    pub fn set_split_threshold(&self, v: f64) -> Result<(), PantryError> {
        if !SPLIT_RANGE.contains(&v) {
            return Err(PantryError::Invalid(
                "the threshold must be between 0 and 0.9".into(),
            ));
        }
        self.lock().execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SPLIT_THRESHOLD, v.to_string()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Flexibility, Product};

    fn store(name: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("moli-pantry-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("pantry.db");
        (Store::open(&path).unwrap(), dir)
    }

    /// The `purchases` table as the first version created it.
    const V1_PURCHASES: &str = "
        CREATE TABLE purchases (
            id          INTEGER PRIMARY KEY,
            product     TEXT NOT NULL,
            offer       INTEGER,
            provider    TEXT NOT NULL,
            store       TEXT NOT NULL,
            qty         REAL NOT NULL,
            price_cents INTEGER NOT NULL,
            at          INTEGER NOT NULL,
            source      TEXT NOT NULL,
            basket      INTEGER,
            status      TEXT NOT NULL DEFAULT 'confirmed'
        );
        INSERT INTO purchases(product, provider, store, qty, price_cents, at, source, status)
        VALUES ('lait', 'carrefour', 'Ville-A', 2.0, 399, 1000, 'ticket', 'confirmed');";

    #[test]
    fn an_old_database_is_migrated_once() {
        let dir = std::env::temp_dir().join(format!("moli-pantry-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pantry.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(V1_PURCHASES)
            .unwrap();
        for _ in 0..2 {
            let store = Store::open(&path).unwrap();
            let lines = store.purchases(0, u64::MAX / 4).unwrap();
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].line_cents, Some(798));
            assert_eq!(lines[0].discount_cents, 0);
            assert_eq!(lines[0].label, "");
            assert_eq!(lines[0].receipt, None);
            assert_eq!(lines[0].unit, None);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn adding_twice_the_same_product_raises_the_quantity() {
        let (store, dir) = store("same-product");
        store
            .add_item("couches", Some("couches"), 1.0, "Élodie", "ui")
            .unwrap();
        store
            .add_item("Couches", Some("couches"), 1.0, "Sam", "agent")
            .unwrap();
        let items = store.open_items().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].qty, 2.0);
        assert_eq!(items[0].added_by, "Élodie");
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn free_text_items_merge_when_the_same_words() {
        let (store, dir) = store("free-text");
        store
            .add_item("lessive", None, 1.0, "Élodie", "ui")
            .unwrap();
        store.add_item("Lessive  ", None, 1.0, "Sam", "ui").unwrap();
        let items = store.open_items().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].qty, 2.0);
        store
            .add_item("lessive bébé", None, 1.0, "Sam", "ui")
            .unwrap();
        assert_eq!(store.open_items().unwrap().len(), 2);
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn removed_items_leave_the_list() {
        let (store, dir) = store("removed");
        let item = store.add_item("lait", None, 1.0, "Élodie", "ui").unwrap();
        store.remove_item(item.id, "Sam").unwrap();
        assert!(store.open_items().unwrap().is_empty());
        // The line stays in the database, for the statistics.
        let removed: Option<i64> = store
            .lock()
            .query_row(
                "SELECT removed_at FROM items WHERE id = ?1",
                [item.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(removed.is_some());
        // Removing twice, or an unknown line, is a not-found.
        assert!(matches!(
            store.remove_item(item.id, "Sam"),
            Err(PantryError::NotFound(_))
        ));
        // A removed line does not absorb a new request.
        let again = store.add_item("lait", None, 1.0, "Sam", "ui").unwrap();
        assert_ne!(again.id, item.id);
        assert_eq!(again.qty, 1.0);
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn products_round_trip() {
        let (store, dir) = store("products");
        let plain = Product {
            id: "couches".into(),
            name: "Couches taille 4".into(),
            ..Product::default()
        };
        let picky = Product {
            id: "lait".into(),
            name: "Lait demi-écrémé".into(),
            detail: "1 L, UHT".into(),
            flexibility: Flexibility::Brand,
            fixed_provider: Some("ville-a".into()),
            aisle: Some("crèmerie".into()),
        };
        store.save_product(&plain).unwrap();
        store.save_product(&picky).unwrap();
        let mut got = store.products().unwrap();
        got.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(got, vec![plain.clone(), picky]);
        assert_eq!(got[0].flexibility, Flexibility::Strict);
        // Saving again replaces.
        let renamed = Product {
            name: "Couches T4".into(),
            ..plain
        };
        store.save_product(&renamed).unwrap();
        assert_eq!(store.products().unwrap().len(), 2);
        assert!(store.products().unwrap().contains(&renamed));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn set_qty_rejects_zero_or_negative() {
        let (store, dir) = store("qty");
        let item = store.add_item("pâtes", None, 1.0, "Sam", "ui").unwrap();
        assert!(matches!(
            store.set_qty(item.id, 0.0),
            Err(PantryError::Invalid(_))
        ));
        assert!(matches!(
            store.set_qty(item.id, -1.0),
            Err(PantryError::Invalid(_))
        ));
        assert!(matches!(
            store.set_qty(item.id, f64::NAN),
            Err(PantryError::Invalid(_))
        ));
        assert_eq!(store.set_qty(item.id, 3.0).unwrap().qty, 3.0);
        assert!(matches!(
            store.set_qty(999, 1.0),
            Err(PantryError::NotFound(_))
        ));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_input_is_refused() {
        let (store, dir) = store("invalid");
        assert!(matches!(
            store.add_item("  ", None, 1.0, "Sam", "ui"),
            Err(PantryError::Invalid(_))
        ));
        assert!(matches!(
            store.add_item("riz", None, 0.0, "Sam", "ui"),
            Err(PantryError::Invalid(_))
        ));
        assert!(matches!(
            store.save_product(&Product::default()),
            Err(PantryError::Invalid(_))
        ));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_whole_v1_schema_exists_from_the_start() {
        let (store, dir) = store("schema");
        let names: Vec<String> = {
            let conn = store.lock();
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        for table in [
            "baskets",
            "items",
            "offers",
            "products",
            "providers",
            "purchases",
            "readings",
            "refusals",
            "settings",
        ] {
            assert!(
                names.iter().any(|n| n == table),
                "{table} missing: {names:?}"
            );
        }
        // Reopening an existing database keeps its content.
        store.add_item("sel", None, 1.0, "Sam", "ui").unwrap();
        let path = dir.join("pantry.db");
        drop(store);
        let again = Store::open(&path).unwrap();
        assert_eq!(again.open_items().unwrap().len(), 1);
        drop(again);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn offer(product: &str, provider: &str, reference: &str) -> Offer {
        Offer {
            product: product.into(),
            provider: provider.into(),
            reference: reference.into(),
            label: format!("{product} {reference}"),
            ..Offer::default()
        }
    }

    fn reading(offer: i64, price_cents: i64, read_at: u64) -> Reading {
        Reading {
            offer,
            store: "Ville-A".into(),
            price_cents,
            unit_price_cents: None,
            promo: None,
            available: true,
            read_at,
            read_by: "Élodie".into(),
        }
    }

    #[test]
    fn confirming_an_offer_unconfirms_its_rivals() {
        let (store, dir) = store("confirm");
        let first = store
            .save_offer(&offer("couches", "carrefour", "A1"))
            .unwrap();
        let second = store
            .save_offer(&offer("couches", "carrefour", "B2"))
            .unwrap();
        // Another chain's sheet for the same product is not a rival.
        let other = store
            .save_offer(&offer("couches", "intermarche", "C3"))
            .unwrap();
        store.confirm_offer(other, "Sam").unwrap();
        store.confirm_offer(first, "Élodie").unwrap();
        store.confirm_offer(second, "Sam").unwrap();
        let status = |id: i64| {
            store
                .offers(Some("couches"))
                .unwrap()
                .into_iter()
                .find(|o| o.id == id)
                .unwrap()
                .status
        };
        assert_eq!(status(first), OfferStatus::Pending);
        assert_eq!(status(second), OfferStatus::Confirmed);
        assert_eq!(status(other), OfferStatus::Confirmed);
        assert!(matches!(
            store.confirm_offer(999, "Sam"),
            Err(PantryError::NotFound(_))
        ));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saving_an_offer_again_updates_it_in_place() {
        let (store, dir) = store("offer-upsert");
        let mut o = offer("lait", "carrefour", "L1");
        let id = store.save_offer(&o).unwrap();
        store.confirm_offer(id, "Sam").unwrap();
        // Same data again: the human's check still stands.
        assert_eq!(store.save_offer(&o).unwrap(), id);
        assert_eq!(
            store.offers(None).unwrap()[0].status,
            OfferStatus::Confirmed
        );
        // Different data: what was checked is not what is described now.
        o.label = "Lait 1 L".into();
        o.ean = Some("3000000000000".into());
        o.unit_qty = Some(1.0);
        o.unit = Some("L".into());
        assert_eq!(store.save_offer(&o).unwrap(), id);
        let all = store.offers(None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].label, "Lait 1 L");
        assert_eq!(all[0].ean.as_deref(), Some("3000000000000"));
        assert_eq!(all[0].unit_qty, Some(1.0));
        assert_eq!(all[0].status, OfferStatus::Pending);
        // Another reference of the same product and chain is a new candidate.
        let other = store.save_offer(&offer("lait", "carrefour", "L2")).unwrap();
        assert_ne!(other, id);
        assert_eq!(store.offers(Some("lait")).unwrap().len(), 2);
        assert!(store.offers(Some("riz")).unwrap().is_empty());
        assert!(matches!(
            store.save_offer(&offer("", "carrefour", "L3")),
            Err(PantryError::Invalid(_))
        ));
        assert!(matches!(
            store.save_offer(&offer("lait", "carrefour", " ")),
            Err(PantryError::Invalid(_))
        ));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn changing_a_confirmed_offer_sends_it_back_to_pending() {
        let (store, dir) = store("offer-reset");
        let mut o = offer("lait", "carrefour", "L1");
        o.unit_qty = Some(1.0);
        let id = store.save_offer(&o).unwrap();
        store.confirm_offer(id, "Sam").unwrap();
        let checked = |store: &Store| -> (OfferStatus, Option<String>, Option<i64>) {
            let status = store.offers(None).unwrap()[0].status;
            let (by, at) = store
                .lock()
                .query_row(
                    "SELECT confirmed_by, confirmed_at FROM offers WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            (status, by, at)
        };
        // The same data, saved again: still confirmed, by the same person.
        store.save_offer(&o).unwrap();
        let (status, by, at) = checked(&store);
        assert_eq!(status, OfferStatus::Confirmed);
        assert_eq!(by.as_deref(), Some("Sam"));
        assert!(at.is_some());
        // A different pack size: back to pending, nobody has checked it.
        o.unit_qty = Some(2.0);
        store.save_offer(&o).unwrap();
        assert_eq!(checked(&store), (OfferStatus::Pending, None, None));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn latest_reading_wins_and_old_ones_are_ignored() {
        let (store, dir) = store("readings");
        let id = store
            .save_offer(&offer("couches", "carrefour", "A1"))
            .unwrap();
        store
            .record_readings(&[reading(id, 1_000, 1_000), reading(id, 1_200, 2_000)])
            .unwrap();
        let got = store.latest_readings(0).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].price_cents, 1_200);
        assert_eq!(got[0].read_at, 2_000);
        assert!(store.latest_readings(3_000).unwrap().is_empty());
        // The older reading never comes back once the newest is the one kept.
        assert_eq!(store.latest_readings(1_500).unwrap().len(), 1);
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn readings_are_kept_per_offer_and_store() {
        let (store, dir) = store("readings-store");
        let a = store
            .save_offer(&offer("couches", "carrefour", "A1"))
            .unwrap();
        let b = store
            .save_offer(&offer("couches", "intermarche", "B1"))
            .unwrap();
        let mut elsewhere = reading(a, 1_100, 1_500);
        elsewhere.store = "Ville-B".into();
        elsewhere.promo = Some("2 pour 1".into());
        elsewhere.unit_price_cents = Some(25);
        elsewhere.available = false;
        store
            .record_readings(&[
                reading(a, 1_000, 1_000),
                elsewhere.clone(),
                reading(b, 900, 1_000),
            ])
            .unwrap();
        let got = store.latest_readings(0).unwrap();
        assert_eq!(got.len(), 3);
        assert!(got.contains(&elsewhere));
        assert!(got.contains(&reading(b, 900, 1_000)));
        // A reading of an unknown sheet, or with a negative price, is refused
        // and nothing of the batch is kept.
        assert!(matches!(
            store.record_readings(&[reading(a, 1, 9_000), reading(999, 1, 9_000)]),
            Err(PantryError::NotFound(_))
        ));
        assert!(matches!(
            store.record_readings(&[reading(a, -1, 9_000)]),
            Err(PantryError::Invalid(_))
        ));
        assert!(store.latest_readings(9_000).unwrap().is_empty());
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn provider_settings_list_every_chain_disabled_until_set() {
        let (store, dir) = store("providers");
        let all = store.provider_settings().unwrap();
        assert_eq!(all.len(), PROVIDERS.len());
        assert!(all.iter().all(|s| !s.enabled && s.store.is_empty()));
        assert!(all.iter().zip(PROVIDERS).all(|(s, p)| s.id == p.id));
        let set = ProviderSettings {
            id: "carrefour".into(),
            enabled: true,
            store: "Ville-A".into(),
            min_order_cents: 3_000,
            fee_cents: 499,
        };
        store.save_provider_settings(&set).unwrap();
        let all = store.provider_settings().unwrap();
        assert_eq!(all.iter().find(|s| s.id == "carrefour"), Some(&set));
        assert_eq!(all.iter().filter(|s| s.enabled).count(), 1);
        // Saving again replaces.
        let off = ProviderSettings {
            enabled: false,
            ..set
        };
        store.save_provider_settings(&off).unwrap();
        assert!(
            store
                .provider_settings()
                .unwrap()
                .iter()
                .all(|s| !s.enabled)
        );
        let negative = ProviderSettings {
            fee_cents: -1,
            ..off
        };
        assert!(matches!(
            store.save_provider_settings(&negative),
            Err(PantryError::Invalid(_))
        ));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_provider_is_refused() {
        let (store, dir) = store("unknown-provider");
        let s = ProviderSettings {
            id: "nulle-part".into(),
            enabled: true,
            store: "Ville-A".into(),
            min_order_cents: 0,
            fee_cents: 0,
        };
        assert!(matches!(
            store.save_provider_settings(&s),
            Err(PantryError::Invalid(_))
        ));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn threshold_defaults_to_thirty_percent() {
        let (store, dir) = store("threshold");
        assert!((store.split_threshold().unwrap() - 0.30).abs() < f64::EPSILON);
        assert!(matches!(
            store.set_split_threshold(0.95),
            Err(PantryError::Invalid(_))
        ));
        assert!(matches!(
            store.set_split_threshold(-0.1),
            Err(PantryError::Invalid(_))
        ));
        assert!(matches!(
            store.set_split_threshold(f64::NAN),
            Err(PantryError::Invalid(_))
        ));
        assert!((store.split_threshold().unwrap() - 0.30).abs() < f64::EPSILON);
        store.set_split_threshold(0.5).unwrap();
        store.set_split_threshold(0.0).unwrap();
        assert!(store.split_threshold().unwrap().abs() < f64::EPSILON);
        store.set_split_threshold(0.9).unwrap();
        let path = dir.join("pantry.db");
        drop(store);
        let again = Store::open(&path).unwrap();
        assert!((again.split_threshold().unwrap() - 0.9).abs() < f64::EPSILON);
        drop(again);
        let _ = std::fs::remove_dir_all(dir);
    }
}
