//! The sky over the house, for the dashboard (crate `moli-weather`).
//!
//! - `GET /api/weather/forecast`: the next 24 hours and the next 7 days.
//! - `GET /api/weather/map`: the weather map around the house: the area,
//!   the next day's wind, clouds and rain on a grid, and the land under it.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Router};
use moli_runtime::Hub;
use moli_weather::Weather;
use serde_json::json;

#[derive(Clone)]
pub(crate) struct Sky(pub(crate) Option<Weather>);

pub(crate) fn routes(weather: Option<Weather>) -> Router<Hub> {
    Router::new()
        .route("/api/weather/forecast", get(forecast))
        .route("/api/weather/map", get(map))
        .layer(Extension(Sky(weather)))
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
}

fn unplaced() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "the house has no position (latitude, longitude)",
    )
}

pub(crate) async fn forecast(Extension(Sky(weather)): Extension<Sky>) -> Response {
    let Some(weather) = weather else {
        return unplaced();
    };
    match weather.forecast().await {
        Ok(f) => axum::Json(f).into_response(),
        Err(e) => error(StatusCode::BAD_GATEWAY, &format!("{e:#}")),
    }
}

pub(crate) async fn map(Extension(Sky(weather)): Extension<Sky>) -> Response {
    let Some(weather) = weather else {
        return unplaced();
    };
    match weather.map().await {
        Ok(m) => axum::Json(m).into_response(),
        Err(e) => error(StatusCode::BAD_GATEWAY, &format!("{e:#}")),
    }
}
