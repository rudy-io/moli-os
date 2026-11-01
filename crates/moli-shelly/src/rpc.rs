//! Shelly's JSON-RPC over the device's websocket (`ws://<host>/rpc`).
//!
//! One connection carries everything: Moli's calls and their answers, and the
//! changes the device pushes (`NotifyStatus`, `NotifyFullStatus`,
//! `NotifyEvent`) to whoever spoke to it with a `src`. A notification read
//! while waiting for an answer is kept for later, in order.
//!
//! A device with a password answers `401` with a digest challenge
//! (SHA-256, user `admin`); the call is sent again with its answer, and the
//! challenge is kept for the next calls until the device asks again.

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_net::ws::WebSocket;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value as Json, json};

/// How long a call waits for its answer.
const ANSWER: Duration = Duration::from_secs(10);
/// Pushed changes kept while a call waits: beyond, the oldest go (the next
/// full status puts everything right).
const KEPT: usize = 256;
/// Who Moli says it is: the device sends its notifications back to it.
const SOURCE: &str = "moli-os";

/// The device asked for a password and none is known, or it refused it.
#[derive(Debug)]
pub struct AuthNeeded {
    pub refused: bool,
}

impl std::fmt::Display for AuthNeeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.refused {
            "password refused"
        } else {
            "password needed"
        })
    }
}

impl std::error::Error for AuthNeeded {}

/// A digest challenge, as the device gives it.
#[derive(Clone, Debug)]
struct Challenge {
    realm: String,
    nonce: Json,
    nc: Json,
}

pub struct Rpc {
    ws: WebSocket,
    next: u64,
    password: Option<String>,
    challenge: Option<Challenge>,
    pushed: VecDeque<Json>,
}

impl std::fmt::Debug for Rpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rpc")
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

fn sha256_hex(text: &str) -> String {
    digest(&SHA256, text.as_bytes()).as_ref().iter().fold(
        String::with_capacity(64),
        |mut out, b| {
            use std::fmt::Write as _;
            let _ = write!(out, "{b:02x}");
            out
        },
    )
}

/// The `auth` object of a call, for this challenge (Shelly's Gen2 digest:
/// `ha2` is fixed, `qop` is `auth`).
fn answer(challenge: &Challenge, password: &str, cnonce: u32) -> Json {
    let plain = |v: &Json| v.as_str().map_or_else(|| v.to_string(), str::to_owned);
    let ha1 = sha256_hex(&format!("admin:{}:{password}", challenge.realm));
    let ha2 = sha256_hex("dummy_method:dummy_uri");
    let response = sha256_hex(&format!(
        "{ha1}:{}:{}:{cnonce}:auth:{ha2}",
        plain(&challenge.nonce),
        plain(&challenge.nc)
    ));
    json!({
        "realm": challenge.realm,
        "username": "admin",
        "nonce": challenge.nonce,
        "cnonce": cnonce,
        "response": response,
        "algorithm": "SHA-256",
    })
}

fn challenge_of(error: &Json) -> Option<Challenge> {
    let message: Json = serde_json::from_str(error["message"].as_str()?).ok()?;
    Some(Challenge {
        realm: message["realm"].as_str()?.to_owned(),
        nonce: message.get("nonce")?.clone(),
        nc: message.get("nc").cloned().unwrap_or(json!(1)),
    })
}

impl Rpc {
    pub async fn open(host: &str, port: u16, password: Option<String>) -> anyhow::Result<Self> {
        let ws = WebSocket::connect_plain(host, port, "/rpc", &[]).await?;
        Ok(Self {
            ws,
            next: 0,
            password,
            challenge: None,
            pushed: VecDeque::new(),
        })
    }

    fn request(&mut self, method: &str, params: &Json) -> anyhow::Result<(u64, String)> {
        self.next += 1;
        let mut request = json!({ "id": self.next, "src": SOURCE, "method": method });
        if !params.is_null() {
            request["params"] = params.clone();
        }
        if let (Some(challenge), Some(password)) = (&self.challenge, &self.password) {
            let mut raw = [0u8; 4];
            SystemRandom::new()
                .fill(&mut raw)
                .map_err(|_| anyhow::anyhow!("no randomness"))?;
            request["auth"] = answer(challenge, password, u32::from_be_bytes(raw));
        }
        Ok((self.next, request.to_string()))
    }

    /// Calls `method`; the device's `result`.
    pub async fn call(&mut self, method: &str, params: Json) -> anyhow::Result<Json> {
        let mut asked_again = false;
        loop {
            let (id, text) = self.request(method, &params)?;
            self.ws.send_text(&text).await?;
            let reply = tokio::time::timeout(ANSWER, self.answer_to(id))
                .await
                .with_context(|| format!("{method}: no answer in time"))??;
            if let Some(error) = reply.get("error") {
                if error["code"].as_i64() == Some(401) {
                    let Some(challenge) = challenge_of(error) else {
                        bail!("{method}: unreadable password challenge");
                    };
                    if self.password.is_none() {
                        return Err(AuthNeeded { refused: false }.into());
                    }
                    if asked_again {
                        return Err(AuthNeeded { refused: true }.into());
                    }
                    self.challenge = Some(challenge);
                    asked_again = true;
                    continue;
                }
                bail!(
                    "{method}: {} ({})",
                    error["message"].as_str().unwrap_or("refused"),
                    error["code"]
                );
            }
            return Ok(reply.get("result").cloned().unwrap_or(Json::Null));
        }
    }

    async fn answer_to(&mut self, id: u64) -> anyhow::Result<Json> {
        loop {
            let message: Json = serde_json::from_str(&self.ws.recv_text(ANSWER).await?)?;
            if message.get("id").and_then(Json::as_u64) == Some(id) {
                return Ok(message);
            }
            if message.get("method").is_some() {
                if self.pushed.len() >= KEPT {
                    self.pushed.pop_front();
                }
                self.pushed.push_back(message);
            }
            // An answer to an older call that gave up: dropped.
        }
    }

    /// The next change the device pushes (`method` + `params`), or an error
    /// when nothing came for `limit`.
    pub async fn pushed(&mut self, limit: Duration) -> anyhow::Result<Json> {
        if let Some(message) = self.pushed.pop_front() {
            return Ok(message);
        }
        loop {
            let message: Json = serde_json::from_str(&self.ws.recv_text(limit).await?)?;
            if message.get("method").is_some() {
                return Ok(message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_follows_shellys_documentation() {
        // Values computed by hand with the formula of Shelly's Gen2 API
        // documentation (« Authentication »), dummy password.
        let challenge = Challenge {
            realm: "shellypro4pm-aabbccddee01".into(),
            nonce: json!(1_625_038_762),
            nc: json!(1),
        };
        let auth = answer(&challenge, "secret", 313_273_957);
        let ha1 = sha256_hex("admin:shellypro4pm-aabbccddee01:secret");
        let ha2 = sha256_hex("dummy_method:dummy_uri");
        let expected = sha256_hex(&format!("{ha1}:1625038762:1:313273957:auth:{ha2}"));
        assert_eq!(auth["response"], json!(expected));
        assert_eq!(auth["username"], "admin");
        assert_eq!(auth["nonce"], json!(1_625_038_762));
        assert_eq!(
            sha256_hex("dummy_method:dummy_uri").len(),
            64,
            "hex, lower case"
        );
    }

    #[test]
    fn a_401_carries_its_challenge() {
        let error = json!({"code": 401, "message": "{\"auth_type\": \"digest\", \"nonce\": 1625038762, \"nc\": 1, \"realm\": \"shellypro4pm-aabbccddee01\", \"algorithm\": \"SHA-256\"}"});
        let challenge = challenge_of(&error).unwrap();
        assert_eq!(challenge.realm, "shellypro4pm-aabbccddee01");
        assert_eq!(challenge.nonce, json!(1_625_038_762));
    }
}
