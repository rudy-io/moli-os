//! The dashboard, compiled into the binary.

use axum::Extension;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

/// What the dashboard is called on a phone's home screen.
const DEFAULT_NAME: &str = "Moli";
/// The names `index.html` is written with, swapped for the house's own.
const PAGE_TITLE: &str = "<title>Moli OS</title>";
const HOME_SCREEN_TITLE: &str = r#"<meta name="apple-mobile-web-app-title" content="Moli" />"#;

/// The house's name for the app (`[server] app_name`), `Moli` when absent.
#[derive(Clone, Debug)]
pub(crate) struct AppName(String);

impl AppName {
    pub(crate) fn new(name: Option<&str>) -> Self {
        Self(
            name.map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(DEFAULT_NAME)
                .to_owned(),
        )
    }
}

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct Dist;

/// The web app manifest: the name is the house's, so it is not a file.
pub(crate) async fn manifest(Extension(AppName(name)): Extension<AppName>) -> Response {
    let icon = |src: &str, sizes: &str, purpose: &str| serde_json::json!({ "src": src, "sizes": sizes, "type": "image/png", "purpose": purpose });
    let manifest = serde_json::json!({
        "name": name,
        "short_name": name,
        "start_url": "/",
        "scope": "/",
        "display": "standalone",
        "background_color": "#161512",
        "theme_color": "#161512",
        "icons": [
            icon("/icon-192.png", "192x192", "any"),
            icon("/icon-512.png", "512x512", "any"),
            icon("/icon-maskable-512.png", "512x512", "maskable"),
        ],
    });
    (
        [
            (header::CONTENT_TYPE, "application/manifest+json"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        manifest.to_string(),
    )
        .into_response()
}

/// `index.html` with the house's name where the build wrote Moli's.
fn named(page: &[u8], name: &str) -> Vec<u8> {
    let escaped = name
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    String::from_utf8_lossy(page)
        .replace(PAGE_TITLE, &format!("<title>{escaped}</title>"))
        .replace(
            HOME_SCREEN_TITLE,
            &format!(r#"<meta name="apple-mobile-web-app-title" content="{escaped}" />"#),
        )
        .into_bytes()
}

pub(crate) async fn serve(Extension(AppName(name)): Extension<AppName>, uri: Uri) -> Response {
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
    let body = if path == "index.html" {
        named(&file.data, &name)
    } else {
        file.data.into_owned()
    };
    (
        [
            (header::CONTENT_TYPE, mime.as_ref()),
            (header::CACHE_CONTROL, cache),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_house_names_its_page_and_the_name_is_escaped() {
        let page = format!("{PAGE_TITLE}\n{HOME_SCREEN_TITLE}");
        let named = String::from_utf8(named(page.as_bytes(), r#"A "b" <c>"#)).unwrap();
        assert!(named.contains("<title>A &quot;b&quot; &lt;c&gt;</title>"));
        assert!(named.contains(r#"content="A &quot;b&quot; &lt;c&gt;""#));
    }

    #[test]
    fn without_a_name_it_is_moli() {
        assert_eq!(AppName::new(None).0, "Moli");
        assert_eq!(AppName::new(Some("  ")).0, "Moli");
        assert_eq!(AppName::new(Some(" Chez nous ")).0, "Chez nous");
    }
}
