//! Frigate NVR cameras, through the MQTT topics Frigate publishes
//! (`frigate/<camera>/…`): motion, objects in view, and its switches
//! (detection, recording, snapshots); and the server itself (`stats`:
//! version, uptime, inference speed, detection rate, GPU and CPU load). No
//! HTTP, no credentials: the same broker Zigbee2MQTT uses.

use std::collections::BTreeSet;
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
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
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
                        if let Some(api) = &api {
                            ctx.provide_snapshots(
                                &ctx.device_id(camera),
                                std::sync::Arc::new(CameraImages {
                                    host: api.host.clone(),
                                    port: api.port,
                                    camera: camera.to_owned(),
                                }),
                            );
                        }
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
}
