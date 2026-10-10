//! Reolink cameras and doorbells behind a Home Hub (or one camera/NVR),
//! through the HTTPS API: motion, AI detections (person, vehicle, animal,
//! package) and the doorbell button. A camera with a speaker (a doorbell)
//! also says what Moli writes to its `say` point (`talk.rs`).
//!
//! Credentials come from the encrypted secret store (`username`,
//! `password`). The station's self-signed certificate is pinned on first
//! contact, *before* any credential is sent. Events are polled once a
//! second on a kept-alive connection; one session (token) is kept across
//! driver restarts, since stations cap concurrent sessions.

mod talk;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_net::{Method, PIN_MISMATCH, PinnedHttps};
use moli_runtime::media::{Image, SnapshotSource};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::time::Instant;

const USERNAME: &str = "username";
const PASSWORD: &str = "password";
const CERT: &str = "cert";
const POLL: Duration = Duration::from_secs(1);
/// Channel list and names refresh.
const RESCAN: Duration = Duration::from_secs(300);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Renew the token this long before its lease ends.
const LEASE_MARGIN: Duration = Duration::from_secs(60);
/// What a doorbell may say at once.
const MAX_SAY: usize = 500;
/// Reolink session errors: « please login first », « login failed ».
const LOGIN_CODES: [i64; 2] = [-6, -7];

/// Event kinds as the API names them, and how Moli shows them (the key of
/// the label in the catalogue).
const EVENTS: [(&str, &str, &str); 6] = [
    ("md", "motion", "pilotes.reolink.mouvement"),
    ("people", "person", "pilotes.reolink.personne"),
    ("vehicle", "vehicle", "pilotes.reolink.vehicule"),
    ("dog_cat", "animal", "pilotes.reolink.animal"),
    ("package", "package", "pilotes.reolink.colis"),
    ("visitor", "doorbell", "pilotes.reolink.sonnette"),
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// The station's RTSP server: the doorbell's speaker is reached there.
    #[serde(default = "default_rtsp_port")]
    pub rtsp_port: u16,
}

fn default_port() -> u16 {
    443
}

fn default_rtsp_port() -> u16 {
    554
}

/// A session token and when it must be renewed. Never printed.
#[derive(Clone)]
struct Token {
    name: String,
    renew_at: Instant,
}

#[derive(Debug)]
pub struct Reolink {
    config: Config,
    /// Survives driver restarts: one session, not one per restart.
    token: Arc<Mutex<Option<Token>>>,
}

impl Reolink {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            token: Arc::new(Mutex::new(None)),
        }
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl Driver for Reolink {
    fn kind(&self) -> &'static str {
        "reolink"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, Arc::clone(&self.token), ctx))
    }
}

/// A logged-in session with the station.
struct Session {
    http: PinnedHttps,
    username: String,
    password: String,
    token: Arc<Mutex<Option<Token>>>,
}

/// The answer of one command in a batch, or why it failed.
fn value(answer: &Json) -> Result<&Json, String> {
    if answer["code"].as_i64().unwrap_or(0) == 0 {
        Ok(&answer["value"])
    } else {
        Err(answer["error"]["detail"]
            .as_str()
            .unwrap_or("error")
            .to_owned())
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("station", &self.http.host())
            .finish_non_exhaustive()
    }
}

/// One camera's images, through the station's session.
#[derive(Debug)]
struct ChannelImages {
    session: Arc<Session>,
    channel: u64,
}

impl SnapshotSource for ChannelImages {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<Image>> {
        Box::pin(self.session.snap(self.channel))
    }
}

impl Session {
    /// A JPEG from one channel. A refusal (expired token…) logs in again
    /// once. The token travels in the URL: no error ever quotes the URL.
    async fn snap(&self, channel: u64) -> anyhow::Result<Image> {
        for attempt in 0..2 {
            let token = match self.current() {
                Some(token) => token,
                None => self.login().await?,
            };
            let path = format!(
                "/cgi-bin/api.cgi?cmd=Snap&channel={channel}&rs=moli{}&token={token}",
                moli_core::now_ms()
            );
            let request = moli_net::empty(self.http.request(Method::GET, &path))?;
            // A battery camera wakes up first: up to ~10 s.
            let (status, bytes) = self
                .http
                .send_slow(request, Duration::from_secs(14))
                .await?;
            if status.is_success() && bytes.starts_with(&[0xFF, 0xD8]) {
                return Ok(Image {
                    content_type: "image/jpeg".into(),
                    bytes: bytes.to_vec(),
                });
            }
            if attempt == 0 {
                *self.token.lock().unwrap_or_else(PoisonError::into_inner) = None;
            }
        }
        bail!(moli_i18n::tr!("pilotes.reolink.aucune_image"))
    }

    fn current(&self) -> Option<String> {
        let token = self.token.lock().unwrap_or_else(PoisonError::into_inner);
        token
            .as_ref()
            .filter(|t| Instant::now() < t.renew_at)
            .map(|t| t.name.clone())
    }

    /// One batch; the per-command answers (each may hold its own error).
    async fn call(&self, cmd: &str, body: &Json) -> anyhow::Result<Vec<Json>> {
        let token = match self.current() {
            Some(token) => token,
            None => self.login().await?,
        };
        match self.raw(cmd, body, Some(&token)).await {
            Err(e) if e.to_string().contains("new login") => {
                // The station forgot us (restart): once more.
                let token = self.login().await?;
                self.raw(cmd, body, Some(&token)).await
            }
            other => other,
        }
    }

    async fn raw(&self, cmd: &str, body: &Json, token: Option<&str>) -> anyhow::Result<Vec<Json>> {
        let mut path = format!("/api.cgi?cmd={cmd}");
        if let Some(token) = token {
            path.push_str("&token=");
            path.push_str(token);
        }
        let request = moli_net::json_body(self.http.request(Method::POST, &path), body)?;
        let (status, bytes) = if cmd == "Login" {
            self.http.send_once(request).await?
        } else {
            self.http.send(request).await?
        };
        if !status.is_success() {
            bail!("station answered {status}");
        }
        let answers: Vec<Json> = serde_json::from_slice(&bytes).context("station answer")?;
        if answers
            .iter()
            .any(|a| LOGIN_CODES.contains(&a["error"]["rspCode"].as_i64().unwrap_or(0)))
        {
            bail!("station wants a new login");
        }
        Ok(answers)
    }

    async fn login(&self) -> anyhow::Result<String> {
        *self.token.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let body = json!([{ "cmd": "Login", "action": 0, "param": { "User": {
            "Version": "0", "userName": self.username, "password": self.password } } }]);
        let answers = self.raw("Login", &body, None).await.context("login")?;
        let token = answers
            .first()
            .map(value)
            .context("login: empty answer")?
            .map_err(|e| anyhow::anyhow!("login refused: {e}"))?;
        let name = token["Token"]["name"]
            .as_str()
            .context("login: no token")?
            .to_owned();
        let lease = Duration::from_secs(token["Token"]["leaseTime"].as_u64().unwrap_or(3600));
        *self.token.lock().unwrap_or_else(PoisonError::into_inner) = Some(Token {
            name: name.clone(),
            renew_at: Instant::now() + lease.saturating_sub(LEASE_MARGIN),
        });
        Ok(name)
    }
}

/// One camera (or doorbell) channel of the station.
#[derive(Clone, Debug, PartialEq)]
struct Channel {
    number: u64,
    /// The camera's own UID: its identity, whatever channel it is plugged
    /// into. Empty on firmwares that do not report it.
    uid: String,
    name: String,
    online: bool,
    /// API event names this channel supports.
    events: Vec<&'static str>,
    /// It has a speaker Moli can talk through (`None`: not asked yet).
    talks: Option<bool>,
}

fn point(key: &str, label: &str) -> PointSpec {
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

fn channel_id(ctx: &DriverCtx, channel: &Channel) -> DeviceId {
    if channel.uid.is_empty() {
        ctx.device_id(&format!("ch{}", channel.number))
    } else {
        ctx.device_id(&channel.uid)
    }
}

fn channel_device(ctx: &DriverCtx, model: &str, channel: &Channel) -> Device {
    Device {
        id: channel_id(ctx, channel),
        instance: ctx.instance().clone(),
        native_name: channel.name.as_str().into(),
        manufacturer: Some("Reolink".into()),
        model: Some(model.into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: EVENTS
            .iter()
            .filter(|(api, _, _)| channel.events.contains(api))
            .map(|(_, key, label)| point(key, &moli_i18n::tr(label)))
            .chain((channel.talks == Some(true)).then(|| PointSpec {
                kind: Kind::Text,
                access: Access {
                    read: true,
                    write: true,
                },
                semantic: Semantic::Other,
                ..point("say", &moli_i18n::tr!("pilotes.reolink.dire"))
            }))
            .collect(),
    }
}

/// Which events a `GetEvents` answer says the channel supports, and their
/// states (`md`/`visitor` at the top, AI kinds under `ai`).
fn read_events(value: &Json) -> Vec<(&'static str, bool, bool)> {
    EVENTS
        .iter()
        .filter_map(|(api, _, _)| {
            let e = value.get(*api).or_else(|| value["ai"].get(*api))?;
            Some((
                *api,
                e["support"].as_i64() == Some(1),
                e["alarm_state"].as_i64() == Some(1),
            ))
        })
        .collect()
}

/// The station's model and channels (named or online ones).
/// The device and its channels. `verbose`: say what each slot holds (the
/// first scan, so that a missing camera can be explained from the log).
async fn scan(session: &Session, verbose: bool) -> anyhow::Result<(String, Vec<Channel>)> {
    let info = session
        .call(
            "GetDevInfo",
            &json!([{ "cmd": "GetDevInfo", "action": 0, "param": {} }]),
        )
        .await?;
    let model = info
        .first()
        .and_then(|a| value(a).ok())
        .and_then(|v| v["DevInfo"]["model"].as_str())
        .unwrap_or("Reolink")
        .to_owned();
    let status = session
        .call(
            "GetChannelstatus",
            &json!([{ "cmd": "GetChannelstatus", "action": 0, "param": {} }]),
        )
        .await?;
    let status = status
        .first()
        .map(value)
        .context("no channel status")?
        .map_err(|e| anyhow::anyhow!("channel status: {e}"))?;
    let mut channels = Vec::new();
    for ch in status["status"].as_array().into_iter().flatten() {
        let number = ch["channel"].as_u64().unwrap_or(0);
        let online = ch["online"].as_i64() == Some(1);
        let name = ch["name"].as_str().filter(|n| !n.is_empty());
        let uid = ch["uid"].as_str().filter(|u| !u.is_empty());
        if verbose {
            tracing::info!(
                number,
                online,
                sleeping = ch["sleep"].as_i64() == Some(1),
                named = name.is_some(),
                camera = uid.is_some(),
                "reolink channel"
            );
        }
        // An empty slot has neither name nor camera; an offline camera
        // (battery, unplugged) still has its uid.
        if !online && name.is_none() && uid.is_none() {
            continue;
        }
        let events = if online {
            let answers = session
                .call(
                    "GetEvents",
                    &json!([{ "cmd": "GetEvents", "action": 0, "param": { "channel": number } }]),
                )
                .await?;
            answers
                .first()
                .and_then(|a| value(a).ok())
                .map(|v| {
                    read_events(v)
                        .into_iter()
                        .filter(|(_, support, _)| *support)
                        .map(|(api, _, _)| api)
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        channels.push(Channel {
            number,
            uid: ch["uid"].as_str().unwrap_or_default().to_owned(),
            name: name.map_or_else(
                || moli_i18n::tr!("pilotes.reolink.camera", number = number),
                str::to_owned,
            ),
            online,
            events,
            talks: None,
        });
    }
    Ok((model, channels))
}

async fn run(
    config: &Config,
    token: Arc<Mutex<Option<Token>>>,
    ctx: &mut DriverCtx,
) -> anyhow::Result<()> {
    let (Some(username), Some(password)) = (ctx.secret(USERNAME), ctx.secret(PASSWORD)) else {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.reolink.identifiants_manquants",
            instance = ctx.instance(),
            username = USERNAME,
            password = PASSWORD
        ));
        ctx.cancelled().await;
        return Ok(());
    };
    // Pin the certificate before any credential leaves.
    let pin = if let Some(pin) = ctx.secret(CERT) {
        pin
    } else {
        first_pin(config, ctx).await?
    };
    let station = talk::Station {
        host: config.host.clone(),
        port: config.rtsp_port,
        user: username.clone(),
        password: password.clone(),
    };
    let session = Arc::new(Session {
        http: PinnedHttps::new(&config.host, config.port, Some(pin))?,
        username,
        password,
        token,
    });
    serve(ctx, &session, &station).await
}

/// First contact: a bare handshake records the certificate, stored before
/// anything else is sent.
async fn first_pin(config: &Config, ctx: &DriverCtx) -> anyhow::Result<String> {
    if !ctx.can_store_secrets() {
        bail!("cannot pin the station's certificate: the secret store is unavailable");
    }
    let probe = PinnedHttps::new(&config.host, config.port, None)?;
    probe.handshake().await.context("first contact")?;
    let seen = probe.seen_fingerprint().context("no certificate seen")?;
    ctx.store_secret(CERT, &seen).await?;
    tracing::info!(instance = %ctx.instance(), "reolink certificate pinned");
    Ok(seen)
}

async fn serve(
    ctx: &mut DriverCtx,
    session: &Arc<Session>,
    station: &talk::Station,
) -> anyhow::Result<()> {
    // One sentence at a time through the station.
    let turn = Arc::new(tokio::sync::Mutex::new(()));
    let mut model = String::from("Reolink");
    let mut channels: BTreeMap<u64, Channel> = BTreeMap::new();
    let mut backoff = Duration::from_secs(2);
    let mut retry_at: Option<Instant> = Some(Instant::now());
    let mut rescan_at = Instant::now();
    let mut ready = false;
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => {
                    let result = speak(ctx, station, &turn, &channels, &command);
                    command.reply(result);
                }
                None => return Ok(()),
            },
            _ = poll.tick() => {
                if retry_at.is_some_and(|t| Instant::now() < t) {
                    continue;
                }
                let result = if retry_at.is_some() || Instant::now() >= rescan_at {
                    rescan(ctx, session, station, &mut model, &mut channels).await.map(|()| {
                        rescan_at = Instant::now() + RESCAN;
                    })
                } else {
                    poll_events(ctx, session, &channels).await
                };
                match result {
                    Ok(()) => {
                        retry_at = None;
                        backoff = Duration::from_secs(2);
                        if !ready {
                            ready = true;
                            ctx.ready();
                            tracing::info!(instance = %ctx.instance(), channels = channels.len(), "reolink station loaded");
                        }
                    }
                    Err(e) if format!("{e:#}").contains(PIN_MISMATCH) => {
                        unavailable(ctx, &channels);
                        ctx.wait_for(moli_i18n::tr!(
                            "pilotes.reolink.certificat_change",
                            instance = ctx.instance(),
                            key = CERT
                        ));
                        ctx.cancelled().await;
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::warn!(instance = %ctx.instance(), error = %e, retry_in = ?backoff, "reolink station unreachable");
                        unavailable(ctx, &channels);
                        retry_at = Some(Instant::now() + backoff);
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                    }
                }
            }
        }
    }
}

/// Station down: nothing is known any more — no stale « person seen ».
fn unavailable(ctx: &DriverCtx, channels: &BTreeMap<u64, Channel>) {
    for channel in channels.values() {
        let id = channel_id(ctx, channel);
        ctx.set_availability(&id, false);
        for (api, key, _) in EVENTS {
            if channel.events.contains(&api) {
                ctx.set_state(&id, key, Value::Null);
            }
        }
    }
}

async fn rescan(
    ctx: &DriverCtx,
    session: &Arc<Session>,
    station: &talk::Station,
    model: &mut String,
    channels: &mut BTreeMap<u64, Channel>,
) -> anyhow::Result<()> {
    let (found_model, found) = scan(session, channels.is_empty()).await?;
    *model = found_model;
    for channel in found {
        let known = channels.get(&channel.number);
        // An offline channel keeps the events it had.
        let mut channel = match known {
            Some(old) if !channel.online => Channel {
                events: old.events.clone(),
                ..channel
            },
            _ => channel,
        };
        // Whether it has a speaker: asked once, when it is first online.
        channel.talks = known.and_then(|old| old.talks);
        if channel.talks.is_none() && channel.online {
            ask_speaker(ctx, station, &mut channel).await;
        }
        if known != Some(&channel) {
            ctx.upsert_device(channel_device(ctx, model, &channel));
            ctx.provide_snapshots(
                &channel_id(ctx, &channel),
                Arc::new(ChannelImages {
                    session: Arc::clone(session),
                    channel: channel.number,
                }),
            );
        }
        ctx.set_availability(&channel_id(ctx, &channel), channel.online);
        channels.insert(channel.number, channel);
    }
    Ok(())
}

/// Asks the station whether `channel` has a speaker; unanswered, it is
/// asked again at the next scan.
async fn ask_speaker(ctx: &DriverCtx, station: &talk::Station, channel: &mut Channel) {
    match talk::offers(station, channel.number).await {
        Ok(talks) => {
            tracing::info!(instance = %ctx.instance(), channel = channel.number, talks, "reolink speaker");
            channel.talks = Some(talks);
        }
        Err(e) => tracing::debug!(
            instance = %ctx.instance(),
            channel = channel.number,
            error = %format!("{e:#}"),
            "reolink back channel not asked"
        ),
    }
}

/// A sentence through a channel's speaker: checked now, said in the
/// background (Moli's voice is made, then sent in real time), one at a time.
fn speak(
    ctx: &DriverCtx,
    station: &talk::Station,
    turn: &Arc<tokio::sync::Mutex<()>>,
    channels: &BTreeMap<u64, Channel>,
    command: &CommandRequest,
) -> Result<(), String> {
    if &*command.key != "say" {
        return Err(moli_i18n::tr!("pilotes.reolink.lecture_seule"));
    }
    let channel = channels
        .values()
        .find(|c| c.talks == Some(true) && channel_id(ctx, c) == command.device.id)
        .ok_or_else(|| moli_i18n::tr!("pilotes.reolink.sans_haut_parleur"))?;
    let text = match &command.value {
        Value::Text(t) if !t.trim().is_empty() && t.trim().chars().count() <= MAX_SAY => {
            t.trim().to_owned()
        }
        _ => return Err(moli_i18n::tr!("pilotes.reolink.phrase")),
    };
    let brain = ctx
        .voice_brain()
        .ok_or_else(|| moli_i18n::tr!("pilotes.reolink.sans_voix"))?;
    let (station, turn, number) = (station.clone(), Arc::clone(turn), channel.number);
    let instance = ctx.instance().to_string();
    tokio::spawn(async move {
        let _turn = turn.lock().await;
        let result = async {
            let pcm = brain.voice_pcm(&text).await.map_err(anyhow::Error::msg)?;
            #[allow(clippy::cast_precision_loss)]
            let seconds = pcm.samples.len() as f64 / f64::from(pcm.rate.max(1));
            let limit = Duration::from_secs_f64(seconds) + Duration::from_secs(15);
            tokio::time::timeout(limit, talk::say(&station, number, &pcm))
                .await
                .context("the doorbell took too long")?
        }
        .await;
        match result {
            Ok(()) => tracing::info!(instance, channel = number, "said through the doorbell"),
            Err(e) => {
                tracing::warn!(instance, channel = number, error = %format!("{e:#}"), "doorbell speech failed");
            }
        }
    });
    Ok(())
}

async fn poll_events(
    ctx: &DriverCtx,
    session: &Session,
    channels: &BTreeMap<u64, Channel>,
) -> anyhow::Result<()> {
    let online: Vec<&Channel> = channels.values().filter(|c| c.online).collect();
    if online.is_empty() {
        return Ok(());
    }
    let body: Vec<Json> = online
        .iter()
        .map(|c| json!({ "cmd": "GetEvents", "action": 0, "param": { "channel": c.number } }))
        .collect();
    let answers = session.call("GetEvents", &Json::Array(body)).await?;
    for (channel, answer) in online.iter().zip(&answers) {
        let id = channel_id(ctx, channel);
        let Ok(value) = value(answer) else {
            // This channel only: the others still count.
            ctx.set_availability(&id, false);
            continue;
        };
        ctx.set_availability(&id, true);
        for (api, support, active) in read_events(value) {
            if support && let Some((_, key, _)) = EVENTS.iter().find(|(a, _, _)| *a == api) {
                ctx.set_state(&id, key, Value::Bool(active));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_read_from_both_levels() {
        // As the Home Hub answered for the doorbell (2026-10-03).
        let value = serde_json::json!({
            "ai": {"cry": {"alarm_state": 0, "support": 0}, "dog_cat": {"alarm_state": 0, "support": 1},
                   "people": {"alarm_state": 1, "support": 1}, "package": {"alarm_state": 0, "support": 1},
                   "vehicle": {"alarm_state": 0, "support": 1}},
            "channel": 1, "md": {"alarm_state": 1, "support": 1}, "visitor": {"alarm_state": 0, "support": 1}
        });
        let events = read_events(&value);
        assert!(events.contains(&("md", true, true)));
        assert!(events.contains(&("people", true, true)));
        assert!(events.contains(&("visitor", true, false)));
        assert_eq!(events.len(), 6);
    }

    #[test]
    fn per_command_errors_stay_per_command() {
        let ok = json!({"cmd": "GetEvents", "code": 0, "value": {"md": {}}});
        let failed = json!({"cmd": "GetEvents", "code": 1, "error": {"detail": "not support", "rspCode": -9}});
        assert!(value(&ok).is_ok());
        assert_eq!(value(&failed), Err("not support".to_owned()));
        assert!(
            format!(
                "{:?}",
                Token {
                    name: "secret".into(),
                    renew_at: Instant::now()
                }
            )
            .contains("redacted")
        );
    }
}
