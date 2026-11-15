//! Jackery HomePower / SolarVault battery, through its local MQTT link.
//!
//! The battery is an MQTT client: in Jackery's app, the person gives it a
//! broker of the home (address, user, password) and gets a token. This
//! driver connects to the same broker, asks the battery for its state on
//! `{prefix}/device/{serial}/action` and reads the answers on `…/status` and
//! `…/event` ([`wire`]).
//!
//! What a winter with one taught:
//! - at rest the battery drops off the broker for minutes at a time, then
//!   comes back by itself. Its last values stay, the device goes offline
//!   after [`SILENCE`], and nothing here tries to wake it;
//! - asking often does not help: one round every [`ROUND`], one message
//!   every [`PACE`];
//! - a setting is written only when it changes: an order for the value the
//!   battery already reports is answered without sending anything.
//!
//! Read-only but for the reserve (the level under which the battery stops
//! feeding the home), bounded to what the battery accepts.

pub mod wire;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde::Deserialize;
use tokio::time::Instant;

/// The secret holding the token Jackery's app shows with the MQTT settings.
const TOKEN: &str = "token";
/// The secret holding the broker's password, when `username` is set.
const PASSWORD: &str = "password";
/// Between two rounds of reads.
const ROUND: Duration = Duration::from_secs(30);
/// Between two messages to the battery.
const PACE: Duration = Duration::from_secs(1);
/// Without any answer for this long, the battery is offline.
const SILENCE: Duration = Duration::from_secs(100);

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The broker the battery was given in Jackery's app.
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// The topics' first segment, as set in Jackery's app.
    #[serde(default = "default_prefix")]
    pub topic_prefix: String,
    /// The battery's serial number (on its label and in the app).
    pub serial: String,
    /// The broker's user; its password is the secret `password`.
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    1883
}
fn default_prefix() -> String {
    "hb".into()
}

/// A topic segment the person typed: no separator, no wildcard.
fn segment(name: &str, value: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.contains(['/', '+', '#']) || value.chars().any(char::is_whitespace)
    {
        bail!("{name} {value:?}: one topic segment, without '/', '+', '#' or spaces");
    }
    Ok(())
}

#[derive(Debug)]
pub struct Jackery {
    config: Config,
}

impl Jackery {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Jackery {
    fn kind(&self) -> &'static str {
        "jackery"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

fn numeric(
    key: &str,
    label: String,
    unit: Unit,
    semantic: Semantic,
    bounds: Option<(f64, f64)>,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Numeric {
            min: bounds.map(|(min, _)| min),
            max: bounds.map(|(_, max)| max),
            step: bounds.map(|_| 1.0),
        },
        access: Access {
            read: true,
            write: bounds.is_some(),
        },
        unit: Some(unit),
        semantic,
    }
}

fn device(id: DeviceId, ctx: &DriverCtx) -> Device {
    use moli_i18n::tr;
    let watts = |key: &str, label: String| numeric(key, label, Unit::Watt, Semantic::Power, None);
    Device {
        id,
        instance: ctx.instance().clone(),
        native_name: tr!("pilotes.jackery.nom").into(),
        manufacturer: Some("Jackery".into()),
        model: None,
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            numeric(
                "battery",
                tr!("pilotes.jackery.niveau"),
                Unit::Percent,
                Semantic::Battery,
                None,
            ),
            watts("charge_power", tr!("pilotes.jackery.charge")),
            watts("discharge_power", tr!("pilotes.jackery.decharge")),
            watts("solar_power", tr!("pilotes.jackery.solaire")),
            watts("grid_power", tr!("pilotes.jackery.reseau")),
            watts("home_power", tr!("pilotes.jackery.maison")),
            PointSpec {
                key: "status".into(),
                label: tr!("pilotes.jackery.etat").into(),
                kind: Kind::Enum {
                    values: wire::STATES.iter().map(|s| Arc::from(*s)).collect(),
                },
                access: Access {
                    read: true,
                    write: false,
                },
                unit: None,
                semantic: Semantic::Other,
            },
            // The one setting Moli writes: never under the battery's own
            // floor (the hub refuses an order out of these bounds).
            numeric(
                "reserve",
                tr!("pilotes.jackery.reserve"),
                Unit::Percent,
                Semantic::Config,
                Some((wire::RESERVE_MIN, wire::RESERVE_MAX)),
            ),
            numeric(
                "charge_limit",
                tr!("pilotes.jackery.limite_charge"),
                Unit::Percent,
                Semantic::Config,
                None,
            ),
            numeric(
                "max_output_power",
                tr!("pilotes.jackery.puissance_max"),
                Unit::Watt,
                Semantic::Config,
                None,
            ),
            numeric(
                "temperature",
                tr!("pilotes.jackery.temperature"),
                Unit::Celsius,
                Semantic::Temperature,
                None,
            ),
            numeric(
                "signal",
                tr!("pilotes.jackery.signal"),
                Unit::Other("dBm".into()),
                Semantic::SignalStrength,
                None,
            ),
            PointSpec {
                key: "packs".into(),
                label: tr!("pilotes.jackery.packs").into(),
                kind: Kind::Numeric {
                    min: None,
                    max: None,
                    step: None,
                },
                access: Access {
                    read: true,
                    write: false,
                },
                unit: None,
                semantic: Semantic::Other,
            },
        ],
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What an order becomes: nothing to send (already there), a message, or a
/// refusal.
#[derive(Debug, PartialEq)]
enum Order {
    AlreadyThere,
    Send(u8),
    Refuse(String),
}

/// Only the reserve is written, in whole percent, and only when it differs
/// from what the battery reports.
fn order(key: &str, value: &Value, reported: Option<&Value>) -> Order {
    if key != "reserve" {
        return Order::Refuse(moli_i18n::tr!("pilotes.jackery.lecture_seule"));
    }
    let Some(wanted) = value.as_f64().map(f64::round) else {
        return Order::Refuse(moli_i18n::tr!("pilotes.jackery.lecture_seule"));
    };
    // The hub checked the bounds; checked again where the message is made.
    if !(wire::RESERVE_MIN..=wire::RESERVE_MAX).contains(&wanted) {
        return Order::Refuse(moli_i18n::tr!("pilotes.jackery.hors_bornes"));
    }
    if reported.and_then(Value::as_f64).map(f64::round) == Some(wanted) {
        return Order::AlreadyThere;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Order::Send(wanted as u8)
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    segment("topic_prefix", &config.topic_prefix)?;
    segment("serial", &config.serial)?;
    let Some(token) = ctx.secret(TOKEN) else {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.jackery.jeton_manquant",
            instance = ctx.instance(),
            name = TOKEN
        ));
        ctx.cancelled().await;
        return Ok(());
    };
    let client_id = config
        .client_id
        .clone()
        .unwrap_or_else(|| format!("moli-os-{}", ctx.instance()));
    let mut options = MqttOptions::new(client_id, &config.host, config.port);
    options.set_keep_alive(Duration::from_secs(30));
    if let Some(username) = &config.username {
        options.set_credentials(username, ctx.secret(PASSWORD).unwrap_or_default());
    }
    let (client, mut events) = AsyncClient::new(options, 16);
    let base = format!("{}/device/{}", config.topic_prefix, config.serial);
    let action = format!("{base}/action");

    let id = ctx.device_id(&config.serial);
    ctx.upsert_device(device(id.clone(), ctx));

    // Until the broker accepts us, an order would only wait in the client's
    // queue and leave much later: refuse it. Any poll error ends the run,
    // so the flag never outlives a connection.
    let mut connected = false;
    let mut online = false;
    let mut heard = Instant::now();
    let mut message_id: u32 = 1000;
    let mut outbox: VecDeque<String> = VecDeque::new();
    let mut round = tokio::time::interval(ROUND);
    round.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pacer = tokio::time::interval(PACE);
    pacer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            event = events.poll() => match event.context("mqtt connection")? {
                Event::Incoming(Packet::ConnAck(_)) => {
                    connected = true;
                    client.try_subscribe(format!("{base}/status"), QoS::AtMostOnce)?;
                    client.try_subscribe(format!("{base}/event"), QoS::AtMostOnce)?;
                    tracing::info!(instance = %ctx.instance(), "connected to mqtt");
                    ctx.ready();
                }
                Event::Incoming(Packet::Publish(publish)) => {
                    if let Some(readings) = wire::readings(&publish.payload) {
                        for (key, value) in readings {
                            ctx.set_state(&id, key, value);
                        }
                        heard = Instant::now();
                        if !online {
                            online = true;
                            ctx.set_availability(&id, true);
                        }
                    }
                }
                _ => {}
            },
            _ = round.tick(), if connected => {
                if online && heard.elapsed() > SILENCE {
                    online = false;
                    ctx.set_availability(&id, false);
                }
                // A round still waiting means the broker is slow: never pile up.
                if outbox.is_empty() {
                    outbox.extend(wire::reads(&token, now(), message_id));
                    message_id = message_id.wrapping_add(2);
                }
            },
            _ = pacer.tick(), if connected && !outbox.is_empty() => {
                if let Some(message) = outbox.pop_front()
                    && client.try_publish(&action, QoS::AtMostOnce, false, message.clone()).is_err()
                {
                    outbox.push_front(message);
                }
            },
            command = ctx.next_command() => match command {
                None => return Ok(()),
                Some(command) if !connected || !online => {
                    command.reply(Err(moli_i18n::tr!("pilotes.jackery.injoignable")));
                }
                Some(command) => {
                    on_command(ctx, &client, &action, &token, &mut message_id, &mut outbox, command);
                }
            },
        }
    }
}

fn on_command(
    ctx: &DriverCtx,
    client: &AsyncClient,
    action: &str,
    token: &str,
    message_id: &mut u32,
    outbox: &mut VecDeque<String>,
    command: CommandRequest,
) {
    let reported = ctx.current(&command.device.id, "reserve");
    match order(&command.key, &command.value, reported.as_ref()) {
        Order::Refuse(why) => command.reply(Err(why)),
        Order::AlreadyThere => command.reply(Ok(())),
        Order::Send(percent) => {
            let message = wire::set_reserve(percent, token, now(), *message_id);
            *message_id = message_id.wrapping_add(1);
            if client
                .try_publish(action, QoS::AtMostOnce, false, message)
                .is_err()
            {
                command.reply(Err(moli_i18n::tr!("pilotes.jackery.injoignable")));
                return;
            }
            // Read the settings back next: the dashboard shows what the
            // battery did, not what was asked.
            let [_, settings] = wire::reads(token, now(), *message_id);
            *message_id = message_id.wrapping_add(2);
            outbox.push_front(settings);
            command.reply(Ok(()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserve_is_written_only_when_it_changes() {
        let at = |v: f64| Value::Float(v);
        assert_eq!(
            order("reserve", &at(15.0), Some(&at(24.0))),
            Order::Send(15)
        );
        assert_eq!(
            order("reserve", &at(15.4), Some(&at(10.0))),
            Order::Send(15)
        );
        // What the battery already reports: nothing leaves.
        assert_eq!(
            order("reserve", &at(24.0), Some(&at(24.0))),
            Order::AlreadyThere
        );
        // Never reported yet: sent.
        assert_eq!(order("reserve", &at(24.0), None), Order::Send(24));
    }

    #[test]
    fn nothing_else_is_written_and_the_floor_holds() {
        let at = |v: f64| Value::Float(v);
        for key in ["charge_limit", "max_output_power", "battery", "status"] {
            assert!(matches!(order(key, &at(50.0), None), Order::Refuse(_)));
        }
        for out in [4.0, 0.0, -1.0, 50.0, 100.0] {
            assert!(
                matches!(order("reserve", &at(out), None), Order::Refuse(_)),
                "{out}"
            );
        }
        assert!(matches!(
            order("reserve", &Value::Text("15".into()), None),
            Order::Refuse(_)
        ));
    }

    #[test]
    fn topic_segments_cannot_reach_other_topics() {
        assert!(segment("serial", "AB12C00000000XX").is_ok());
        for bad in ["", "a/b", "+", "#", "a b"] {
            assert!(segment("serial", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_configuration_has_defaults_and_refuses_unknown_keys() {
        let config: Config = serde_json::from_str(r#"{"serial":"AB12C00000000XX"}"#).unwrap();
        assert_eq!(
            (
                config.host.as_str(),
                config.port,
                config.topic_prefix.as_str()
            ),
            ("127.0.0.1", 1883, "hb")
        );
        assert!(serde_json::from_str::<Config>(r#"{"serial":"x","token":"t"}"#).is_err());
        assert!(serde_json::from_str::<Config>("{}").is_err());
    }
}
