//! 433 MHz driver (rfxcom2mqtt MQTT gateway).
//!
//! Subscribes first to the bridge's Home Assistant discovery configs and its
//! retained status, and to its state topics only once the configs have
//! settled (no new config for `DISCOVERY_QUIET_MS`): the broker does not
//! order retained messages across subscriptions, and a retained state
//! arriving before its device's config would be lost. At that moment, the
//! devices of a previous run the bridge no longer announces are removed, but
//! only when the bridge said `online` and announced at least one device: a
//! broker restarted without persistence, or a bridge that has not published
//! its configs yet, must not empty the house.
//! Devices come from the discovery configs only (a 433 MHz receiver hears
//! the whole street; see `catalog`). State topics (`{base}/devices/{id}`,
//! `{base}/devices/{id}/{unit}`) update typed points, `{base}/bridge/status`
//! drives availability, and a switch's `on` is published to the command
//! topic its config names (`{base}/cmd/{type}/{subtype}/{id}/{unit}`).

mod catalog;

use std::time::Duration;

use tokio::time::Instant;

use anyhow::Context as _;
use moli_core::{DeviceId, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, Publish, QoS};
use serde::Deserialize;

pub use catalog::{Catalog, Change, Command, Conv, Entry, Prop};

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// The bridge's MQTT base topic (`mqtt.base_topic` of rfxcom2mqtt).
    #[serde(default = "default_base_topic")]
    pub base_topic: String,
    /// The bridge's discovery prefix (`homeassistant.discovery_topic`).
    #[serde(default = "default_discovery_topic")]
    pub discovery_topic: String,
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
    "rfxcom2mqtt".into()
}

fn default_discovery_topic() -> String {
    "homeassistant".into()
}

/// The rfxcom2mqtt driver.
#[derive(Debug)]
pub struct Rfxcom2Mqtt {
    config: Config,
}

impl Rfxcom2Mqtt {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Rfxcom2Mqtt {
    fn kind(&self) -> &'static str {
        "rfxcom2mqtt"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// How long the discovery configs must stay quiet before the state topics
/// are subscribed.
const DISCOVERY_QUIET_MS: u64 = 1000;

/// The longest wait for the discovery configs to settle.
const DISCOVERY_MAX_MS: u64 = 15_000;

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let client_id = config
        .client_id
        .clone()
        .unwrap_or_else(|| format!("moli-os-{}", ctx.instance()));
    let mut options = MqttOptions::new(client_id, &config.host, config.port);
    options.set_keep_alive(Duration::from_secs(30));
    // The discovery prefix also carries the other bridges' configs
    // (Zigbee2MQTT's can be large): the default 10 KiB is too small.
    options.set_max_packet_size(1024 * 1024, 64 * 1024);
    if let Some(username) = &config.username {
        let password = match &config.password_env {
            Some(var) => std::env::var(var).with_context(|| format!("env var {var} not set"))?,
            None => String::new(),
        };
        options.set_credentials(username, password);
    }

    let (client, mut events) = AsyncClient::new(options, 64);
    let base = config.base_topic.trim_end_matches('/');
    let discovery = config.discovery_topic.trim_end_matches('/');
    let mut state = State::default();
    // Until the broker accepts us, a command would only wait in the client
    // queue, be acknowledged, and be lost: refuse it. Any poll error ends
    // the run, so the flag never outlives a connection.
    let mut connected = false;
    // When the state topics get subscribed (`None`: done, or not connected).
    let mut states_at: Option<Instant> = None;
    let mut states_by = Instant::now();

    loop {
        tokio::select! {
            event = events.poll() => match event.context("mqtt connection")? {
                Event::Incoming(Packet::ConnAck(_)) => {
                    connected = true;
                    // Discovery first; the states once it has settled (see
                    // the module comment).
                    client.try_subscribe(format!("{discovery}/+/+/config"), QoS::AtLeastOnce)?;
                    client.try_subscribe(format!("{discovery}/+/+/+/config"), QoS::AtLeastOnce)?;
                    // Retained: known by the time discovery has settled.
                    client.try_subscribe(format!("{base}/bridge/status"), QoS::AtLeastOnce)?;
                    let now = Instant::now();
                    states_by = now + Duration::from_millis(DISCOVERY_MAX_MS);
                    states_at = Some(now + Duration::from_millis(DISCOVERY_QUIET_MS));
                    ctx.ready();
                    tracing::info!(instance = %ctx.instance(), "connected to rfxcom2mqtt");
                }
                Event::Incoming(Packet::Publish(publish)) => {
                    let config = on_publish(ctx, base, discovery, &mut state, &publish);
                    if config && states_at.is_some() {
                        let quiet = Instant::now() + Duration::from_millis(DISCOVERY_QUIET_MS);
                        states_at = Some(quiet.min(states_by));
                    }
                }
                _ => {}
            },
            () = sleep_until(states_at), if states_at.is_some() => {
                states_at = None;
                if may_sweep(&state) {
                    sweep(ctx, &state.catalog);
                } else {
                    tracing::warn!(
                        devices = state.catalog.len(),
                        bridge_online = ?state.bridge_online,
                        "rfxcom2mqtt not online or no device announced: known devices kept"
                    );
                }
                client.try_subscribe(format!("{base}/devices/#"), QoS::AtLeastOnce)?;
                tracing::info!(devices = state.catalog.len(), "rfxcom2mqtt catalog loaded");
            },
            command = ctx.next_command() => {
                let Some(command) = command else {
                    return Ok(());
                };
                match decide_command(connected, &state.catalog, &command.device.id, &command.key, &command.value) {
                    CommandDecision::Refuse { reason } => command.reply(Err(reason)),
                    CommandDecision::Publish { topic, payload } => {
                        let result = client
                            .try_publish(topic, QoS::AtLeastOnce, false, payload)
                            .map_err(|e| format!("mqtt publish failed: {e}"));
                        command.reply(result);
                    }
                }
            }
        }
    }
}

/// Awaits the deadline (resolves at once if `None`).
async fn sleep_until(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    }
}

/// Whether the catalog is trustworthy enough to remove the devices it lacks:
/// the bridge said `online`, and announced at least one device.
fn may_sweep(state: &State) -> bool {
    state.bridge_online == Some(true) && !state.catalog.is_empty()
}

/// Removes the devices of a previous run the bridge no longer announces.
fn sweep(ctx: &DriverCtx, catalog: &Catalog) {
    for id in ctx.devices() {
        if catalog.by_id(&id).is_none() {
            ctx.remove_device(&id);
        }
    }
}

/// Mutable state of one driver run.
#[derive(Debug, Default)]
struct State {
    catalog: Catalog,
    /// The last `{base}/bridge/status` (`None`: not received yet): a
    /// device announced while the bridge is offline starts offline.
    bridge_online: Option<bool>,
}

/// The decision for one command: refuse it, or publish it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CommandDecision {
    Refuse { reason: String },
    Publish { topic: String, payload: String },
}

/// Pure decision for one command: the broker must be connected, the device
/// known, and the point a switch's `on`. A 433 MHz receiver never answers:
/// success means the order was handed to the bridge, not that it was heard.
fn decide_command(
    connected: bool,
    catalog: &Catalog,
    device: &DeviceId,
    key: &str,
    value: &Value,
) -> CommandDecision {
    if !connected {
        return CommandDecision::Refuse {
            reason: moli_i18n::tr!("pilotes.rfxcom2mqtt.broker_injoignable"),
        };
    }
    let Some(entry) = catalog.by_id(device) else {
        return CommandDecision::Refuse {
            reason: moli_i18n::tr!("pilotes.rfxcom2mqtt.appareil_inconnu"),
        };
    };
    match entry.encode(key, value) {
        Ok((topic, payload)) => CommandDecision::Publish { topic, payload },
        Err(e) => CommandDecision::Refuse {
            reason: e.to_string(),
        },
    }
}

/// How one incoming topic is routed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Routed {
    /// A discovery config: `{discovery}/{component}/…/config`.
    Config {
        component: String,
    },
    /// A device state: `{base}/devices/{rest}`.
    State {
        rest: String,
    },
    /// `{base}/bridge/status`.
    BridgeStatus,
    Ignored,
}

/// Pure topic classification (no IO).
fn route_topic(topic: &str, base: &str, discovery: &str) -> Routed {
    if let Some(rest) = topic.strip_prefix(base).and_then(|t| t.strip_prefix('/')) {
        if rest == "bridge/status" {
            return Routed::BridgeStatus;
        }
        if let Some(device) = rest.strip_prefix("devices/")
            && !device.is_empty()
        {
            return Routed::State {
                rest: device.to_owned(),
            };
        }
        return Routed::Ignored;
    }
    if let Some(rest) = topic
        .strip_prefix(discovery)
        .and_then(|t| t.strip_prefix('/'))
        .and_then(|t| t.strip_suffix("/config"))
        && let Some((component, _)) = rest.split_once('/')
    {
        return Routed::Config {
            component: component.to_owned(),
        };
    }
    Routed::Ignored
}

/// Routes one message; `true` for one of this bridge's discovery configs.
fn on_publish(
    ctx: &DriverCtx,
    base: &str,
    discovery: &str,
    state: &mut State,
    publish: &Publish,
) -> bool {
    match route_topic(&publish.topic, base, discovery) {
        Routed::Config { component } => {
            // Cheap filter before parsing: the other bridges' configs never
            // name this bridge's state topics.
            let marker = format!("\"{base}/devices/");
            if !publish.payload.is_empty() && !contains(&publish.payload, marker.as_bytes()) {
                return false;
            }
            match state.catalog.apply_config(
                ctx.instance(),
                base,
                &publish.topic,
                &component,
                &publish.payload,
            ) {
                Change::Upsert(device) => {
                    let id = device.id.clone();
                    ctx.upsert_device(device);
                    ctx.set_availability(&id, state.bridge_online.unwrap_or(true));
                }
                Change::Remove(id) => ctx.remove_device(&id),
                Change::None => {}
            }
            return true;
        }
        Routed::State { rest } => {
            if let Some(entry) = state.catalog.by_state(&rest) {
                for (key, value) in entry.decode(&publish.payload) {
                    ctx.set_state(&entry.device.id, &key, value);
                }
            }
        }
        Routed::BridgeStatus => {
            let Some(online) = parse_status(&publish.payload) else {
                return false;
            };
            state.bridge_online = Some(online);
            for entry in state.catalog.entries() {
                ctx.set_availability(&entry.device.id, online);
            }
        }
        Routed::Ignored => {}
    }
    false
}

/// `online` / `offline`, as rfxcom2mqtt publishes its status.
fn parse_status(payload: &[u8]) -> Option<bool> {
    match std::str::from_utf8(payload).ok()?.trim().trim_matches('"') {
        "online" => Some(true),
        "offline" => Some(false),
        _ => None,
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use moli_core::InstanceId;

    #[test]
    fn config_defaults() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 1883);
        assert_eq!(config.base_topic, "rfxcom2mqtt");
        assert_eq!(config.discovery_topic, "homeassistant");
        assert!(serde_json::from_str::<Config>(r#"{"surprise":1}"#).is_err());
    }

    #[test]
    fn topics_are_routed() {
        let route = |t| route_topic(t, "rfxcom2mqtt", "homeassistant");
        assert_eq!(
            route("homeassistant/sensor/0x5A01/temperature/config"),
            Routed::Config {
                component: "sensor".into()
            }
        );
        assert_eq!(
            route("homeassistant/switch/0x00ABCDE1/12/config"),
            Routed::Config {
                component: "switch".into()
            }
        );
        assert_eq!(
            route("rfxcom2mqtt/devices/0x00ABCDE1/12"),
            Routed::State {
                rest: "0x00ABCDE1/12".into()
            }
        );
        assert_eq!(route("rfxcom2mqtt/bridge/status"), Routed::BridgeStatus);
        // The bare `devices` topic (a bridge quirk), commands and the
        // bridge's other topics are not states.
        assert_eq!(route("rfxcom2mqtt/devices"), Routed::Ignored);
        assert_eq!(
            route("rfxcom2mqtt/cmd/lighting2/0/0x00ABCDE1/12"),
            Routed::Ignored
        );
        assert_eq!(route("rfxcom2mqtt/bridge/info"), Routed::Ignored);
        assert_eq!(route("zigbee2mqtt/bridge/state"), Routed::Ignored);
    }

    #[test]
    fn the_sweep_needs_an_online_bridge_and_a_device() {
        let config = br#"{"state_topic":"rfxcom2mqtt/devices/0x5A01","value_template":"{{ value_json.temperature }}","device":{"name":"x"}}"#;
        let mut announced = Catalog::default();
        announced.apply_config(
            &moli_core::InstanceId::from("rfxcom"),
            "rfxcom2mqtt",
            "homeassistant/sensor/0x5A01/temperature/config",
            "sensor",
            config,
        );
        let state = |catalog: &Catalog, bridge_online| State {
            catalog: catalog.clone(),
            bridge_online,
        };
        // A broker restarted without persistence: nothing announced.
        assert!(!may_sweep(&state(&Catalog::default(), Some(true))));
        assert!(!may_sweep(&state(&Catalog::default(), None)));
        // The bridge offline, or its status not received yet.
        assert!(!may_sweep(&state(&announced, Some(false))));
        assert!(!may_sweep(&state(&announced, None)));
        assert!(may_sweep(&state(&announced, Some(true))));
    }

    #[test]
    fn status_payloads() {
        assert_eq!(parse_status(b"online"), Some(true));
        assert_eq!(parse_status(b"offline"), Some(false));
        assert_eq!(parse_status(b"\"online\""), Some(true));
        assert_eq!(parse_status(b"maybe"), None);
    }

    #[test]
    fn commands_need_a_connection_a_device_and_a_switch() {
        let instance = InstanceId::from("rfxcom");
        let mut catalog = Catalog::default();
        catalog.apply_config(
            &instance,
            "rfxcom2mqtt",
            "homeassistant/switch/0x00ABCDE1/12/config",
            "switch",
            br#"{"state_topic":"rfxcom2mqtt/devices/0x00ABCDE1/12","value_template":"{{ value_json.command }}","command_topic":"rfxcom2mqtt/cmd/lighting2/0/0x00ABCDE1/12","payload_on":"On","payload_off":"Off","device":{"name":"AC_00ABCDE1"}}"#,
        );
        let id = DeviceId::new(&instance, "0x00ABCDE1/12");
        assert_eq!(
            decide_command(false, &catalog, &id, "on", &Value::Bool(true)),
            CommandDecision::Refuse {
                reason: moli_i18n::tr!("pilotes.rfxcom2mqtt.broker_injoignable")
            }
        );
        assert_eq!(
            decide_command(
                true,
                &catalog,
                &DeviceId::new(&instance, "0x01"),
                "on",
                &Value::Bool(true)
            ),
            CommandDecision::Refuse {
                reason: moli_i18n::tr!("pilotes.rfxcom2mqtt.appareil_inconnu")
            }
        );
        assert_eq!(
            decide_command(true, &catalog, &id, "on", &Value::Bool(true)),
            CommandDecision::Publish {
                topic: "rfxcom2mqtt/cmd/lighting2/0/0x00ABCDE1/12".into(),
                payload: "On".into()
            }
        );
        assert!(matches!(
            decide_command(true, &catalog, &id, "rssi", &Value::Int(3)),
            CommandDecision::Refuse { .. }
        ));
    }

    #[test]
    fn contains_finds_a_marker() {
        assert!(contains(
            br#"{"state_topic":"rfxcom2mqtt/devices/0x01"}"#,
            b"\"rfxcom2mqtt/devices/"
        ));
        assert!(!contains(
            br#"{"state_topic":"zigbee2mqtt/lampe"}"#,
            b"\"rfxcom2mqtt/devices/"
        ));
    }
}
