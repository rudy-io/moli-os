//! Z-Wave driver (zwave-js-ui MQTT gateway).
//!
//! Subscribes to `{base}/#` and builds its catalog from the gateway's
//! `getNodes` API. The request is re-sent every `GET_NODES_RETRY_SECS`
//! until the first accepted answer, and every `GET_NODES_REFRESH_SECS`
//! afterwards, so nodes paired or renamed later appear without a driver
//! restart. If the first answer is not received within
//! `GET_NODES_TIMEOUT_SECS`, the driver starts accumulating devices from
//! the value topics it observes (tolerant fallback — armed only after that
//! first window, so a late answer never leaves ghost `zwave:<name>`
//! devices behind). Value topics update typed points, `{base}/status`
//! drives availability (a node never seen online never goes offline), and
//! commands are published to `{base}/{cc}/{endpoint}/{property}/set` —
//! where `{base}` is the node's topic base, `[loc/]name` (the gateway's
//! own sanitization).

mod catalog;

use std::collections::HashMap;
use std::time::Duration;

use tokio::time::Instant;

use anyhow::Context as _;
use moli_core::{DeviceId, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, Publish, QoS};
use serde::Deserialize;
use serde_json::Value as Json;

pub use catalog::{Catalog, Entry, SetTarget};

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// The gateway's MQTT base topic.
    #[serde(default = "default_base_topic")]
    pub base_topic: String,
    /// The gateway's MQTT client name (the `Name` field of the gateway's
    /// MQTT panel; zwave-js-ui defaults to `zwave-js-ui` unless the house
    /// runs several gateways with distinct names). The gateway's `MQTT_NAME`
    /// environment variable overrides the panel field when set.
    #[serde(default = "default_mqtt_name")]
    pub mqtt_name: String,
    #[serde(default)]
    pub username: Option<String>,
    /// Name of the environment variable holding the broker password
    /// (never the password itself).
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
    "zwave".into()
}

fn default_mqtt_name() -> String {
    "zwave-js-ui".into()
}

/// The Z-Wave driver.
#[derive(Debug)]
pub struct Zwave {
    config: Config,
}

impl Zwave {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Zwave {
    fn kind(&self) -> &'static str {
        "zwave"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// Payload of the `getNodes` request.
const GET_NODES_REQUEST: &str = r#"{"args":[]}"#;

/// How long to wait for the first `getNodes` answer before the value
/// topics start accumulating devices (tolerant fallback).
const GET_NODES_TIMEOUT_SECS: u64 = 15;

/// How often to re-request `getNodes` until the first accepted answer.
const GET_NODES_RETRY_SECS: u64 = 30;

/// How often to re-request `getNodes` once bootstrapped, so nodes paired
/// or renamed later appear without a driver restart.
const GET_NODES_REFRESH_SECS: u64 = 300;

/// Mutable state of one driver run.
#[derive(Debug, Default)]
struct ZwaveState {
    catalog: Catalog,
    /// `true` once a `getNodes` answer has rebuilt the catalog.
    bootstrapped: bool,
    /// Value topics accumulate devices only after the first
    /// `GET_NODES_TIMEOUT_SECS` window has passed (a `getNodes` answer in
    /// the meantime wins, and no ghost `zwave:<name>` device is created).
    /// Reset on each connect while not bootstrapped.
    accumulation_armed: bool,
    /// Node ids already seen online: a node never seen online never goes
    /// offline (battery sensors sleep for days).
    seen_online: HashMap<DeviceId, bool>,
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let client_id = config
        .client_id
        .clone()
        .unwrap_or_else(|| format!("moli-os-{}", ctx.instance()));
    let mut options = MqttOptions::new(client_id, &config.host, config.port);
    options.set_keep_alive(Duration::from_secs(30));
    // A `getNodes` answer carries the whole node list; the default 10 KiB
    // is too small for a full house.
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
    let mut state = ZwaveState::default();
    // Until the broker accepts us, a command would only wait in the client
    // queue, be acknowledged, and be lost (or sent much later): refuse it.
    // Any poll error ends the run, so the flag never outlives a connection.
    let mut connected = false;
    // Deadline of the next `getNodes` request. 15 s after connect (first
    // window), then 30 s retries until the first accepted answer, 300 s
    // refresh cycles afterwards.
    let mut next_request_at: Option<Instant> = None;

    loop {
        tokio::select! {
            event = events.poll() => match event.context("mqtt connection")? {
                Event::Incoming(Packet::ConnAck(_)) => {
                    connected = true;
                    client.try_subscribe(format!("{base}/#"), QoS::AtLeastOnce)?;
                    ctx.ready();
                    tracing::info!(instance = %ctx.instance(), "connected to zwave gateway");
                    if !state.bootstrapped {
                        request_get_nodes(&client, base, &config.mqtt_name)?;
                        state.accumulation_armed = false;
                        next_request_at =
                            Some(Instant::now() + Duration::from_secs(GET_NODES_TIMEOUT_SECS));
                    }
                }
                Event::Incoming(Packet::Publish(publish)) => {
                    // Suffix only: `route_topic` strips `{base}/` from the
                    // incoming topic before comparing, so the expected value
                    // must not carry the prefix (a `{base}/` prefix here would
                    // never match and the `getNodes` answer would be dropped).
                    let bootstrap_response_suffix =
                        format!("_CLIENTS/ZWAVE_GATEWAY-{}/api/getNodes", config.mqtt_name);
                    on_publish(ctx, base, &bootstrap_response_suffix, &mut state, &mut next_request_at, &publish);
                }
                _ => {}
            },
            () = sleep_until(next_request_at), if next_request_at.is_some() => {
                if state.bootstrapped {
                    // Periodic refresh: nodes paired or renamed since the last
                    // answer must appear without a driver restart.
                    request_get_nodes(&client, base, &config.mqtt_name)?;
                    next_request_at =
                        Some(Instant::now() + Duration::from_secs(GET_NODES_REFRESH_SECS));
                } else {
                    // First 15 s window (or retry) elapsed without an accepted
                    // answer: from now on value topics accumulate devices, and
                    // `getNodes` is re-requested every 30 s until accepted.
                    let requested_topic =
                        request_get_nodes(&client, base, &config.mqtt_name)?;
                    if !state.accumulation_armed {
                        // First window only: no answer here is almost always a
                        // `base_topic`/`mqtt_name` mismatch (the gateway ignores
                        // API traffic addressed to another client id) — make it
                        // visible instead of a silent 30 s retry loop.
                        tracing::warn!(
                            instance = %ctx.instance(),
                            requested_topic = %requested_topic,
                            "zwave getNodes unanswered after 15 s: check base_topic and \
                             mqtt_name (Name field of the gateway's MQTT panel, or its \
                             MQTT_NAME env var)"
                        );
                    }
                    state.accumulation_armed = true;
                    next_request_at =
                        Some(Instant::now() + Duration::from_secs(GET_NODES_RETRY_SECS));
                }
            },
            command = ctx.next_command() => {
                let Some(command) = command else {
                    return Ok(());
                };
                let decision =
                    decide_command(connected, &state.catalog, &command.device.id, &command.key, &command.value);
                match decision {
                    CommandDecision::Refuse { reason } => {
                        command.reply(Err(reason));
                    }
                    CommandDecision::Publish { topic, payload } => {
                        let full_topic = format!("{base}/{topic}");
                        let result = client
                            .try_publish(full_topic, QoS::AtLeastOnce, false, payload)
                            .map_err(|e| format!("mqtt publish failed: {e}"));
                        command.reply(result);
                    }
                }
            }
        }
    }
}

/// Awaits until the next request deadline (resolves immediately if `None`).
async fn sleep_until(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    }
}

/// Publishes the `getNodes` request to the gateway's per-client API topic;
/// returns the full topic addressed (for diagnostics).
fn request_get_nodes(client: &AsyncClient, base: &str, mqtt_name: &str) -> anyhow::Result<String> {
    let topic = format!("{base}/_CLIENTS/ZWAVE_GATEWAY-{mqtt_name}/api/getNodes/set");
    client.try_publish(
        topic.clone(),
        QoS::AtLeastOnce,
        false,
        GET_NODES_REQUEST.to_owned(),
    )?;
    Ok(topic)
}

/// The decision for one command: refuse it, or publish it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CommandDecision {
    Refuse { reason: String },
    Publish { topic: String, payload: String },
}

/// Pure decision for one command: the broker must be connected, the device
/// must be known, and the point must carry a write target. The caller
/// publishes (or refuses) the result.
fn decide_command(
    connected: bool,
    catalog: &Catalog,
    device: &DeviceId,
    key: &str,
    value: &Value,
) -> CommandDecision {
    if !connected {
        return CommandDecision::Refuse {
            reason: moli_i18n::tr!("pilotes.zwave.broker_injoignable"),
        };
    }
    let Some(entry) = catalog.by_id(device) else {
        return CommandDecision::Refuse {
            reason: moli_i18n::tr!("pilotes.zwave.appareil_inconnu"),
        };
    };
    match entry.encode(key, value) {
        Ok((topic, payload)) => CommandDecision::Publish { topic, payload },
        Err(e) => CommandDecision::Refuse {
            reason: e.to_string(),
        },
    }
}

/// How one gateway topic is routed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Routed {
    /// The `getNodes` API answer (the one we asked for).
    GetNodes,
    /// `{base}/status` availability (`base` = the node's topic base,
    /// possibly `{loc}/{name}`).
    Status { base: String },
    /// A value topic `{name}/{cc}/{endpoint}/{property}[/{propertyKey}]`.
    Value { rest: String },
    /// Everything else: foreign topics, other `_CLIENTS/` API traffic,
    /// `set`/`get` command echoes.
    Ignored,
}

/// Pure topic classification (no IO): full incoming topic + base + the
/// expected `getNodes` answer **suffix** (no base prefix) → one `Routed`.
/// The base is stripped first, so the expected suffix must not carry it.
fn route_topic(topic: &str, base: &str, bootstrap_response_suffix: &str) -> Routed {
    let Some(rest) = topic.strip_prefix(base).and_then(|t| t.strip_prefix('/')) else {
        return Routed::Ignored;
    };
    if rest == bootstrap_response_suffix {
        return Routed::GetNodes;
    }
    if rest.starts_with("_CLIENTS/") {
        // Other gateway API traffic (requests, other clients).
        return Routed::Ignored;
    }
    if is_request(rest) {
        // Echoes of `set`/`get` commands: not state.
        return Routed::Ignored;
    }
    if let Some(base) = rest.strip_suffix("/status") {
        return Routed::Status {
            base: base.to_owned(),
        };
    }
    Routed::Value {
        rest: rest.to_owned(),
    }
}

/// Routes one gateway message: the `getNodes` answer, `{name}/status`, or a
/// value topic. On a `getNodes` answer, reschedules the next request: an
/// accepted answer switches to the 300 s refresh cadence, a refused one
/// (malformed, or empty while devices are accumulated) to a 30 s retry.
fn on_publish(
    ctx: &DriverCtx,
    base: &str,
    bootstrap_response_suffix: &str,
    state: &mut ZwaveState,
    next_request_at: &mut Option<Instant>,
    publish: &Publish,
) {
    match route_topic(&publish.topic, base, bootstrap_response_suffix) {
        Routed::GetNodes => {
            let accepted = handle_get_nodes(ctx, state, &publish.payload);
            let after_secs = if accepted {
                GET_NODES_REFRESH_SECS
            } else {
                GET_NODES_RETRY_SECS
            };
            *next_request_at = Some(Instant::now() + Duration::from_secs(after_secs));
        }
        Routed::Status { base } => handle_status(ctx, state, &base, &publish.payload),
        Routed::Value { rest } => handle_value(ctx, state, &rest, &publish.payload),
        Routed::Ignored => {}
    }
}

/// An empty `getNodes` answer is only accepted when nothing was accumulated
/// yet: the gateway can answer before its Z-Wave controller finished
/// initializing, and such an empty answer must not wipe out observed devices.
#[must_use]
fn replaces_catalog(fresh: &Catalog, current: &Catalog) -> bool {
    !fresh.is_empty() || current.is_empty()
}

/// Rebuilds the catalog from the `getNodes` answer, every time: the answer
/// is the source of truth for the node list, so a refresh picks up nodes
/// paired or renamed since the last one. A malformed or empty answer
/// (the gateway can answer before its controller finished initializing)
/// keeps the current catalog. Returns `true` when the answer was accepted
/// (catalog rebuilt) so the caller schedules the 300 s refresh cadence;
/// `false` otherwise (retry in 30 s).
fn handle_get_nodes(ctx: &DriverCtx, state: &mut ZwaveState, payload: &[u8]) -> bool {
    match Catalog::from_get_nodes(ctx.instance(), payload) {
        Ok(fresh) if !replaces_catalog(&fresh, &state.catalog) => {
            tracing::warn!(
                instance = %ctx.instance(),
                devices = state.catalog.len(),
                "zwave getNodes returned an empty node list while devices are already known; \
                 keeping the current catalog and waiting for a populated answer"
            );
            false
        }
        Ok(fresh) => {
            apply_catalog(ctx, &mut state.catalog, fresh);
            state.bootstrapped = true;
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "zwave getNodes answer unusable; keeping the accumulated catalog");
            false
        }
    }
}

/// `{base}/status`: `true`/`false` (JSON boolean or string), or wrapped in
/// the gateway's `{time, value, status, nodeId}` envelope. Other payloads
/// are ignored; a node never seen online never goes offline.
fn handle_status(ctx: &DriverCtx, state: &mut ZwaveState, base: &str, payload: &[u8]) {
    let Some(online) = parse_status(payload) else {
        return;
    };
    let Some(entry) = state.catalog.by_base(base) else {
        return;
    };
    let id = entry.device.id.clone();
    let seen = state.seen_online.get(&id).copied().unwrap_or(false);
    let Some(action) = availability_action(seen, online) else {
        return;
    };
    if online {
        state.seen_online.insert(id.clone(), true);
    }
    ctx.set_availability(&id, action);
}

/// One value topic: `{base}/{cc}/{endpoint}/{property}[/{propertyKey}]`.
/// After the `getNodes` answer, the entry is resolved by its topic base
/// (which may carry a `{loc}/` prefix). Before it, value topics are ignored
/// until the first `GET_NODES_TIMEOUT_SECS` window has elapsed (the
/// `accumulation_armed` flag, set by the timer): accumulating from the first
/// message would create ghost `zwave:<name>` devices with retain when the
/// `getNodes` answer arrives seconds later. Once armed, known
/// `(cc, property[, key])` tuples accumulate devices under the first topic
/// segment (the base cannot be known yet).
fn handle_value(ctx: &DriverCtx, state: &mut ZwaveState, rest: &str, payload: &[u8]) {
    if state.bootstrapped {
        if let Some(entry) = state.catalog.entry_for_value_topic(rest) {
            for (key, value) in entry.decode(rest, payload) {
                ctx.set_state(&entry.device.id, &key, value);
            }
        }
        return;
    }
    if !state.accumulation_armed {
        return;
    }
    // Tolerant accumulation: only known `(cc, property)` pairs create a
    // device; unknown ones never do. The device keeps the node name as its
    // identity until a `getNodes` answer replaces it with the node id.
    let Some((name, cc, endpoint, property, property_key)) = value_topic_parts(rest) else {
        return;
    };
    let Some(entry) = state.catalog.upsert_accumulated(
        ctx.instance(),
        name,
        cc,
        endpoint,
        property,
        property_key,
    ) else {
        return;
    };
    let id = entry.device.id.clone();
    ctx.upsert_device(entry.device.clone());
    if !state.seen_online.get(&id).copied().unwrap_or(false) {
        state.seen_online.insert(id.clone(), true);
    }
    ctx.set_availability(&id, true);
    for (key, value) in entry.decode(rest, payload) {
        ctx.set_state(&id, &key, value);
    }
}

/// Replaces the catalog: removes vanished device ids, upserts the fresh
/// devices. Accumulated (named) devices are removed here too.
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
    tracing::info!(devices = fresh.len(), "zwave catalog loaded");
    *current = fresh;
}

/// Our own (and others') `set`/`get` commands echo back through `#`.
fn is_request(rest: &str) -> bool {
    rest.ends_with("/set")
        || rest.ends_with("/get")
        || rest.contains("/set/")
        || rest.contains("/get/")
}

/// Splits a value topic suffix `{base}/{cc}/{endpoint}/{property}`; the
/// optional fifth `{propertyKey}` segment is kept (it decides the mapping
/// for meter values, CC 50).
fn value_topic_parts(rest: &str) -> Option<(&str, u8, u8, &str, Option<&str>)> {
    let mut parts = rest.splitn(5, '/');
    let name = parts.next()?;
    let cc: u8 = parts.next()?.parse().ok()?;
    let endpoint: u8 = parts.next()?.parse().ok()?;
    let property = parts.next()?;
    let property_key = parts.next();
    (!name.is_empty() && !property.is_empty()).then_some((
        name,
        cc,
        endpoint,
        property,
        property_key,
    ))
}

/// `{base}/status` payload: `true`/`false` as a JSON boolean or a string,
/// or wrapped in the gateway's `{time, value, status, nodeId}` envelope.
fn parse_status(payload: &[u8]) -> Option<bool> {
    let json: Json = serde_json::from_slice(payload).ok()?;
    match &json {
        Json::Object(_) => json.get("value").and_then(bool_from_json),
        other => bool_from_json(other),
    }
}

/// Extracts a boolean from a status payload body.
fn bool_from_json(json: &Json) -> Option<bool> {
    match json {
        Json::Bool(b) => Some(*b),
        Json::String(s) => match s.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A node that was never seen online never goes offline (a battery sensor
/// may be asleep for days); the first `true` marks it as seen.
fn availability_action(seen_online: bool, online: bool) -> Option<bool> {
    if online {
        Some(true)
    } else {
        seen_online.then_some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moli_core::InstanceId;

    fn catalog() -> Catalog {
        Catalog::from_get_nodes(
            &InstanceId::from("zwave"),
            include_bytes!("../../../integrations/zwave/fixtures/get_nodes.json"),
        )
        .unwrap()
    }

    fn instance() -> InstanceId {
        InstanceId::from("zwave")
    }

    #[test]
    fn config_defaults() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 1883);
        assert_eq!(config.base_topic, "zwave");
        assert_eq!(config.mqtt_name, "zwave-js-ui");
        assert!(config.username.is_none());
        assert!(config.password_env.is_none());
        assert!(config.client_id.is_none());
    }

    #[test]
    fn config_denies_unknown_field() {
        let err =
            serde_json::from_str::<Config>(r#"{"host":"127.0.0.1","unknown":1}"#).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("unknown"), "{message}");
    }

    #[test]
    fn status_payloads() {
        assert_eq!(parse_status(b"true"), Some(true));
        assert_eq!(parse_status(b"false"), Some(false));
        assert_eq!(parse_status(b"\"true\""), Some(true));
        assert_eq!(parse_status(b"\"false\""), Some(false));
        assert_eq!(
            parse_status(b"\"offline\""),
            None,
            "other strings are ignored"
        );
        assert_eq!(
            parse_status(b"maybe"),
            None,
            "non-JSON payloads are ignored"
        );
        // Real gateway wraps status in a {time, value, status, nodeId} envelope.
        assert_eq!(
            parse_status(
                br#"{"time":"2026-06-24T16:20:00Z","value":true,"status":"Alive","nodeId":6}"#
            ),
            Some(true)
        );
        assert_eq!(
            parse_status(br#"{"time":"2026-06-24T16:20:00Z","value":false}"#),
            Some(false)
        );
        assert_eq!(
            parse_status(br#"{"value":"Alive"}"#),
            None,
            "non-boolean envelope value is ignored"
        );
    }

    #[test]
    fn availability_never_drops_a_node_never_seen_online() {
        assert_eq!(availability_action(false, true), Some(true));
        assert_eq!(availability_action(true, true), Some(true));
        assert_eq!(availability_action(true, false), Some(false));
        assert_eq!(availability_action(false, false), None);
    }

    #[test]
    fn command_decisions() {
        let catalog = catalog();
        let lamp = DeviceId::new(&instance(), "2");

        // No broker: refused with the i18n reason.
        let decision = decide_command(false, &catalog, &lamp, "on", &Value::Bool(true));
        assert_eq!(
            decision,
            CommandDecision::Refuse {
                reason: moli_i18n::tr!("pilotes.zwave.broker_injoignable")
            }
        );

        // Unknown device: refused with the i18n reason.
        let ghost = DeviceId::new(&instance(), "999");
        let decision = decide_command(true, &catalog, &ghost, "on", &Value::Bool(true));
        assert_eq!(
            decision,
            CommandDecision::Refuse {
                reason: moli_i18n::tr!("pilotes.zwave.appareil_inconnu")
            }
        );

        // Multilevel lamp: `on` goes to `38/0/targetValue/set` with a numeric
        // payload (CC 38 targetValue takes a level 0-99, or 255 to restore
        // the previous one, not a boolean).
        let decision = decide_command(true, &catalog, &lamp, "on", &Value::Bool(true));
        assert_eq!(
            decision,
            CommandDecision::Publish {
                topic: "bureau_lamp/38/0/targetValue/set".into(),
                payload: "255".into(),
            }
        );

        // Read-only point: refused.
        let sensor = DeviceId::new(&instance(), "5");
        let decision = decide_command(true, &catalog, &sensor, "battery", &Value::Int(100));
        assert!(matches!(decision, CommandDecision::Refuse { .. }));
    }

    const BOOTSTRAP_SUFFIX: &str = "_CLIENTS/ZWAVE_GATEWAY-zwave-js-ui/api/getNodes";

    /// Regression (CR-01): the `getNodes` answer arrives on
    /// `{base}/_CLIENTS/…` — the router strips the base before comparing, so
    /// the expected suffix must not carry it, and the answer must be routed
    /// to the bootstrap, not swallowed by the `_CLIENTS/` ignore branch.
    #[test]
    fn the_get_nodes_answer_reaches_the_bootstrap() {
        assert_eq!(
            route_topic(
                "zwave/_CLIENTS/ZWAVE_GATEWAY-zwave-js-ui/api/getNodes",
                "zwave",
                BOOTSTRAP_SUFFIX
            ),
            Routed::GetNodes
        );
    }

    #[test]
    fn routing_classifies_gateway_topics() {
        // Value topic: passes through with its suffix.
        assert_eq!(
            route_topic(
                "zwave/bureau_lamp/38/0/currentValue",
                "zwave",
                BOOTSTRAP_SUFFIX
            ),
            Routed::Value {
                rest: "bureau_lamp/38/0/currentValue".into()
            }
        );
        // Availability topic.
        assert_eq!(
            route_topic("zwave/bureau_lamp/status", "zwave", BOOTSTRAP_SUFFIX),
            Routed::Status {
                base: "bureau_lamp".into()
            }
        );
        // Availability topic with a location segment in the base.
        assert_eq!(
            route_topic("zwave/Salon_Est/lampe/status", "zwave", BOOTSTRAP_SUFFIX),
            Routed::Status {
                base: "Salon_Est/lampe".into()
            }
        );
        // Our own commands echo back through `{base}/#`: never state.
        assert_eq!(
            route_topic(
                "zwave/bureau_lamp/38/0/targetValue/set",
                "zwave",
                BOOTSTRAP_SUFFIX
            ),
            Routed::Ignored
        );
        // The `getNodes` request itself (with `/set`) is API traffic.
        assert_eq!(
            route_topic(
                "zwave/_CLIENTS/ZWAVE_GATEWAY-zwave-js-ui/api/getNodes/set",
                "zwave",
                BOOTSTRAP_SUFFIX
            ),
            Routed::Ignored
        );
        // Traffic from another base topic is ignored.
        assert_eq!(
            route_topic("zigbee2mqtt/bridge/state", "zwave", BOOTSTRAP_SUFFIX),
            Routed::Ignored
        );
    }

    #[test]
    fn an_empty_get_nodes_answer_never_wipes_accumulated_devices() {
        let populated = catalog();
        let empty = Catalog::default();
        assert!(replaces_catalog(&populated, &empty));
        assert!(replaces_catalog(&empty, &empty));
        assert!(
            !replaces_catalog(&empty, &populated),
            "an empty answer while devices are accumulated must keep the catalog"
        );
    }
}
