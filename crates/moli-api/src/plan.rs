//! `/api/plan`: the house drawn on its own plans, floor by floor, with each
//! device where it really is (« le plan vivant »).
//!
//! - `GET /api/plan`: the floors and where devices sit. A floor is either
//!   an image (devices at relative 0–1 coordinates on it), or drawn: its
//!   `size` in cm, walls, rooms, openings and fixtures, devices in cm. A
//!   drawn floor is zoomable and takes any skin (`skin`): the rendering is
//!   the dashboard's, the house is data an agent writes from a plan.
//! - `PUT /api/plan`: a person (PIN session) arranges it.
//! - `POST /api/plan/images`: a person sends a floor's image (PNG, JPEG,
//!   WebP or SVG, 8 MB at most), stored under a name derived from its
//!   content; `GET /api/plan/images/<name>` serves it, scripts forbidden
//!   (an SVG is a document, not just a picture).
//!
//! Files: `<data>/plan.json` and `<data>/plan/<name>`.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::caller::Caller;
use crate::session::Sessions;

/// The data directory (plan.json and plan/ live there).
#[derive(Clone)]
pub(crate) struct PlanDir(pub(crate) Option<PathBuf>);

pub(crate) const MAX_IMAGE: usize = 8 * 1024 * 1024;
const MAX_FLOORS: usize = 8;
const MAX_PLACED: usize = 400;
const MAX_SHAPES: usize = 400;
const MAX_POINTS: usize = 64;
/// A floor is at most 100 m on a side (gardens included).
const MAX_SIDE_CM: f64 = 10_000.0;

/// How the dashboard may draw a drawn floor.
const SKINS: &[&str] = &["plan", "blueprint", "nuit", "aquarelle"];
const ROOM_KINDS: &[&str] = &[
    "living", "kitchen", "bedroom", "bath", "wc", "office", "hall", "veranda", "storage", "void",
    "roof", "garden", "deck", "pool", "terrace", "driveway", "garage", "other",
];
const OPENING_KINDS: &[&str] = &["window", "door", "bay", "gate"];
const ITEM_KINDS: &[&str] = &[
    "stairs",
    "stairs_u",
    "bed",
    "bunk",
    "sofa",
    "armchair",
    "chair",
    "table",
    "car",
    "counter",
    "fridge",
    "washer",
    "plant",
    "tv",
    "desk",
    "wardrobe",
    "bathtub",
    "shower",
    "sink",
    "toilet",
    "lounger",
    "trampoline",
    "bin",
    "other",
];
/// Heights (cm) a wall, a fixture, a floor or a roof may say.
const HEIGHTS: std::ops::RangeInclusive<f64> = 1.0..=1000.0;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    skin: Option<String>,
    #[serde(default)]
    floors: Vec<Floor>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Floor {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    /// Width and height in cm: the floor is drawn (no image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<[f64; 2]>,
    /// In 3D: the height of this floor's ground (cm), and where its origin
    /// sits on the first floor's (cm), so floors stack wall on wall.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    elevation: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offset: Option<[f64; 2]>,
    /// In 3D: how high its walls stand (cm; 250 by default, less under a
    /// roof).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    height: Option<f64>,
    /// In 3D: a two-sided roof over this floor's rooms (the house seen
    /// from outside when this floor is looked at).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    roof: Option<Roof>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    walls: Vec<Shape>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    rooms: Vec<Area>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    openings: Vec<Shape>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    items: Vec<Shape>,
    #[serde(default)]
    devices: Vec<Placed>,
}

/// A two-sided roof: its ridge runs along `x` or `y` (cm on the floor),
/// `rise` cm above the eaves.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Roof {
    ridge: String,
    rise: f64,
}

/// A box in cm: a wall, an opening (`kind`: window, door…), a fixture
/// (`kind`: bed, stairs…, turned by `r` degrees around its centre), how
/// high it stands in 3D (`height`, cm: a low garden wall, a railing).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Shape {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    height: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

/// A room: its outline in cm, what it is, and the Moli room it is (for the
/// lights that make it glow and the devices that belong in it).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Area {
    id: String,
    name: String,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    room: Option<String>,
    poly: Vec<[f64; 2]>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Placed {
    id: String,
    x: f64,
    y: f64,
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
}

/// Plan images kept at most (a house has a few floors, not a gallery).
const MAX_IMAGES: usize = 32;

/// `<16 hex>.<png|jpg|webp|svg>`: the only names ever written or served
/// (or backed up, or restored).
pub fn image_name_ok(name: &str) -> bool {
    let Some((stem, ext)) = name.split_once('.') else {
        return false;
    };
    stem.len() == 16
        && stem
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && matches!(ext, "png" | "jpg" | "webp" | "svg")
}

fn text_ok(s: &str, max: usize) -> bool {
    let s = s.trim();
    !s.is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control)
}

fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Floor {
    /// The drawing of a drawn floor: everything inside it (a margin for
    /// walls drawn on its edge), kinds from the known lists, bounded.
    fn check_drawing(&self, [w, h]: [f64; 2]) -> Result<(), String> {
        let side = |v: f64| v.is_finite() && v > 0.0 && v <= MAX_SIDE_CM;
        if !side(w) || !side(h) {
            return Err(moli_i18n::tr!("serveur.plan.taille_etage", w = w, h = h));
        }
        let shift = |v: f64| v.is_finite() && v.abs() <= MAX_SIDE_CM;
        if self.elevation.is_some_and(|e| !shift(e))
            || self.offset.is_some_and(|[x, y]| !shift(x) || !shift(y))
        {
            return Err(moli_i18n::tr!("serveur.plan.altitude"));
        }
        if self.height.is_some_and(|h| !HEIGHTS.contains(&h))
            || self.roof.as_ref().is_some_and(|r| {
                !matches!(r.ridge.as_str(), "x" | "y") || !HEIGHTS.contains(&r.rise)
            })
        {
            return Err(moli_i18n::tr!("serveur.plan.hauteur"));
        }
        let inside = |x: f64, y: f64| {
            x.is_finite()
                && y.is_finite()
                && (-100.0..=w + 100.0).contains(&x)
                && (-100.0..=h + 100.0).contains(&y)
        };
        let boxed = |s: &Shape, kinds: Option<&[&str]>, what: &str| -> Result<(), String> {
            let sized = s.w.is_finite() && s.h.is_finite() && s.w > 0.0 && s.h > 0.0;
            let turned =
                s.r.is_none_or(|r| r.is_finite() && (-360.0..=360.0).contains(&r));
            let kind = match (kinds, &s.kind) {
                (Some(kinds), Some(k)) => kinds.contains(&k.as_str()),
                (Some(_), None) => false,
                (None, k) => k.is_none(),
            };
            let label = s.label.as_deref().is_none_or(|l| text_ok(l, 40));
            let high = s.height.is_none_or(|h| HEIGHTS.contains(&h));
            if sized
                && turned
                && kind
                && label
                && high
                && inside(s.x, s.y)
                && inside(s.x + s.w, s.y + s.h)
            {
                Ok(())
            } else {
                Err(moli_i18n::tr!(
                    "serveur.plan.forme_invalide",
                    what = what,
                    kind = format!("{:?}", s.kind),
                    x = s.x,
                    y = s.y
                ))
            }
        };
        if self.walls.len() > MAX_SHAPES
            || self.openings.len() > MAX_SHAPES
            || self.items.len() > MAX_SHAPES
        {
            return Err(moli_i18n::tr!(
                "serveur.plan.trop_de_formes",
                max = MAX_SHAPES
            ));
        }
        for wall in &self.walls {
            boxed(wall, None, &moli_i18n::tr!("serveur.plan.forme.mur"))?;
        }
        for opening in &self.openings {
            boxed(
                opening,
                Some(OPENING_KINDS),
                &moli_i18n::tr!("serveur.plan.forme.ouverture"),
            )?;
        }
        for item in &self.items {
            boxed(
                item,
                Some(ITEM_KINDS),
                &moli_i18n::tr!("serveur.plan.forme.element"),
            )?;
        }
        if self.rooms.len() > 100 {
            return Err(moli_i18n::tr!("serveur.plan.trop_de_pieces"));
        }
        let mut ids = std::collections::HashSet::new();
        for room in &self.rooms {
            let shape = (3..=MAX_POINTS).contains(&room.poly.len())
                && room.poly.iter().all(|[x, y]| inside(*x, *y));
            if !id_ok(&room.id)
                || !ids.insert(&room.id)
                || !text_ok(&room.name, 60)
                || !ROOM_KINDS.contains(&room.kind.as_str())
                || room.room.as_deref().is_some_and(|r| !text_ok(r, 60))
                || !shape
            {
                return Err(moli_i18n::tr!(
                    "serveur.plan.piece_invalide",
                    id = format!("{:?}", room.id)
                ));
            }
        }
        Ok(())
    }
}

impl Plan {
    fn check(&self, dir: &FsPath) -> Result<(), String> {
        if self.floors.len() > MAX_FLOORS {
            return Err(moli_i18n::tr!(
                "serveur.plan.trop_d_etages",
                max = MAX_FLOORS
            ));
        }
        if let Some(skin) = &self.skin
            && !SKINS.contains(&skin.as_str())
        {
            return Err(moli_i18n::tr!(
                "serveur.plan.habillage_inconnu",
                skin = format!("{skin:?}")
            ));
        }
        let mut ids = std::collections::HashSet::new();
        for f in &self.floors {
            if !id_ok(&f.id) || !ids.insert(&f.id) {
                return Err(moli_i18n::tr!(
                    "serveur.plan.etage_id",
                    id = format!("{:?}", f.id)
                ));
            }
            let name = f.name.trim();
            if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
                return Err(moli_i18n::tr!("serveur.plan.etage_nom"));
            }
            if let Some(image) = &f.image
                && (!image_name_ok(image) || !dir.join("plan").join(image).exists())
            {
                return Err(moli_i18n::tr!(
                    "serveur.plan.image_inconnue",
                    image = format!("{image:?}")
                ));
            }
            if f.devices.len() > MAX_PLACED {
                return Err(moli_i18n::tr!(
                    "serveur.plan.trop_d_appareils",
                    max = MAX_PLACED
                ));
            }
            if let Some(size) = f.size {
                f.check_drawing(size)?;
            } else if !(f.walls.is_empty()
                && f.rooms.is_empty()
                && f.openings.is_empty()
                && f.items.is_empty())
            {
                return Err(moli_i18n::tr!(
                    "serveur.plan.etage_sans_taille",
                    name = f.name
                ));
            }
            // On an image, 0–1 of it; on a drawing, cm inside it.
            let [w, h] = f.size.unwrap_or([1.0, 1.0]);
            for d in &f.devices {
                let inside = |v: f64, max: f64| v.is_finite() && (0.0..=max).contains(&v);
                if d.id.is_empty() || d.id.len() > 256 || !inside(d.x, w) || !inside(d.y, h) {
                    return Err(moli_i18n::tr!(
                        "serveur.plan.placement_invalide",
                        id = format!("{:?}", d.id)
                    ));
                }
            }
        }
        Ok(())
    }
}

pub(crate) async fn get(Extension(PlanDir(dir)): Extension<PlanDir>) -> Response {
    let Some(dir) = dir else {
        return axum::Json(Plan::default()).into_response();
    };
    match tokio::fs::read(dir.join("plan.json")).await {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(plan) => axum::Json(plan).into_response(),
            Err(e) => error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &moli_i18n::tr!("serveur.plan.fichier_illisible", error = e),
            ),
        },
        Err(_) => axum::Json(Plan::default()).into_response(),
    }
}

pub(crate) async fn put(
    Extension(PlanDir(dir)): Extension<PlanDir>,
    Extension(humans): Extension<Arc<Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    axum::Json(plan): axum::Json<Plan>,
) -> Response {
    if !humans.is_human(&headers, &caller.key) {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.plan.humain_arrange"),
        );
    }
    let Some(dir) = dir else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.plan.pas_de_dossier"),
        );
    };
    if let Err(why) = plan.check(&dir) {
        return error(StatusCode::UNPROCESSABLE_ENTITY, &why);
    }
    let text = match serde_json::to_string_pretty(&plan) {
        Ok(text) => text + "\n",
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    // A name of its own: two saves at once never write into the same file.
    let tmp = dir.join(format!(
        "plan.json.{}.tmp",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let path = dir.join("plan.json");
    let written = async {
        tokio::fs::write(&tmp, text).await?;
        tokio::fs::rename(&tmp, &path).await
    };
    match written.await {
        Ok(()) => axum::Json(plan).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

pub(crate) async fn upload(
    Extension(PlanDir(dir)): Extension<PlanDir>,
    Extension(humans): Extension<Arc<Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !humans.is_human(&headers, &caller.key) {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.plan.humain_ajoute"),
        );
    }
    let Some(dir) = dir else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.plan.pas_de_dossier"),
        );
    };
    let kind = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let ext = match kind.split(';').next().unwrap_or_default().trim() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => {
            return error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                &moli_i18n::tr!("serveur.plan.type_image"),
            );
        }
    };
    if body.is_empty() {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &moli_i18n::tr!("serveur.plan.image_vide"),
        );
    }
    let hash = digest(&SHA256, &body);
    let mut name = hash.as_ref()[..8]
        .iter()
        .fold(String::with_capacity(20), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        });
    name.push('.');
    name.push_str(ext);
    let folder = dir.join("plan");
    let known = std::fs::read_dir(&folder).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_str().is_some_and(image_name_ok))
            .count()
    });
    if known >= MAX_IMAGES && !folder.join(&name).exists() {
        return error(
            StatusCode::INSUFFICIENT_STORAGE,
            &moli_i18n::tr!("serveur.plan.trop_d_images", max = MAX_IMAGES),
        );
    }
    let saved = async {
        tokio::fs::create_dir_all(&folder).await?;
        tokio::fs::write(folder.join(&name), &body).await
    };
    match saved.await {
        Ok(()) => axum::Json(json!({ "image": name })).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

pub(crate) async fn image(
    Extension(PlanDir(dir)): Extension<PlanDir>,
    Path(name): Path<String>,
) -> Response {
    let (Some(dir), true) = (dir, image_name_ok(&name)) else {
        return error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.plan.introuvable"),
        );
    };
    let kind = match name.rsplit('.').next() {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/svg+xml",
    };
    match tokio::fs::read(dir.join("plan").join(&name)).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, kind),
                // The name is its content: it never changes.
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                // An SVG opened on its own must not run anything.
                (
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; style-src 'unsafe-inline'; sandbox",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.plan.introuvable"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sane_plans_are_kept() {
        let dir = std::env::temp_dir().join(format!("moli-plan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("plan")).unwrap();
        std::fs::write(dir.join("plan").join("0123456789abcdef.png"), b"x").unwrap();
        let plan = |json: serde_json::Value| serde_json::from_value::<Plan>(json).unwrap();
        let good = plan(json!({ "floors": [{ "id": "rdc", "name": "Rez-de-chaussée",
            "image": "0123456789abcdef.png", "devices": [{ "id": "hue:1", "x": 0.4, "y": 0.6 }] }] }));
        assert!(good.check(&dir).is_ok());
        let outside = plan(
            json!({ "floors": [{ "id": "rdc", "name": "R", "devices": [{ "id": "a", "x": 1.4, "y": 0.1 }] }] }),
        );
        assert!(outside.check(&dir).is_err());
        let traversal =
            plan(json!({ "floors": [{ "id": "rdc", "name": "R", "image": "../moli.toml" }] }));
        assert!(traversal.check(&dir).is_err());
        let twice =
            plan(json!({ "floors": [{ "id": "a", "name": "A" }, { "id": "a", "name": "B" }] }));
        assert!(twice.check(&dir).is_err());
        assert!(!image_name_ok("0123456789ABCDEF.png"));
        assert!(!image_name_ok("0123456789abcdef.exe"));
        assert!(serde_json::from_value::<Plan>(json!({ "floors": [], "extra": 1 })).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_drawn_floor_is_checked_in_cm() {
        let dir = std::env::temp_dir();
        let plan = |json: serde_json::Value| serde_json::from_value::<Plan>(json).unwrap();
        let drawn = |extra: serde_json::Value| {
            let mut floor = json!({ "id": "rdc", "name": "Rez-de-chaussée", "size": [900, 1200],
                "walls": [{ "x": 0, "y": 0, "w": 900, "h": 20 }],
                "rooms": [{ "id": "salon", "name": "Salon", "kind": "living", "room": "Salon",
                            "poly": [[20, 20], [500, 20], [500, 400], [20, 400]] }],
                "openings": [{ "kind": "window", "x": 100, "y": 0, "w": 120, "h": 20 }],
                "items": [{ "kind": "sofa", "x": 50, "y": 50, "w": 200, "h": 90, "r": 90 }],
                "devices": [{ "id": "hue:1", "x": 250, "y": 200 }] });
            if let (Some(f), Some(e)) = (floor.as_object_mut(), extra.as_object()) {
                for (k, v) in e {
                    f.insert(k.clone(), v.clone());
                }
            }
            plan(json!({ "skin": "blueprint", "floors": [floor] }))
        };
        assert_eq!(drawn(json!({})).check(&dir), Ok(()));
        // Devices in cm on a drawing: 250 is fine there, not on an image.
        assert!(
            drawn(json!({ "devices": [{ "id": "a", "x": 950, "y": 10 }] }))
                .check(&dir)
                .is_err()
        );
        assert!(drawn(json!({ "size": [0, 10] })).check(&dir).is_err());
        assert_eq!(
            drawn(json!({ "elevation": 280, "offset": [55, 728] })).check(&dir),
            Ok(())
        );
        assert!(drawn(json!({ "elevation": 1e9 })).check(&dir).is_err());
        assert_eq!(
            drawn(
                json!({ "height": 150, "roof": { "ridge": "x", "rise": 160 },
                "walls": [{ "x": 0, "y": 0, "w": 900, "h": 20, "r": -4.4, "height": 100 }] })
            )
            .check(&dir),
            Ok(())
        );
        assert!(
            drawn(json!({ "roof": { "ridge": "z", "rise": 160 } }))
                .check(&dir)
                .is_err()
        );
        assert!(drawn(json!({ "height": 0 })).check(&dir).is_err());
        assert!(
            drawn(json!({ "walls": [{ "x": 0, "y": 0, "w": 9, "h": 9, "height": -1 }] }))
                .check(&dir)
                .is_err()
        );
        assert!(
            drawn(json!({ "items": [{ "kind": "piano", "x": 1, "y": 1, "w": 1, "h": 1 }] }))
                .check(&dir)
                .is_err()
        );
        assert!(
            drawn(json!({ "openings": [{ "x": 1, "y": 1, "w": 1, "h": 1 }] }))
                .check(&dir)
                .is_err(),
            "an opening says what it is"
        );
        assert!(
            drawn(json!({ "walls": [{ "x": 1, "y": 1, "w": -5, "h": 1 }] }))
                .check(&dir)
                .is_err()
        );
        assert!(
            drawn(json!({ "walls": [{ "x": 5000, "y": 1, "w": 5, "h": 1 }] }))
                .check(&dir)
                .is_err(),
            "inside the floor"
        );
        assert!(drawn(json!({ "rooms": [{ "id": "x", "name": "X", "kind": "living", "poly": [[0, 0], [1, 1]] }] })).check(&dir).is_err());
        let mut skin = drawn(json!({}));
        skin.skin = Some("néon".into());
        assert!(skin.check(&dir).is_err());
        let shapes_without_size = plan(json!({ "floors": [{ "id": "a", "name": "A",
            "walls": [{ "x": 0, "y": 0, "w": 1, "h": 1 }] }] }));
        assert!(shapes_without_size.check(&dir).is_err());
    }
}
