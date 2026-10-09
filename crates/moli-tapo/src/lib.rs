//! TP-Link Tapo bulbs and plugs, 100 % local, through the KLAP protocol
//! (v2 handshake): power, brightness, colour temperature, colour.
//!
//! The secret is the account's *auth hash* — `sha256(sha1(user) +
//! sha1(password))` — never the password itself; it lives in the encrypted
//! store as base64 (`auth_hash`), exactly as Home Assistant keeps it.

use std::time::Duration;

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt as _, BlockEncrypt as _, KeyInit as _};
use anyhow::{Context as _, bail};
use http::Method;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::Deserialize;
use serde_json::{Value as Json, json};

const AUTH_HASH: &str = "auth_hash";
const POLL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    80
}

#[derive(Debug)]
pub struct Tapo {
    config: Config,
}

impl Tapo {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Tapo {
    fn kind(&self) -> &'static str {
        "tapo"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let joined: Vec<u8> = parts.concat();
    let mut out = [0u8; 32];
    out.copy_from_slice(digest(&SHA256, &joined).as_ref());
    out
}

/// A KLAP session: key, IV base, signature key and request counter.
struct Session {
    key: [u8; 16],
    iv: [u8; 12],
    sig: [u8; 28],
    seq: i32,
    cookie: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("seq", &self.seq)
            .finish_non_exhaustive()
    }
}

impl Session {
    fn derive(local: &[u8; 16], remote: &[u8; 16], auth: &[u8; 32], cookie: String) -> Self {
        let mut key = [0u8; 16];
        key.copy_from_slice(&sha256(&[b"lsk", local, remote, auth])[..16]);
        let full_iv = sha256(&[b"iv", local, remote, auth]);
        let mut iv = [0u8; 12];
        iv.copy_from_slice(&full_iv[..12]);
        let seq = i32::from_be_bytes([full_iv[28], full_iv[29], full_iv[30], full_iv[31]]);
        let mut sig = [0u8; 28];
        sig.copy_from_slice(&sha256(&[b"ldk", local, remote, auth])[..28]);
        Self {
            key,
            iv,
            sig,
            seq,
            cookie,
        }
    }

    fn cbc_iv(&self) -> [u8; 16] {
        let mut iv = [0u8; 16];
        iv[..12].copy_from_slice(&self.iv);
        iv[12..].copy_from_slice(&self.seq.to_be_bytes());
        iv
    }

    /// Signature + ciphertext, and the sequence number it carries.
    fn encrypt(&mut self, plain: &[u8]) -> (Vec<u8>, i32) {
        self.seq = self.seq.wrapping_add(1);
        let cipher = Aes128::new(GenericArray::from_slice(&self.key));
        let mut data = plain.to_vec();
        let pad = 16 - data.len() % 16;
        data.extend(std::iter::repeat_n(u8::try_from(pad).unwrap_or(16), pad));
        let mut previous = self.cbc_iv();
        for block in data.as_chunks_mut::<16>().0 {
            for (b, p) in block.iter_mut().zip(previous) {
                *b ^= p;
            }
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
            previous = *block;
        }
        let signature = sha256(&[&self.sig, &self.seq.to_be_bytes(), &data]);
        ([&signature[..], &data].concat(), self.seq)
    }

    fn decrypt(&self, message: &[u8]) -> anyhow::Result<Vec<u8>> {
        let data = message.get(32..).context("short answer")?;
        if data.is_empty() || !data.len().is_multiple_of(16) {
            bail!("answer is not whole blocks");
        }
        let cipher = Aes128::new(GenericArray::from_slice(&self.key));
        let mut out = data.to_vec();
        let mut previous = self.cbc_iv();
        for block in out.as_chunks_mut::<16>().0 {
            let encrypted = *block;
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
            for (b, p) in block.iter_mut().zip(previous) {
                *b ^= p;
            }
            previous = encrypted;
        }
        let pad = usize::from(*out.last().context("empty")?);
        if pad == 0 || pad > 16 || pad > out.len() {
            bail!(moli_i18n::tr!("pilotes.tapo.mauvais_bourrage"));
        }
        out.truncate(out.len() - pad);
        Ok(out)
    }
}

async fn post(
    host: &str,
    port: u16,
    path: &str,
    cookie: Option<&str>,
    body: Vec<u8>,
) -> anyhow::Result<(http::StatusCode, http::HeaderMap, moli_net::Body)> {
    let mut builder = http::Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(
            "host",
            if port == 80 {
                host.to_owned()
            } else {
                format!("{host}:{port}")
            },
        )
        .header("content-type", "application/octet-stream");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    let request = builder.body(http_body_util::Full::new(moli_net::Body::from(body)))?;
    moli_net::plain_with_headers(host, port, request).await
}

async fn handshake(host: &str, port: u16, auth: &[u8; 32]) -> anyhow::Result<Session> {
    let mut local = [0u8; 16];
    SystemRandom::new()
        .fill(&mut local)
        .map_err(|_| anyhow::anyhow!("no randomness"))?;
    let (status, headers, body) = post(host, port, "/app/handshake1", None, local.to_vec()).await?;
    if !status.is_success() || body.len() < 48 {
        bail!(moli_i18n::tr!("pilotes.tapo.handshake1", status = status));
    }
    let mut remote = [0u8; 16];
    remote.copy_from_slice(&body[..16]);
    if sha256(&[&local, &remote, auth]) != body[16..48] {
        bail!(moli_i18n::tr!("pilotes.tapo.identifiants_refuses"));
    }
    let cookie = headers
        .get_all(http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .find(|c| c.starts_with("TP_SESSIONID="))
        .context("no session cookie")?
        .to_owned();
    let (status, _, _) = post(
        host,
        port,
        "/app/handshake2",
        Some(&cookie),
        sha256(&[&remote, &local, auth]).to_vec(),
    )
    .await?;
    if !status.is_success() {
        bail!(moli_i18n::tr!("pilotes.tapo.handshake2", status = status));
    }
    Ok(Session::derive(&local, &remote, auth, cookie))
}

struct Bulb {
    host: String,
    port: u16,
    auth: [u8; 32],
    session: Option<Session>,
}

impl Bulb {
    async fn call(&mut self, request: &Json) -> anyhow::Result<Json> {
        let mut last = None;
        for _ in 0..2 {
            match self.attempt(request).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // Expired or invalidated session, bulb restarted…: a
                    // fresh handshake, once.
                    self.session = None;
                    last = Some(e);
                }
            }
        }
        Err(last.expect("two attempts"))
    }

    async fn attempt(&mut self, request: &Json) -> anyhow::Result<Json> {
        if self.session.is_none() {
            self.session = Some(handshake(&self.host, self.port, &self.auth).await?);
        }
        let session = self.session.as_mut().expect("just ensured");
        let (body, seq) = session.encrypt(request.to_string().as_bytes());
        let cookie = session.cookie.clone();
        let (status, _, bytes) = post(
            &self.host,
            self.port,
            &format!("/app/request?seq={seq}"),
            Some(&cookie),
            body,
        )
        .await?;
        if !status.is_success() {
            bail!(moli_i18n::tr!(
                "pilotes.tapo.ampoule_repond",
                status = status
            ));
        }
        let session = self.session.as_ref().expect("just ensured");
        let reply: Json = serde_json::from_slice(&session.decrypt(&bytes)?)?;
        let code = reply["error_code"].as_i64().unwrap_or(0);
        if code != 0 {
            bail!(moli_i18n::tr!("pilotes.tapo.ampoule_refuse", code = code));
        }
        Ok(reply["result"].clone())
    }
}

fn spec(key: &str, label: &str, kind: Kind, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: true,
        },
        unit,
        semantic,
    }
}

fn range(min: f64, max: f64) -> Kind {
    Kind::Numeric {
        min: Some(min),
        max: Some(max),
        step: Some(1.0),
    }
}

fn device(ctx: &DriverCtx, id: &DeviceId, info: &Json) -> Device {
    let name = info["nickname"]
        .as_str()
        .and_then(base64_decode)
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_else(|| moli_i18n::tr!("pilotes.tapo.ampoule"));
    let mut points = vec![spec(
        "on",
        &moli_i18n::tr!("pilotes.tapo.allumee"),
        Kind::Binary,
        None,
        Semantic::OnOff,
    )];
    if info.get("brightness").is_some() {
        points.push(spec(
            "brightness",
            &moli_i18n::tr!("pilotes.tapo.luminosite"),
            range(1.0, 100.0),
            Some(Unit::Percent),
            Semantic::Brightness,
        ));
    }
    if info.get("color_temp").is_some() {
        points.push(spec(
            "color_temp",
            &moli_i18n::tr!("pilotes.tapo.temperature_couleur"),
            range(2500.0, 6500.0),
            Unit::parse("K"),
            Semantic::infer("color_temp", None),
        ));
    }
    if info.get("hue").is_some() {
        points.push(spec(
            "hue",
            &moli_i18n::tr!("pilotes.tapo.teinte"),
            range(0.0, 360.0),
            None,
            Semantic::infer("hue", None),
        ));
        points.push(spec(
            "saturation",
            &moli_i18n::tr!("pilotes.tapo.saturation"),
            range(0.0, 100.0),
            Some(Unit::Percent),
            Semantic::infer("saturation", None),
        ));
    }
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: name.into(),
        manufacturer: Some("TP-Link".into()),
        model: info["model"].as_str().map(Into::into),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
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

fn values(info: &Json) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    if let Some(on) = info["device_on"].as_bool() {
        out.push(("on", Value::Bool(on)));
    }
    for key in ["brightness", "color_temp", "hue", "saturation"] {
        if let Some(v) = info[key].as_i64() {
            out.push((key, Value::Int(v)));
        }
    }
    out
}

async fn apply(bulb: &mut Bulb, command: &CommandRequest) -> anyhow::Result<()> {
    let params = match (&*command.key, &command.value) {
        ("on", Value::Bool(on)) => json!({ "device_on": on }),
        (key @ ("brightness" | "color_temp" | "hue" | "saturation"), v) => {
            #[allow(clippy::cast_possible_truncation)]
            let n = v.as_f64().context("number")?.round() as i64;
            let mut params = json!({ key: n });
            // A colour change cancels the colour temperature, and the
            // reverse.
            if key == "hue" || key == "saturation" {
                params["color_temp"] = json!(0);
            }
            params
        }
        (key, _) => bail!(moli_i18n::tr!("pilotes.tapo.ne_se_regle_pas", point = key)),
    };
    bulb.call(&json!({ "method": "set_device_info", "params": params }))
        .await?;
    Ok(())
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let Some(auth) = ctx
        .secret(AUTH_HASH)
        .as_deref()
        .and_then(base64_decode)
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
    else {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.tapo.hash_manquant",
            instance = ctx.instance(),
            key = AUTH_HASH
        ));
        ctx.cancelled().await;
        return Ok(());
    };
    let mut bulb = Bulb {
        host: config.host.clone(),
        port: config.port,
        auth,
        session: None,
    };
    // A bulb switched off at the wall: wait for it, refusing orders.
    let info = loop {
        match bulb.call(&json!({ "method": "get_device_info" })).await {
            Ok(info) => break info,
            Err(e) => {
                ctx.wait_for(moli_i18n::tr!(
                    "pilotes.tapo.injoignable_nouvel_essai",
                    error = format!("{e:#}")
                ));
                let wake = tokio::time::sleep(Duration::from_secs(60));
                tokio::pin!(wake);
                loop {
                    tokio::select! {
                        () = &mut wake => break,
                        command = ctx.next_command() => match command {
                            Some(command) => command.reply(Err(moli_i18n::tr!("pilotes.tapo.injoignable"))),
                            None => return Ok(()),
                        },
                    }
                }
            }
        }
    };
    let native = info["device_id"]
        .as_str()
        .unwrap_or(&config.host)
        .to_owned();
    let id = ctx.device_id(&native);
    ctx.upsert_device(device(ctx, &id, &info));
    ctx.ready();
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut online = None;
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                let result = apply(&mut bulb, &command).await.map_err(|e| format!("{e:#}"));
                if result.is_ok() {
                    ctx.set_state(&id, &command.key, command.value.clone());
                }
                command.reply(result);
            }
            _ = tick.tick() => match bulb.call(&json!({ "method": "get_device_info" })).await {
                Ok(info) => {
                    if online != Some(true) {
                        online = Some(true);
                        ctx.set_availability(&id, true);
                    }
                    for (key, value) in values(&info) {
                        ctx.set_state(&id, key, value);
                    }
                }
                Err(e) => {
                    if online != Some(false) {
                        online = Some(false);
                        ctx.set_availability(&id, false);
                        tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "tapo unreachable");
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_matches_python_kasa_byte_for_byte() {
        // Generated with python-kasa's KlapEncryptionSession (the library
        // Home Assistant runs) and a dummy auth hash.
        let local: [u8; 16] = std::array::from_fn(|i| u8::try_from(i).unwrap());
        let remote: [u8; 16] = std::array::from_fn(|i| u8::try_from(i + 16).unwrap());
        let auth = sha256(&[b"test-auth"]);
        let mut session = Session::derive(&local, &remote, &auth, String::new());
        let (payload, seq) = session.encrypt(br#"{"method":"get_device_info"}"#);
        assert_eq!(seq, 1_857_442_340);
        let hex = payload.iter().fold(String::new(), |mut out, b| {
            use std::fmt::Write as _;
            let _ = write!(out, "{b:02x}");
            out
        });
        assert_eq!(
            hex,
            "40fdc393ec66a8c5b8054c87de40de802f781b51d0e7aeaa203c67ad8b7530017fa34caad325815b6bb902108eaa6929dd0420d99100acbedfe385b2ac0242b1"
        );
        assert_eq!(
            session.decrypt(&payload).unwrap(),
            br#"{"method":"get_device_info"}"#
        );
    }

    #[test]
    fn device_info_reads_as_points() {
        let info = json!({"device_on": true, "brightness": 40, "color_temp": 2700, "hue": 0, "saturation": 100,
                          "nickname": "VG9pbGV0dGVz", "model": "L530", "device_id": "80221"});
        let v: std::collections::HashMap<_, _> = values(&info).into_iter().collect();
        assert_eq!(v["on"], Value::Bool(true));
        assert_eq!(v["brightness"], Value::Int(40));
        assert_eq!(base64_decode("VG9pbGV0dGVz").unwrap(), b"Toilettes");
    }
}
