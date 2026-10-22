//! Somfy Protect's cloud (the API the app speaks, ex-Myfox): an OAuth2
//! token from the account's e-mail and password, kept and refreshed, then
//! the sites (one per alarm) and their devices.
//!
//! Written from the API's observable behaviour; somfy-protect-api (GPL-3)
//! served as a reference for the addresses and fields, no code is taken.

use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Method, Request};
use http_body_util::Full;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

const SSO_HOST: &str = "sso.myfox.io";
const TOKEN_PATH: &str = "/oauth/oauth/v2/token";
const API_HOST: &str = "api.myfox.io";
const LIMIT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 4 << 20;

/// The app's OAuth client: not the person's, but not ours to publish
/// either, so it lives in the vault with the account.
#[derive(Clone)]
pub struct Client {
    pub id: String,
    pub secret: String,
}

/// What survives a restart, in the vault (one JSON secret). Never printed.
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// A token good for another minute at least.
    pub fn fresh(&self, now: i64) -> bool {
        !self.access_token.is_empty() && self.expires_at > now + 60
    }
}

/// Why Somfy said no, when a person has to act.
#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error("wrong e-mail, password or client")]
    Credentials,
    #[error("token refused")]
    Expired,
}

/// A site: one alarm, its mode.
#[derive(Clone, Debug, Deserialize)]
pub struct Site {
    #[serde(rename = "site_id")]
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// `disarmed`, `partial` or `armed`.
    #[serde(default)]
    pub security_level: String,
}

/// A device of a site (Link, detector, siren, remote, camera…).
#[derive(Clone, Debug, Deserialize)]
pub struct Device {
    #[serde(rename = "device_id")]
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default, rename = "device_definition")]
    pub definition: Definition,
    /// Battery, radio, temperature…: varies with the kind.
    #[serde(default)]
    pub status: serde_json::Map<String, Json>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Definition {
    /// « Myfox Security Infrared Sensor », « IntelliTag », « Link »…
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Deserialize)]
struct Items<T> {
    items: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct Grant {
    #[serde(rename = "access_token")]
    access: String,
    #[serde(default, rename = "refresh_token")]
    refresh: String,
    #[serde(default)]
    expires_in: i64,
}

/// `application/x-www-form-urlencoded`, every byte but the unreserved ones escaped.
pub fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", escape(k), escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn escape(text: &str) -> String {
    use std::fmt::Write as _;
    text.bytes().fold(String::new(), |mut out, b| {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
        out
    })
}

#[derive(Debug)]
pub struct Cloud {
    pub session: Session,
}

impl Cloud {
    /// A new token from the account (`grant_type=password`).
    pub async fn log_in(
        &mut self,
        client: &Client,
        email: &str,
        password: &str,
        now: i64,
    ) -> anyhow::Result<()> {
        let body = form(&[
            ("grant_type", "password"),
            ("client_id", &client.id),
            ("client_secret", &client.secret),
            ("username", email),
            ("password", password),
        ]);
        self.token(body, now).await
    }

    /// The token renewed (`grant_type=refresh_token`).
    pub async fn refresh(&mut self, client: &Client, now: i64) -> anyhow::Result<()> {
        if self.session.refresh_token.is_empty() {
            return Err(Refusal::Expired.into());
        }
        let body = form(&[
            ("grant_type", "refresh_token"),
            ("client_id", &client.id),
            ("client_secret", &client.secret),
            ("refresh_token", &self.session.refresh_token),
        ]);
        self.token(body, now).await
    }

    async fn token(&mut self, body: String, now: i64) -> anyhow::Result<()> {
        let request = Request::builder()
            .method(Method::POST)
            .uri(TOKEN_PATH)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .header("User-Agent", "Somfy Protect")
            .body(Full::new(body.into()))?;
        let (status, bytes) = moli_net::web(SSO_HOST, 443, true, request, LIMIT, MAX_BODY).await?;
        // `invalid_grant`, `invalid_client`: a person must fix the vault.
        if status.as_u16() == 400 || status.as_u16() == 401 {
            return Err(Refusal::Credentials.into());
        }
        if !status.is_success() {
            bail!("Somfy SSO answered {status}");
        }
        let token: Grant = serde_json::from_slice(&bytes).context("Somfy SSO: no token")?;
        self.session = Session {
            access_token: token.access,
            // A refresh may not hand a new one: the old one stays good.
            refresh_token: if token.refresh.is_empty() {
                std::mem::take(&mut self.session.refresh_token)
            } else {
                token.refresh
            },
            expires_at: now + token.expires_in.max(60),
        };
        Ok(())
    }

    pub async fn sites(&self) -> anyhow::Result<Vec<Site>> {
        let items: Items<Site> = serde_json::from_value(self.get("/v3/site").await?)
            .context("Somfy: sites not understood")?;
        Ok(items.items)
    }

    pub async fn devices(&self, site: &str) -> anyhow::Result<Vec<Device>> {
        anyhow::ensure!(is_id(site), "Somfy: odd site id");
        let items: Items<Device> =
            serde_json::from_value(self.get(&format!("/v3/site/{site}/device")).await?)
                .context("Somfy: devices not understood")?;
        Ok(items.items)
    }

    /// One read; the token travels in a header, never in an error.
    async fn get(&self, path: &str) -> anyhow::Result<Json> {
        let request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header("Accept", "application/json")
            .header("User-Agent", "Somfy Protect")
            .header(
                "Authorization",
                format!("Bearer {}", self.session.access_token),
            )
            .body(Full::new(moli_net::Body::new()))?;
        let (status, bytes) = moli_net::web(API_HOST, 443, true, request, LIMIT, MAX_BODY).await?;
        if status.as_u16() == 401 {
            return Err(Refusal::Expired.into());
        }
        if !status.is_success() {
            bail!("Somfy answered {status} on {path}");
        }
        serde_json::from_slice(&bytes).context("Somfy: not JSON")
    }
}

/// Ids go into paths: letters, digits and dashes only.
fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_escapes_what_must_be() {
        assert_eq!(
            form(&[("username", "user+1@example.com"), ("password", "a b&c=d")]),
            "username=user%2B1%40example.com&password=a%20b%26c%3Dd"
        );
    }

    #[test]
    fn a_token_is_fresh_until_its_last_minute() {
        let session = Session {
            access_token: "t".into(),
            refresh_token: String::new(),
            expires_at: 1000,
        };
        assert!(session.fresh(900));
        assert!(!session.fresh(950));
        assert!(!Session::default().fresh(0));
    }

    #[test]
    fn sites_and_devices_are_read_from_their_lists() {
        let sites: Items<Site> = serde_json::from_value(serde_json::json!({
            "items": [{ "site_id": "abc-1", "label": "Maison", "security_level": "partial", "other": 1 }]
        }))
        .unwrap();
        assert_eq!(sites.items[0].security_level, "partial");
        let devices: Items<Device> = serde_json::from_value(serde_json::json!({
            "items": [{
                "device_id": "dev-1",
                "label": "Entrée",
                "device_definition": { "label": "Myfox Security Infrared Sensor", "type": "pir" },
                "status": { "battery_level": 80, "temperature": 19.5 }
            }]
        }))
        .unwrap();
        assert_eq!(
            devices.items[0].definition.label,
            "Myfox Security Infrared Sensor"
        );
        assert_eq!(devices.items[0].status["battery_level"], 80);
    }

    #[test]
    fn only_plain_ids_go_into_paths() {
        assert!(is_id("a1-b2_c3"));
        assert!(!is_id("../x"));
        assert!(!is_id(""));
    }
}
