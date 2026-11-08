//! Linking a Ring account (crate `moli-ring`), in two one-way steps.
//!
//! 1. `POST /api/ring/link` — Ring's backend sends the one-time
//!    authorization code, form-encoded (not JSON: with JSON parsing,
//!    `code` comes back empty and the link dies without an obvious error).
//!    Moli exchanges it at Ring's token endpoint, reads the account id
//!    (`GET /v1/users/me`) and holds the token pair in memory, unclaimed.
//!    The route is public: the code is single-use, expires after 60 s and
//!    is useless without the `client_secret` in the vault.
//! 2. `GET /api/ring/linked?nonce=…&time=…` — the Ring app redirects the
//!    browser here. The nonce is Ring's HMAC-SHA256 over
//!    `"<time ms>:<account id>"` keyed with the partner's HMAC key,
//!    URL-safe base64 without padding, valid for 600 s exactly. A human
//!    session (PIN) is required before any token moves: on a match, the
//!    integration is confirmed with Ring (POST, then the mandatory PATCH)
//!    and the refresh token is filed in the vault; the driver picks it up
//!    at its next start.
//!
//! Both routes answer with a self-contained page (or JSON for the backend):
//! a public URL, never the dashboard.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use http_body_util::Full;
use moli_core::{InstanceId, now_ms};
use moli_net::Body as Bytes;
use moli_runtime::Hub;
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::caller::Caller;

/// Ring's cloud (JSON:API) and the token endpoint, on its own host.
const API_HOST: &str = "api.amazonvision.com";
const TOKEN_HOST: &str = "oauth.ring.com";
const TOKEN_PATH: &str = "/oauth/token";
/// The two headers Ring checks on every call, token endpoint included.
const USER_AGENT: &str = "moli-os/0.1";
const HARDWARE_ID_HEADER: &str = "Hardware_Id";
const LIMIT: Duration = Duration::from_secs(20);
const MAX_BODY: usize = 512 * 1024;
/// Ring's signature window, in milliseconds: 600 exactly.
const NONCE_WINDOW_MS: u64 = 600_000;
/// The secret store's keys, per driver instance.
const CLIENT_ID: &str = "client_id";
const CLIENT_SECRET: &str = "client_secret";
const HMAC_KEY: &str = "hmac_key";
const REFRESH: &str = "refresh_token";
const HARDWARE_ID: &str = "hardware_id";

/// A token pair Ring sent for an account nobody confirmed yet.
#[derive(Clone, Debug)]
pub struct Claim {
    pub account_id: String,
    pub access_token: String,
    pub refresh_token: String,
}

/// The unclaimed Ring token pairs, pending the person's confirmation.
#[derive(Clone)]
pub struct RingGate {
    instance: InstanceId,
    claims: Arc<Mutex<BTreeMap<String, Claim>>>,
}

impl fmt::Debug for RingGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let claims = self.claims.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("RingGate")
            .field("instance", &self.instance)
            .field("unclaimed", &claims.len())
            .finish()
    }
}

impl RingGate {
    /// The instance the claims belong to (its vault namespacing).
    #[must_use]
    pub fn new(instance: InstanceId) -> Self {
        Self {
            instance,
            claims: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Files (or replaces) an account's pair: re-linking the same account
    /// reissues a code, so the newest claim wins.
    pub fn store(&self, claim: Claim) {
        self.claims
            .lock()
            .expect("the claims map is not poisoned")
            .insert(claim.account_id.clone(), claim);
    }

    /// The claim whose nonce, signed by the partner's HMAC key, matches
    /// `nonce` at `time_ms` — compared in constant time against every
    /// unclaimed account — then removed (a claim is single-use).
    pub fn take_matching(&self, key: &[u8], nonce: &str, time_ms: u64) -> Option<Claim> {
        let mut claims = self.claims.lock().expect("the claims map is not poisoned");
        let mut found = None;
        for account in claims.keys() {
            let expected = nonce_for(key, time_ms, account);
            if constant_time_eq(nonce.as_bytes(), expected.as_bytes()) {
                found = Some(account.clone());
            }
        }
        found.and_then(|account| claims.remove(&account))
    }
}

/// Ring's backend → Moli: the one-time authorization code, form-encoded.
pub(crate) async fn link(
    State(hub): State<Hub>,
    Extension(gate): Extension<Option<RingGate>>,
    body: axum::body::Bytes,
) -> Response {
    let Some(gate) = gate else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("pilotes.ring.cles_manquantes_api"),
        );
    };
    let form = parse_form(&body);
    let Some(code) = form.get("code").filter(|c| !c.is_empty()).cloned() else {
        return json_error(
            StatusCode::BAD_REQUEST,
            &moli_i18n::tr!("pilotes.ring.code_manquant"),
        );
    };
    let Some(client_id) = hub.secret(&gate.instance, CLIENT_ID) else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("pilotes.ring.cles_manquantes_api"),
        );
    };
    let Some(client_secret) = hub.secret(&gate.instance, CLIENT_SECRET) else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("pilotes.ring.cles_manquantes_api"),
        );
    };
    // Ring's own app id, cross-checked with the vault: a code delivered by
    // another partner's backend is refused here, not at Ring.
    if let Some(id) = form.get("client_id")
        && !id.is_empty()
        && *id != client_id
    {
        return json_error(
            StatusCode::UNAUTHORIZED,
            &moli_i18n::tr!("pilotes.ring.client_inconnu"),
        );
    }
    let hardware_id = match ensure_hardware_id(&hub, &gate.instance).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "ring: cannot keep the hardware id");
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                &moli_i18n::tr!("pilotes.ring.pas_de_coffre"),
            );
        }
    };
    let exchange = exchange_code(&client_id, &client_secret, &code, &hardware_id).await;
    match exchange {
        Ok((access, refresh)) => match ring_account_id(&access, &hardware_id).await {
            Ok(account_id) => {
                gate.store(Claim {
                    account_id: account_id.clone(),
                    access_token: access,
                    refresh_token: refresh,
                });
                tracing::info!(
                    instance = %gate.instance,
                    %account_id,
                    "ring: a token pair waits unclaimed for the account link"
                );
                Json(json!({ "ok": true })).into_response()
            }
            Err(e) => {
                tracing::warn!(error = %e, "ring: the account could not be read");
                json_error(
                    StatusCode::BAD_GATEWAY,
                    &moli_i18n::tr!("pilotes.ring.echec_echange"),
                )
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "ring: the code exchange failed");
            json_error(
                StatusCode::BAD_GATEWAY,
                &moli_i18n::tr!("pilotes.ring.echec_echange"),
            )
        }
    }
}

/// The browser → Moli: the Ring app's redirect, carrying the signed nonce.
/// A human session (PIN) is required before any token moves.
pub(crate) async fn linked(
    State(hub): State<Hub>,
    Extension(gate): Extension<Option<RingGate>>,
    Extension(humans): Extension<Arc<crate::session::Sessions>>,
    caller: Caller,
    headers: header::HeaderMap,
    Query(query): Query<LinkQuery>,
) -> Response {
    if !humans.is_human(&headers, &caller.key) {
        return page(
            StatusCode::UNAUTHORIZED,
            "Moli – Ring",
            &moli_i18n::tr!("pilotes.ring.connecte_dabord"),
        );
    }
    let Some(gate) = gate else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Moli – Ring",
            &moli_i18n::tr!("pilotes.ring.cles_manquantes_api"),
        );
    };
    let claim = match take_claim(&hub, &gate, &query) {
        Ok(claim) => claim,
        Err(error) => return claim_error_page(&error),
    };
    let hardware_id = match ensure_hardware_id(&hub, &gate.instance).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "ring: cannot keep the hardware id");
            return page(
                StatusCode::SERVICE_UNAVAILABLE,
                "Moli – Ring",
                &moli_i18n::tr!("pilotes.ring.pas_de_coffre"),
            );
        }
    };
    let nonce = query.nonce.as_deref().unwrap_or_default();
    if let Err(e) = confirm_integration(
        &claim.access_token,
        &hardware_id,
        nonce,
        account_identifier(&caller).as_deref(),
    )
    .await
    {
        tracing::warn!(
            error = %e,
            account = %claim.account_id,
            "ring: the integration could not be confirmed"
        );
        return page(
            StatusCode::BAD_GATEWAY,
            "Moli – Ring",
            &moli_i18n::tr!("pilotes.ring.echec_lien"),
        );
    }
    if let Err(e) = hub
        .store_secret(&gate.instance, REFRESH, &claim.refresh_token)
        .await
    {
        tracing::error!(error = %e, "ring: the refresh token could not be filed");
        return page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Moli – Ring",
            &moli_i18n::tr!("pilotes.ring.pas_de_coffre"),
        );
    }
    tracing::info!(instance = %gate.instance, account = %claim.account_id, "ring account linked");
    hub.notice(moli_core::Notice {
        title: Some(moli_i18n::tr!("pilotes.ring.lie_titre")),
        message: moli_i18n::tr!("pilotes.ring.lie"),
        from: Some("Moli".into()),
        ts: now_ms(),
        to: None,
    });
    page(
        StatusCode::OK,
        &moli_i18n::tr!("pilotes.ring.lie_titre"),
        &moli_i18n::tr!("pilotes.ring.lie"),
    )
}

/// Why the redirect's signed nonce could not claim an account.
enum ClaimError {
    /// The nonce or its timestamp is missing.
    Invalid,
    /// The timestamp is outside Ring's 600 s window.
    Expired,
    /// No unclaimed account signed this nonce.
    Unknown,
    /// The partner's HMAC key is not (yet) in the vault.
    NoKeys,
}

/// The page for a claim that could not be taken.
fn claim_error_page(error: &ClaimError) -> Response {
    let (status, key) = match error {
        ClaimError::Invalid => (StatusCode::BAD_REQUEST, "pilotes.ring.lien_invalide"),
        ClaimError::Expired => (StatusCode::GONE, "pilotes.ring.lien_expire"),
        ClaimError::Unknown => (StatusCode::GONE, "pilotes.ring.nonce_inconnu"),
        ClaimError::NoKeys => (
            StatusCode::SERVICE_UNAVAILABLE,
            "pilotes.ring.cles_manquantes_api",
        ),
    };
    page(status, "Moli – Ring", &moli_i18n::tr!(key))
}

/// The unclaimed pair for the redirect's signed nonce: a missing nonce or
/// timestamp is invalid, a stale or future timestamp is expired, and the
/// nonce is compared in constant time against every unclaimed account.
fn take_claim(hub: &Hub, gate: &RingGate, query: &LinkQuery) -> Result<Claim, ClaimError> {
    let Some(nonce) = query.nonce.as_deref().filter(|n| !n.is_empty()) else {
        return Err(ClaimError::Invalid);
    };
    let Some(time) = query.time else {
        return Err(ClaimError::Invalid);
    };
    let now = now_ms();
    // The window is 600 s exactly; a future time is not Ring's.
    if time > now || now - time > NONCE_WINDOW_MS {
        return Err(ClaimError::Expired);
    }
    let Some(key) = hub.secret(&gate.instance, HMAC_KEY) else {
        return Err(ClaimError::NoKeys);
    };
    if let Some(claim) = gate.take_matching(key.as_bytes(), nonce, time) {
        Ok(claim)
    } else {
        tracing::info!(instance = %gate.instance, "ring: a link nonce nobody signed");
        Err(ClaimError::Unknown)
    }
}

/// The query of the account-link redirect (`?nonce=…&time=…`, time in ms).
#[derive(Debug, Deserialize)]
pub(crate) struct LinkQuery {
    nonce: Option<String>,
    time: Option<u64>,
}

/// Ring's optional `account_identifier`: an obfuscated view of the person
/// signed in — the initial of their name, or of their Access e-mail, never
/// a full value. Nothing, when no one is known.
fn account_identifier(caller: &Caller) -> Option<String> {
    if let Some(person) = &caller.person {
        return Some(mask_name(&person.name));
    }
    match &caller.via {
        crate::caller::Via::Access { email } => Some(mask_email(email)),
        _ => None,
    }
}

fn mask_name(name: &str) -> String {
    name.chars()
        .next()
        .map_or_else(|| "***".to_owned(), |first| format!("{first}***"))
}

fn mask_email(email: &str) -> String {
    email.split_once('@').map_or_else(
        || "*****".to_owned(),
        |(local, domain)| {
            local.chars().next().map_or_else(
                || format!("*****@{domain}"),
                |first| format!("{first}***@{domain}"),
            )
        },
    )
}

/// The instance's hardware id, read from the vault or generated and filed
/// there. The driver does the same before its first call: either way the
/// value is one stable identifier per instance.
async fn ensure_hardware_id(hub: &Hub, instance: &InstanceId) -> anyhow::Result<String> {
    if let Some(id) = hub
        .secret(instance, HARDWARE_ID)
        .filter(|id| !id.trim().is_empty())
    {
        return Ok(id);
    }
    let id = uuid_v4();
    hub.store_secret(instance, HARDWARE_ID, &id).await?;
    Ok(id)
}

/// A request to Ring's clouds, with the two headers Ring checks on every
/// call; `access` when the session is linked.
fn request(
    method: Method,
    host: &str,
    path: &str,
    access: Option<&str>,
    hardware_id: &str,
    payload: Option<(&'static str, String)>,
) -> anyhow::Result<axum::http::Request<Full<Bytes>>> {
    let mut builder = axum::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("User-Agent", USER_AGENT)
        .header(HARDWARE_ID_HEADER, hardware_id);
    if let Some(access) = access {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {access}"));
    }
    let payload = match payload {
        Some((content_type, body)) => {
            builder = builder.header(header::CONTENT_TYPE, content_type);
            Some(body)
        }
        None => None,
    };
    Ok(builder.body(Full::new(payload.map_or_else(Bytes::new, Bytes::from)))?)
}

/// The words of a refusal (JSON:API `errors[].title`, or the token
/// endpoint's `error`): logged, never echoed with a token.
fn api_error(status: StatusCode, body: &Bytes) -> String {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return status.to_string();
    };
    if let Some(title) = value
        .get("errors")
        .and_then(|errors| errors.get(0))
        .and_then(|error| error.get("title"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        return format!("{status} : {title}");
    }
    for field in ["error_description", "error"] {
        if let Some(words) = value
            .get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return format!("{status} : {words}");
        }
    }
    status.to_string()
}

/// `POST /oauth/token`: the one-time code becomes a token pair. The
/// refresh token rotates with every exchange: it is the one filed.
async fn exchange_code(
    client_id: &str,
    client_secret: &str,
    code: &str,
    hardware_id: &str,
) -> anyhow::Result<(String, String)> {
    let form = format!(
        "grant_type=authorization_code&code={}&client_id={}&client_secret={}",
        form_escape(code),
        form_escape(client_id),
        form_escape(client_secret),
    );
    let request = request(
        Method::POST,
        TOKEN_HOST,
        TOKEN_PATH,
        None,
        hardware_id,
        Some(("application/x-www-form-urlencoded", form)),
    )?;
    let (status, body) = moli_net::web(TOKEN_HOST, 443, true, request, LIMIT, MAX_BODY).await?;
    if status != StatusCode::OK {
        anyhow::bail!("ring token endpoint: {}", api_error(status, &body));
    }
    let value: Value = serde_json::from_slice(&body)?;
    let access = token_field(&value, "access_token")?;
    let refresh = token_field(&value, "refresh_token")?;
    Ok((access, refresh))
}

fn token_field(value: &Value, field: &str) -> anyhow::Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("ring token endpoint: no {field}"))
}

/// `GET /v1/users/me` → `data.id`, the account id (`ava1.ring.account…`),
/// never to be trusted to the browser.
async fn ring_account_id(access: &str, hardware_id: &str) -> anyhow::Result<String> {
    let request = request(
        Method::GET,
        API_HOST,
        "/v1/users/me",
        Some(access),
        hardware_id,
        None,
    )?;
    let (status, body) = moli_net::web(API_HOST, 443, true, request, LIMIT, MAX_BODY).await?;
    if !status.is_success() {
        anyhow::bail!("ring users/me: {}", api_error(status, &body));
    }
    let value: Value = serde_json::from_slice(&body)?;
    value
        .get("data")
        .and_then(|data| data.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("ring users/me: no account id"))
}

/// Ring's two mandatory calls: the POST confirms the link (the nonce),
/// the PATCH closes it (`status: completed`). Both must succeed.
async fn confirm_integration(
    access: &str,
    hardware_id: &str,
    nonce: &str,
    identifier: Option<&str>,
) -> anyhow::Result<()> {
    const PATH: &str = "/v1/accounts/me/app-integrations";
    let mut body = serde_json::Map::new();
    body.insert("nonce".to_owned(), Value::String(nonce.to_owned()));
    if let Some(id) = identifier {
        body.insert(
            "account_identifier".to_owned(),
            Value::String(id.to_owned()),
        );
    }
    let posted = request(
        Method::POST,
        API_HOST,
        PATH,
        Some(access),
        hardware_id,
        Some(("application/json", Value::Object(body).to_string())),
    )?;
    let (status, body) = moli_net::web(API_HOST, 443, true, posted, LIMIT, MAX_BODY).await?;
    if !status.is_success() {
        anyhow::bail!(
            "ring app-integrations (confirm): {}",
            api_error(status, &body)
        );
    }
    let patched = request(
        Method::PATCH,
        API_HOST,
        PATH,
        Some(access),
        hardware_id,
        Some((
            "application/json",
            json!({ "status": "completed" }).to_string(),
        )),
    )?;
    let (status, body) = moli_net::web(API_HOST, 443, true, patched, LIMIT, MAX_BODY).await?;
    if !status.is_success() {
        anyhow::bail!(
            "ring app-integrations (completed): {}",
            api_error(status, &body)
        );
    }
    Ok(())
}

/// Ring's nonce: HMAC-SHA256 over `"<time ms>:<account id>"`, URL-safe
/// base64, without padding (RFC 4648 §5).
fn nonce_for(key: &[u8], time_ms: u64, account_id: &str) -> String {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mac = hmac::sign(&k, format!("{time_ms}:{account_id}").as_bytes());
    b64url_no_padding(mac.as_ref())
}

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64url_no_padding(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL[(n >> 18) as usize] as char);
        out.push(B64URL[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64URL[((n >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[(n & 0x3f) as usize] as char);
        }
    }
    out
}

/// Byte-by-byte compare without a position-revealing early exit; the
/// lengths are not secret, so the length check stands outside.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `application/x-www-form-urlencoded` body, read by field name: Ring's
/// token-exchange POST is a form, not JSON.
fn parse_form(body: &Bytes) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(body);
    let mut fields = HashMap::new();
    for pair in text.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        fields.insert(percent_decode(key), percent_decode(value));
    }
    fields
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    out.push(hi * 16 + lo);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `application/x-www-form-urlencoded` value: safe characters as-is, the
/// rest `%XX` (uppercase), a space as `+`.
fn form_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// The stable `Hardware_Id` Ring tracks, per Moli instance (a UUID v4).
fn uuid_v4() -> String {
    let mut bytes = [0_u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("the system RNG failed");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = |bytes: &[u8]| {
        bytes.iter().fold(String::new(), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
    };
    format!(
        "{}-{}-{}-{}-{}",
        hex(&bytes[0..4]),
        hex(&bytes[4..6]),
        hex(&bytes[6..8]),
        hex(&bytes[8..10]),
        hex(&bytes[10..16])
    )
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// The page the browser lands on: it says only the outcome, in the
/// house's language, and links nowhere (a public URL, never the dashboard).
fn page(status: StatusCode, title: &str, message: &str) -> Response {
    let title = escape_html(title);
    let message = escape_html(message);
    let html = format!(
        r#"<!doctype html>
<html lang="{lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
body {{ margin: 0; min-height: 100vh; display: grid; place-content: center;
       background: #171a1f; color: #e9e6df; font: 16px/1.6 system-ui, sans-serif; }}
main {{ max-width: 32rem; padding: 2rem; text-align: center; }}
h1 {{ font-size: 1.25rem; margin-bottom: .5rem; }}
p {{ color: #a9a49a; }}
</style>
</head>
<body>
<main>
<h1>{title}</h1>
<p>{message}</p>
</main>
</body>
</html>
"#,
        lang = moli_i18n::language(),
        title = title,
        message = message,
    );
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// `&` first: escaping the others' `&` afterwards would double-escape.
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vectors verified independently (node:crypto, python hmac/base64).
    #[test]
    fn the_ring_nonce_vectors() {
        assert_eq!(
            nonce_for(
                b"test-hmac-key-123",
                1_787_176_751_000,
                "ava1.ring.account.TESTACCOUNT123"
            ),
            "gH20grCa9XtdlJ1o46FOFbxhitHt8IwU_Yb0n-o-EfU"
        );
        assert_eq!(
            nonce_for(b"k", 0, "ava1.ring.account.AAAA"),
            "IbbB90isLQiXZU7zzsRx5eacXf4dPl6M-5MvWwRh2yA"
        );
        assert_eq!(
            nonce_for(&[b'a'; 80], 1_700_000_000_000, "ava1.ring.account.BBBB"),
            "OvZTvHuvoEI9tTBP9ogbPN_cs8SsCLBiiAOpnPxZcvo"
        );
    }

    #[test]
    fn a_form_body_is_read_by_field_name() {
        let body = Bytes::from("code=abc123&client_id=cid-1&x=%C3%A9%20&empty=&only");
        let form = parse_form(&body);
        assert_eq!(form.get("code").map(String::as_str), Some("abc123"));
        assert_eq!(form.get("client_id").map(String::as_str), Some("cid-1"));
        assert_eq!(form.get("x").map(String::as_str), Some("é "));
        assert_eq!(form.get("empty").map(String::as_str), Some(""));
        assert_eq!(form.get("only").map(String::as_str), Some(""));
        assert_eq!(form.get("nope"), None);
    }

    #[test]
    fn percent_decoding_is_rough_on_bad_input() {
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%41%42"), "AB");
        assert_eq!(percent_decode("50%"), "50%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode(""), "");
    }

    #[test]
    fn form_escaping_keeps_safe_chars_and_uppercases() {
        assert_eq!(form_escape("a b&c=d"), "a+b%26c%3Dd");
        assert_eq!(form_escape("AZaz09-_.~"), "AZaz09-_.~");
        assert_eq!(form_escape("é"), "%C3%A9");
        let back = parse_form(&Bytes::from("k=v&k2=a+b"));
        assert_eq!(back.get("k").map(String::as_str), Some("v"));
        assert_eq!(back.get("k2").map(String::as_str), Some("a b"));
    }

    #[test]
    fn constant_time_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn a_hardware_id_is_a_uuid_v4() {
        let id = uuid_v4();
        let chars: Vec<char> = id.chars().collect();
        assert_eq!(chars.len(), 36);
        for (i, c) in chars.iter().enumerate() {
            let dash = matches!(i, 8 | 13 | 18 | 23);
            assert!(dash || c.is_ascii_hexdigit(), "{i}: {id}");
        }
        assert_eq!(chars[14], '4');
        assert!(matches!(chars[19], '8' | '9' | 'a' | 'b'));
    }

    #[test]
    fn base64url_without_padding() {
        assert_eq!(b64url_no_padding(b""), "");
        assert_eq!(b64url_no_padding(b"a"), "YQ");
        assert_eq!(b64url_no_padding(b"ab"), "YWI");
        assert_eq!(b64url_no_padding(b"abc"), "YWJj");
        assert_eq!(b64url_no_padding(b"\xfb\xff\xbf"), "-_-_");
    }

    #[test]
    fn html_escaping_runs_ampersand_first() {
        assert_eq!(
            escape_html("<a href=\"/x\">&</a>"),
            "&lt;a href=&quot;/x&quot;&gt;&amp;&lt;/a&gt;"
        );
    }

    #[test]
    fn a_claim_is_taken_once_by_its_signed_nonce() {
        let gate = RingGate::new(InstanceId::from("ring"));
        gate.store(Claim {
            account_id: "ava1.ring.account.TEST".to_owned(),
            access_token: "a1".to_owned(),
            refresh_token: "r1".to_owned(),
        });
        let time = 1_787_176_751_000;
        let nonce = nonce_for(b"key", time, "ava1.ring.account.TEST");
        let claim = gate
            .take_matching(b"key", &nonce, time)
            .expect("the signed nonce is taken");
        assert_eq!(claim.account_id, "ava1.ring.account.TEST");
        // Single-use: gone.
        assert!(gate.take_matching(b"key", &nonce, time).is_none());
        // Another key signs another nonce: refused.
        gate.store(Claim {
            account_id: "ava1.ring.account.TEST".to_owned(),
            access_token: "a2".to_owned(),
            refresh_token: "r2".to_owned(),
        });
        assert!(gate.take_matching(b"other-key", &nonce, time).is_none());
        // The same account, a newer claim, replaces the older one.
        let nonce2 = nonce_for(b"key", time, "ava1.ring.account.TEST");
        gate.store(Claim {
            account_id: "ava1.ring.account.TEST".to_owned(),
            access_token: "a3".to_owned(),
            refresh_token: "r3".to_owned(),
        });
        let claim = gate
            .take_matching(b"key", &nonce2, time)
            .expect("the newest claim wins");
        assert_eq!(claim.access_token, "a3");
    }
}
