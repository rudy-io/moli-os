//! The dashboard, compiled into the binary.

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct Dist;

pub(crate) async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Unknown paths fall back to the single-page app.
    let (path, file) = match Dist::get(path) {
        Some(file) if !path.is_empty() => (path, file),
        _ => match Dist::get("index.html") {
            Some(file) => ("index.html", file),
            None => return (StatusCode::NOT_FOUND, "dashboard not built").into_response(),
        },
    };
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // Vite fingerprints everything under assets/: cache forever.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, mime.as_ref()),
            (header::CACHE_CONTROL, cache),
        ],
        file.data,
    )
        .into_response()
}
