//! Micronova's cloud (Agua IoT), as the Easy Connect apps speak to it: an
//! app registers, a person logs in (e-mail and password, from the vault),
//! then devices, register maps and readings, all JSON over HTTPS. A reading
//! is a job: asked, then polled until done. Nothing is written here.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Method, Request, StatusCode};
use http_body_util::Full;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value as Json, json};

use crate::model::{Map, Reading, Register};

const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 1 << 20;
/// A reading job is polled this often, this many times.
const JOB_POLL: Duration = Duration::from_secs(1);
const JOB_TRIES: usize = 12;
/// What the apps send as the map's last update: the whole map comes back.
const MAP_SINCE: &str = "2018-06-03T08:59:54.043";

/// A stove of the account.
#[derive(Clone, Debug)]
pub struct Stove {
    pub id_device: String,
    pub id_product: String,
    pub name: String,
    pub product: Option<String>,
    pub online: bool,
}

/// The token is gone (expired or refused): log in again.
#[derive(Debug, thiserror::Error)]
#[error("Micronova: not logged in")]
pub struct LoggedOut;

pub struct Cloud {
    host: String,
    customer_code: String,
    /// This app's identity for the cloud, new at each start.
    app: String,
    token: Option<String>,
}

impl Cloud {
    #[must_use]
    pub fn new(host: &str, customer_code: &str) -> Self {
        let mut bytes = [0u8; 16];
        let _ = SystemRandom::new().fill(&mut bytes);
        let app = bytes.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
        Self {
            host: host.to_owned(),
            customer_code: customer_code.to_owned(),
            app,
            token: None,
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: &Json,
        login: bool,
    ) -> anyhow::Result<(StatusCode, Json)> {
        let auth = if login {
            self.app.as_str()
        } else {
            self.token.as_deref().ok_or(LoggedOut)?
        };
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("Accept", "application/json, text/javascript, */*; q=0.01")
            .header("Content-Type", "application/json")
            .header("Origin", "file://")
            .header("id_brand", "1")
            .header("customer_code", &self.customer_code)
            .header("local", if login { "true" } else { "false" })
            .header("Authorization", auth)
            .body(Full::new(moli_net::Body::from(body.to_string())))?;
        let (status, bytes) =
            moli_net::web(&self.host, 443, true, request, TIMEOUT, MAX_BODY).await?;
        let answer = if bytes.is_empty() {
            Json::Null
        } else {
            serde_json::from_slice(&bytes).context("Micronova: not JSON")?
        };
        Ok((status, answer))
    }

    async fn call(&self, method: Method, path: &str, body: &Json) -> anyhow::Result<Json> {
        let (status, answer) = self.send(method, path, body, false).await?;
        match status {
            StatusCode::OK | StatusCode::CREATED => Ok(answer),
            StatusCode::UNAUTHORIZED => Err(LoggedOut.into()),
            other => bail!("Micronova {path}: HTTP {other}"),
        }
    }

    /// Registers this app, then logs the person in.
    pub async fn login(&mut self, email: &str, password: &str) -> anyhow::Result<()> {
        let me = self.app.clone();
        let signup = json!({
            "phone_type": "Android", "phone_id": me, "phone_version": "1.0", "language": "en",
            "id_app": me, "push_notification_token": me, "push_notification_active": false,
        });
        let (status, _) = self.send(Method::POST, "/appSignup", &signup, true).await?;
        if status != StatusCode::CREATED {
            tracing::debug!(%status, "micronova: app signup");
        }
        let (status, answer) = self
            .send(
                Method::POST,
                "/userLogin",
                &json!({ "email": email, "password": password }),
                true,
            )
            .await?;
        if status != StatusCode::OK {
            bail!(moli_i18n::tr!(
                "pilotes.micronova.connexion_refusee",
                status = status.as_u16()
            ));
        }
        let token = answer["token"].as_str().context("Micronova: no token")?;
        self.token = Some(token.to_owned());
        Ok(())
    }

    pub async fn stoves(&self) -> anyhow::Result<Vec<Stove>> {
        let answer = self.call(Method::POST, "/deviceList", &json!({})).await?;
        Ok(answer["device"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| {
                Some(Stove {
                    id_device: text(&d["id_device"])?,
                    id_product: text(&d["id_product"])?,
                    name: d["name"].as_str().unwrap_or_default().trim().to_owned(),
                    product: d["name_product"].as_str().map(|p| p.trim().to_owned()),
                    online: d["is_online"].as_bool().unwrap_or(true),
                })
            })
            .collect())
    }

    fn ids(stove: &Stove) -> Json {
        json!({ "id_device": stove.id_device, "id_product": stove.id_product })
    }

    /// The register map of a stove's board.
    pub async fn map(&self, stove: &Stove) -> anyhow::Result<Map> {
        let info = self
            .call(Method::POST, "/deviceGetInfo", &Self::ids(stove))
            .await?;
        let wanted = info["device_info"][0]["id_registers_map"].clone();
        let mut body = Self::ids(stove);
        body["last_update"] = json!(MAP_SINCE);
        let maps = self
            .call(Method::POST, "/deviceGetRegistersMap", &body)
            .await?;
        let registers = maps["device_registers_map"]["registers_map"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|m| m["id"] == wanted)
            .map(|m| m["registers"].clone())
            .context("Micronova: no register map for this board")?;
        let registers: Vec<Register> =
            serde_json::from_value(registers).context("Micronova: register map")?;
        Ok(Map::new(registers))
    }

    /// One reading of a stove's registers.
    pub async fn read(&self, stove: &Stove) -> anyhow::Result<Reading> {
        let mut body = Self::ids(stove);
        body["BufferId"] = json!(1);
        let job = self
            .call(Method::POST, "/deviceGetBufferReading", &body)
            .await?;
        let id = text(&job["idRequest"]).context("Micronova: no reading job")?;
        for _ in 0..JOB_TRIES {
            tokio::time::sleep(JOB_POLL).await;
            let state = self
                .call(Method::GET, &format!("/deviceJobStatus/{id}"), &json!({}))
                .await?;
            if state["jobAnswerStatus"] == "completed" {
                return Ok(reading(&state["jobAnswerData"]));
            }
        }
        bail!("Micronova: the stove did not answer in time")
    }
}

/// An id the cloud sends as a number or as text.
fn text(v: &Json) -> Option<String> {
    v.as_str()
        .map(str::to_owned)
        .or_else(|| v.as_i64().map(|n| n.to_string()))
}

/// `{Items: [offsets], Values: [raw]}` → offset → raw.
#[must_use]
pub fn reading(data: &Json) -> Reading {
    let offsets = data["Items"].as_array().into_iter().flatten();
    let values = data["Values"].as_array().into_iter().flatten();
    offsets
        .zip(values)
        .filter_map(|(o, v)| Some((u32::try_from(o.as_u64()?).ok()?, v.as_i64()?)))
        .collect::<HashMap<_, _>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_answer_pairs_offsets_and_values() {
        let data = json!({ "Items": [1, 33, 32893], "Values": [36, 0, 19] });
        let r = reading(&data);
        assert_eq!(r[&1], 36);
        assert_eq!(r[&33], 0);
        assert_eq!(r[&32893], 19);
    }

    #[test]
    fn ids_may_be_numbers_or_text() {
        assert_eq!(text(&json!(42)), Some("42".into()));
        assert_eq!(text(&json!("abc")), Some("abc".into()));
        assert_eq!(text(&json!(null)), None);
    }

    #[test]
    fn each_start_is_a_new_app() {
        let a = Cloud::new("remote.example", "1");
        let b = Cloud::new("remote.example", "1");
        assert_eq!(a.app.len(), 32);
        assert_ne!(a.app, b.app);
    }
}
