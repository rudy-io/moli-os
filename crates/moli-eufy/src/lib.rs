//! Eufy Security: HomeBases, cameras and doorbells, through Eufy's cloud
//! (read-only for now). Ported from eufy-security-client (MIT, see NOTICE).
//! Detections (motion, person, doorbell…) and mode changes arrive as push
//! notifications through Google, as they do to the app (`push.rs`).
//!
//! The account (`email`, `password`) comes from the encrypted secret store:
//! a guest account of its own, since Eufy keeps one session per account. A
//! first login asks for a code e-mailed to it (sometimes a captcha first):
//! the person files it in the store and restarts Moli; Moli is then a
//! trusted phone and logs in alone for months. The session (Moli's key, its
//! phone identity, the token) is kept in the store as well.

mod cloud;
mod crypto;
mod events;
mod image;
mod p2p;
mod push;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use moli_core::{Access, Device, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::media::Image;
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::cloud::{Answer, Cloud, Login, Refusal, Session};

const EMAIL: &str = "email";
const PASSWORD: &str = "password";
/// Written by the driver: Moli's identity with Eufy and its token.
const SESSION: &str = "session";
/// Written by a person: the e-mailed code, the captcha's characters.
const CODE: &str = "verify_code";
const CAPTCHA: &str = "captcha";
/// Written by the driver: which captcha the answer is for.
const CAPTCHA_ID: &str = "captcha_id";

/// Written by the driver: Moli's identity with Google's push service.
const PUSH: &str = "push";

/// A detection stays « on » this long after its push.
const HOLD: Duration = Duration::from_secs(10);

/// The list is read again this often (names, battery, guard mode).
const RESCAN: Duration = Duration::from_secs(10 * 60);

/// Parameters (`param_type`) read from the lists.
const BATTERY: i64 = 1101;
const GUARD_MODE: i64 = 1224;

/// Guard modes as Eufy numbers them, and their Moli value.
const MODES: [(i64, &str); 8] = [
    (0, "away"),
    (1, "home"),
    (2, "schedule"),
    (3, "custom1"),
    (4, "custom2"),
    (5, "custom3"),
    (47, "geofencing"),
    (63, "disarmed"),
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The account's country (ISO 3166, `FR`): it chooses Eufy's servers.
    pub country: String,
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_language() -> String {
    "fr".into()
}

#[derive(Debug)]
pub struct Eufy {
    config: Config,
}

impl Eufy {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        anyhow::ensure!(
            config.country.len() == 2 && config.country.bytes().all(|b| b.is_ascii_alphabetic()),
            "country: two letters (FR, DE, US…)"
        );
        anyhow::ensure!(
            config.language.len() == 2 && config.language.bytes().all(|b| b.is_ascii_alphabetic()),
            "language: two letters (fr, en…)"
        );
        Ok(Self { config })
    }
}

impl Driver for Eufy {
    fn kind(&self) -> &'static str {
        "eufy"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn now_s() -> i64 {
    i64::try_from(moli_core::now_ms() / 1000).unwrap_or(i64::MAX)
}

/// Waits for a person (shown everywhere), until Moli stops.
async fn wait(ctx: &DriverCtx, reason: String) -> anyhow::Result<()> {
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

async fn save(ctx: &DriverCtx, session: &Session) -> anyhow::Result<()> {
    ctx.store_secret(SESSION, &serde_json::to_string(session)?)
        .await
        .context("saving the Eufy session")
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let instance = ctx.instance().to_string();
    let (Some(email), Some(password)) = (ctx.secret(EMAIL), ctx.secret(PASSWORD)) else {
        let reason = moli_i18n::tr!(
            "pilotes.eufy.identifiants_manquants",
            instance = instance,
            email = EMAIL,
            password = PASSWORD
        );
        return wait(ctx, reason).await;
    };
    if !ctx.can_store_secrets() {
        anyhow::bail!("the Eufy session cannot be kept: the secret store is unavailable");
    }
    let session = if let Some(text) = ctx.secret(SESSION) {
        serde_json::from_str(&text).context("stored Eufy session")?
    } else {
        let session = Session::new()?;
        save(ctx, &session).await?;
        session
    };
    let mut cloud = Cloud::new(session, &config.country, &config.language)?;
    if !cloud.session.logged_in(now_s()) && !log_in(ctx, &mut cloud, &email, &password).await? {
        return Ok(());
    }
    serve(ctx, &mut cloud).await
}

/// `false`: a person must act first (the reason is shown, Moli waits).
async fn log_in(
    ctx: &DriverCtx,
    cloud: &mut Cloud,
    email: &str,
    password: &str,
) -> anyhow::Result<bool> {
    let instance = ctx.instance().to_string();
    let code = ctx.secret(CODE);
    let captcha = ctx.secret(CAPTCHA).zip(ctx.secret(CAPTCHA_ID));
    let answer = match (&code, &captcha) {
        (Some(code), _) => Some(Answer::Code(code.trim())),
        (None, Some((text, id))) => Some(Answer::Captcha {
            id,
            text: text.trim(),
        }),
        (None, None) => None,
    };
    let result = cloud.login(email, password, answer).await;
    // An answer serves once, right or wrong.
    if code.is_some() {
        ctx.forget_secret(CODE).await?;
    }
    if captcha.is_some() {
        ctx.forget_secret(CAPTCHA).await?;
        ctx.forget_secret(CAPTCHA_ID).await?;
    }
    match result {
        Ok(Login::Done) => {
            if let Some(code) = &code {
                cloud.trust(code.trim()).await?;
            }
            save(ctx, &cloud.session).await?;
            tracing::info!(instance, "eufy: logged in");
            Ok(true)
        }
        Ok(Login::NeedCode) => {
            save(ctx, &cloud.session).await?;
            let reason =
                moli_i18n::tr!("pilotes.eufy.code_envoye", instance = instance, code = CODE);
            wait(ctx, reason).await?;
            Ok(false)
        }
        Ok(Login::NeedCaptcha { id, image }) => {
            ctx.store_secret(CAPTCHA_ID, &id).await?;
            save(ctx, &cloud.session).await?;
            let link = captcha_image(&image)
                .and_then(|image| ctx.media_publisher().publish("png", image))
                .map_or_else(String::new, |name| format!("/api/media/{name}"));
            let reason = moli_i18n::tr!(
                "pilotes.eufy.captcha",
                instance = instance,
                captcha = CAPTCHA,
                link = link
            );
            wait(ctx, reason).await?;
            Ok(false)
        }
        Err(e) => match e.downcast_ref::<Refusal>() {
            Some(Refusal::Password) => {
                let reason = moli_i18n::tr!(
                    "pilotes.eufy.mot_de_passe_refuse",
                    instance = instance,
                    password = PASSWORD
                );
                wait(ctx, reason).await?;
                Ok(false)
            }
            // A wrong code: start again, a new one is e-mailed.
            Some(Refusal::Code) => Box::pin(log_in(ctx, cloud, email, password)).await,
            _ => Err(e),
        },
    }
}

/// The captcha's picture, from its `data:image/…;base64,` URL.
fn captcha_image(url: &str) -> Option<Image> {
    let (head, data) = url.split_once(',')?;
    let content_type = head.strip_prefix("data:")?.strip_suffix(";base64")?;
    if !content_type.starts_with("image/") {
        return None;
    }
    Some(Image {
        content_type: content_type.into(),
        bytes: crypto::unbase64(data)?,
    })
}

async fn serve(ctx: &mut DriverCtx, cloud: &mut Cloud) -> anyhow::Result<()> {
    let mut known: BTreeMap<String, Device> = BTreeMap::new();
    let mut cameras: BTreeMap<String, Camera> = BTreeMap::new();
    let mut stations: BTreeMap<String, Arming> = BTreeMap::new();
    let (done_tx, mut done) = mpsc::channel::<(String, i64)>(8);
    let mut rescan = tokio::time::interval(RESCAN);
    rescan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut expire = tokio::time::interval(Duration::from_secs(1));
    expire.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Detections held until then.
    let mut holds: BTreeMap<(String, &'static str), Instant> = BTreeMap::new();
    let (up_tx, mut up) = mpsc::channel(32);
    let _push = Abort(listen_for_pushes(ctx, cloud, up_tx).await);
    loop {
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => order(&known, &stations, command, &done_tx),
                None => return Ok(()),
            },
            _ = rescan.tick() => {
                if let Err(e) = inventory(ctx, cloud, &mut known, &mut cameras, &mut stations).await {
                    if matches!(e.downcast_ref::<Refusal>(), Some(Refusal::Expired)) {
                        // Log in again on the next run.
                        cloud.session.token.clear();
                        save(ctx, &cloud.session).await?;
                    }
                    return Err(e);
                }
                ctx.ready();
            }
            Some(message) = up.recv() => match message {
                push::Up::Connected => tracing::info!(instance = %ctx.instance(), "eufy: listening for events"),
                push::Up::Push(push) => {
                    for change in events::read(&push) {
                        apply(ctx, &known, &cameras, cloud.token(), &mut holds, change);
                    }
                }
                push::Up::Refused => {
                    // A stale identity with Google: a new one next run.
                    ctx.forget_secret(PUSH).await?;
                    anyhow::bail!("Google refused Moli's push identity");
                }
            },
            Some((serial, mode)) = done.recv() => {
                apply(ctx, &known, &cameras, cloud.token(), &mut holds, events::Change::Mode { serial, mode });
            }
            _ = expire.tick(), if !holds.is_empty() => {
                let now = Instant::now();
                holds.retain(|(serial, key), until| {
                    if *until > now {
                        return true;
                    }
                    ctx.set_state(&ctx.device_id(serial), key, Value::Bool(false));
                    false
                });
            }
        }
    }
}

/// Aborts the task when the driver's run ends.
struct Abort(Option<tokio::task::JoinHandle<()>>);

impl Drop for Abort {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// Moli's push identity (kept, or a new one), given to Eufy, then the
/// connection to Google in its own task. Without it the devices are still
/// listed: events wait for the next run.
async fn listen_for_pushes(
    ctx: &DriverCtx,
    cloud: &mut Cloud,
    up: mpsc::Sender<push::Up>,
) -> Option<tokio::task::JoinHandle<()>> {
    let instance = ctx.instance().to_string();
    let stored = ctx
        .secret(PUSH)
        .and_then(|text| serde_json::from_str::<push::Credentials>(&text).ok());
    let creds = if let Some(creds) = stored {
        creds
    } else {
        match push::register().await {
            Ok(creds) => {
                let text = serde_json::to_string(&creds).ok()?;
                if let Err(e) = ctx.store_secret(PUSH, &text).await {
                    tracing::warn!(instance, error = %e, "eufy: push identity not kept");
                }
                tracing::info!(instance, "eufy: registered for push notifications");
                creds
            }
            Err(e) => {
                tracing::warn!(
                    instance,
                    error = format!("{e:#}"),
                    "eufy: no events (push registration failed)"
                );
                return None;
            }
        }
    };
    if let Err(e) = cloud.register_push(&creds.token).await {
        tracing::warn!(
            instance,
            error = format!("{e:#}"),
            "eufy: no events (Eufy refused the push token)"
        );
        return None;
    }
    Some(tokio::spawn(push::run(creds, up, instance)))
}

/// How a station's guard mode is set, in P2P on the home network.
#[derive(Clone, Debug)]
struct Arming {
    station: p2p::Station,
    /// Recent firmwares take a JSON payload, older ones and the Wi-Fi
    /// indoor cameras a number (as the library decides).
    payload: bool,
    admin_user_id: String,
    nick_name: String,
}

impl Arming {
    fn of(station: &cloud::Station) -> Option<Self> {
        if station.ip_addr.is_empty() || station.p2p_did.is_empty() {
            return None;
        }
        let serial = &station.serial;
        let integrated = INTEGRATED.iter().any(|p| serial.starts_with(p));
        let solo = SOLO.iter().any(|p| serial.starts_with(p));
        Some(Self {
            station: p2p::Station {
                serial: serial.clone(),
                ip: station.ip_addr.clone(),
                p2p_did: station.p2p_did.clone(),
            },
            payload: (at_least(&station.main_sw_version, [2, 0, 7, 9]) && !integrated) || solo,
            admin_user_id: station.member.admin_user_id.clone(),
            nick_name: station.member.name().to_owned(),
        })
    }

    fn command(&self, mode: i64) -> (u16, Vec<u8>, Option<[u8; 16]>) {
        let key = p2p::level1_key(&self.station.serial, &self.station.p2p_did);
        let value = u32::try_from(mode).unwrap_or(0);
        if self.payload {
            let json = serde_json::json!({
                "account_id": self.admin_user_id,
                "cmd": p2p::CMD_SET_ARMING,
                "mValue3": 0,
                "payload": { "mode_type": mode, "user_name": self.nick_name },
            });
            let body = p2p::string_command(
                key.as_ref(),
                p2p::CMD_SET_PAYLOAD,
                &json.to_string(),
                p2p::STATION,
            );
            (p2p::CMD_SET_PAYLOAD, body, key)
        } else {
            let body = p2p::int_command(
                key.as_ref(),
                p2p::CMD_SET_ARMING,
                value,
                &self.admin_user_id,
                p2p::STATION,
            );
            (p2p::CMD_SET_ARMING, body, key)
        }
    }
}

/// Serial prefixes the library calls « integrated » and « solo »
/// (`isIntegratedDeviceBySn`, `isSoloCameraBySn`).
const INTEGRATED: [&str; 22] = [
    "T8420", "T820", "T8410", "T8400", "T8401", "T8411", "T8414", "T8130", "T8131", "T8171",
    "T8172", "T8173", "T86P2", "T8214", "T8422", "T8423", "T8424", "T8425", "T8426", "T8440",
    "T8441", "T8442",
];
const SOLO: [&str; 9] = [
    "T8130", "T8131", "T8122", "T8123", "T8124", "T8171", "T8173", "T86P2", "T8134",
];

/// `2.0.7.9` or later (unreadable: no).
fn at_least(version: &str, minimum: [u32; 4]) -> bool {
    let parts: Option<Vec<u32>> = version.trim().split('.').map(|p| p.parse().ok()).collect();
    let Some(mut parts) = parts else {
        return false;
    };
    parts.resize(4, 0);
    parts.as_slice() >= minimum.as_slice()
}

/// An order to an Eufy device: only the guard mode is writable. Sent in
/// its own task; the result comes back through `done`.
fn order(
    known: &BTreeMap<String, Device>,
    stations: &BTreeMap<String, Arming>,
    command: moli_runtime::CommandRequest,
    done: &mpsc::Sender<(String, i64)>,
) {
    if &*command.key != "guard_mode" {
        command.reply(Err(moli_i18n::tr!("pilotes.eufy.lecture_seule")));
        return;
    }
    let mode = match &command.value {
        Value::Text(text) => MODES
            .iter()
            .find(|(_, name)| *name == &**text)
            .map(|(n, _)| *n),
        _ => None,
    };
    let serial = known
        .iter()
        .find(|(_, d)| d.id == command.device.id)
        .map(|(s, _)| s.clone());
    let (Some(mode), Some(serial)) = (mode, serial) else {
        command.reply(Err(moli_i18n::tr!("pilotes.eufy.mode_inconnu")));
        return;
    };
    let Some(arming) = stations.get(&serial).cloned() else {
        command.reply(Err(moli_i18n::tr!("pilotes.eufy.pas_de_p2p")));
        return;
    };
    let done = done.clone();
    tokio::spawn(async move {
        if command.abandoned() {
            return;
        }
        let (kind, body, key) = arming.command(mode);
        // Answered before the hub gives up on us.
        let limit = command
            .deadline
            .saturating_duration_since(std::time::Instant::now())
            .saturating_sub(Duration::from_millis(300));
        let outcome = p2p::send(&arming.station, kind, body, key, limit).await;
        tracing::info!(station = %arming.station.serial, mode, payload = arming.payload, outcome = ?outcome.as_ref().map_err(ToString::to_string), "eufy: guard mode order");
        match outcome {
            Ok(0) => {
                command.reply(Ok(()));
                let _ = done.send((serial, mode)).await;
            }
            Ok(code) => command.reply(Err(moli_i18n::tr!("pilotes.eufy.mode_refuse", code = code))),
            Err(e) => command.reply(Err(format!("{e:#}"))),
        }
    });
}

/// A camera's picture: the station's P2P id keys it, the last one is kept.
#[derive(Debug)]
struct Camera {
    p2p_did: String,
    last: Arc<image::Last>,
}

fn apply(
    ctx: &DriverCtx,
    known: &BTreeMap<String, Device>,
    cameras: &BTreeMap<String, Camera>,
    token: &str,
    holds: &mut BTreeMap<(String, &'static str), Instant>,
    change: events::Change,
) {
    match change {
        events::Change::Detected { serial, key } => {
            let Some(device) = known.get(&serial) else {
                return;
            };
            if !device.points.iter().any(|p| &*p.key == key) {
                return;
            }
            ctx.set_state(&device.id, key, Value::Bool(true));
            holds.insert((serial, key), Instant::now() + HOLD);
        }
        events::Change::Mode { serial, mode } => {
            if let (Some(device), Some((_, mode))) =
                (known.get(&serial), MODES.iter().find(|(m, _)| *m == mode))
            {
                ctx.set_state(&device.id, "guard_mode", Value::Text((*mode).into()));
            }
        }
        events::Change::Picture { serial, url } => {
            let Some(camera) = cameras.get(&serial) else {
                return;
            };
            let (did, last, token) = (
                camera.p2p_did.clone(),
                Arc::clone(&camera.last),
                token.to_owned(),
            );
            let instance = ctx.instance().to_string();
            // Fetched aside: the driver keeps listening meanwhile.
            tokio::spawn(async move {
                match image::fetch(&url, &token).await {
                    Ok(data) => {
                        if let Some(jpeg) = image::decode(&did, &data) {
                            last.set(jpeg);
                        } else {
                            tracing::info!(
                                instance,
                                shape = image::shape(&data),
                                "eufy: picture not understood"
                            );
                        }
                    }
                    Err(e) => tracing::info!(instance, error = %e, "eufy: picture not fetched"),
                }
            });
        }
    }
}

/// One Moli device per station and per device; a Wi-Fi camera is both a
/// station and a device: one Moli device.
async fn inventory(
    ctx: &DriverCtx,
    cloud: &mut Cloud,
    known: &mut BTreeMap<String, Device>,
    cameras: &mut BTreeMap<String, Camera>,
    arming: &mut BTreeMap<String, Arming>,
) -> anyhow::Result<()> {
    let stations = cloud.stations().await.context("Eufy stations")?;
    let devices = cloud.devices().await.context("Eufy devices")?;
    arming.clear();
    for station in &stations {
        if let Some(a) = Arming::of(station) {
            arming.insert(station.serial.clone(), a);
        }
    }
    let mut seen = Vec::new();
    for device in &devices {
        let station = stations.iter().find(|s| s.serial == device.serial);
        let mut params = device.params.clone();
        if let Some(station) = station {
            params.extend(station.params.iter().cloned());
        }
        let found = Found {
            serial: &device.serial,
            name: &device.name,
            model: &device.model,
            params: &params,
            station: &device.station_sn,
        };
        publish(ctx, known, &found);
        let did = stations
            .iter()
            .find(|s| s.serial == device.station_sn || s.serial == device.serial)
            .map(|s| s.p2p_did.as_str())
            .unwrap_or_default();
        watch_pictures(ctx, cameras, &found, did);
        seen.push(device.serial.clone());
    }
    let alone: Vec<_> = stations
        .iter()
        .filter(|s| !seen.contains(&s.serial))
        .collect();
    for station in alone {
        let found = Found {
            serial: &station.serial,
            name: &station.name,
            model: &station.model,
            params: &station.params,
            station: "",
        };
        publish(ctx, known, &found);
        watch_pictures(ctx, cameras, &found, &station.p2p_did);
        seen.push(station.serial.clone());
    }
    for gone in known
        .keys()
        .filter(|k| !seen.contains(k))
        .cloned()
        .collect::<Vec<_>>()
    {
        if let Some(device) = known.remove(&gone) {
            ctx.remove_device(&device.id);
        }
    }
    if known.is_empty() {
        tracing::warn!(instance = %ctx.instance(), "eufy: the account sees no device (shared with it?)");
    }
    Ok(())
}

struct Found<'a> {
    serial: &'a str,
    name: &'a str,
    model: &'a str,
    params: &'a [cloud::Param],
    /// The HomeBase it hangs from (empty: it is its own station).
    station: &'a str,
}

fn read_only(key: &str, label: String, kind: Kind, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        semantic: Semantic::infer(key, unit.as_ref()),
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
    }
}

fn values(model: &str, params: &[cloud::Param]) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    if !events::wired(model)
        && let Some(level) =
            cloud::param(params, BATTERY).and_then(|v| v.trim().parse::<i64>().ok())
        && (0..=100).contains(&level)
    {
        out.push(("battery", Value::Int(level)));
    }
    if let Some(mode) = cloud::param(params, GUARD_MODE)
        .and_then(|v| v.trim().parse::<i64>().ok())
        .and_then(|n| MODES.iter().find(|(m, _)| *m == n))
    {
        out.push(("guard_mode", Value::Text(mode.1.into())));
    }
    out
}

fn device(ctx: &DriverCtx, found: &Found<'_>, values: &[(&str, Value)]) -> Device {
    let mut points = Vec::new();
    for (key, _) in values {
        points.push(match *key {
            "battery" => read_only(
                "battery",
                moli_i18n::tr!("pilotes.eufy.batterie"),
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: Some(1.0),
                },
                Some(Unit::Percent),
            ),
            // Writable, in P2P; an agent's order waits for a person.
            _ => PointSpec {
                access: Access {
                    read: true,
                    write: true,
                },
                semantic: Semantic::Control,
                ..read_only(
                    "guard_mode",
                    moli_i18n::tr!("pilotes.eufy.mode"),
                    Kind::Enum {
                        values: MODES.iter().map(|(_, v)| (*v).into()).collect(),
                    },
                    None,
                )
            },
        });
    }
    for key in events::detections(found.model) {
        if let Some((_, label)) = events::DETECTIONS.iter().find(|(k, _)| k == key) {
            points.push(read_only(key, moli_i18n::tr(label), Kind::Binary, None));
        }
    }
    Device {
        id: ctx.device_id(found.serial),
        instance: ctx.instance().clone(),
        native_name: if found.name.is_empty() {
            found.model.into()
        } else {
            found.name.into()
        },
        manufacturer: Some("Eufy".into()),
        model: (!found.model.is_empty()).then(|| found.model.into()),
        description: None,
        native_room: None,
        parent: (!found.station.is_empty() && found.station != found.serial)
            .then(|| ctx.device_id(found.station)),
        members: Vec::new(),
        points,
    }
}

/// A camera (a model that detects) shows its last event picture.
fn watch_pictures(
    ctx: &DriverCtx,
    cameras: &mut BTreeMap<String, Camera>,
    found: &Found<'_>,
    p2p_did: &str,
) {
    if found.serial.is_empty() || events::detections(found.model).is_empty() {
        return;
    }
    if let Some(camera) = cameras.get_mut(found.serial) {
        p2p_did.clone_into(&mut camera.p2p_did);
        return;
    }
    let last = Arc::new(image::Last::default());
    ctx.provide_snapshots(
        &ctx.device_id(found.serial),
        Arc::new(image::Pictures(Arc::clone(&last))),
    );
    cameras.insert(
        found.serial.to_owned(),
        Camera {
            p2p_did: p2p_did.to_owned(),
            last,
        },
    );
}

fn publish(ctx: &DriverCtx, known: &mut BTreeMap<String, Device>, found: &Found<'_>) {
    if found.serial.is_empty() {
        return;
    }
    let values = values(found.model, found.params);
    let device = device(ctx, found, &values);
    let id = device.id.clone();
    if known.get(found.serial) != Some(&device) {
        ctx.upsert_device(device.clone());
        known.insert(found.serial.to_owned(), device);
    }
    ctx.set_availability(&id, true);
    for (key, value) in values {
        ctx.set_state(&id, key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::Param;

    fn p(kind: i64, value: &str) -> Param {
        Param {
            param_type: kind,
            param_value: value.into(),
        }
    }

    #[test]
    fn battery_and_guard_mode_are_read_from_parameters() {
        let v = values("T8111", &[p(1101, "87"), p(1224, "63"), p(9999, "x")]);
        assert_eq!(
            v,
            vec![
                ("battery", Value::Int(87)),
                ("guard_mode", Value::Text("disarmed".into()))
            ]
        );
        assert!(values("T8111", &[p(1101, "250"), p(1224, "12")]).is_empty());
        // A wired light camera's « battery » is not shown.
        assert!(values("T84A1", &[p(1101, "100")]).is_empty());
    }

    #[test]
    fn firmware_versions_are_compared_part_by_part() {
        assert!(at_least("2.0.7.9", [2, 0, 7, 9]));
        assert!(at_least("3.2.4.1", [2, 0, 7, 9]));
        assert!(at_least("2.1", [2, 0, 7, 9]));
        assert!(!at_least("2.0.7.8", [2, 0, 7, 9]));
        assert!(!at_least("1.10.0.0", [2, 0, 7, 9]));
        assert!(!at_least("", [2, 0, 7, 9]));
    }

    #[test]
    fn a_homebase_takes_a_payload_an_indoor_camera_a_number() {
        let station = |serial: &str, version: &str| cloud::Station {
            serial: serial.into(),
            name: String::new(),
            model: String::new(),
            p2p_did: "ABCDEFG-012345-HIJKL".into(),
            ip_addr: "192.168.1.10".into(),
            main_sw_version: version.into(),
            member: cloud::Member {
                admin_user_id: "u1".into(),
                nick_name: "Moli".into(),
                nick: String::new(),
            },
            params: Vec::new(),
        };
        let hb = Arming::of(&station("T8010P0000001234", "3.2.4.1")).unwrap();
        assert!(hb.payload);
        let (kind, _, key) = hb.command(63);
        assert_eq!(kind, p2p::CMD_SET_PAYLOAD);
        assert!(key.is_some());
        let indoor = Arming::of(&station("T8410P0000001234", "2.2.1.0")).unwrap();
        assert!(!indoor.payload);
        assert_eq!(indoor.command(1).0, p2p::CMD_SET_ARMING);
        let mut far = station("T8010P0000001234", "3.2.4.1");
        far.ip_addr.clear();
        assert!(Arming::of(&far).is_none());
    }

    #[test]
    fn the_captcha_picture_is_read_from_its_data_url() {
        let image = captcha_image("data:image/png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(image.content_type, "image/png");
        assert_eq!(image.bytes, b"\x89PNG\r\n\x1a\n");
        assert!(captcha_image("data:text/html;base64,PGI+").is_none());
        assert!(captcha_image("https://example.org/x.png").is_none());
    }

    #[test]
    fn the_configuration_is_checked() {
        let ok = |country: &str, language: &str| {
            Eufy::new(Config {
                country: country.into(),
                language: language.into(),
            })
            .is_ok()
        };
        assert!(ok("FR", "fr"));
        assert!(!ok("FRA", "fr"));
        assert!(!ok("F1", "fr"));
        assert!(!ok("FR", ""));
    }
}
