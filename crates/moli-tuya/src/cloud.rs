//! The Tuya cloud, for what never listens on the LAN: Wi-Fi battery sensors
//! (doors, leaks) that wake, tell the cloud and sleep again, and devices
//! behind a Tuya gateway. Read-only.
//!
//! The household's own cloud project (Access ID and Secret, from the vault
//! through `/run/secrets/tuya-cloud`): states from the signed OpenAPI at
//! start and every half hour, changes as they happen from the project's
//! message service (Pulsar over a websocket). Nothing is sent to the cloud
//! but these reads.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use http::{Method, Request};
use http_body_util::Full;
use md5::{Digest as _, Md5};
use moli_core::Value;
use moli_net::ws::WebSocket;
use ring::digest::{SHA256, digest};
use serde_json::Value as Json;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::device::Up;
use crate::model::TuyaDevice;
use crate::protocol::{ecb_decrypt, gcm_open, hmac_sha256, random};

/// Categories that sleep on battery: the cloud is their only voice.
pub const SLEEPY: [&str; 8] = ["mcs", "sj", "ywbj", "pir", "wsdcg", "rqbj", "cobj", "sos"];

const RESYNC: Duration = Duration::from_secs(30 * 60);
const QUIET: Duration = Duration::from_secs(60);
const BATCH: usize = 20;

/// The project's keys, one per line (`/run/secrets/tuya-cloud`).
#[derive(Clone)]
pub struct Keys {
    pub id: String,
    secret: String,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Where a person's keys are filed in Moli's encrypted store (dashboard).
pub const STORED_ID: &str = "cloud_access_id";
pub const STORED_SECRET: &str = "cloud_access_secret";

impl Keys {
    /// Keys given one by one (Moli's store).
    #[must_use]
    pub fn new(id: &str, secret: &str) -> Option<Self> {
        Self::parse(&format!(
            "{id}
{secret}"
        ))
    }

    /// Both lines present, or nothing (the vault does not have them yet).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        let (id, secret) = (lines.next()?, lines.next()?);
        Some(Self {
            id: id.to_owned(),
            secret: secret.to_owned(),
        })
    }

    /// Messages are encrypted with the middle of the secret.
    fn message_key(&self) -> Option<[u8; 16]> {
        self.secret.as_bytes().get(8..24)?.try_into().ok()
    }

    /// The message service's password: `md5(id + md5(secret))`, its middle.
    fn mq_password(&self) -> String {
        let inner = hex(&Md5::digest(self.secret.as_bytes()));
        hex(&Md5::digest(format!("{}{inner}", self.id).as_bytes()))[8..24].to_owned()
    }
}

/// Where the account lives: its API and its message service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub api: &'static str,
    pub mq: &'static str,
}

impl Region {
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        let (api, mq) = match name {
            "eu" => ("openapi.tuyaeu.com", "mqe.tuyaeu.com"),
            "eu-west" => ("openapi-weaz.tuyaeu.com", "mqe-weaz.tuyaeu.com"),
            "us" => ("openapi.tuyaus.com", "mqe.tuyaus.com"),
            "us-east" => ("openapi-ueaz.tuyaus.com", "mqe-ueaz.tuyaus.com"),
            "in" => ("openapi.tuyain.com", "mqe.tuyain.com"),
            "cn" => ("openapi.tuyacn.com", "mqe.tuyacn.com"),
            _ => return None,
        };
        Some(Self { api, mq })
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// What Tuya signs: method, body hash, (no) headers, path with sorted query.
#[must_use]
pub fn string_to_sign(method: &str, body: &[u8], url: &str) -> String {
    format!("{method}\n{}\n\n{url}", hex(digest(&SHA256, body).as_ref()))
}

/// `HMAC-SHA256(secret, id [+ token] + t + nonce + string_to_sign)`, upper hex.
#[must_use]
pub fn sign(keys: &Keys, token: Option<&str>, t: u64, nonce: &str, to_sign: &str) -> String {
    let message = format!("{}{}{t}{nonce}{to_sign}", keys.id, token.unwrap_or(""));
    hex(&hmac_sha256(keys.secret.as_bytes(), message.as_bytes())).to_ascii_uppercase()
}

struct Api {
    keys: Keys,
    region: Region,
    token: Option<(String, Instant)>,
}

impl Api {
    async fn call(&mut self, path: &str) -> anyhow::Result<Json> {
        let token = self.token().await?;
        self.request(path, Some(&token)).await
    }

    async fn token(&mut self) -> anyhow::Result<String> {
        if let Some((token, until)) = &self.token
            && Instant::now() < *until
        {
            return Ok(token.clone());
        }
        let result = self.request("/v1.0/token?grant_type=1", None).await?;
        let token = result["access_token"]
            .as_str()
            .context("no access token")?
            .to_owned();
        // Renewed five minutes early (Tuya gives two hours).
        let life = result["expire_time"]
            .as_u64()
            .unwrap_or(7200)
            .saturating_sub(300);
        self.token = Some((token.clone(), Instant::now() + Duration::from_secs(life)));
        Ok(token)
    }

    async fn request(&self, path: &str, token: Option<&str>) -> anyhow::Result<Json> {
        let t = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis(),
        )?;
        let nonce = hex(&random::<16>());
        let signature = sign(
            &self.keys,
            token,
            t,
            &nonce,
            &string_to_sign("GET", b"", path),
        );
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header("client_id", &self.keys.id)
            .header("sign", signature)
            .header("t", t.to_string())
            .header("nonce", nonce)
            .header("sign_method", "HMAC-SHA256");
        if let Some(token) = token {
            builder = builder.header("access_token", token);
        }
        let request = builder.body(Full::new(moli_net::Body::new()))?;
        let (_, body) = moli_net::web(
            self.region.api,
            443,
            true,
            request,
            Duration::from_secs(10),
            1 << 20,
        )
        .await?;
        let answer: Json = serde_json::from_slice(&body).context("Tuya: not JSON")?;
        if answer["success"] != Json::Bool(true) {
            bail!(
                "Tuya {}: {}",
                answer["code"],
                answer["msg"].as_str().unwrap_or("refused")
            );
        }
        Ok(answer["result"].clone())
    }
}

/// One device's readings as Moli values (unknown codes skipped).
fn readings(device: &TuyaDevice, status: &Json) -> Vec<(String, Value)> {
    status
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let code = s["code"].as_str()?;
            let dp = device.dp_by_code(code)?;
            Some((code.to_owned(), dp.decode(&s["value"])?))
        })
        .collect()
}

/// What a decrypted message says.
#[derive(Debug, PartialEq)]
pub enum Event {
    Status { device: String, status: Json },
    Online { device: String, online: bool },
}

/// A message's content, decrypted (`em` = the encryption, from its properties).
pub fn open(keys: &Keys, payload_b64: &str, em: Option<&str>) -> anyhow::Result<Json> {
    let outer: Json = serde_json::from_slice(&base64(payload_b64).context("payload")?)?;
    let data = base64(outer["data"].as_str().context("no data")?).context("data")?;
    let key = keys.message_key().context("secret too short")?;
    let plain = if em == Some("aes_gcm") {
        ensure!(data.len() > 28, "message too short");
        let iv: [u8; 12] = data[..12].try_into()?;
        gcm_open(&key, iv, b"", &data[12..]).map_err(|_| anyhow::anyhow!("integrity"))?
    } else {
        ecb_decrypt(&key, &data).map_err(|_| anyhow::anyhow!("cannot decrypt"))?
    };
    Ok(serde_json::from_slice(&plain)?)
}

/// The events in a decrypted message, whatever its generation.
#[must_use]
pub fn events(message: &Json) -> Vec<Event> {
    let mut out = Vec::new();
    // Device status (protocol 4): `{devId, status: [{code, value}]}`.
    if let (Some(dev), Some(_)) = (message["devId"].as_str(), message["status"].as_array()) {
        out.push(Event::Status {
            device: dev.to_owned(),
            status: message["status"].clone(),
        });
    }
    // Newer business messages: `{bizCode, bizData: {devId, properties|status}}`.
    let biz = &message["bizData"];
    let dev = biz["devId"].as_str().or_else(|| message["devId"].as_str());
    match (message["bizCode"].as_str(), dev) {
        (Some("online" | "deviceOnline"), Some(dev)) => out.push(Event::Online {
            device: dev.to_owned(),
            online: true,
        }),
        (Some("offline" | "deviceOffline"), Some(dev)) => out.push(Event::Online {
            device: dev.to_owned(),
            online: false,
        }),
        (Some(_), Some(dev)) => {
            for field in ["properties", "status"] {
                if biz[field].is_array() {
                    out.push(Event::Status {
                        device: dev.to_owned(),
                        status: biz[field].clone(),
                    });
                }
            }
        }
        _ => {}
    }
    out
}

/// Standard base64, padding optional.
fn base64(text: &str) -> Option<Vec<u8>> {
    let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buf, mut bits) = (0u32, 0);
    for c in text
        .bytes()
        .filter(|&c| c != b'=' && !c.is_ascii_whitespace())
    {
        buf = (buf << 6) | u32::try_from(table.iter().position(|&t| t == c)?).ok()?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buf >> bits) & 0xFF).ok()?);
        }
    }
    Some(out)
}

/// The devices the cloud speaks for, by Tuya id: (index in the driver, device).
pub type Watched = HashMap<String, (usize, Arc<TuyaDevice>)>;

/// Reads the states, then follows the changes, for ever (reconnecting).
pub async fn watch(keys: Keys, region: Region, devices: Watched, up: mpsc::Sender<Up>) {
    let mut api = Api {
        keys,
        region,
        token: None,
    };
    let mut pause = Duration::from_secs(30);
    loop {
        match follow(&mut api, &devices, &up).await {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(error = %e.root_cause(), "tuya cloud: retrying in {} s", pause.as_secs());
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(Duration::from_secs(600));
            }
        }
    }
}

async fn sync(api: &mut Api, devices: &Watched, up: &mpsc::Sender<Up>) -> anyhow::Result<()> {
    let ids: Vec<&String> = devices.keys().collect();
    for chunk in ids.chunks(BATCH) {
        let list = chunk
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let states = api
            .call(&format!("/v1.0/iot-03/devices/status?device_ids={list}"))
            .await?;
        for item in states.as_array().into_iter().flatten() {
            let Some((index, device)) = item["id"].as_str().and_then(|id| devices.get(id)) else {
                continue;
            };
            let values = readings(device, &item["status"]);
            if !values.is_empty() {
                up.send(Up::State(*index, values)).await?;
            }
        }
        // Online or not, as the cloud last heard from them: a sensor whose
        // battery died keeps its last state, « open » included.
        let mut told = 0;
        for path in [
            format!("/v2.0/cloud/thing/batch?device_ids={list}"),
            format!("/v1.0/iot-03/devices?device_ids={list}"),
        ] {
            match api.call(&path).await {
                Ok(info) => {
                    for (id, online) in online_flags(&info) {
                        if let Some((index, _)) = devices.get(id) {
                            up.send(Up::Online(*index, online)).await?;
                            told += 1;
                        }
                    }
                }
                Err(e) => {
                    let endpoint = path.split('?').next().unwrap_or_default();
                    tracing::debug!(error = %e, endpoint, "tuya cloud: no online state");
                }
            }
            if told > 0 {
                break;
            }
        }
        if told == 0 {
            tracing::warn!("tuya cloud: the devices' online state is unreadable");
        }
    }
    Ok(())
}

/// `(device id, online)` from a device list answer, whatever its shape
/// (`{list: […]}` or `[…]`; `isOnline`, `is_online` or `online`).
fn online_flags(info: &Json) -> Vec<(&str, bool)> {
    let list = info["list"].as_array().or_else(|| info.as_array());
    list.into_iter()
        .flatten()
        .filter_map(|item| {
            let online = ["isOnline", "is_online", "online"]
                .iter()
                .find_map(|k| item[*k].as_bool())?;
            Some((item["id"].as_str()?, online))
        })
        .collect()
}

async fn follow(api: &mut Api, devices: &Watched, up: &mpsc::Sender<Up>) -> anyhow::Result<()> {
    sync(api, devices, up).await?;
    tracing::info!(devices = devices.len(), "tuya cloud: states read");
    let keys = api.keys.clone();
    let path = format!(
        "/ws/v2/consumer/persistent/{0}/out/event/{0}-sub?ackTimeoutMillis=3000&subscriptionType=Failover",
        keys.id
    );
    let password = keys.mq_password();
    let mut ws = WebSocket::connect_public(
        api.region.mq,
        8285,
        &path,
        &[
            ("username", keys.id.as_str()),
            ("password", password.as_str()),
        ],
    )
    .await
    .context("message service")?;
    tracing::info!("tuya cloud: listening to the message service");
    let mut resync = Instant::now() + RESYNC;
    loop {
        if Instant::now() >= resync {
            sync(api, devices, up).await?;
            resync = Instant::now() + RESYNC;
        }
        let text = match ws.recv_text(QUIET).await {
            Ok(text) => text,
            Err(e) if e.to_string().contains("in time") => {
                ws.ping().await?;
                continue;
            }
            Err(e) => return Err(e),
        };
        if up.is_closed() {
            return Ok(());
        }
        let envelope: Json = serde_json::from_str(&text).context("message service: not JSON")?;
        if let Some(id) = envelope["messageId"].as_str() {
            ws.send_text(&serde_json::json!({ "messageId": id }).to_string())
                .await?;
        }
        let Some(payload) = envelope["payload"].as_str() else {
            continue;
        };
        let em = envelope["properties"]["em"].as_str();
        let message = match open(&keys, payload, em) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, em, "tuya cloud: unreadable message");
                continue;
            }
        };
        let found = events(&message);
        if found.is_empty() {
            let shape: Vec<&str> = message
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            tracing::debug!(?shape, "tuya cloud: message without device news");
        }
        for event in found {
            match event {
                Event::Status { device, status } => {
                    if let Some((index, d)) = devices.get(&device) {
                        let values = readings(d, &status);
                        if !values.is_empty() {
                            up.send(Up::State(*index, values)).await?;
                        }
                    }
                }
                Event::Online { device, online } => {
                    if let Some((index, _)) = devices.get(&device) {
                        up.send(Up::Online(*index, online)).await?;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ecb_encrypt, gcm_seal};

    #[test]
    fn online_flags_read_both_answer_shapes() {
        let v2 =
            serde_json::json!([{ "id": "a", "isOnline": false }, { "id": "b", "isOnline": true }]);
        assert_eq!(online_flags(&v2), [("a", false), ("b", true)]);
        let v1 = serde_json::json!({ "list": [{ "id": "c", "online": true }, { "id": "d" }] });
        assert_eq!(
            online_flags(&v1),
            [("c", true)],
            "no flag: unknown, not offline"
        );
    }

    fn keys() -> Keys {
        Keys::parse("abcdefghij0123456789\nSECRETSECRET0123456789abcdefghij\n").unwrap()
    }

    fn b64(bytes: &[u8]) -> String {
        let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(char::from(table[((n >> (18 - 6 * i)) & 63) as usize]));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn keys_come_from_two_lines_or_not_at_all() {
        assert_eq!(keys().id, "abcdefghij0123456789");
        assert!(Keys::parse("\n\n").is_none());
        assert!(Keys::parse("only-an-id\n").is_none());
        assert!(!format!("{:?}", keys()).contains("SECRET"), "never printed");
    }

    #[test]
    fn requests_are_signed_as_tuya_documents() {
        let to_sign = string_to_sign("GET", b"", "/v1.0/token?grant_type=1");
        assert_eq!(
            to_sign,
            "GET\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n\n/v1.0/token?grant_type=1"
        );
        let a = sign(&keys(), None, 1_588_925_778_000, "n", &to_sign);
        let b = sign(&keys(), Some("token"), 1_588_925_778_000, "n", &to_sign);
        assert_eq!(a.len(), 64);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
        );
        assert_ne!(a, b, "the token is part of a business request's signature");
    }

    #[test]
    fn the_message_password_is_md5_of_md5() {
        // md5("SECRETSECRET0123456789abcdefghij") then md5(id + that), middle 16.
        let p = keys().mq_password();
        assert_eq!(p.len(), 16);
        assert!(p.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn messages_open_both_ways_and_say_what_changed() {
        let k = keys();
        let key = k.message_key().unwrap();
        let inner = br#"{"devId":"bf00000000000000000001","status":[{"code":"doorcontact_state","value":true,"t":1}]}"#;
        // Old messages: AES-ECB.
        let outer =
            serde_json::json!({ "data": b64(&ecb_encrypt(&key, inner, true)), "protocol": 4 });
        let msg = open(&k, &b64(outer.to_string().as_bytes()), None).unwrap();
        assert_eq!(
            events(&msg),
            [Event::Status {
                device: "bf00000000000000000001".into(),
                status: serde_json::json!([{"code":"doorcontact_state","value":true,"t":1}])
            }]
        );
        // New ones: AES-GCM, the IV first.
        let iv = [7u8; 12];
        let mut sealed = iv.to_vec();
        sealed.extend(gcm_seal(&key, iv, b"", inner));
        let outer = serde_json::json!({ "data": b64(&sealed), "protocol": 4 });
        let msg = open(&k, &b64(outer.to_string().as_bytes()), Some("aes_gcm")).unwrap();
        assert_eq!(events(&msg).len(), 1);
        // A tampered one is refused.
        let mut bad = sealed.clone();
        bad[20] ^= 1;
        let outer = serde_json::json!({ "data": b64(&bad) });
        assert!(open(&k, &b64(outer.to_string().as_bytes()), Some("aes_gcm")).is_err());
    }

    #[test]
    fn online_and_business_messages_are_understood() {
        // The message service's own names (messaging rules), and older ones.
        let off = serde_json::json!({"bizCode": "deviceOffline", "bizData": {"devId": "x"}});
        assert_eq!(
            events(&off),
            [Event::Online {
                device: "x".into(),
                online: false
            }]
        );
        let online = serde_json::json!({"bizCode": "online", "bizData": {"devId": "x"}});
        assert_eq!(
            events(&online),
            [Event::Online {
                device: "x".into(),
                online: true
            }]
        );
        let props = serde_json::json!({"bizCode": "devicePropertyMessage", "bizData": {"devId": "x", "properties": [{"code": "doorcontact_state", "value": false}]}});
        assert!(matches!(&events(&props)[..], [Event::Status { device, .. }] if device == "x"));
        assert!(events(&serde_json::json!({"hello": 1})).is_empty());
    }
}
