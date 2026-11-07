//! Room photos (and page banners, keys starting with « @ »), kept apart from
//! the plan: the household sees them, a guest does not (no route here is in
//! a guest's list), and only a person (PIN session) changes them.
//!
//! - `GET    /api/photos`: the map (room → image name).
//! - `PUT    /api/photos/{room}`: a person sends a room's photo (PNG, JPEG or
//!   WebP); the one it replaces is deleted when nothing else uses it.
//! - `DELETE /api/photos/{room}`: a person takes it away.
//! - `GET    /api/photos/images/{name}`: serves one (never an SVG).
//!
//! Files: `<data>/photos.json` (`{ room: image }`) and `<data>/photos/<name>`.
//! Each change reads, edits and writes the map under one lock: two photos
//! sent at once never undo each other.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, Path as AxumPath};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use ring::digest::{SHA256, digest};
use serde_json::json;

use crate::caller::Caller;
use crate::plan::{PlanDir, image_name_ok};
use crate::session::Sessions;

/// A photo is 8 MB at most (the dashboard sends it shrunk, far smaller).
pub(crate) const MAX_PHOTO: usize = 8 * 1024 * 1024;

/// Rooms and pages with a photo, at most.
const MAX_PHOTOS: usize = 48;

/// One change of the map at a time.
static WRITE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

type Map = BTreeMap<String, String>;
type Failure = (StatusCode, String);

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn io(e: &std::io::Error) -> Failure {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn room_ok(room: &str) -> bool {
    let room = room.trim();
    !room.is_empty()
        && room.chars().count() <= 60
        && !room
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
}

async fn read_map(dir: &Path) -> Map {
    let Ok(data) = tokio::fs::read(dir.join("photos.json")).await else {
        return Map::new();
    };
    serde_json::from_slice::<Map>(&data)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, name)| image_name_ok(name) && name.rsplit('.').next() != Some("svg"))
        .collect()
}

async fn write_map(dir: &Path, map: &Map) -> std::io::Result<()> {
    let path = dir.join("photos.json");
    let data = serde_json::to_vec_pretty(map).map_err(std::io::Error::other)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    // A name of its own: two writes never mix in one file.
    let tmp: PathBuf = dir.join(format!("photos.json.{nanos}.tmp"));
    tokio::fs::write(&tmp, &data).await?;
    tokio::fs::rename(&tmp, &path).await
}

fn hex8(body: &[u8]) -> String {
    digest(&SHA256, body).as_ref()[..8]
        .iter()
        .fold(String::with_capacity(16), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// An image nobody uses any more is deleted (a failure only leaves a file).
async fn drop_unused(dir: &Path, map: &Map, old: Option<String>, new: Option<&str>) {
    if let Some(old) = old
        && Some(old.as_str()) != new
        && !map.values().any(|v| *v == old)
    {
        let _ = tokio::fs::remove_file(dir.join("photos").join(&old)).await;
    }
}

/// Keeps `body` as `room`'s photo; returns the map.
pub(crate) async fn store(dir: &Path, room: &str, ext: &str, body: &[u8]) -> Result<Map, Failure> {
    if !room_ok(room) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            moli_i18n::tr!("serveur.photos.piece_invalide"),
        ));
    }
    if body.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            moli_i18n::tr!("serveur.plan.image_vide"),
        ));
    }
    let room = room.trim().to_owned();
    let name = format!("{}.{ext}", hex8(body));

    let _guard = WRITE.lock().await;
    let mut map = read_map(dir).await;
    if !map.contains_key(&room) && map.len() >= MAX_PHOTOS {
        return Err((
            StatusCode::INSUFFICIENT_STORAGE,
            moli_i18n::tr!("serveur.photos.trop", max = MAX_PHOTOS),
        ));
    }
    let folder = dir.join("photos");
    tokio::fs::create_dir_all(&folder)
        .await
        .map_err(|e| io(&e))?;
    tokio::fs::write(folder.join(&name), body)
        .await
        .map_err(|e| io(&e))?;
    let previous = map.insert(room, name.clone());
    write_map(dir, &map).await.map_err(|e| io(&e))?;
    drop_unused(dir, &map, previous, Some(&name)).await;
    Ok(map)
}

/// Takes `room`'s photo away; returns the map.
pub(crate) async fn forget(dir: &Path, room: &str) -> Result<Map, Failure> {
    let room = room.trim();
    let _guard = WRITE.lock().await;
    let mut map = read_map(dir).await;
    let previous = map.remove(room);
    if previous.is_some() {
        write_map(dir, &map).await.map_err(|e| io(&e))?;
    }
    drop_unused(dir, &map, previous, None).await;
    Ok(map)
}

pub(crate) async fn list(Extension(PlanDir(dir)): Extension<PlanDir>) -> Response {
    match dir {
        Some(dir) => Json(read_map(&dir).await).into_response(),
        None => Json(Map::new()).into_response(),
    }
}

/// The data folder, if the caller is a person and there is one.
fn allowed(
    dir: Option<PathBuf>,
    humans: &Sessions,
    caller: &Caller,
    headers: &HeaderMap,
) -> Result<PathBuf, Failure> {
    if !humans.is_human(headers, &caller.key) {
        return Err((
            StatusCode::FORBIDDEN,
            moli_i18n::tr!("serveur.plan.humain_ajoute"),
        ));
    }
    dir.ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            moli_i18n::tr!("serveur.plan.pas_de_dossier"),
        )
    })
}

pub(crate) async fn put(
    Extension(PlanDir(dir)): Extension<PlanDir>,
    Extension(humans): Extension<Arc<Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    AxumPath(room): AxumPath<String>,
    body: Bytes,
) -> Response {
    let dir = match allowed(dir, &humans, &caller, &headers) {
        Ok(dir) => dir,
        Err((status, why)) => return error(status, &why),
    };
    let kind = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let ext = match kind.split(';').next().unwrap_or_default().trim() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => {
            return error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                &moli_i18n::tr!("serveur.plan.type_image"),
            );
        }
    };
    match store(&dir, &room, ext, &body).await {
        Ok(map) => Json(map).into_response(),
        Err((status, why)) => error(status, &why),
    }
}

pub(crate) async fn remove(
    Extension(PlanDir(dir)): Extension<PlanDir>,
    Extension(humans): Extension<Arc<Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    AxumPath(room): AxumPath<String>,
) -> Response {
    let dir = match allowed(dir, &humans, &caller, &headers) {
        Ok(dir) => dir,
        Err((status, why)) => return error(status, &why),
    };
    match forget(&dir, &room).await {
        Ok(map) => Json(map).into_response(),
        Err((status, why)) => error(status, &why),
    }
}

pub(crate) async fn image(
    Extension(PlanDir(dir)): Extension<PlanDir>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    let missing = || {
        error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.plan.introuvable"),
        )
    };
    let Some(dir) = dir.filter(|_| image_name_ok(&name)) else {
        return missing();
    };
    let mime = match name.rsplit('.').next() {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => return missing(),
    };
    let Ok(data) = tokio::fs::read(dir.join("photos").join(&name)).await else {
        return missing();
    };
    let mut res = data.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    // The name is its content: it never changes; it is the household's.
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("moli-photos-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).expect("temporary folder");
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn a_new_photo_deletes_the_one_it_replaces() {
        let td = TempDir::new("replace");
        let dir = &td.0;
        let first = store(dir, "Salon", "jpg", b"one").await.expect("first")["Salon"].clone();
        let second = store(dir, "Salon", "jpg", b"two").await.expect("second")["Salon"].clone();
        assert_ne!(first, second);
        assert!(dir.join("photos").join(&second).exists());
        assert!(!dir.join("photos").join(&first).exists());
    }

    #[tokio::test]
    async fn a_photo_two_rooms_share_stays() {
        let td = TempDir::new("shared");
        let dir = &td.0;
        let shared = store(dir, "Salon", "jpg", b"same").await.expect("a")["Salon"].clone();
        store(dir, "Cuisine", "jpg", b"same").await.expect("b");
        store(dir, "Salon", "jpg", b"other").await.expect("c");
        assert!(dir.join("photos").join(&shared).exists());
    }

    #[tokio::test]
    async fn at_most_48_rooms_but_one_can_always_change() {
        let td = TempDir::new("limit");
        let dir = &td.0;
        for i in 0..MAX_PHOTOS {
            store(
                dir,
                &format!("room{i}"),
                "jpg",
                format!("body-{i}").as_bytes(),
            )
            .await
            .expect("fill");
        }
        let full = store(dir, "@accueil", "jpg", b"x").await.expect_err("full");
        assert_eq!(full.0, StatusCode::INSUFFICIENT_STORAGE);
        store(dir, "room0", "jpg", b"again").await.expect("replace");
    }

    #[tokio::test]
    async fn odd_rooms_are_refused() {
        let td = TempDir::new("odd");
        let dir = &td.0;
        for room in ["../x", "", "   ", "a\nb"] {
            let refused = store(dir, room, "jpg", b"x").await.expect_err(room);
            assert_eq!(refused.0, StatusCode::UNPROCESSABLE_ENTITY);
        }
        assert!(store(dir, "Salon", "jpg", b"").await.is_err());
    }

    #[tokio::test]
    async fn forgetting_deletes_the_file() {
        let td = TempDir::new("forget");
        let dir = &td.0;
        let name = store(dir, "Salon", "jpg", b"bye").await.expect("store")["Salon"].clone();
        let map = forget(dir, "Salon").await.expect("forget");
        assert!(!map.contains_key("Salon"));
        assert!(!dir.join("photos").join(&name).exists());
    }
}
