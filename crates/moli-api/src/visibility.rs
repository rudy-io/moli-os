//! A person's reversible choice to show or hide a device in Moli. The
//! existing layout editor owns the shared lock and atomic file replacement.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use moli_core::{DeviceId, Origin, Outcome};
use moli_runtime::Hub;
use serde::Deserialize;
use serde_json::json;

use crate::caller::Caller;
use crate::rest::{HomeFile, origin};
use crate::session::Sessions;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Visibility {
    hidden: bool,
}

pub(crate) async fn put(
    State(hub): State<Hub>,
    Extension(HomeFile(path)): Extension<HomeFile>,
    Extension(humans): Extension<Arc<Sessions>>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(change): Json<Visibility>,
) -> Response {
    let origin = origin(&hub, &headers, &humans, &caller);
    if origin != Origin::Ui {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.session.humain_seulement"),
        );
    }
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &moli_i18n::tr!("serveur.visibilite.identifiant_invalide"),
        );
    }
    let device = DeviceId::from(id.clone());
    if hub.device(&device).is_none() {
        return error(StatusCode::NOT_FOUND, "unknown device");
    }
    let Some(path) = path else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.visibilite.indisponible"),
        );
    };
    let hidden = change.hidden;
    let saved = tokio::task::spawn_blocking(move || moli_assistant::set_hidden(&path, &id, hidden))
        .await
        .map_err(std::io::Error::other)
        .and_then(std::convert::identity);
    let outcome = match &saved {
        Ok(()) => Outcome::Ok,
        Err(error) => Outcome::Err(error.to_string()),
    };
    hub.record_visibility(device, hidden, origin, Some(caller.describe()), outcome);
    match saved {
        Ok(()) => Json(json!({ "hidden": hidden })).into_response(),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &moli_i18n::tr!("serveur.visibilite.enregistrement_impossible"),
        ),
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}
