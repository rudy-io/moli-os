//! The life of a basket: proposed from the open list and the fresh prices,
//! chosen by a human, ordered, collected and settled. Also the deviations the
//! household accepts or refuses, and the purchases made.
//!
//! The SQL lives here, as a second `impl Store` block, to keep `store.rs` small.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::de::DeserializeOwned;

use crate::basket::{Choice, Line, Terms, compute};
use crate::model::{
    Basket, BasketStatus, Deviation, Item, Offer, OfferStatus, Product, ProviderKind,
    ProviderSettings, Purchase, Reading,
};
use crate::providers::PROVIDERS;
use crate::store::{NOT_IN_A_CART, PantryError, Store, confirm_offer_in, ms, now_ms, ts};

/// A price older than this (12 h, in ms) is "unknown", never used.
const FRESH_MS: u64 = 43_200_000;
/// A deviation refused this many times for a product is not proposed again.
const REFUSAL_LIMIT: i64 = 2;
const BASKET_COLUMNS: &str =
    "id, created_at, created_by, status, options, deviations, chosen, accepted, chosen_by";
const PURCHASE_COLUMNS: &str = "product, provider, store, qty, price_cents, at, source, basket,      status, id, receipt, label, line_cents, discount_cents, unit";
const SOURCES: [&str; 2] = ["drive", "ticket"];
const PURCHASE_STATUSES: [&str; 2] = ["provisional", "confirmed"];

/// The sheet bought for an item in a store, as priced in a basket's options.
type Used = (i64, String, i64);

pub(crate) fn json_col<T: DeserializeOwned>(r: &Row<'_>, idx: usize) -> rusqlite::Result<T> {
    let text: String = r.get(idx)?;
    serde_json::from_str(&text)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e)))
}

pub(crate) fn json_text<T: serde::Serialize>(value: &T) -> Result<String, PantryError> {
    serde_json::to_string(value).map_err(|e| PantryError::Invalid(format!("cannot encode: {e}")))
}

fn basket_row(r: &Row<'_>) -> rusqlite::Result<Basket> {
    let status: String = r.get(3)?;
    Ok(Basket {
        id: r.get(0)?,
        created_at: ms(r.get(1)?),
        created_by: r.get(2)?,
        status: BasketStatus::parse(&status).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                Type::Text,
                format!("unknown basket status {status}").into(),
            )
        })?,
        options: json_col(r, 4)?,
        deviations: json_col(r, 5)?,
        chosen: r.get(6)?,
        accepted: json_col(r, 7)?,
        chosen_by: r.get(8)?,
    })
}

fn purchase_row(r: &Row<'_>) -> rusqlite::Result<Purchase> {
    Ok(Purchase {
        product: r.get(0)?,
        provider: r.get(1)?,
        store: r.get(2)?,
        qty: r.get(3)?,
        price_cents: r.get(4)?,
        at: ms(r.get(5)?),
        source: r.get(6)?,
        basket: r.get(7)?,
        status: r.get(8)?,
        id: r.get(9)?,
        receipt: r.get(10)?,
        label: r.get(11)?,
        line_cents: r.get(12)?,
        discount_cents: r.get(13)?,
        unit: r.get(14)?,
    })
}

fn load_purchase(conn: &Connection, id: i64) -> Result<Purchase, PantryError> {
    conn.query_row(
        &format!("SELECT {PURCHASE_COLUMNS} FROM purchases WHERE id = ?1"),
        [id],
        purchase_row,
    )
    .optional()?
    .ok_or_else(|| PantryError::NotFound(format!("purchase {id}")))
}

fn load_basket(conn: &Connection, id: i64) -> Result<Basket, PantryError> {
    conn.query_row(
        &format!("SELECT {BASKET_COLUMNS} FROM baskets WHERE id = ?1"),
        [id],
        basket_row,
    )
    .optional()?
    .ok_or_else(|| PantryError::NotFound(format!("basket {id}")))
}

/// Which moves a basket may make: forward one step at a time, or dropped from
/// anywhere but `Done`. `Chosen` is reached through [`Store::choose`] only.
fn allowed(from: BasketStatus, to: BasketStatus) -> bool {
    matches!(
        (from, to),
        (BasketStatus::Chosen, BasketStatus::Filled)
            | (BasketStatus::Filled, BasketStatus::Ordered)
            | (BasketStatus::Ordered, BasketStatus::Done)
    ) || (to == BasketStatus::Dropped && from != BasketStatus::Done)
}

/// The bought items leave the list, the basket is done.
fn finish_basket(conn: &Connection, id: i64) -> Result<(), PantryError> {
    conn.execute(
        "UPDATE items SET done_at = ?2 WHERE basket = ?1 AND done_at IS NULL AND removed_at IS NULL",
        params![id, ts(now_ms())],
    )?;
    conn.execute(
        "UPDATE baskets SET status = ?2 WHERE id = ?1",
        params![id, BasketStatus::Done.as_str()],
    )?;
    Ok(())
}

/// Closes a basket whose purchases are all confirmed (and not none), once it
/// has been chosen: a proposal, a drop or a finished basket is left alone.
fn close_if_settled(conn: &Connection, id: i64) -> Result<(), PantryError> {
    let status = load_basket(conn, id)?.status;
    if !matches!(
        status,
        BasketStatus::Chosen | BasketStatus::Filled | BasketStatus::Ordered
    ) {
        return Ok(());
    }
    let (lines, unsettled): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(status <> 'confirmed'), 0) FROM purchases WHERE basket = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if lines > 0 && unsettled == 0 {
        finish_basket(conn, id)?;
    }
    Ok(())
}

pub(crate) fn check_purchase(p: &Purchase) -> Result<(), PantryError> {
    if p.product.trim().is_empty() || p.provider.trim().is_empty() {
        return Err(PantryError::Invalid(
            "a purchase needs a product and a provider".into(),
        ));
    }
    if !p.qty.is_finite() || p.qty <= 0.0 || p.price_cents < 0 {
        return Err(PantryError::Invalid(
            "a purchase needs a quantity above zero and a price of zero or more".into(),
        ));
    }
    if !SOURCES.contains(&p.source.as_str()) {
        return Err(PantryError::Invalid(format!(
            "unknown purchase source: {}",
            p.source
        )));
    }
    if !PURCHASE_STATUSES.contains(&p.status.as_str()) {
        return Err(PantryError::Invalid(format!(
            "unknown purchase status: {}",
            p.status
        )));
    }
    if p.discount_cents < 0 {
        return Err(PantryError::Invalid(
            "a purchase discount cannot be negative".into(),
        ));
    }
    Ok(())
}

/// What a line cost: the stated amount, or `round(qty * price) - discount`.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn line_cost(p: &Purchase) -> i64 {
    p.line_cents
        .unwrap_or_else(|| (p.qty * p.price_cents as f64).round() as i64 - p.discount_cents)
}

pub(crate) fn insert_purchase(
    conn: &Connection,
    p: &Purchase,
    basket: Option<i64>,
) -> Result<(), PantryError> {
    conn.execute(
        "INSERT INTO purchases(product, provider, store, qty, price_cents, at, source, basket,
                               status, receipt, label, line_cents, discount_cents, unit)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            p.product,
            p.provider,
            p.store,
            p.qty,
            p.price_cents,
            ts(p.at),
            p.source,
            basket,
            p.status,
            p.receipt,
            p.label,
            line_cost(p),
            p.discount_cents,
            p.unit
        ],
    )?;
    Ok(())
}

/// The fresh prices of every sheet, to answer "what does this product cost in
/// this chain?".
struct Prices<'a> {
    offers: HashMap<(&'a str, &'a str), Vec<&'a Offer>>,
    readings: HashMap<i64, Vec<&'a Reading>>,
}

impl<'a> Prices<'a> {
    fn new(offers: &'a [Offer], readings: &'a [Reading]) -> Self {
        let mut by_key: HashMap<(&str, &str), Vec<&Offer>> = HashMap::new();
        for o in offers {
            by_key
                .entry((o.product.as_str(), o.provider.as_str()))
                .or_default()
                .push(o);
        }
        let mut by_offer: HashMap<i64, Vec<&Reading>> = HashMap::new();
        for r in readings {
            by_offer.entry(r.offer).or_default().push(r);
        }
        Self {
            offers: by_key,
            readings: by_offer,
        }
    }

    /// The reading that counts for a sheet in a chain: the one taken in the
    /// chain's configured store, or the most recent when none is configured.
    fn reading(&self, offer: i64, store: &str) -> Option<&'a Reading> {
        let all = self.readings.get(&offer)?;
        if store.is_empty() {
            all.iter().copied().max_by_key(|r| r.read_at)
        } else {
            all.iter().copied().find(|r| r.store == store)
        }
    }

    /// The sheet and unit price used for a product in a chain: the confirmed
    /// sheet; else, among the pending ones, the most recently read. `None` when
    /// that sheet has no fresh reading, or the product is unavailable.
    fn price(&self, product: &str, provider: &str, store: &str) -> Option<(i64, i64)> {
        let candidates = self.offers.get(&(product, provider))?;
        let (offer, reading) = if let Some(sure) = candidates
            .iter()
            .find(|o| o.status == OfferStatus::Confirmed)
        {
            (*sure, self.reading(sure.id, store)?)
        } else {
            candidates
                .iter()
                .filter_map(|o| self.reading(o.id, store).map(|r| (*o, r)))
                .max_by_key(|(o, r)| (r.read_at, o.id))?
        };
        reading.available.then_some((offer.id, reading.price_cents))
    }
}

/// One priced [`Line`] per open item, and which sheet priced each of them where.
fn price_lines(
    items: &[Item],
    products: &HashMap<String, Product>,
    drives: &[ProviderSettings],
    prices: &Prices<'_>,
) -> (Vec<Line>, Vec<Used>) {
    let mut used: Vec<Used> = Vec::new();
    let lines = items
        .iter()
        .map(|item| {
            let product = item.product.as_deref().and_then(|id| products.get(id));
            let mut line_prices = BTreeMap::new();
            if let Some(id) = item.product.as_deref() {
                for drive in drives {
                    let found = prices.price(id, &drive.id, &drive.store);
                    if let Some((offer, _)) = found {
                        used.push((item.id, drive.id.clone(), offer));
                    }
                    line_prices.insert(drive.id.clone(), found.map(|(_, price)| price));
                }
            }
            Line {
                item: item.id,
                product: item
                    .product
                    .as_deref()
                    .map(|id| product.map_or(id, |p| p.name.as_str()))
                    .unwrap_or_default()
                    .to_owned(),
                qty: item.qty,
                fixed_provider: product.and_then(|p| p.fixed_provider.clone()),
                prices: line_prices,
            }
        })
        .collect();
    (lines, used)
}

impl Store {
    /// Prices the open list (minus the lines already in a filled or ordered
    /// cart) with the fresh readings (taken at `now − 12 h` or
    /// later) of the enabled drives, and stores the resulting options. The
    /// `deviations` are the agent's suggestions, minus those the household
    /// already refused twice for the same product and kind. Every previous
    /// basket still `Proposed` or `Chosen` is dropped (its open items are freed);
    /// a `Filled` or `Ordered` one is left alone, its carts already exist.
    pub fn propose(
        &self,
        by: &str,
        deviations: Vec<Deviation>,
        now: u64,
    ) -> Result<Basket, PantryError> {
        let items = self.items_to_buy()?;
        let products: HashMap<String, Product> = self
            .products()?
            .into_iter()
            .map(|p| (p.id.clone(), p))
            .collect();
        let offers = self.offers(None)?;
        let readings = self.latest_readings(now.saturating_sub(FRESH_MS))?;
        let threshold = self.split_threshold()?;
        // Only the enabled drives are compared; an online chain is reached
        // through a product's fixed provider.
        let drives: Vec<_> = self
            .provider_settings()?
            .into_iter()
            .filter(|s| {
                s.enabled
                    && PROVIDERS
                        .iter()
                        .any(|p| p.id == s.id && p.kind == ProviderKind::Drive)
            })
            .collect();
        let terms: Vec<Terms> = drives
            .iter()
            .map(|s| Terms {
                provider: s.id.clone(),
                min_order_cents: s.min_order_cents,
                fee_cents: s.fee_cents,
            })
            .collect();

        let prices = Prices::new(&offers, &readings);
        let (lines, used) = price_lines(&items, &products, &drives, &prices);
        let options = compute(&lines, &terms, threshold);

        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let mut kept = Vec::with_capacity(deviations.len());
        for d in deviations {
            let refused: i64 = tx
                .query_row(
                    "SELECT count FROM refusals WHERE product = ?1 AND kind = ?2",
                    params![d.product, d.kind],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            if refused < REFUSAL_LIMIT {
                kept.push(d);
            }
        }
        let replaced = format!(
            "SELECT id FROM baskets WHERE status IN ('{}', '{}')",
            BasketStatus::Proposed.as_str(),
            BasketStatus::Chosen.as_str()
        );
        tx.execute(
            &format!(
                "UPDATE items SET basket = NULL WHERE done_at IS NULL AND basket IN ({replaced})"
            ),
            [],
        )?;
        tx.execute(
            &format!("UPDATE baskets SET status = ?1 WHERE id IN ({replaced})"),
            [BasketStatus::Dropped.as_str()],
        )?;
        tx.execute(
            "INSERT INTO baskets(created_at, created_by, status, options, deviations, used)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                ts(now),
                by,
                BasketStatus::Proposed.as_str(),
                json_text(&options)?,
                json_text(&kept)?,
                json_text(&used)?
            ],
        )?;
        let basket = load_basket(&tx, tx.last_insert_rowid())?;
        tx.commit()?;
        Ok(basket)
    }

    /// A human picks one of the options. The sheets it buys are confirmed (in
    /// the chooser's name), the items it covers are tied to the basket (except
    /// a line that meanwhile went into a filled or ordered cart), and
    /// each deviation not in `accepted` (indexes into `deviations`) counts one
    /// refusal for its product and kind.
    pub fn choose(
        &self,
        basket: i64,
        option: &str,
        accepted: &[usize],
        by: &str,
    ) -> Result<Basket, PantryError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let current = load_basket(&tx, basket)?;
        if current.status != BasketStatus::Proposed {
            return Err(PantryError::Invalid(format!(
                "basket {basket} is already {}",
                current.status.as_str()
            )));
        }
        let choice: &Choice = current
            .options
            .singles
            .iter()
            .chain(current.options.split.iter())
            .find(|c| c.key == option)
            .ok_or_else(|| PantryError::NotFound(format!("option {option}")))?;
        if !choice.valid {
            return Err(PantryError::Invalid(format!(
                "option {option} does not reach a minimum order"
            )));
        }
        if accepted.iter().any(|i| *i >= current.deviations.len()) {
            return Err(PantryError::Invalid("unknown deviation index".into()));
        }
        let accepted: Vec<usize> = accepted
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let used: Vec<Used> =
            tx.query_row("SELECT used FROM baskets WHERE id = ?1", [basket], |r| {
                json_col(r, 0)
            })?;
        let wanted: BTreeSet<i64> = used
            .iter()
            .filter(|(item, provider, _)| {
                choice
                    .groups
                    .get(provider)
                    .is_some_and(|items| items.contains(item))
            })
            .map(|(_, _, offer)| *offer)
            .collect();
        for offer in wanted {
            confirm_offer_in(&tx, offer, by)?;
        }
        for item in choice.groups.values().flatten() {
            tx.execute(
                &format!("UPDATE items SET basket = ?1 WHERE id = ?2 AND {NOT_IN_A_CART}"),
                params![basket, item],
            )?;
        }
        // One refusal per distinct (product, kind) and call.
        let mut counted: BTreeSet<(&str, &str)> = BTreeSet::new();
        for (i, d) in current.deviations.iter().enumerate() {
            if !accepted.contains(&i) && counted.insert((d.product.as_str(), d.kind.as_str())) {
                tx.execute(
                    "INSERT INTO refusals(product, kind, count) VALUES (?1, ?2, 1)
                     ON CONFLICT(product, kind) DO UPDATE SET count = count + 1",
                    params![d.product, d.kind],
                )?;
            }
        }
        tx.execute(
            "UPDATE baskets SET status = ?2, chosen = ?3, accepted = ?4, chosen_by = ?5,
                 chosen_at = ?6 WHERE id = ?1",
            params![
                basket,
                BasketStatus::Chosen.as_str(),
                option,
                json_text(&accepted)?,
                by,
                ts(now_ms())
            ],
        )?;
        let chosen = load_basket(&tx, basket)?;
        tx.commit()?;
        Ok(chosen)
    }

    /// Moves a basket along its life: `Chosen → Filled → Ordered → Done`, or
    /// `Dropped` from anywhere but `Done`. Done closes the items it bought;
    /// dropped frees them.
    pub fn set_status(
        &self,
        basket: i64,
        status: BasketStatus,
        by: &str,
    ) -> Result<Basket, PantryError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let current = load_basket(&tx, basket)?;
        if !allowed(current.status, status) {
            return Err(PantryError::Invalid(format!(
                "a {} basket cannot become {}",
                current.status.as_str(),
                status.as_str()
            )));
        }
        match status {
            BasketStatus::Done => finish_basket(&tx, basket)?,
            BasketStatus::Dropped => {
                tx.execute(
                    "UPDATE baskets SET status = ?2 WHERE id = ?1",
                    params![basket, status.as_str()],
                )?;
                tx.execute(
                    "UPDATE items SET basket = NULL WHERE basket = ?1 AND done_at IS NULL",
                    [basket],
                )?;
            }
            _ => {
                tx.execute(
                    "UPDATE baskets SET status = ?2 WHERE id = ?1",
                    params![basket, status.as_str()],
                )?;
            }
        }
        tracing::info!(
            basket,
            status = status.as_str(),
            by,
            "basket status changed"
        );
        let updated = load_basket(&tx, basket)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Stores purchases, all or none. When a line names a basket and all the
    /// lines of that basket are now `confirmed`, the basket is done and its
    /// items leave the list.
    pub fn record_purchases(&self, p: &[Purchase]) -> Result<(), PantryError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for line in p {
            check_purchase(line)?;
            if let Some(basket) = line.basket {
                load_basket(&tx, basket)?;
            }
            insert_purchase(&tx, line, line.basket)?;
        }
        let baskets: BTreeSet<i64> = p.iter().filter_map(|l| l.basket).collect();
        for basket in baskets {
            close_if_settled(&tx, basket)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// At the pick-up: the drive lines of a basket (what was ordered, and any
    /// correction already made) are replaced by the corrected ones (what was
    /// really bought), `confirmed` drive lines. Calling it again with the same
    /// lines changes nothing; a receipt (`ticket`) line of the basket is left
    /// alone.
    pub fn confirm_purchases(
        &self,
        basket: i64,
        corrected: &[Purchase],
    ) -> Result<(), PantryError> {
        if corrected.is_empty() {
            return Err(PantryError::Invalid(
                "the corrected purchases cannot be empty".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if load_basket(&tx, basket)?.status == BasketStatus::Dropped {
            return Err(PantryError::Invalid(format!("basket {basket} was dropped")));
        }
        tx.execute(
            "DELETE FROM purchases WHERE basket = ?1 AND source = 'drive'",
            [basket],
        )?;
        for line in corrected {
            // What the drive delivered: a drive line, whatever was sent, so a
            // retry replaces it instead of adding to it.
            let line = Purchase {
                source: "drive".into(),
                status: "confirmed".into(),
                ..line.clone()
            };
            check_purchase(&line)?;
            insert_purchase(&tx, &line, Some(basket))?;
        }
        close_if_settled(&tx, basket)?;
        tx.commit()?;
        Ok(())
    }

    /// Purchases made between `from` and `to` (ms, both included), oldest first.
    pub fn purchases(&self, from: u64, to: u64) -> Result<Vec<Purchase>, PantryError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PURCHASE_COLUMNS} FROM purchases WHERE at BETWEEN ?1 AND ?2 ORDER BY at, id"
        ))?;
        Ok(stmt
            .query_map([ts(from), ts(to)], purchase_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Corrects a purchase from the purchases page: quantity, unit price,
    /// cost of the line (recomputed from the quantity, the price and the
    /// recorded discount when `None`) and product.
    pub fn update_purchase(
        &self,
        id: i64,
        qty: f64,
        price_cents: i64,
        line_cents: Option<i64>,
        product: &str,
    ) -> Result<Purchase, PantryError> {
        let conn = self.lock();
        let mut line = load_purchase(&conn, id)?;
        line.qty = qty;
        line.price_cents = price_cents;
        line.line_cents = line_cents;
        product.trim().clone_into(&mut line.product);
        check_purchase(&line)?;
        let known: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM products WHERE id = ?1)",
            [&line.product],
            |r| r.get(0),
        )?;
        if !known {
            return Err(PantryError::NotFound(format!("product {}", line.product)));
        }
        conn.execute(
            "UPDATE purchases SET qty = ?2, price_cents = ?3, line_cents = ?4, product = ?5
             WHERE id = ?1",
            params![id, qty, price_cents, line_cost(&line), line.product],
        )?;
        load_purchase(&conn, id)
    }

    /// Removes a purchase recorded by mistake.
    pub fn delete_purchase(&self, id: i64) -> Result<(), PantryError> {
        let n = self
            .lock()
            .execute("DELETE FROM purchases WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(PantryError::NotFound(format!("purchase {id}")));
        }
        Ok(())
    }

    /// The latest basket that is neither done nor dropped.
    pub fn open_basket(&self) -> Result<Option<Basket>, PantryError> {
        Ok(self
            .lock()
            .query_row(
                &format!(
                    "SELECT {BASKET_COLUMNS} FROM baskets
                     WHERE status NOT IN ('done', 'dropped') ORDER BY id DESC LIMIT 1"
                ),
                [],
                basket_row,
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Offer, OfferStatus, Product, ProviderSettings, Reading};

    const NOW: u64 = 1_000_000_000_000;
    const HOUR: u64 = 3_600_000;

    /// Two enabled drives, each tied to its own store, no minimum, no fee.
    fn setup(name: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("moli-baskets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir.join("pantry.db")).unwrap();
        for (id, site) in [("carrefour", "Ville-A"), ("intermarche", "Ville-B")] {
            store
                .save_provider_settings(&ProviderSettings {
                    id: id.into(),
                    enabled: true,
                    store: site.into(),
                    min_order_cents: 0,
                    fee_cents: 0,
                })
                .unwrap();
        }
        (store, dir)
    }

    fn product(store: &Store, id: &str) {
        store
            .save_product(&Product {
                id: id.into(),
                name: format!("Produit {id}"),
                ..Product::default()
            })
            .unwrap();
    }

    /// A sheet with one reading of `price` taken `age` ms before `NOW`, in `site`.
    fn priced(
        store: &Store,
        product: &str,
        provider: &str,
        reference: &str,
        site: &str,
        price: i64,
        age: u64,
    ) -> i64 {
        let id = store
            .save_offer(&Offer {
                product: product.into(),
                provider: provider.into(),
                reference: reference.into(),
                ..Offer::default()
            })
            .unwrap();
        store
            .record_readings(&[Reading {
                offer: id,
                store: site.into(),
                price_cents: price,
                unit_price_cents: None,
                promo: None,
                available: true,
                read_at: NOW - age,
                read_by: "Élodie".into(),
            }])
            .unwrap();
        id
    }

    fn status_of(store: &Store, offer: i64) -> OfferStatus {
        store
            .offers(None)
            .unwrap()
            .into_iter()
            .find(|o| o.id == offer)
            .unwrap()
            .status
    }

    fn deviation(product: &str, kind: &str) -> Deviation {
        Deviation {
            product: product.into(),
            kind: kind.into(),
            text: "texte".into(),
            gain_cents: 50,
        }
    }

    fn purchase(product: &str, basket: Option<i64>, status: &str, price: i64) -> Purchase {
        Purchase {
            product: product.into(),
            provider: "carrefour".into(),
            store: "Ville-A".into(),
            qty: 2.0,
            price_cents: price,
            at: NOW,
            source: "drive".into(),
            basket,
            status: status.into(),
            ..Purchase::default()
        }
    }

    /// A line of the `purchase` helper (quantity 2) as read back: the row id
    /// and the computed cost are filled.
    fn stored(p: &Purchase, id: i64) -> Purchase {
        Purchase {
            id: Some(id),
            line_cents: Some(2 * p.price_cents - p.discount_cents),
            ..p.clone()
        }
    }

    fn cleanup(store: Store, dir: PathBuf) {
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn propose_uses_only_fresh_prices() {
        let (store, dir) = setup("fresh");
        for p in ["vieux", "limite", "frais"] {
            product(&store, p);
        }
        priced(
            &store,
            "vieux",
            "carrefour",
            "V1",
            "Ville-A",
            100,
            13 * HOUR,
        );
        // Exactly twelve hours old still counts.
        priced(
            &store,
            "limite",
            "carrefour",
            "L1",
            "Ville-A",
            200,
            12 * HOUR,
        );
        priced(&store, "frais", "carrefour", "F1", "Ville-A", 300, HOUR);
        let old = store
            .add_item("vieux", Some("vieux"), 1.0, "Sam", "ui")
            .unwrap();
        let edge = store
            .add_item("limite", Some("limite"), 1.0, "Sam", "ui")
            .unwrap();
        let fresh = store
            .add_item("frais", Some("frais"), 2.0, "Sam", "ui")
            .unwrap();

        let basket = store.propose("Élodie", vec![], NOW).unwrap();
        assert_eq!(basket.status, BasketStatus::Proposed);
        assert_eq!(basket.created_by, "Élodie");
        assert_eq!(basket.created_at, NOW);
        assert_eq!(basket.options.nowhere, vec![old.id]);
        let single = basket
            .options
            .singles
            .iter()
            .find(|c| c.key == "carrefour")
            .unwrap();
        assert_eq!(single.groups["carrefour"], vec![edge.id, fresh.id]);
        assert_eq!(single.total_cents, 200 + 2 * 300);
        cleanup(store, dir);
    }

    #[test]
    fn items_without_price_or_product_end_nowhere() {
        let (store, dir) = setup("nowhere");
        product(&store, "lait");
        // Sold, but not available today: no price.
        let id = priced(&store, "lait", "carrefour", "L1", "Ville-A", 100, HOUR);
        store
            .record_readings(&[Reading {
                offer: id,
                store: "Ville-A".into(),
                price_cents: 100,
                unit_price_cents: None,
                promo: None,
                available: false,
                read_at: NOW,
                read_by: "Élodie".into(),
            }])
            .unwrap();
        let milk = store
            .add_item("lait", Some("lait"), 1.0, "Sam", "ui")
            .unwrap();
        let free = store.add_item("bougies", None, 1.0, "Sam", "ui").unwrap();
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        assert_eq!(basket.options.nowhere, vec![milk.id, free.id]);
        cleanup(store, dir);
    }

    #[test]
    fn a_reading_counts_only_in_the_configured_store() {
        let (store, dir) = setup("store");
        product(&store, "lait");
        // The price seen in another town does not apply to the configured drive.
        priced(&store, "lait", "carrefour", "L1", "Ville-B", 90, HOUR);
        let item = store
            .add_item("lait", Some("lait"), 1.0, "Sam", "ui")
            .unwrap();
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        assert_eq!(basket.options.nowhere, vec![item.id]);

        // With no configured store, the most recent reading of the sheet counts.
        store
            .save_provider_settings(&ProviderSettings {
                id: "carrefour".into(),
                enabled: true,
                store: String::new(),
                min_order_cents: 0,
                fee_cents: 0,
            })
            .unwrap();
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        assert!(basket.options.nowhere.is_empty());
        cleanup(store, dir);
    }

    #[test]
    fn a_confirmed_sheet_beats_a_pending_one() {
        let (store, dir) = setup("confirmed-first");
        product(&store, "lait");
        let sure = priced(&store, "lait", "carrefour", "L1", "Ville-A", 300, 2 * HOUR);
        priced(&store, "lait", "carrefour", "L2", "Ville-A", 100, HOUR);
        store.confirm_offer(sure, "Élodie").unwrap();
        let item = store
            .add_item("lait", Some("lait"), 1.0, "Sam", "ui")
            .unwrap();
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        let single = &basket.options.singles[0];
        assert_eq!(single.key, "carrefour");
        assert_eq!(single.groups["carrefour"], vec![item.id]);
        assert_eq!(single.total_cents, 300);
        cleanup(store, dir);
    }

    #[test]
    fn among_pending_sheets_the_most_recently_read_wins() {
        let (store, dir) = setup("pending-recent");
        product(&store, "lait");
        priced(&store, "lait", "carrefour", "L1", "Ville-A", 300, 2 * HOUR);
        priced(&store, "lait", "carrefour", "L2", "Ville-A", 100, HOUR);
        store
            .add_item("lait", Some("lait"), 1.0, "Sam", "ui")
            .unwrap();
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        assert_eq!(basket.options.singles[0].total_cents, 100);
        cleanup(store, dir);
    }

    #[test]
    fn only_drives_are_compared_and_pinned_online_items_go_elsewhere() {
        let (store, dir) = setup("elsewhere");
        store
            .save_provider_settings(&ProviderSettings {
                id: "amazon".into(),
                enabled: true,
                store: String::new(),
                min_order_cents: 0,
                fee_cents: 0,
            })
            .unwrap();
        store
            .save_product(&Product {
                id: "capsules".into(),
                name: "Capsules".into(),
                fixed_provider: Some("amazon".into()),
                ..Product::default()
            })
            .unwrap();
        priced(&store, "capsules", "amazon", "C1", "", 500, HOUR);
        let item = store
            .add_item("capsules", Some("capsules"), 1.0, "Sam", "ui")
            .unwrap();
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        assert_eq!(basket.options.elsewhere, vec![item.id]);
        assert!(basket.options.singles.iter().all(|c| c.key != "amazon"));
        assert_eq!(basket.options.singles.len(), 2);
        cleanup(store, dir);
    }

    #[test]
    fn a_new_proposal_drops_the_previous_one() {
        let (store, dir) = setup("drops");
        let first = store.propose("Sam", vec![], NOW).unwrap();
        let second = store.propose("Sam", vec![], NOW + 1).unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(store.open_basket().unwrap().map(|b| b.id), Some(second.id));
        let status: String = store
            .lock()
            .query_row(
                "SELECT status FROM baskets WHERE id = ?1",
                [first.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "dropped");
        cleanup(store, dir);
    }

    /// A list of one product sold in both drives, carrefour cheaper.
    fn two_drives(name: &str) -> (Store, PathBuf, i64, i64, i64) {
        let (store, dir) = setup(name);
        product(&store, "lait");
        let a = priced(&store, "lait", "carrefour", "L1", "Ville-A", 100, HOUR);
        let b = priced(&store, "lait", "intermarche", "L2", "Ville-B", 200, HOUR);
        let item = store
            .add_item("lait", Some("lait"), 1.0, "Sam", "ui")
            .unwrap();
        (store, dir, a, b, item.id)
    }

    #[test]
    fn choosing_confirms_offers() {
        let (store, dir, a, b, item) = two_drives("choose");
        assert_eq!(status_of(&store, a), OfferStatus::Pending);
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        let chosen = store.choose(basket.id, "carrefour", &[], "Élodie").unwrap();
        assert_eq!(chosen.status, BasketStatus::Chosen);
        assert_eq!(chosen.chosen.as_deref(), Some("carrefour"));
        assert_eq!(chosen.chosen_by.as_deref(), Some("Élodie"));
        // Only the sheet the option buys is confirmed, by the chooser.
        assert_eq!(status_of(&store, a), OfferStatus::Confirmed);
        assert_eq!(status_of(&store, b), OfferStatus::Pending);
        let by: String = store
            .lock()
            .query_row("SELECT confirmed_by FROM offers WHERE id = ?1", [a], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(by, "Élodie");
        // The bought items are tied to the basket.
        let tied: Option<i64> = store
            .lock()
            .query_row("SELECT basket FROM items WHERE id = ?1", [item], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(tied, Some(basket.id));
        assert_eq!(store.open_basket().unwrap().unwrap().id, basket.id);
        cleanup(store, dir);
    }

    #[test]
    fn choose_refuses_what_it_cannot_do() {
        let (store, dir, _, _, _) = two_drives("choose-errors");
        let basket = store
            .propose("Sam", vec![deviation("lait", "own_brand")], NOW)
            .unwrap();
        assert!(matches!(
            store.choose(basket.id, "nulle-part", &[], "Sam"),
            Err(PantryError::NotFound(_))
        ));
        assert!(matches!(
            store.choose(basket.id, "split", &[], "Sam"),
            Err(PantryError::NotFound(_))
        ));
        assert!(matches!(
            store.choose(basket.id, "carrefour", &[1], "Sam"),
            Err(PantryError::Invalid(_))
        ));
        assert!(matches!(
            store.choose(999, "carrefour", &[], "Sam"),
            Err(PantryError::NotFound(_))
        ));
        store.choose(basket.id, "carrefour", &[0], "Sam").unwrap();
        // A basket already chosen cannot be chosen again.
        assert!(matches!(
            store.choose(basket.id, "intermarche", &[], "Sam"),
            Err(PantryError::Invalid(_))
        ));
        cleanup(store, dir);
    }

    #[test]
    fn an_option_under_the_minimum_order_cannot_be_chosen() {
        let (store, dir, _, _, _) = two_drives("minimum");
        store
            .save_provider_settings(&ProviderSettings {
                id: "carrefour".into(),
                enabled: true,
                store: "Ville-A".into(),
                min_order_cents: 5_000,
                fee_cents: 0,
            })
            .unwrap();
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        assert!(matches!(
            store.choose(basket.id, "carrefour", &[], "Sam"),
            Err(PantryError::Invalid(_))
        ));
        cleanup(store, dir);
    }

    #[test]
    fn deviation_refused_twice_is_not_proposed_again() {
        let (store, dir, _, _, _) = two_drives("refusals");
        let both = || vec![deviation("x", "own_brand"), deviation("x", "bigger_size")];
        for round in 1..=2 {
            let basket = store.propose("Sam", both(), NOW).unwrap();
            assert_eq!(basket.deviations.len(), 2, "round {round}");
            // The bigger size is accepted, the own brand is not.
            store.choose(basket.id, "carrefour", &[1], "Sam").unwrap();
        }
        let third = store.propose("Sam", both(), NOW).unwrap();
        assert_eq!(third.deviations, vec![deviation("x", "bigger_size")]);
        // Another product is unaffected.
        let other = store
            .propose("Sam", vec![deviation("y", "own_brand")], NOW)
            .unwrap();
        assert_eq!(other.deviations.len(), 1);
        cleanup(store, dir);
    }

    #[test]
    fn invalid_transition_is_refused() {
        let (store, dir, _, _, _) = two_drives("transitions");
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        for target in [
            BasketStatus::Ordered,
            BasketStatus::Filled,
            BasketStatus::Done,
            BasketStatus::Chosen,
            BasketStatus::Proposed,
        ] {
            assert!(
                matches!(
                    store.set_status(basket.id, target, "Sam"),
                    Err(PantryError::Invalid(_))
                ),
                "{target:?}"
            );
        }
        store.choose(basket.id, "carrefour", &[], "Sam").unwrap();
        assert!(matches!(
            store.set_status(basket.id, BasketStatus::Ordered, "Sam"),
            Err(PantryError::Invalid(_))
        ));
        for next in [
            BasketStatus::Filled,
            BasketStatus::Ordered,
            BasketStatus::Done,
        ] {
            assert_eq!(
                store.set_status(basket.id, next, "Sam").unwrap().status,
                next
            );
        }
        // Nothing leaves Done, not even by dropping.
        assert!(matches!(
            store.set_status(basket.id, BasketStatus::Dropped, "Sam"),
            Err(PantryError::Invalid(_))
        ));
        assert!(matches!(
            store.set_status(999, BasketStatus::Dropped, "Sam"),
            Err(PantryError::NotFound(_))
        ));
        assert!(store.open_basket().unwrap().is_none());
        // Done by hand also closes the bought items.
        assert!(store.open_items().unwrap().is_empty());
        cleanup(store, dir);
    }

    #[test]
    fn dropping_a_chosen_basket_frees_its_items() {
        let (store, dir, _, _, item) = two_drives("drop-chosen");
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        store.choose(basket.id, "carrefour", &[], "Sam").unwrap();
        let dropped = store
            .set_status(basket.id, BasketStatus::Dropped, "Sam")
            .unwrap();
        assert_eq!(dropped.status, BasketStatus::Dropped);
        let tied: Option<i64> = store
            .lock()
            .query_row("SELECT basket FROM items WHERE id = ?1", [item], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(tied, None);
        assert_eq!(store.open_items().unwrap().len(), 1);
        assert!(store.open_basket().unwrap().is_none());
        cleanup(store, dir);
    }

    #[test]
    fn confirmed_purchases_close_the_basket() {
        let (store, dir, _, _, _) = two_drives("purchases");
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        store.choose(basket.id, "carrefour", &[], "Sam").unwrap();
        store
            .set_status(basket.id, BasketStatus::Filled, "Sam")
            .unwrap();
        store
            .set_status(basket.id, BasketStatus::Ordered, "Sam")
            .unwrap();
        store
            .record_purchases(&[purchase("lait", Some(basket.id), "provisional", 100)])
            .unwrap();
        // Still waiting for the pick-up.
        assert_eq!(
            store.open_basket().unwrap().unwrap().status,
            BasketStatus::Ordered
        );
        assert_eq!(store.open_items().unwrap().len(), 1);

        // At the counter the price differed: the corrected line replaces it.
        store
            .confirm_purchases(basket.id, &[purchase("lait", None, "provisional", 95)])
            .unwrap();
        assert!(store.open_basket().unwrap().is_none());
        assert!(store.open_items().unwrap().is_empty());
        let done = store.purchases(0, u64::MAX / 4).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].status, "confirmed");
        assert_eq!(done[0].price_cents, 95);
        assert_eq!(done[0].basket, Some(basket.id));
        let status: String = store
            .lock()
            .query_row(
                "SELECT status FROM baskets WHERE id = ?1",
                [basket.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "done");
        cleanup(store, dir);
    }

    #[test]
    fn recording_confirmed_lines_closes_the_basket_only_when_all_are() {
        let (store, dir, _, _, _) = two_drives("ticket");
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        store.choose(basket.id, "carrefour", &[], "Sam").unwrap();
        store
            .record_purchases(&[
                purchase("lait", Some(basket.id), "provisional", 100),
                purchase("lait", Some(basket.id), "confirmed", 100),
            ])
            .unwrap();
        // One line is still provisional: not closed.
        assert!(store.open_basket().unwrap().is_some());
        // A purchase tied to no basket never closes one.
        store
            .record_purchases(&[purchase("pain", None, "confirmed", 120)])
            .unwrap();
        assert!(store.open_basket().unwrap().is_some());
        // A proposed basket is not closed by a purchase either.
        let (store2, dir2, _, _, _) = two_drives("ticket-proposed");
        let proposed = store2.propose("Sam", vec![], NOW).unwrap();
        store2
            .record_purchases(&[purchase("lait", Some(proposed.id), "confirmed", 100)])
            .unwrap();
        assert_eq!(
            store2.open_basket().unwrap().unwrap().status,
            BasketStatus::Proposed
        );
        cleanup(store, dir);
        cleanup(store2, dir2);
    }

    #[test]
    fn a_purchase_can_be_corrected_or_deleted() {
        let (store, dir, _, _, _) = two_drives("correct");
        product(&store, "lait");
        product(&store, "beurre");
        let mut p = purchase("lait", None, "confirmed", 100);
        p.discount_cents = 20;
        store.record_purchases(&[p]).unwrap();
        let id = store.purchases(0, u64::MAX / 4).unwrap()[0].id.unwrap();

        // The cost is recomputed from the new quantity and price, discount kept.
        let fixed = store.update_purchase(id, 3.0, 110, None, "beurre").unwrap();
        assert_eq!(fixed.product, "beurre");
        assert_eq!(fixed.qty, 3.0);
        assert_eq!(fixed.line_cents, Some(310));
        let fixed = store
            .update_purchase(id, 3.0, 110, Some(300), "beurre")
            .unwrap();
        assert_eq!(fixed.line_cents, Some(300));

        assert!(store.update_purchase(id, 0.0, 110, None, "beurre").is_err());
        assert!(matches!(
            store.update_purchase(id, 1.0, 110, None, "nope"),
            Err(PantryError::NotFound(_))
        ));
        assert!(matches!(
            store.update_purchase(id + 9, 1.0, 110, None, "lait"),
            Err(PantryError::NotFound(_))
        ));

        store.delete_purchase(id).unwrap();
        assert!(store.purchases(0, u64::MAX / 4).unwrap().is_empty());
        assert!(matches!(
            store.delete_purchase(id),
            Err(PantryError::NotFound(_))
        ));
        cleanup(store, dir);
    }

    #[test]
    fn line_cents_defaults_from_qty_price_and_discount() {
        let (store, dir, _, _, _) = two_drives("line-default");
        let mut p = purchase("fromage", None, "confirmed", 1299);
        p.source = "ticket".into();
        p.qty = 0.532;
        p.discount_cents = 50;
        p.label = "FROMAGE AU LAIT".into();
        p.unit = Some("kg".into());
        store.record_purchases(&[p]).unwrap();
        let read = store.purchases(0, u64::MAX / 4).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].line_cents, Some(641));
        assert_eq!(read[0].discount_cents, 50);
        assert_eq!(read[0].label, "FROMAGE AU LAIT");
        assert_eq!(read[0].unit.as_deref(), Some("kg"));
        assert!(read[0].id.is_some());
        cleanup(store, dir);
    }

    #[test]
    fn an_explicit_line_cents_is_kept() {
        let (store, dir, _, _, _) = two_drives("line-explicit");
        let mut p = purchase("pain", None, "confirmed", 100);
        p.line_cents = Some(500);
        store.record_purchases(&[p]).unwrap();
        let read = store.purchases(0, u64::MAX / 4).unwrap();
        assert_eq!(read[0].line_cents, Some(500));
        cleanup(store, dir);
    }

    #[test]
    fn purchases_are_filtered_by_time_and_validated() {
        let (store, dir, _, _, _) = two_drives("range");
        let mut early = purchase("lait", None, "confirmed", 100);
        early.at = 1_000;
        early.source = "ticket".into();
        let mut late = purchase("pain", None, "confirmed", 120);
        late.at = 5_000;
        store
            .record_purchases(&[early.clone(), late.clone()])
            .unwrap();
        assert_eq!(store.purchases(0, 2_000).unwrap(), vec![stored(&early, 1)]);
        assert_eq!(
            store.purchases(1_000, 5_000).unwrap(),
            vec![stored(&early, 1), stored(&late, 2)]
        );
        assert!(store.purchases(6_000, 9_000).unwrap().is_empty());

        let mut bad = purchase("lait", None, "payé", 100);
        assert!(matches!(
            store.record_purchases(std::slice::from_ref(&bad)),
            Err(PantryError::Invalid(_))
        ));
        bad.status = "confirmed".into();
        bad.qty = 0.0;
        assert!(matches!(
            store.record_purchases(&[bad]),
            Err(PantryError::Invalid(_))
        ));
        assert!(matches!(
            store.record_purchases(&[
                purchase("lait", None, "confirmed", 1),
                purchase("lait", Some(999), "confirmed", 1)
            ]),
            Err(PantryError::NotFound(_))
        ));
        assert!(matches!(
            store.confirm_purchases(999, &[purchase("lait", None, "confirmed", 1)]),
            Err(PantryError::NotFound(_))
        ));
        // Nothing of a refused batch is kept.
        assert_eq!(store.purchases(0, u64::MAX / 4).unwrap().len(), 2);
        cleanup(store, dir);
    }

    #[test]
    fn a_new_proposal_drops_the_chosen_basket_but_not_a_filled_one() {
        let (store, dir, _, _, item) = two_drives("replace-chosen");
        let first = store.propose("Sam", vec![], NOW).unwrap();
        store.choose(first.id, "carrefour", &[], "Sam").unwrap();
        let second = store.propose("Sam", vec![], NOW + 1).unwrap();
        let state = |id: i64| {
            store
                .lock()
                .query_row("SELECT status FROM baskets WHERE id = ?1", [id], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap()
        };
        let tied = || -> Option<i64> {
            store
                .lock()
                .query_row("SELECT basket FROM items WHERE id = ?1", [item], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        assert_eq!(state(first.id), "dropped");
        assert_eq!(tied(), None);
        assert_eq!(state(second.id), "proposed");

        // A basket already filled (or ordered) has real carts: it stays.
        store.choose(second.id, "carrefour", &[], "Sam").unwrap();
        store
            .set_status(second.id, BasketStatus::Filled, "Sam")
            .unwrap();
        let third = store.propose("Sam", vec![], NOW + 2).unwrap();
        assert_eq!(state(second.id), "filled");
        assert_eq!(tied(), Some(second.id));
        store
            .set_status(second.id, BasketStatus::Ordered, "Sam")
            .unwrap();
        store.propose("Sam", vec![], NOW + 3).unwrap();
        assert_eq!(state(second.id), "ordered");
        assert_eq!(state(third.id), "dropped");
        cleanup(store, dir);
    }

    /// Every item a basket's options speak of, wherever they put it.
    fn covered(basket: &Basket) -> BTreeSet<i64> {
        let o = &basket.options;
        o.singles
            .iter()
            .chain(o.split.iter())
            .flat_map(|c| c.groups.values().flatten().chain(c.missing.iter()))
            .chain(o.nowhere.iter())
            .chain(o.elsewhere.iter())
            .copied()
            .collect()
    }

    fn tied_to(store: &Store, item: i64) -> Option<i64> {
        store
            .lock()
            .query_row("SELECT basket FROM items WHERE id = ?1", [item], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// The basket chosen at carrefour, then moved up to `status`.
    fn carried(store: &Store, status: BasketStatus) -> i64 {
        let basket = store.propose("Sam", vec![], NOW).unwrap();
        store.choose(basket.id, "carrefour", &[], "Sam").unwrap();
        for next in [BasketStatus::Filled, BasketStatus::Ordered] {
            store.set_status(basket.id, next, "Sam").unwrap();
            if next == status {
                break;
            }
        }
        basket.id
    }

    #[test]
    fn an_item_already_in_a_filled_or_ordered_basket_is_not_proposed_again() {
        for status in [BasketStatus::Filled, BasketStatus::Ordered] {
            let (store, dir, _, _, item) = two_drives(&format!("in-cart-{}", status.as_str()));
            let first = carried(&store, status);
            let bread = store.add_item("pain", None, 1.0, "Sam", "ui").unwrap();
            let second = store.propose("Sam", vec![], NOW + 1).unwrap();
            let lines = covered(&second);
            assert!(!lines.contains(&item), "{status:?}: {lines:?}");
            assert!(lines.contains(&bread.id), "{status:?}: {lines:?}");
            assert_eq!(tied_to(&store, item), Some(first));
            cleanup(store, dir);
        }
    }

    #[test]
    fn a_new_request_during_an_order_makes_a_new_line() {
        let (store, dir, _, _, item) = two_drives("new-line");
        let basket = carried(&store, BasketStatus::Ordered);
        // Asked again while the first line is in the ordered cart: a new need.
        let again = store
            .add_item("lait", Some("lait"), 1.0, "Élodie", "ui")
            .unwrap();
        assert_ne!(again.id, item);
        let open: Vec<i64> = store.open_items().unwrap().iter().map(|i| i.id).collect();
        assert_eq!(open, vec![item, again.id]);
        // The same goes for a free-text line.
        let bread = store.add_item("Pain", None, 1.0, "Sam", "ui").unwrap();
        store
            .lock()
            .execute(
                "UPDATE items SET basket = ?1 WHERE id = ?2",
                params![basket, bread.id],
            )
            .unwrap();
        let bread_again = store.add_item("pain", None, 1.0, "Sam", "ui").unwrap();
        assert_ne!(bread_again.id, bread.id);
        // A third request joins the new line, which is in no cart.
        let third = store
            .add_item("lait", Some("lait"), 2.0, "Sam", "ui")
            .unwrap();
        assert_eq!(third.id, again.id);
        assert!((third.qty - 3.0).abs() < f64::EPSILON);

        // The pick-up closes only what the basket bought.
        store
            .record_purchases(&[purchase("lait", Some(basket), "provisional", 100)])
            .unwrap();
        store
            .confirm_purchases(basket, &[purchase("lait", None, "confirmed", 100)])
            .unwrap();
        let open: Vec<i64> = store.open_items().unwrap().iter().map(|i| i.id).collect();
        assert_eq!(open, vec![again.id, bread_again.id]);
        cleanup(store, dir);
    }

    #[test]
    fn choosing_never_takes_an_item_from_a_cart() {
        let (store, dir, _, _, item) = two_drives("choose-guard");
        let proposed = store.propose("Sam", vec![], NOW).unwrap();
        assert!(covered(&proposed).contains(&item));
        // Meanwhile the line went into a cart that is now ordered.
        let ordered = {
            let conn = store.lock();
            conn.execute(
                "INSERT INTO baskets(created_at, created_by, status, options, deviations, used)
                 SELECT created_at, created_by, 'ordered', options, deviations, used
                 FROM baskets WHERE id = ?1",
                [proposed.id],
            )
            .unwrap();
            let id = conn.last_insert_rowid();
            conn.execute(
                "UPDATE items SET basket = ?1 WHERE id = ?2",
                params![id, item],
            )
            .unwrap();
            id
        };
        store.choose(proposed.id, "carrefour", &[], "Sam").unwrap();
        assert_eq!(tied_to(&store, item), Some(ordered));
        cleanup(store, dir);
    }

    #[test]
    fn confirming_purchases_twice_keeps_the_corrected_lines_once() {
        let (store, dir, _, _, _) = two_drives("confirm-twice");
        let basket = carried(&store, BasketStatus::Ordered);
        let mut ticket = purchase("piles", Some(basket), "confirmed", 450);
        ticket.source = "ticket".into();
        store
            .record_purchases(&[
                purchase("lait", Some(basket), "provisional", 100),
                purchase("pain", Some(basket), "provisional", 120),
                ticket.clone(),
            ])
            .unwrap();
        let corrected = [
            purchase("lait", None, "confirmed", 95),
            purchase("pain", None, "confirmed", 120),
        ];
        store.confirm_purchases(basket, &corrected).unwrap();
        // A retry (lost answer, second call) changes nothing.
        store.confirm_purchases(basket, &corrected).unwrap();
        let mut kept: Vec<(String, String, i64, String)> = store
            .purchases(0, u64::MAX / 4)
            .unwrap()
            .into_iter()
            .map(|p| (p.source, p.product, p.price_cents, p.status))
            .collect();
        kept.sort();
        let line = |source: &str, product: &str, price: i64| {
            (source.into(), product.into(), price, "confirmed".to_owned())
        };
        assert_eq!(
            kept,
            vec![
                line("drive", "lait", 95),
                line("drive", "pain", 120),
                line("ticket", "piles", 450),
            ]
        );
        // A second correction replaces the first one; what the drive delivered
        // is a drive line even when sent as a receipt line, so it is replaced
        // in turn.
        let mut late = purchase("lait", None, "confirmed", 90);
        late.source = "ticket".into();
        for _ in 0..2 {
            store
                .confirm_purchases(basket, std::slice::from_ref(&late))
                .unwrap();
        }
        let drive: Vec<(String, i64)> = store
            .purchases(0, u64::MAX / 4)
            .unwrap()
            .into_iter()
            .filter(|p| p.product == "lait")
            .map(|p| (p.source, p.price_cents))
            .collect();
        assert_eq!(drive, vec![("drive".to_owned(), 90)]);
        cleanup(store, dir);
    }

    #[test]
    fn a_repeated_deviation_counts_one_refusal_per_choice() {
        let (store, dir, _, _, _) = two_drives("refusal-once");
        let twice = vec![deviation("x", "own_brand"), deviation("x", "own_brand")];
        let basket = store.propose("Sam", twice, NOW).unwrap();
        store.choose(basket.id, "carrefour", &[], "Sam").unwrap();
        let count: i64 = store
            .lock()
            .query_row(
                "SELECT count FROM refusals WHERE product = 'x' AND kind = 'own_brand'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        cleanup(store, dir);
    }
}
