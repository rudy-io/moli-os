//! Pantry (« Le Cellier »): the household shopping list, the products it is
//! made of, and the price comparison of grocery drives.
//!
//! Humans (dashboard) and AI agents (MCP) work on the same list. Everything
//! lives in one SQLite file (`pantry.db` in the data folder, saved by
//! `moli-os backup`); amounts are `i64` cents and times are `u64`
//! milliseconds since the epoch, like `moli-history`.
//!
//! - [`Store`] holds the storage and the pure logic, synchronous.
//! - [`Pantry`] is the handle the surfaces (REST, MCP) use: every store
//!   method, async, on a blocking thread; each successful write announces
//!   `Event::PantryChanged` on the hub, so the dashboards re-read.
//! - Who did what is kept in the pantry's own rows (`added_by`,
//!   `confirmed_by`, `chosen_by`, `read_by`), not in the hub's journal, which
//!   speaks of devices.
//! - Dormant by default: only a `[pantry]` section in `moli.toml` opens it,
//!   and never in the demo house. Nothing runs in the background.

mod basket;
mod baskets;
mod model;
mod providers;
mod receipts;
mod store;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use moli_runtime::Hub;
use serde::Deserialize;
use serde_json::json;

use crate::receipts::{purge_old_photos, remove_photos, write_photos};
use crate::store::now_ms;

pub use basket::{BasketOptions, Choice, Line, Terms, compute};
pub use model::{
    Basket, BasketStatus, Deviation, DraftLine, Flexibility, Item, Offer, OfferStatus, Product,
    ProviderInfo, ProviderKind, ProviderSettings, Purchase, Reading, Receipt, ReceiptDraft,
    ReceiptStatus,
};
pub use providers::PROVIDERS;
pub use receipts::MAX_PHOTOS;
pub use store::{PantryError, Store};

/// The database's file in the data folder.
pub const FILE: &str = "pantry.db";

/// The `[pantry]` section of `moli.toml`. Empty for now: its presence alone
/// switches the pantry on.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {}

/// Cheap handle used by the surfaces: the store, and the hub it announces
/// its changes on.
#[derive(Clone, Debug)]
pub struct Pantry {
    store: Arc<Store>,
    hub: Hub,
    data_dir: PathBuf,
}

impl Pantry {
    /// Opens (or creates) `data_dir/pantry.db`, and erases the receipt photos
    /// that are past their keeping time.
    pub fn open(hub: &Hub, data_dir: &Path) -> Result<Self, PantryError> {
        let pantry = Self {
            store: Arc::new(Store::open(&data_dir.join(FILE))?),
            hub: hub.clone(),
            data_dir: data_dir.to_path_buf(),
        };
        if let Err(e) = purge_old_photos(&pantry.store, &pantry.receipts_dir(), now_ms()) {
            tracing::warn!(error = %e, "cannot erase the old receipt photos");
        }
        Ok(pantry)
    }

    /// The folder holding the photos of the receipts.
    pub fn receipts_dir(&self) -> PathBuf {
        self.data_dir.join("receipts")
    }

    /// Where shot `n` (1 to [`MAX_PHOTOS`]) of a receipt is kept.
    pub fn photo_path(&self, id: i64, n: usize) -> PathBuf {
        receipts::photo_file(&self.receipts_dir(), id, n)
    }

    /// Everything a dashboard or an agent needs at once: the open list, the
    /// products, their sheets, each chain with its settings, the split
    /// threshold and the open basket (or `null`), read in one go. `now` (ms)
    /// is the time the overview is taken at; nothing in it depends on the
    /// time yet.
    pub async fn overview(&self, _now: u64) -> Result<serde_json::Value, PantryError> {
        self.read(|s| {
            let providers: Vec<serde_json::Value> = PROVIDERS
                .iter()
                .zip(s.provider_settings()?)
                .map(|(info, settings)| json!({ "info": info, "settings": settings }))
                .collect();
            Ok(json!({
                "items": s.open_items()?,
                "products": s.products()?,
                "offers": s.offers(None)?,
                "providers": providers,
                "threshold": s.split_threshold()?,
                "basket": s.open_basket()?,
            }))
        })
        .await
    }

    // ---- the list and the products -------------------------------------------

    pub async fn add_item(
        &self,
        text: &str,
        product: Option<&str>,
        qty: f64,
        by: &str,
        channel: &str,
    ) -> Result<Item, PantryError> {
        let (text, product) = (text.to_owned(), product.map(str::to_owned));
        let (by, channel) = (by.to_owned(), channel.to_owned());
        self.write(move |s| s.add_item(&text, product.as_deref(), qty, &by, &channel))
            .await
    }

    pub async fn remove_item(&self, id: i64, by: &str) -> Result<(), PantryError> {
        let by = by.to_owned();
        self.write(move |s| s.remove_item(id, &by)).await
    }

    pub async fn set_qty(&self, id: i64, qty: f64) -> Result<Item, PantryError> {
        self.write(move |s| s.set_qty(id, qty)).await
    }

    pub async fn open_items(&self) -> Result<Vec<Item>, PantryError> {
        self.read(Store::open_items).await
    }

    pub async fn save_product(&self, p: &Product) -> Result<(), PantryError> {
        let p = p.clone();
        self.write(move |s| s.save_product(&p)).await
    }

    pub async fn products(&self) -> Result<Vec<Product>, PantryError> {
        self.read(Store::products).await
    }

    // ---- sheets, prices, chains, threshold -----------------------------------

    pub async fn save_offer(&self, o: &Offer) -> Result<i64, PantryError> {
        let o = o.clone();
        self.write(move |s| s.save_offer(&o)).await
    }

    pub async fn confirm_offer(&self, id: i64, by: &str) -> Result<(), PantryError> {
        let by = by.to_owned();
        self.write(move |s| s.confirm_offer(id, &by)).await
    }

    pub async fn offers(&self, product: Option<&str>) -> Result<Vec<Offer>, PantryError> {
        let product = product.map(str::to_owned);
        self.read(move |s| s.offers(product.as_deref())).await
    }

    pub async fn record_readings(&self, readings: &[Reading]) -> Result<(), PantryError> {
        let readings = readings.to_vec();
        self.write(move |s| s.record_readings(&readings)).await
    }

    pub async fn latest_readings(&self, newer_than: u64) -> Result<Vec<Reading>, PantryError> {
        self.read(move |s| s.latest_readings(newer_than)).await
    }

    pub async fn provider_settings(&self) -> Result<Vec<ProviderSettings>, PantryError> {
        self.read(Store::provider_settings).await
    }

    pub async fn save_provider_settings(
        &self,
        settings: &ProviderSettings,
    ) -> Result<(), PantryError> {
        let settings = settings.clone();
        self.write(move |s| s.save_provider_settings(&settings))
            .await
    }

    pub async fn split_threshold(&self) -> Result<f64, PantryError> {
        self.read(Store::split_threshold).await
    }

    pub async fn set_split_threshold(&self, v: f64) -> Result<(), PantryError> {
        self.write(move |s| s.set_split_threshold(v)).await
    }

    // ---- baskets and purchases -----------------------------------------------

    pub async fn propose(
        &self,
        by: &str,
        deviations: Vec<Deviation>,
        now: u64,
    ) -> Result<Basket, PantryError> {
        let by = by.to_owned();
        self.write(move |s| s.propose(&by, deviations, now)).await
    }

    pub async fn choose(
        &self,
        basket: i64,
        option: &str,
        accepted: &[usize],
        by: &str,
    ) -> Result<Basket, PantryError> {
        let (option, accepted, by) = (option.to_owned(), accepted.to_vec(), by.to_owned());
        self.write(move |s| s.choose(basket, &option, &accepted, &by))
            .await
    }

    pub async fn set_status(
        &self,
        basket: i64,
        status: BasketStatus,
        by: &str,
    ) -> Result<Basket, PantryError> {
        let by = by.to_owned();
        self.write(move |s| s.set_status(basket, status, &by)).await
    }

    pub async fn record_purchases(&self, p: &[Purchase]) -> Result<(), PantryError> {
        let p = p.to_vec();
        self.write(move |s| s.record_purchases(&p)).await
    }

    pub async fn confirm_purchases(
        &self,
        basket: i64,
        corrected: &[Purchase],
    ) -> Result<(), PantryError> {
        let corrected = corrected.to_vec();
        self.write(move |s| s.confirm_purchases(basket, &corrected))
            .await
    }

    pub async fn purchases(&self, from: u64, to: u64) -> Result<Vec<Purchase>, PantryError> {
        self.read(move |s| s.purchases(from, to)).await
    }

    pub async fn open_basket(&self) -> Result<Option<Basket>, PantryError> {
        self.read(Store::open_basket).await
    }

    /// Corrects a purchase (see [`Store::update_purchase`]).
    pub async fn update_purchase(
        &self,
        id: i64,
        qty: f64,
        price_cents: i64,
        line_cents: Option<i64>,
        product: &str,
    ) -> Result<Purchase, PantryError> {
        let product = product.to_owned();
        self.write(move |s| s.update_purchase(id, qty, price_cents, line_cents, &product))
            .await
    }

    pub async fn delete_purchase(&self, id: i64) -> Result<(), PantryError> {
        self.write(move |s| s.delete_purchase(id)).await
    }

    // ---- receipts ------------------------------------------------------------

    /// Stores a freshly read receipt as a draft, **then** writes its photos
    /// (1 to [`MAX_PHOTOS`]); if they cannot be written the draft is removed
    /// again. `read` is the model's raw answer, `reader` who or what read it.
    pub async fn add_receipt(
        &self,
        photos: Vec<Vec<u8>>,
        draft: ReceiptDraft,
        read: serde_json::Value,
        reader: String,
        by: String,
    ) -> Result<Receipt, PantryError> {
        if photos.is_empty() || photos.len() > MAX_PHOTOS || photos.iter().any(Vec::is_empty) {
            return Err(PantryError::Invalid(format!(
                "a receipt needs 1 to {MAX_PHOTOS} photos"
            )));
        }
        let count = photos.len();
        let id = self
            .read(move |s| s.create_receipt(&draft, &read, &reader, count, &by, now_ms()))
            .await?;
        let dir = self.receipts_dir();
        let written = tokio::task::spawn_blocking(move || write_photos(&dir, id, &photos))
            .await
            .map_err(|e| PantryError::Internal(e.to_string()))?;
        if let Err(e) = written {
            let _ = self.read(move |s| s.abandon_receipt(id)).await;
            return Err(PantryError::Internal(format!(
                "cannot save the photos: {e}"
            )));
        }
        self.hub.pantry_changed();
        self.read(move |s| s.receipt(id)).await
    }

    pub async fn receipt(&self, id: i64) -> Result<Receipt, PantryError> {
        self.read(move |s| s.receipt(id)).await
    }

    pub async fn receipts(
        &self,
        status: Option<ReceiptStatus>,
    ) -> Result<Vec<Receipt>, PantryError> {
        self.read(move |s| s.receipts(status)).await
    }

    pub async fn save_draft(&self, id: i64, draft: &ReceiptDraft) -> Result<Receipt, PantryError> {
        let draft = draft.clone();
        self.write(move |s| s.save_draft(id, &draft)).await
    }

    /// Validates a receipt (see [`Store::validate_receipt`]), then erases the
    /// photos that are past their keeping time.
    pub async fn validate_receipt(
        &self,
        id: i64,
        by: &str,
        now: u64,
        force: bool,
    ) -> Result<Receipt, PantryError> {
        let by = by.to_owned();
        let receipt = self
            .write(move |s| s.validate_receipt(id, &by, now, force))
            .await?;
        if let Err(e) = self.purge_photos(now).await {
            tracing::warn!(error = %e, "cannot erase the old receipt photos");
        }
        Ok(receipt)
    }

    /// Deletes a draft and its photo files.
    pub async fn abandon_receipt(&self, id: i64) -> Result<(), PantryError> {
        self.write(move |s| s.abandon_receipt(id)).await?;
        let dir = self.receipts_dir();
        tokio::task::spawn_blocking(move || remove_photos(&dir, id))
            .await
            .map_err(|e| PantryError::Internal(e.to_string()))
    }

    pub async fn photos_to_purge(&self, now: u64) -> Result<Vec<(i64, usize)>, PantryError> {
        self.read(move |s| s.photos_to_purge(now)).await
    }

    pub async fn forget_photos(&self, id: i64) -> Result<(), PantryError> {
        self.write(move |s| s.forget_photos(id)).await
    }

    /// Erases the photos of the receipts validated over 90 days before
    /// `now`; returns how many receipts had some.
    pub async fn purge_photos(&self, now: u64) -> Result<usize, PantryError> {
        let dir = self.receipts_dir();
        let purged = self.read(move |s| purge_old_photos(s, &dir, now)).await?;
        if purged > 0 {
            self.hub.pantry_changed();
        }
        Ok(purged)
    }

    // ---- internals -----------------------------------------------------------

    /// Runs `f` on a blocking thread (SQLite is synchronous).
    async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Store) -> Result<T, PantryError> + Send + 'static,
    ) -> Result<T, PantryError> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .map_err(|e| PantryError::Internal(e.to_string()))?
    }

    /// Like [`Pantry::read`], then announces the change once it succeeded.
    async fn write<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Store) -> Result<T, PantryError> + Send + 'static,
    ) -> Result<T, PantryError> {
        let done = self.read(f).await?;
        self.hub.pantry_changed();
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use moli_core::Event;
    use moli_runtime::{Hub, HubOptions};
    use tokio::sync::broadcast::error::TryRecvError;

    use super::*;

    fn pantry(name: &str) -> (Pantry, Hub, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("moli-pantry-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hub = Hub::new(HubOptions::default()).unwrap();
        let pantry = Pantry::open(&hub, &dir).unwrap();
        assert!(dir.join("pantry.db").exists());
        (pantry, hub, dir)
    }

    #[tokio::test]
    async fn writes_announce_a_change() {
        let (pantry, hub, dir) = pantry("announce");
        let mut events = hub.subscribe();
        pantry.open_items().await.unwrap();
        assert_eq!(
            events.try_recv(),
            Err(TryRecvError::Empty),
            "a read announces nothing"
        );
        assert!(
            pantry.add_item("  ", None, 1.0, "Sam", "ui").await.is_err(),
            "an item needs a text"
        );
        assert_eq!(
            events.try_recv(),
            Err(TryRecvError::Empty),
            "a refused write announces nothing"
        );
        pantry
            .add_item("couches", None, 1.0, "Élodie", "ui")
            .await
            .unwrap();
        assert_eq!(events.recv().await.unwrap(), Event::PantryChanged);
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty), "once");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_overview_shows_the_whole_cellier() {
        let (pantry, _hub, dir) = pantry("overview");
        pantry
            .add_item("couches", None, 2.0, "Élodie", "ui")
            .await
            .unwrap();
        let view = pantry.overview(0).await.unwrap();
        assert_eq!(view["items"][0]["text"], "couches");
        assert_eq!(view["products"], serde_json::json!([]));
        assert_eq!(view["offers"], serde_json::json!([]));
        let providers = view["providers"].as_array().unwrap();
        assert_eq!(providers.len(), PROVIDERS.len());
        assert_eq!(providers[0]["info"]["id"], PROVIDERS[0].id);
        assert_eq!(providers[0]["settings"]["id"], PROVIDERS[0].id);
        assert_eq!(view["threshold"], 0.3);
        assert!(view["basket"].is_null(), "no basket yet");

        let basket = pantry.propose("agent", Vec::new(), 0).await.unwrap();
        let view = pantry.overview(0).await.unwrap();
        assert_eq!(view["basket"]["id"], basket.id);
        let _ = std::fs::remove_dir_all(dir);
    }
}
