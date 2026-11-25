//! The household's vehicles (crate `moli-vehicles`).
//!
//! - `GET|POST /api/torque/<token>`: Torque Pro's « Upload to web server »
//!   (query string or form body), answered `OK!`. No Cloudflare login
//!   needed (see `caller.rs`): the token is the proof, and it is the only
//!   route such a caller reaches.
//! - `GET /api/vehicles` (the household): each vehicle's mileage, garage
//!   book, what is due and its fault codes.
//! - `GET /api/car/<token>/dashboard`: the car's own screen. Its own points,
//!   and of the house only what its configuration shares (`share`).
//! - `POST /api/car/<token>/action/<id>`: one of the orders its
//!   configuration lists (`action`), as the household's dashboard would give
//!   it (the guard still holds protected rooms for a person).

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::caller::Caller;
use moli_core::{DeviceId, Origin, PointId, Value};
use moli_runtime::{CommandError, Hub};

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

fn json_value(v: Option<moli_core::Sample>) -> serde_json::Value {
    v.map_or(serde_json::Value::Null, |s| s.value.to_json())
}

pub(crate) async fn dashboard(
    State(hub): State<Hub>,
    Extension(VehiclesGate(gate)): Extension<VehiclesGate>,
    Path(token): Path<String>,
) -> Response {
    let Some(gate) = gate else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no vehicles driver").into_response();
    };
    let Some(vehicle) = gate.vehicle_for(&token) else {
        return (StatusCode::NOT_FOUND, "unknown").into_response();
    };
    let car = gate
        .device_id(&vehicle)
        .and_then(|id| hub.device(&DeviceId::from(id)))
        .map(|d| {
            d.state
                .iter()
                .map(|(k, s)| (k.to_string(), s.value.to_json()))
                .collect::<serde_json::Map<_, _>>()
        })
        .unwrap_or_default();
    let shared: serde_json::Map<_, _> = gate
        .shared(&vehicle)
        .into_iter()
        .map(|s| (s.id, json_value(hub.state(&PointId::from(s.point)))))
        .collect();
    let actions: Vec<_> = gate
        .actions(&vehicle)
        .into_iter()
        .map(|a| json!({ "id": a.id, "name": a.name }))
        .collect();
    axum::Json(
        json!({ "car": car, "shared": shared, "actions": actions, "at": moli_core::now_ms() }),
    )
    .into_response()
}

pub(crate) async fn action(
    State(hub): State<Hub>,
    Extension(VehiclesGate(gate)): Extension<VehiclesGate>,
    Path((token, id)): Path<(String, String)>,
) -> Response {
    let Some(gate) = gate else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no vehicles driver").into_response();
    };
    let Some(vehicle) = gate.vehicle_for(&token) else {
        return (StatusCode::NOT_FOUND, "unknown").into_response();
    };
    let Some(order) = gate.actions(&vehicle).into_iter().find(|a| a.id == id) else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({ "error": "unknown action" })),
        )
            .into_response();
    };
    let point = PointId::from(order.point.clone());
    let actor = Some(format!("voiture ({vehicle})"));
    match hub
        .command_from(
            &point,
            Value::from_json(&order.value),
            Origin::Household,
            actor,
            None,
        )
        .await
    {
        Ok(()) => axum::Json(json!({ "ok": true })).into_response(),
        Err(CommandError::NeedsApproval { id, reason }) => (
            StatusCode::ACCEPTED,
            axum::Json(json!({ "ok": false, "pending": id, "reason": reason })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            axum::Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
