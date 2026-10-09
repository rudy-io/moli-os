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
    // Paired from a person's session: the phone is theirs.
    let person = caller.person.as_ref().map(|p| p.id.as_str());
    match gate.register(
        &p.name,
        &p.platform,
        &p.model,
        p.push_token.as_deref(),
        person,
    ) {
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

/// The household's phones, and whose each is.
pub(crate) async fn list(
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    caller: Caller,
) -> Response {
    if !caller.is_household() {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.acces.refuse"),
        );
    }
    axum::Json(json!({ "phones": gate.map(|g| g.list()).unwrap_or_default() })).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Owner {
    person: Option<String>,
}

/// Gives a phone to a person (an owner of the house). Under `/api/mobile/`:
/// a house behind Cloudflare Access lets `/api/phones` through unsigned
/// (the phones' reports), so an owner's order must not live there.
pub(crate) async fn assign(
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    Extension(humans): Extension<std::sync::Arc<crate::session::Sessions>>,
    Extension(persons): Extension<std::sync::Arc<crate::people::People>>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<Owner>,
) -> Response {
    let Some(gate) = gate else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.pilote.absent", pilote = "phones"),
        );
    };
    if !crate::people_api::is_owner(&caller, &humans, &persons, &headers) {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.personnes.proprietaire_seul"),
        );
    }
    if let Some(p) = &body.person
        && persons.get(p).is_none()
    {
        return error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.personnes.inconnue"),
        );
    }
    match gate.assign(&id, body.person.as_deref()) {
        Ok(true) => axum::Json(json!({ "phone": id, "person": body.person })).into_response(),
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.telephone.inconnu"),
        ),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}
