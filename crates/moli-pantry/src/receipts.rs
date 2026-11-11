//! Receipts: what was read on the photos of a till receipt, kept as an
//! editable draft until a human validates it. Validating turns the kept lines
//! into purchases, creates the products the draft asked for, remembers which
//! product each printed label stands for (so the next receipt of the chain is
//! pre-filled), and closes the shopping-list lines the receipt settles.
//!
//! The SQL lives here, as a third `impl Store` block. The photos themselves
//! are files, kept by [`crate::Pantry`]; the store only counts them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::baskets::{
    check_purchase, insert_lines, insert_purchase, json_col, json_text, line_cost,
};
use crate::model::{
    AgentReceipt, DraftLine, DriveTwin, Product, Purchase, Receipt, ReceiptDraft, ReceiptStatus,
};
use crate::store::{PantryError, Store, day_of, ms, save_product_in, ts};

/// A receipt is photographed in at most this many shots.
pub const MAX_PHOTOS: usize = 4;
/// The photos of a validated receipt are erased after 90 days (in ms).
const KEEP_PHOTOS_MS: u64 = 90 * 86_400_000;
const MINUTE_MS: u64 = 60_000;
const COLUMNS: &str = "id, status, photos, sent_at, sent_by, validated_at, validated_by, reader, \
                       draft, read, version";
/// A drive basket within this share of a receipt's total (1/50, 2 %) is
/// probably the same shopping.
const DRIVE_TWIN_SHARE: i64 = 50;

/// Letters with an accent (or a ligature) and what they fold to in an id.
fn fold(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => "a",
        'ç' => "c",
        'è' | 'é' | 'ê' | 'ë' => "e",
        'ì' | 'í' | 'î' | 'ï' => "i",
        'ñ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => "o",
        'ù' | 'ú' | 'û' | 'ü' => "u",
        'ý' | 'ÿ' => "y",
        'œ' => "oe",
        'æ' => "ae",
        'ß' => "ss",
        _ => return None,
    })
}

/// A product id from a name: accents folded, lowercase, every run of
/// non-alphanumerics a single `-`, none at either end.
pub(crate) fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if ('\u{300}'..='\u{36f}').contains(&c) {
            continue; // a combining accent: the letter before it already counts
        }
        let folded = fold(c);
        if folded.is_none() && !c.is_alphanumeric() {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push('-');
        }
        gap = false;
        match folded {
            Some(letters) => out.push_str(letters),
            None => out.push(c),
        }
    }
    out
}

/// A printed label as a key: spaces collapsed, case ignored.
pub(crate) fn label_key(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A text field that may be blank.
fn filled(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

fn check_draft(d: &ReceiptDraft) -> Result<(), PantryError> {
    for (n, line) in d.lines.iter().enumerate() {
        if filled(line.product.as_deref()).is_some()
            && filled(line.new_product.as_deref()).is_some()
        {
            return Err(PantryError::Invalid(format!(
                "line {} has both a product and a new product",
                n + 1
            )));
        }
    }
    Ok(())
}

/// Refuses a change made from `version` when the receipt moved on since.
fn check_version(receipt: &Receipt, version: Option<u64>) -> Result<(), PantryError> {
    match version {
        Some(v) if v != receipt.version => Err(PantryError::Conflict(format!(
            "receipt {} was changed elsewhere (version {}, not {v})",
            receipt.id, receipt.version
        ))),
        _ => Ok(()),
    }
}

fn receipt_row(r: &Row<'_>) -> rusqlite::Result<Receipt> {
    let status: String = r.get(1)?;
    let draft: ReceiptDraft = json_col(r, 8)?;
    let photos: i64 = r.get(2)?;
    let gap_cents = draft
        .total_cents
        .map(|t| t - draft.lines.iter().map(|l| l.line_cents).sum::<i64>());
    Ok(Receipt {
        id: r.get(0)?,
        status: ReceiptStatus::parse(&status).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                Type::Text,
                format!("unknown receipt status {status}").into(),
            )
        })?,
        photos: usize::try_from(photos).unwrap_or(0),
        sent_at: ms(r.get(3)?),
        sent_by: r.get(4)?,
        validated_at: r.get::<_, Option<i64>>(5)?.map(ms),
        validated_by: r.get(6)?,
        reader: r.get(7)?,
        draft,
        read: json_col(r, 9)?,
        gap_cents,
        duplicate_of: None,
        drive_twin: None,
        version: u64::try_from(r.get::<_, i64>(10)?).unwrap_or(0),
    })
}

/// The validated receipt (other than `except`) of the same chain, the same
/// minute and the same total, if any.
fn validated_twin(
    conn: &Connection,
    provider: &str,
    total_cents: i64,
    at: u64,
    except: i64,
) -> Result<Option<i64>, PantryError> {
    Ok(conn
        .query_row(
            "SELECT id FROM receipts
             WHERE status = 'validated' AND provider = ?1 AND total_cents = ?2
               AND at / ?3 = ?4 AND id <> ?5
             ORDER BY id LIMIT 1",
            params![
                provider,
                total_cents,
                ts(MINUTE_MS),
                ts(at / MINUTE_MS),
                except
            ],
            |r| r.get(0),
        )
        .optional()?)
}

/// The confirmed drive basket of `provider` collected on the house's day of
/// `at` whose lines cost about `total_cents` (2 % apart at most), the
/// closest one if several.
fn drive_twin(
    conn: &Connection,
    tz: &TimeZone,
    provider: &str,
    total_cents: i64,
    at: u64,
) -> Result<Option<DriveTwin>, PantryError> {
    let Some((start, end)) = day_of(tz, at) else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT basket, MIN(at), SUM(COALESCE(line_cents, 0)) FROM purchases
         WHERE source = 'drive' AND status = 'confirmed' AND basket IS NOT NULL
           AND provider = ?1 AND at >= ?2 AND at < ?3
         GROUP BY basket ORDER BY basket",
    )?;
    let baskets = stmt
        .query_map(params![provider, ts(start), ts(end)], |r| {
            Ok(DriveTwin {
                basket: r.get(0)?,
                at: ms(r.get(1)?),
                total_cents: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(baskets
        .into_iter()
        .filter(|b| (b.total_cents - total_cents).abs() * DRIVE_TWIN_SHARE <= total_cents.abs())
        .min_by_key(|b| (b.total_cents - total_cents).abs()))
}

/// A receipt, with what it probably repeats (looked for on a draft only):
/// a validated receipt of the same chain, minute and total, and a drive
/// basket of the same chain, day and about the same total.
fn load_receipt(conn: &Connection, tz: &TimeZone, id: i64) -> Result<Receipt, PantryError> {
    let mut receipt = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM receipts WHERE id = ?1"),
            [id],
            receipt_row,
        )
        .optional()?
        .ok_or_else(|| PantryError::NotFound(format!("receipt {id}")))?;
    let d = &receipt.draft;
    if let (ReceiptStatus::Draft, Some(at), Some(total)) = (receipt.status, d.at, d.total_cents) {
        receipt.duplicate_of = validated_twin(conn, &d.provider, total, at, id)?;
        receipt.drive_twin = drive_twin(conn, tz, &d.provider, total, at)?;
    }
    Ok(receipt)
}

/// The product a chain's printed label was last matched to, if it still exists.
fn linked_product(
    conn: &Connection,
    provider: &str,
    label: &str,
) -> Result<Option<String>, PantryError> {
    Ok(conn
        .query_row(
            "SELECT l.product FROM label_links l JOIN products p ON p.id = l.product
             WHERE l.provider = ?1 AND l.label = ?2",
            params![provider, label_key(label)],
            |r| r.get(0),
        )
        .optional()?)
}

fn product_exists(conn: &Connection, id: &str) -> Result<bool, PantryError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM products WHERE id = ?1)",
        [id],
        |r| r.get(0),
    )?)
}

/// The name of a product, if it exists.
fn product_name(conn: &Connection, id: &str) -> Result<Option<String>, PantryError> {
    Ok(conn
        .query_row("SELECT name FROM products WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?)
}

/// Remembers that a chain's printed label stands for `product`.
pub(crate) fn link_label(
    conn: &Connection,
    provider: &str,
    label: &str,
    product: &str,
) -> Result<(), PantryError> {
    if label.trim().is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO label_links(provider, label, product) VALUES (?1, ?2, ?3)
         ON CONFLICT(provider, label) DO UPDATE SET product = excluded.product",
        params![provider, label_key(label), product],
    )?;
    Ok(())
}

/// The existing product whose name is `name` once folded as an id (case,
/// accents and spacing ignored), if any.
fn product_named(conn: &Connection, name: &str) -> Result<Option<String>, PantryError> {
    let wanted = slug(name);
    let mut stmt = conn.prepare("SELECT id, name FROM products ORDER BY id")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let (id, known): (String, String) = (row.get(0)?, row.get(1)?);
        if slug(&known) == wanted {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// The product a kept line is bought as: the existing one it names, the
/// existing one named as its `new_product`, or a new one made from
/// `new_product` (one per distinct name in the receipt) under the first
/// free id.
fn resolve_product(
    tx: &Connection,
    n: usize,
    line: &DraftLine,
    created: &mut HashMap<String, String>,
) -> Result<String, PantryError> {
    match (
        filled(line.product.as_deref()),
        filled(line.new_product.as_deref()),
    ) {
        (Some(_), Some(_)) => Err(PantryError::Invalid(format!(
            "line {n} has both a product and a new product"
        ))),
        (None, None) => Err(PantryError::Invalid(format!("line {n} has no product"))),
        (Some(id), None) => {
            if product_exists(tx, id)? {
                Ok(id.to_owned())
            } else {
                Err(PantryError::NotFound(format!("product {id}")))
            }
        }
        (None, Some(name)) => {
            let base = slug(name);
            if base.is_empty() {
                return Err(PantryError::Invalid(format!(
                    "line {n}: a new product needs a name with letters or digits"
                )));
            }
            if let Some(id) = created.get(&base) {
                return Ok(id.clone());
            }
            if let Some(id) = product_named(tx, name)? {
                created.insert(base, id.clone());
                return Ok(id);
            }
            let mut id = base.clone();
            let mut suffix = 2;
            while product_exists(tx, &id)? {
                id = format!("{base}-{suffix}");
                suffix += 1;
            }
            save_product_in(
                tx,
                &Product {
                    id: id.clone(),
                    name: name.to_owned(),
                    aisle: line.aisle.clone(),
                    ..Product::default()
                },
            )?;
            created.insert(base, id.clone());
            Ok(id)
        }
    }
}

impl Store {
    /// Stores a freshly read receipt as a draft. A line whose printed label
    /// was matched to a product on an earlier receipt of the same chain gets
    /// that product, whatever the model guessed or proposed to create: a
    /// person chose it. `read` is the model's answer, already cut down to the schema's fields;
    /// `photos` how many pictures go with it.
    pub fn create_receipt(
        &self,
        draft: &ReceiptDraft,
        read: &serde_json::Value,
        reader: &str,
        photos: usize,
        by: &str,
        now: u64,
    ) -> Result<i64, PantryError> {
        check_draft(draft)?;
        let mut draft = draft.clone();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for line in &mut draft.lines {
            if let Some(product) = linked_product(&tx, &draft.provider, &line.label)? {
                line.product = Some(product);
                line.new_product = None;
            }
        }
        tx.execute(
            "INSERT INTO receipts(status, provider, store, at, total_cents, photos, sent_at,
                                  sent_by, reader, draft, read)
             VALUES ('draft', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                draft.provider,
                draft.store,
                draft.at.map(ts),
                draft.total_cents,
                i64::try_from(photos).unwrap_or(0),
                ts(now),
                by,
                reader,
                json_text(&draft)?,
                json_text(read)?
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        tracing::info!(receipt = id, by, "receipt sent");
        Ok(id)
    }

    pub fn receipt(&self, id: i64) -> Result<Receipt, PantryError> {
        load_receipt(&self.lock(), self.tz(), id)
    }

    /// The receipts, the drafts first, then the most recent.
    pub fn receipts(&self, status: Option<ReceiptStatus>) -> Result<Vec<Receipt>, PantryError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id FROM receipts WHERE ?1 IS NULL OR status = ?1
             ORDER BY status = 'draft' DESC, COALESCE(at, sent_at) DESC, id DESC",
        )?;
        let ids = stmt
            .query_map([status.map(ReceiptStatus::as_str)], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| load_receipt(&conn, self.tz(), id))
            .collect()
    }

    /// Replaces the draft of a receipt not yet validated. With the
    /// `version` the editor loaded, refused when someone saved meanwhile.
    pub fn save_draft(
        &self,
        id: i64,
        draft: &ReceiptDraft,
        version: Option<u64>,
    ) -> Result<Receipt, PantryError> {
        check_draft(draft)?;
        let conn = self.lock();
        let current = load_receipt(&conn, self.tz(), id)?;
        if current.status == ReceiptStatus::Validated {
            return Err(PantryError::Invalid(format!(
                "receipt {id} is validated: it cannot be edited"
            )));
        }
        check_version(&current, version)?;
        conn.execute(
            "UPDATE receipts SET provider = ?2, store = ?3, at = ?4, total_cents = ?5, draft = ?6,
                                 version = version + 1
             WHERE id = ?1",
            params![
                id,
                draft.provider,
                draft.store,
                draft.at.map(ts),
                draft.total_cents,
                json_text(draft)?
            ],
        )?;
        load_receipt(&conn, self.tz(), id)
    }

    /// Records the receipt, all or nothing: its kept lines become `ticket`
    /// purchases, the new products are created, the labels are remembered and
    /// the shopping-list lines in `items_done` that are still open are
    /// closed. A receipt that looks already recorded is refused unless
    /// `force`, one saved since the validator loaded `version` too; one
    /// already validated is returned as it is.
    pub fn validate_receipt(
        &self,
        id: i64,
        by: &str,
        now: u64,
        force: bool,
        version: Option<u64>,
    ) -> Result<Receipt, PantryError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let receipt = load_receipt(&tx, self.tz(), id)?;
        if receipt.status == ReceiptStatus::Validated {
            return Ok(receipt);
        }
        check_version(&receipt, version)?;
        if let (Some(twin), false) = (receipt.duplicate_of, force) {
            return Err(PantryError::Invalid(format!(
                "this receipt looks already recorded: {twin}"
            )));
        }
        let mut draft = receipt.draft;
        let mut created = HashMap::new();
        let (provider, store, at) = (draft.provider.clone(), draft.store.clone(), draft.at);
        for (n, line) in draft.lines.iter_mut().enumerate().filter(|(_, l)| l.keep) {
            let product = resolve_product(&tx, n + 1, line, &mut created)?;
            let purchase = Purchase {
                product: product.clone(),
                provider: provider.clone(),
                store: store.clone(),
                qty: line.qty,
                price_cents: line.price_cents,
                at: at.unwrap_or(now),
                source: "ticket".into(),
                status: "confirmed".into(),
                receipt: Some(id),
                label: line.label.clone(),
                line_cents: Some(line.line_cents),
                discount_cents: line.discount_cents,
                unit: line.unit.clone(),
                ..Purchase::default()
            };
            check_purchase(&purchase).map_err(|e| match e {
                PantryError::Invalid(why) => PantryError::Invalid(format!("line {}: {why}", n + 1)),
                other => other,
            })?;
            insert_purchase(&tx, &purchase, None)?;
            link_label(&tx, &provider, &line.label, &product)?;
            line.product = Some(product);
            line.new_product = None;
        }
        for item in &draft.items_done {
            tx.execute(
                "UPDATE items SET done_at = ?2
                 WHERE id = ?1 AND removed_at IS NULL AND done_at IS NULL",
                params![item, ts(now)],
            )?;
        }
        tx.execute(
            "UPDATE receipts SET status = 'validated', validated_at = ?2, validated_by = ?3,
                                 draft = ?4, version = version + 1
             WHERE id = ?1",
            params![id, ts(now), by, json_text(&draft)?],
        )?;
        let done = load_receipt(&tx, self.tz(), id)?;
        tx.commit()?;
        tracing::info!(receipt = id, by, "receipt validated");
        Ok(done)
    }

    /// Records a receipt an agent read itself, with its lines, in one go: a
    /// validated receipt without photos (reader `agent`, sent and validated
    /// by `by`) and its lines as confirmed `ticket` purchases attached to it.
    /// Every line must be a `ticket` line; the receipt of the same chain,
    /// minute and total already recorded is refused, so a retry cannot count
    /// a receipt twice. Returns the receipt.
    pub fn record_receipt(
        &self,
        header: &AgentReceipt,
        lines: &[Purchase],
        by: &str,
        now: u64,
    ) -> Result<Receipt, PantryError> {
        if lines.is_empty() {
            return Err(PantryError::Invalid(
                "a receipt needs at least one line".into(),
            ));
        }
        if let Some(n) = lines.iter().position(|l| l.source != "ticket") {
            return Err(PantryError::Invalid(format!(
                "line {}: a receipt only holds lines of source ticket",
                n + 1
            )));
        }
        if header.provider.trim().is_empty() {
            return Err(PantryError::Invalid("a receipt needs a provider".into()));
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if let Some(total) = header.total_cents
            && let Some(twin) = validated_twin(&tx, &header.provider, total, header.at, 0)?
        {
            return Err(PantryError::Invalid(format!(
                "this receipt looks already recorded: {twin}"
            )));
        }
        let mut draft_lines = Vec::with_capacity(lines.len());
        for l in lines {
            draft_lines.push(DraftLine {
                label: l.label.clone(),
                name: product_name(&tx, &l.product)?.unwrap_or_else(|| l.label.clone()),
                qty: l.qty,
                unit: l.unit.clone(),
                price_cents: l.price_cents,
                discount_cents: l.discount_cents,
                line_cents: line_cost(l),
                keep: true,
                product: Some(l.product.clone()),
                ..DraftLine::default()
            });
        }
        let draft = ReceiptDraft {
            provider: header.provider.clone(),
            store: header.store.clone(),
            at: Some(header.at),
            total_cents: header.total_cents,
            lines: draft_lines,
            ..ReceiptDraft::default()
        };
        tx.execute(
            "INSERT INTO receipts(status, provider, store, at, total_cents, photos, sent_at,
                                  sent_by, validated_at, validated_by, reader, draft, read)
             VALUES ('validated', ?1, ?2, ?3, ?4, 0, ?5, ?6, ?5, ?6, 'agent', ?7, 'null')",
            params![
                header.provider,
                header.store,
                ts(header.at),
                header.total_cents,
                ts(now),
                by,
                json_text(&draft)?
            ],
        )?;
        let id = tx.last_insert_rowid();
        let attached: Vec<Purchase> = lines
            .iter()
            .map(|l| Purchase {
                provider: header.provider.clone(),
                store: header.store.clone(),
                at: header.at,
                receipt: Some(id),
                status: "confirmed".into(),
                ..l.clone()
            })
            .collect();
        insert_lines(&tx, &attached)?;
        // As a validation does: remember what each printed label stands for.
        for l in lines {
            link_label(&tx, &header.provider, &l.label, &l.product)?;
        }
        let done = load_receipt(&tx, self.tz(), id)?;
        tx.commit()?;
        tracing::info!(receipt = id, by, "receipt recorded by an agent");
        Ok(done)
    }

    /// Deletes a draft; returns how many photos it had, for the caller to erase.
    pub fn abandon_receipt(&self, id: i64) -> Result<usize, PantryError> {
        let conn = self.lock();
        let receipt = load_receipt(&conn, self.tz(), id)?;
        if receipt.status == ReceiptStatus::Validated {
            return Err(PantryError::Invalid(format!(
                "receipt {id} is validated: it cannot be abandoned"
            )));
        }
        conn.execute("DELETE FROM receipts WHERE id = ?1", [id])?;
        Ok(receipt.photos)
    }

    /// The validated receipts whose photos are due to be erased: validated
    /// more than 90 days before `now`, and still holding some.
    pub fn photos_to_purge(&self, now: u64) -> Result<Vec<(i64, usize)>, PantryError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, photos FROM receipts
             WHERE status = 'validated' AND photos > 0 AND validated_at < ?1 ORDER BY id",
        )?;
        Ok(stmt
            .query_map([ts(now.saturating_sub(KEEP_PHOTOS_MS))], |r| {
                Ok((r.get(0)?, usize::try_from(r.get::<_, i64>(1)?).unwrap_or(0)))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Notes that the photos of a receipt were erased.
    pub fn forget_photos(&self, id: i64) -> Result<(), PantryError> {
        let n = self
            .lock()
            .execute("UPDATE receipts SET photos = 0 WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(PantryError::NotFound(format!("receipt {id}")));
        }
        Ok(())
    }
}

// ---- the photo files ---------------------------------------------------------

/// Where shot `n` (from 1) of a receipt lives in `dir`.
pub(crate) fn photo_file(dir: &Path, id: i64, n: usize) -> PathBuf {
    dir.join(format!("{id}-{n}.jpg"))
}

/// Erases every photo of a receipt (and any half-written one). A file that is
/// already missing counts as erased; any other failure is returned (the first
/// one) after the other files were tried.
pub(crate) fn remove_photos(dir: &Path, id: i64) -> std::io::Result<()> {
    let mut failure = None;
    for n in 1..=MAX_PHOTOS {
        let path = photo_file(dir, id, n);
        for file in [path.with_extension("jpg.tmp"), path] {
            match std::fs::remove_file(&file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    failure.get_or_insert(e);
                }
                _ => {}
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

/// Writes the photos of a receipt, each through a temporary file renamed once
/// complete. On failure nothing of the receipt is left behind.
pub(crate) fn write_photos(dir: &Path, id: i64, photos: &[Vec<u8>]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let written = photos.iter().enumerate().try_for_each(|(i, bytes)| {
        let path = photo_file(dir, id, i + 1);
        let tmp = path.with_extension("jpg.tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)
    });
    if written.is_err()
        && let Err(e) = remove_photos(dir, id)
    {
        tracing::warn!(receipt = id, error = %e, "cannot clean up the receipt photos");
    }
    written
}

/// Erases the photos that are due; returns how many receipts were cleared. A
/// receipt whose files could not all be erased keeps its photo count, so the
/// next purge tries again.
pub(crate) fn purge_old_photos(store: &Store, dir: &Path, now: u64) -> Result<usize, PantryError> {
    let mut purged = 0;
    for (id, _) in store.photos_to_purge(now)? {
        match remove_photos(dir, id) {
            Ok(()) => {
                store.forget_photos(id)?;
                purged += 1;
            }
            Err(e) => {
                tracing::warn!(receipt = id, error = %e, "cannot erase the receipt photos, will retry");
            }
        }
    }
    Ok(purged)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use moli_runtime::{Hub, HubOptions};
    use serde_json::json;

    use super::*;
    use crate::Pantry;
    use crate::model::{DraftLine, DriveTwin, Product, Purchase, ReceiptDraft, ReceiptStatus};

    const NOW: u64 = 1_000_000_000_000;
    const DAY: u64 = 86_400_000;
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 3_600_000;

    fn setup(name: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("moli-receipts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir.join("pantry.db")).unwrap();
        (store, dir)
    }

    fn cleanup(store: Store, dir: PathBuf) {
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn product(store: &Store, id: &str, name: &str) {
        store
            .save_product(&Product {
                id: id.into(),
                name: name.into(),
                ..Product::default()
            })
            .unwrap();
    }

    /// A kept line of `cents`, matched to `product` when given.
    fn line(label: &str, cents: i64, product: Option<&str>) -> DraftLine {
        DraftLine {
            label: label.into(),
            name: label.to_lowercase(),
            qty: 1.0,
            price_cents: cents,
            line_cents: cents,
            keep: true,
            product: product.map(str::to_owned),
            ..DraftLine::default()
        }
    }

    fn draft(lines: Vec<DraftLine>) -> ReceiptDraft {
        ReceiptDraft {
            provider: "enseigne-x".into(),
            store: "Ville-A".into(),
            at: Some(NOW),
            total_cents: None,
            lines,
            unreadable: false,
            items_done: Vec::new(),
        }
    }

    fn create(store: &Store, d: &ReceiptDraft) -> i64 {
        store
            .create_receipt(d, &json!({"lines": []}), "model-x", 1, "Sam", NOW)
            .unwrap()
    }

    fn all_purchases(store: &Store) -> Vec<Purchase> {
        store.purchases(0, u64::MAX / 4).unwrap()
    }

    #[test]
    fn a_validated_draft_becomes_purchases() {
        let (store, dir) = setup("validate");
        product(&store, "p1", "Lait");
        product(&store, "p2", "Pain");
        let mut bag = line("SAC", 5, None);
        bag.keep = false;
        let id = create(
            &store,
            &draft(vec![
                line("LAIT 1L", 120, Some("p1")),
                bag,
                line("PAIN", 250, Some("p2")),
            ]),
        );
        assert_eq!(store.receipt(id).unwrap().status, ReceiptStatus::Draft);
        assert!(
            all_purchases(&store).is_empty(),
            "a draft is not a purchase"
        );

        let done = store
            .validate_receipt(id, "Élodie", NOW + 5, false, None)
            .unwrap();
        assert_eq!(done.status, ReceiptStatus::Validated);
        assert_eq!(done.validated_by.as_deref(), Some("Élodie"));
        assert_eq!(done.validated_at, Some(NOW + 5));
        let bought = all_purchases(&store);
        assert_eq!(bought.len(), 2);
        assert_eq!(bought[0].product, "p1");
        assert_eq!(bought[0].label, "LAIT 1L");
        assert_eq!(bought[0].line_cents, Some(120));
        assert_eq!(bought[1].label, "PAIN");
        for p in &bought {
            assert_eq!(p.source, "ticket");
            assert_eq!(p.status, "confirmed");
            assert_eq!(p.receipt, Some(id));
            assert_eq!(p.provider, "enseigne-x");
            assert_eq!(p.store, "Ville-A");
            assert_eq!(p.at, NOW);
        }
        cleanup(store, dir);
    }

    /// A `ticket` line as an agent sends it.
    fn ticket(product: &str, label: &str, cents: i64) -> Purchase {
        Purchase {
            product: product.into(),
            provider: "enseigne-x".into(),
            store: "Ville-A".into(),
            qty: 1.0,
            price_cents: cents,
            at: NOW,
            source: "ticket".into(),
            status: "confirmed".into(),
            label: label.into(),
            ..Purchase::default()
        }
    }

    fn header(total: Option<i64>) -> AgentReceipt {
        AgentReceipt {
            provider: "enseigne-x".into(),
            store: "Ville-A".into(),
            at: NOW,
            total_cents: total,
        }
    }

    #[test]
    fn an_agent_receipt_is_validated_with_its_lines() {
        let (store, dir) = setup("agent-receipt");
        let mut milk = ticket("p1", "LAIT", 130);
        milk.discount_cents = 10;
        milk.line_cents = Some(120);
        let lines = [milk, ticket("p2", "PAIN", 250)];
        let done = store
            .record_receipt(&header(Some(370)), &lines, "agent-x", NOW + 5)
            .unwrap();
        assert_eq!(done.status, ReceiptStatus::Validated);
        assert_eq!(done.photos, 0);
        assert_eq!(done.reader, "agent");
        assert_eq!(done.sent_by, "agent-x");
        assert_eq!(done.validated_by.as_deref(), Some("agent-x"));
        assert_eq!(done.validated_at, Some(NOW + 5));
        assert_eq!(done.gap_cents, Some(0), "120 + 250 = 370");
        assert_eq!(done.draft.lines.len(), 2);
        assert_eq!(store.receipts(None).unwrap().len(), 1);
        let bought = all_purchases(&store);
        assert_eq!(bought.len(), 2);
        for p in &bought {
            assert_eq!(p.receipt, Some(done.id));
            assert_eq!(p.source, "ticket");
            assert_eq!(p.status, "confirmed");
        }
        assert_eq!(bought[0].line_cents, Some(120));
        assert_eq!(bought[0].discount_cents, 10);
        cleanup(store, dir);
    }

    #[test]
    fn an_agent_receipt_dates_its_lines_and_teaches_its_labels() {
        let (store, dir) = setup("agent-receipt-links");
        product(&store, "p1", "Céréales");
        // The lines say other things than the header: the header wins.
        let mut cereal = ticket("p1", "CHOCO CEREAL 430G", 300);
        cereal.provider = "enseigne-y".into();
        cereal.store = "Ville-Z".into();
        cereal.at = 7;
        let unknown = ticket("p9", "AUTRE", 100);
        let done = store
            .record_receipt(&header(None), &[cereal, unknown], "agent-x", NOW)
            .unwrap();
        assert_eq!(done.draft.lines[0].name, "Céréales", "the product's name");
        assert_eq!(
            done.draft.lines[1].name, "AUTRE",
            "else the label as printed"
        );
        for p in all_purchases(&store) {
            assert_eq!(p.provider, "enseigne-x");
            assert_eq!(p.store, "Ville-A");
            assert_eq!(p.at, NOW);
        }
        // A later photo reading of the chain is pre-filled.
        let mut later = draft(vec![line("choco  cereal 430g", 310, None)]);
        later.at = Some(NOW + DAY);
        let next = store.receipt(create(&store, &later)).unwrap().draft.lines;
        assert_eq!(next[0].product.as_deref(), Some("p1"));
        cleanup(store, dir);
    }

    #[test]
    fn an_agent_receipt_only_holds_ticket_lines_and_nothing_is_kept_on_error() {
        let (store, dir) = setup("agent-receipt-invalid");
        let mut drive = ticket("p1", "LAIT", 130);
        drive.source = "drive".into();
        let err = store
            .record_receipt(&header(None), &[ticket("p2", "PAIN", 250), drive], "a", NOW)
            .unwrap_err();
        assert!(matches!(err, PantryError::Invalid(_)), "{err}");
        let err = store
            .record_receipt(&header(None), &[], "a", NOW)
            .unwrap_err();
        assert!(matches!(err, PantryError::Invalid(_)), "{err}");
        // A bad line (no product) rolls the receipt back.
        let err = store
            .record_receipt(&header(None), &[ticket("", "X", 1)], "a", NOW)
            .unwrap_err();
        assert!(matches!(err, PantryError::Invalid(_)), "{err}");
        assert!(store.receipts(None).unwrap().is_empty());
        assert!(all_purchases(&store).is_empty());
        cleanup(store, dir);
    }

    #[test]
    fn an_agent_receipt_sent_twice_is_recorded_once() {
        let (store, dir) = setup("agent-receipt-twice");
        let lines = [ticket("p1", "LAIT", 120)];
        let first = store
            .record_receipt(&header(Some(120)), &lines, "a", NOW)
            .unwrap();
        let err = store
            .record_receipt(&header(Some(120)), &lines, "a", NOW + 1)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("this receipt looks already recorded: {}", first.id)
        );
        assert_eq!(all_purchases(&store).len(), 1);
        cleanup(store, dir);
    }

    #[test]
    fn a_reading_without_items_done_is_a_draft() {
        // What a model answers: every field but `items_done`.
        let reading = json!({
            "provider": "enseigne-x", "store": "Ville-A", "at": null, "total_cents": 120,
            "unreadable": false,
            "lines": [{
                "label": "LAIT", "name": "lait", "qty": 1.0, "unit": null, "price_cents": 120,
                "discount_cents": 0, "line_cents": 120, "vat": 5.5, "aisle": null,
                "keep": true, "reason": "", "product": null, "new_product": null
            }]
        });
        let d: ReceiptDraft = serde_json::from_value(reading).unwrap();
        assert!(d.items_done.is_empty());
        assert_eq!(d.lines[0].vat, Some(5.5));
    }

    #[test]
    fn a_ticket_without_a_date_is_dated_at_validation() {
        let (store, dir) = setup("no-date");
        product(&store, "p1", "Lait");
        let mut d = draft(vec![line("LAIT", 120, Some("p1"))]);
        d.at = None;
        let id = create(&store, &d);
        store
            .validate_receipt(id, "Sam", NOW + 7, false, None)
            .unwrap();
        assert_eq!(all_purchases(&store)[0].at, NOW + 7);
        cleanup(store, dir);
    }

    #[test]
    fn a_line_must_name_a_product_to_be_validated() {
        let (store, dir) = setup("no-product");
        product(&store, "p1", "Lait");
        let mut both = line("A", 100, Some("p1"));
        both.new_product = Some("Autre".into());
        assert!(
            store
                .create_receipt(&draft(vec![both]), &json!({}), "m", 0, "Sam", NOW)
                .is_err(),
            "a product and a new product are exclusive"
        );
        let id = create(
            &store,
            &draft(vec![line("LAIT", 120, Some("p1")), line("???", 100, None)]),
        );
        assert!(matches!(
            store.validate_receipt(id, "Sam", NOW, false, None),
            Err(PantryError::Invalid(_))
        ));
        let mut zero = line("ZERO", 100, Some("p1"));
        zero.qty = 0.0;
        let bad = create(&store, &draft(vec![line("LAIT", 120, Some("p1")), zero]));
        let err = store
            .validate_receipt(bad, "Sam", NOW, false, None)
            .unwrap_err();
        assert!(err.to_string().starts_with("line 2: "), "{err}");
        let unknown = create(&store, &draft(vec![line("X", 100, Some("nope"))]));
        assert!(
            store
                .validate_receipt(unknown, "Sam", NOW, false, None)
                .is_err()
        );
        assert!(
            all_purchases(&store).is_empty(),
            "all or nothing: nothing was recorded"
        );
        assert_eq!(store.receipt(id).unwrap().status, ReceiptStatus::Draft);
        cleanup(store, dir);
    }

    #[test]
    fn validating_twice_records_once() {
        let (store, dir) = setup("twice");
        product(&store, "p1", "Lait");
        let id = create(&store, &draft(vec![line("LAIT", 120, Some("p1"))]));
        let first = store.validate_receipt(id, "Sam", NOW, false, None).unwrap();
        let second = store
            .validate_receipt(id, "Élodie", NOW + DAY, false, None)
            .unwrap();
        assert_eq!(first, second, "the second call changes nothing");
        assert_eq!(all_purchases(&store).len(), 1);
        cleanup(store, dir);
    }

    #[test]
    fn a_new_product_is_created_with_a_free_id() {
        let (store, dir) = setup("new-product");
        // The id is taken, by a product of another name.
        product(&store, "cereales-chocolat", "Autre chose");
        let mut a = line("CHOCO CEREAL", 300, None);
        a.new_product = Some("Céréales chocolat".into());
        a.aisle = Some("petit-dejeuner".into());
        let mut b = line("CHOCO CEREAL 2", 300, None);
        b.new_product = Some("Céréales chocolat".into());
        let id = create(&store, &draft(vec![a, b]));
        let done = store.validate_receipt(id, "Sam", NOW, false, None).unwrap();

        let made = store
            .products()
            .unwrap()
            .into_iter()
            .find(|p| p.id == "cereales-chocolat-2")
            .expect("the new product takes the next free id");
        assert_eq!(made.name, "Céréales chocolat");
        assert_eq!(made.aisle.as_deref(), Some("petit-dejeuner"));
        assert_eq!(
            store.products().unwrap().len(),
            2,
            "one product for both lines"
        );
        assert!(all_purchases(&store).iter().all(|p| p.product == made.id));
        assert!(
            done.draft
                .lines
                .iter()
                .all(|l| l.product.as_deref() == Some("cereales-chocolat-2")
                    && l.new_product.is_none()),
            "the validated draft shows what was done"
        );
        cleanup(store, dir);
    }

    #[test]
    fn ids_are_made_of_folded_lowercase_words() {
        assert_eq!(slug("Céréales chocolat"), "cereales-chocolat");
        assert_eq!(slug("  Œufs bio (x6) !  "), "oeufs-bio-x6");
        assert_eq!(
            slug("Crème fraîche, À Ça Ôtez ùn"),
            "creme-fraiche-a-ca-otez-un"
        );
        assert_eq!(
            slug("cre\u{300}me"),
            "creme",
            "a decomposed accent is folded too"
        );
        assert_eq!(slug("???"), "");
    }

    #[test]
    fn a_label_link_fills_the_next_draft() {
        let (store, dir) = setup("link");
        product(&store, "p1", "Céréales");
        let id = create(
            &store,
            &draft(vec![line("CHOCO CEREAL 430G", 300, Some("p1"))]),
        );
        store.validate_receipt(id, "Sam", NOW, false, None).unwrap();

        let mut later = draft(vec![
            line("CHOCO CEREAL 430G", 310, None),
            line("choco  cereal 430g", 310, None),
            line("AUTRE", 100, None),
        ]);
        later.at = Some(NOW + DAY);
        let next = store.receipt(create(&store, &later)).unwrap().draft.lines;
        assert_eq!(next[0].product.as_deref(), Some("p1"));
        assert_eq!(
            next[1].product.as_deref(),
            Some("p1"),
            "case and spaces do not matter"
        );
        assert_eq!(next[2].product, None);

        let mut other = draft(vec![line("CHOCO CEREAL 430G", 310, None)]);
        other.provider = "enseigne-y".into();
        let other = store.receipt(create(&store, &other)).unwrap();
        assert_eq!(other.draft.lines[0].product, None, "links are per chain");
        cleanup(store, dir);
    }

    /// A line as the reader tidies it when the model knows no product for
    /// it: no product, a product to create named after it.
    fn fresh(label: &str, cents: i64, name: &str) -> DraftLine {
        DraftLine {
            new_product: Some(name.into()),
            ..line(label, cents, None)
        }
    }

    #[test]
    fn a_label_a_person_matched_fills_the_next_reading() {
        let (store, dir) = setup("link-reading");
        product(&store, "p1", "Céréales");
        // Ticket 1, as read: a product to create. A person points it to p1.
        let first = create(
            &store,
            &draft(vec![fresh("CHOCO CEREAL 430G", 300, "Choco")]),
        );
        let mut corrected = store.receipt(first).unwrap().draft;
        corrected.lines[0].product = Some("p1".into());
        corrected.lines[0].new_product = None;
        store.save_draft(first, &corrected, None).unwrap();
        store
            .validate_receipt(first, "Sam", NOW, false, None)
            .unwrap();

        // Ticket 2, as read: the same label, again a product to create, and
        // a line the model guessed wrong. The person's choice wins.
        let mut guessed = line("choco cereal 430g", 310, Some("p2"));
        guessed.name = "autre".into();
        product(&store, "p2", "Autre");
        let mut later = draft(vec![
            fresh("CHOCO CEREAL 430G", 310, "Choco"),
            guessed,
            fresh("INCONNU", 100, "Inconnu"),
        ]);
        later.at = Some(NOW + DAY);
        let next = store.receipt(create(&store, &later)).unwrap().draft.lines;
        for l in &next[..2] {
            assert_eq!(l.product.as_deref(), Some("p1"), "{l:?}");
            assert_eq!(l.new_product, None, "{l:?}");
        }
        assert_eq!(next[2].product, None, "an unknown label is left as read");
        assert_eq!(next[2].new_product.as_deref(), Some("Inconnu"));
        cleanup(store, dir);
    }

    #[test]
    fn a_new_product_named_as_an_existing_one_is_that_one() {
        let (store, dir) = setup("same-name");
        product(&store, "cereales-chocolat", "Céréales chocolat");
        let id = create(
            &store,
            &draft(vec![fresh("CHOCO", 300, "  CEREALES   Chocolat ")]),
        );
        let done = store.validate_receipt(id, "Sam", NOW, false, None).unwrap();
        assert_eq!(store.products().unwrap().len(), 1, "no copy is made");
        assert_eq!(
            done.draft.lines[0].product.as_deref(),
            Some("cereales-chocolat")
        );
        assert_eq!(all_purchases(&store)[0].product, "cereales-chocolat");
        cleanup(store, dir);
    }

    #[test]
    fn correcting_a_purchase_teaches_its_label() {
        let (store, dir) = setup("correct-link");
        product(&store, "p1", "Céréales");
        product(&store, "p2", "Muesli");
        let id = create(&store, &draft(vec![line("MUESLI 500G", 300, Some("p1"))]));
        store.validate_receipt(id, "Sam", NOW, false, None).unwrap();
        let bought = all_purchases(&store)[0].id.unwrap();
        store.update_purchase(bought, 1.0, 300, None, "p2").unwrap();

        let mut later = draft(vec![fresh("MUESLI 500G", 300, "Muesli")]);
        later.at = Some(NOW + DAY);
        let next = store.receipt(create(&store, &later)).unwrap().draft.lines;
        assert_eq!(next[0].product.as_deref(), Some("p2"));
        cleanup(store, dir);
    }

    #[test]
    fn an_edit_from_a_stale_page_is_refused() {
        let (store, dir) = setup("version");
        product(&store, "p1", "Lait");
        let id = create(&store, &draft(vec![line("LAIT", 120, Some("p1"))]));
        let loaded = store.receipt(id).unwrap().version;
        // Two adults opened the draft; the first one saves.
        let mut first = draft(vec![line("LAIT", 130, Some("p1"))]);
        first.store = "Ville-B".into();
        let saved = store.save_draft(id, &first, Some(loaded)).unwrap();
        assert_ne!(saved.version, loaded, "a save moves the version on");
        // The second one saves or validates what they loaded: refused.
        let err = store
            .save_draft(id, &draft(Vec::new()), Some(loaded))
            .unwrap_err();
        assert!(matches!(err, PantryError::Conflict(_)), "{err}");
        let err = store
            .validate_receipt(id, "Sam", NOW, false, Some(loaded))
            .unwrap_err();
        assert!(matches!(err, PantryError::Conflict(_)), "{err}");
        assert!(all_purchases(&store).is_empty());
        assert_eq!(store.receipt(id).unwrap().draft, first, "nothing lost");
        // Keeping their version: sent again with the new one.
        let kept = store
            .save_draft(
                id,
                &draft(vec![line("LAIT", 140, Some("p1"))]),
                Some(saved.version),
            )
            .unwrap();
        // Without a version (an older page, a script), no check.
        let unchecked = store.save_draft(id, &kept.draft, None).unwrap();
        let done = store
            .validate_receipt(id, "Sam", NOW, false, Some(unchecked.version))
            .unwrap();
        assert_eq!(done.status, ReceiptStatus::Validated);
        // A validation sent again (a lost answer) is not a conflict.
        let again = store
            .validate_receipt(id, "Sam", NOW, false, Some(unchecked.version))
            .unwrap();
        assert_eq!(again, done);
        assert_eq!(all_purchases(&store).len(), 1);
        cleanup(store, dir);
    }

    #[test]
    fn a_receipts_table_without_versions_is_migrated() {
        let (store, dir) = setup("version-migrate");
        let path = dir.join("pantry.db");
        drop(store);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "DROP TABLE receipts;
                 CREATE TABLE receipts (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     status TEXT NOT NULL, provider TEXT NOT NULL, store TEXT NOT NULL,
                     at INTEGER, total_cents INTEGER, photos INTEGER NOT NULL DEFAULT 0,
                     sent_at INTEGER NOT NULL, sent_by TEXT NOT NULL, validated_at INTEGER,
                     validated_by TEXT, reader TEXT NOT NULL DEFAULT '',
                     draft TEXT NOT NULL, read TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO receipts(status, provider, store, sent_at, sent_by, draft, read)
                 VALUES ('draft', 'enseigne-x', 'Ville-A', 1, 'Sam', ?1, 'null')",
                [serde_json::to_string(&draft(Vec::new())).unwrap()],
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let old = store.receipts(None).unwrap();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].version, 1);
        let saved = store.save_draft(old[0].id, &old[0].draft, Some(1)).unwrap();
        assert_eq!(saved.version, 2);
        cleanup(store, dir);
    }

    /// A drive basket of `cents`, collected at `at` from enseigne-x.
    fn collected(store: &Store, at: u64, cents: i64, status: &str) -> i64 {
        let basket = store.propose("Sam", vec![], at).unwrap().id;
        let mut bought = ticket("p1", "", cents);
        bought.source = "drive".into();
        bought.status = status.into();
        bought.basket = Some(basket);
        bought.at = at;
        store.record_purchases(&[bought]).unwrap();
        basket
    }

    #[test]
    fn a_drive_basket_of_the_same_day_and_total_is_pointed_out() {
        let (store, dir) = setup("drive-twin");
        product(&store, "p1", "Lait");
        let basket = collected(&store, NOW - HOUR, 10_000, "confirmed");
        let mut paper = draft(vec![line("LAIT", 10_150, Some("p1"))]);
        paper.total_cents = Some(10_150);
        let id = create(&store, &paper);
        let twin = store.receipt(id).unwrap().drive_twin.expect("1.5 % apart");
        assert_eq!(
            twin,
            DriveTwin {
                basket,
                at: NOW - HOUR,
                total_cents: 10_000
            }
        );
        // A warning only: the receipt is validated all the same.
        store.validate_receipt(id, "Sam", NOW, false, None).unwrap();
        assert_eq!(store.receipt(id).unwrap().drive_twin, None, "drafts only");

        for change in [
            |d: &mut ReceiptDraft| d.total_cents = Some(10_300),
            |d: &mut ReceiptDraft| d.total_cents = None,
            |d: &mut ReceiptDraft| d.at = Some(NOW + DAY),
            |d: &mut ReceiptDraft| d.provider = "enseigne-y".into(),
        ] {
            let mut d = paper.clone();
            change(&mut d);
            let r = store.receipt(create(&store, &d)).unwrap();
            assert_eq!(r.drive_twin, None, "{:?}", r.draft);
        }
        cleanup(store, dir);
    }

    #[test]
    fn a_drive_basket_still_provisional_is_not_counted_yet() {
        let (store, dir) = setup("drive-provisional");
        product(&store, "p1", "Lait");
        collected(&store, NOW - HOUR, 10_000, "provisional");
        let mut paper = draft(Vec::new());
        paper.total_cents = Some(10_000);
        assert_eq!(
            store.receipt(create(&store, &paper)).unwrap().drive_twin,
            None
        );
        cleanup(store, dir);
    }

    #[test]
    fn the_same_day_is_the_houses_day() {
        let dir = std::env::temp_dir().join(format!("moli-receipts-day-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paris = jiff::tz::TimeZone::get("Europe/Paris").unwrap();
        let store = Store::open_in(&dir.join("pantry.db"), paris).unwrap();
        product(&store, "p1", "Lait");
        // NOW is 01:46 UTC (03:46 in Paris); two hours before is the day
        // before in UTC, the same day in Paris.
        collected(&store, NOW - 2 * HOUR, 5_000, "confirmed");
        let mut paper = draft(Vec::new());
        paper.total_cents = Some(5_000);
        assert!(
            store
                .receipt(create(&store, &paper))
                .unwrap()
                .drive_twin
                .is_some()
        );
        cleanup(store, dir.clone());

        let (store, dir) = setup("day-utc");
        product(&store, "p1", "Lait");
        collected(&store, NOW - 2 * HOUR, 5_000, "confirmed");
        assert_eq!(
            store.receipt(create(&store, &paper)).unwrap().drive_twin,
            None
        );
        cleanup(store, dir);
    }

    #[test]
    fn a_duplicate_needs_force() {
        let (store, dir) = setup("duplicate");
        product(&store, "p1", "Lait");
        let mut first = draft(vec![line("LAIT", 120, Some("p1"))]);
        first.total_cents = Some(120);
        let a = create(&store, &first);
        let mut again = first.clone();
        again.at = Some(NOW + 5_000);
        let b = create(&store, &again);
        assert_eq!(
            store.receipt(b).unwrap().duplicate_of,
            None,
            "a is still a draft"
        );

        store.validate_receipt(a, "Sam", NOW, false, None).unwrap();
        assert_eq!(store.receipt(b).unwrap().duplicate_of, Some(a));
        let err = store
            .validate_receipt(b, "Sam", NOW, false, None)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("this receipt looks already recorded: {a}")
        );
        assert_eq!(all_purchases(&store).len(), 1);
        store.validate_receipt(b, "Sam", NOW, true, None).unwrap();
        assert_eq!(all_purchases(&store).len(), 2);

        // Another minute, another total or another chain is another receipt.
        for change in [
            |d: &mut ReceiptDraft| d.at = Some(NOW + 2 * MINUTE),
            |d: &mut ReceiptDraft| d.total_cents = Some(121),
            |d: &mut ReceiptDraft| d.provider = "enseigne-y".into(),
            |d: &mut ReceiptDraft| d.total_cents = None,
        ] {
            let mut d = first.clone();
            change(&mut d);
            assert_eq!(
                store.receipt(create(&store, &d)).unwrap().duplicate_of,
                None
            );
        }
        cleanup(store, dir);
    }

    #[test]
    fn the_gap_is_total_minus_lines() {
        let (store, dir) = setup("gap");
        let mut skipped = line("SAC", 250, None);
        skipped.keep = false;
        let mut d = draft(vec![line("A", 400, None), skipped, line("B", 300, None)]);
        assert_eq!(store.receipt(create(&store, &d)).unwrap().gap_cents, None);
        d.total_cents = Some(1000);
        let r = store.receipt(create(&store, &d)).unwrap();
        assert_eq!(r.gap_cents, Some(50), "every line counts, kept or not");
        d.lines.pop();
        assert_eq!(
            store.save_draft(r.id, &d, None).unwrap().gap_cents,
            Some(350)
        );
        cleanup(store, dir);
    }

    #[test]
    fn items_done_are_closed() {
        let (store, dir) = setup("items");
        product(&store, "p1", "Lait");
        let milk = store
            .add_item("lait", Some("p1"), 1.0, "Sam", "ui")
            .unwrap()
            .id;
        let eggs = store.add_item("oeufs", None, 1.0, "Sam", "ui").unwrap().id;
        let gone = store.add_item("sel", None, 1.0, "Sam", "ui").unwrap().id;
        let kept = store.add_item("riz", None, 1.0, "Sam", "ui").unwrap().id;
        store.remove_item(gone, "Sam").unwrap();
        let mut d = draft(vec![line("LAIT", 120, Some("p1"))]);
        d.items_done = vec![milk, eggs, gone, 9_999];
        let id = create(&store, &d);
        store.validate_receipt(id, "Sam", NOW, false, None).unwrap();
        let open: Vec<i64> = store.open_items().unwrap().iter().map(|i| i.id).collect();
        assert_eq!(open, vec![kept]);
        let done_at: Option<i64> = store
            .lock()
            .query_row("SELECT done_at FROM items WHERE id = ?1", [gone], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(done_at, None, "a removed line is not bought");
        cleanup(store, dir);
    }

    #[test]
    fn a_validated_receipt_cannot_be_edited_or_abandoned() {
        let (store, dir) = setup("frozen");
        product(&store, "p1", "Lait");
        let d = draft(vec![line("LAIT", 120, Some("p1"))]);
        let id = create(&store, &d);
        store.validate_receipt(id, "Sam", NOW, false, None).unwrap();
        assert!(matches!(
            store.save_draft(id, &draft(Vec::new()), None),
            Err(PantryError::Invalid(_))
        ));
        assert!(matches!(
            store.abandon_receipt(id),
            Err(PantryError::Invalid(_))
        ));
        assert_eq!(store.receipt(id).unwrap().draft.lines.len(), 1);
        cleanup(store, dir);
    }

    #[test]
    fn a_draft_can_be_edited_and_abandoned() {
        let (store, dir) = setup("abandon");
        let id = store
            .create_receipt(&draft(Vec::new()), &json!({}), "m", 3, "Sam", NOW)
            .unwrap();
        let mut d = draft(vec![line("A", 100, None)]);
        d.store = "Ville-B".into();
        d.total_cents = Some(100);
        let saved = store.save_draft(id, &d, None).unwrap();
        assert_eq!(saved.draft, d);
        assert_eq!(saved.photos, 3);
        assert_eq!(store.abandon_receipt(id).unwrap(), 3);
        assert!(matches!(store.receipt(id), Err(PantryError::NotFound(_))));
        assert!(matches!(
            store.abandon_receipt(id),
            Err(PantryError::NotFound(_))
        ));
        cleanup(store, dir);
    }

    #[test]
    fn drafts_come_first_then_the_newest() {
        let (store, dir) = setup("order");
        product(&store, "p1", "Lait");
        let mut ids = Vec::new();
        for day in 0..3 {
            let mut d = draft(vec![line("LAIT", 100, Some("p1"))]);
            d.at = Some(NOW + day * DAY);
            ids.push(create(&store, &d));
        }
        store
            .validate_receipt(ids[2], "Sam", NOW, false, None)
            .unwrap();
        let listed: Vec<i64> = store.receipts(None).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(listed, vec![ids[1], ids[0], ids[2]]);
        let validated = store.receipts(Some(ReceiptStatus::Validated)).unwrap();
        assert_eq!(validated.len(), 1);
        assert_eq!(store.receipts(Some(ReceiptStatus::Draft)).unwrap().len(), 2);
        cleanup(store, dir);
    }

    #[test]
    fn photos_to_purge_are_the_old_validated_ones() {
        let (store, dir) = setup("purge-list");
        product(&store, "p1", "Lait");
        let mk = |at: u64| {
            let mut d = draft(vec![line("LAIT", 100, Some("p1"))]);
            d.at = Some(at);
            create(&store, &d)
        };
        let old = mk(NOW);
        let recent = mk(NOW + 1);
        let draft_only = mk(NOW + 2);
        store
            .validate_receipt(old, "Sam", NOW - 91 * DAY, false, None)
            .unwrap();
        store
            .validate_receipt(recent, "Sam", NOW - 89 * DAY, false, None)
            .unwrap();
        assert_eq!(store.photos_to_purge(NOW).unwrap(), vec![(old, 1)]);
        store.forget_photos(old).unwrap();
        assert!(store.photos_to_purge(NOW).unwrap().is_empty());
        assert_eq!(store.receipt(old).unwrap().photos, 0);
        assert_eq!(store.receipt(draft_only).unwrap().photos, 1);
        cleanup(store, dir);
    }

    // ---- the Pantry handle: photos on disk ---------------------------------

    fn pantry(name: &str) -> (Pantry, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("moli-receipts-p-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hub = Hub::new(HubOptions::default()).unwrap();
        (Pantry::open(&hub, &dir).unwrap(), dir)
    }

    fn jpeg(n: usize) -> Vec<Vec<u8>> {
        (0..n)
            .map(|i| vec![0xFF, 0xD8, u8::try_from(i).unwrap()])
            .collect()
    }

    async fn add(
        pantry: &Pantry,
        photos: Vec<Vec<u8>>,
        d: ReceiptDraft,
    ) -> Result<Receipt, PantryError> {
        pantry
            .add_receipt(photos, d, json!({}), "model-x".into(), "Sam".into())
            .await
    }

    async fn milk(pantry: &Pantry) {
        pantry
            .save_product(&Product {
                id: "p1".into(),
                name: "Lait".into(),
                ..Product::default()
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn photos_are_written_next_to_the_ticket() {
        let (pantry, dir) = pantry("write");
        let r = pantry
            .add_receipt(
                jpeg(2),
                draft(Vec::new()),
                json!({"a": 1}),
                "model-x".into(),
                "Sam".into(),
            )
            .await
            .unwrap();
        assert_eq!((r.photos, r.read.clone()), (2, json!({"a": 1})));
        assert_eq!(pantry.receipts_dir(), dir.join("receipts"));
        for n in 1..=2 {
            let path = pantry.photo_path(r.id, n);
            assert_eq!(path, dir.join("receipts").join(format!("{}-{n}.jpg", r.id)));
            assert_eq!(
                std::fs::read(path).unwrap(),
                vec![0xFF, 0xD8, u8::try_from(n - 1).unwrap()]
            );
        }
        let names: Vec<_> = std::fs::read_dir(pantry.receipts_dir()).unwrap().collect();
        assert_eq!(names.len(), 2, "no temporary file is left");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn more_than_four_photos_are_refused() {
        let (pantry, dir) = pantry("max");
        for n in [0, MAX_PHOTOS + 1] {
            let err = add(&pantry, jpeg(n), draft(Vec::new())).await;
            assert!(matches!(err, Err(PantryError::Invalid(_))), "{n} photos");
        }
        assert!(pantry.receipts(None).await.unwrap().is_empty());
        add(&pantry, jpeg(MAX_PHOTOS), draft(Vec::new()))
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_failed_photo_write_removes_the_ticket() {
        let (pantry, dir) = pantry("write-fails");
        std::fs::write(pantry.receipts_dir(), b"not a folder").unwrap();
        assert!(add(&pantry, jpeg(1), draft(Vec::new())).await.is_err());
        assert!(pantry.receipts(None).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn abandon_removes_the_photo_files() {
        let (pantry, dir) = pantry("abandon");
        let r = add(&pantry, jpeg(3), draft(Vec::new())).await.unwrap();
        assert!(pantry.photo_path(r.id, 3).exists());
        pantry.abandon_receipt(r.id).await.unwrap();
        assert!((1..=3).all(|n| !pantry.photo_path(r.id, n).exists()));
        assert!(pantry.receipt(r.id).await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn old_photos_are_purged() {
        let (pantry, dir) = pantry("purge");
        milk(&pantry).await;
        let d = draft(vec![line("LAIT", 100, Some("p1"))]);
        let r = add(&pantry, jpeg(2), d).await.unwrap();
        let validated = pantry
            .validate_receipt(r.id, "Sam", NOW - 91 * DAY, false, None)
            .await
            .unwrap();
        assert_eq!(validated.photos, 2, "not old yet when it was validated");
        assert_eq!(pantry.purge_photos(NOW).await.unwrap(), 1);
        assert!((1..=2).all(|n| !pantry.photo_path(r.id, n).exists()));
        assert_eq!(pantry.receipt(r.id).await.unwrap().photos, 0);
        assert_eq!(all_purchases(&pantry.store).len(), 1, "the purchases stay");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn photos_that_cannot_be_erased_are_kept_for_the_next_purge() {
        let (pantry, dir) = pantry("purge-retry");
        milk(&pantry).await;
        let d = draft(vec![line("LAIT", 100, Some("p1"))]);
        let r = add(&pantry, jpeg(1), d).await.unwrap();
        pantry
            .validate_receipt(r.id, "Sam", NOW - 91 * DAY, false, None)
            .await
            .unwrap();
        // A non-empty folder where the photo should be: removing it as a file fails.
        let path = pantry.photo_path(r.id, 1);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("stuck"), b"x").unwrap();

        assert_eq!(pantry.purge_photos(NOW).await.unwrap(), 0);
        assert_eq!(pantry.receipt(r.id).await.unwrap().photos, 1, "still due");
        assert_eq!(pantry.photos_to_purge(NOW).await.unwrap(), vec![(r.id, 1)]);

        std::fs::remove_dir_all(&path).unwrap();
        assert_eq!(pantry.purge_photos(NOW).await.unwrap(), 1);
        assert_eq!(pantry.receipt(r.id).await.unwrap().photos, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn photos_are_purged_when_the_pantry_opens() {
        let (pantry, dir) = pantry("purge-open");
        milk(&pantry).await;
        let d = draft(vec![line("LAIT", 100, Some("p1"))]);
        let r = add(&pantry, jpeg(1), d).await.unwrap();
        pantry
            .validate_receipt(r.id, "Sam", 1_000, false, None)
            .await
            .unwrap();
        drop(pantry);
        let reopened = Pantry::open(&Hub::new(HubOptions::default()).unwrap(), &dir).unwrap();
        assert!(!reopened.photo_path(r.id, 1).exists());
        assert_eq!(reopened.receipt(r.id).await.unwrap().photos, 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
