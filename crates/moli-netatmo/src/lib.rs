//! Netatmo Security cameras (`NACamera` Welcome indoor, `NOC` Presence
//! outdoor with floodlight, `NDB` Doorbell).
//!
//! Credentials (`client_id`, `client_secret`, `refresh_token` from
//! <https://dev.netatmo.com/apps/>) live in Moli's encrypted secret store and
//! the rotated `refresh_token` is saved back at every token renewal.
//! Live snapshots are fetched directly on the home network when the camera's
//! local URL answers (`<local_url>/live/snapshot_720.jpg`), falling back to
//! the camera's `vpn_url` when needed.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Method, Request, StatusCode};
use http_body_util::Full;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_net::{Body as Bytes, WebUrl};
use moli_runtime::media::{Image, SnapshotSource};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::time::{Instant, MissedTickBehavior};

const API_HOST: &str = "api.netatmo.com";
const USER_AGENT: &str = "MoliOS/0.1";
const CLIENT_ID_KEY: &str = "client_id";
const CLIENT_SECRET_KEY: &str = "client_secret";
const REFRESH_KEY: &str = "refresh_token";

const HTTP_LIMIT: Duration = Duration::from_secs(15);
const SNAPSHOT_LIMIT: Duration = Duration::from_secs(8);
const MAX_BODY: usize = 4 * 1024 * 1024;
const POLL_STATUS: Duration = Duration::from_secs(30);
const POLL_HOMES: Duration = Duration::from_secs(30 * 60);
/// Netatmo access tokens last 3 hours (10800 s): renew after 2 hours.
const TOKEN_LIFETIME: Duration = Duration::from_secs(2 * 3600);
/// How long a camera detection stays `true` on the card after an event.
const EVENT_WINDOW_SECS: i64 = 180;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Optional Netatmo home ID if the account has multiple homes.
    #[serde(default)]
    pub home_id: Option<String>,
}

#[derive(Debug)]
pub struct Netatmo {
    config: Config,
}

impl Netatmo {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Netatmo {
    fn kind(&self) -> &'static str {
        "netatmo"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

#[derive(Debug, Deserialize)]
struct TokenPair {
    access_token: String,
    refresh_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CameraKind {
    Presence,
    Welcome,
    Advance,
    Doorbell,
}

impl CameraKind {
    fn from_netatmo(kind: &str) -> Option<Self> {
        match kind {
            "NOC" => Some(Self::Presence),
            "NACamera" => Some(Self::Welcome),
            "NPC" => Some(Self::Advance),
            "NDB" => Some(Self::Doorbell),
            _ => None,
        }
    }

    const fn model_name(&self) -> &'static str {
        match self {
            Self::Presence => "Presence",
            Self::Welcome => "Welcome",
            Self::Advance => "Advance",
            Self::Doorbell => "Doorbell",
        }
    }

    const fn has_floodlight(&self) -> bool {
        matches!(self, Self::Presence)
    }
}

#[derive(Clone, Debug, Default)]
struct StreamUrls {
    local_url: Option<String>,
    vpn_url: Option<String>,
}

#[derive(Debug)]
struct CameraSnapshot {
    urls: Arc<RwLock<StreamUrls>>,
}

impl SnapshotSource for CameraSnapshot {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<Image>> {
        Box::pin(async move {
            let (local, vpn) = {
                let guard = self
                    .urls
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (guard.local_url.clone(), guard.vpn_url.clone())
            };
            if let Some(base) = &local
                && let Ok(img) = fetch_jpeg(&format!("{base}/live/snapshot_720.jpg")).await
            {
                return Ok(img);
            }
            if let Some(base) = &vpn {
                return fetch_jpeg(&format!("{base}/live/snapshot_720.jpg")).await;
            }
            bail!(moli_i18n::tr!("pilotes.netatmo.aucune_image"))
        })
    }
}

async fn fetch_jpeg(raw_url: &str) -> anyhow::Result<Image> {
    let parsed = WebUrl::parse(raw_url)?;
    let req = Request::builder()
        .method(Method::GET)
        .uri(&parsed.path)
        .header("accept", "image/jpeg, */*")
        .body(Full::new(Bytes::new()))?;
    let (status, bytes) = moli_net::web(
        &parsed.host,
        parsed.port,
        parsed.tls,
        req,
        SNAPSHOT_LIMIT,
        MAX_BODY,
    )
    .await?;
    if !status.is_success() || !bytes.starts_with(&[0xFF, 0xD8]) {
        bail!(moli_i18n::tr!(
            "pilotes.netatmo.aucune_image_statut",
            status = status.as_u16()
        ));
    }
    Ok(Image {
        content_type: "image/jpeg".into(),
        bytes: bytes.to_vec(),
    })
}

#[derive(Clone, Debug)]
pub struct CameraInfo {
    pub home_id: String,
    pub mac_id: String,
    pub name: String,
    pub room: Option<String>,
    pub kind: CameraKind,
    urls: Arc<RwLock<StreamUrls>>,
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(k);
        out.push('=');
        for b in v.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(char::from(b));
            } else {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

enum TokenOutcome {
    Ok(TokenPair),
    InvalidCredentials(String),
}

fn looks_corrupted(secret: &str) -> bool {
    secret.is_empty()
        || secret.len() > 256
        || secret
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, ';' | '$' | '"' | '\'' | '`'))
}

async fn refresh_access_token(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> anyhow::Result<TokenOutcome> {
    if looks_corrupted(client_id)
        || looks_corrupted(client_secret)
        || looks_corrupted(refresh_token)
    {
        return Ok(TokenOutcome::InvalidCredentials(
            "secret contains spaces or shell characters".into(),
        ));
    }
    let body = form_encode(&[
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("refresh_token", refresh_token),
    ]);
    let bytes_body = body.into_bytes();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/oauth2/token")
        .header("user-agent", USER_AGENT)
        .header(
            "content-type",
            "application/x-www-form-urlencoded;charset=UTF-8",
        )
        .header("content-length", bytes_body.len().to_string())
        .header("accept", "application/json")
        .body(Full::new(Bytes::from(bytes_body)))?;
    let (status, bytes) = moli_net::web(API_HOST, 443, true, req, HTTP_LIMIT, MAX_BODY).await?;
    if matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        let detail = String::from_utf8_lossy(&bytes[..bytes.len().min(160)]).into_owned();
        return Ok(TokenOutcome::InvalidCredentials(format!(
            "HTTP {} {detail}",
            status.as_u16()
        )));
    }
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).into_owned();
        bail!("netatmo oauth2/token: HTTP {} {detail}", status.as_u16());
    }
    let pair: TokenPair =
        serde_json::from_slice(&bytes).context("invalid netatmo token response")?;
    Ok(TokenOutcome::Ok(pair))
}

async fn api_call(
    method: Method,
    path: &str,
    token: &str,
    body: Option<&Json>,
) -> anyhow::Result<(StatusCode, Json)> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("user-agent", USER_AGENT)
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json");
    let payload = if let Some(b) = body {
        let raw = b.to_string().into_bytes();
        builder = builder
            .header("content-type", "application/json; charset=utf-8")
            .header("content-length", raw.len().to_string());
        Bytes::from(raw)
    } else {
        Bytes::new()
    };
    let req = builder.body(Full::new(payload))?;
    let (status, bytes) = moli_net::web(API_HOST, 443, true, req, HTTP_LIMIT, MAX_BODY).await?;
    let json = serde_json::from_slice(&bytes).unwrap_or(Json::Null);
    Ok((status, json))
}

async fn resolve_local_url(vpn_url: &str) -> Option<String> {
    let ping_url = format!("{}/command/ping", vpn_url.trim_end_matches('/'));
    let parsed = WebUrl::parse(&ping_url).ok()?;
    let req = Request::builder()
        .method(Method::GET)
        .uri(&parsed.path)
        .header("accept", "application/json")
        .body(Full::new(Bytes::new()))
        .ok()?;
    let (status, bytes) = moli_net::web(
        &parsed.host,
        parsed.port,
        parsed.tls,
        req,
        SNAPSHOT_LIMIT,
        MAX_BODY,
    )
    .await
    .ok()?;
    if !status.is_success() {
        return None;
    }
    let json: Json = serde_json::from_slice(&bytes).ok()?;
    let local = json["local_url"].as_str()?.trim_end_matches('/').to_owned();
    if local.is_empty() {
        return None;
    }
    let local_ping = WebUrl::parse(&format!("{local}/command/ping")).ok()?;
    let check_req = Request::builder()
        .method(Method::GET)
        .uri(&local_ping.path)
        .header("accept", "application/json")
        .body(Full::new(Bytes::new()))
        .ok()?;
    let (local_status, _) = moli_net::web(
        &local_ping.host,
        local_ping.port,
        local_ping.tls,
        check_req,
        Duration::from_secs(3),
        MAX_BODY,
    )
    .await
    .ok()?;
    local_status.is_success().then_some(local)
}

pub fn parse_homesdata(body: &Json, filter_home: Option<&str>) -> Vec<CameraInfo> {
    let Some(homes) = body["body"]["homes"].as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for home in homes {
        let Some(home_id) = home["id"].as_str() else {
            continue;
        };
        if filter_home.is_some_and(|want| want != home_id) {
            continue;
        }
        let mut rooms: HashMap<&str, &str> = HashMap::new();
        if let Some(r_list) = home["rooms"].as_array() {
            for r in r_list {
                if let (Some(id), Some(name)) = (r["id"].as_str(), r["name"].as_str()) {
                    rooms.insert(id, name);
                }
            }
        }
        let Some(modules) = home["modules"].as_array() else {
            continue;
        };
        for m in modules {
            let Some(kind) = m["type"].as_str().and_then(CameraKind::from_netatmo) else {
                continue;
            };
            let Some(mac_id) = m["id"].as_str() else {
                continue;
            };
            let name = m["name"]
                .as_str()
                .unwrap_or_else(|| kind.model_name())
                .trim()
                .to_owned();
            let room = m["room_id"]
                .as_str()
                .and_then(|rid| rooms.get(rid).copied())
                .map(str::to_owned);
            out.push(CameraInfo {
                home_id: home_id.to_owned(),
                mac_id: mac_id.to_owned(),
                name,
                room,
                kind,
                urls: Arc::new(RwLock::new(StreamUrls::default())),
            });
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedModuleStatus {
    pub online: bool,
    pub monitoring: Option<bool>,
    pub floodlight: Option<String>,
    pub light_on: Option<bool>,
    pub vpn_url: Option<String>,
}

pub fn decode_module_status(module: &Json) -> DecodedModuleStatus {
    let reachable = module["reachable"].as_bool().unwrap_or(true);
    let alim_ok = module["alim_status"].as_str() != Some("off");
    let online = reachable && alim_ok;
    let monitoring = module["monitoring"].as_str().map(|s| s == "on");
    let floodlight = module["floodlight"]
        .as_str()
        .filter(|m| matches!(*m, "auto" | "on" | "off"))
        .map(str::to_owned);
    let light_on = module["light_mode_status"]
        .as_str()
        .or_else(|| module["floodlight"].as_str())
        .map(|s| s == "on");
    let vpn_url = module["vpn_url"]
        .as_str()
        .map(|s| s.trim_end_matches('/').to_owned());
    DecodedModuleStatus {
        online,
        monitoring,
        floodlight,
        light_on,
        vpn_url,
    }
}

fn build_device(ctx: &DriverCtx, id: &DeviceId, cam: &CameraInfo) -> Device {
    let mut points = vec![
        PointSpec {
            key: "monitoring".into(),
            label: moli_i18n::tr!("pilotes.netatmo.surveillance").into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: Semantic::Config,
        },
        PointSpec {
            key: "person".into(),
            label: moli_i18n::tr!("pilotes.netatmo.personne").into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: Semantic::Occupancy,
        },
        PointSpec {
            key: "vehicle".into(),
            label: moli_i18n::tr!("pilotes.netatmo.vehicule").into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: Semantic::Occupancy,
        },
        PointSpec {
            key: "animal".into(),
            label: moli_i18n::tr!("pilotes.netatmo.animal").into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: Semantic::Occupancy,
        },
        PointSpec {
            key: "motion".into(),
            label: moli_i18n::tr!("pilotes.netatmo.mouvement").into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: Semantic::Occupancy,
        },
    ];
    if cam.kind.has_floodlight() {
        points.push(PointSpec {
            key: "floodlight".into(),
            label: moli_i18n::tr!("pilotes.netatmo.projecteur").into(),
            kind: Kind::Enum {
                values: vec!["auto".into(), "on".into(), "off".into()],
            },
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: Semantic::Config,
        });
        points.push(PointSpec {
            key: "light".into(),
            label: moli_i18n::tr!("pilotes.netatmo.lumiere").into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: Semantic::OnOff,
        });
    }
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: cam.name.clone().into(),
        manufacturer: Some("Netatmo".into()),
        model: Some(cam.kind.model_name().into()),
        description: Some(moli_i18n::tr!("pilotes.netatmo.camera").into()),
        native_room: cam.room.as_deref().map(Into::into),
        members: Vec::new(),
        points,
    }
}

fn native_id(mac: &str) -> String {
    mac.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

async fn ensure_token(
    ctx: &mut DriverCtx,
    client_id: &str,
    client_secret: &str,
    cached: &mut Option<(String, Instant)>,
) -> anyhow::Result<Option<String>> {
    if let Some((tok, born)) = cached
        && born.elapsed() < TOKEN_LIFETIME
    {
        return Ok(Some(tok.clone()));
    }
    let refresh = ctx
        .secret(REFRESH_KEY)
        .context("missing netatmo refresh_token")?;
    match refresh_access_token(client_id, client_secret, &refresh).await? {
        TokenOutcome::Ok(pair) => {
            if !pair.refresh_token.is_empty() && pair.refresh_token != refresh {
                ctx.store_secret(REFRESH_KEY, &pair.refresh_token).await?;
            }
            let tok = pair.access_token;
            *cached = Some((tok.clone(), Instant::now()));
            Ok(Some(tok))
        }
        TokenOutcome::InvalidCredentials(why) => {
            tracing::warn!(instance = %ctx.instance(), reason = %why, "netatmo credentials rejected: clearing vault keys");
            ctx.forget_secret(CLIENT_ID_KEY).await?;
            ctx.forget_secret(CLIENT_SECRET_KEY).await?;
            ctx.forget_secret(REFRESH_KEY).await?;
            *cached = None;
            Ok(None)
        }
    }
}

async fn sync_cameras(
    config: &Config,
    ctx: &mut DriverCtx,
    token: &str,
    cameras: &mut HashMap<DeviceId, CameraInfo>,
) -> anyhow::Result<()> {
    let (status, body) = api_call(Method::GET, "/api/homesdata", token, None).await?;
    if !status.is_success() {
        bail!("/api/homesdata: HTTP {}", status.as_u16());
    }
    let discovered = parse_homesdata(&body, config.home_id.as_deref());
    let mut seen = std::collections::HashSet::new();
    for mut cam in discovered {
        let id = ctx.device_id(&native_id(&cam.mac_id));
        if let Some(prev) = cameras.get(&id) {
            cam.urls = Arc::clone(&prev.urls);
        }
        ctx.upsert_device(build_device(ctx, &id, &cam));
        ctx.provide_snapshots(
            &id,
            Arc::new(CameraSnapshot {
                urls: Arc::clone(&cam.urls),
            }),
        );
        seen.insert(id.clone());
        cameras.insert(id, cam);
    }
    cameras.retain(|id, _| seen.contains(id));
    Ok(())
}

async fn poll_all_homes(
    ctx: &mut DriverCtx,
    token: &str,
    cameras: &HashMap<DeviceId, CameraInfo>,
) -> anyhow::Result<()> {
    let mut home_ids: Vec<String> = cameras.values().map(|c| c.home_id.clone()).collect();
    home_ids.sort();
    home_ids.dedup();

    for home_id in home_ids {
        let status_path = format!("/api/homestatus?home_id={home_id}");
        let (status, body) = api_call(Method::GET, &status_path, token, None).await?;
        if !status.is_success() {
            bail!("{status_path}: HTTP {}", status.as_u16());
        }
        if let Some(modules) = body["body"]["home"]["modules"].as_array() {
            for m in modules {
                let Some(mac) = m["id"].as_str() else {
                    continue;
                };
                let id = ctx.device_id(&native_id(mac));
                let Some(cam) = cameras.get(&id) else {
                    continue;
                };
                let decoded = decode_module_status(m);
                ctx.set_availability(&id, decoded.online);
                if let Some(mon) = decoded.monitoring {
                    ctx.set_state(&id, "monitoring", Value::Bool(mon));
                }
                if let Some(fl) = &decoded.floodlight {
                    ctx.set_state(&id, "floodlight", Value::Text(fl.clone().into()));
                }
                if let Some(lit) = decoded.light_on {
                    ctx.set_state(&id, "light", Value::Bool(lit));
                }
                if let Some(vpn) = decoded.vpn_url {
                    let changed = {
                        let guard = cam
                            .urls
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        guard.vpn_url.as_deref() != Some(vpn.as_str()) || guard.local_url.is_none()
                    };
                    if changed {
                        let local = resolve_local_url(&vpn).await;
                        let mut guard = cam
                            .urls
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        guard.vpn_url = Some(vpn);
                        if local.is_some() {
                            guard.local_url = local;
                        }
                    }
                }
            }
        }

        let events_path = format!("/api/getevents?home_id={home_id}&size=15");
        if let Ok((ev_status, ev_body)) = api_call(Method::GET, &events_path, token, None).await
            && ev_status.is_success()
        {
            update_events(ctx, cameras, &home_id, &ev_body);
        }
    }
    Ok(())
}

fn update_events(
    ctx: &DriverCtx,
    cameras: &HashMap<DeviceId, CameraInfo>,
    home_id: &str,
    ev_body: &Json,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let mut active: HashMap<DeviceId, (bool, bool, bool, bool)> = cameras
        .iter()
        .filter(|(_, c)| c.home_id == home_id)
        .map(|(id, _)| (id.clone(), (false, false, false, false)))
        .collect();

    if let Some(events) = ev_body["body"]["home"]["events"].as_array() {
        for ev in events {
            let Some(mac) = ev["module_id"]
                .as_str()
                .or_else(|| ev["camera_id"].as_str())
            else {
                continue;
            };
            let id = ctx.device_id(&native_id(mac));
            let Some(flags) = active.get_mut(&id) else {
                continue;
            };
            let ts = ev["time"].as_i64().unwrap_or(0);
            if now - ts > EVENT_WINDOW_SECS {
                continue;
            }
            let mut apply_kind = |kind: &str| match kind {
                "human" | "person" | "person_away" => {
                    flags.0 = true;
                    flags.3 = true;
                }
                "vehicle" => {
                    flags.1 = true;
                    flags.3 = true;
                }
                "animal" => {
                    flags.2 = true;
                    flags.3 = true;
                }
                "movement" | "outdoor" => {
                    flags.3 = true;
                }
                _ => {}
            };
            if let Some(t) = ev["type"].as_str() {
                apply_kind(t);
            }
            if let Some(subs) = ev["event_list"].as_array() {
                for sub in subs {
                    let sub_ts = sub["time"].as_i64().unwrap_or(ts);
                    if now - sub_ts <= EVENT_WINDOW_SECS
                        && let Some(t) = sub["type"].as_str()
                    {
                        apply_kind(t);
                    }
                }
            }
        }
    }

    for (id, (person, vehicle, animal, motion)) in active {
        ctx.set_state(&id, "person", Value::Bool(person));
        ctx.set_state(&id, "vehicle", Value::Bool(vehicle));
        ctx.set_state(&id, "animal", Value::Bool(animal));
        ctx.set_state(&id, "motion", Value::Bool(motion));
    }
}

async fn handle_command(
    ctx: &mut DriverCtx,
    token: &str,
    cameras: &HashMap<DeviceId, CameraInfo>,
    cmd: CommandRequest,
) {
    let id = cmd.device.id.clone();
    let Some(cam) = cameras.get(&id) else {
        cmd.reply(Err(moli_i18n::tr!("pilotes.netatmo.camera_inconnue")));
        return;
    };
    let key = cmd.key.clone();
    let value = cmd.value.clone();
    let outcome: anyhow::Result<()> = async {
        match (&*key, &value) {
            ("monitoring", Value::Bool(on)) => {
                let state_str = if *on { "on" } else { "off" };
                let body = json!({
                    "home": {
                        "id": cam.home_id,
                        "modules": [{
                            "id": cam.mac_id,
                            "monitoring": state_str,
                        }]
                    }
                });
                let (status, ans) =
                    api_call(Method::POST, "/api/setstate", token, Some(&body)).await?;
                if !status.is_success() {
                    bail!("HTTP {} {}", status.as_u16(), ans);
                }
                ctx.set_state(&id, "monitoring", Value::Bool(*on));
            }
            ("floodlight", Value::Text(mode))
                if cam.kind.has_floodlight() && matches!(&**mode, "auto" | "on" | "off") =>
            {
                let body = json!({
                    "home": {
                        "id": cam.home_id,
                        "modules": [{
                            "id": cam.mac_id,
                            "floodlight": &**mode,
                        }]
                    }
                });
                let (status, ans) =
                    api_call(Method::POST, "/api/setstate", token, Some(&body)).await?;
                if !status.is_success() {
                    bail!("HTTP {} {}", status.as_u16(), ans);
                }
                ctx.set_state(&id, "floodlight", Value::Text(mode.clone()));
                ctx.set_state(&id, "light", Value::Bool(&**mode == "on"));
            }
            _ => {
                bail!(moli_i18n::tr!(
                    "pilotes.netatmo.point_inconnu",
                    point = &*key
                ));
            }
        }
        Ok(())
    }
    .await;

    match outcome {
        Ok(()) => cmd.reply(Ok(())),
        Err(e) => cmd.reply(Err(moli_i18n::tr!(
            "pilotes.netatmo.echec",
            why = format!("{e:#}")
        ))),
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let (Some(client_id), Some(client_secret), Some(_)) = (
        ctx.secret(CLIENT_ID_KEY),
        ctx.secret(CLIENT_SECRET_KEY),
        ctx.secret(REFRESH_KEY),
    ) else {
        let missing = [CLIENT_ID_KEY, CLIENT_SECRET_KEY, REFRESH_KEY]
            .into_iter()
            .filter(|k| ctx.secret(k).is_none())
            .collect::<Vec<_>>()
            .join(", ");
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.netatmo.identifiants_manquants",
            instance = ctx.instance(),
            missing = missing
        ));
        ctx.cancelled().await;
        return Ok(());
    };

    let mut cached_token: Option<(String, Instant)> = None;
    let Some(initial_token) =
        ensure_token(ctx, &client_id, &client_secret, &mut cached_token).await?
    else {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.netatmo.identifiants_manquants",
            instance = ctx.instance(),
            missing = "client_id, client_secret, refresh_token"
        ));
        ctx.cancelled().await;
        return Ok(());
    };

    let mut cameras: HashMap<DeviceId, CameraInfo> = HashMap::new();
    sync_cameras(config, ctx, &initial_token, &mut cameras).await?;
    poll_all_homes(ctx, &initial_token, &cameras).await?;
    ctx.ready();

    let mut status_tick = tokio::time::interval(POLL_STATUS);
    status_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    status_tick.tick().await;

    let mut homes_tick = tokio::time::interval(POLL_HOMES);
    homes_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    homes_tick.tick().await;

    loop {
        tokio::select! {
            _ = status_tick.tick() => {
                let Some(token) = ensure_token(ctx, &client_id, &client_secret, &mut cached_token).await? else {
                    return Ok(());
                };
                poll_all_homes(ctx, &token, &cameras).await?;
            }
            _ = homes_tick.tick() => {
                let Some(token) = ensure_token(ctx, &client_id, &client_secret, &mut cached_token).await? else {
                    return Ok(());
                };
                sync_cameras(config, ctx, &token, &mut cameras).await?;
            }
            cmd = ctx.next_command() => {
                let Some(cmd) = cmd else { return Ok(()) };
                let Some(token) = ensure_token(ctx, &client_id, &client_secret, &mut cached_token).await? else {
                    cmd.reply(Err(moli_i18n::tr!("pilotes.netatmo.identifiants_manquants", instance = ctx.instance(), missing = REFRESH_KEY)));
                    return Ok(());
                };
                handle_command(ctx, &token, &cameras, cmd).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_homesdata_and_homestatus_fixture() {
        let fixture: Json = serde_json::from_slice(include_bytes!(
            "../../../integrations/netatmo/fixtures/homestatus.json"
        ))
        .unwrap();
        let cams = parse_homesdata(&fixture["homesdata"], None);
        assert_eq!(cams.len(), 2);
        assert_eq!(cams[0].name, "Jardin");
        assert_eq!(cams[0].kind, CameraKind::Presence);
        assert_eq!(cams[0].room.as_deref(), Some("Extérieur"));
        assert_eq!(cams[1].name, "Entrée");
        assert_eq!(cams[1].kind, CameraKind::Welcome);

        let modules = fixture["homestatus"]["body"]["home"]["modules"]
            .as_array()
            .unwrap();
        let st0 = decode_module_status(&modules[0]);
        assert!(st0.online);
        assert_eq!(st0.monitoring, Some(true));
        assert_eq!(st0.floodlight.as_deref(), Some("auto"));
        assert_eq!(st0.light_on, Some(false));
    }
}
