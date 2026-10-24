//! HTTPS client for an Overkiz gateway (TaHoma, Connexion kit): the pinned
//! connection of `moli-net`, and the bearer token the Somfy app hands out in
//! developer mode.
//!
//! The local API has no login: the token is generated in the app and stored
//! in Moli's vault, so the certificate is pinned first, then the token is
//! used. A gateway that answers with another certificate never sees it.

use anyhow::Context as _;
use http::StatusCode;
use hyper::body::Bytes;
use moli_net::{Method, PinnedHttps};
use serde_json::Value as Json;

use crate::model::RawDevice;

pub use moli_net::PIN_MISMATCH;

/// The local API's port, announced by mDNS (`_kizboxdev._tcp`).
pub const PORT: u16 = 8443;

const BASE: &str = "/enduser-mobile-web/1/enduserAPI";

/// Longest piece of an answer quoted in an error.
const EXCERPT: usize = 200;

/// The gateway refused the token: a human must generate another one.
pub const TOKEN_REFUSED: &str = "tahoma: token refused";

/// One gateway and its token.
pub struct Gateway {
    http: PinnedHttps,
    token: String,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the token: it moves every shutter of the house.
        f.debug_struct("Gateway")
            .field("http", &self.http)
            .field("token", &if self.token.is_empty() { "none" } else { "set" })
            .finish_non_exhaustive()
    }
}

impl Gateway {
    pub fn new(host: &str, pin: Option<String>, token: &str) -> anyhow::Result<Self> {
        Ok(Self {
            http: PinnedHttps::new(host, PORT, pin)?,
            token: token.to_owned(),
        })
    }

    /// Pins the certificate of a gateway nobody has talked to yet.
    pub async fn first_contact(host: &str) -> anyhow::Result<String> {
        let http = PinnedHttps::new(host, PORT, None)?;
        http.handshake().await.context("tahoma")?;
        http.seen_fingerprint()
            .context("the gateway presented no certificate")
    }

    #[must_use]
    pub fn seen_fingerprint(&self) -> Option<String> {
        self.http.seen_fingerprint()
    }

    #[must_use]
    pub fn pinned_fingerprint(&self) -> Option<String> {
        self.http.pinned_fingerprint()
    }

    /// Every device the gateway knows, with its states.
    pub async fn devices(&self) -> anyhow::Result<Vec<RawDevice>> {
        let (status, body) = self.get("/setup/devices").await?;
        if !status.is_success() {
            return Err(refused(status, &body));
        }
        serde_json::from_slice(&body).context("unreadable /setup/devices answer")
    }

    /// Sends one action. Returns the execution id the gateway followed it
    /// with (useful in logs); the state itself arrives on the next poll.
    pub async fn apply(&self, label: &str, actions: Vec<Json>) -> anyhow::Result<String> {
        let body = serde_json::json!({ "label": label, "actions": actions });
        let (status, answer) = self.post("/exec/apply", &body).await?;
        if !status.is_success() {
            return Err(refused(status, &answer));
        }
        let answer: Json = serde_json::from_slice(&answer).unwrap_or(Json::Null);
        Ok(answer
            .get("execId")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    async fn get(&self, path: &str) -> anyhow::Result<(StatusCode, Bytes)> {
        let request = moli_net::empty(
            self.http
                .request(Method::GET, &format!("{BASE}{path}"))
                .header("authorization", format!("Bearer {}", self.token))
                .header("accept", "application/json"),
        )?;
        self.http.send(request).await
    }

    async fn post(&self, path: &str, body: &Json) -> anyhow::Result<(StatusCode, Bytes)> {
        let request = moli_net::json_body(
            self.http
                .request(Method::POST, &format!("{BASE}{path}"))
                .header("authorization", format!("Bearer {}", self.token))
                .header("accept", "application/json"),
            body,
        )?;
        self.http.send(request).await
    }
}

/// A non-2xx answer: the token being refused is singled out, because it asks
/// for a human, not for a retry.
fn refused(status: StatusCode, body: &[u8]) -> anyhow::Error {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return anyhow::anyhow!("{TOKEN_REFUSED}: {status}");
    }
    anyhow::anyhow!("gateway answered {status}: {}", excerpt(body))
}

/// The start of an answer, for a status line.
#[must_use]
pub fn excerpt(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.chars().count() <= EXCERPT {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(EXCERPT).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unauthorized_answer_asks_for_a_human() {
        let error = refused(StatusCode::UNAUTHORIZED, b"{\"error\":\"no\"}");
        assert!(format!("{error:#}").contains(TOKEN_REFUSED));
    }

    #[test]
    fn another_failure_keeps_the_gateway_message_short() {
        let body = format!("{{\"error\":\"{}\"}}", "x".repeat(500));
        let error = refused(StatusCode::INTERNAL_SERVER_ERROR, body.as_bytes());
        let text = format!("{error:#}");
        assert!(text.contains("500"));
        assert!(text.chars().count() < 300);
    }
}
