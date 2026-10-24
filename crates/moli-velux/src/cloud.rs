//! The VELUX ACTIVE cloud, as its app speaks to it (a Netatmo server): a
//! person logs in (e-mail and password, from the vault, with the app's own
//! public client id), then the home's modules (`api/homesdata`) and their
//! state (`api/homestatus`), forms in, JSON out, over HTTPS. Reads only.

use std::fmt::Write as _;
use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Method, Request, StatusCode};
use http_body_util::Full;
use serde_json::Value as Json;

const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 1 << 20;
/// The VELUX ACTIVE app's client, as it sends it (public: in every copy of
/// the app, and in the projects that describe this cloud).
const CLIENT_ID: &str = "5931426da127d981e76bdd3f";
const CLIENT_SECRET: &str = "6ae2d89d15e767ae5c56b456b452d319";
const APP_VERSION: &str = "791302006";
/// A token is renewed this long before the cloud says it ends.
const MARGIN: Duration = Duration::from_secs(120);

/// The token is gone (expired or refused): log in again.
#[derive(Debug, thiserror::Error)]
#[error("VELUX: not logged in")]
pub struct LoggedOut;

pub struct Cloud {
    host: String,
    token: Option<(String, tokio::time::Instant)>,
}

impl Cloud {
    #[must_use]
    pub fn new(host: &str) -> Self {
        Self {
            host: host.to_owned(),
            token: None,
        }
    }

    async fn post(&self, path: &str, form: &[(&str, &str)]) -> anyhow::Result<(StatusCode, Json)> {
        let mut request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json");
        if let Some((token, _)) = &self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let request = request.body(Full::new(moli_net::Body::from(encode(form))))?;
        let (status, bytes) =
            moli_net::web(&self.host, 443, true, request, TIMEOUT, MAX_BODY).await?;
        let answer = if bytes.is_empty() {
            Json::Null
        } else {
            serde_json::from_slice(&bytes).context("VELUX: not JSON")?
        };
        Ok((status, answer))
    }

    /// A read, with the token; refused for want of one: [`LoggedOut`].
    async fn read(&self, path: &str, form: &[(&str, &str)]) -> anyhow::Result<Json> {
        if self
            .token
            .as_ref()
            .is_none_or(|(_, until)| tokio::time::Instant::now() >= *until)
        {
            return Err(LoggedOut.into());
        }
        let (status, mut answer) = self.post(path, form).await?;
        match status {
            StatusCode::OK => Ok(answer["body"].take()),
            // Netatmo says an expired token with a 403 (code 3), a bad one with a 401.
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(LoggedOut.into()),
            other => bail!("VELUX {path}: HTTP {other}"),
        }
    }

    /// Logs the person in, as the app does.
    pub async fn login(&mut self, email: &str, password: &str) -> anyhow::Result<()> {
        self.token = None;
        let (status, answer) = self
            .post(
                "/oauth2/token",
                &[
                    ("grant_type", "password"),
                    ("client_id", CLIENT_ID),
                    ("client_secret", CLIENT_SECRET),
                    ("app_version", APP_VERSION),
                    ("username", email),
                    ("password", password),
                    ("user_prefix", "velux"),
                    ("scope", "velux_scopes"),
                ],
            )
            .await?;
        if status != StatusCode::OK {
            bail!(moli_i18n::tr!(
                "pilotes.velux.connexion_refusee",
                status = status.as_u16()
            ));
        }
        let token = answer["access_token"].as_str().context("VELUX: no token")?;
        let lasts = Duration::from_secs(answer["expires_in"].as_u64().unwrap_or(10_800));
        let until = tokio::time::Instant::now() + lasts.saturating_sub(MARGIN);
        self.token = Some((token.to_owned(), until));
        Ok(())
    }

    /// The account's home with a gateway (else its first): `homesdata`'s.
    pub async fn home(&self) -> anyhow::Result<Json> {
        let mut body = self.read("/api/homesdata", &[]).await?;
        let homes = body["homes"].as_array_mut().context("VELUX: no homes")?;
        let at = homes
            .iter()
            .position(|h| {
                h["modules"]
                    .as_array()
                    .is_some_and(|m| m.iter().any(|m| m["type"] == "NXG"))
            })
            .unwrap_or(0);
        if homes.is_empty() {
            bail!(moli_i18n::tr("pilotes.velux.aucune_maison"));
        }
        Ok(homes.swap_remove(at))
    }

    /// The state of a home's modules and rooms: `homestatus`'s `home`.
    pub async fn status(&self, home: &str) -> anyhow::Result<Json> {
        let mut body = self.read("/api/homestatus", &[("home_id", home)]).await?;
        Ok(body["home"].take())
    }
}

/// `a=1&b=x%40y`: a form, each value percent-encoded.
fn encode(form: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (key, value)) in form.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        for (j, part) in [key, value].into_iter().enumerate() {
            if j > 0 {
                out.push('=');
            }
            for b in part.bytes() {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                    out.push(char::from(b));
                } else {
                    let _ = write!(out, "%{b:02X}");
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_is_percent_encoded() {
        assert_eq!(
            encode(&[("username", "user@example.com"), ("password", "a b&c=é")]),
            "username=user%40example.com&password=a%20b%26c%3D%C3%A9"
        );
        assert_eq!(encode(&[]), "");
    }

    #[tokio::test]
    async fn no_token_is_logged_out() {
        let cloud = Cloud::new("app.example.com");
        let e = cloud.home().await.unwrap_err();
        assert!(e.is::<LoggedOut>());
    }
}
