//! The Freebox (Freebox OS's API: the Server v7 « Delta » and the others):
//! its temperatures and fans, the connection (state, rates, public
//! address), its disks and their array, Wi-Fi. Read only.
//!
//! - Found at `mafreebox.freebox.fr` (`host`): `GET /api_version`, in plain
//!   HTTP and without any secret, gives the API's version and the domain its
//!   certificate bears. Every other call goes over HTTPS to the same local
//!   address, the certificate checked in full against Free's own authorities
//!   (`freebox-roots.pem`, published at dev.freebox.fr) for that domain:
//!   nothing pinned, so the certificate the box renews every three months is
//!   taken as it comes.
//! - Authorised once, on the box itself: Moli asks (`login/authorize`), a
//!   person accepts on the Freebox Server's screen, and the app token goes to
//!   the encrypted secret store. It never travels again: each session opens
//!   with HMAC-SHA1(app token, challenge). A token revoked from Freebox OS
//!   (« Gestion des accès ») is forgotten and the box asked again; a token
//!   given by another box (another `uid`) is never sent to this one.
//! - Reading needs no permission of the box: none is asked for.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use anyhow::{Context as _, bail};
use http::Method;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::PinnedHttps;
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};

/// Free's authorities for the Freebox's certificates.
const ROOTS: &str = include_str!("freebox-roots.pem");
/// How Moli is known to the box (its list of authorised applications).
const APP_ID: &str = "fr.moli-os.moli";
const APP_NAME: &str = "Moli";
/// The vault's names: the app token, and the box it was given by.
const APP_TOKEN: &str = "app_token";
const BOX_UID: &str = "box_uid";
/// Asking the box's screen again after nobody answered it.
const ASK_AGAIN: Duration = Duration::from_secs(600);
const AUTHORIZE_POLL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The box's local name or address.
    #[serde(default = "default_host")]
    pub host: String,
    /// Seconds between two readings.
    #[serde(default = "default_poll")]
    pub poll_s: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: default_host(),
            poll_s: default_poll(),
        }
    }
}

fn default_host() -> String {
    "mafreebox.freebox.fr".into()
}

fn default_poll() -> u64 {
    30
}

#[derive(Debug)]
pub struct Freebox {
    config: Config,
}

impl Freebox {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Freebox {
    fn kind(&self) -> &'static str {
        "freebox"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

// ---- the API ----

/// What `GET /api_version` says.
#[derive(Clone, Debug, Deserialize)]
struct Version {
    api_domain: String,
    api_base_url: String,
    /// `api_version`: « 16.1 ».
    #[serde(rename = "api_version")]
    number: String,
    #[serde(default)]
    uid: String,
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    box_model_name: Option<String>,
}

impl Version {
    /// `/api/v16/`: the major version the box speaks.
    fn base(&self) -> anyhow::Result<String> {
        let major = self
            .number
            .split('.')
            .next()
            .and_then(|m| m.parse::<u32>().ok())
            .context("API version")?;
        let base = self.api_base_url.trim_end_matches('/');
        if !base.starts_with('/') || base.contains("..") {
            bail!("unexpected API base {base}");
        }
        Ok(format!("{base}/v{major}/"))
    }
}

/// The box's answer: a result, or a refusal with its code.
#[derive(Debug, PartialEq)]
enum Answer {
    Done(Json),
    Refused(String),
}

/// `{"success": true, "result": …}` or `{"success": false, "error_code": …}`.
fn answer(body: &[u8]) -> anyhow::Result<Answer> {
    let message: Json = serde_json::from_slice(body).context("the box's answer")?;
    if message["success"].as_bool() == Some(true) {
        return Ok(Answer::Done(message["result"].clone()));
    }
    Ok(Answer::Refused(
        message["error_code"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned(),
    ))
}

/// The session password: HMAC-SHA1 of the challenge, keyed by the app token,
/// in hex (Freebox OS's login).
fn password(app_token: &str, challenge: &str) -> String {
    let key = ring::hmac::Key::new(
        ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
        app_token.as_bytes(),
    );
    let tag = ring::hmac::sign(&key, challenge.as_bytes());
    let mut hex = String::with_capacity(40);
    for byte in tag.as_ref() {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

struct Api {
    https: PinnedHttps,
    base: String,
}

impl Api {
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Json>,
        session: Option<&str>,
    ) -> anyhow::Result<Answer> {
        let mut builder = self.https.request(method, &format!("{}{path}", self.base));
        if let Some(session) = session {
            builder = builder.header("x-fbx-app-auth", session);
        }
        let request = match body {
            Some(body) => moli_net::json_body(builder, body)?,
            None => moli_net::empty(builder)?,
        };
        let (_, body) = self.https.send(request).await?;
        answer(&body)
    }
}

async fn version(host: &str) -> anyhow::Result<Version> {
    let request = moli_net::empty(
        http::Request::builder()
            .uri("/api_version")
            .header("host", host),
    )?;
    let (status, body) = moli_net::plain(host, 80, request).await?;
    if !status.is_success() {
        bail!("{host}: /api_version answered {status}");
    }
    serde_json::from_slice(&body).context("/api_version")
}

// ---- what the box says, as points ----

#[derive(Clone, Debug, PartialEq)]
struct Reading {
    key: String,
    label: String,
    value: Value,
    unit: Option<Unit>,
}

fn reading(
    key: impl Into<String>,
    label: impl Into<String>,
    value: Value,
    unit: Option<Unit>,
) -> Reading {
    Reading {
        key: key.into(),
        label: label.into(),
        value,
        unit,
    }
}

/// An id the box gives (`temp_cpum`, `fan0_speed`) as a point's key: lower
/// case letters, digits and `_` only.
fn tidy(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn round(x: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (x * f).round() / f
}

/// `system/`: the temperatures (`sensors`, °C) and fans (`fans`, rpm) as
/// the box lists them (their ids and names vary from box to box; the oldest
/// API had `temp_cpum`, `temp_sw`, `temp_cpub` and `fan_rpm`), the uptime and
/// the firmware.
fn system(result: &Json) -> Vec<Reading> {
    let mut out = Vec::new();
    for sensor in result["sensors"].as_array().into_iter().flatten() {
        let (Some(id), Some(value)) = (sensor["id"].as_str(), sensor["value"].as_f64()) else {
            continue;
        };
        let id = tidy(id);
        let key = if id.starts_with("temp_") {
            id
        } else {
            format!("temp_{id}")
        };
        let label = sensor["name"].as_str().unwrap_or(&key).to_owned();
        out.push(reading(
            key,
            label,
            Value::Float(value),
            Some(Unit::Celsius),
        ));
    }
    for fan in result["fans"].as_array().into_iter().flatten() {
        let (Some(id), Some(value)) = (fan["id"].as_str(), fan["value"].as_f64()) else {
            continue;
        };
        let id = tidy(id);
        let key = if id.starts_with("fan") {
            id
        } else {
            format!("fan_{id}")
        };
        let label = fan["name"].as_str().unwrap_or(&key).to_owned();
        out.push(reading(key, label, Value::Float(value), Unit::parse("rpm")));
    }
    for (id, label) in [
        ("temp_cpum", "CPU M"),
        ("temp_cpub", "CPU B"),
        ("temp_sw", "Switch"),
    ] {
        if let Some(value) = result[id].as_f64()
            && !out.iter().any(|r| r.key == id)
        {
            out.push(reading(id, label, Value::Float(value), Some(Unit::Celsius)));
        }
    }
    if let Some(rpm) = result["fan_rpm"].as_f64()
        && !out.iter().any(|r| r.key.starts_with("fan"))
    {
        out.push(reading(
            "fan_rpm",
            moli_i18n::tr!("pilotes.freebox.ventilateur"),
            Value::Float(rpm),
            Unit::parse("rpm"),
        ));
    }
    if let Some(up) = result["uptime_val"].as_i64() {
        out.push(reading(
            "uptime",
            moli_i18n::tr!("pilotes.freebox.allumee_depuis"),
            Value::Int(up),
            Some(Unit::Second),
        ));
    }
    if let Some(firmware) = result["firmware_version"].as_str() {
        out.push(reading(
            "firmware",
            moli_i18n::tr!("pilotes.freebox.micrologiciel"),
            Value::Text(firmware.into()),
            None,
        ));
    }
    out
}

/// `connection/`: up or not, the rates now (bytes/s → kB/s), the line's
/// bandwidth (bit/s → Mbit/s), the public address.
#[allow(clippy::cast_precision_loss)]
fn connection(result: &Json) -> Vec<Reading> {
    let mut out = Vec::new();
    if let Some(state) = result["state"].as_str() {
        out.push(reading(
            "connected",
            moli_i18n::tr!("pilotes.freebox.internet"),
            Value::Bool(state == "up"),
            None,
        ));
    }
    let kbs = Unit::parse("kB/s");
    let mbits = Unit::parse("Mbit/s");
    for (field, key, label, divide, unit) in [
        (
            "rate_down",
            "rate_down",
            moli_i18n::tr!("pilotes.freebox.debit_descendant"),
            1_000.0,
            &kbs,
        ),
        (
            "rate_up",
            "rate_up",
            moli_i18n::tr!("pilotes.freebox.debit_montant"),
            1_000.0,
            &kbs,
        ),
        (
            "bandwidth_down",
            "line_down",
            moli_i18n::tr!("pilotes.freebox.ligne_descendante"),
            1_000_000.0,
            &mbits,
        ),
        (
            "bandwidth_up",
            "line_up",
            moli_i18n::tr!("pilotes.freebox.ligne_montante"),
            1_000_000.0,
            &mbits,
        ),
    ] {
        if let Some(n) = result[field].as_u64() {
            out.push(reading(
                key,
                label,
                Value::Float(round(n as f64 / divide, 2)),
                unit.clone(),
            ));
        }
    }
    if let Some(ip) = result["ipv4"].as_str().filter(|ip| !ip.is_empty()) {
        out.push(reading(
            "external_ip",
            moli_i18n::tr!("pilotes.freebox.adresse_publique"),
            Value::Text(ip.into()),
            None,
        ));
    }
    out
}

/// `storage/disk/`: each disk's temperature, and the free space of all
/// their partitions together (%).
#[allow(clippy::cast_precision_loss)]
fn disks(result: &Json) -> Vec<Reading> {
    let mut out = Vec::new();
    let (mut free, mut total) = (0u64, 0u64);
    for disk in result.as_array().into_iter().flatten() {
        if let (Some(id), Some(temp)) = (disk["id"].as_i64(), disk["temp"].as_f64())
            && temp > 0.0
        {
            out.push(reading(
                format!("disk_{id}_temp"),
                moli_i18n::tr!("pilotes.freebox.disque", id = id),
                Value::Float(temp),
                Some(Unit::Celsius),
            ));
        }
        for part in disk["partitions"].as_array().into_iter().flatten() {
            if let (Some(f), Some(t)) = (part["free_bytes"].as_u64(), part["total_bytes"].as_u64())
            {
                free += f;
                total += t;
            }
        }
    }
    if total > 0 {
        out.push(reading(
            "free_space",
            moli_i18n::tr!("pilotes.freebox.espace_libre"),
            Value::Float(round(free as f64 * 100.0 / total as f64, 1)),
            Some(Unit::Percent),
        ));
    }
    out
}

/// `storage/raid/`: true when an array is degraded (a disk lost).
fn raid(result: &Json) -> Vec<Reading> {
    let arrays = result.as_array().map_or(&[][..], Vec::as_slice);
    if arrays.is_empty() {
        return Vec::new();
    }
    let degraded = arrays.iter().any(|a| {
        a["degraded"].as_bool().unwrap_or(false)
            || a["state"]
                .as_str()
                .is_some_and(|s| !matches!(s, "running" | "active" | "clean"))
    });
    vec![reading(
        "raid_degraded",
        moli_i18n::tr!("pilotes.freebox.raid_degrade"),
        Value::Bool(degraded),
        None,
    )]
}

/// `wifi/config/`: Wi-Fi on or off.
fn wifi(result: &Json) -> Vec<Reading> {
    result["enabled"]
        .as_bool()
        .map(|on| reading("wifi", "Wi-Fi", Value::Bool(on), None))
        .into_iter()
        .collect()
}

fn spec(r: &Reading) -> PointSpec {
    let kind = match r.value {
        Value::Bool(_) => Kind::Binary,
        Value::Text(_) => Kind::Text,
        _ => Kind::Numeric {
            min: None,
            max: None,
            step: None,
        },
    };
    PointSpec {
        key: r.key.clone().into(),
        label: r.label.clone().into(),
        semantic: Semantic::infer(&r.key, r.unit.as_ref()),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit: r.unit.clone(),
    }
}

fn device(
    ctx: &DriverCtx,
    id: &DeviceId,
    v: &Version,
    points: &BTreeMap<String, PointSpec>,
) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: v
            .device_name
            .clone()
            .unwrap_or_else(|| "Freebox".into())
            .into(),
        manufacturer: Some("Free".into()),
        model: v.box_model_name.clone().map(Into::into),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: points.values().cloned().collect(),
    }
}

// ---- the driver ----

/// Shows `reason` and idles until shutdown.
async fn park(ctx: &DriverCtx, reason: String) -> anyhow::Result<()> {
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

fn secrets_problem(ctx: &DriverCtx) -> String {
    ctx.secrets_problem().map_or_else(
        || moli_i18n::tr!("pilotes.freebox.coffre_indisponible"),
        str::to_owned,
    )
}

/// Asks the box for an app token, a person accepting it on the Freebox
/// Server's screen. `None`: shutting down.
async fn authorize(api: &Api, ctx: &DriverCtx, uid: &str) -> anyhow::Result<Option<String>> {
    loop {
        let request = json!({
            "app_id": APP_ID,
            "app_name": APP_NAME,
            "app_version": env!("CARGO_PKG_VERSION"),
            "device_name": format!("Moli ({})", ctx.instance()),
        });
        let asked = match api
            .call(Method::POST, "login/authorize/", Some(&request), None)
            .await?
        {
            Answer::Done(result) => result,
            Answer::Refused(code) => bail!("the box refused to be asked: {code}"),
        };
        let token = asked["app_token"]
            .as_str()
            .context("no app token")?
            .to_owned();
        let track = asked["track_id"].as_u64().context("no track id")?;
        ctx.wait_for(moli_i18n::tr!("pilotes.freebox.autoriser"));
        tracing::info!(instance = %ctx.instance(), "waiting for the Freebox's screen");
        let status = loop {
            tokio::select! {
                () = ctx.cancelled() => return Ok(None),
                () = tokio::time::sleep(AUTHORIZE_POLL) => {}
            }
            match api
                .call(Method::GET, &format!("login/authorize/{track}"), None, None)
                .await?
            {
                Answer::Done(result) => match result["status"].as_str() {
                    Some("pending") => {}
                    Some(other) => break other.to_owned(),
                    None => bail!("no authorization status"),
                },
                Answer::Refused(code) => bail!("authorization not followed: {code}"),
            }
        };
        match status.as_str() {
            "granted" => {
                ctx.store_secret(BOX_UID, uid).await?;
                ctx.store_secret(APP_TOKEN, &token).await?;
                tracing::info!(instance = %ctx.instance(), "Freebox authorised");
                return Ok(Some(token));
            }
            "denied" => {
                park(ctx, moli_i18n::tr!("pilotes.freebox.refusee")).await?;
                return Ok(None);
            }
            // Nobody at the screen in time: asked again later.
            _ => {
                ctx.wait_for(moli_i18n::tr!(
                    "pilotes.freebox.pas_de_reponse",
                    minutes = ASK_AGAIN.as_secs() / 60
                ));
                tokio::select! {
                    () = ctx.cancelled() => return Ok(None),
                    () = tokio::time::sleep(ASK_AGAIN) => {}
                }
            }
        }
    }
}

/// Why a session could not open.
enum Login {
    Open(String),
    /// The app token is no longer valid (revoked from Freebox OS).
    Revoked,
}

async fn login(api: &Api, token: &str) -> anyhow::Result<Login> {
    let challenge = match api.call(Method::GET, "login/", None, None).await? {
        Answer::Done(result) => result["challenge"]
            .as_str()
            .context("no challenge")?
            .to_owned(),
        Answer::Refused(code) => bail!("no challenge: {code}"),
    };
    let request = json!({ "app_id": APP_ID, "password": password(token, &challenge) });
    match api
        .call(Method::POST, "login/session/", Some(&request), None)
        .await?
    {
        Answer::Done(result) => Ok(Login::Open(
            result["session_token"]
                .as_str()
                .context("no session token")?
                .to_owned(),
        )),
        Answer::Refused(code) if code == "invalid_token" || code == "pending_token" => {
            Ok(Login::Revoked)
        }
        Answer::Refused(code) => bail!("session refused: {code}"),
    }
}

/// The disks' temperatures once: the box's own sensors (`temp_hdd*`, with
/// their names) when it has them, else those of the disk list.
fn without_twins(mut readings: Vec<Reading>) -> Vec<Reading> {
    if readings.iter().any(|r| r.key.starts_with("temp_hdd")) {
        readings.retain(|r| !(r.key.starts_with("disk_") && r.key.ends_with("_temp")));
    }
    readings
}

/// One reading of everything. `Err(None)`: the session ran out (opened
/// again by the caller).
async fn read_all(api: &Api, session: &str) -> Result<Vec<Reading>, Option<anyhow::Error>> {
    let mut out = Vec::new();
    for (path, parse) in [
        ("system/", system as fn(&Json) -> Vec<Reading>),
        ("connection/", connection),
        ("storage/disk/", disks),
        ("storage/raid/", raid),
        ("wifi/config/", wifi),
    ] {
        match api
            .call(Method::GET, path, None, Some(session))
            .await
            .map_err(Some)?
        {
            Answer::Done(result) => out.extend(parse(&result)),
            Answer::Refused(code) if code == "auth_required" => return Err(None),
            // A box without it (no disk, no array), or a right it keeps.
            Answer::Refused(code) => tracing::debug!(path, %code, "freebox: not read"),
        }
    }
    Ok(without_twins(out))
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let found = version(&config.host)
        .await
        .with_context(|| format!("{}: Freebox OS API", config.host))?;
    let api = Api {
        https: PinnedHttps::with_roots(&config.host, 443, &found.api_domain, ROOTS)?,
        base: found.base()?,
    };
    if !ctx.can_store_secrets() {
        let reason = moli_i18n::tr!(
            "pilotes.freebox.cle_non_enregistrable",
            problem = secrets_problem(ctx)
        );
        return park(ctx, reason).await;
    }
    // A token given by another box is never sent to this one.
    if ctx.secret(BOX_UID).is_some_and(|uid| uid != found.uid) {
        ctx.forget_secret(APP_TOKEN).await?;
        ctx.forget_secret(BOX_UID).await?;
    }
    let id = ctx.device_id("box");
    let mut points: BTreeMap<String, PointSpec> = BTreeMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(config.poll_s.max(5)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut session: Option<String> = None;
    let mut ready = false;
    loop {
        let token = match ctx.secret(APP_TOKEN) {
            Some(token) => token,
            None => match authorize(&api, ctx, &found.uid).await? {
                Some(token) => token,
                None => return Ok(()),
            },
        };
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => {
                    command.reply(Err(moli_i18n::tr!("pilotes.freebox.lecture_seule")));
                    continue;
                }
                None => return Ok(()),
            },
            _ = tick.tick() => {}
        }
        if session.is_none() {
            match login(&api, &token).await {
                Ok(Login::Open(s)) => session = Some(s),
                Ok(Login::Revoked) => {
                    tracing::warn!(instance = %ctx.instance(), "Freebox app token revoked: asking again");
                    ctx.forget_secret(APP_TOKEN).await?;
                    continue;
                }
                Err(e) => {
                    ctx.set_availability(&id, false);
                    tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "Freebox unreachable");
                    continue;
                }
            }
        }
        let Some(open) = session.as_deref() else {
            continue;
        };
        match read_all(&api, open).await {
            Ok(readings) => {
                let fresh = readings.iter().any(|r| !points.contains_key(&r.key));
                for r in &readings {
                    points.entry(r.key.clone()).or_insert_with(|| spec(r));
                }
                if fresh {
                    ctx.upsert_device(device(ctx, &id, &found, &points));
                }
                ctx.set_availability(&id, true);
                for r in readings {
                    ctx.set_state(&id, &r.key, r.value);
                }
                if !ready {
                    ready = true;
                    ctx.ready();
                }
            }
            Err(None) => session = None,
            Err(Some(e)) => {
                session = None;
                ctx.set_availability(&id, false);
                tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "Freebox not read");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_password_is_hmac_sha1_in_hex() {
        // RFC 2202, test case 2.
        assert_eq!(
            password("Jefe", "what do ya want for nothing?"),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
    }

    #[test]
    fn answers_and_refusals() {
        assert_eq!(
            answer(br#"{"success":true,"result":{"challenge":"c"}}"#).unwrap(),
            Answer::Done(json!({"challenge": "c"}))
        );
        assert_eq!(
            answer(br#"{"success":false,"error_code":"auth_required","msg":"x"}"#).unwrap(),
            Answer::Refused("auth_required".into())
        );
        assert!(answer(b"<html>").is_err());
    }

    #[test]
    fn the_api_base_follows_the_major_version() {
        let v: Version = serde_json::from_value(json!({
            "api_domain": "abcd.fbxos.fr", "api_base_url": "/api/", "api_version": "16.1",
            "uid": "u", "device_name": "Freebox Server", "box_model_name": "Freebox v7 (r1)"
        }))
        .unwrap();
        assert_eq!(v.base().unwrap(), "/api/v16/");
        let odd = Version {
            api_base_url: "/../x".into(),
            ..v.clone()
        };
        assert!(odd.base().is_err());
        let none = Version {
            number: "x".into(),
            ..v
        };
        assert!(none.base().is_err());
    }

    #[test]
    fn temperatures_fans_and_the_oldest_fields() {
        let new = system(&json!({
            "sensors": [{"id": "temp_cpum", "name": "Température CPU M", "value": 73},
                        {"id": "temp_hdd0", "name": "Disque", "value": 38}],
            "fans": [{"id": "fan0_speed", "name": "Ventilateur 1", "value": 1531}],
            "uptime_val": 120, "firmware_version": "4.9.12"
        }));
        let keys: Vec<&str> = new.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            ["temp_cpum", "temp_hdd0", "fan0_speed", "uptime", "firmware"]
        );
        assert_eq!(new[0].value, Value::Float(73.0));
        assert_eq!(new[0].label, "Température CPU M");
        let old = system(&json!({"temp_cpum": 60, "temp_sw": 50, "fan_rpm": 2000}));
        let keys: Vec<&str> = old.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["temp_cpum", "temp_sw", "fan_rpm"]);
    }

    #[test]
    fn the_connection_in_the_units_of_the_dashboard() {
        let r = connection(&json!({
            "state": "up", "rate_down": 59_840, "rate_up": 11_300,
            "bandwidth_down": 8_000_000_000_u64, "bandwidth_up": 700_000_000, "ipv4": "198.51.100.7"
        }));
        let get = |k: &str| r.iter().find(|x| x.key == k).unwrap().value.clone();
        assert_eq!(get("connected"), Value::Bool(true));
        assert_eq!(get("rate_down"), Value::Float(59.84));
        assert_eq!(get("line_down"), Value::Float(8000.0));
        assert_eq!(get("external_ip"), Value::Text("198.51.100.7".into()));
        assert_eq!(
            connection(&json!({"state": "down"}))[0].value,
            Value::Bool(false)
        );
    }

    #[test]
    fn disks_their_array_and_wifi() {
        let r = disks(&json!([
            {"id": 2, "temp": 37, "partitions": [{"free_bytes": 400, "total_bytes": 1000}]},
            {"id": 4, "temp": 0, "partitions": [{"free_bytes": 100, "total_bytes": 1000}]}
        ]));
        let keys: Vec<&str> = r.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(
            keys,
            ["disk_2_temp", "free_space"],
            "a disk at 0 °C is asleep, not cold"
        );
        assert_eq!(r[1].value, Value::Float(25.0));
        assert!(disks(&json!([])).is_empty());
        assert_eq!(
            raid(&json!([{"state": "running", "degraded": false}]))[0].value,
            Value::Bool(false)
        );
        assert_eq!(
            raid(&json!([{"state": "running", "degraded": true}]))[0].value,
            Value::Bool(true)
        );
        assert_eq!(
            raid(&json!([{"state": "failed"}]))[0].value,
            Value::Bool(true)
        );
        assert!(raid(&json!([])).is_empty());
        assert_eq!(
            wifi(&json!({"enabled": false}))[0].value,
            Value::Bool(false)
        );
    }

    #[test]
    fn free_s_authorities_are_both_read() {
        assert_eq!(ROOTS.matches("-----BEGIN CERTIFICATE-----").count(), 2);
        assert!(PinnedHttps::with_roots("192.0.2.1", 443, "abcd.fbxos.fr", ROOTS).is_ok());
    }

    #[test]
    fn ids_become_keys() {
        assert_eq!(tidy("Temp-CPU M"), "tempcpum");
        assert_eq!(tidy("fan0_speed"), "fan0_speed");
    }

    #[test]
    fn a_disk_s_temperature_once() {
        let both = [
            reading(
                "temp_hdd1",
                "Disque dur 2",
                Value::Float(38.0),
                Some(Unit::Celsius),
            ),
            reading(
                "disk_1000_temp",
                "Disque 1000",
                Value::Float(38.0),
                Some(Unit::Celsius),
            ),
            reading(
                "free_space",
                "Espace libre",
                Value::Float(50.0),
                Some(Unit::Percent),
            ),
        ];
        let keys = |r: Vec<Reading>| r.into_iter().map(|x| x.key).collect::<Vec<_>>();
        assert_eq!(
            keys(without_twins(both.to_vec())),
            ["temp_hdd1", "free_space"]
        );
        assert_eq!(
            keys(without_twins(both[1..].to_vec())),
            ["disk_1000_temp", "free_space"]
        );
    }
}
