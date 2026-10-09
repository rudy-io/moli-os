//! The Moli app on the household's phones (crate `moli-phones`).
//!
//! - `POST /api/mobile/register` (the household: LAN or Cloudflare Access,
//!   which is how the app's own web view is signed in): `{ name, platform,
//!   model, push_token? }` → `{ id, token, home }`; the token is shown once.
//! - `POST /api/phones/<id>/report` (`Authorization: Bearer <token>`, no
//!   Cloudflare login needed: see `caller.rs`): a `Report` → `{ home }`.

use axum::Extension;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::caller::Caller;

#[derive(Clone)]
pub(crate) struct PhonesGate(pub(crate) Option<moli_phones::Gateway>);

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pairing {
    name: String,
    platform: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    push_token: Option<String>,
}

pub(crate) async fn register(
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    caller: Caller,
    axum::Json(p): axum::Json<Pairing>,
) -> Response {
    let Some(gate) = gate else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.pilote.absent", pilote = "phones"),
        );
    };
    if !caller.is_household() {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.telephone.foyer_seulement"),
        );
    }
    match gate.register(&p.name, &p.platform, &p.model, p.push_token.as_deref()) {
        Ok(paired) => axum::Json(paired).into_response(),
        Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, &format!("{e:#}")),
    }
}

/// Forgets a phone (a person, with the PIN: a lost phone is cut off).
pub(crate) async fn remove(
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    Extension(humans): Extension<std::sync::Arc<crate::session::Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(gate) = gate else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.pilote.absent", pilote = "phones"),
        );
    };
    if !humans.is_human(&headers, &caller.key) {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.telephone.retrait_humain"),
        );
    }
    match gate.remove(&id) {
        Ok(true) => axum::Json(json!({ "removed": id })).into_response(),
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.telephone.inconnu"),
        ),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}

pub(crate) async fn report(
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    Path(id): Path<String>,
    headers: HeaderMap,
    axum::Json(r): axum::Json<moli_phones::Report>,
) -> Response {
    let Some(gate) = gate else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.pilote.absent", pilote = "phones"),
        );
    };
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !gate.verify(&id, token.trim()) {
        return error(
            StatusCode::UNAUTHORIZED,
            &moli_i18n::tr!("serveur.telephone.jeton_refuse"),
        );
    }
    match gate.report(&id, r) {
        Ok(home) => axum::Json(json!({ "home": home })).into_response(),
        Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, &format!("{e:#}")),
    }
}
