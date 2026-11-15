//! Govee lights, 100 % local, through Govee's official LAN API (UDP): power,
//! brightness, colour and white temperature. No cloud, no account, no key.
//!
//! One instance runs every light it finds: by a `scan` sent to the API's
//! multicast group unless `discovery = false`, and at the addresses of
//! `hosts`. A light is known by its identifier, never by its address.
//!
//! The LAN API must be switched on light by light in the Govee app, and not
//! every model has it. It has no authentication, no request number and no
//! acknowledgement (see `lights` and `wire`): the driver ties an answer to a
//! light by the address it comes from, sends an order only to a light that
//! answered recently, and reads the state back after each order, so that
//! what is published is what the light did, not what was asked.
//!
//! Split in three: `wire` (the messages), `lights` (what the driver knows
//! and decides), both without a socket, and the loop below.

mod lights;
mod wire;

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use tokio::net::UdpSocket;
use tokio::time::Instant;

use lights::{Event, Light, Lights};
use wire::{Order, Reply, Rgb, Status};

/// Polls without an answer before a light is shown offline: one datagram
/// lost is not a light gone.
const MISSES: u32 = 3;
/// A new search while a light is missing or offline, or none is known…
const SEARCH_MISSING: Duration = Duration::from_secs(60);
/// …and otherwise, for new lights and new addresses.
const SEARCH_AGAIN: Duration = Duration::from_secs(300);
/// How long the first search listens, twice if nothing answered.
const FIRST_SEARCH: Duration = Duration::from_secs(1);
/// Lights one instance runs at most.
const MAX_LIGHTS: usize = 100;
/// After an order: the time left to the light before its state is asked…
const SETTLE: Duration = Duration::from_millis(300);
/// …and how long its answer is waited for. An order is sent twice at most
/// and read three times: 2.7 s in all, within the hub's 5 s.
const READ: Duration = Duration::from_millis(700);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Addresses asked directly (lights with a fixed address, or on a
    /// network the multicast search does not reach).
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Also look for lights with a multicast search on the local network.
    #[serde(default = "yes")]
    pub discovery: bool,
    /// Seconds between two readings of each light.
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
}

fn yes() -> bool {
    true
}

fn default_poll() -> u64 {
    5
}

#[derive(Debug)]
pub struct Govee {
    hosts: Vec<Ipv4Addr>,
    discovery: bool,
    poll: Duration,
}

impl Govee {
    /// # Errors
    /// An address that is not IPv4, nothing to look for, or a poll period
    /// out of bounds.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let Config {
            hosts,
            discovery,
            poll_secs,
        } = config;
        let hosts = hosts
            .iter()
            .map(|h| {
                h.trim()
                    .parse::<Ipv4Addr>()
                    .with_context(|| moli_i18n::tr!("pilotes.govee.adresse_invalide", host = h))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        if hosts.is_empty() && !discovery {
            bail!(moli_i18n::tr!("pilotes.govee.rien_a_chercher"));
        }
        if !(1..=3600).contains(&poll_secs) {
            bail!(moli_i18n::tr!("pilotes.govee.poll_secs"));
        }
        Ok(Self {
            hosts,
            discovery,
            poll: Duration::from_secs(poll_secs),
        })
    }
}

impl Driver for Govee {
    fn kind(&self) -> &'static str {
        "govee"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

/// The order a command asks, if a light takes it.
fn order(key: &str, value: &Value) -> Result<Order, String> {
    let refuse = || moli_i18n::tr!("pilotes.govee.ne_se_regle_pas", point = key);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let within = |min: u16, max: u16| {
        value
            .as_f64()
            .filter(|n| n.is_finite())
            .map(|n| n.round().clamp(f64::from(min), f64::from(max)) as u16)
            .ok_or_else(refuse)
    };
    match key {
        "on" => match *value {
            Value::Bool(on) => Ok(Order::Turn(on)),
            _ => Err(refuse()),
        },
        "brightness" => {
            let percent = within(1, 100)?;
            Ok(Order::Brightness(u8::try_from(percent).unwrap_or(100)))
        }
        "color_temp" => Ok(Order::Kelvin(within(wire::KELVIN.0, wire::KELVIN.1)?)),
        "color" => {
            let text = match value {
                Value::Text(text) => Some(&**text),
                _ => None,
            };
            text.and_then(Rgb::parse)
                .map(Order::Color)
                .ok_or_else(|| moli_i18n::tr!("pilotes.govee.couleur_invalide"))
        }
        _ => Err(refuse()),
    }
}

/// A reading as the values of the points. A field the light left out is
/// not published, nor a temperature of 0 (the light shows a colour): the
/// last white stays.
fn values(status: &Status) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    if let Some(on) = status.on {
        out.push(("on", Value::Bool(on)));
    }
    if let Some(percent) = status.brightness {
        out.push(("brightness", Value::Int(i64::from(percent))));
    }
    if let Some(kelvin) = status.kelvin.filter(|k| *k > 0) {
        out.push(("color_temp", Value::Int(i64::from(kelvin))));
    }
    if let Some(color) = status.color {
        out.push(("color", Value::from(color.hex().as_str())));
    }
    out
}

/// The points of every light, under the keys and meanings of the other
/// light drivers. The LAN API does not say what a model can do: all four
/// are offered.
fn points() -> Vec<PointSpec> {
    let spec =
        |key: &str, label: String, kind: Kind, unit: Option<Unit>, semantic: Semantic| PointSpec {
            key: key.into(),
            label: label.into(),
            kind,
            access: Access {
                read: true,
                write: true,
            },
            unit,
            semantic,
        };
    let range = |min: f64, max: f64| Kind::Numeric {
        min: Some(min),
        max: Some(max),
        step: Some(1.0),
    };
    vec![
        spec(
            "on",
            moli_i18n::tr!("pilotes.govee.allumee"),
            Kind::Binary,
            None,
            Semantic::OnOff,
        ),
        spec(
            "brightness",
            moli_i18n::tr!("pilotes.govee.luminosite"),
            range(1.0, 100.0),
            Some(Unit::Percent),
            Semantic::Brightness,
        ),
        spec(
            "color_temp",
            moli_i18n::tr!("pilotes.govee.temperature_couleur"),
            range(f64::from(wire::KELVIN.0), f64::from(wire::KELVIN.1)),
            Unit::parse("K"),
            Semantic::ColorTemp,
        ),
        spec(
            "color",
            moli_i18n::tr!("pilotes.govee.couleur"),
            Kind::Text,
            None,
            Semantic::Color,
        ),
    ]
}

/// The LAN API gives a light no name: its model and the end of its
/// identifier tell two lights apart.
fn name(light: &Light) -> String {
    let end = &light.native[light.native.len().saturating_sub(4)..];
    if light.sku.is_empty() {
        moli_i18n::tr!("pilotes.govee.lumiere", end = end)
    } else {
        moli_i18n::tr!("pilotes.govee.lumiere_modele", sku = light.sku, end = end)
    }
}

fn device(ctx: &DriverCtx, light: &Light) -> Device {
    Device {
        id: ctx.device_id(&light.native),
        instance: ctx.instance().clone(),
        native_name: name(light).into(),
        manufacturer: Some("Govee".into()),
        model: (!light.sku.is_empty()).then(|| light.sku.as_str().into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: points(),
    }
}

/// The socket and what it serves.
struct Session<'a> {
    govee: &'a Govee,
    socket: UdpSocket,
    lights: Lights,
}

impl Session<'_> {
    async fn send(&self, to: Ipv4Addr, port: u16, message: &[u8]) {
        if let Err(error) = self.socket.send_to(message, (to, port)).await {
            tracing::debug!(%to, %error, "govee datagram not sent");
        }
    }

    /// Asks who is there: the multicast group, and each address of `hosts`.
    async fn search(&mut self, ctx: &DriverCtx) {
        self.lights.full = false;
        let scan = wire::scan();
        if self.govee.discovery {
            self.send(wire::GROUP, wire::SCAN_PORT, &scan).await;
        }
        for host in &self.govee.hosts {
            self.send(*host, wire::SCAN_PORT, &scan).await;
        }
        ctx.clear_warning();
    }

    async fn apply(&self, ctx: &DriverCtx, events: Vec<Event>) {
        for event in events {
            match event {
                Event::Publish(native) => {
                    if let Some(light) = self.lights.get(&native) {
                        ctx.upsert_device(device(ctx, light));
                        if !light.online {
                            ctx.set_availability(&ctx.device_id(&native), false);
                        }
                        tracing::info!(instance = %ctx.instance(), light = %native, "govee light met");
                    }
                }
                Event::Ask(at) => {
                    self.send(at, wire::COMMAND_PORT, &wire::status_request())
                        .await;
                }
                Event::Online(native, online) => {
                    ctx.set_availability(&ctx.device_id(&native), online);
                    if !online {
                        tracing::warn!(instance = %ctx.instance(), light = %native, "govee unreachable");
                    }
                }
                Event::State(native, status) => {
                    let id = ctx.device_id(&native);
                    for (key, value) in values(&status) {
                        ctx.set_state(&id, key, value);
                    }
                }
                Event::Moved(native, at) => {
                    tracing::info!(instance = %ctx.instance(), light = %native, new = %at, "govee found at a new address");
                }
            }
        }
    }

    /// One datagram heard. The address and the reading, if it is the state
    /// of a light known there.
    async fn datagram(
        &mut self,
        ctx: &DriverCtx,
        bytes: &[u8],
        from: SocketAddr,
    ) -> Option<(Ipv4Addr, Status)> {
        let SocketAddr::V4(from) = from else {
            return None;
        };
        let from = *from.ip();
        match wire::decode(bytes)? {
            Reply::Scan(found) => {
                let events = self.lights.heard(from, &found);
                self.apply(ctx, events).await;
                if self.lights.full {
                    ctx.warn(moli_i18n::tr!("pilotes.govee.trop", max = MAX_LIGHTS));
                }
                None
            }
            Reply::Status(status) => {
                let events = self.lights.status(from, status, Instant::now());
                let known = !events.is_empty();
                self.apply(ctx, events).await;
                known.then_some((from, status))
            }
        }
    }

    /// Serves what arrives until `until`; stops at once with the state of
    /// the light at `wanted`, if one is asked and comes.
    async fn listen(
        &mut self,
        ctx: &DriverCtx,
        until: Instant,
        wanted: Option<Ipv4Addr>,
    ) -> Option<Status> {
        let mut buf = [0u8; 2048];
        loop {
            match tokio::time::timeout_at(until, self.socket.recv_from(&mut buf)).await {
                Err(_) => return None,
                Ok(Ok((n, from))) => {
                    if let Some((at, status)) = self.datagram(ctx, &buf[..n], from).await
                        && Some(at) == wanted
                    {
                        return Some(status);
                    }
                }
                // Windows reports here a datagram refused by its target.
                Ok(Err(_)) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    }

    /// Carries out an order: refused for a light that did not answer
    /// recently, else sent, and the state read back. The order has no
    /// acknowledgement, so it is sent once more if the first reading does
    /// not show it. Done once the light answered after it: what it then
    /// says is published, whether or not it is what was asked.
    async fn order(&mut self, ctx: &DriverCtx, native: &str, order: Order) -> Result<(), String> {
        let at = self
            .lights
            .target(native, Instant::now())
            .ok_or_else(|| moli_i18n::tr!("pilotes.govee.injoignable"))?;
        let message = order.message();
        let mut answered = false;
        for round in 0..3 {
            if round < 2 {
                self.send(at, wire::COMMAND_PORT, &message).await;
                self.listen(ctx, Instant::now() + SETTLE, None).await;
            }
            self.send(at, wire::COMMAND_PORT, &wire::status_request())
                .await;
            if let Some(status) = self.listen(ctx, Instant::now() + READ, Some(at)).await {
                answered = true;
                if order.done(&status) {
                    return Ok(());
                }
            }
        }
        if answered {
            Ok(())
        } else {
            Err(moli_i18n::tr!("pilotes.govee.pas_de_reponse"))
        }
    }

    /// Whether to search again soon: see [`Lights::missing`], or a light
    /// published by an earlier run not met in this one.
    fn missing(&self, ctx: &DriverCtx, known: &BTreeSet<DeviceId>) -> bool {
        let met: BTreeSet<DeviceId> = self
            .lights
            .iter()
            .map(|l| ctx.device_id(&l.native))
            .collect();
        self.lights.missing() || !known.is_subset(&met)
    }

    fn status(&self, ctx: &DriverCtx) {
        if self.lights.is_empty() {
            ctx.wait_for(moli_i18n::tr!("pilotes.govee.aucune"));
        } else {
            ctx.ready();
        }
    }
}

async fn run(govee: &Govee, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    // Restored readings are stale until a light answers in this run.
    let known: BTreeSet<DeviceId> = ctx.known_devices().into_iter().collect();
    for id in &known {
        ctx.set_availability(id, false);
    }
    // Lights answer to this port and no other.
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, wire::LISTEN_PORT))
        .await
        .with_context(|| moli_i18n::tr!("pilotes.govee.port_occupe", port = wire::LISTEN_PORT))?;
    let mut session = Session {
        govee,
        socket,
        lights: Lights::new(
            govee.hosts.clone(),
            govee.discovery,
            MAX_LIGHTS,
            MISSES,
            govee.poll * MISSES + Duration::from_secs(1),
        ),
    };
    // The first search's lights are met before saying ready or waiting.
    for _ in 0..2 {
        if session.lights.is_empty() {
            session.search(ctx).await;
            session
                .listen(ctx, Instant::now() + FIRST_SEARCH, None)
                .await;
        }
    }
    session.status(ctx);
    let after = |session: &Session, ctx: &DriverCtx| {
        Instant::now()
            + if session.missing(ctx, &known) {
                SEARCH_MISSING
            } else {
                SEARCH_AGAIN
            }
    };
    let mut next_search = after(&session, ctx);
    let mut tick = tokio::time::interval(govee.poll);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut buf = [0u8; 2048];
    loop {
        tokio::select! {
            // Orders first, then the search when it is due, then the poll
            // (datagrams sent, nothing waited for), then what arrives.
            biased;
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                // <instance>:<identifier>, and an instance has no ':'.
                let id = command.device.id.as_str();
                let native = id.split_once(':').map_or(id, |(_, native)| native).to_owned();
                let result = match order(&command.key, &command.value) {
                    Ok(order) => session.order(ctx, &native, order).await,
                    Err(refusal) => Err(refusal),
                };
                command.reply(result);
            }
            () = tokio::time::sleep_until(next_search) => {
                session.search(ctx).await;
                next_search = after(&session, ctx);
            }
            _ = tick.tick() => {
                let before = session.lights.missing();
                let events = session.lights.poll();
                session.apply(ctx, events).await;
                // A light just gone offline brings the search forward.
                if !before && session.lights.missing() {
                    next_search = next_search.min(Instant::now() + SEARCH_MISSING);
                }
            }
            got = session.socket.recv_from(&mut buf) => {
                match got {
                    Ok((n, from)) => {
                        session.datagram(ctx, &buf[..n], from).await;
                        session.status(ctx);
                    }
                    // Windows reports here a datagram refused by its target.
                    Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> anyhow::Result<Govee> {
        Govee::new(toml::from_str(toml)?)
    }

    #[test]
    fn options_are_checked() {
        let govee = config("").unwrap();
        assert!(govee.discovery && govee.hosts.is_empty());
        assert_eq!(govee.poll, Duration::from_secs(5));
        let govee = config(
            r#"hosts = ["192.168.77.10", " 192.168.77.11 "]
               discovery = false
               poll_secs = 30"#,
        )
        .unwrap();
        assert_eq!(
            govee.hosts,
            [
                Ipv4Addr::new(192, 168, 77, 10),
                Ipv4Addr::new(192, 168, 77, 11)
            ]
        );
        assert!(!govee.discovery);
        assert!(config("discovery = false").is_err());
        assert!(config(r#"hosts = ["lampe.local"]"#).is_err());
        assert!(config("poll_secs = 0").is_err());
        assert!(config("poll_secs = 3601").is_err());
        assert!(config("colour = true").is_err());
    }

    #[test]
    fn commands_become_orders() {
        assert_eq!(order("on", &Value::Bool(false)), Ok(Order::Turn(false)));
        assert!(order("on", &Value::Int(1)).is_err());
        assert_eq!(
            order("brightness", &Value::Int(40)),
            Ok(Order::Brightness(40))
        );
        // Bounded to what the light takes.
        assert_eq!(
            order("brightness", &Value::Int(0)),
            Ok(Order::Brightness(1))
        );
        assert_eq!(
            order("brightness", &Value::Int(250)),
            Ok(Order::Brightness(100))
        );
        assert_eq!(
            order("color_temp", &Value::Int(2700)),
            Ok(Order::Kelvin(2700))
        );
        assert_eq!(
            order("color_temp", &Value::Int(500)),
            Ok(Order::Kelvin(2000))
        );
        assert_eq!(
            order("color_temp", &Value::Int(12_000)),
            Ok(Order::Kelvin(9000))
        );
        assert_eq!(
            order("color", &Value::from("#ff0000")),
            Ok(Order::Color(Rgb { r: 255, g: 0, b: 0 }))
        );
        assert!(order("color", &Value::from("rouge")).is_err());
        assert!(order("color", &Value::Int(3)).is_err());
        assert!(order("brightness", &Value::from("fort")).is_err());
        assert!(order("rssi", &Value::Int(-50)).is_err());
    }

    #[test]
    fn a_reading_becomes_values() {
        let white = Status {
            on: Some(true),
            brightness: Some(80),
            color: Some(Rgb {
                r: 255,
                g: 214,
                b: 170,
            }),
            kelvin: Some(2700),
        };
        assert_eq!(
            values(&white),
            [
                ("on", Value::Bool(true)),
                ("brightness", Value::Int(80)),
                ("color_temp", Value::Int(2700)),
                ("color", Value::from("#ffd6aa")),
            ]
        );
        // A colour: no temperature published, the last white stays.
        let red = Status {
            color: Some(Rgb { r: 255, g: 0, b: 0 }),
            kelvin: Some(0),
            ..white
        };
        let keys: Vec<_> = values(&red).into_iter().map(|(key, _)| key).collect();
        assert_eq!(keys, ["on", "brightness", "color"]);
        // Only what the light said.
        let partial = Status {
            on: Some(false),
            ..Status::default()
        };
        assert_eq!(values(&partial), [("on", Value::Bool(false))]);
    }

    #[test]
    fn the_points_are_those_of_the_other_lights() {
        let points = points();
        let seen: Vec<_> = points
            .iter()
            .map(|p| (&*p.key, p.semantic, p.access.write))
            .collect();
        assert_eq!(
            seen,
            [
                ("on", Semantic::OnOff, true),
                ("brightness", Semantic::Brightness, true),
                ("color_temp", Semantic::ColorTemp, true),
                ("color", Semantic::Color, true),
            ]
        );
    }

    #[test]
    fn two_lights_of_a_model_have_two_names() {
        let light = |native: &str, sku: &str| Light {
            native: native.into(),
            sku: sku.into(),
            at: None,
            online: false,
            unanswered: 0,
            answered: None,
        };
        let first = name(&light("aabbccddeeff0001", "H6163"));
        assert!(first.contains("H6163") && first.contains("0001"), "{first}");
        assert_ne!(first, name(&light("aabbccddeeff0002", "H6163")));
        assert!(name(&light("aabbccddeeff0001", "")).contains("0001"));
    }
}
