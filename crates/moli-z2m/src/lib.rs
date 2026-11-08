//! Zigbee2MQTT driver.
//!
//! Subscribes to `{base}/#`, maps `bridge/devices` to Moli devices, state
//! messages to values and availability messages to online flags, and turns
//! commands into `{friendly_name}/set` publications.
//!
//! The bridge itself is published as a `bridge` pseudo-device exposing the
//! pairing window: the `pairing` point is opened with
//! `{base}/bridge/request/permit_join` and confirmed by
//! `bridge/response/permit_join`, never blindly.
//!
//! Publishing to MQTT is not a delivery: Z2M routes the command to the device
//! and only surfaces failures on its log topic (`bridge/logging`). A command
//! is therefore answered when the device's state echo confirms the value,
//! when Z2M's log records a refusal, or, past `WRITE_TIMEOUT`, as a failure
//! ("sent, not confirmed") — never blindly.

mod catalog;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, Publish, QoS};
use serde::Deserialize;
use serde_json::Value as Json;

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

/// Everything a run keeps between MQTT events.
struct State {
    catalog: Catalog,
    /// In-flight `set` commands, by (ieee, point): only a confirming state
    /// echo, a Z2M failure log or the `WRITE_TIMEOUT` deadline settle them.
    pending: HashMap<(String, Arc<str>), (CommandRequest, Instant)>,
    /// Devices Z2M reports off the network. Only mains-powered devices are
    /// tracked here: a sleeping end device is not off the network.
    offline: HashSet<String>,
    /// The bridge's pseudo-device: the pairing window as a writable point.
    bridge: Device,
    /// In-flight `permit_join` request: (target state, command, sent at).
    permit: Option<(bool, CommandRequest, Instant)>,
    /// When the open pairing window closes by itself (Z2M's own timer).
    permit_until: Option<Instant>,
}

/// One device read every half second.
const READ_PACE: Duration = Duration::from_millis(500);
/// A write not confirmed within 15 s is reported as a failure: Z2M gives up
/// on the device's ZCL command at 10 s and logs the refusal, so by then the
/// echo or the failure log has been seen — silence means "not confirmed".
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);
/// The bridge's pseudo-device, native part of its id (`"<instance>:bridge"`).
const BRIDGE_NATIVE: &str = "bridge";
/// The pairing point of the bridge pseudo-device.
const PAIRING_KEY: &str = "pairing";
/// The pairing window opened for a command: Z2M's hard ceiling (seconds).
const PERMIT_JOIN_SECS: u64 = 254;

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
    let mut state = State {
        catalog: Catalog::default(),
        pending: HashMap::new(),
        offline: HashSet::new(),
        bridge: bridge_device(ctx.instance()),
        permit: None,
        permit_until: None,
    };
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

    loop {
        tokio::select! {
            event = events.poll() => match event {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    connected = true;
                    // The bridge pseudo-device is published before subscribing:
                    // the retained `bridge/info` may carry the window's state
                    // as soon as it arrives, and states of unknown devices are
                    // dropped.
                    ctx.upsert_device(state.bridge.clone());
                    ctx.set_availability(&state.bridge.id, true);
                    client.try_subscribe(format!("{base}/#"), QoS::AtLeastOnce)?;
                    tracing::info!(instance = %ctx.instance(), "connected to mqtt");
                    ctx.ready();
                }
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    let reloaded = on_publish(ctx, base, &mut state, &publish);
                    if reloaded {
                        queue_reads(base, &state.catalog, &mut read_done, &mut reads);
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    // The link to the broker is down: the bridge is
                    // unreachable until the supervisor reconnects.
                    ctx.set_availability(&state.bridge.id, false);
                    return Err(anyhow::anyhow!(e).context("mqtt connection"));
                }
            },
            _ = pacer.tick(),
                if !reads.is_empty()
                    || !state.pending.is_empty()
                    || state.permit.is_some()
                    || state.permit_until.is_some()
                =>
            {
                if let Some((topic, payload)) = reads.pop_front()
                    && client.try_publish(&topic, QoS::AtMostOnce, false, payload.clone()).is_err()
                {
                    reads.push_front((topic, payload));
                }
                expire_pending(&mut state);
                let now = Instant::now();
                // A `permit_join` request the bridge never answered fails like
                // any other write.
                if state
                    .permit
                    .as_ref()
                    .is_some_and(|(_, _, sent)| now.duration_since(*sent) >= WRITE_TIMEOUT)
                {
                    let (target, command, sent) =
                        state.permit.take().expect("permit request in flight");
                    command.reply(Err(moli_i18n::tr!("pilotes.z2m.sans_accuse")));
                    if target {
                        // The publish reached the local broker, so the window
                        // is almost certainly open: assume it, and close the
                        // point at the bridge's own deadline.
                        ctx.set_state(&state.bridge.id, PAIRING_KEY, Value::Bool(true));
                        state.permit_until = Some(sent + Duration::from_secs(PERMIT_JOIN_SECS));
                    }
                }
                // The window Z2M closed by itself: the point must not stay "on".
                if state.permit_until.is_some_and(|deadline| now >= deadline) {
                    state.permit_until = None;
                    ctx.set_state(&state.bridge.id, PAIRING_KEY, Value::Bool(false));
                }
            },
            command = ctx.next_command() => match command {
                Some(command) if !connected => {
                    command.reply(Err(moli_i18n::tr!("pilotes.z2m.pont_injoignable")));
                }
                Some(command) => on_command(&client, base, &mut state, command),
                None => return Ok(()),
            },
        }
    }
}

/// Handles one message. Returns `true` when the catalog was reloaded.
fn on_publish(ctx: &DriverCtx, base: &str, state: &mut State, publish: &Publish) -> bool {
    let Some(rest) = publish
        .topic
        .strip_prefix(base)
        .and_then(|t| t.strip_prefix('/'))
    else {
        return false;
    };
    if rest == "bridge/devices" {
        return match Catalog::from_bridge_devices(ctx.instance(), &publish.payload) {
            Ok(fresh) => {
                apply_catalog(ctx, &mut state.catalog, fresh, &state.bridge.id);
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "invalid bridge/devices payload");
                false
            }
        };
    }
    if rest == "bridge/logging" || rest == "bridge/log" {
        on_bridge_log(state, &publish.payload);
        return false;
    }
    if rest == "bridge/info" {
        on_bridge_info(ctx, state, &publish.payload);
        return false;
    }
    if rest == "bridge/response/permit_join" {
        on_permit_join_response(ctx, state, &publish.payload);
        return false;
    }
    if rest.starts_with("bridge/") || is_request(rest) {
        return false;
    }
    if let Some(name) = rest.strip_suffix("/availability") {
        let online = parse_online(&publish.payload);
        if let Some(entry) = state.catalog.by_name(name) {
            let (device, ieee) = (entry.device.id.clone(), entry.ieee.clone());
            ctx.set_availability(&device, online);
            if online {
                state.offline.remove(&ieee);
            } else if entry.mains {
                state.offline.insert(ieee.clone());
                fail_pending(
                    state,
                    &ieee,
                    None,
                    &moli_i18n::tr!("pilotes.z2m.hors_ligne"),
                );
            }
        }
        return false;
    }
    let Some(entry) = state.catalog.by_name(rest) else {
        return false;
    };
    if let Ok(Json::Object(payload)) = serde_json::from_slice(&publish.payload) {
        let decoded = entry.decode(&payload);
        let device = entry.device.id.clone();
        let ieee = entry.ieee.clone();
        for (key, value) in decoded {
            settle(state, &ieee, &key, &value);
            ctx.set_state(&device, &key, value);
        }
    } else {
        tracing::debug!(topic = %publish.topic, "non-object state payload ignored");
    }
    false
}

/// Z2M's log mirror (`bridge/logging` in 2.x, `bridge/log` in 1.x): the only
/// place a refused `set` shows up, shaped
/// `Publish 'set' '<point>' to '<name>' failed: '<error>'`.
fn on_bridge_log(state: &mut State, payload: &[u8]) {
    let Ok(Json::Object(map)) = serde_json::from_slice(payload) else {
        return;
    };
    let Some(message) = map.get("message").and_then(Json::as_str) else {
        return;
    };
    if map
        .get("level")
        .and_then(Json::as_str)
        .is_some_and(|level| level != "error")
    {
        return;
    }
    let Some((name, key, error)) = parse_publish_failure(message) else {
        return;
    };
    let Some(ieee) = state.catalog.by_name(name).map(|entry| entry.ieee.clone()) else {
        return;
    };
    tracing::warn!(device = %ieee, key, "zigbee2mqtt refused the command");
    let reason = moli_i18n::tr!("pilotes.z2m.ecriture_refusee", detail = error_detail(error));
    fail_pending(state, &ieee, Some(key), &reason);
}

/// The part of a Z2M failure worth keeping in the journal: the last quoted
/// token (`NWK_NO_ROUTE`), or the whole error, truncated.
fn error_detail(error: &str) -> String {
    let mut prev: Option<usize> = None;
    let mut last: Option<usize> = None;
    for (index, _) in error.match_indices('\'') {
        prev = last;
        last = Some(index);
    }
    let detail = match (prev, last) {
        (Some(open), Some(close)) if close > open + 1 => &error[open + 1..close],
        _ => error,
    };
    detail.chars().take(120).collect()
}

/// `Publish 'set' '<key>' to '<name>' failed: '<error>'` → (name, key, error).
/// Only `set` operations are ours (Z2M also logs `get` failures).
fn parse_publish_failure(message: &str) -> Option<(&str, &str, &str)> {
    let rest = message.strip_prefix("Publish '")?;
    let (kind, rest) = rest.split_once("' '")?;
    if kind != "set" {
        return None;
    }
    let (key, rest) = rest.split_once("' to '")?;
    let (name, rest) = rest.split_once("' failed: '")?;
    let error = rest.strip_suffix('\'')?;
    (!name.is_empty() && !key.is_empty()).then_some((name, key, error))
}

/// A state echo matching an in-flight command settles it as delivered.
fn settle(state: &mut State, ieee: &str, key: &Arc<str>, value: &Value) {
    let point = (ieee.to_owned(), key.clone());
    if state
        .pending
        .get(&point)
        .is_some_and(|(command, _)| same_value(&command.value, value))
        && let Some((command, _)) = state.pending.remove(&point)
    {
        command.reply(Ok(()));
    }
}

/// Loose comparison of a commanded value with the device's echo: booleans
/// exactly, text without case, numbers by value (a whole-number Float may
/// travel back as an Int).
fn same_value(commanded: &Value, echoed: &Value) -> bool {
    match (commanded, echoed) {
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Text(a), Value::Text(b)) => a.eq_ignore_ascii_case(b),
        _ => match (commanded.as_f64(), echoed.as_f64()) {
            (Some(a), Some(b)) => (a - b).abs() < 1e-9,
            _ => false,
        },
    }
}

/// Fails the in-flight commands of one device. `key` limits the damage to
/// the point Z2M refused (a composite point matches on its top-level key);
/// `None` hits them all (the device went off the network).
fn fail_pending(state: &mut State, ieee: &str, key: Option<&str>, reason: &str) {
    let doomed: Vec<_> = state
        .pending
        .keys()
        .filter(|(id, _k)| id == ieee && key.is_none_or(|k| k.split('.').next() == Some(k)))
        .cloned()
        .collect();
    for point in doomed {
        if let Some((command, _)) = state.pending.remove(&point) {
            command.reply(Err(reason.to_owned()));
        }
    }
}

/// In-flight writes past `WRITE_TIMEOUT`: neither the echo nor a failure log
/// arrived, so the write is reported as a failure.
fn expire_pending(state: &mut State) {
    let now = Instant::now();
    let expired: Vec<_> = state
        .pending
        .iter()
        .filter(|(_, (_, sent))| now.duration_since(*sent) >= WRITE_TIMEOUT)
        .map(|(point, _)| point.clone())
        .collect();
    for point in expired {
        if let Some((command, _)) = state.pending.remove(&point) {
            command.reply(Err(moli_i18n::tr!("pilotes.z2m.sans_accuse")));
        }
    }
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

fn apply_catalog(ctx: &DriverCtx, current: &mut Catalog, fresh: Catalog, bridge: &DeviceId) {
    let keep: Vec<DeviceId> = fresh.entries().map(|e| e.device.id.clone()).collect();
    for id in ctx.devices() {
        // The bridge pseudo-device is not part of Z2M's catalog: never pruned.
        if !keep.contains(&id) && &id != bridge {
            ctx.remove_device(&id);
        }
    }
    for entry in fresh.entries() {
        ctx.upsert_device(entry.device.clone());
    }
    tracing::info!(devices = fresh.len(), "zigbee catalog loaded");
    *current = fresh;
}

fn on_command(client: &AsyncClient, base: &str, state: &mut State, command: CommandRequest) {
    // The bridge's pseudo-device: the pairing window is a command, not a
    // device point.
    if command.device.id == state.bridge.id {
        on_bridge_command(client, base, state, command);
        return;
    }
    let Some(entry) = state.catalog.by_id(&command.device.id) else {
        command.reply(Err(moli_i18n::tr!("pilotes.z2m.appareil_inconnu")));
        return;
    };
    // A mains-powered device Z2M reports off the network cannot be reached:
    // refusing is faster and truer than waiting for the ZCL timeout.
    if entry.mains && state.offline.contains(&entry.ieee) {
        command.reply(Err(moli_i18n::tr!("pilotes.z2m.hors_ligne")));
        return;
    }
    let key = command.key.clone();
    let point = (entry.ieee.clone(), key.clone());
    if state.pending.contains_key(&point) {
        command.reply(Err(moli_i18n::tr!("pilotes.z2m.occupe")));
        return;
    }
    let payload = entry.encode(&key, &command.value).to_string();
    let topic = format!("{base}/{}/set", entry.friendly_name);
    tracing::debug!(%topic, %payload, "publish command");
    match client.try_publish(topic, QoS::AtLeastOnce, false, payload) {
        Ok(()) => {
            state.pending.insert(point, (command, Instant::now()));
        }
        Err(e) => {
            tracing::warn!(error = %e, "mqtt publish failed (command refused)");
            command.reply(Err(moli_i18n::tr!("pilotes.z2m.pont_injoignable")));
        }
    }
}

/// Pseudo-device representing the bridge: the pairing window as a writable
/// binary point, so the dashboard, the journal, the guard and agents all go
/// through the regular command pipeline.
fn bridge_device(instance: &InstanceId) -> Device {
    Device {
        id: DeviceId::new(instance, BRIDGE_NATIVE),
        instance: instance.clone(),
        native_name: moli_i18n::tr!("pilotes.z2m.passerelle").into(),
        manufacturer: None,
        model: None,
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![PointSpec {
            key: PAIRING_KEY.into(),
            label: moli_i18n::tr!("pilotes.z2m.appairage").into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: Semantic::Other,
        }],
    }
}

/// `pairing = true` opens the window for `PERMIT_JOIN_SECS` (Z2M's ceiling),
/// `pairing = false` closes it. Z2M parses the payload with parseInt: a
/// seconds count opens, anything else closes.
fn on_bridge_command(client: &AsyncClient, base: &str, state: &mut State, command: CommandRequest) {
    let Value::Bool(open) = &command.value else {
        // The point is binary and the hub validates values: unreachable.
        command.reply(Err(moli_i18n::tr!("pilotes.z2m.pont_injoignable")));
        return;
    };
    if state.permit.is_some() {
        command.reply(Err(moli_i18n::tr!("pilotes.z2m.occupe")));
        return;
    }
    let payload = if *open {
        PERMIT_JOIN_SECS.to_string()
    } else {
        "false".into()
    };
    let topic = format!("{base}/bridge/request/permit_join");
    tracing::debug!(%topic, %payload, "publish permit_join");
    match client.try_publish(topic, QoS::AtLeastOnce, false, payload) {
        Ok(()) => {
            state.permit = Some((*open, command, Instant::now()));
        }
        Err(e) => {
            tracing::warn!(error = %e, "mqtt publish failed (permit_join refused)");
            command.reply(Err(moli_i18n::tr!("pilotes.z2m.pont_injoignable")));
        }
    }
}

/// The retained `bridge/info`: the window's state at the moment the bridge
/// published it (startup, re-subscribe), plus its expiry in epoch ms.
fn on_bridge_info(ctx: &DriverCtx, state: &mut State, payload: &[u8]) {
    // A request we're waiting on settles the state; the retained message
    // may be stale.
    if state.permit.is_some() {
        return;
    }
    let Some((open, end_ms)) = parse_bridge_info(payload) else {
        return;
    };
    ctx.set_state(&state.bridge.id, PAIRING_KEY, Value::Bool(open));
    // A window still open across our restart: the bridge's timer keeps
    // running.
    state.permit_until = open
        .then_some(end_ms)
        .flatten()
        .filter(|end| *end > moli_core::now_ms())
        .map(|end| Instant::now() + Duration::from_millis(end - moli_core::now_ms()));
}

/// `permit_join` (+ optional `permit_join_end`, epoch ms) from `bridge/info`.
#[must_use]
fn parse_bridge_info(payload: &[u8]) -> Option<(bool, Option<u64>)> {
    let Json::Object(map) = serde_json::from_slice(payload).ok()? else {
        return None;
    };
    let open = map.get("permit_join").and_then(Json::as_bool)?;
    Some((open, map.get("permit_join_end").and_then(Json::as_u64)))
}

/// Z2M's answer to `bridge/request/permit_join`: settles our in-flight
/// request if any, and moves the point to the window's state. The answer is
/// the bridge's ground truth even when the request came from elsewhere
/// (a script, the Z2M UI): the state is tracked either way.
fn on_permit_join_response(ctx: &DriverCtx, state: &mut State, payload: &[u8]) {
    let pending = state.permit.take();
    match parse_permit_response(payload) {
        Ok(time_secs) => {
            if let Some((_, command, _)) = pending {
                command.reply(Ok(()));
            }
            if time_secs > 0 {
                ctx.set_state(&state.bridge.id, PAIRING_KEY, Value::Bool(true));
                state.permit_until = Some(Instant::now() + Duration::from_secs(time_secs));
            } else {
                state.permit_until = None;
                ctx.set_state(&state.bridge.id, PAIRING_KEY, Value::Bool(false));
            }
        }
        Err(detail) => {
            let Some((_, command, _)) = pending else {
                // A refusal for a request we did not send: not ours.
                return;
            };
            let detail = detail.unwrap_or_else(|| moli_i18n::tr!("pilotes.z2m.reponse_illisible"));
            command.reply(Err(moli_i18n::tr!(
                "pilotes.z2m.ecriture_refusee",
                detail = detail
            )));
        }
    }
}

/// `{"status":"ok","data":{"time":254}}` on success,
/// `{"status":"error","error":"…"}` on refusal. `Ok(0)` when the answer
/// carries no usable duration (closing sends `null`).
fn parse_permit_response(payload: &[u8]) -> Result<u64, Option<String>> {
    let Some(Json::Object(map)) = serde_json::from_slice(payload).ok() else {
        return Err(None);
    };
    let status = map.get("status").and_then(Json::as_str).unwrap_or("error");
    if status == "ok" {
        Ok(map
            .get("data")
            .and_then(|d| d.get("time"))
            .and_then(Json::as_u64)
            .unwrap_or(0))
    } else {
        Err(map.get("error").and_then(Json::as_str).map(str::to_owned))
    }
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
        assert!(!is_request("salon/lamp"));
    }

    #[test]
    fn config_defaults() {
        let config: Config = toml_like("{}");
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 1883);
        assert_eq!(config.base_topic, "zigbee2mqtt");
    }

    #[test]
    fn refused_publishes_are_recognized() {
        let message = "Publish 'set' 'state' to '0xa4c138ddee6fab4e' failed: 'Error: ZCL command 0xa4c138ddee6fab4e/1 manuSpecificTuya.dataRequest({\"seq\":1}) failed (Data request failed with error: 'NWK_NO_ROUTE' (0xcd))'";
        let (name, key, error) = parse_publish_failure(message).unwrap();
        assert_eq!(name, "0xa4c138ddee6fab4e");
        assert_eq!(key, "state");
        assert!(error.contains("NWK_NO_ROUTE"));
        // A composite point: Z2M logs the top-level key.
        assert_eq!(
            parse_publish_failure("Publish 'set' 'color' to 'lamp' failed: 'Error: no route'"),
            Some(("lamp", "color", "Error: no route"))
        );
    }

    #[test]
    fn other_log_lines_are_ignored() {
        assert_eq!(
            parse_publish_failure("Publish 'get' 'state' to 'x' failed: 'Error: timeout'"),
            None
        );
        assert_eq!(parse_publish_failure("Device 'x' joined"), None);
    }

    #[test]
    fn the_error_detail_keeps_the_zcl_error() {
        assert_eq!(
            error_detail(
                "Error: ZCL command 0x1/1 failed (Data request failed with error: 'NWK_NO_ROUTE' (0xcd))"
            ),
            "NWK_NO_ROUTE"
        );
        assert_eq!(
            error_detail("Error: something broke"),
            "Error: something broke"
        );
    }

    #[test]
    fn values_compare_loosely() {
        assert!(same_value(&Value::Bool(false), &Value::Bool(false)));
        assert!(!same_value(&Value::Bool(true), &Value::Bool(false)));
        assert!(same_value(&Value::Float(60.0), &Value::Int(60)));
        assert!(!same_value(&Value::Int(60), &Value::Int(61)));
        assert!(same_value(
            &Value::Text("ON".into()),
            &Value::Text("on".into())
        ));
    }

    #[test]
    fn the_bridge_device_exposes_the_pairing_point() {
        let device = bridge_device(&InstanceId::from("z2m"));
        assert_eq!(device.id.as_str(), "z2m:bridge");
        assert_eq!(device.points.len(), 1);
        let point = &device.points[0];
        assert_eq!(&*point.key, PAIRING_KEY);
        assert_eq!(point.kind, Kind::Binary);
        assert!(point.access.read && point.access.write);
    }

    #[test]
    fn the_bridge_info_reports_the_window_state() {
        assert_eq!(
            parse_bridge_info(br#"{"permit_join":true,"permit_join_end":1760000000000}"#),
            Some((true, Some(1_760_000_000_000)))
        );
        assert_eq!(
            parse_bridge_info(br#"{"permit_join":false}"#),
            Some((false, None))
        );
        assert_eq!(parse_bridge_info(b"{}"), None);
        assert_eq!(parse_bridge_info(b"garbage"), None);
    }

    #[test]
    fn the_permit_join_response_parses() {
        assert_eq!(
            parse_permit_response(br#"{"status":"ok","data":{"time":254}}"#),
            Ok(254)
        );
        // Fermeture : la durée part en `null` (NaN côté Z2M) → 0.
        assert_eq!(
            parse_permit_response(br#"{"status":"ok","data":{"time":null}}"#),
            Ok(0)
        );
        assert_eq!(
            parse_permit_response(br#"{"data":{},"status":"error","error":"Invalid payload"}"#),
            Err(Some("Invalid payload".into()))
        );
        assert_eq!(parse_permit_response(b"garbage"), Err(None));
    }

    fn toml_like(json: &str) -> Config {
        serde_json::from_str(json).unwrap()
    }
}
