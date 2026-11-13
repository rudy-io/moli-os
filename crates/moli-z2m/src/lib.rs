//! Zigbee2MQTT driver.
//!
//! Subscribes to `{base}/#`, maps `bridge/devices` to Moli devices, state
//! messages to values and availability messages to online flags, and turns
//! commands into `{friendly_name}/set` publications. Colour lamps also get
//! a flame the driver plays itself ([`flame`]).

mod catalog;
mod flame;

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use anyhow::Context as _;
use flame::{FLAME, Flame, NO_EFFECT};
use moli_core::{DeviceId, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, Publish, QoS};
use serde::Deserialize;
use tokio::time::Instant;

pub use catalog::{Catalog, Entry};

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_base_topic")]
    pub base_topic: String,
    #[serde(default)]
    pub username: Option<String>,
    /// Name of the environment variable holding the password (never the
    /// password itself).
    #[serde(default)]
    pub password_env: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    /// Relays wired as triggers (a gate's contacts, a garage door): point ids
    /// such as `z2m:0xa4c138aa581f6da3/state_l1`. Each shows as an « Impulsion »
    /// order instead of an on/off state that falls back to off.
    #[serde(default)]
    pub impulse_points: Vec<String>,
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    1883
}
fn default_base_topic() -> String {
    "zigbee2mqtt".into()
}

#[derive(Debug)]
pub struct Z2m {
    config: Config,
}

impl Z2m {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Z2m {
    fn kind(&self) -> &'static str {
        "z2m"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let client_id = config
        .client_id
        .clone()
        .unwrap_or_else(|| format!("moli-os-{}", ctx.instance()));
    let mut options = MqttOptions::new(client_id, &config.host, config.port);
    options.set_keep_alive(Duration::from_secs(30));
    // `bridge/devices` grows with the network; the default 10 KiB is too small.
    options.set_max_packet_size(4 * 1024 * 1024, 256 * 1024);
    if let Some(username) = &config.username {
        let password = match &config.password_env {
            Some(var) => std::env::var(var).with_context(|| format!("env var {var} not set"))?,
            None => String::new(),
        };
        options.set_credentials(username, password);
    }

    let (client, mut events) = AsyncClient::new(options, 64);
    let base = config.base_topic.trim_end_matches('/');
    let mut catalog = Catalog::default();
    // Devices already queued for a read during this run, and the queue
    // itself: reads are paced so they never crowd the MQTT channel or the
    // Zigbee network (and commands always find room).
    let mut read_done = HashSet::new();
    let mut reads: VecDeque<(String, String)> = VecDeque::new();
    let mut pacer = tokio::time::interval(READ_PACE);
    pacer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Until the broker accepts us, a command would only wait in the client's
    // queue, acknowledged, and be lost (or sent much later): refuse it.
    // Any poll error ends the run, so the flag never outlives a connection.
    let mut connected = false;
    // The lamps playing the flame, and when each takes its next fade.
    let mut flames: Flames = HashMap::new();
    let mut flicker = tokio::time::interval(FLAME_TICK);
    flicker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            event = events.poll() => match event.context("mqtt connection")? {
                Event::Incoming(Packet::ConnAck(_)) => {
                    connected = true;
                    client.try_subscribe(format!("{base}/#"), QoS::AtLeastOnce)?;
                    tracing::info!(instance = %ctx.instance(), "connected to mqtt");
                    ctx.ready();
                }
                Event::Incoming(Packet::Publish(publish)) => {
                    let reloaded = on_publish(ctx, config, base, &mut catalog, &mut flames, &publish);
                    if reloaded {
                        queue_reads(base, &catalog, &mut read_done, &mut reads);
                        resume_flames(ctx, &catalog, &mut flames);
                    }
                }
                _ => {}
            },
            _ = pacer.tick(), if !reads.is_empty() => {
                if let Some((topic, payload)) = reads.pop_front()
                    && client.try_publish(&topic, QoS::AtMostOnce, false, payload.clone()).is_err()
                {
                    reads.push_front((topic, payload));
                }
            },
            _ = flicker.tick(), if connected && !flames.is_empty() => {
                play_flames(&client, base, &catalog, &mut flames);
            },
            command = ctx.next_command() => match command {
                Some(command) if !connected => command.reply(Err(UNREACHABLE.into())),
                Some(command) => on_command(ctx, &client, base, &catalog, &mut flames, command),
                None => return Ok(()),
            },
        }
    }
}

/// Handles one message. Returns `true` when the catalog was reloaded.
fn on_publish(
    ctx: &DriverCtx,
    config: &Config,
    base: &str,
    catalog: &mut Catalog,
    flames: &mut Flames,
    publish: &Publish,
) -> bool {
    let Some(rest) = publish
        .topic
        .strip_prefix(base)
        .and_then(|t| t.strip_prefix('/'))
    else {
        return false;
    };
    if rest == "bridge/devices" {
        return match Catalog::with_impulses(
            ctx.instance(),
            &publish.payload,
            &config.impulse_points,
        ) {
            Ok(fresh) => {
                apply_catalog(ctx, catalog, fresh);
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "invalid bridge/devices payload");
                false
            }
        };
    }
    if rest.starts_with("bridge/") || is_request(rest) {
        return false;
    }
    if let Some(name) = rest.strip_suffix("/availability") {
        if let Some(entry) = catalog.by_name(name) {
            let online = parse_online(&publish.payload);
            ctx.set_availability(&entry.device.id, online);
            // Cut at the wall: nothing left to animate.
            if !online && flames.remove(&entry.device.id).is_some() {
                ctx.set_state(&entry.device.id, "effect", Value::from(NO_EFFECT));
            }
        }
        return false;
    }
    let Some(entry) = catalog.by_name(rest) else {
        return false;
    };
    if let Ok(serde_json::Value::Object(payload)) = serde_json::from_slice(&publish.payload) {
        let id = &entry.device.id;
        let playing = flames.contains_key(id);
        for (key, value) in entry.decode(&payload) {
            // The lamp knows nothing of the flame: what plays is ours to say.
            if entry.flame && &*key == "effect" {
                continue;
            }
            // The fades of the flame are not what the person chose: the level
            // and the colour on record stay theirs.
            if playing && is_look(&key) {
                continue;
            }
            // Switched off some other way (its own button): the flame is over.
            if playing && &*key == "state" && value == Value::Bool(false) {
                flames.remove(id);
                ctx.set_state(id, "effect", Value::from(NO_EFFECT));
            }
            // A press is an occurrence: told every time, even the same twice.
            // Not a retained one, replayed by the broker when Moli connects:
            // an old press would set off an automation (as with Hue).
            if entry.device.point(&key).is_some_and(catalog::is_press) {
                if !publish.retain {
                    ctx.pulse_state(id, &key, value);
                }
                continue;
            }
            ctx.set_state(id, &key, value);
        }
    } else {
        tracing::debug!(topic = %publish.topic, "non-object state payload ignored");
    }
    false
}

/// Z2M does not replay known states to new subscribers. Listening
/// (mains-powered) devices are asked once per run; sleepy ones are covered
/// by the hub's state cache until they report.
fn queue_reads(
    base: &str,
    catalog: &Catalog,
    done: &mut HashSet<String>,
    reads: &mut VecDeque<(String, String)>,
) {
    for entry in catalog.entries() {
        if let Some(request) = entry.read_request()
            && done.insert(entry.ieee.clone())
        {
            let topic = format!("{base}/{}/get", entry.friendly_name);
            reads.push_back((topic, request.to_string()));
        }
    }
}

/// One device read every half second.
const READ_PACE: Duration = Duration::from_millis(500);

/// Answer to a command received while the broker is not connected.
const UNREACHABLE: &str = "broker injoignable";

/// Our own (and others') requests to Z2M echo back through `#`.
fn is_request(rest: &str) -> bool {
    rest.ends_with("/set")
        || rest.ends_with("/get")
        || rest.contains("/set/")
        || rest.contains("/get/")
}

/// Availability is `{"state":"online"}` (Z2M ≥ 1.29) or plain `online`.
fn parse_online(payload: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Availability {
        state: String,
    }
    serde_json::from_slice::<Availability>(payload).map_or_else(
        |_| String::from_utf8_lossy(payload).trim().to_owned(),
        |a| a.state,
    ) == "online"
}

fn apply_catalog(ctx: &DriverCtx, current: &mut Catalog, fresh: Catalog) {
    let keep: Vec<DeviceId> = fresh.entries().map(|e| e.device.id.clone()).collect();
    for id in ctx.devices() {
        if !keep.contains(&id) {
            ctx.remove_device(&id);
        }
    }
    for entry in fresh.entries() {
        ctx.upsert_device(entry.device.clone());
    }
    tracing::info!(devices = fresh.len(), "zigbee catalog loaded");
    *current = fresh;
}

/// The lamps playing the flame: the flame, and when its next fade is due.
type Flames = HashMap<DeviceId, (Flame, Instant)>;

/// How often due flames are looked for (a fade lasts one to two seconds).
const FLAME_TICK: Duration = Duration::from_millis(250);

/// The lamp's own colour loop (Zigbee), and the order that ends it.
const COLORLOOP: &str = "colorloop";
const STOP_COLORLOOP: &str = "stop_colorloop";

/// The level a flame moves around when the lamp never said its own.
const FLAME_BASE: f64 = 30.0;

/// What a flame changes on a lamp: its level and its colour.
fn is_look(key: &str) -> bool {
    key == "brightness" || key == "color_temp" || key == "color" || key.starts_with("color.")
}

/// A seed that differs from one lamp and one start to the next.
fn flame_seed(id: &DeviceId) -> u64 {
    use std::hash::{BuildHasher as _, RandomState};
    RandomState::new().hash_one(id)
}

/// Sends its next fade to every lamp whose time has come.
fn play_flames(client: &AsyncClient, base: &str, catalog: &Catalog, flames: &mut Flames) {
    let now = Instant::now();
    flames.retain(|id, (flame, due)| {
        let Some(entry) = catalog.by_id(id).filter(|e| e.flame) else {
            return false;
        };
        if *due <= now {
            let fade = flame.frame();
            let topic = format!("{base}/{}/set", entry.friendly_name);
            let payload = entry.flame_frame(&fade, false).to_string();
            // A full channel: this fade is skipped, never queued behind orders.
            let _ = client.try_publish(topic, QoS::AtMostOnce, false, payload);
            *due = now + fade.period();
        }
        true
    });
}

/// After a restart (or a new catalog), the lamps on record as playing the
/// flame play it again; one that is off by now is on record as stopped.
fn resume_flames(ctx: &DriverCtx, catalog: &Catalog, flames: &mut Flames) {
    for entry in catalog.entries().filter(|e| e.flame) {
        let id = &entry.device.id;
        if flames.contains_key(id) || ctx.current(id, "effect") != Some(Value::from(FLAME)) {
            continue;
        }
        if ctx.current(id, "state") == Some(Value::Bool(true)) {
            let level = ctx.current(id, "brightness").and_then(|v| v.as_f64());
            let flame = Flame::new(level.unwrap_or(FLAME_BASE), flame_seed(id));
            flames.insert(id.clone(), (flame, Instant::now()));
        } else {
            ctx.set_state(id, "effect", Value::from(NO_EFFECT));
        }
    }
}

/// An order to a lamp that can play the flame. Returns the payload to send
/// to the lamp, or `None` when the order is fully handled here.
fn flame_order(
    ctx: &DriverCtx,
    entry: &Entry,
    flames: &mut Flames,
    key: &str,
    value: &Value,
) -> Option<serde_json::Value> {
    let id = &entry.device.id;
    let text = match value {
        Value::Text(t) => Some(&**t),
        _ => None,
    };
    match (key, text) {
        ("effect", Some(FLAME)) => {
            if flames.contains_key(id) {
                return None;
            }
            let level = ctx.current(id, "brightness").and_then(|v| v.as_f64());
            let mut flame = Flame::new(level.unwrap_or(FLAME_BASE), flame_seed(id));
            let first = flame.frame();
            flames.insert(id.clone(), (flame, Instant::now() + first.period()));
            ctx.set_state(id, "effect", Value::from(FLAME));
            Some(entry.flame_frame(&first, true))
        }
        ("effect", Some(NO_EFFECT)) => {
            let looping = ctx.current(id, "effect") == Some(Value::from(COLORLOOP));
            ctx.set_state(id, "effect", Value::from(NO_EFFECT));
            if looping {
                return Some(entry.encode("effect", &Value::from(STOP_COLORLOOP)));
            }
            flames
                .remove(id)
                .map(|(flame, _)| entry.flame_rest(flame.base()))
        }
        // The slider sets the level the flame moves around.
        ("brightness", _) if flames.contains_key(id) => {
            if let Some(((flame, due), level)) = flames.get_mut(id).zip(value.as_f64()) {
                flame.set_base(level);
                *due = Instant::now();
            }
            ctx.set_state(id, "brightness", value.clone());
            None
        }
        _ => {
            // Another look, another animation or switched off: the flame is over.
            let off = key == "state" && *value == Value::Bool(false);
            if (off || is_look(key) || key == "effect") && flames.remove(id).is_some() {
                ctx.set_state(id, "effect", Value::from(NO_EFFECT));
            }
            // The lamp's own colour loop goes on until told to stop, and the
            // lamp never says it plays it: the driver keeps that on record,
            // so that whoever sets a look next knows to stop it first.
            match (key, text) {
                ("effect", Some(COLORLOOP)) => {
                    ctx.set_state(id, "effect", Value::from(COLORLOOP));
                }
                ("effect", Some(STOP_COLORLOOP)) => {
                    ctx.set_state(id, "effect", Value::from(NO_EFFECT));
                }
                _ => {}
            }
            // A level is on record at once: a flame started right after it
            // (a scene sets the level, then the flame) moves around it, not
            // around the level before.
            if key == "brightness" {
                ctx.set_state(id, "brightness", value.clone());
            }
            Some(entry.encode(key, value))
        }
    }
}

fn on_command(
    ctx: &DriverCtx,
    client: &AsyncClient,
    base: &str,
    catalog: &Catalog,
    flames: &mut Flames,
    command: CommandRequest,
) {
    let Some(entry) = catalog.by_id(&command.device.id) else {
        command.reply(Err("device no longer known to zigbee2mqtt".into()));
        return;
    };
    let payload = if entry.flame {
        match flame_order(ctx, entry, flames, &command.key, &command.value) {
            Some(payload) => payload.to_string(),
            None => return command.reply(Ok(())),
        }
    } else {
        entry.encode(&command.key, &command.value).to_string()
    };
    let topic = format!("{base}/{}/set", entry.friendly_name);
    tracing::debug!(%topic, %payload, "publish command");
    let result = client
        .try_publish(topic, QoS::AtLeastOnce, false, payload)
        .map_err(|e| format!("mqtt publish failed: {e}"));
    command.reply(result);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_formats() {
        assert!(parse_online(br#"{"state":"online"}"#));
        assert!(!parse_online(br#"{"state":"offline"}"#));
        assert!(parse_online(b"online"));
        assert!(!parse_online(b"offline"));
    }

    #[test]
    fn requests_are_not_states() {
        assert!(is_request("lamp/set"));
        assert!(is_request("lamp/set/state"));
        assert!(is_request("lamp/get"));
        assert!(!is_request("salon/lamp"));
    }

    #[test]
    fn config_defaults() {
        let config: Config = toml_like("{}");
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 1883);
        assert_eq!(config.base_topic, "zigbee2mqtt");
    }

    fn toml_like(json: &str) -> Config {
        serde_json::from_str(json).unwrap()
    }

    /// Plays MQTT messages through `on_publish`, as the broker would.
    struct Replay(Vec<Publish>);

    impl Driver for Replay {
        fn kind(&self) -> &'static str {
            "z2m"
        }

        fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                let config = toml_like("{}");
                let (mut catalog, mut flames) = (Catalog::default(), Flames::new());
                ctx.ready();
                for publish in &self.0 {
                    on_publish(
                        ctx,
                        &config,
                        "zigbee2mqtt",
                        &mut catalog,
                        &mut flames,
                        publish,
                    );
                }
                ctx.cancelled().await;
                Ok(())
            })
        }
    }

    const SWITCH: &str = "0x00124b000000000c";

    fn message(topic: &str, payload: &str, retained: bool) -> Publish {
        let mut publish = Publish::new(
            format!("zigbee2mqtt/{topic}"),
            QoS::AtMostOnce,
            payload.to_owned(),
        );
        publish.retain = retained;
        publish
    }

    /// The presses the hub hands on for the switch's `action`, after `messages`.
    async fn presses(messages: Vec<Publish>) -> Vec<String> {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let mut events = hub.subscribe();
        let mut all = vec![message(
            "bridge/devices",
            include_str!("../../../integrations/z2m/fixtures/bridge_devices.json"),
            true,
        )];
        all.extend(messages);
        let cancel = tokio_util::sync::CancellationToken::new();
        moli_runtime::spawn_driver(
            &hub,
            "z2m".into(),
            std::sync::Arc::new(Replay(all)),
            cancel.clone(),
        );
        let mut out = Vec::new();
        let point = moli_core::PointId::new(&DeviceId::new(&"z2m".into(), SWITCH), "action");
        while let Ok(Ok(event)) =
            tokio::time::timeout(Duration::from_millis(300), events.recv()).await
        {
            if let moli_core::Event::State {
                point: p,
                value,
                pulse,
                ..
            } = event
                && p == point
            {
                assert!(pulse, "a press is an occurrence");
                out.push(match value {
                    Value::Text(t) => t.to_string(),
                    other => format!("{other:?}"),
                });
            }
        }
        cancel.cancel();
        out
    }

    /// Two identical presses in a row are two presses (#198). Real frames of an
    /// Aqara WXKG06LM: Z2M sends the battery and the rest with each press.
    #[tokio::test]
    async fn the_same_press_twice_is_two_presses() {
        let press = |action: &str| {
            let frame = format!(
                r#"{{"action":"{action}","battery":100,"device_temperature":26,"linkquality":156,"power_outage_count":12,"voltage":3045}}"#
            );
            message(SWITCH, &frame, false)
        };
        let got = presses(vec![
            press("single"),
            press("single"),
            press("double"),
            press("double"),
            press("hold"),
        ])
        .await;
        assert_eq!(got, ["single", "single", "double", "double", "hold"]);
    }

    /// A retained press, replayed by the broker when Moli connects, is an old one:
    /// it sets off nothing. An empty `action` (Z2M clears it) is no press.
    #[tokio::test]
    async fn neither_a_retained_press_nor_an_empty_one_is_a_press() {
        let got = presses(vec![
            message(SWITCH, r#"{"action":"single","battery":100}"#, true),
            message(SWITCH, r#"{"action":"","battery":100}"#, false),
            message(SWITCH, r#"{"battery":99}"#, false),
        ])
        .await;
        assert!(got.is_empty(), "{got:?}");
    }
}
