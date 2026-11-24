//! The household's vehicles (crate `moli-vehicles`).
//!
//! - `GET|POST /api/torque/<token>`: Torque Pro's « Upload to web server »
//!   (query string or form body), answered `OK!`. No Cloudflare login
//!   needed (see `caller.rs`): the token is the proof, and it is the only
//!   route such a caller reaches.
//! - `GET /api/vehicles` (the household): each vehicle's mileage, garage
//!   book, what is due and its fault codes.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::caller::Caller;

#[derive(Clone)]
pub(crate) struct VehiclesGate(pub(crate) Option<moli_vehicles::Gateway>);

pub(crate) async fn torque(
    Extension(VehiclesGate(gate)): Extension<VehiclesGate>,
    Path(token): Path<String>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    let Some(gate) = gate else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no vehicles driver").into_response();
    };
    let Some(vehicle) = gate.vehicle_for(&token) else {
        return (StatusCode::NOT_FOUND, "unknown").into_response();
    };
    let query = query.unwrap_or_default();
    let raw = if query.is_empty() {
        String::from_utf8_lossy(&body).into_owned()
    } else {
        query
    };
    if let Err(e) = gate.upload(&vehicle, &raw) {
        // Answered OK all the same: Torque would keep resending what Moli
        // will never take.
        tracing::warn!(vehicle, error = %e, "torque upload dropped");
    }
    // Torque wants exactly this, or it keeps the data and tries again.
    "OK!".into_response()
}

pub(crate) async fn list(
    Extension(VehiclesGate(gate)): Extension<VehiclesGate>,
    caller: Caller,
) -> Response {
    if !caller.is_household() {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({ "error": moli_i18n::tr!("serveur.acces.refuse") })),
        )
            .into_response();
    }
    axum::Json(json!({ "vehicles": gate.map(|g| g.list()).unwrap_or_default() })).into_response()
}
