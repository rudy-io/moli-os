//! Eufy's cloud API (the "legacy" backend the app has spoken since v4):
//! login, the second factor, and the household's stations and devices.
//!
//! Ported from eufy-security-client (MIT, see NOTICE). The password and
//! every answer that matters are AES-encrypted with a key agreed with the
//! server (P-256): its well-known login key first, then the key it gives at
//! login. A session is kept for months; nothing is sent to the cloud but
//! these reads.

use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Method, Request};
use http_body_util::Full;
use md5::{Digest as _, Md5};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::crypto::{self, ClientKey};

/// The server's login key, as the app ships it.
pub const SERVER_PUBLIC_KEY: &str = "04c5c00c4f8d1197cc7c3167c52bf7acb054d722f0ef08dcd7e0883236e0d72a3868d9750cb47fa4619248f3d83f0f662671dadc6e2d31c2f41db0161651c7c076";

/// Where the API for a country is asked.
const DOMAIN_HOST: &str = "extend.eufylife.com";
const LIMIT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 4 << 20;

/// Answer codes (`ResponseErrorCode`).
const OK: i64 = 0;
const NEED_VERIFY_CODE: i64 = 26052;
const NEED_CAPTCHA: i64 = 100_032;
const CAPTCHA_ERROR: i64 = 100_033;
const VERIFY_CODE_ERRORS: [i64; 4] = [26050, 26051, 26053, 26054];
const WRONG_PASSWORD: [i64; 3] = [26006, 26108, 26055];
const SESSION_TIMEOUT: i64 = 401;

/// What survives a restart, in the vault (one JSON secret). Never printed.
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Session {
    /// The P-256 key Moli agreed with the server (hex).
    pub client_key: String,
    /// The phone Moli says it is: a trusted device is this identity.
    pub phone_id: String,
    pub phone_sn: String,
    #[serde(default)]
    pub api_host: String,
    #[serde(default)]
    pub token: String,
    /// Unix seconds.
    #[serde(default)]
    pub token_expires_at: i64,
    #[serde(default)]
    pub user_id: String,
    /// The key the server gave at login (hex); answers are encrypted to it.
    #[serde(default)]
    pub server_key: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("api_host", &self.api_host)
            .field("token_expires_at", &self.token_expires_at)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// A new identity: its own key and phone, unknown to Eufy yet.
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client_key: ClientKey::generate()?.to_hex(),
            phone_id: crypto::hex(&crypto::random::<8>()?),
            phone_sn: crypto::hex(&crypto::random::<6>()?),
            ..Self::default()
        })
    }

    /// A token still good for a day.
    pub fn logged_in(&self, now: i64) -> bool {
        !self.token.is_empty() && self.token_expires_at > now + 24 * 3600
    }
}

/// What the person must do for a login to go through.
#[derive(Debug, PartialEq)]
pub enum Login {
    Done,
    /// A code was e-mailed to the account.
    NeedCode,
    /// Type the characters of this image (a `data:` URL).
    NeedCaptcha {
        id: String,
        image: String,
    },
}

/// The code or captcha answer the person gave.
#[derive(Debug)]
pub enum Answer<'a> {
    Code(&'a str),
    Captcha { id: &'a str, text: &'a str },
}

/// Why the cloud said no, when a person has to act.
#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error("wrong e-mail or password")]
    Password,
    #[error("wrong or expired code")]
    Code,
    #[error("session expired")]
    Expired,
}

#[derive(Debug)]
pub struct Cloud {
    pub session: Session,
    key: ClientKey,
    country: String,
    language: String,
}

/// A station (HomeBase, or a Wi-Fi camera that is its own station).
#[derive(Clone, Debug, Deserialize)]
pub struct Station {
    #[serde(rename = "station_sn")]
    pub serial: String,
    #[serde(default, rename = "station_name")]
    pub name: String,
    #[serde(default, rename = "station_model")]
    pub model: String,
    #[serde(default)]
    pub params: Vec<Param>,
}

/// A camera, a doorbell, a sensor (or a Wi-Fi camera, again).
#[derive(Clone, Debug, Deserialize)]
pub struct Device {
    #[serde(rename = "device_sn")]
    pub serial: String,
    #[serde(default, rename = "device_name")]
    pub name: String,
    #[serde(default, rename = "device_model")]
    pub model: String,
    #[serde(default)]
    pub params: Vec<Param>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Param {
    pub param_type: i64,
    #[serde(default, deserialize_with = "string_or_number")]
    pub param_value: String,
}

fn string_or_number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Json::deserialize(d)? {
        Json::String(s) => s,
        Json::Null => String::new(),
        other => other.to_string(),
    })
}

/// The value of one parameter, if the list has it.
pub fn param(params: &[Param], kind: i64) -> Option<&str> {
    params
        .iter()
        .find(|p| p.param_type == kind)
        .map(|p| p.param_value.as_str())
}

fn md5_hex(text: &str) -> String {
    crypto::hex(&Md5::digest(text.as_bytes()))
}

fn now_ms() -> i64 {
    i64::try_from(moli_core::now_ms()).unwrap_or(i64::MAX)
}

/// The local offset, in milliseconds east of UTC (`time_zone`).
fn offset_ms() -> i64 {
    i64::from(jiff::Zoned::now().offset().seconds()) * 1000
}

fn gmt(offset_ms: i64) -> String {
    let minutes = offset_ms / 60_000;
    let sign = if minutes < 0 { '-' } else { '+' };
    format!(
        "GMT{sign}{:02}:{:02}",
        minutes.abs() / 60,
        minutes.abs() % 60
    )
}

impl Cloud {
    pub fn new(session: Session, country: &str, language: &str) -> anyhow::Result<Self> {
        Ok(Self {
            key: ClientKey::from_hex(&session.client_key)?,
            session,
            country: country.to_ascii_uppercase(),
            language: language.to_ascii_lowercase(),
        })
    }

    /// The country's API host, asked once and remembered.
    pub async fn locate(&mut self) -> anyhow::Result<()> {
        if !self.session.api_host.is_empty() {
            return Ok(());
        }
        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("/domain/{}", self.country))
            .body(Full::new(moli_net::Body::new()))?;
        let (_, body) = moli_net::web(DOMAIN_HOST, 443, true, request, LIMIT, MAX_BODY).await?;
        let answer: Json = serde_json::from_slice(&body).context("Eufy: not JSON")?;
        let host = answer["data"]["domain"]
            .as_str()
            .filter(|h| answer["code"].as_i64() == Some(OK) && is_host(h))
            .with_context(|| format!("Eufy: no API for country {}", self.country))?;
        host.clone_into(&mut self.session.api_host);
        Ok(())
    }

    /// Log in, with the person's answer to the last challenge if any.
    pub async fn login(
        &mut self,
        email: &str,
        password: &str,
        answer: Option<Answer<'_>>,
    ) -> anyhow::Result<Login> {
        self.locate().await?;
        let shared = self.key.shared(SERVER_PUBLIC_KEY)?;
        let mut body = json!({
            "ab": self.country,
            "client_secret_info": { "public_key": self.key.public_hex() },
            "enc": 0,
            "email": email,
            "password": crypto::encrypt(&shared, password.as_bytes()),
            "time_zone": offset_ms(),
            "transaction": now_ms().to_string(),
        });
        match answer {
            Some(Answer::Code(code)) => body["verify_code"] = json!(code),
            Some(Answer::Captcha { id, text }) => {
                body["captcha_id"] = json!(id);
                body["answer"] = json!(text);
            }
            None => {}
        }
        let answer = self.post("v2/passport/login_sec", &body, false).await?;
        let data = &answer["data"];
        match answer["code"].as_i64() {
            Some(OK) => {
                data["server_secret_info"]["public_key"]
                    .as_str()
                    .unwrap_or(SERVER_PUBLIC_KEY)
                    .clone_into(&mut self.session.server_key);
                self.take_token(data)?;
                data["user_id"]
                    .as_str()
                    .unwrap_or_default()
                    .clone_into(&mut self.session.user_id);
                Ok(Login::Done)
            }
            Some(NEED_VERIFY_CODE) => {
                // A short token, only good to ask for the code.
                self.take_token(data)?;
                let sent = self
                    .post(
                        "v1/sms/send/verify_code",
                        &json!({ "message_type": 2, "transaction": now_ms().to_string() }),
                        true,
                    )
                    .await?;
                check(&sent).context("asking for the e-mailed code")?;
                Ok(Login::NeedCode)
            }
            Some(NEED_CAPTCHA | CAPTCHA_ERROR) => Ok(Login::NeedCaptcha {
                id: data["captcha_id"]
                    .as_str()
                    .context("captcha without id")?
                    .to_owned(),
                image: data["item"]
                    .as_str()
                    .context("captcha without image")?
                    .to_owned(),
            }),
            Some(code) if WRONG_PASSWORD.contains(&code) => Err(Refusal::Password.into()),
            Some(code) if VERIFY_CODE_ERRORS.contains(&code) => Err(Refusal::Code.into()),
            _ => Err(check(&answer)
                .err()
                .unwrap_or_else(|| anyhow::anyhow!("Eufy: login refused"))),
        }
    }

    /// After a login with an e-mailed code: trust this "phone", so that the
    /// next logins need none.
    pub async fn trust(&mut self, code: &str) -> anyhow::Result<()> {
        let list = self
            .post("v1/app/trust_device/list", &json!({}), true)
            .await?;
        let trusted = list["data"]["list"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|d| d["is_current_device"].as_i64() == Some(1));
        if !trusted {
            let added = self
                .post(
                    "v1/app/trust_device/add",
                    &json!({ "verify_code": code, "transaction": now_ms().to_string() }),
                    true,
                )
                .await?;
            check(&added).context("trusting Moli")?;
        }
        Ok(())
    }

    pub async fn stations(&mut self) -> anyhow::Result<Vec<Station>> {
        self.list("v2/house/station_list").await
    }

    pub async fn devices(&mut self) -> anyhow::Result<Vec<Device>> {
        self.list("v2/house/device_list").await
    }

    async fn list<T: serde::de::DeserializeOwned>(&mut self, path: &str) -> anyhow::Result<Vec<T>> {
        let body = json!({
            "device_sn": "", "num": 1000, "orderby": "", "page": 0, "station_sn": "",
            "time_zone": offset_ms(), "transaction": now_ms().to_string(),
        });
        let answer = self.post(path, &body, true).await?;
        check(&answer)?;
        let items: Vec<Json> = match &answer["data"] {
            Json::String(text) => {
                let plain = crypto::decrypt(&self.key.shared(&self.session.server_key)?, text)?;
                serde_json::from_slice(&plain).context("Eufy: list not JSON")?
            }
            Json::Array(items) => items.clone(),
            _ => Vec::new(),
        };
        // One odd entry does not hide the others.
        Ok(items
            .into_iter()
            .filter_map(|item| match serde_json::from_value(item) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(error = %e, path, "eufy: entry skipped");
                    None
                }
            })
            .collect())
    }

    fn take_token(&mut self, data: &Json) -> anyhow::Result<()> {
        data["auth_token"]
            .as_str()
            .context("login without token")?
            .clone_into(&mut self.session.token);
        self.session.token_expires_at = data["token_expires_at"].as_i64().unwrap_or(0);
        Ok(())
    }

    /// One call; the token travels in a header, never in an error.
    async fn post(&self, path: &str, body: &Json, authed: bool) -> anyhow::Result<Json> {
        let host = &self.session.api_host;
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(format!("/{path}"))
            .header("Content-Type", "application/json")
            .header("App_version", "v4.6.0_1630")
            .header("Os_type", "android")
            .header("Os_version", "31")
            .header("Phone_model", "ONEPLUS A3003")
            .header("Country", &self.country)
            .header("Language", &self.language)
            .header("Openudid", &self.session.phone_id)
            .header("Net_type", "wifi")
            .header("Sn", &self.session.phone_sn)
            .header("Model_type", "PHONE")
            .header("Timezone", gmt(offset_ms()))
            .header("Cache-Control", "no-cache");
        if authed {
            builder = builder.header("X-Auth-Token", &self.session.token);
            if !self.session.user_id.is_empty() {
                builder = builder.header("gtoken", md5_hex(&self.session.user_id));
            }
        }
        let request = builder.body(Full::new(serde_json::to_vec(body)?.into()))?;
        let (status, bytes) = moli_net::web(host, 443, true, request, LIMIT, MAX_BODY).await?;
        if status.as_u16() == 401 {
            return Err(Refusal::Expired.into());
        }
        if !status.is_success() {
            bail!("Eufy answered {status} on {path}");
        }
        let answer: Json = serde_json::from_slice(&bytes).context("Eufy: not JSON")?;
        if answer["code"].as_i64() == Some(SESSION_TIMEOUT) {
            return Err(Refusal::Expired.into());
        }
        Ok(answer)
    }
}

fn check(answer: &Json) -> anyhow::Result<()> {
    match answer["code"].as_i64() {
        Some(OK) => Ok(()),
        code => bail!(
            "Eufy {}: {}",
            code.map_or_else(|| "?".to_owned(), |c| c.to_string()),
            answer["msg"].as_str().unwrap_or("refused")
        ),
    }
}

/// A host name the cloud may send us to: letters, digits, dots, dashes.
fn is_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() < 254
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_zone_is_written_like_the_app() {
        assert_eq!(gmt(3_600_000), "GMT+01:00");
        assert_eq!(gmt(-16_200_000), "GMT-04:30");
        assert_eq!(gmt(0), "GMT+00:00");
    }

    #[test]
    fn only_plain_host_names_are_followed() {
        assert!(is_host("security-app-eu.eufylife.com"));
        assert!(!is_host("evil.example/x"));
        assert!(!is_host("a@b"));
        assert!(!is_host(""));
    }

    #[test]
    fn a_session_is_good_while_a_day_remains() {
        let mut s = Session::new().unwrap();
        assert!(!s.logged_in(0));
        s.token = "t".into();
        s.token_expires_at = 100_000;
        assert!(s.logged_in(100_000 - 24 * 3600 - 1));
        assert!(!s.logged_in(100_000 - 24 * 3600));
        assert_eq!(s.phone_id.len(), 16);
        assert!(!format!("{s:?}").contains(&s.client_key));
    }

    #[test]
    fn parameters_may_be_numbers_or_strings() {
        let station: Station = serde_json::from_value(json!({
            "station_sn": "T8010P0000000000",
            "params": [
                { "param_type": 1224, "param_value": "63" },
                { "param_type": 1101, "param_value": 87 },
                { "param_type": 1, "param_value": null },
            ],
        }))
        .unwrap();
        assert_eq!(param(&station.params, 1224), Some("63"));
        assert_eq!(param(&station.params, 1101), Some("87"));
        assert_eq!(param(&station.params, 1), Some(""));
        assert_eq!(param(&station.params, 2), None);
    }
}
