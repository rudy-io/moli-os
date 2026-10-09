//! Zigbee2MQTT driver.
//!
//! Subscribes to `{base}/#`, maps `bridge/devices` to Moli devices, state
//! messages to values and availability messages to online flags, and turns
//! commands into `{friendly_name}/set` publications.

mod catalog;

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use anyhow::Context as _;
use moli_core::DeviceId;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, Publish, QoS};
use serde::Deserialize;

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
                    let reloaded = on_publish(ctx, base, &mut catalog, &publish);
                    if reloaded {
                        queue_reads(base, &catalog, &mut read_done, &mut reads);
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
            command = ctx.next_command() => match command {
                Some(command) if !connected => command.reply(Err(UNREACHABLE.into())),
                Some(command) => on_command(&client, base, &catalog, command),
                None => return Ok(()),
            },
        }
    }
}

/// Handles one message. Returns `true` when the catalog was reloaded.
fn on_publish(ctx: &DriverCtx, base: &str, catalog: &mut Catalog, publish: &Publish) -> bool {
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
            ctx.set_availability(&entry.device.id, parse_online(&publish.payload));
        }
        return false;
    }
    let Some(entry) = catalog.by_name(rest) else {
        return false;
    };
    if let Ok(serde_json::Value::Object(payload)) = serde_json::from_slice(&publish.payload) {
        for (key, value) in entry.decode(&payload) {
            ctx.set_state(&entry.device.id, &key, value);
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

fn on_command(client: &AsyncClient, base: &str, catalog: &Catalog, command: CommandRequest) {
    let Some(entry) = catalog.by_id(&command.device.id) else {
        command.reply(Err("device no longer known to zigbee2mqtt".into()));
        return;
    };
    let payload = entry.encode(&command.key, &command.value).to_string();
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
}
