//! tado° driver (thermostats, radiator valves and air conditioning zones).
//!
//! Authentication uses tado°'s OAuth2 Device Code flow (`login.tado.com`):
//! on first start, Moli asks for a device code and displays the verification
//! URL in the driver's waiting status. Once a person signs in on `login.tado.com`,
//! the refresh token is kept encrypted in Moli's secret store (`refresh_token`)
//! and rotated automatically whenever a new access token is requested.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Method, Request, StatusCode};
use http_body_util::Full;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::Body as Bytes;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::time::{Instant, MissedTickBehavior};

const LOGIN_HOST: &str = "login.tado.com";
const API_HOST: &str = "my.tado.com";
const CLIENT_ID: &str = "1bb50063-6b0c-4d11-bd99-387f4a91cc46";
const REFRESH_KEY: &str = "refresh_token";

const HTTP_LIMIT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 2 * 1024 * 1024;
const POLL_STATE: Duration = Duration::from_secs(60);
const POLL_ZONES: Duration = Duration::from_secs(30 * 60);
/// Access tokens last 10 minutes (599 s): renew after 8 minutes.
const TOKEN_LIFETIME: Duration = Duration::from_secs(8 * 60);

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Optional tado° home ID when the account has several homes; by default
    /// every home of the account is loaded.
    #[serde(default)]
    pub home_id: Option<i64>,
}

#[derive(Debug)]
pub struct Tado {
    config: Config,
}

impl Tado {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Tado {
    fn kind(&self) -> &'static str {
        "tado"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

#[derive(Debug, Deserialize)]
struct DeviceAuth {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default = "default_expires")]
    expires_in: u64,
}

const fn default_interval() -> u64 {
    5
}

const fn default_expires() -> u64 {
    300
}

#[derive(Debug, Deserialize)]
struct TokenPair {
    access_token: String,
    refresh_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZoneKind {
    Heating,
    AirConditioning,
}

impl ZoneKind {
    fn from_tado(kind: &str) -> Option<Self> {
        match kind {
            "HEATING" => Some(Self::Heating),
            "AIR_CONDITIONING" => Some(Self::AirConditioning),
            _ => None,
        }
    }

    const fn tado_type(&self) -> &'static str {
        match self {
            Self::Heating => "HEATING",
            Self::AirConditioning => "AIR_CONDITIONING",
        }
    }

    fn modes(&self) -> Vec<std::sync::Arc<str>> {
        match self {
            Self::Heating => vec!["auto".into(), "chaud".into()],
            Self::AirConditioning => vec![
                "auto".into(),
                "froid".into(),
                "chaud".into(),
                "déshumidification".into(),
                "ventilation".into(),
            ],
        }
    }

    const fn bounds(&self) -> (f64, f64) {
        match self {
            Self::Heating => (5.0, 25.0),
            Self::AirConditioning => (16.0, 30.0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ZoneInfo {
    pub home_id: i64,
    pub zone_id: i64,
    pub name: String,
    pub kind: ZoneKind,
    pub model: String,
    pub battery_low: bool,
    pub last_target: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedState {
    pub online: bool,
    pub on: bool,
    pub mode: &'static str,
    pub target_temperature: Option<f64>,
    pub temperature: Option<f64>,
    pub humidity: Option<f64>,
    pub heating_power: Option<f64>,
    pub open_window: bool,
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

async fn post_form(
    host: &str,
    path: &str,
    pairs: &[(&str, &str)],
) -> anyhow::Result<(StatusCode, Json)> {
    let body = form_encode(pairs);
    let req = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(Full::new(Bytes::from(body.into_bytes())))?;
    let (status, bytes) = moli_net::web(host, 443, true, req, HTTP_LIMIT, MAX_BODY).await?;
    let json = serde_json::from_slice(&bytes).unwrap_or(Json::Null);
    Ok((status, json))
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
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json");
    let payload = if let Some(b) = body {
        builder = builder.header("content-type", "application/json; charset=utf-8");
        Bytes::from(b.to_string().into_bytes())
    } else {
        Bytes::new()
    };
    let req = builder.body(Full::new(payload))?;
    let (status, bytes) = moli_net::web(API_HOST, 443, true, req, HTTP_LIMIT, MAX_BODY).await?;
    let json = serde_json::from_slice(&bytes).unwrap_or(Json::Null);
    Ok((status, json))
}

async fn start_device_auth() -> anyhow::Result<DeviceAuth> {
    let (status, json) = post_form(
        LOGIN_HOST,
        "/oauth2/device_authorize",
        &[("client_id", CLIENT_ID), ("scope", "offline_access")],
    )
    .await?;
    if !status.is_success() {
        bail!("tado device_authorize: HTTP {}", status.as_u16());
    }
    serde_json::from_value(json).context("invalid device_authorize response")
}

enum PollOutcome {
    Pending,
    Authorized(TokenPair),
    Expired,
}

async fn poll_device_token(device_code: &str) -> anyhow::Result<PollOutcome> {
    let (status, json) = post_form(
        LOGIN_HOST,
        "/oauth2/token",
        &[
            ("client_id", CLIENT_ID),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
        ],
    )
    .await?;
    if status.is_success() {
        let pair: TokenPair = serde_json::from_value(json).context("invalid token response")?;
        return Ok(PollOutcome::Authorized(pair));
    }
    let err = json["error"].as_str().unwrap_or_default();
    if matches!(err, "authorization_pending" | "slow_down") {
        return Ok(PollOutcome::Pending);
    }
    if err == "expired_token" {
        return Ok(PollOutcome::Expired);
    }
    bail!("tado oauth2/token: HTTP {} ({err})", status.as_u16())
}

enum RefreshOutcome {
    Refreshed(TokenPair),
    InvalidGrant,
}

async fn refresh_access_token(refresh_token: &str) -> anyhow::Result<RefreshOutcome> {
    let (status, json) = post_form(
        LOGIN_HOST,
        "/oauth2/token",
        &[
            ("client_id", CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )
    .await?;
    if status.is_success() {
        let pair: TokenPair = serde_json::from_value(json).context("invalid refresh response")?;
        return Ok(RefreshOutcome::Refreshed(pair));
    }
    if matches!(status, StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED) {
        return Ok(RefreshOutcome::InvalidGrant);
    }
    bail!("tado refresh_token: HTTP {}", status.as_u16())
}

async fn authorize(ctx: &mut DriverCtx) -> anyhow::Result<Option<TokenPair>> {
    loop {
        let auth = start_device_auth().await?;
        let wait_msg = moli_i18n::tr!(
            "pilotes.tado.autoriser",
            url = auth.verification_uri_complete.as_str(),
            code = auth.user_code.as_str()
        );
        tracing::warn!(
            instance = %ctx.instance(),
            url = %auth.verification_uri_complete,
            code = %auth.user_code,
            "tado: waiting for account authorization"
        );
        ctx.wait_for(wait_msg);
        let deadline = Instant::now() + Duration::from_secs(auth.expires_in.saturating_sub(5));
        let mut tick = tokio::time::interval(Duration::from_secs(auth.interval.max(3)));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tokio::select! {
                cmd = ctx.next_command() => {
                    let Some(cmd) = cmd else { return Ok(None) };
                    cmd.reply(Err(moli_i18n::tr!("pilotes.tado.pas_connecte")));
                }
                _ = tick.tick() => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    match poll_device_token(&auth.device_code).await? {
                        PollOutcome::Pending => {}
                        PollOutcome::Expired => break,
                        PollOutcome::Authorized(pair) => {
                            ctx.store_secret(REFRESH_KEY, &pair.refresh_token).await?;
                            tracing::info!(instance = %ctx.instance(), "tado account authorized");
                            return Ok(Some(pair));
                        }
                    }
                }
            }
        }
    }
}

pub fn parse_zones(home_id: i64, list: &Json) -> Vec<ZoneInfo> {
    let Some(arr) = list.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr {
        let Some(zone_id) = item["id"].as_i64() else {
            continue;
        };
        let Some(kind) = item["type"].as_str().and_then(ZoneKind::from_tado) else {
            continue;
        };
        let name = item["name"].as_str().unwrap_or("tado°").trim().to_owned();
        let devices = item["devices"].as_array();
        let model = devices
            .and_then(|d| d.first())
            .and_then(|d| d["deviceType"].as_str())
            .unwrap_or("Thermostat")
            .to_owned();
        let battery_low = devices.is_some_and(|ds| {
            ds.iter()
                .any(|d| d["batteryState"].as_str().is_some_and(|s| s != "NORMAL"))
        });
        out.push(ZoneInfo {
            home_id,
            zone_id,
            name,
            kind,
            model,
            battery_low,
            last_target: 20.0,
        });
    }
    out
}

pub fn decode_zone_state(kind: &ZoneKind, state: &Json) -> DecodedState {
    let online = state["link"]["state"].as_str() != Some("OFFLINE");
    let power = state["setting"]["power"].as_str().unwrap_or("OFF");
    let on = power == "ON";
    let has_overlay = !state["overlay"].is_null();
    let mode = if has_overlay {
        match kind {
            ZoneKind::Heating => "chaud",
            ZoneKind::AirConditioning => match state["setting"]["mode"].as_str() {
                Some("COOL") => "froid",
                Some("HEAT") => "chaud",
                Some("DRY") => "déshumidification",
                Some("FAN") => "ventilation",
                _ => "auto",
            },
        }
    } else {
        "auto"
    };
    let target_temperature = state["setting"]["temperature"]["celsius"]
        .as_f64()
        .filter(|v| v.is_finite());
    let temperature = state["sensorDataPoints"]["insideTemperature"]["celsius"]
        .as_f64()
        .filter(|v| v.is_finite());
    let humidity = state["sensorDataPoints"]["humidity"]["percentage"]
        .as_f64()
        .filter(|v| v.is_finite());
    let heating_power = state["activityDataPoints"]["heatingPower"]["percentage"]
        .as_f64()
        .filter(|v| v.is_finite());
    let open_window =
        state["openWindowDetected"].as_bool().unwrap_or(false) || !state["openWindow"].is_null();
    DecodedState {
        online,
        on,
        mode,
        target_temperature,
        temperature,
        humidity,
        heating_power,
        open_window,
    }
}

#[allow(clippy::too_many_lines)] // one entry per point
fn build_device(ctx: &DriverCtx, id: &DeviceId, zone: &ZoneInfo) -> Device {
    let (min, max) = zone.kind.bounds();
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: zone.name.clone().into(),
        manufacturer: Some("tado°".into()),
        model: Some(zone.model.clone().into()),
        description: Some(moli_i18n::tr!("pilotes.tado.thermostat").into()),
        native_room: Some(zone.name.clone().into()),
        members: Vec::new(),
        points: vec![
            PointSpec {
                key: "on".into(),
                label: moli_i18n::tr!("pilotes.tado.marche").into(),
                kind: Kind::Binary,
                access: Access {
                    read: true,
                    write: true,
                },
                unit: None,
                semantic: Semantic::OnOff,
            },
            PointSpec {
                key: "mode".into(),
                label: moli_i18n::tr!("pilotes.tado.mode").into(),
                kind: Kind::Enum {
                    values: zone.kind.modes(),
                },
                access: Access {
                    read: true,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Other,
            },
            PointSpec {
                key: "target_temperature".into(),
                label: moli_i18n::tr!("pilotes.tado.consigne").into(),
                kind: Kind::Numeric {
                    min: Some(min),
                    max: Some(max),
                    step: Some(0.5),
                },
                access: Access {
                    read: true,
                    write: true,
                },
                unit: Some(Unit::Celsius),
                semantic: Semantic::Config,
            },
            PointSpec {
                key: "temperature".into(),
                label: moli_i18n::tr!("pilotes.tado.temperature").into(),
                kind: Kind::Numeric {
                    min: None,
                    max: None,
                    step: None,
                },
                access: Access {
                    read: true,
                    write: false,
                },
                unit: Some(Unit::Celsius),
                semantic: Semantic::Temperature,
            },
            PointSpec {
                key: "outdoor_temperature".into(),
                label: moli_i18n::tr!("pilotes.tado.temperature_exterieure").into(),
                kind: Kind::Numeric {
                    min: None,
                    max: None,
                    step: None,
                },
                access: Access {
                    read: true,
                    write: false,
                },
                unit: Some(Unit::Celsius),
                semantic: Semantic::Temperature,
            },
            PointSpec {
                key: "humidity".into(),
                label: moli_i18n::tr!("pilotes.tado.humidite").into(),
                kind: Kind::Numeric {
                    min: None,
                    max: None,
                    step: None,
                },
                access: Access {
                    read: true,
                    write: false,
                },
                unit: Some(Unit::Percent),
                semantic: Semantic::Humidity,
            },
            PointSpec {
                key: "heating_power".into(),
                label: moli_i18n::tr!("pilotes.tado.puissance_chauffe").into(),
                kind: Kind::Numeric {
                    min: None,
                    max: None,
                    step: None,
                },
                access: Access {
                    read: true,
                    write: false,
                },
                unit: Some(Unit::Percent),
                semantic: Semantic::Other,
            },
            PointSpec {
                key: "open_window".into(),
                label: moli_i18n::tr!("pilotes.tado.fenetre_ouverte").into(),
                kind: Kind::Binary,
                access: Access {
                    read: true,
                    write: false,
                },
                unit: None,
                semantic: Semantic::Contact,
            },
            PointSpec {
                key: "battery_low".into(),
                label: moli_i18n::tr!("pilotes.tado.pile_faible").into(),
                kind: Kind::Binary,
                access: Access {
                    read: true,
                    write: false,
                },
                unit: None,
                semantic: Semantic::BatteryLow,
            },
        ],
    }
}

pub fn overlay_body(
    zone: &ZoneInfo,
    power_on: bool,
    target_celsius: f64,
    mode: Option<&str>,
) -> Json {
    if !power_on {
        return json!({
            "setting": {
                "type": zone.kind.tado_type(),
                "power": "OFF",
            },
            "termination": {
                "typeSkillBasedApp": "MANUAL",
            }
        });
    }
    let rounded = (target_celsius * 2.0).round() / 2.0;
    match zone.kind {
        ZoneKind::Heating => json!({
            "setting": {
                "type": "HEATING",
                "power": "ON",
                "temperature": { "celsius": rounded },
            },
            "termination": {
                "typeSkillBasedApp": "MANUAL",
            }
        }),
        ZoneKind::AirConditioning => {
            let ac_mode = match mode {
                Some("froid") => "COOL",
                Some("déshumidification") => "DRY",
                Some("ventilation") => "FAN",
                _ => "HEAT",
            };
            json!({
                "setting": {
                    "type": "AIR_CONDITIONING",
                    "power": "ON",
                    "mode": ac_mode,
                    "temperature": { "celsius": rounded },
                },
                "termination": {
                    "typeSkillBasedApp": "MANUAL",
                }
            })
        }
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    if ctx.secret(REFRESH_KEY).is_none() && !ctx.can_store_secrets() {
        ctx.wait_for(moli_i18n::tr!("pilotes.tado.coffre_indisponible"));
        ctx.cancelled().await;
        return Ok(());
    }

    let mut access_token: Option<(String, Instant)> = None;
    loop {
        let token = if let Some((tok, born)) = &access_token
            && born.elapsed() < TOKEN_LIFETIME
        {
            tok.clone()
        } else if let Some(refresh) = ctx.secret(REFRESH_KEY) {
            match refresh_access_token(&refresh).await? {
                RefreshOutcome::Refreshed(pair) => {
                    ctx.store_secret(REFRESH_KEY, &pair.refresh_token).await?;
                    let tok = pair.access_token;
                    access_token = Some((tok.clone(), Instant::now()));
                    tok
                }
                RefreshOutcome::InvalidGrant => {
                    tracing::warn!(instance = %ctx.instance(), "tado refresh token rejected: re-authorizing");
                    ctx.forget_secret(REFRESH_KEY).await?;
                    access_token = None;
                    continue;
                }
            }
        } else {
            let Some(pair) = authorize(ctx).await? else {
                return Ok(());
            };
            let tok = pair.access_token;
            access_token = Some((tok.clone(), Instant::now()));
            tok
        };

        if let Err(e) = connected(config, ctx, &mut access_token, token).await {
            tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "tado session error");
            return Err(e);
        }
        return Ok(());
    }
}

async fn ensure_token(
    ctx: &mut DriverCtx,
    cached: &mut Option<(String, Instant)>,
) -> anyhow::Result<String> {
    if let Some((tok, born)) = cached
        && born.elapsed() < TOKEN_LIFETIME
    {
        return Ok(tok.clone());
    }
    let refresh = ctx
        .secret(REFRESH_KEY)
        .context("missing tado refresh_token")?;
    match refresh_access_token(&refresh).await? {
        RefreshOutcome::Refreshed(pair) => {
            ctx.store_secret(REFRESH_KEY, &pair.refresh_token).await?;
            let tok = pair.access_token;
            *cached = Some((tok.clone(), Instant::now()));
            Ok(tok)
        }
        RefreshOutcome::InvalidGrant => {
            ctx.forget_secret(REFRESH_KEY).await?;
            *cached = None;
            bail!("tado refresh token expired or revoked");
        }
    }
}

async fn load_homes(config: &Config, token: &str) -> anyhow::Result<Vec<i64>> {
    if let Some(id) = config.home_id {
        return Ok(vec![id]);
    }
    let (status, me) = api_call(Method::GET, "/api/v2/me", token, None).await?;
    if !status.is_success() {
        bail!("/api/v2/me: HTTP {}", status.as_u16());
    }
    let homes = me["homes"]
        .as_array()
        .context("no homes array in /api/v2/me")?
        .iter()
        .filter_map(|h| h["id"].as_i64())
        .collect::<Vec<_>>();
    if homes.is_empty() {
        bail!("no tado° home found on this account");
    }
    Ok(homes)
}

async fn sync_zones(
    config: &Config,
    ctx: &mut DriverCtx,
    token: &str,
    zones: &mut HashMap<DeviceId, ZoneInfo>,
) -> anyhow::Result<()> {
    let homes = load_homes(config, token).await?;
    let mut seen = std::collections::HashSet::new();
    for home_id in homes {
        let path = format!("/api/v2/homes/{home_id}/zones");
        let (status, body) = api_call(Method::GET, &path, token, None).await?;
        if !status.is_success() {
            bail!("{path}: HTTP {}", status.as_u16());
        }
        for mut z in parse_zones(home_id, &body) {
            let id = ctx.device_id(&format!("{}_{}", z.home_id, z.zone_id));
            if let Some(prev) = zones.get(&id) {
                z.last_target = prev.last_target;
            }
            ctx.upsert_device(build_device(ctx, &id, &z));
            ctx.set_state(&id, "battery_low", Value::Bool(z.battery_low));
            seen.insert(id.clone());
            zones.insert(id, z);
        }
    }
    zones.retain(|id, _| seen.contains(id));
    Ok(())
}

async fn poll_all_states(
    ctx: &mut DriverCtx,
    token: &str,
    zones: &mut HashMap<DeviceId, ZoneInfo>,
) -> anyhow::Result<()> {
    let mut outdoor_by_home: HashMap<i64, f64> = HashMap::new();
    for z in zones.values() {
        if outdoor_by_home.contains_key(&z.home_id) {
            continue;
        }
        let path = format!("/api/v2/homes/{}/weather", z.home_id);
        if let Ok((status, weather)) = api_call(Method::GET, &path, token, None).await
            && status.is_success()
            && let Some(temp) = weather["outsideTemperature"]["celsius"]
                .as_f64()
                .filter(|v| v.is_finite())
        {
            outdoor_by_home.insert(z.home_id, (temp * 10.0).round() / 10.0);
        }
    }

    for (id, zone) in zones.iter_mut() {
        refresh_zone_state(
            ctx,
            token,
            id,
            zone,
            outdoor_by_home.get(&zone.home_id).copied(),
        )
        .await?;
    }
    Ok(())
}

async fn refresh_zone_state(
    ctx: &mut DriverCtx,
    token: &str,
    id: &DeviceId,
    zone: &mut ZoneInfo,
    outdoor: Option<f64>,
) -> anyhow::Result<()> {
    let path = format!(
        "/api/v2/homes/{}/zones/{}/state",
        zone.home_id, zone.zone_id
    );
    let (status, body) = api_call(Method::GET, &path, token, None).await?;
    if !status.is_success() {
        bail!("{path}: HTTP {}", status.as_u16());
    }
    let decoded = decode_zone_state(&zone.kind, &body);
    ctx.set_availability(id, decoded.online);
    ctx.set_state(id, "on", Value::Bool(decoded.on));
    ctx.set_state(id, "mode", Value::Text(decoded.mode.into()));
    if let Some(target) = decoded.target_temperature {
        zone.last_target = target;
        ctx.set_state(id, "target_temperature", Value::Float(target));
    } else {
        ctx.set_state(id, "target_temperature", Value::Float(zone.last_target));
    }
    if let Some(t) = decoded.temperature {
        ctx.set_state(id, "temperature", Value::Float((t * 10.0).round() / 10.0));
    }
    if let Some(o) = outdoor {
        ctx.set_state(id, "outdoor_temperature", Value::Float(o));
    }
    if let Some(h) = decoded.humidity {
        ctx.set_state(id, "humidity", Value::Float(h.round()));
    }
    if let Some(p) = decoded.heating_power {
        ctx.set_state(id, "heating_power", Value::Float(p.round()));
    }
    ctx.set_state(id, "open_window", Value::Bool(decoded.open_window));
    Ok(())
}

async fn handle_command(
    ctx: &mut DriverCtx,
    token: &str,
    zones: &mut HashMap<DeviceId, ZoneInfo>,
    cmd: CommandRequest,
) {
    let id = cmd.device.id.clone();
    let Some(zone) = zones.get_mut(&id) else {
        cmd.reply(Err(moli_i18n::tr!("pilotes.tado.zone_inconnue")));
        return;
    };
    if !matches!(&*cmd.key, "on" | "mode" | "target_temperature") {
        let msg = moli_i18n::tr!("pilotes.tado.point_inconnu", point = &*cmd.key);
        cmd.reply(Err(msg));
        return;
    }
    let overlay_path = format!(
        "/api/v2/homes/{}/zones/{}/overlay",
        zone.home_id, zone.zone_id
    );
    let key = cmd.key.clone();
    let value = cmd.value.clone();
    let outcome: anyhow::Result<()> = async {
        match (&*key, &value) {
            ("on", Value::Bool(false)) => {
                let body = overlay_body(zone, false, zone.last_target, None);
                let (status, _) = api_call(Method::PUT, &overlay_path, token, Some(&body)).await?;
                if !status.is_success() {
                    bail!("HTTP {}", status.as_u16());
                }
            }
            ("on", Value::Bool(true)) => {
                // Resume Smart Schedule first; if the schedule itself has the
                // zone off, set a manual heating overlay at `last_target`.
                let _ = api_call(Method::DELETE, &overlay_path, token, None).await;
                let state_path = format!(
                    "/api/v2/homes/{}/zones/{}/state",
                    zone.home_id, zone.zone_id
                );
                let (status, state) = api_call(Method::GET, &state_path, token, None).await?;
                if status.is_success() && state["setting"]["power"].as_str() != Some("ON") {
                    let body = overlay_body(zone, true, zone.last_target, None);
                    let (put_status, _) =
                        api_call(Method::PUT, &overlay_path, token, Some(&body)).await?;
                    if !put_status.is_success() {
                        bail!("HTTP {}", put_status.as_u16());
                    }
                }
            }
            ("mode", Value::Text(mode)) if &**mode == "auto" => {
                let (status, _) = api_call(Method::DELETE, &overlay_path, token, None).await?;
                if !status.is_success() && status != StatusCode::NO_CONTENT {
                    bail!("HTTP {}", status.as_u16());
                }
            }
            ("mode", Value::Text(mode)) => {
                let body = overlay_body(zone, true, zone.last_target, Some(mode));
                let (status, _) = api_call(Method::PUT, &overlay_path, token, Some(&body)).await?;
                if !status.is_success() {
                    bail!("HTTP {}", status.as_u16());
                }
            }
            ("target_temperature", v) => {
                let celsius = v.as_f64().context("invalid temperature")?;
                zone.last_target = celsius;
                let body = overlay_body(zone, true, celsius, None);
                let (status, _) = api_call(Method::PUT, &overlay_path, token, Some(&body)).await?;
                if !status.is_success() {
                    bail!("HTTP {}", status.as_u16());
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    .await;

    match outcome {
        Ok(()) => {
            cmd.reply(Ok(()));
            if let Some(zone) = zones.get_mut(&id) {
                let _ = refresh_zone_state(ctx, token, &id, zone, None).await;
            }
        }
        Err(e) => {
            cmd.reply(Err(moli_i18n::tr!(
                "pilotes.tado.echec",
                why = format!("{e:#}")
            )));
        }
    }
}

async fn connected(
    config: &Config,
    ctx: &mut DriverCtx,
    cached_token: &mut Option<(String, Instant)>,
    initial_token: String,
) -> anyhow::Result<()> {
    let mut zones: HashMap<DeviceId, ZoneInfo> = HashMap::new();
    sync_zones(config, ctx, &initial_token, &mut zones).await?;
    poll_all_states(ctx, &initial_token, &mut zones).await?;
    ctx.ready();

    let mut state_tick = tokio::time::interval(POLL_STATE);
    state_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    state_tick.tick().await;

    let mut zones_tick = tokio::time::interval(POLL_ZONES);
    zones_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    zones_tick.tick().await;

    loop {
        tokio::select! {
            _ = state_tick.tick() => {
                let token = ensure_token(ctx, cached_token).await?;
                poll_all_states(ctx, &token, &mut zones).await?;
            }
            _ = zones_tick.tick() => {
                let token = ensure_token(ctx, cached_token).await?;
                sync_zones(config, ctx, &token, &mut zones).await?;
            }
            cmd = ctx.next_command() => {
                let Some(cmd) = cmd else { return Ok(()) };
                let token = ensure_token(ctx, cached_token).await?;
                handle_command(ctx, &token, &mut zones, cmd).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_zones_and_state_fixture() {
        let zones_json: Json = serde_json::from_str(
            r#"[
                {
                    "id": 1,
                    "name": "Salon",
                    "type": "HEATING",
                    "devices": [
                        {
                            "deviceType": "RU02",
                            "serialNo": "RU0000000001",
                            "batteryState": "NORMAL"
                        }
                    ]
                }
            ]"#,
        )
        .unwrap();
        let zones = parse_zones(100, &zones_json);
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].name, "Salon");
        assert_eq!(zones[0].kind, ZoneKind::Heating);
        assert_eq!(zones[0].model, "RU02");
        assert!(!zones[0].battery_low);

        let state_json: Json = serde_json::from_slice(include_bytes!(
            "../../../integrations/tado/fixtures/zone_state.json"
        ))
        .unwrap();
        let decoded = decode_zone_state(&ZoneKind::Heating, &state_json);
        assert!(decoded.online);
        assert!(decoded.on);
        assert_eq!(decoded.mode, "auto");
        assert_eq!(decoded.target_temperature, Some(20.5));
        assert_eq!(decoded.temperature, Some(19.8));
        assert_eq!(decoded.humidity, Some(54.2));
        assert_eq!(decoded.heating_power, Some(35.0));
        assert!(!decoded.open_window);

        let body = overlay_body(&zones[0], true, 21.0, Some("chaud"));
        assert_eq!(body["setting"]["power"], "ON");
        assert_eq!(body["setting"]["temperature"]["celsius"], 21.0);
    }
}
