//! `/api/cellier`: the household's pantry (« Le Cellier », crate
//! `moli-pantry`): the shopping list, the products, the price comparison.
//!
//! - `GET /api/cellier`: everything at once (list, products, sheets, chains,
//!   threshold, open basket).
//! - `POST /api/cellier/items {text, product?, qty?}`: add a need.
//! - `PUT|DELETE /api/cellier/items/{id}`: change its quantity / remove it.
//! - `PUT /api/cellier/products/{id}`: create or change a product.
//! - `POST /api/cellier/offers/{id}/confirm`: a human checked the sheet.
//! - `PUT /api/cellier/settings {threshold?, providers?}`: the split
//!   threshold and the chains' settings.
//! - `POST /api/cellier/baskets/{id}/choose {option, accepted}`: pick an
//!   option of the basket.
//! - `POST /api/cellier/baskets/{id}/status {status}`: move the basket on.
//! - `POST /api/cellier/receipts {photos}`: read the photos (1 to 4, JPEG,
//!   base64) of one receipt and keep the reading as a draft.
//! - `GET /api/cellier/receipts?status=` (without the raw reading),
//!   `GET|PUT|DELETE /api/cellier/receipts/{id}`: the receipts, a draft
//!   corrected or dropped.
//! - `POST /api/cellier/receipts/{id}/validate {force}`: a person (a human
//!   session) records the draft: its kept lines become purchases.
//! - `GET /api/cellier/receipts/{id}/photos/{n}`: a photo kept.
//! - `GET /api/cellier/purchases?from=&to=`, `PUT|DELETE
//!   /api/cellier/purchases/{id}`: what was bought, corrected or dropped.
//! - `GET /api/cellier/reader`: who reads receipts, and whether they may.
//!
//! Without a `[pantry]` section every route answers `404`. Guests and the
//! demo house are kept out by the caller check, before any handler.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use base64::Engine as _;
use moli_pantry::{
    BasketStatus, MAX_PHOTOS, PROVIDERS, Pantry, PantryError, Product, ProviderSettings,
    ReceiptDraft, ReceiptStatus,
};
use moli_runtime::Hub;
use serde::Deserialize;
use serde_json::json;

use crate::assistant::Moli;
use crate::caller::Caller;
use crate::session::Sessions;

/// The channel of everything done from the dashboard.
const CHANNEL: &str = "ui";

/// The photos of one receipt, base64 in a JSON body: four JPEGs the
/// dashboard has already shrunk (a few hundred KB each), with a wide margin.
pub(crate) const MAX_RECEIPT: usize = 12 * 1024 * 1024;

/// A year, in ms: how far back the purchases page looks by default.
const YEAR_MS: u64 = 365 * 24 * 3_600_000;
/// A day, in ms: the slack kept after now for a clock a little ahead.
pub(crate) const DAY_MS: u64 = 24 * 3_600_000;

#[derive(Clone)]
pub(crate) struct Cellier(pub(crate) Option<Pantry>);

pub(crate) fn routes(pantry: Option<Pantry>, humans: &Arc<Sessions>) -> Router<Hub> {
    Router::new()
        .route("/api/cellier", get(overview))
        .route("/api/cellier/items", post(add_item))
        .route("/api/cellier/items/{id}", put(set_qty).delete(remove_item))
        .route("/api/cellier/products/{id}", put(save_product))
        .route("/api/cellier/offers/{id}/confirm", post(confirm_offer))
        .route("/api/cellier/settings", put(settings))
        .route("/api/cellier/baskets/{id}/choose", post(choose))
        .route("/api/cellier/baskets/{id}/status", post(set_status))
        .route(
            "/api/cellier/receipts",
            get(list_receipts).merge(crate::upload(post(read_receipt), MAX_RECEIPT, humans)),
        )
        .route(
            "/api/cellier/receipts/{id}",
            get(show_receipt).put(save_receipt).delete(drop_receipt),
        )
        // Nothing becomes a purchase without a person (agents record theirs
        // through MCP).
        .route(
            "/api/cellier/receipts/{id}/validate",
            crate::human_only(post(validate_receipt), humans),
        )
        .route("/api/cellier/receipts/{id}/photos/{n}", get(receipt_photo))
        .route("/api/cellier/purchases", get(purchases))
        .route(
            "/api/cellier/purchases/{id}",
            put(correct_purchase).delete(drop_purchase),
        )
        .route("/api/cellier/reader", get(reader))
        .layer(Extension(Cellier(pantry)))
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn absent() -> Response {
    error(StatusCode::NOT_FOUND, "the pantry is not set up ([pantry])")
}

/// A refused or failed order, said the same way everywhere.
fn failed(e: &PantryError) -> Response {
    let status = match e {
        PantryError::NotFound(_) => StatusCode::NOT_FOUND,
        PantryError::Invalid(_) => StatusCode::BAD_REQUEST,
        PantryError::Db(_) | PantryError::Internal(_) => {
            tracing::error!(error = %e, "pantry");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    error(status, &e.to_string())
}

/// A result as JSON, or the error.
fn json_or<T: serde::Serialize>(r: Result<T, PantryError>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => failed(&e),
    }
}

/// A result without a body, or the error.
fn done(r: Result<(), PantryError>) -> Response {
    match r {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => failed(&e),
    }
}

/// Who acts: the person's name when Moli knows who it is, else how the
/// caller is described.
fn actor(caller: &Caller) -> String {
    caller
        .person
        .as_ref()
        .map_or_else(|| caller.describe(), |p| p.name.clone())
}

pub(crate) async fn overview(Extension(Cellier(pantry)): Extension<Cellier>) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    json_or(pantry.overview(moli_core::now_ms()).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewItem {
    text: String,
    #[serde(default)]
    product: Option<String>,
    #[serde(default)]
    qty: Option<f64>,
}

pub(crate) async fn add_item(
    Extension(Cellier(pantry)): Extension<Cellier>,
    caller: Caller,
    Json(body): Json<NewItem>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    let added = pantry
        .add_item(
            &body.text,
            body.product.as_deref(),
            body.qty.unwrap_or(1.0),
            &actor(&caller),
            CHANNEL,
        )
        .await;
    match added {
        Ok(item) => (StatusCode::CREATED, Json(item)).into_response(),
        Err(e) => failed(&e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Qty {
    qty: f64,
}

pub(crate) async fn set_qty(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Path(id): Path<i64>,
    Json(body): Json<Qty>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    json_or(pantry.set_qty(id, body.qty).await)
}

pub(crate) async fn remove_item(
    Extension(Cellier(pantry)): Extension<Cellier>,
    caller: Caller,
    Path(id): Path<i64>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    done(pantry.remove_item(id, &actor(&caller)).await)
}

pub(crate) async fn save_product(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Path(id): Path<String>,
    Json(product): Json<Product>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    if product.id != id {
        return error(
            StatusCode::BAD_REQUEST,
            "the product's id does not match the address",
        );
    }
    done(pantry.save_product(&product).await)
}

pub(crate) async fn confirm_offer(
    Extension(Cellier(pantry)): Extension<Cellier>,
    caller: Caller,
    Path(id): Path<i64>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    done(pantry.confirm_offer(id, &actor(&caller)).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    #[serde(default)]
    threshold: Option<f64>,
    #[serde(default)]
    providers: Option<Vec<ProviderSettings>>,
}

pub(crate) async fn settings(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Json(body): Json<Settings>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    if let Some(threshold) = body.threshold
        && let Err(e) = pantry.set_split_threshold(threshold).await
    {
        return failed(&e);
    }
    for provider in body.providers.iter().flatten() {
        if let Err(e) = pantry.save_provider_settings(provider).await {
            return failed(&e);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Choice {
    option: String,
    #[serde(default)]
    accepted: Vec<usize>,
}

pub(crate) async fn choose(
    Extension(Cellier(pantry)): Extension<Cellier>,
    caller: Caller,
    Path(id): Path<i64>,
    Json(body): Json<Choice>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    json_or(
        pantry
            .choose(id, &body.option, &body.accepted, &actor(&caller))
            .await,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Status {
    status: String,
}

pub(crate) async fn set_status(
    Extension(Cellier(pantry)): Extension<Cellier>,
    caller: Caller,
    Path(id): Path<i64>,
    Json(body): Json<Status>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    let Ok(status) = serde_json::from_value::<BasketStatus>(json!(body.status)) else {
        return error(StatusCode::BAD_REQUEST, "unknown basket status");
    };
    json_or(pantry.set_status(id, status, &actor(&caller)).await)
}

/// A JPEG starts with these three bytes. The dashboard always sends JPEG
/// (it shrinks every photo first), and the files are served as such.
const JPEG_MAGIC: [u8; 3] = [0xFF, 0xD8, 0xFF];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Photos {
    photos: Vec<String>,
}

/// Decodes the photos of a receipt: 1 to [`MAX_PHOTOS`], each a base64 JPEG.
/// The error is what to tell the sender.
fn decode_photos(encoded: &[String]) -> Result<Vec<Vec<u8>>, String> {
    if encoded.is_empty() || encoded.len() > MAX_PHOTOS {
        return Err(format!("a receipt needs 1 to {MAX_PHOTOS} photos"));
    }
    encoded
        .iter()
        .map(|photo| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(photo.trim())
                .map_err(|_| "a photo is not valid base64".to_owned())?;
            if bytes.starts_with(&JPEG_MAGIC) {
                Ok(bytes)
            } else {
                Err("a photo is not a JPEG".to_owned())
            }
        })
        .collect()
}

/// Reads the photos of a receipt with the assistant and keeps the reading as
/// a draft for a person to check. Nothing is created when the reading fails
/// or says the photos are not a receipt.
pub(crate) async fn read_receipt(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Extension(Moli(moli)): Extension<Moli>,
    caller: Caller,
    Json(body): Json<Photos>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    let photos = match decode_photos(&body.photos) {
        Ok(photos) => photos,
        Err(why) => return error(StatusCode::BAD_REQUEST, &why),
    };
    let Some(moli) = moli else {
        return crate::assistant::failure(&moli_assistant::AssistantError::NotConfigured(
            "no assistant".into(),
        ));
    };
    let products = match pantry.products().await {
        Ok(products) => products
            .into_iter()
            .map(|p| (p.id, p.name, p.aisle))
            .collect::<Vec<_>>(),
        Err(e) => return failed(&e),
    };
    let chains = PROVIDERS
        .iter()
        .map(|p| (p.id.to_owned(), p.name.to_owned()))
        .collect::<Vec<_>>();
    let (read, reader) = match moli.read_receipt(&photos, &products, &chains).await {
        Ok(done) => done,
        Err(e) => return crate::assistant::failure(&e),
    };
    if read["unreadable"] == json!(true) {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "unreadable");
    }
    let draft = match serde_json::from_value::<ReceiptDraft>(read.clone()) {
        Ok(draft) => draft,
        Err(e) => {
            // The schema keeps card numbers out of a reading: it is safe to log.
            tracing::warn!(error = %e, reading = %read, "the receipt reading does not fit a draft");
            return error(
                StatusCode::BAD_GATEWAY,
                "the reading of the receipt is not usable",
            );
        }
    };
    match pantry
        .add_receipt(photos, draft, read, reader, actor(&caller))
        .await
    {
        Ok(receipt) => (StatusCode::CREATED, Json(receipt)).into_response(),
        Err(e) => failed(&e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceiptsQuery {
    #[serde(default)]
    status: Option<String>,
}

pub(crate) async fn list_receipts(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Query(query): Query<ReceiptsQuery>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    let status = match query.status.as_deref() {
        None => None,
        Some("draft") => Some(ReceiptStatus::Draft),
        Some("validated") => Some(ReceiptStatus::Validated),
        Some(_) => return error(StatusCode::BAD_REQUEST, "unknown receipt status"),
    };
    // As in the purchases listing, the raw reading stays out of the list.
    json_or(pantry.receipts(status).await.map(|mut receipts| {
        for receipt in &mut receipts {
            receipt.read = serde_json::Value::Null;
        }
        receipts
    }))
}

pub(crate) async fn show_receipt(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Path(id): Path<i64>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    json_or(pantry.receipt(id).await)
}

pub(crate) async fn save_receipt(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Path(id): Path<i64>,
    Json(draft): Json<ReceiptDraft>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    json_or(pantry.save_draft(id, &draft).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Validation {
    #[serde(default)]
    force: bool,
}

pub(crate) async fn validate_receipt(
    Extension(Cellier(pantry)): Extension<Cellier>,
    caller: Caller,
    Path(id): Path<i64>,
    Json(body): Json<Validation>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    json_or(
        pantry
            .validate_receipt(id, &actor(&caller), moli_core::now_ms(), body.force)
            .await,
    )
}

pub(crate) async fn drop_receipt(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Path(id): Path<i64>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    done(pantry.abandon_receipt(id).await)
}

/// A photo kept of a receipt (`n` from 1); `404` once it is erased.
pub(crate) async fn receipt_photo(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Path((id, n)): Path<(i64, usize)>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    if !(1..=MAX_PHOTOS).contains(&n) {
        return error(StatusCode::NOT_FOUND, "no such photo");
    }
    match tokio::fs::read(pantry.photo_path(id, n)).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "private, max-age=86400"),
            ],
            bytes,
        )
            .into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            error(StatusCode::NOT_FOUND, "no such photo")
        }
        Err(e) => {
            tracing::error!(error = %e, "cannot read a receipt photo");
            error(StatusCode::INTERNAL_SERVER_ERROR, "cannot read the photo")
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Period {
    #[serde(default)]
    from: Option<u64>,
    #[serde(default)]
    to: Option<u64>,
}

/// The purchases of a period with the receipts they come from and the drafts
/// still to check, as `{purchases, receipts}`. Receipts go without the
/// model's raw reading. Shared by the REST route and the MCP tool.
pub(crate) async fn purchases_with_receipts(
    pantry: &Pantry,
    from: u64,
    to: u64,
) -> Result<serde_json::Value, PantryError> {
    let bought = pantry.purchases(from, to).await?;
    let mut receipts = pantry.receipts(Some(ReceiptStatus::Draft)).await?;
    let known = receipts.iter().map(|r| r.id).collect::<BTreeSet<_>>();
    let wanted = bought
        .iter()
        .filter_map(|p| p.receipt)
        .filter(|id| !known.contains(id))
        .collect::<BTreeSet<_>>();
    for id in wanted {
        match pantry.receipt(id).await {
            Ok(receipt) => receipts.push(receipt),
            // A purchase may outlive its receipt's record.
            Err(PantryError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
    }
    for receipt in &mut receipts {
        receipt.read = serde_json::Value::Null;
    }
    Ok(json!({ "purchases": bought, "receipts": receipts }))
}

/// The purchases of a period (the last 12 months by default), with the
/// receipts they come from and the drafts still to check.
pub(crate) async fn purchases(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Query(period): Query<Period>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    let now = moli_core::now_ms();
    let to = period.to.unwrap_or(now.saturating_add(DAY_MS));
    let from = period.from.unwrap_or_else(|| now.saturating_sub(YEAR_MS));
    json_or(purchases_with_receipts(&pantry, from, to).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Correction {
    qty: f64,
    price_cents: i64,
    #[serde(default)]
    line_cents: Option<i64>,
    product: String,
}

pub(crate) async fn correct_purchase(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Path(id): Path<i64>,
    Json(body): Json<Correction>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    json_or(
        pantry
            .update_purchase(
                id,
                body.qty,
                body.price_cents,
                body.line_cents,
                &body.product,
            )
            .await,
    )
}

pub(crate) async fn drop_purchase(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Path(id): Path<i64>,
) -> Response {
    let Some(pantry) = pantry else {
        return absent();
    };
    done(pantry.delete_purchase(id).await)
}

/// Who reads receipts, and whether they may.
pub(crate) async fn reader(
    Extension(Cellier(pantry)): Extension<Cellier>,
    Extension(Moli(moli)): Extension<Moli>,
) -> Response {
    if pantry.is_none() {
        return absent();
    }
    match moli {
        Some(moli) => Json(moli.receipt_reader()).into_response(),
        None => Json(json!({ "allowed": false, "reason": "no assistant" })).into_response(),
    }
}
