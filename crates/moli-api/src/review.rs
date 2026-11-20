//! A camera's past (`/api/devices/{id}/review/…`), from its NVR: what it saw
//! and recorded, its zones, and their pictures and clips, streamed (never
//! held in memory) with the browser's `Range` passed on (seeking). The same
//! rules as its live image: never for a guest, and a camera in a protected
//! room shows itself to a person signed in with the house's code only.

use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use moli_core::DeviceId;
use moli_runtime::Hub;
use moli_runtime::media::{ReviewMedia, ReviewSource};
use serde::Deserialize;
use serde_json::json;

use crate::caller::Caller;
use crate::session::Sessions;

type Humans = Extension<Arc<Sessions>>;

/// The longest stretch asked at once (seconds): two days.
const MAX_SPAN: f64 = 2.0 * 86_400.0;

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
}

/// The camera's NVR, when this caller may look at it.
fn source(
    hub: &Hub,
    humans: &Sessions,
    caller: &Caller,
    headers: &HeaderMap,
    id: &str,
) -> Result<Arc<dyn ReviewSource>, (StatusCode, String)> {
    let id = DeviceId::from(id.to_owned());
    if caller
        .person
        .as_ref()
        .is_some_and(crate::people::PersonRef::is_guest)
    {
        return Err((
            StatusCode::FORBIDDEN,
            moli_i18n::tr!("serveur.personnes.invite_limite"),
        ));
    }
    if let Some(room) = hub.protected_room(&id)
        && !humans.is_human(headers, &caller.key)
    {
        return Err((
            StatusCode::FORBIDDEN,
            moli_i18n::tr!("serveur.camera.piece_protegee", room = room),
        ));
    }
    hub.review_source(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "no past to review for this device".to_owned(),
        )
    })
}

#[derive(Deserialize)]
pub(crate) struct Span {
    after: f64,
    before: f64,
}

/// What the camera saw and recorded between `after` and `before`.
pub(crate) async fn timeline(
    State(hub): State<Hub>,
    Extension(humans): Humans,
    caller: Caller,
    Path(id): Path<String>,
    Query(span): Query<Span>,
    headers: HeaderMap,
) -> Response {
    let source = match source(&hub, &humans, &caller, &headers, &id) {
        Ok(s) => s,
        Err((status, message)) => return error(status, &message),
    };
    if !(span.after.is_finite() && span.before.is_finite())
        || span.before <= span.after
        || span.before - span.after > MAX_SPAN
    {
        return error(StatusCode::BAD_REQUEST, "after < before, two days at most");
    }
    let (items, recorded) = tokio::join!(
        source.review(span.after, span.before),
        source.recorded(span.after, span.before)
    );
    match (items, recorded) {
        (Ok(items), Ok(recorded)) => {
            axum::Json(json!({ "items": items, "recorded": recorded })).into_response()
        }
        (Err(e), _) | (_, Err(e)) => error(StatusCode::BAD_GATEWAY, &format!("{e:#}")),
    }
}

/// The camera's zones.
pub(crate) async fn zones(
    State(hub): State<Hub>,
    Extension(humans): Humans,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let source = match source(&hub, &humans, &caller, &headers, &id) {
        Ok(s) => s,
        Err((status, message)) => return error(status, &message),
    };
    match source.zones().await {
        Ok(zones) => axum::Json(json!({ "zones": zones })).into_response(),
        Err(e) => error(StatusCode::BAD_GATEWAY, &format!("{e:#}")),
    }
}

/// An event's picture, small picture or clip (`what`: image, thumbnail, clip).
pub(crate) async fn event(
    State(hub): State<Hub>,
    Extension(humans): Humans,
    caller: Caller,
    Path((id, event, what)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let media = match what.as_str() {
        "image" => ReviewMedia::EventImage(event),
        "thumbnail" => ReviewMedia::EventThumbnail(event),
        "clip" => ReviewMedia::EventClip(event),
        _ => return error(StatusCode::NOT_FOUND, "image, thumbnail or clip"),
    };
    stream(&hub, &humans, &caller, &headers, &id, media).await
}

#[derive(Deserialize)]
pub(crate) struct Stretch {
    start: f64,
    end: f64,
    /// Set: the clip is saved under this name rather than played.
    download: Option<String>,
}

/// A name to save a clip under: letters, digits, `-`, `_` and `.`, ending
/// in `.mp4`, short. Anything else is no name (the clip plays).
fn file_name(name: &str) -> Option<&str> {
    let ok = (5..=100).contains(&name.len())
        && std::path::Path::new(name).extension() == Some(std::ffi::OsStr::new("mp4"))
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    ok.then_some(name)
}

/// What was recorded between `start` and `end` (an hour at most), as MP4;
/// with `download`, as a file to save (an extract of the recordings).
pub(crate) async fn recording(
    State(hub): State<Hub>,
    Extension(humans): Humans,
    caller: Caller,
    Path(id): Path<String>,
    Query(stretch): Query<Stretch>,
    headers: HeaderMap,
) -> Response {
    let media = ReviewMedia::Recording {
        start: stretch.start,
        end: stretch.end,
    };
    let mut response = stream(&hub, &humans, &caller, &headers, &id, media).await;
    if response.status().is_success()
        && let Some(name) = stretch.download.as_deref().and_then(file_name)
        && let Ok(value) = HeaderValue::try_from(format!("attachment; filename=\"{name}\""))
    {
        response
            .headers_mut()
            .insert(axum::http::header::CONTENT_DISPOSITION, value);
    }
    response
}

async fn stream(
    hub: &Hub,
    humans: &Sessions,
    caller: &Caller,
    headers: &HeaderMap,
    id: &str,
    media: ReviewMedia,
) -> Response {
    let source = match source(hub, humans, caller, headers, id) {
        Ok(s) => s,
        Err((status, message)) => return error(status, &message),
    };
    let range = headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .filter(|r| r.len() <= 100 && r.starts_with("bytes="))
        .map(str::to_owned);
    let reply = match source.media(media, range).await {
        Ok(r) => r,
        Err(e) => return error(StatusCode::BAD_GATEWAY, &format!("{e:#}")),
    };
    let mut response = Body::from_stream(reply.body).into_response();
    *response.status_mut() = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::OK);
    let out = response.headers_mut();
    for (name, value) in reply.headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(name.as_str()),
            HeaderValue::try_from(value.as_str()),
        ) {
            out.insert(name, value);
        }
    }
    out.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::file_name;

    #[test]
    fn a_clip_is_saved_only_under_a_plain_name() {
        assert_eq!(
            file_name("entree-2026-11-20-11h21.mp4"),
            Some("entree-2026-11-20-11h21.mp4")
        );
        assert_eq!(file_name("../etc/passwd.mp4"), None);
        assert_eq!(file_name("a\"b.mp4"), None);
        assert_eq!(file_name("clip.exe"), None);
        assert_eq!(file_name(".mp4"), None);
        assert_eq!(file_name(&format!("{}.mp4", "a".repeat(120))), None);
    }
}
