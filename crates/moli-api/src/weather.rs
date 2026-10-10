//! The sky over the house, for the dashboard (crate `moli-weather`).
//!
//! - `GET /api/weather/forecast`: the next 24 hours and the next 7 days.
//! - `GET /api/weather/satellite`: the Meteosat images at hand (their
//!   times) and where the house is on them.
//! - `GET /api/weather/satellite/{time}`: one of those images (JPEG), only
//!   ever served from what is at hand: asking never makes Moli fetch.

use axum::extract::Path;
use axum::http::{StatusCode, header};
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
        .route("/api/weather/satellite", get(satellite))
        .route("/api/weather/satellite/{time}", get(frame))
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

pub(crate) async fn satellite(Extension(Sky(weather)): Extension<Sky>) -> Response {
    let Some(weather) = weather else {
        return unplaced();
    };
    match weather.sky().await {
        Ok(s) => axum::Json(s).into_response(),
        Err(e) => error(StatusCode::BAD_GATEWAY, &format!("{e:#}")),
    }
}

pub(crate) async fn frame(
    Extension(Sky(weather)): Extension<Sky>,
    Path(time): Path<String>,
) -> Response {
    let Some(weather) = weather else {
        return unplaced();
    };
    let time = time.strip_suffix(".jpg").unwrap_or(&time);
    match weather.frame(time).await {
        // An image never changes: kept by the browser as long as it likes.
        Some(jpeg) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "private, max-age=86400, immutable"),
            ],
            jpeg,
        )
            .into_response(),
        None => error(StatusCode::NOT_FOUND, "no such image"),
    }
}
