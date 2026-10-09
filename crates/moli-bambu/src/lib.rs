//! Bambu Lab printers (A1, P1, X1) over their local MQTT (TLS 8883, user
//! `bblp`, the access code shown on the printer). Writes: the chamber
//! light, and `control` (pause, resume, stop), which the guard holds for a
//! human when an agent asks.
//!
//! The printer's certificate is pinned on first contact, before the access
//! code is sent. Reports are deltas: they are merged into the last known
//! state, and a full report (`pushall`) is asked for every few minutes.

mod ams;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_core::{Access, Device, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::{PinnedHttps, PinnedTls};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, TlsConfiguration, Transport};
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

const ACCESS_CODE: &str = "access_code";
const CERT: &str = "cert";
const PORT: u16 = 8883;
const PUSHALL_EVERY: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    /// Printed on the printer and in Bambu Studio.
    pub serial: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug)]
pub struct Bambu {
    config: Config,
}

impl Bambu {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Bambu {
    fn kind(&self) -> &'static str {
        "bambu"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// Moli point ← report field, and how to read it.
#[derive(Clone, Copy)]
enum Read {
    Text,
    Lower,
    Number,
    Integer,
    Dbm,
}

const FIELDS: [(&str, &str, &str, Read); 12] = [
    ("state", "gcode_state", "pilotes.bambu.etat", Read::Lower),
    (
        "progress",
        "mc_percent",
        "pilotes.bambu.progression",
        Read::Integer,
    ),
    (
        "remaining",
        "mc_remaining_time",
        "pilotes.bambu.temps_restant",
        Read::Integer,
    ),
    (
        "nozzle_temperature",
        "nozzle_temper",
        "pilotes.bambu.buse",
        Read::Number,
    ),
    (
        "nozzle_target",
        "nozzle_target_temper",
        "pilotes.bambu.buse_consigne",
        Read::Number,
    ),
    (
        "bed_temperature",
        "bed_temper",
        "pilotes.bambu.plateau",
        Read::Number,
    ),
    (
        "bed_target",
        "bed_target_temper",
        "pilotes.bambu.plateau_consigne",
        Read::Number,
    ),
    (
        "job",
        "subtask_name",
        "pilotes.bambu.impression",
        Read::Text,
    ),
    ("layer", "layer_num", "pilotes.bambu.couche", Read::Integer),
    (
        "total_layers",
        "total_layer_num",
        "pilotes.bambu.couches",
        Read::Integer,
    ),
    (
        "error",
        "print_error",
        "pilotes.bambu.erreur",
        Read::Integer,
    ),
    (
        "wifi_signal",
        "wifi_signal",
        "pilotes.bambu.wifi",
        Read::Dbm,
    ),
];

fn spec(key: &str, label: &str, kind: Kind, write: bool, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        semantic: Semantic::infer(key, unit.as_ref()),
        kind,
        access: Access { read: true, write },
        unit,
    }
}

fn numeric() -> Kind {
    Kind::Numeric {
        min: None,
        max: None,
        step: None,
    }
}

/// What the printer is called: its configured name, else a generic one.
fn printer_name(config: &Config) -> String {
    config
        .name
        .clone()
        .unwrap_or_else(|| moli_i18n::tr!("pilotes.bambu.imprimante_3d"))
}

fn device(ctx: &DriverCtx, config: &Config) -> Device {
    let unit = |key: &str| match key {
        "progress" => Some(Unit::Percent),
        "remaining" => Some(Unit::Minute),
        k if k.contains("temperature") || k.contains("target") => Some(Unit::Celsius),
        "wifi_signal" => Unit::parse("dBm"),
        _ => None,
    };
    let mut points: Vec<PointSpec> = FIELDS
        .iter()
        .map(|(key, _, label_key, read)| {
            let kind = match read {
                Read::Text | Read::Lower => Kind::Text,
                _ => numeric(),
            };
            spec(key, &moli_i18n::tr(label_key), kind, false, unit(key))
        })
        .collect();
    points.push(spec(
        "printing",
        &moli_i18n::tr!("pilotes.bambu.en_impression"),
        Kind::Binary,
        false,
        None,
    ));
    points.push(spec(
        "light",
        &moli_i18n::tr!("pilotes.bambu.lumiere"),
        Kind::Binary,
        true,
        None,
    ));
    let mut control = spec(
        "control",
        &moli_i18n::tr!("pilotes.bambu.commande"),
        Kind::Enum {
            values: CONTROLS.iter().map(|c| (*c).into()).collect(),
        },
        true,
        None,
    );
    control.semantic = Semantic::Control;
    points.push(control);
    Device {
        id: ctx.device_id(&config.serial),
        instance: ctx.instance().clone(),
        native_name: printer_name(config).into(),
        manufacturer: Some("Bambu Lab".into()),
        model: None,
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

/// The Moli values a merged report holds.
fn values(report: &Map<String, Json>) -> Vec<(&'static str, Value)> {
    let mut out: Vec<(&'static str, Value)> = FIELDS
        .iter()
        .filter_map(|(key, field, _, read)| {
            let raw = report.get(*field)?;
            let value = match (read, raw) {
                (Read::Text, Json::String(s)) => Value::Text(s.trim().into()),
                (Read::Lower, Json::String(s)) => Value::Text(s.trim().to_lowercase().into()),
                (Read::Number, v) => Value::Float((v.as_f64()? * 10.0).round() / 10.0),
                (Read::Integer, v) => Value::Int(v.as_i64().or_else(|| v.as_str()?.parse().ok())?),
                (Read::Dbm, Json::String(s)) => {
                    Value::Int(s.trim_end_matches("dBm").trim().parse().ok()?)
                }
                _ => return None,
            };
            Some((*key, value))
        })
        .collect();
    if let Some(Json::String(state)) = report.get("gcode_state") {
        out.push((
            "printing",
            Value::Bool(matches!(state.as_str(), "RUNNING" | "PREPARE")),
        ));
    }
    let light = report
        .get("lights_report")
        .and_then(Json::as_array)
        .and_then(|lights| lights.iter().find(|l| l["node"] == "chamber_light"))
        .and_then(|l| l["mode"].as_str());
    if let Some(mode) = light {
        out.push(("light", Value::Bool(mode == "on")));
    }
    out
}

async fn first_pin(config: &Config, ctx: &DriverCtx) -> anyhow::Result<String> {
    if !ctx.can_store_secrets() {
        bail!("cannot pin the printer's certificate: the secret store is unavailable");
    }
    let probe = PinnedHttps::new(&config.host, PORT, None)?;
    probe.handshake().await.context("first contact")?;
    let seen = probe.seen_fingerprint().context("no certificate seen")?;
    ctx.store_secret(CERT, &seen).await?;
    tracing::info!(instance = %ctx.instance(), "printer certificate pinned");
    Ok(seen)
}

/// Waits `delay` while refusing orders. `false`: the driver is stopping.
async fn idle(ctx: &mut DriverCtx, delay: Duration) -> bool {
    let wake = tokio::time::sleep(delay);
    tokio::pin!(wake);
    loop {
        tokio::select! {
            () = &mut wake => return true,
            command = ctx.next_command() => match command {
                Some(command) => command.reply(Err(moli_i18n::tr!("pilotes.bambu.injoignable"))),
                None => return false,
            },
        }
    }
}

/// MQTT options with the access code. `MqttOptions`'s `Debug` prints the
/// password: never log these.
fn mqtt_options(ctx: &DriverCtx, config: &Config, code: String, tls: &PinnedTls) -> MqttOptions {
    let mut options = MqttOptions::new(format!("moli-{}", ctx.instance()), &config.host, PORT);
    options.set_credentials("bblp", code);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_max_packet_size(256 * 1024, 16 * 1024);
    options.set_transport(Transport::tls_with_config(TlsConfiguration::Rustls(
        Arc::clone(&tls.config),
    )));
    options
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let Some(code) = ctx.secret(ACCESS_CODE) else {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.bambu.code_manquant",
            instance = ctx.instance(),
            key = ACCESS_CODE
        ));
        ctx.cancelled().await;
        return Ok(());
    };
    let id = ctx.device_id(&config.serial);
    ctx.upsert_device(device(ctx, config));
    ctx.set_availability(&id, false);
    ctx.ready();
    // The certificate is pinned at first contact: wait for the printer
    // (often switched off on a smart plug) instead of failing.
    let pin = loop {
        if let Some(pin) = ctx.secret(CERT) {
            break pin;
        }
        match first_pin(config, ctx).await {
            Ok(pin) => break pin,
            Err(e) => {
                tracing::debug!(instance = %ctx.instance(), error = %format!("{e:#}"), "printer not reachable yet");
                if !idle(ctx, Duration::from_secs(60)).await {
                    return Ok(());
                }
            }
        }
    };
    let tls = PinnedTls::new(Some(pin))?;
    let (client, mut events) = AsyncClient::new(mqtt_options(ctx, config, code, &tls), 16);
    let report_topic = format!("device/{}/report", config.serial);
    let request_topic = format!("device/{}/request", config.serial);
    let pushall = json!({"pushing": {"sequence_id": "0", "command": "pushall"}}).to_string();
    let mut state = Map::new();
    // AMS units and the external spool, declared when first reported.
    let mut filament: HashSet<moli_core::DeviceId> = HashSet::new();
    let printer = printer_name(config);
    let mut refresh = tokio::time::interval(PUSHALL_EVERY);
    let mut connected = false;
    let mut last_error: Option<String> = None;
    let mut paused_until: Option<tokio::time::Instant> = None;
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                if connected {
                    reply(&client, &request_topic, command);
                } else {
                    // Queued now, sent when the printer comes back: refuse.
                    command.reply(Err(moli_i18n::tr!("pilotes.bambu.injoignable")));
                }
            }
            _ = refresh.tick(), if connected => {
                let _ = client.try_publish(&request_topic, QoS::AtMostOnce, false, pushall.clone());
            }
            () = async { tokio::time::sleep_until(paused_until.unwrap_or_else(tokio::time::Instant::now)).await }, if paused_until.is_some() => {
                paused_until = None;
            }
            event = events.poll(), if paused_until.is_none() => match event {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    connected = true;
                    last_error = None;
                    if let Err(e) = client.try_subscribe(&report_topic, QoS::AtMostOnce) {
                        tracing::warn!(instance = %ctx.instance(), error = %e, "printer subscription not sent");
                    }
                    let _ = client.try_publish(&request_topic, QoS::AtMostOnce, false, pushall.clone());
                    online(ctx, &id, &filament, true);
                }
                Ok(Event::Incoming(Packet::Publish(publish))) if publish.topic == report_topic => {
                    let Ok(Json::Object(report)) = serde_json::from_slice::<Json>(&publish.payload) else { continue };
                    if let Some(Json::Object(print)) = report.get("print") {
                        on_report(ctx, &config.serial, &printer, &id, &mut state, &mut filament, print);
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    connected = false;
                    online(ctx, &id, &filament, false);
                    let text = format!("{e:#}");
                    if text.contains(moli_net::PIN_MISMATCH) {
                        ctx.wait_for(moli_i18n::tr!(
                            "pilotes.bambu.certificat_change",
                            instance = ctx.instance(),
                            key = CERT
                        ));
                        ctx.cancelled().await;
                        return Ok(());
                    }
                    // A refused access code is not « switched off »: the
                    // person reads the code on the printer and gives it again.
                    if refused_code(&text) {
                        ctx.wait_for(moli_i18n::tr!(
                            "pilotes.bambu.code_refuse",
                            instance = ctx.instance(),
                            key = ACCESS_CODE
                        ));
                        ctx.cancelled().await;
                        return Ok(());
                    }
                    // Printer off: rumqttc reconnects on the next poll. Each
                    // new reason is said once (a printer switched off says
                    // the same thing every 30 s).
                    if last_error.as_deref() == Some(text.as_str()) {
                        tracing::debug!(instance = %ctx.instance(), error = %text, "printer unreachable");
                    } else {
                        tracing::warn!(instance = %ctx.instance(), error = %text, "printer unreachable");
                        last_error = Some(text);
                    }
                    paused_until = Some(tokio::time::Instant::now() + Duration::from_secs(30));
                }
            },
        }
    }
}

/// The printer said no to the access code (MQTT `CONNACK` 4 or 5).
fn refused_code(error: &str) -> bool {
    error.contains("BadUserNamePassword") || error.contains("NotAuthorized")
}

/// One report: merged into what is known, then read into the printer's
/// points and the filament's (AMS units and spool, declared when first seen).
fn on_report(
    ctx: &DriverCtx,
    serial: &str,
    printer: &str,
    id: &moli_core::DeviceId,
    state: &mut Map<String, Json>,
    filament: &mut HashSet<moli_core::DeviceId>,
    print: &Map<String, Json>,
) {
    ams::merge(state, print);
    for (key, value) in values(state) {
        ctx.set_state(id, key, value);
    }
    let read = ams::read(ctx, serial, printer, state);
    for device in read.devices {
        if filament.insert(device.id.clone()) {
            let fid = device.id.clone();
            ctx.upsert_device(device);
            ctx.set_availability(&fid, true);
        }
    }
    for (fid, key, value) in read.values {
        ctx.set_state(&fid, key, value);
    }
}

/// The printer and its filament devices come and go together.
fn online(
    ctx: &DriverCtx,
    id: &moli_core::DeviceId,
    filament: &HashSet<moli_core::DeviceId>,
    up: bool,
) {
    ctx.set_availability(id, up);
    for f in filament {
        ctx.set_availability(f, up);
    }
}

/// What can be asked of a print (the printer's own words: `stop` cancels).
const CONTROLS: [&str; 3] = ["pause", "resume", "stop"];

/// The request a print order becomes (as ha-bambulab sends it).
fn print_order(order: &str) -> Option<Json> {
    CONTROLS
        .contains(&order)
        .then(|| json!({"print": {"sequence_id": "0", "command": order, "param": ""}}))
}

fn reply(client: &AsyncClient, topic: &str, command: CommandRequest) {
    let result = match (&*command.key, &command.value) {
        ("control", Value::Text(order)) => match print_order(order) {
            Some(payload) => client
                .try_publish(topic, QoS::AtMostOnce, false, payload.to_string())
                .map_err(|e| e.to_string()),
            None => Err(moli_i18n::tr!("pilotes.bambu.ordre_inconnu", order = order)),
        },
        ("light", Value::Bool(on)) => {
            let payload = json!({"system": {"sequence_id": "0", "command": "ledctrl",
                "led_node": "chamber_light", "led_mode": if *on { "on" } else { "off" },
                "led_on_time": 500, "led_off_time": 500, "loop_times": 0, "interval_time": 0}});
            client
                .try_publish(topic, QoS::AtMostOnce, false, payload.to_string())
                .map_err(|e| e.to_string())
        }
        (key, _) => Err(moli_i18n::tr!("pilotes.bambu.lecture_seule", point = key)),
    };
    command.reply(result);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_access_code_is_told_apart() {
        for (error, refused) in [
            (
                "Connection refused, return code: `BadUserNamePassword`",
                true,
            ),
            ("Connection refused, return code: `NotAuthorized`", true),
            ("I/O: Connection refused (os error 111)", false),
            ("Timeout", false),
        ] {
            assert_eq!(refused_code(error), refused, "{error}");
        }
    }

    #[test]
    fn a_merged_report_reads_as_points() {
        let report: Map<String, Json> = serde_json::from_value(json!({
            "gcode_state": "RUNNING", "mc_percent": 42, "mc_remaining_time": 37,
            "nozzle_temper": 219.94, "bed_temper": 65.0, "subtask_name": "Benchy ",
            "layer_num": 12, "total_layer_num": 150, "print_error": 0, "wifi_signal": "-58dBm",
            "lights_report": [{"node": "chamber_light", "mode": "on"}]
        }))
        .unwrap();
        let v: std::collections::HashMap<_, _> = values(&report).into_iter().collect();
        assert_eq!(v["state"], Value::Text("running".into()));
        assert_eq!(v["printing"], Value::Bool(true));
        assert_eq!(v["progress"], Value::Int(42));
        assert_eq!(v["nozzle_temperature"], Value::Float(219.9));
        assert_eq!(v["job"], Value::Text("Benchy".into()));
        assert_eq!(v["wifi_signal"], Value::Int(-58));
        assert_eq!(v["light"], Value::Bool(true));
        assert!(!v.contains_key("nozzle_target"));
    }

    #[test]
    fn print_orders_are_the_printers_words() {
        assert_eq!(
            print_order("pause").unwrap(),
            json!({"print": {"sequence_id": "0", "command": "pause", "param": ""}})
        );
        assert!(print_order("start").is_none());
    }
}
