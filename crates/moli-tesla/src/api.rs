//! Tesla's cloud API: the app's own OAuth (PKCE, the token endpoint at
//! `/oauth2/v3/token`), the vehicles, their data, and the orders.

use std::fmt::Write;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use http::{Method, Request, StatusCode};
use http_body_util::Full;
use moli_net::Body as Bytes;
use serde_json::Value as Json;

/// The refresh token has died (or the code was already spent): sign in again.
pub const REFRESH_INVALID: &str = "the Tesla refresh token is no longer valid";
/// An answer went out with an access token Tesla no longer accepts.
pub const TOKEN_EXPIRED: &str = "the Tesla access token expired";
/// The account is disabled by Tesla's billing limit: the Fleet API is paid
/// per use, and once the allowance is spent every call (vehicles, data,
/// commands, even partner registration) answers `403` with
/// `account disabled: EXCEEDED_LIMIT` until the limit is raised or the next
/// billing cycle begins.
pub const ACCOUNT_DISABLED: &str = "the Tesla account is disabled (billing limit exceeded)";

/// Tesla asked the driver to slow down (HTTP 429): `secs` is the `Retry-After`
/// it sent (a few minutes, sometimes hours), with a fallback when it did not.
#[derive(Debug)]
pub struct RateLimited {
    pub secs: u64,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tesla rate limit: wait {} s", self.secs)
    }
}

impl std::error::Error for RateLimited {}

/// The application's scopes: the set registered on developer.tesla.com,
/// asked for at the sign-in page and on the partner token alike.
pub const SCOPE: &str = "openid offline_access user_data vehicle_device_data vehicle_cmds";

const USER_AGENT: &str = "Moli OS";
/// The token endpoint is the only call that may take its time.
const AUTH: Duration = Duration::from_secs(10);
const READ: Duration = Duration::from_secs(15);
/// An order must reach the hub's 5 s deadline with room to spare.
const ORDER: Duration = Duration::from_millis(4_500);
const MAX_BODY: usize = 1024 * 1024;
const MAX_TOKEN: usize = 64 * 1024;

/// The account's credentials: the house's own application (developer.tesla.com)
/// from the secret store — the shared public client was retired, so the id is
/// always required; the secret only when Tesla gave one.
#[derive(Clone, Debug)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: Option<String>,
}

/// A usable pair: the access token (memory only, it expires) and the refresh
/// token (single-use: Tesla rotates it at every exchange, so the new one
/// must be kept in the secret store).
#[derive(Clone, Debug)]
pub struct Token {
    pub access: String,
    pub refresh: String,
    pub expires_at: Instant,
}

/// Unreserved characters pass, the rest percent-encoded (a form body and a
/// query share the rule).
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// `k=v&k=v`, form-encoded.
#[must_use]
pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", escape(k), escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url without padding (PKCE).
#[must_use]
pub fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        out.push(sext(n >> 18));
        out.push(sext(n >> 12));
        if chunk.len() > 1 {
            out.push(sext(n >> 6));
        }
        if chunk.len() > 2 {
            out.push(sext(n));
        }
    }
    out
}

/// One base64 character, from the low six bits.
fn sext(bits: u32) -> char {
    char::from(B64[usize::try_from(bits & 0x3F).expect("six bits fit")])
}

/// The PKCE challenge of a verifier (SHA-256, base64url).
#[must_use]
pub fn challenge_of(verifier: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    base64url(digest.as_ref())
}

/// `POST /oauth2/v3/token`, form-encoded.
async fn grant(host: &str, body: &str) -> anyhow::Result<Token> {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/oauth2/v3/token")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("user-agent", USER_AGENT)
        .body(Full::new(Bytes::from(body.to_owned())))
        .context("building the token request")?;
    let (status, bytes) = moli_net::web(host, 443, true, request, AUTH, MAX_TOKEN)
        .await
        .map_err(|e| {
            anyhow::anyhow!(moli_i18n::tr!(
                "pilotes.tesla.injoignable",
                cause = e.root_cause()
            ))
        })?;
    let answer: Json = serde_json::from_slice(&bytes).unwrap_or_default();
    if !status.is_success() {
        let kind = answer.get("error").and_then(Json::as_str).unwrap_or("");
        if kind == "invalid_grant" {
            bail!("{REFRESH_INVALID}");
        }
        let why = answer
            .get("error_description")
            .or_else(|| answer.get("error"))
            .and_then(Json::as_str)
            .unwrap_or("");
        bail!(
            "{} ({status}) {why}",
            moli_i18n::tr!("pilotes.tesla.point_refuse")
        );
    }
    let access = answer
        .get("access_token")
        .and_then(Json::as_str)
        .context("no access token in the answer")?;
    let refresh = answer
        .get("refresh_token")
        .and_then(Json::as_str)
        .context("no refresh token in the answer")?;
    let expires_in = answer
        .get("expires_in")
        .and_then(Json::as_i64)
        .unwrap_or(3600)
        .clamp(60, 12 * 3600);
    let expires_in = u64::try_from(expires_in).unwrap_or(3600);
    Ok(Token {
        access: access.to_owned(),
        refresh: refresh.to_owned(),
        expires_at: Instant::now() + Duration::from_secs(expires_in),
    })
}

/// The sign-in's code, exchanged for a token (PKCE); `redirect_uri` must be
/// the same callback address the sign-in page asked for, and `audience` the
/// regional Fleet API the token must target.
pub async fn code_for(
    host: &str,
    credentials: &Credentials,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
    audience: &str,
) -> anyhow::Result<Token> {
    let mut pairs: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("client_id", credentials.client_id.as_str()),
        ("code_verifier", code_verifier),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("audience", audience),
    ];
    if let Some(secret) = &credentials.client_secret {
        pairs.push(("client_secret", secret.as_str()));
    }
    grant(host, &form(&pairs)).await
}

/// The refresh grant. The refresh token is single-use: the answer rotates
/// it, and the new one is what must be kept.
pub async fn refresh(
    host: &str,
    credentials: &Credentials,
    refresh_token: &str,
) -> anyhow::Result<Token> {
    let mut pairs: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("client_id", credentials.client_id.as_str()),
        ("refresh_token", refresh_token),
    ];
    if let Some(secret) = &credentials.client_secret {
        pairs.push(("client_secret", secret.as_str()));
    }
    grant(host, &form(&pairs)).await
}

/// App activation: each application from developer.tesla.com must register
/// itself in the region it serves, or every endpoint answers 412 ("must be
/// registered in the current region"). Idempotent: re-registering an already
/// registered application is a no-op. The partner token is a
/// `client_credentials` grant (it needs the client secret), and `domain`
/// must be the root domain of the application's allowed origins.
pub async fn partner_register(
    auth_host: &str,
    api_host: &str,
    credentials: &Credentials,
    domain: &str,
) -> anyhow::Result<()> {
    let Some(secret) = &credentials.client_secret else {
        bail!("the application activation needs the client secret");
    };
    // 1) The partner token (client_credentials, for this region's Fleet API).
    let audience = format!("https://{api_host}");
    let pairs = form(&[
        ("grant_type", "client_credentials"),
        ("client_id", credentials.client_id.as_str()),
        ("client_secret", secret),
        ("scope", SCOPE),
        ("audience", audience.as_str()),
    ]);
    let request = Request::builder()
        .method(Method::POST)
        .uri("/oauth2/v3/token")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("user-agent", USER_AGENT)
        .body(Full::new(Bytes::from(pairs)))
        .context("building the partner token request")?;
    let (status, bytes) = moli_net::web(auth_host, 443, true, request, AUTH, MAX_TOKEN)
        .await
        .map_err(|e| {
            anyhow::anyhow!(moli_i18n::tr!(
                "pilotes.tesla.injoignable",
                cause = e.root_cause()
            ))
        })?;
    let answer: Json = serde_json::from_slice(&bytes).unwrap_or_default();
    if !status.is_success() {
        let why = answer
            .get("error_description")
            .or_else(|| answer.get("error"))
            .and_then(Json::as_str)
            .unwrap_or("");
        let raw: String = String::from_utf8_lossy(&bytes).chars().take(300).collect();
        bail!("the partner token was refused ({status}) (why={why}; raw={raw})");
    }
    let partner_token = answer
        .get("access_token")
        .and_then(Json::as_str)
        .context("no access token in the partner answer")?;
    // 2) Register the application under its domain in this region.
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/1/partner_accounts")
        .header("authorization", format!("Bearer {partner_token}"))
        .header("content-type", "application/json")
        .header("user-agent", USER_AGENT)
        .body(Full::new(Bytes::from(
            serde_json::json!({ "domain": domain }).to_string(),
        )))
        .context("building the registration request")?;
    let (status, _headers, bytes) =
        moli_net::web_with_headers(api_host, 443, true, request, READ, MAX_BODY)
            .await
            .map_err(|e| {
                anyhow::anyhow!(moli_i18n::tr!(
                    "pilotes.tesla.injoignable",
                    cause = e.root_cause()
                ))
            })?;
    let answer: Json = serde_json::from_slice(&bytes).unwrap_or_default();
    if !status.is_success() {
        let why = answer
            .get("error_description")
            .or_else(|| answer.get("error"))
            .and_then(Json::as_str)
            .unwrap_or("");
        let raw: String = String::from_utf8_lossy(&bytes).chars().take(300).collect();
        if billing_limit(status, &bytes) {
            bail!("{ACCOUNT_DISABLED} (HTTP {status}, raw={raw})");
        }
        if status == StatusCode::FAILED_DEPENDENCY {
            bail!(
                "the region refused the registration (424 Failed Dependency): the public key is probably not reachable at https://{domain}/.well-known/appspecific/com.tesla.3p.public-key.pem (why={why}; raw={raw})"
            );
        }
        if status == StatusCode::PRECONDITION_FAILED {
            bail!(
                "the region refused the registration (412 Precondition Failed): the domain {domain} must match an allowed origin of the application on developer.tesla.com, and the public key must be reachable at https://{domain}/.well-known/appspecific/com.tesla.3p.public-key.pem (why={why}; raw={raw})"
            );
        }
        bail!("the region refused the registration ({status}) (why={why}; raw={raw})");
    }
    Ok(())
}

/// The pause a 429 asked for, in seconds (`Retry-After`, with a fallback).
fn retry_after(headers: &http::HeaderMap) -> u64 {
    const FALLBACK_SECS: u64 = 15 * 60;
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(FALLBACK_SECS)
}

/// A 403 whose body carries `EXCEEDED_LIMIT`: the account is disabled by its
/// billing limit, not a single call that went too fast.
fn billing_limit(status: StatusCode, raw: &[u8]) -> bool {
    status == StatusCode::FORBIDDEN
        && std::str::from_utf8(raw).is_ok_and(|body| body.contains("EXCEEDED_LIMIT"))
}

/// A `GET` of the vehicles' API, bearer-authenticated.
async fn get(host: &str, token: &Token, uri: &str) -> anyhow::Result<Json> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("authorization", format!("Bearer {}", token.access))
        .header("user-agent", USER_AGENT)
        .body(Full::new(Bytes::new()))
        .context("building the request")?;
    let (status, headers, bytes) =
        moli_net::web_with_headers(host, 443, true, request, READ, MAX_BODY)
            .await
            .map_err(|e| {
                anyhow::anyhow!(moli_i18n::tr!(
                    "pilotes.tesla.injoignable",
                    cause = e.root_cause()
                ))
            })?;
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(anyhow::anyhow!(RateLimited {
            secs: retry_after(&headers)
        }));
    }
    if status == StatusCode::UNAUTHORIZED {
        bail!("{TOKEN_EXPIRED}");
    }
    let answer: Json = serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!(moli_i18n::tr!(
            "pilotes.tesla.sans_json",
            status = status.as_u16()
        ))
    })?;
    if !status.is_success() {
        // Prefer a non-empty reason: Tesla may send an empty
        // `error_description` alongside a populated `error`.
        let why = ["error_description", "error"]
            .iter()
            .filter_map(|k| answer.get(*k).and_then(Json::as_str))
            .find(|s| !s.is_empty())
            .unwrap_or_default();
        // The status and the raw body stay in the error: an answer without a
        // usable reason must not produce an empty, undecipherable error.
        let raw = String::from_utf8_lossy(&bytes)
            .chars()
            .take(200)
            .collect::<String>();
        if billing_limit(status, &bytes) {
            bail!("{ACCOUNT_DISABLED} (HTTP {status}, raw={raw})");
        }
        bail!("{why} (HTTP {status}, raw={raw})");
    }
    Ok(answer)
}

/// `GET /api/1/vehicles`.
pub async fn vehicles(host: &str, token: &Token) -> anyhow::Result<Json> {
    get(host, token, "/api/1/vehicles").await
}

/// `GET /api/1/vehicles/{id}/vehicle_data`.
pub async fn vehicle_data(host: &str, token: &Token, vehicle_id: &str) -> anyhow::Result<Json> {
    get(
        host,
        token,
        &format!("/api/1/vehicles/{vehicle_id}/vehicle_data"),
    )
    .await
}

/// `POST /api/1/vehicles/{id}/command/{cmd}`: the car's own answer — `result`, and
/// its `reason` when it refuses. An inner `Ok(Err(why))` is a refusal the
/// car explained (not a round-trip failure); a `401` is [`TOKEN_EXPIRED`].
pub async fn command(
    host: &str,
    token: &Token,
    vehicle_id: &str,
    cmd: &str,
    body: Option<&str>,
) -> anyhow::Result<Result<(), String>> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/1/vehicles/{vehicle_id}/command/{cmd}"))
        .header("authorization", format!("Bearer {}", token.access))
        .header("user-agent", USER_AGENT);
    let request = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        builder
            .body(Full::new(Bytes::from(body.to_owned())))
            .context("building the order")?
    } else {
        builder
            .body(Full::new(Bytes::new()))
            .context("building the order")?
    };
    let (status, headers, bytes) =
        moli_net::web_with_headers(host, 443, true, request, ORDER, MAX_TOKEN)
            .await
            .map_err(|e| {
                anyhow::anyhow!(moli_i18n::tr!(
                    "pilotes.tesla.injoignable",
                    cause = e.root_cause()
                ))
            })?;
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(anyhow::anyhow!(RateLimited {
            secs: retry_after(&headers)
        }));
    }
    if status == StatusCode::UNAUTHORIZED {
        bail!("{TOKEN_EXPIRED}");
    }
    let answer: Json = serde_json::from_slice(&bytes).unwrap_or_default();
    if !status.is_success() {
        if billing_limit(status, &bytes) {
            bail!("{ACCOUNT_DISABLED} (HTTP {status})");
        }
        let why = answer
            .get("error_description")
            .or_else(|| answer.get("error"))
            .and_then(Json::as_str)
            .unwrap_or("");
        bail!("{why}");
    }
    let response = &answer["response"];
    if response.get("result").and_then(Json::as_bool) == Some(true) {
        return Ok(Ok(()));
    }
    let reason = response
        .get("reason")
        .and_then(Json::as_str)
        .filter(|r| !r.is_empty())
        .map_or_else(
            || moli_i18n::tr!("pilotes.tesla.sans_explication"),
            str::to_owned,
        );
    Ok(Err(reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_billing_limit_disable_is_recognized() {
        let body = br#"{"error":"account disabled: EXCEEDED_LIMIT"}"#;
        assert!(billing_limit(StatusCode::FORBIDDEN, body));
        // Another 403 (scopes, region…) is not a billing disable.
        assert!(!billing_limit(
            StatusCode::FORBIDDEN,
            br#"{"error":"unauthorized"}"#
        ));
        // EXCEEDED_LIMIT elsewhere in the body, without the 403, does not count.
        assert!(!billing_limit(StatusCode::OK, body));
    }

    #[test]
    fn the_form_is_encoded() {
        assert_eq!(form(&[("a", "b c"), ("d", "e/f")]), "a=b%20c&d=e%2Ff");
        assert_eq!(
            form(&[(
                "code_verifier",
                "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
            )]),
            "code_verifier=dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        );
    }

    #[test]
    fn base64url_has_no_padding() {
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
    }

    #[test]
    fn the_challenge_follows_the_verifier() {
        // The RFC 7636 example vector.
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
