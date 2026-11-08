//! Ring doorbells and cameras, through the Amazon Vision Partner API:
//! events (ding, motion) and on-demand snapshots.
//!
//! The account is linked one-way: from the Ring app, the person shares the
//! device with the partner app, Ring hands Moli's API a one-time
//! authorization code (`POST /api/ring/link`), and the matching browser
//! redirect claims it after a sign-in. What reaches this driver, in the
//! encrypted secret store:
//!
//! - `client_id`, `client_secret`, `hmac_key` — the partner account's keys,
//!   filed by the person (`moli-os secrets set ring …`);
//! - `refresh_token` — filed by Moli's API when the account is linked;
//! - `hardware_id` — a UUID Moli generates once and files itself.
//!
//! The access token is short-lived (~4 h) and the refresh token rotates with
//! every refresh: the driver's tick is the single refresher, and files the
//! rotated token before using it. Snapshots only read the in-memory token
//! (a 303 hands a pre-signed URL, then a plain GET of the JPEG).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use http::{Method, Request, header};
use http_body_util::Full;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_net::Body as Bytes;
use moli_runtime::media::{Image, SnapshotSource};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::{Value as Json, json};

const CLIENT_ID: &str = "client_id";
const CLIENT_SECRET: &str = "client_secret";
pub const HMAC_KEY: &str = "hmac_key";
pub const REFRESH: &str = "refresh_token";
pub const HARDWARE_ID: &str = "hardware_id";

const API_HOST: &str = "api.amazonvision.com";
const OAUTH_HOST: &str = "oauth.ring.com";
const USER_AGENT: &str = "moli-os/0.1";
const HARDWARE_ID_HEADER: &str = "Hardware_Id";

/// Events poll; the account's device list rescans every RESCAN.
const POLL: Duration = Duration::from_secs(15);
const RESCAN: Duration = Duration::from_secs(300);
/// A detection is a level: the point goes back to false after DECAY.
const DECAY: Duration = Duration::from_secs(90);
/// Renew the access token this long before its lease ends.
const LEASE_MARGIN: Duration = Duration::from_secs(60);
/// The snapshot's search window (the API allows up to 24 h).
const SNAPSHOT_WINDOW_MS: u64 = 3_600_000;
const LIMIT: Duration = Duration::from_secs(20);
const MAX_BODY: usize = 512 * 1024;
const MAX_IMAGE: usize = 5 * 1024 * 1024;
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// The error's marker when the account must be (re)linked: the driver stops,
/// the person re-shares the device from the Ring app.
const NEEDS_LINK: &str = "ring account must be (re)linked";
/// The error's marker when Ring refuses the partner keys themselves.
const CREDENTIALS_REJECTED: &str = "ring refused the partner keys";

/// Point key, the event type as Ring names it, the label's key.
const POINTS: [(&str, &str, &str); 4] = [
    ("doorbell", "ding", "pilotes.ring.sonnette"),
    ("person", "motion.human", "pilotes.ring.personne"),
    ("vehicle", "motion.vehicle", "pilotes.ring.vehicule"),
    ("animal", "motion.animal", "pilotes.ring.animal"),
];

/// What a cycle needs to talk to Ring: the shared session, the partner's
/// keys and the stable hardware id.
struct Api {
    session: Arc<Session>,
    client_id: String,
    client_secret: String,
    hardware_id: String,
}

/// No driver options: everything is in the secret store.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {}

#[derive(Debug)]
pub struct Ring;

impl Ring {
    #[must_use]
    pub fn new(_config: Config) -> Self {
        Self
    }
}

/// The access token and its clock, shared with the snapshot sources.
/// Never printed.
#[derive(Clone)]
struct Token {
    access: String,
    expires_at: Instant,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[derive(Debug)]
struct Session {
    token: Mutex<Option<Token>>,
    /// The refresh token last used in an exchange (to tell a dead token from
    /// an account re-linked in the meantime).
    last_refresh: Mutex<String>,
}

impl Session {
    fn new() -> Self {
        Self {
            token: Mutex::new(None),
            last_refresh: Mutex::new(String::new()),
        }
    }

    /// The live access token, without the network (the snapshot path reads
    /// it; the driver's tick keeps it fresh).
    fn access(&self) -> Option<String> {
        self.token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|t| Instant::now() < t.expires_at)
            .map(|t| t.access.clone())
    }

    fn set(&self, access: String, expires_in: u64) {
        *self.token.lock().unwrap_or_else(PoisonError::into_inner) = Some(Token {
            access,
            expires_at: Instant::now()
                + Duration::from_secs(expires_in).saturating_sub(LEASE_MARGIN),
        });
    }

    /// A 401 mid-window (a revoked token): the next tick re-exchanges from
    /// the vault instead of reusing the dead token.
    fn invalidate(&self) {
        *self.token.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn last_refresh(&self) -> String {
        self.last_refresh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn remember_refresh(&self, refresh: &str) {
        refresh.clone_into(
            &mut self
                .last_refresh
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

impl Driver for Ring {
    fn kind(&self) -> &'static str {
        "ring"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(ctx))
    }
}

/// One camera (or doorbell) of the account, as the discovery says it.
/// The API names no device type — Moli does not infer one either: every
/// device holding a `video` capability gets the same four points.
#[derive(Clone, Debug, PartialEq)]
struct Camera {
    /// The directed device id: stable, opaque.
    native: String,
    name: String,
    model: Option<String>,
    online: bool,
    /// A multi-camera device's module, named in image requests.
    component: Option<String>,
}

/// What a `latest_in_range` picture request needs of the device.
#[derive(Debug)]
struct RingImages {
    session: Arc<Session>,
    hardware_id: String,
    device: String,
    component: Option<String>,
}

impl SnapshotSource for RingImages {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<Image>> {
        Box::pin(self.snap())
    }
}

impl RingImages {
    /// A JPEG from the last hour. The pre-signed URL holds its token in the
    /// query string: no error ever quotes it.
    async fn snap(&self) -> anyhow::Result<Image> {
        let access = self
            .session
            .access()
            .ok_or_else(|| anyhow::anyhow!(moli_i18n::tr!("pilotes.ring.pas_de_jeton")))?;
        let now = moli_core::now_ms();
        let mut body = json!({
            "type": "latest_in_range",
            "start_timestamp": now.saturating_sub(SNAPSHOT_WINDOW_MS),
            "end_timestamp": now,
            "image_options": { "format": "jpeg" }
        });
        if let Some(component) = &self.component {
            // A multi-camera device answers non-deterministically without it.
            body["components"] = json!([{ "component_id": component }]);
        }
        let path = format!("/v1/devices/{}/media/image/download", self.device);
        let (status, headers, _) = moli_net::web_with_headers(
            API_HOST,
            443,
            true,
            api_post(&access, &self.hardware_id, &path, &body)?,
            LIMIT,
            MAX_BODY,
        )
        .await?;
        let location = headers
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .filter(|l| l.starts_with("https://"))
            .filter(|_| status.is_redirection())
            .ok_or_else(|| anyhow::anyhow!(moli_i18n::tr!("pilotes.ring.aucune_image")))?;
        let (host, port, uri) = split_location(location)
            .ok_or_else(|| anyhow::anyhow!(moli_i18n::tr!("pilotes.ring.aucune_image")))?;
        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Full::new(Bytes::new()))?;
        let (status, bytes) = moli_net::web(host, port, true, request, LIMIT, MAX_IMAGE).await?;
        if status.is_success() && !bytes.is_empty() {
            Ok(Image {
                content_type: "image/jpeg".into(),
                bytes: bytes.to_vec(),
            })
        } else {
            bail!(moli_i18n::tr!("pilotes.ring.aucune_image"));
        }
    }
}

fn point_spec(key: &str, label: &str) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Binary,
        access: Access {
            read: true,
            write: false,
        },
        unit: None,
        semantic: Semantic::infer(key, None),
    }
}

fn camera_device(ctx: &DriverCtx, cam: &Camera) -> Device {
    Device {
        id: ctx.device_id(&cam.native),
        instance: ctx.instance().clone(),
        native_name: cam.name.as_str().into(),
        manufacturer: Some("Ring".into()),
        model: cam.model.as_deref().map(Arc::from),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: POINTS
            .iter()
            .map(|(point, _, label)| point_spec(point, &moli_i18n::tr(label)))
            .collect(),
    }
}

async fn run(ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let (client_id, client_secret) = wait_for_secrets(ctx).await?;
    // One stable id per Moli instance, Ring tracks it in its headers.
    let hardware_id = if let Some(id) = ctx.secret(HARDWARE_ID) {
        id
    } else {
        if !ctx.can_store_secrets() {
            bail!("cannot keep the ring hardware id: the secret store is unavailable");
        }
        let id = generate_hardware_id();
        ctx.store_secret(HARDWARE_ID, &id)
            .await
            .with_context(|| "cannot keep the hardware id in the secret store")?;
        id
    };
    let mut api = Api {
        session: Arc::new(Session::new()),
        client_id,
        client_secret,
        hardware_id,
    };
    serve(ctx, &mut api).await
}

/// The partner's keys (filed by the person) and the refresh token (filed by
/// Moli's API when the account is linked). Re-checked while waiting: the
/// link can arrive while the house is running.
async fn wait_for_secrets(ctx: &DriverCtx) -> anyhow::Result<(String, String)> {
    loop {
        let client_id = ctx.secret(CLIENT_ID);
        let client_secret = ctx.secret(CLIENT_SECRET);
        let refresh = ctx.secret(REFRESH);
        if client_id.is_none() || client_secret.is_none() {
            ctx.wait_for(moli_i18n::tr!(
                "pilotes.ring.cles_manquantes",
                instance = ctx.instance()
            ));
        } else if refresh.is_none() {
            ctx.wait_for(moli_i18n::tr!(
                "pilotes.ring.non_lie",
                instance = ctx.instance()
            ));
        }
        let (Some(client_id), Some(client_secret), Some(_)) = (client_id, client_secret, refresh)
        else {
            tokio::select! {
                () = ctx.cancelled() => return Ok((String::new(), String::new())),
                () = tokio::time::sleep(POLL) => {}
            };
            continue;
        };
        return Ok((client_id, client_secret));
    }
}

fn generate_hardware_id() -> String {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("the system RNG failed");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = |bytes: &[u8]| {
        bytes.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    };
    format!(
        "{}-{}-{}-{}-{}",
        hex(&bytes[0..4]),
        hex(&bytes[4..6]),
        hex(&bytes[6..8]),
        hex(&bytes[8..10]),
        hex(&bytes[10..16])
    )
}

async fn serve(ctx: &mut DriverCtx, api: &mut Api) -> anyhow::Result<()> {
    let mut cameras: BTreeMap<String, Camera> = BTreeMap::new();
    let mut follow: Follow = BTreeMap::new();
    let mut backoff = Duration::from_secs(2);
    let mut retry_at: Option<Instant> = None;
    let mut rescan_at = Instant::now();
    let mut ready = false;
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => {
                    command.reply(Err(moli_i18n::tr!("pilotes.ring.lecture_seule")));
                }
                None => return Ok(()),
            },
            _ = poll.tick() => {
                if retry_at.is_some_and(|t| Instant::now() < t) {
                    continue;
                }
                let now = Instant::now();
                let result = tick(ctx, api, &mut cameras, &mut follow, &mut rescan_at, now).await;
                match result {
                    Ok(()) => {
                        retry_at = None;
                        backoff = Duration::from_secs(2);
                        if !ready {
                            ready = true;
                            ctx.ready();
                            tracing::info!(
                                instance = %ctx.instance(),
                                cameras = cameras.len(),
                                "ring account loaded"
                            );
                        }
                    }
                    Err(e) if e.to_string().contains(NEEDS_LINK) => {
                        // The same refresh token the vault still holds is dead:
                        // forget it. A different one (the account re-linked in
                        // the meantime) stays: the next start will use it.
                        if ctx.secret(REFRESH).as_deref() == Some(api.session.last_refresh().as_str()) {
                            let _ = ctx.forget_secret(REFRESH).await;
                        }
                        ctx.wait_for(moli_i18n::tr!(
                            "pilotes.ring.non_lie",
                            instance = ctx.instance()
                        ));
                        return Ok(());
                    }
                    Err(e) if e.to_string().contains(CREDENTIALS_REJECTED) => {
                        // The person can fix the keys without restarting:
                        // re-read them from the vault, then retry.
                        tracing::warn!(instance = %ctx.instance(), "ring partner keys refused");
                        if let Some(id) = ctx.secret(CLIENT_ID) {
                            api.client_id = id;
                        }
                        if let Some(sec) = ctx.secret(CLIENT_SECRET) {
                            api.client_secret = sec;
                        }
                        ctx.wait_for(moli_i18n::tr!(
                            "pilotes.ring.cles_refusees",
                            instance = ctx.instance()
                        ));
                        unavailable(ctx, &cameras);
                        retry_at = Some(Instant::now() + backoff);
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                    }
                    Err(e) => {
                        tracing::warn!(
                            instance = %ctx.instance(),
                            error = %e,
                            retry_in = ?backoff,
                            "ring cloud unreachable"
                        );
                        unavailable(ctx, &cameras);
                        retry_at = Some(Instant::now() + backoff);
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                    }
                }
            }
        }
    }
}

/// One cycle: the token, the device list when due, the events, the decay.
async fn tick(
    ctx: &DriverCtx,
    api: &Api,
    cameras: &mut BTreeMap<String, Camera>,
    follow: &mut Follow,
    rescan_at: &mut Instant,
    now: Instant,
) -> anyhow::Result<()> {
    ensure(ctx, api).await?;
    let Some(access) = api.session.access() else {
        bail!("ring: no access token after the refresh");
    };
    if now >= *rescan_at {
        match scan_devices(ctx, api, &access, cameras, follow).await {
            Ok(()) => *rescan_at = now + RESCAN,
            Err(e) if e.to_string().contains(NEEDS_LINK) => return Err(e),
            Err(e) => {
                // The known list stays; the events still say.
                tracing::warn!(
                    instance = %ctx.instance(),
                    error = %e,
                    "ring device list not read"
                );
            }
        }
    }
    for cam in cameras.values().filter(|c| c.online) {
        poll_camera(ctx, api, cam, follow, now).await;
    }
    decay(ctx, cameras, follow, now);
    Ok(())
}

/// A live access token: the in-memory one while it lives, else a refresh
/// from the vault. The refresh token rotates with each refresh — the new
/// one is filed before the old one dies.
async fn ensure(ctx: &DriverCtx, api: &Api) -> anyhow::Result<()> {
    if api.session.access().is_some() {
        return Ok(());
    }
    let Some(refresh) = ctx.secret(REFRESH) else {
        bail!("{NEEDS_LINK}");
    };
    api.session.remember_refresh(&refresh);
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}",
        form_escape(&refresh),
        form_escape(&api.client_id),
        form_escape(&api.client_secret)
    );
    let request = Request::builder()
        .method(Method::POST)
        .uri("/oauth/token")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("User-Agent", USER_AGENT)
        .header(HARDWARE_ID_HEADER, &api.hardware_id)
        .body(Full::new(Bytes::from(body)))?;
    let (status, bytes) = moli_net::web(OAUTH_HOST, 443, true, request, LIMIT, MAX_BODY).await?;
    match status.as_u16() {
        200 => {
            let v: Json = serde_json::from_slice(&bytes).context("ring token answer")?;
            let access = v["access_token"]
                .as_str()
                .context("ring token answer has no access token")?
                .to_owned();
            let new_refresh = v["refresh_token"]
                .as_str()
                .context("ring token answer has no refresh token")?
                .to_owned();
            let expires_in = v["expires_in"].as_u64().unwrap_or(14400);
            if let Err(e) = ctx.store_secret(REFRESH, &new_refresh).await {
                tracing::warn!(
                    instance = %ctx.instance(),
                    error = %e,
                    "ring refresh token not filed"
                );
            }
            api.session.set(access, expires_in);
            Ok(())
        }
        401 => bail!("{CREDENTIALS_REJECTED}"),
        s if (400..500).contains(&s) => bail!("{NEEDS_LINK}"),
        s => bail!("ring token endpoint answered {s}"),
    }
}

/// The account's cameras and doorbells: the list, their status, their
/// capabilities, all in one sideloaded call.
async fn scan_devices(
    ctx: &DriverCtx,
    api: &Api,
    access: &str,
    cameras: &mut BTreeMap<String, Camera>,
    follow: &mut Follow,
) -> anyhow::Result<()> {
    let request = api_get(
        access,
        &api.hardware_id,
        "/v1/devices?include=status,capabilities",
    )?;
    let (status, bytes) = moli_net::web(API_HOST, 443, true, request, LIMIT, MAX_BODY).await?;
    match status.as_u16() {
        200 => {}
        401 | 403 => bail!("{NEEDS_LINK}"),
        other => bail!("ring answered {other} on the device list"),
    }
    let v: Json = serde_json::from_slice(&bytes).context("ring device list")?;
    let included = v["included"].as_array().cloned().unwrap_or_default();
    let mut found: BTreeMap<String, Camera> = BTreeMap::new();
    for dev in v["data"].as_array().into_iter().flatten() {
        let Some(native) = dev["id"].as_str() else {
            continue;
        };
        let name = dev["attributes"]["name"].as_str().unwrap_or(native);
        let model = dev["attributes"]["model"].as_str().map(String::from);
        let status_id = dev["relationships"]["status"]["data"]["id"].as_str();
        let online = included
            .iter()
            .find(|inc| {
                inc["type"].as_str() == Some("device-status") && inc["id"].as_str() == status_id
            })
            .and_then(|s| s["attributes"]["online"].as_bool())
            .unwrap_or(false);
        let caps_id = dev["relationships"]["capabilities"]["data"]["id"].as_str();
        let caps = included.iter().find(|inc| {
            inc["type"].as_str() == Some("device-capabilities") && inc["id"].as_str() == caps_id
        });
        // Only devices holding a video module: Moli's events and images
        // concern cameras and doorbells (chimes and sensors stay out).
        if !caps.is_some_and(|c| c["attributes"]["video"].is_object()) {
            continue;
        }
        // A multi-camera device names its module in image requests.
        let component = caps
            .and_then(|c| c["attributes"]["components"]["items"].as_array())
            .and_then(|items| items.first())
            .and_then(|item| item["component_id"].as_str())
            .map(String::from);
        found.insert(
            native.to_owned(),
            Camera {
                native: native.to_owned(),
                name: name.to_owned(),
                model,
                online,
                component,
            },
        );
    }
    // A device no longer shared with the account leaves the house.
    for native in cameras.keys() {
        if !found.contains_key(native) {
            ctx.remove_device(&ctx.device_id(native));
            follow.retain(|(n, _), _| n != native);
        }
    }
    for (native, cam) in &found {
        if !cameras.contains_key(native) || cameras[native] != *cam {
            let device = ctx.device_id(native);
            ctx.upsert_device(camera_device(ctx, cam));
            ctx.provide_snapshots(
                &device,
                Arc::new(RingImages {
                    session: Arc::clone(&api.session),
                    hardware_id: api.hardware_id.clone(),
                    device: native.clone(),
                    component: cam.component.clone(),
                }),
            );
        }
        ctx.set_availability(&ctx.device_id(native), cam.online);
    }
    *cameras = found;
    Ok(())
}

/// The events of one camera, one kind at a time: the history does not say
/// which subtype a motion belongs to, only the query does.
async fn poll_camera(ctx: &DriverCtx, api: &Api, cam: &Camera, follow: &mut Follow, now: Instant) {
    let Some(access) = api.session.access() else {
        return;
    };
    let device = ctx.device_id(&cam.native);
    let mut ok = true;
    for (point, api_type, _) in POINTS {
        match fetch_events(&access, &api.hardware_id, &cam.native, api_type).await {
            Ok(events) => apply_events(ctx, &device, &cam.native, point, &events, follow, now),
            Err(e) if e.to_string().contains(NEEDS_LINK) => {
                // A dead token: the next tick re-exchanges from the vault.
                api.session.invalidate();
                return;
            }
            Err(e) => {
                tracing::debug!(
                    instance = %ctx.instance(),
                    device = %cam.native,
                    point,
                    error = %e,
                    "ring events not read"
                );
                ok = false;
            }
        }
    }
    ctx.set_availability(&device, ok);
}

#[derive(Debug, Clone)]
struct Event {
    id: String,
    start: u64,
}

/// The newest events of one kind of one camera, newest first.
async fn fetch_events(
    access: &str,
    hardware_id: &str,
    device: &str,
    api: &str,
) -> anyhow::Result<Vec<Event>> {
    let path = format!("/v1/history/devices/{device}/events?event_types={api}");
    let (status, bytes) = moli_net::web(
        API_HOST,
        443,
        true,
        api_get(access, hardware_id, &path)?,
        LIMIT,
        MAX_BODY,
    )
    .await?;
    match status.as_u16() {
        200 => {
            let v: Json = serde_json::from_slice(&bytes).context("ring event list")?;
            Ok(v["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| {
                    Some(Event {
                        id: e["id"].as_str()?.to_owned(),
                        start: e["attributes"]["start"].as_u64().unwrap_or(0),
                    })
                })
                .collect())
        }
        401 | 403 => bail!("{NEEDS_LINK}"),
        404 | 416 => Ok(Vec::new()),
        429 => bail!("ring rate limited"),
        other => bail!("ring answered {other} on the event history"),
    }
}

/// The events are newest-first: nothing already seen is said twice, the
/// first read only records the inventory (no burst of history).
fn apply_events(
    ctx: &DriverCtx,
    device: &DeviceId,
    native: &str,
    point: &'static str,
    events: &[Event],
    follow: &mut Follow,
    now: Instant,
) {
    let Some(newest) = events.first() else {
        return;
    };
    let entry = follow.entry((native.to_owned(), point)).or_default();
    let seen = entry.last.as_ref().map(|(start, _)| *start);
    for _ in fresh_events(seen, events) {
        if point == "doorbell" {
            // A button press: each one is a new event, broadcast as-is.
            ctx.pulse_state(device, point, Value::Bool(true));
        } else {
            ctx.set_state(device, point, Value::Bool(true));
            entry.active_until = Some(now + DECAY);
        }
    }
    entry.last = Some((newest.start, newest.id.clone()));
}

/// The events newer than the last read, oldest first. `None` (a first read)
/// yields nothing: it only records the inventory.
fn fresh_events(seen_start: Option<u64>, events: &[Event]) -> Vec<&Event> {
    let Some(start) = seen_start else {
        return Vec::new();
    };
    events.iter().rev().filter(|e| e.start > start).collect()
}

/// The (native id, point) a driver follows: its newest seen event, and until
/// when the point stays true.
#[derive(Default)]
struct Following {
    last: Option<(u64, String)>,
    active_until: Option<Instant>,
}

type Follow = BTreeMap<(String, &'static str), Following>;

/// A detection is a level, not a state: after DECAY without a new event,
/// the point goes back to false (the journal keeps the occurrences).
fn decay(ctx: &DriverCtx, cameras: &BTreeMap<String, Camera>, follow: &mut Follow, now: Instant) {
    for ((native, point), f) in follow.iter_mut() {
        if f.active_until.is_some_and(|until| now >= until) {
            f.active_until = None;
            if cameras.get(native).is_some_and(|cam| cam.online) {
                ctx.set_state(&ctx.device_id(native), point, Value::Bool(false));
            }
        }
    }
}

/// Cloud down: the devices say so, the levels decay on their own.
fn unavailable(ctx: &DriverCtx, cameras: &BTreeMap<String, Camera>) {
    for cam in cameras.values() {
        ctx.set_availability(&ctx.device_id(&cam.native), false);
    }
}

fn api_get(access: &str, hardware_id: &str, path: &str) -> anyhow::Result<Request<Full<Bytes>>> {
    Ok(Request::builder()
        .method(Method::GET)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {access}"))
        .header("User-Agent", USER_AGENT)
        .header(HARDWARE_ID_HEADER, hardware_id)
        .body(Full::new(Bytes::new()))?)
}

fn api_post(
    access: &str,
    hardware_id: &str,
    path: &str,
    body: &Json,
) -> anyhow::Result<Request<Full<Bytes>>> {
    Ok(Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {access}"))
        .header("User-Agent", USER_AGENT)
        .header(HARDWARE_ID_HEADER, hardware_id)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(serde_json::to_vec(body)?)))?)
}

/// `application/x-www-form-urlencoded` value: safe chars kept, the rest in
/// `%XX` (uppercase).
fn form_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// `https://host[:port]/path…` → (host, port, path): the pre-signed
/// download URL, kept away from every error message.
fn split_location(url: &str) -> Option<(&str, u16, String)> {
    let rest = url.strip_prefix("https://")?;
    let (authority, path) = rest.split_once('/')?;
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (authority, 443),
    };
    Some((host, port, format!("/{path}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_location_is_split_without_leaking_the_query() {
        assert_eq!(
            split_location("https://download-eu.example.com/v1/download?security_token=s3cr3t"),
            Some((
                "download-eu.example.com",
                443,
                "/v1/download?security_token=s3cr3t".to_owned()
            ))
        );
        assert_eq!(
            split_location("https://d.example.com:8443/x"),
            Some(("d.example.com", 8443, "/x".to_owned()))
        );
        assert_eq!(split_location("http://d.example.com/x"), None);
        assert_eq!(split_location("https://"), None);
    }

    #[test]
    fn the_form_escape_round_trips_the_safe_chars() {
        assert_eq!(form_escape("abc-DEF._~09"), "abc-DEF._~09");
        assert_eq!(form_escape("a b+c/d"), "a%20b%2Bc%2Fd");
    }

    #[test]
    fn the_first_reading_records_without_emitting() {
        // Newest first, as the history answers.
        let events = [
            Event {
                id: "e2".into(),
                start: 200,
            },
            Event {
                id: "e1".into(),
                start: 100,
            },
        ];
        // First read: nothing is said, only the inventory is recorded.
        assert!(fresh_events(None, &events).is_empty());
        // The inventory read: an older batch stays silent, a newer one
        // speaks once, oldest first.
        assert!(fresh_events(Some(200), &events).is_empty());
        let newer = [
            Event {
                id: "e3".into(),
                start: 300,
            },
            Event {
                id: "e2b".into(),
                start: 250,
            },
        ];
        let fresh = fresh_events(Some(200), &newer);
        assert_eq!(
            fresh.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["e2b", "e3"]
        );
    }

    #[test]
    fn the_hardware_id_keeps_its_shape() {
        let id = generate_hardware_id();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert_eq!(id.chars().nth(14), Some('4'), "version 4");
    }
}
