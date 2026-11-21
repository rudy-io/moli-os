//! The Moli app's own ways in, beside the dashboard it shows.
//!
//! - Its updates, with the phone's token (under `/api/phones`, which a house
//!   behind Cloudflare Access lets through unsigned):
//!   `GET /api/phones/<id>/app` → what the house holds (`{ version_code,
//!   version_name, size, sha256 }`, 404 when nothing), and
//!   `GET /api/phones/<id>/app.apk` → the file. The house keeps them in its
//!   data folder: `app/android.json` and `app/moli.apk`, written by
//!   `mobile/publish-android.sh`.
//! - The Google sign-in handoff. Google refuses to sign in inside an app's
//!   web view, so the app opens Cloudflare Access's sign-in in a browser tab
//!   of its own; the session then lives in that browser, not in the app.
//!   The tab ends on `GET /api/mobile/handoff?challenge=…` (behind Access:
//!   the person is known), which keeps the Access token two minutes under a
//!   one-time code and sends the tab back to the app (`moli://handoff?code=…`,
//!   the only way back accepted).
//!   The app trades the code, with the secret its challenge was made of,
//!   for that token (`POST /api/phones/handoff`) and sets it in its web view.
//!   The token never travels in a link: an app catching the return finds a
//!   code it cannot use without the secret (PKCE, RFC 7636).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::json;

use crate::caller::{Caller, Via};
use crate::phones::PhonesGate;

/// The house's data folder (`app/` holds the app's update).
#[derive(Clone)]
pub(crate) struct AppDir(pub(crate) Option<PathBuf>);

/// How long a handoff code is good for.
const HANDOFF_MS: u64 = 120_000;
/// Codes kept at once (a browser tab opened again and again).
const HANDOFF_MAX: usize = 32;

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
}

fn b64url(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(T[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

/// Why the phone named in the path may not (its token is not its own, or
/// it is no longer of the house): the answer to give; `None`: it may.
fn refused(gate: Option<&moli_phones::Gateway>, id: &str, headers: &HeaderMap) -> Option<Response> {
    let Some(gate) = gate else {
        return Some(error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.pilote.absent", pilote = "phones"),
        ));
    };
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !gate.verify(id, token.trim()) {
        return Some(error(
            StatusCode::UNAUTHORIZED,
            &moli_i18n::tr!("serveur.telephone.jeton_refuse"),
        ));
    }
    if !gate.belongs(id) {
        return Some(error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("pilotes.phones.hors_maison"),
        ));
    }
    None
}

/// What `mobile/publish-android.sh` writes next to the APK.
#[derive(Deserialize)]
struct Manifest {
    version_code: u64,
    #[serde(default)]
    version_name: String,
    sha256: String,
}

fn manifest(dir: &std::path::Path) -> Option<(Manifest, u64)> {
    let raw = std::fs::read(dir.join("app/android.json")).ok()?;
    let m: Manifest = serde_json::from_slice(&raw).ok()?;
    let size = std::fs::metadata(dir.join("app/moli.apk")).ok()?.len();
    let hex = m.sha256.len() == 64 && m.sha256.bytes().all(|b| b.is_ascii_hexdigit());
    (hex && m.version_code > 0 && size > 0).then_some((m, size))
}

/// What the house holds of the app (Android).
pub(crate) async fn latest(
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    Extension(AppDir(dir)): Extension<AppDir>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = refused(gate.as_ref(), &id, &headers) {
        return r;
    }
    match dir.as_deref().and_then(manifest) {
        Some((m, size)) => axum::Json(json!({
            "version_code": m.version_code,
            "version_name": m.version_name,
            "size": size,
            "sha256": m.sha256.to_ascii_lowercase(),
        }))
        .into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.appli.aucune"),
        ),
    }
}

/// The app's file (Android).
pub(crate) async fn apk(
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    Extension(AppDir(dir)): Extension<AppDir>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = refused(gate.as_ref(), &id, &headers) {
        return r;
    }
    let Some((_, size)) = dir.as_deref().and_then(manifest) else {
        return error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.appli.aucune"),
        );
    };
    let Some(path) = dir.map(|d| d.join("app/moli.apk")) else {
        return error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.appli.aucune"),
        );
    };
    match tokio::fs::File::open(&path).await {
        Ok(file) => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/vnd.android.package-archive"),
                ),
                (header::CONTENT_LENGTH, HeaderValue::from(size)),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            Body::from_stream(tokio_util::io::ReaderStream::new(file)),
        )
            .into_response(),
        Err(_) => error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.appli.aucune"),
        ),
    }
}

/// A sign-in waiting for its app.
struct Pending {
    challenge: String,
    token: String,
    expires_ms: u64,
}

/// The handoffs under way, by code.
#[derive(Default)]
pub(crate) struct Handoffs(Mutex<HashMap<String, Pending>>);

impl Handoffs {
    fn keep(&self, code: String, pending: Pending, now: u64) {
        let mut all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        all.retain(|_, p| p.expires_ms > now);
        if all.len() >= HANDOFF_MAX
            && let Some(oldest) = all
                .iter()
                .min_by_key(|(_, p)| p.expires_ms)
                .map(|(c, _)| c.clone())
        {
            all.remove(&oldest);
        }
        all.insert(code, pending);
    }

    /// The token, once: right code, right secret, in time.
    fn take(&self, code: &str, verifier: &str, now: u64) -> Option<String> {
        let pending = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(code)?;
        let challenge = b64url(digest(&SHA256, verifier.as_bytes()).as_ref());
        (pending.expires_ms > now && challenge == pending.challenge).then_some(pending.token)
    }
}

#[derive(Deserialize)]
pub(crate) struct Start {
    challenge: String,
    #[serde(default)]
    back: Option<String>,
}

/// The only ways back to the app, a closed list: the app's own scheme
/// (`scheme` of `mobile/app.json`, the same whatever the publisher's
/// `bundleId`). Any other scheme could be an app on the same phone opening
/// this page itself, with its own challenge, to catch the code.
const BACKS: [&str; 1] = ["moli://handoff"];

fn back_ok(back: &str) -> bool {
    BACKS.contains(&back)
}

/// The end of the sign-in in the app's browser tab: back to the app with a
/// one-time code.
pub(crate) async fn start(
    Extension(handoffs): Extension<Arc<Handoffs>>,
    caller: Caller,
    headers: HeaderMap,
    Query(q): Query<Start>,
) -> Response {
    // Only through Access, for a person of the house: the token to hand
    // over is the one Access just signed for them.
    let token = headers
        .get("cf-access-jwt-assertion")
        .and_then(|v| v.to_str().ok())
        .filter(|t| !t.is_empty() && t.len() <= 8192);
    let person = caller.person.as_ref().filter(|p| !p.is_guest());
    let (Via::Access { .. }, Some(token), Some(_)) = (&caller.via, token, person) else {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.appli.connexion_access"),
        );
    };
    let back = q.back.as_deref().unwrap_or("moli://handoff");
    let challenge_ok = q.challenge.len() == 43
        && q.challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !challenge_ok || !back_ok(back) {
        return error(
            StatusCode::BAD_REQUEST,
            &moli_i18n::tr!("serveur.appli.lien_invalide"),
        );
    }
    let mut raw = [0u8; 32];
    if SystemRandom::new().fill(&mut raw).is_err() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "random");
    }
    let code = b64url(&raw);
    let now = moli_core::now_ms();
    handoffs.keep(
        code.clone(),
        Pending {
            challenge: q.challenge,
            token: token.to_owned(),
            expires_ms: now + HANDOFF_MS,
        },
        now,
    );
    // Both made of safe characters only (checked above, base64url).
    let target = format!("{back}?code={code}");
    let page = format!(
        r#"<!doctype html><html lang="fr"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="refresh" content="0;url={target}">
<title>Moli</title>
<style>body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#161512;color:#f4f1ea;font:16px system-ui,sans-serif;text-align:center}}
a{{display:inline-block;margin-top:18px;padding:14px 26px;border-radius:999px;background:#e8b931;color:#161512;font-weight:650;text-decoration:none}}</style>
</head><body><main><p>{done}</p><a href="{target}">{again}</a></main>
<script>location.replace({target_js});</script></body></html>"#,
        done = moli_i18n::tr!("serveur.appli.connexion_faite"),
        again = moli_i18n::tr!("serveur.appli.revenir"),
        target_js = serde_json::to_string(&target).unwrap_or_default(),
    );
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Html(page),
    )
        .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Trade {
    code: String,
    verifier: String,
}

/// The app trades its code and secret for the Access token.
pub(crate) async fn trade(
    Extension(handoffs): Extension<Arc<Handoffs>>,
    axum::Json(t): axum::Json<Trade>,
) -> Response {
    // RFC 7636: a verifier of 43 to 128 characters.
    if !(43..=128).contains(&t.verifier.len()) || t.code.len() > 64 {
        return error(
            StatusCode::BAD_REQUEST,
            &moli_i18n::tr!("serveur.appli.lien_invalide"),
        );
    }
    match handoffs.take(&t.code, &t.verifier, moli_core::now_ms()) {
        Some(token) => (
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            axum::Json(json!({ "token": token, "expires": expiry(&token) })),
        )
            .into_response(),
        None => error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.appli.code_refuse"),
        ),
    }
}

/// When the token ends (seconds), for the cookie the app sets.
fn expiry(token: &str) -> Option<u64> {
    #[derive(Deserialize)]
    struct Claims {
        exp: u64,
    }
    let claims = token.split('.').nth(1)?;
    serde_json::from_slice::<Claims>(&crate::access::b64url(claims)?)
        .ok()
        .map(|c| c.exp)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    // RFC 7636, appendix B.
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn pending(expires_ms: u64) -> Pending {
        Pending {
            challenge: CHALLENGE.into(),
            token: "t".into(),
            expires_ms,
        }
    }

    #[test]
    fn the_challenge_is_rfc_7636s() {
        assert_eq!(
            b64url(digest(&SHA256, VERIFIER.as_bytes()).as_ref()),
            CHALLENGE
        );
    }

    #[test]
    fn a_code_is_traded_once_with_its_secret_in_time() {
        let h = Handoffs::default();
        h.keep("c".into(), pending(1_000), 0);
        assert_eq!(
            h.take("c", "another-secret-of-forty-three-characters-xx", 10),
            None
        );
        // A wrong secret burns the code too.
        assert_eq!(h.take("c", VERIFIER, 10), None);
        h.keep("c".into(), pending(1_000), 0);
        assert_eq!(h.take("c", VERIFIER, 10).as_deref(), Some("t"));
        assert_eq!(h.take("c", VERIFIER, 10), None);
        h.keep("late".into(), pending(1_000), 0);
        assert_eq!(h.take("late", VERIFIER, 2_000), None);
    }

    #[test]
    fn only_an_apps_scheme_is_a_way_back() {
        assert!(back_ok("moli://handoff"));
        for bad in [
            // Another app's scheme, even a well-formed one: it could catch the code.
            "fr.memslab.moli://handoff",
            "evil://handoff",
            "https://handoff",
            "javascript://handoff",
            "moli://elsewhere",
            "Moli://handoff",
            "moli://handoff/x",
            "://handoff",
        ] {
            assert!(!back_ok(bad), "{bad}");
        }
    }

    #[test]
    fn the_expiry_is_read_from_the_token() {
        let claims = b64url(br#"{"exp":1795201887}"#);
        assert_eq!(expiry(&format!("h.{claims}.s")), Some(1_795_201_887));
        assert_eq!(expiry("nonsense"), None);
    }
}
