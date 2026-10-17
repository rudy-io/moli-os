//! HTTPS and websocket client for a Dirigera hub: the pinned connection of
//! `moli-net` (trust on first use, before any token leaves), plus the
//! bearer token the hub gave at pairing.

use anyhow::{Context as _, bail};
use http::StatusCode;
use http_body_util::Full;
use hyper::body::Bytes;
use moli_net::ws::WebSocket;
use moli_net::{Method, PinnedHttps, PinnedTls};
use serde_json::Value as Json;

use crate::pkce;

pub use moli_net::PIN_MISMATCH;

/// REST (`/v1/…`) and events (`wss://…/v1`) share this port.
pub const PORT: u16 = 8443;

/// Longest piece of an answer quoted in an error.
const EXCERPT: usize = 200;

/// One hub and its token.
pub struct Hub {
    http: PinnedHttps,
    /// The websocket's TLS, same pin: it remembers what it saw too.
    ws_tls: PinnedTls,
    host: String,
    token: Option<String>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the token: it commands every IKEA device of the house.
        f.debug_struct("Hub")
            .field("http", &self.http)
            .field("has_token", &self.token.is_some())
            .finish_non_exhaustive()
    }
}

/// What the hub says to a token request.
#[derive(PartialEq, Eq)]
pub enum Grant {
    Token(String),
    /// Nobody pressed the action button yet.
    NotYet,
    /// The code is past its 60 s (`Pairing request timed out`, then
    /// `Invalid authorization code`): ask for another one.
    Expired,
    /// Anything else, quoted.
    Refused(String),
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Token(_) => f.write_str("Token(…)"),
            Self::NotYet => f.write_str("NotYet"),
            Self::Expired => f.write_str("Expired"),
            Self::Refused(why) => write!(f, "Refused({why:?})"),
        }
    }
}

/// What the hub says to an authorization request.
#[derive(PartialEq, Eq)]
pub enum Authorize {
    /// A code, valid 60 s (it is not secret, but stays out of logs anyway).
    Code(String),
    /// The hub pairs one client at a time: `409 {"error":"Already one
    /// ongoing pairing request"}`. Ours from before a restart, or another
    /// client's; it ends within 60 s.
    Busy,
}

impl std::fmt::Debug for Authorize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(_) => f.write_str("Code(…)"),
            Self::Busy => f.write_str("Busy"),
        }
    }
}

/// Reads an authorization answer.
pub fn authorization(status: StatusCode, body: &[u8]) -> anyhow::Result<Authorize> {
    if status == StatusCode::CONFLICT {
        return Ok(Authorize::Busy);
    }
    if !status.is_success() {
        bail!("dirigera authorize: {status} {}", excerpt(body));
    }
    let json: Json = serde_json::from_slice(body).context("dirigera authorize answer")?;
    json["code"]
        .as_str()
        .filter(|code| !code.is_empty())
        .map(|code| Authorize::Code(code.to_owned()))
        .context("no code in the dirigera authorize answer")
}

/// Reads a token answer. Seen on a hub (firmware 2.975): `403 {"error":
/// "Button not pressed or presence time stamp timed out."}` until the
/// press, `403 {"error":"Pairing request timed out"}` once the code is 60 s
/// old, `401 {"error":"Invalid authorization code"}` after that.
#[must_use]
pub fn grant(status: StatusCode, body: &[u8]) -> Grant {
    let json: Json = serde_json::from_slice(body).unwrap_or_default();
    if status.is_success() {
        return match json["access_token"].as_str().filter(|t| !t.is_empty()) {
            Some(token) => Grant::Token(token.to_owned()),
            None => Grant::Refused(format!("{status} without access_token")),
        };
    }
    let error = json["error"].as_str().unwrap_or_default();
    match status {
        StatusCode::FORBIDDEN if error.starts_with("Button not pressed") => Grant::NotYet,
        StatusCode::FORBIDDEN if error.contains("timed out") => Grant::Expired,
        StatusCode::UNAUTHORIZED => Grant::Expired,
        _ => Grant::Refused(format!("{status} {}", excerpt(body))),
    }
}

/// The beginning of an answer, on one line, for an error message.
#[must_use]
pub fn excerpt(body: &[u8]) -> String {
    String::from_utf8_lossy(body)
        .chars()
        .take(EXCERPT)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

impl Hub {
    /// `pin`: the hub's certificate fingerprint, always known before a
    /// token is sent (see [`Hub::first_contact`]).
    pub fn new(host: &str, pin: String, token: Option<String>) -> anyhow::Result<Self> {
        Ok(Self {
            http: PinnedHttps::new(host, PORT, Some(pin.clone()))?,
            ws_tls: PinnedTls::new(Some(pin))?,
            host: host.to_owned(),
            token,
        })
    }

    /// The fingerprint of the certificate last presented (over HTTPS or the
    /// websocket): what a « certificate changed » message shows.
    #[must_use]
    pub fn seen_fingerprint(&self) -> Option<String> {
        self.http
            .seen_fingerprint()
            .or_else(|| self.ws_tls.seen_fingerprint())
    }

    /// A bare handshake, nothing sent: the fingerprint of the certificate
    /// the hub presents, to store before any token is asked for or sent.
    pub async fn first_contact(host: &str) -> anyhow::Result<String> {
        let probe = PinnedHttps::new(host, PORT, None)?;
        probe.handshake().await.context("first contact")?;
        probe.pinned_fingerprint().context("no certificate seen")
    }

    fn builder(&self, method: Method, path: &str) -> http::request::Builder {
        let builder = self.http.request(method, path);
        match &self.token {
            Some(token) => builder.header("authorization", format!("Bearer {token}")),
            None => builder,
        }
    }

    /// Pairing, step 1: a code for this PKCE challenge, valid 60 s.
    pub async fn authorize(&self, challenge: &str) -> anyhow::Result<Authorize> {
        let query = pkce::form(&[
            ("audience", "homesmart.local"),
            ("response_type", "code"),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ]);
        let path = format!("/v1/oauth/authorize?{query}");
        let (status, body) = self
            .http
            .send(moli_net::empty(self.builder(Method::GET, &path))?)
            .await?;
        authorization(status, &body)
    }

    /// Pairing, step 2: the token, once a person pressed the button. Sent
    /// once, never replayed by the connection.
    pub async fn token(&self, code: &str, verifier: &str, name: &str) -> anyhow::Result<Grant> {
        let form = pkce::form(&[
            ("code", code),
            ("name", name),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier),
        ]);
        let request = self
            .builder(Method::POST, "/v1/oauth/token")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Full::new(Bytes::from(form)))?;
        let (status, body) = self.http.send_once(request).await?;
        Ok(grant(status, &body))
    }

    /// Everything the hub knows: devices, rooms, scenes…
    pub async fn home(&self) -> anyhow::Result<(StatusCode, Bytes)> {
        self.http
            .send(moli_net::empty(self.builder(Method::GET, "/v1/home"))?)
            .await
    }

    /// `PATCH /v1/devices/<id>` with `[{"attributes": {…}}]`. Absolute
    /// values only: safe to repeat on a fresh connection.
    pub async fn patch(&self, device: &str, body: &Json) -> anyhow::Result<(StatusCode, Bytes)> {
        let path = format!("/v1/devices/{}", pkce::escape(device));
        self.http
            .send(moli_net::json_body(
                self.builder(Method::PATCH, &path),
                body,
            )?)
            .await
    }

    /// The event websocket, on its own connection, same pin.
    pub async fn events(&self) -> anyhow::Result<WebSocket> {
        let token = self.token.as_deref().context("no dirigera token")?;
        let bearer = format!("Bearer {token}");
        WebSocket::connect(
            &self.host,
            PORT,
            "/v1",
            &[("authorization", &bearer)],
            &self.ws_tls,
        )
        .await
    }
}

/// The keep-alive the hub expects on its websocket (as lpgera/dirigera
/// sends it every 30 s): it answers with a `pong` event.
#[must_use]
pub fn ping(id: u64, time: &str) -> String {
    serde_json::json!({
        "id": format!("moli-{id}"),
        "specversion": "1.1.0",
        "source": "urn:moli-os:dirigera",
        "time": time,
        "type": "ping",
        "data": null,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_answers_seen_on_a_hub() {
        let not_yet = br#"{"error":"Button not pressed or presence time stamp timed out."}"#;
        assert_eq!(grant(StatusCode::FORBIDDEN, not_yet), Grant::NotYet);
        let late = br#"{"error":"Pairing request timed out"}"#;
        assert_eq!(grant(StatusCode::FORBIDDEN, late), Grant::Expired);
        let gone = br#"{"error":"Invalid authorization code"}"#;
        assert_eq!(grant(StatusCode::UNAUTHORIZED, gone), Grant::Expired);
        assert_eq!(
            grant(StatusCode::OK, br#"{"access_token":"abc.def.ghi"}"#),
            Grant::Token("abc.def.ghi".into())
        );
        assert!(matches!(
            grant(StatusCode::OK, br#"{"access_token":""}"#),
            Grant::Refused(_)
        ));
        let Grant::Refused(why) = grant(StatusCode::BAD_REQUEST, b"{\"error\":\n\"bad\"}") else {
            panic!("a 400 is a refusal");
        };
        assert_eq!(why, "400 Bad Request {\"error\": \"bad\"}");
    }

    #[test]
    fn authorize_answers_seen_on_a_hub() {
        let code = br#"{"code":"0a1b2c3d-0000-4000-8000-000000000000"}"#;
        assert_eq!(
            authorization(StatusCode::OK, code).unwrap(),
            Authorize::Code("0a1b2c3d-0000-4000-8000-000000000000".into())
        );
        let busy = br#"{"error":"Already one ongoing pairing request"}"#;
        assert_eq!(
            authorization(StatusCode::CONFLICT, busy).unwrap(),
            Authorize::Busy
        );
        assert!(authorization(StatusCode::OK, b"{}").is_err());
        let refused = authorization(StatusCode::BAD_REQUEST, b"nope").unwrap_err();
        assert_eq!(
            refused.to_string(),
            "dirigera authorize: 400 Bad Request nope"
        );
    }

    #[test]
    fn neither_the_token_nor_the_grant_is_ever_printed() {
        let hub = Hub::new("192.168.1.x", "ab".into(), Some("secret-token".into())).unwrap();
        assert!(!format!("{hub:?}").contains("secret-token"));
        let grant = Grant::Token("secret-token".into());
        assert!(!format!("{grant:?}").contains("secret-token"));
        let code = Authorize::Code("secret-code".into());
        assert!(!format!("{code:?}").contains("secret-code"));
    }

    #[test]
    fn the_ping_is_an_event() {
        let ping: Json = serde_json::from_str(&ping(7, "2026-10-17T00:00:00Z")).unwrap();
        assert_eq!(ping["type"], "ping");
        assert_eq!(ping["id"], "moli-7");
        assert!(ping["data"].is_null());
    }

    #[test]
    fn excerpts_are_short_and_on_one_line() {
        let long = "x".repeat(1000);
        assert_eq!(excerpt(long.as_bytes()).len(), EXCERPT);
        assert_eq!(excerpt(b"a\r\nb"), "a  b");
    }
}
