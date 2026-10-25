//! The VELUX ACTIVE cloud, as its app speaks to it (a Netatmo server): a
//! person logs in (e-mail and password, from the vault, with the app's own
//! public client id), then the home's modules (`api/homesdata`) and their
//! state (`api/homestatus`), forms in, JSON out, over HTTPS. Orders go to
//! `syncapi/v1/setstate`, JSON both ways; a refusal can come with HTTP 200,
//! as a list of errors in the answer.

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
pub const APP_VERSION: &str = "791302006";
/// The error code of a module (or gateway) the cloud cannot reach.
const UNREACHABLE: i64 = 6;
/// A token is renewed this long before the cloud says it ends.
const MARGIN: Duration = Duration::from_secs(120);

/// The token is gone (expired or refused): log in again.
#[derive(Debug, thiserror::Error)]
#[error("VELUX: not logged in")]
pub struct LoggedOut;

/// The cloud said no to an order: why, in words for the family.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Refused(pub String);

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
        self.send(
            path,
            "application/x-www-form-urlencoded",
            encode(form),
            TIMEOUT,
        )
        .await
    }

    async fn send(
        &self,
        path: &str,
        kind: &str,
        body: String,
        timeout: Duration,
    ) -> anyhow::Result<(StatusCode, Json)> {
        // Said even when 0 (an empty form): without it the server answers 411.
        let mut request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("Content-Type", kind)
            .header("Content-Length", body.len())
            .header("Accept", "application/json");
        if let Some((token, _)) = &self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let request = request.body(Full::new(moli_net::Body::from(body)))?;
        let (status, bytes) =
            moli_net::web(&self.host, 443, true, request, timeout, MAX_BODY).await?;
        let answer = if bytes.is_empty() {
            Json::Null
        } else {
            serde_json::from_slice(&bytes)
                .with_context(|| format!("VELUX {path}: HTTP {status}, not JSON"))?
        };
        Ok((status, answer))
    }

    fn logged_in(&self) -> bool {
        self.token
            .as_ref()
            .is_some_and(|(_, until)| tokio::time::Instant::now() < *until)
    }

    /// A read, with the token; refused for want of one: [`LoggedOut`].
    async fn read(&self, path: &str, form: &[(&str, &str)]) -> anyhow::Result<Json> {
        if !self.logged_in() {
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

    /// Sends an order (a whole `setstate` body), within `timeout`. Taken:
    /// `Ok`; refused: [`Refused`].
    pub async fn set_state(&self, order: &Json, timeout: Duration) -> anyhow::Result<()> {
        if !self.logged_in() {
            return Err(LoggedOut.into());
        }
        let (status, answer) = self
            .send(
                "/syncapi/v1/setstate",
                "application/json",
                order.to_string(),
                timeout,
            )
            .await?;
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(LoggedOut.into()),
            StatusCode::OK => match refusal(&answer) {
                None => Ok(()),
                Some(why) => Err(Refused(why).into()),
            },
            other => Err(Refused(moli_i18n::tr!(
                "pilotes.velux.ordre_refuse",
                why = format!("HTTP {}", other.as_u16())
            ))
            .into()),
        }
    }

    /// Sends the automations back, as `api/synchomeschedule` takes a
    /// schedule: the home, the schedule and its name in the address, the
    /// schedule itself in JSON (`schedule`: what [`crate::schedule`] edited).
    pub async fn sync_schedule(
        &self,
        home: &str,
        id: &str,
        name: &str,
        schedule: &Json,
    ) -> anyhow::Result<()> {
        if !self.logged_in() {
            return Err(LoggedOut.into());
        }
        let ids = [("home_id", home), ("schedule_id", id), ("name", name)];
        let mut body = schedule.clone();
        for (key, value) in ids {
            body[key] = Json::from(value);
        }
        let path = format!("/api/synchomeschedule?{}", encode(&ids));
        let (status, answer) = self
            .send(&path, "application/json", body.to_string(), TIMEOUT)
            .await?;
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(LoggedOut.into()),
            StatusCode::OK if answer["status"] == "ok" => Ok(()),
            other => Err(Refused(moli_i18n::tr!(
                "pilotes.velux.planning.refuse",
                why = answer["error"]["message"]
                    .as_str()
                    .map_or_else(|| format!("HTTP {}", other.as_u16()), str::to_owned)
            ))
            .into()),
        }
    }
}

/// Why a `setstate` answer is a refusal, if it is one: a list of errors in
/// `body`, or no `"status": "ok"`.
fn refusal(answer: &Json) -> Option<String> {
    let codes: Vec<i64> = answer["body"]["errors"]
        .as_array()
        .map(|e| e.iter().filter_map(|e| e["code"].as_i64()).collect())
        .unwrap_or_default();
    if codes.contains(&UNREACHABLE) {
        return Some(moli_i18n::tr("pilotes.velux.injoignable"));
    }
    if !codes.is_empty() {
        let codes: Vec<String> = codes.iter().map(i64::to_string).collect();
        return Some(moli_i18n::tr!(
            "pilotes.velux.ordre_refuse",
            why = format!("code {}", codes.join(", "))
        ));
    }
    (answer["status"] != "ok").then(|| {
        moli_i18n::tr!(
            "pilotes.velux.ordre_refuse",
            why = answer["error"]["message"].as_str().unwrap_or("?")
        )
    })
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
        let e = cloud
            .set_state(&Json::Null, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(e.is::<LoggedOut>(), "no order sent without a token");
    }

    #[test]
    fn a_refusal_can_come_with_http_200() {
        use serde_json::json;
        assert_eq!(refusal(&json!({ "status": "ok", "time_server": 1 })), None);
        let unreachable = json!({ "status": "ok", "body": { "errors": [
            { "code": 6, "id": "aa:bb:cc:dd:ee:01" }
        ] } });
        assert_eq!(
            refusal(&unreachable),
            Some(moli_i18n::tr("pilotes.velux.injoignable"))
        );
        let bad_hash = json!({ "status": "ok", "body": { "errors": [
            { "code": 9, "id": "aa:bb:cc:dd:ee:04" }
        ] } });
        assert!(refusal(&bad_hash).is_some_and(|w| w.contains("code 9")));
        let failed = json!({ "error": { "code": 2, "message": "Invalid access token" } });
        assert!(refusal(&failed).is_some_and(|w| w.contains("Invalid access token")));
    }
}
