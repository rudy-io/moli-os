//! Frigate NVR cameras, through the MQTT topics Frigate publishes
//! (`frigate/<camera>/…`): motion, objects in view, and its switches
//! (detection, recording, snapshots); and the server itself (`stats`:
//! version, uptime, inference speed, detection rate, GPU and CPU load). No
//! HTTP, no credentials: the same broker Zigbee2MQTT uses.

mod go2rtc;
mod review;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use moli_core::{Access, Device, Kind, PointSpec, Semantic, Value};
use moli_runtime::media::{Image, SnapshotSource};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde::Deserialize;

/// Objects counted per camera (Frigate publishes one topic per label), with
/// the key of their label in the catalogue.
const OBJECTS: [(&str, &str); 6] = [
    ("person", "pilotes.frigate.personnes"),
    ("car", "pilotes.frigate.voitures"),
    ("dog", "pilotes.frigate.chiens"),
    ("cat", "pilotes.frigate.chats"),
    ("bird", "pilotes.frigate.oiseaux"),
    ("all", "pilotes.frigate.objets"),
];
/// Frigate switches (`…/<name>/state`, set through `…/<name>/set`), with the
/// key of their label in the catalogue.
const SWITCHES: [(&str, &str, &str); 3] = [
    ("detect", "detection", "pilotes.frigate.detection"),
    ("recordings", "recording", "pilotes.frigate.enregistrement"),
    ("snapshots", "snapshots", "pilotes.frigate.instantanes"),
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_base")]
    pub base_topic: String,
    /// Frigate's HTTP API, for camera images (the internal, unauthenticated
    /// port).
    #[serde(default = "default_api")]
    pub api: String,
    /// A go2rtc serving these cameras (`http://host:1984`): their live video
    /// (WebRTC, played by the browser) and their images, instead of the API.
    #[serde(default)]
    pub go2rtc: Option<String>,
    /// Frigate camera → its stream on that go2rtc, when the names differ.
    #[serde(default)]
    pub streams: BTreeMap<String, String>,
    /// Frigate's authenticated API, for the cameras' past (events,
    /// recordings, zones): a « viewer » account, its password in the vault
    /// (`password`), Frigate's certificate pinned on first contact.
    #[serde(default)]
    pub review: Option<ReviewConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
    pub host: String,
    #[serde(default = "default_review_port")]
    pub port: u16,
    #[serde(default = "default_review_user")]
    pub user: String,
    /// Frigate's certificate fingerprint (SHA-256, hex), written by a
    /// person: it wins over the one pinned on first contact, for whoever
    /// wants no trust on first use.
    #[serde(default)]
    pub cert_sha256: Option<String>,
}

fn default_review_port() -> u16 {
    8971
}

fn default_review_user() -> String {
    "moli".into()
}

/// The vault's names for the review API.
const PASSWORD: &str = "password";
const CERTIFICATE: &str = "certificate";

/// Frigate's API for the past, as the start found it.
enum Review {
    Ready(std::sync::Arc<review::Api>),
    /// Frigate shows another certificate than the pinned one: the driver
    /// stops and says why (never pinned again on its own).
    Changed(String),
}

/// Lowercase hex without separators, as `moli-net` compares fingerprints.
fn normalize(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// What to do when Frigate shows another certificate. The full
/// fingerprints, not secret, go to the log; the message tells how to accept
/// it, once a person checked it is Frigate (only from the machine Moli runs
/// on, or the configuration).
fn certificate_changed(
    r: &ReviewConfig,
    instance: &moli_core::InstanceId,
    pinned: &str,
    seen: Option<&str>,
) -> String {
    tracing::warn!(
        instance = %instance,
        pinned = %normalize(pinned),
        seen = %seen.map(normalize).unwrap_or_default(),
        "frigate certificate changed: driver stopped, password not sent"
    );
    let short = |f: &str| normalize(f).chars().take(16).collect::<String>() + "…";
    let pinned = short(pinned);
    let seen = seen.map_or_else(
        || moli_i18n::tr!("pilotes.frigate.empreinte_inconnue"),
        short,
    );
    if r.cert_sha256.is_some() {
        moli_i18n::tr!(
            "pilotes.frigate.certificat_change_config",
            host = r.host,
            pinned = pinned,
            seen = seen
        )
    } else {
        moli_i18n::tr!(
            "pilotes.frigate.certificat_change",
            host = r.host,
            pinned = pinned,
            seen = seen,
            instance = instance,
            key = CERTIFICATE
        )
    }
}

/// Frigate's API for the past, when configured and its password stored.
async fn review_api(config: &Config, ctx: &DriverCtx) -> Option<Review> {
    let r = config.review.as_ref()?;
    let Some(password) = ctx.secret(PASSWORD) else {
        tracing::warn!(instance = %ctx.instance(), "frigate review: no password in the vault (moli-os secrets set <instance> password)");
        return None;
    };
    let written = r.cert_sha256.as_deref().map(normalize);
    let pin = if let Some(pin) = written.or_else(|| ctx.secret(CERTIFICATE)) {
        // Checked now, before anything is sent: a changed certificate stops
        // the driver at start, not at the first image asked.
        let probe = moli_net::PinnedHttps::new(&r.host, r.port, Some(pin.clone())).ok()?;
        match probe.handshake().await {
            Err(e) if format!("{e:#}").contains(moli_net::PIN_MISMATCH) => {
                let seen = probe.seen_fingerprint();
                return Some(Review::Changed(certificate_changed(
                    r,
                    ctx.instance(),
                    &pin,
                    seen.as_deref(),
                )));
            }
            // Unreachable for now: the requests will say so, and a changed
            // certificate is still noticed on them.
            _ => pin,
        }
    } else {
        // First contact: pin the certificate before any password is sent.
        let probe = moli_net::PinnedHttps::new(&r.host, r.port, None).ok()?;
        if let Err(e) = probe.handshake().await {
            tracing::warn!(instance = %ctx.instance(), error = %e, "frigate review: Frigate unreachable");
            return None;
        }
        let seen = probe.seen_fingerprint()?;
        if let Err(e) = ctx.store_secret(CERTIFICATE, &seen).await {
            tracing::warn!(instance = %ctx.instance(), error = %e, "frigate review: cannot pin the certificate");
            return None;
        }
        seen
    };
    let https = moli_net::PinnedHttps::new(&r.host, r.port, Some(pin)).ok()?;
    let api = std::sync::Arc::new(review::Api::new(https, r.user.clone(), password));
    // In the background: the session and the live streams, before anyone
    // opens a camera.
    let warming = std::sync::Arc::clone(&api);
    tokio::spawn(async move { warming.warm().await });
    Some(Review::Ready(api))
}

/// Waits until Frigate shows another certificate (never, without `review`).
async fn certificate_lost(
    rx: Option<&mut tokio::sync::watch::Receiver<Option<review::Changed>>>,
) -> review::Changed {
    if let Some(rx) = rx
        && let Ok(changed) = rx.wait_for(Option::is_some).await
        && let Some(changed) = changed.clone()
    {
        return changed;
    }
    std::future::pending().await
}

/// While running, Frigate showed another certificate: why the driver stops.
fn stopped_on(config: &Config, ctx: &DriverCtx, changed: &review::Changed) -> String {
    let Some(r) = config.review.as_ref() else {
        return String::new();
    };
    let pinned = r
        .cert_sha256
        .clone()
        .or_else(|| ctx.secret(CERTIFICATE))
        .unwrap_or_default();
    certificate_changed(r, ctx.instance(), &pinned, changed.seen.as_deref())
}

/// Stopped, saying why, until Moli stops or restarts.
async fn park(ctx: &DriverCtx, reason: String) -> anyhow::Result<()> {
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

fn default_api() -> String {
    "http://127.0.0.1:5000".into()
}

/// The latest frame of one camera, from Frigate's API.
#[derive(Debug)]
struct CameraImages {
    host: String,
    port: u16,
    camera: String,
}

impl SnapshotSource for CameraImages {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<Image>> {
        Box::pin(async move {
            let request = http::Request::builder()
                .uri(format!("/api/{}/latest.jpg?h=720", self.camera))
                .header("host", format!("{}:{}", self.host, self.port))
                .body(http_body_util::Full::new(moli_net::Body::new()))?;
            let (status, bytes) = moli_net::plain(&self.host, self.port, request).await?;
            if !status.is_success() || !bytes.starts_with(&[0xFF, 0xD8]) {
                anyhow::bail!(moli_i18n::tr!(
                    "pilotes.frigate.aucune_image",
                    status = status
                ));
            }
            Ok(Image {
                content_type: "image/jpeg".into(),
                bytes: bytes.to_vec(),
            })
        })
    }
}

fn default_host() -> String {
    "127.0.0.1".into()
}

fn default_port() -> u16 {
    1883
}

fn default_base() -> String {
    "frigate".into()
}

#[derive(Debug)]
pub struct Frigate {
    config: Config,
}

impl Frigate {
    /// Checks the configuration.
    ///
    /// # Errors
    /// See [`Config::check`].
    pub fn new(config: Config) -> anyhow::Result<Self> {
        config.check()?;
        Ok(Self { config })
    }
}

impl Config {
    /// A go2rtc address is `http://host[:port]`, without credentials: Moli
    /// speaks to go2rtc without any (its API has none by default), and an
    /// address carrying some would end up in error messages. A stream name
    /// is a plain name. The messages never repeat the address.
    ///
    /// # Errors
    /// A go2rtc address Moli cannot use, or a stream name that is not one.
    pub fn check(&self) -> anyhow::Result<()> {
        if let Some(url) = &self.go2rtc {
            let at = moli_net::upnp::Location::parse(url)
                .filter(|at| !at.host.is_empty() && at.path.trim_end_matches('/').is_empty());
            let Some(at) = at else {
                anyhow::bail!("frigate: go2rtc is an address like http://host:1984");
            };
            anyhow::ensure!(
                !at.host.contains('@'),
                "frigate: no credentials in the go2rtc address (keep go2rtc on the home network)"
            );
        }
        if let Some(fingerprint) = self.review.as_ref().and_then(|r| r.cert_sha256.as_deref()) {
            anyhow::ensure!(
                normalize(fingerprint).len() == 64,
                "frigate: review.cert_sha256 is a SHA-256 fingerprint (64 hex digits)"
            );
        }
        for (camera, stream) in &self.streams {
            anyhow::ensure!(
                !stream.trim().is_empty()
                    && stream.len() <= 100
                    && !stream.chars().any(char::is_control),
                "frigate: streams.{camera}: a go2rtc stream name"
            );
        }
        Ok(())
    }
}

impl Driver for Frigate {
    fn kind(&self) -> &'static str {
        "frigate"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn spec(key: &str, label: &str, kind: Kind, write: bool) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        semantic: Semantic::infer(key, None),
        kind,
        access: Access { read: true, write },
        unit: None,
    }
}

fn camera_device(ctx: &DriverCtx, camera: &str) -> Device {
    let mut points = vec![
        spec(
            "motion",
            &moli_i18n::tr!("pilotes.frigate.mouvement"),
            Kind::Binary,
            false,
        ),
        spec(
            "status",
            &moli_i18n::tr!("pilotes.frigate.detection_active"),
            Kind::Text,
            false,
        ),
    ];
    points.extend(OBJECTS.iter().map(|(key, label)| {
        spec(
            key,
            &moli_i18n::tr(label),
            Kind::Numeric {
                min: Some(0.0),
                max: None,
                step: Some(1.0),
            },
            false,
        )
    }));
    points.extend(
        SWITCHES
            .iter()
            .map(|(_, key, label)| spec(key, &moli_i18n::tr(label), Kind::Binary, true)),
    );
    let mut name = camera.replace('_', " ");
    if let Some(first) = name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    Device {
        id: ctx.device_id(camera),
        instance: ctx.instance().clone(),
        native_name: name.into(),
        manufacturer: Some("Frigate".into()),
        model: Some(moli_i18n::tr!("pilotes.frigate.camera").into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

/// The server itself, as a device (its native id cannot be a camera's:
/// Frigate camera names are lowercase words, this one has a dash).
const SERVER: &str = "frigate-server";

fn server_device(ctx: &DriverCtx) -> Device {
    let point = |key: &str, label: &str, kind: Kind, unit: Option<moli_core::Unit>| PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic: Semantic::Other,
    };
    let number = || Kind::Numeric {
        min: Some(0.0),
        max: None,
        step: None,
    };
    let percent = || Some(moli_core::Unit::Percent);
    Device {
        id: ctx.device_id(SERVER),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.frigate.serveur").into(),
        manufacturer: Some("Frigate".into()),
        model: Some("NVR".into()),
        description: Some(moli_i18n::tr!("pilotes.frigate.enregistreur").into()),
        native_room: None,
        members: Vec::new(),
        points: vec![
            point(
                "running",
                &moli_i18n::tr!("pilotes.frigate.en_marche"),
                Kind::Binary,
                None,
            ),
            point(
                "version",
                &moli_i18n::tr!("pilotes.frigate.version"),
                Kind::Text,
                None,
            ),
            point(
                "uptime",
                &moli_i18n::tr!("pilotes.frigate.en_marche_depuis"),
                number(),
                Some(moli_core::Unit::Hour),
            ),
            point(
                "inference_speed",
                &moli_i18n::tr!("pilotes.frigate.vitesse_inference"),
                number(),
                moli_core::Unit::parse("ms"),
            ),
            point(
                "detection_fps",
                &moli_i18n::tr!("pilotes.frigate.detections_par_seconde"),
                number(),
                None,
            ),
            point(
                "gpu_load",
                &moli_i18n::tr!("pilotes.frigate.charge_gpu"),
                number(),
                percent(),
            ),
            point(
                "cpu_load",
                &moli_i18n::tr!("pilotes.frigate.charge_processeur"),
                number(),
                percent(),
            ),
        ],
    }
}

/// The server's figures from a `stats` message.
fn parse_stats(payload: &str) -> Vec<(&'static str, Value)> {
    let Ok(stats) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Vec::new();
    };
    // « 12.5% », « 12.5 » or a number.
    let pct = |v: &serde_json::Value| {
        v.as_f64()
            .or_else(|| v.as_str()?.trim().trim_end_matches('%').trim().parse().ok())
    };
    let round = |v: f64, d: i32| (v * 10f64.powi(d)).round() / 10f64.powi(d);
    let first = |key: &str| {
        stats[key]
            .as_object()
            .and_then(|o| o.values().next().cloned())
    };
    let mut out = Vec::new();
    if let Some(v) = stats["service"]["version"].as_str() {
        out.push(("version", Value::from(v)));
    }
    if let Some(s) = stats["service"]["uptime"].as_f64() {
        out.push(("uptime", Value::Float(round(s / 3600.0, 1))));
    }
    if let Some(ms) = first("detectors").and_then(|d| d["inference_speed"].as_f64()) {
        out.push(("inference_speed", Value::Float(round(ms, 1))));
    }
    if let Some(fps) = stats["detection_fps"].as_f64() {
        out.push(("detection_fps", Value::Float(round(fps, 1))));
    }
    if let Some(gpu) = first("gpu_usages").and_then(|g| pct(&g["gpu"])) {
        out.push(("gpu_load", Value::Float(round(gpu, 1))));
    }
    if let Some(cpu) = pct(&stats["cpu_usages"]["frigate.full_system"]["cpu"]) {
        out.push(("cpu_load", Value::Float(round(cpu, 1))));
    }
    out
}

/// What a Frigate topic (below the base) says about one camera.
#[derive(Debug, PartialEq)]
enum Update<'a> {
    Value(&'a str, &'static str, Value),
}

fn parse<'a>(rest: &'a str, payload: &str) -> Option<Update<'a>> {
    let parts: Vec<&str> = rest.split('/').collect();
    let on = |p: &str| match p {
        "ON" => Some(true),
        "OFF" => Some(false),
        _ => None,
    };
    match parts.as_slice() {
        [camera, "motion"] => Some(Update::Value(camera, "motion", Value::Bool(on(payload)?))),
        [camera, "status", "detect"] => Some(Update::Value(
            camera,
            "status",
            Value::Text(payload.trim().into()),
        )),
        [camera, object] => {
            let key = OBJECTS.iter().find(|(k, _)| k == object).map(|(k, _)| *k)?;
            Some(Update::Value(
                camera,
                key,
                Value::Int(payload.trim().parse().ok()?),
            ))
        }
        [camera, switch, "state"] => {
            let (_, key, _) = SWITCHES.iter().find(|(s, _, _)| s == switch)?;
            Some(Update::Value(camera, key, Value::Bool(on(payload)?)))
        }
        _ => None,
    }
}

/// Topics that carry what the driver uses — never the retained JPEG
/// snapshots (`<cam>/<label>/snapshot`).
fn subscriptions(base: &str) -> [String; 6] {
    [
        format!("{base}/available"),
        format!("{base}/stats"),
        format!("{base}/+/motion"),
        format!("{base}/+/+"),
        format!("{base}/+/+/state"),
        format!("{base}/+/status/detect"),
    ]
}

/// Topics that prove a name is a camera (zones publish counts too).
fn names_a_camera(key: &str) -> bool {
    key == "motion" || key == "detection"
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let mut options = MqttOptions::new(
        format!("moli-{}", ctx.instance()),
        &config.host,
        config.port,
    );
    options.set_keep_alive(Duration::from_secs(30));
    // Headroom for any message on the subscribed topics (`stats` grows
    // with the number of cameras).
    options.set_max_packet_size(256 * 1024, 16 * 1024);
    let (client, mut events) = AsyncClient::new(options, 32);
    let base = config.base_topic.trim_end_matches('/').to_owned();
    let api = moli_net::upnp::Location::parse(&config.api);
    let review = match review_api(config, ctx).await {
        Some(Review::Changed(reason)) => return park(ctx, reason).await,
        Some(Review::Ready(api)) => Some(api),
        None => None,
    };
    let mut certificate = review.as_ref().map(|api| api.certificate_changed());
    let mut cameras: BTreeSet<String> = BTreeSet::new();
    let mut available = true;
    let mut server = false;
    let mut connected = false;
    let mut paused_until: Option<tokio::time::Instant> = None;
    ctx.ready();
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                if connected {
                    reply(&client, &base, ctx, &cameras, command);
                } else {
                    // Queued now, sent hours later: refuse instead.
                    command.reply(Err(moli_i18n::tr!("pilotes.frigate.broker_injoignable")));
                }
            }
            changed = certificate_lost(certificate.as_mut()) => {
                return park(ctx, stopped_on(config, ctx, &changed)).await;
            }
            () = async { tokio::time::sleep_until(paused_until.unwrap_or_else(tokio::time::Instant::now)).await }, if paused_until.is_some() => {
                paused_until = None;
            }
            event = events.poll(), if paused_until.is_none() => match event {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    connected = true;
                    for topic in subscriptions(&base) {
                        if let Err(e) = client.try_subscribe(topic, QoS::AtMostOnce) {
                            tracing::warn!(instance = %ctx.instance(), error = %e, "frigate subscription not sent");
                        }
                    }
                }
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    let Some(rest) = publish.topic.strip_prefix(&format!("{base}/")) else { continue };
                    let payload = String::from_utf8_lossy(&publish.payload);
                    if rest == "available" {
                        available = payload == "online";
                        for camera in &cameras {
                            ctx.set_availability(&ctx.device_id(camera), available);
                        }
                        if server {
                            ctx.set_state(&ctx.device_id(SERVER), "running", Value::Bool(available));
                        }
                        continue;
                    }
                    if rest == "stats" {
                        let id = ctx.device_id(SERVER);
                        if !server {
                            server = true;
                            ctx.upsert_device(server_device(ctx));
                            ctx.set_state(&id, "running", Value::Bool(available));
                        }
                        for (key, value) in parse_stats(&payload) {
                            ctx.set_state(&id, key, value);
                        }
                        continue;
                    }
                    let Some(Update::Value(camera, key, value)) = parse(rest, &payload) else { continue };
                    if !cameras.contains(camera) {
                        if !names_a_camera(key) {
                            continue; // a zone, or not a camera yet
                        }
                        cameras.insert(camera.to_owned());
                        ctx.upsert_device(camera_device(ctx, camera));
                        provide_media(ctx, config, camera, api.as_ref(), review.as_ref());
                        ctx.set_availability(&ctx.device_id(camera), available);
                    }
                    ctx.set_state(&ctx.device_id(camera), key, value);
                }
                Ok(_) => {}
                Err(e) => {
                    connected = false;
                    tracing::debug!(instance = %ctx.instance(), error = %e, "frigate broker unreachable");
                    for camera in &cameras {
                        ctx.set_availability(&ctx.device_id(camera), false);
                    }
                    paused_until = Some(tokio::time::Instant::now() + Duration::from_secs(5));
                }
            },
        }
    }
}

/// Where a camera's images, live video and past come from. With `review`,
/// all three through Frigate's authenticated port: no go2rtc is spoken to.
/// Without it, the go2rtc when one is set (the house's explicit choice),
/// else Frigate's internal API for the images.
fn provide_media(
    ctx: &DriverCtx,
    config: &Config,
    camera: &str,
    api: Option<&moli_net::upnp::Location>,
    review: Option<&std::sync::Arc<review::Api>>,
) {
    let go2rtc = config
        .go2rtc
        .as_deref()
        .and_then(moli_net::upnp::Location::parse);
    let stream_name = || {
        config
            .streams
            .get(camera)
            .cloned()
            .unwrap_or_else(|| camera.to_owned())
    };
    if let Some(api) = review {
        let id = ctx.device_id(camera);
        ctx.provide_review(
            &id,
            std::sync::Arc::new(review::Camera {
                api: std::sync::Arc::clone(api),
                name: camera.to_owned(),
            }),
        );
        let seen = std::sync::Arc::new(review::Seen {
            api: std::sync::Arc::clone(api),
            camera: camera.to_owned(),
        });
        ctx.provide_snapshots(&id, seen.clone());
        ctx.provide_live(&id, seen);
        return;
    }
    if let Some(g) = go2rtc {
        let stream = std::sync::Arc::new(go2rtc::Stream {
            host: g.host.clone(),
            port: g.port,
            name: stream_name(),
        });
        ctx.provide_snapshots(&ctx.device_id(camera), stream.clone());
        ctx.provide_live(&ctx.device_id(camera), stream);
    } else if let Some(api) = api {
        ctx.provide_snapshots(
            &ctx.device_id(camera),
            std::sync::Arc::new(CameraImages {
                host: api.host.clone(),
                port: api.port,
                camera: camera.to_owned(),
            }),
        );
    }
}

fn reply(
    client: &AsyncClient,
    base: &str,
    ctx: &DriverCtx,
    cameras: &BTreeSet<String>,
    command: CommandRequest,
) {
    let camera = cameras
        .iter()
        .find(|c| ctx.device_id(c) == command.device.id);
    let switch = SWITCHES.iter().find(|(_, key, _)| *key == &*command.key);
    let result = match (camera, switch, &command.value) {
        (Some(camera), Some((topic, _, _)), Value::Bool(on)) => client
            .try_publish(
                format!("{base}/{camera}/{topic}/set"),
                QoS::AtLeastOnce,
                false,
                if *on { "ON" } else { "OFF" },
            )
            .map_err(|e| e.to_string()),
        _ => Err(moli_i18n::tr!(
            "pilotes.frigate.ne_se_regle_pas",
            point = command.key
        )),
    };
    command.reply(result);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_become_points() {
        assert_eq!(
            parse("jardin/motion", "ON"),
            Some(Update::Value("jardin", "motion", Value::Bool(true)))
        );
        assert_eq!(
            parse("jardin/person", "2"),
            Some(Update::Value("jardin", "person", Value::Int(2)))
        );
        assert_eq!(
            parse("jardin/detect/state", "OFF"),
            Some(Update::Value("jardin", "detection", Value::Bool(false)))
        );
        assert_eq!(
            parse("jardin/status/detect", "offline"),
            Some(Update::Value(
                "jardin",
                "status",
                Value::Text("offline".into())
            ))
        );
        assert_eq!(parse("jardin/motion_threshold/state", "30"), None);
        assert_eq!(parse("jardin/unicorn", "1"), None);
        assert_eq!(parse("stats", "{}"), None);
    }

    #[test]
    fn stats_become_the_servers_figures() {
        let stats = r#"{
            "service": {"version": "0.17.1-416a9b7", "uptime": 90000, "latest_version": "0.17.1"},
            "detectors": {"ov": {"inference_speed": 12.345, "pid": 400}},
            "detection_fps": 3.25,
            "gpu_usages": {"intel-vaapi": {"gpu": "7.5%", "mem": "-%"}},
            "cpu_usages": {"frigate.full_system": {"cpu": "14.0", "mem": "22.1"}},
            "cameras": {"jardin": {"camera_fps": 5.0}}
        }"#;
        let v: std::collections::HashMap<_, _> = parse_stats(stats).into_iter().collect();
        assert_eq!(v["version"], Value::from("0.17.1-416a9b7"));
        assert_eq!(v["uptime"], Value::Float(25.0));
        assert_eq!(v["inference_speed"], Value::Float(12.3));
        assert_eq!(v["detection_fps"], Value::Float(3.3));
        assert_eq!(v["gpu_load"], Value::Float(7.5));
        assert_eq!(v["cpu_load"], Value::Float(14.0));
        assert!(parse_stats("not json").is_empty());
    }

    #[test]
    fn a_go2rtc_address_carries_no_credentials() {
        let config = |go2rtc: &str| -> Config {
            serde_json::from_value(serde_json::json!({ "go2rtc": go2rtc })).unwrap()
        };
        assert!(config("http://nvr.example:1984").check().is_ok());
        assert!(config("http://nvr.example:1984/").check().is_ok());
        let error = config("http://admin:secret@example.com:1984")
            .check()
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret"), "{error}");
        for bad in [
            "nvr.example:1984",
            "https://nvr.example",
            "http://nvr.example:1984/api",
        ] {
            let error = config(bad).check().unwrap_err().to_string();
            assert!(!error.contains("nvr.example"), "{error}");
        }
        let named: Config =
            serde_json::from_value(serde_json::json!({ "streams": { "jardin": "" } })).unwrap();
        assert!(named.check().is_err());
    }

    fn review(cert: Option<&str>) -> ReviewConfig {
        ReviewConfig {
            host: "nvr.example".into(),
            port: 8971,
            user: "moli".into(),
            cert_sha256: cert.map(str::to_owned),
        }
    }

    #[test]
    fn a_changed_certificate_says_how_to_accept_it_never_by_itself() {
        let instance = moli_core::InstanceId::from("frigate");
        let pinned = "AB:CD:".repeat(16);
        // Pinned on first contact: forgotten from the machine Moli runs on.
        let said = certificate_changed(&review(None), &instance, &pinned, Some("ff00"));
        assert!(
            said.contains("secrets forget frigate certificate"),
            "{said}"
        );
        assert!(said.contains("abcdabcdabcdabcd"), "{said}");
        // Written in the file: the file changes, nothing is forgotten.
        let written = "ab".repeat(32);
        let said = certificate_changed(&review(Some(&written)), &instance, &written, None);
        assert!(
            said.contains("cert_sha256") && !said.contains("secrets forget"),
            "{said}"
        );
    }

    #[test]
    fn a_written_fingerprint_is_a_sha256() {
        let config = |cert: &str| Config {
            review: Some(review(Some(cert))),
            ..serde_json::from_value(serde_json::json!({})).unwrap()
        };
        assert!(config(&"AB:".repeat(32)).check().is_ok());
        assert!(config("abcd").check().is_err());
    }
}
