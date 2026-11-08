//! LIFX Wi-Fi lights, 100 % local, through LIFX's LAN protocol (UDP
//! 56700): power, brightness, and colour temperature or colour where the
//! model has them. No account, no secret.
//!
//! One instance runs every bulb it finds: at the addresses of `hosts`, and
//! by a `GetService` broadcast unless `discovery = false`. A bulb is known
//! by its serial number, never by its address: one that moves is found
//! again by the next search.

mod products;
mod proto;

use std::collections::{BTreeSet, HashMap};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::Deserialize;
use tokio::net::UdpSocket;
use tokio::time::Instant;

use proto::{Hsbk, LightState, Message, Reply, Serial};

/// How long a bulb has to answer one message.
const ANSWER: Duration = Duration::from_millis(500);
/// Sendings of an order or of a first question (a lost datagram is
/// common). A poll is sent once: the next poll is its retry.
const TRIES: u8 = 2;
/// How long a search listens, in two rounds of messages.
const SEARCH_ROUND: Duration = Duration::from_millis(600);
/// Consecutive missed polls before a bulb is shown offline: a LIFX bulb
/// often misses one and answers the next.
const MISSES: u32 = 3;
/// An offline bulb is asked once every this many polls (it would delay
/// the others' and the orders' turn at each one).
const OFFLINE_EVERY: u32 = 6;
/// A new search while a bulb is missing or offline…
const SEARCH_MISSING: Duration = Duration::from_secs(60);
/// …and otherwise, for new bulbs and new addresses.
const SEARCH_AGAIN: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Addresses asked directly (bulbs with a fixed address, or on a network
    /// the broadcast does not reach).
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Also look for bulbs with a broadcast on the local network.
    #[serde(default = "yes")]
    pub discovery: bool,
    /// Seconds between two readings of each bulb.
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
    /// Fade, in milliseconds, of every change (power, brightness, colour).
    #[serde(default)]
    pub transition_ms: u32,
}

fn yes() -> bool {
    true
}

fn default_poll() -> u64 {
    5
}

#[derive(Debug)]
pub struct Lifx {
    hosts: Vec<Ipv4Addr>,
    discovery: bool,
    poll: Duration,
    transition: u32,
    /// Where bulbs listen: [`proto::PORT`] (another port in tests only).
    port: u16,
    search_missing: Duration,
    search_again: Duration,
}

impl Lifx {
    /// # Errors
    /// An address that is not IPv4, nothing to look for, or a poll period
    /// out of 1-3600 s.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let Config {
            hosts,
            discovery,
            poll_secs,
            transition_ms,
        } = config;
        let hosts = hosts
            .iter()
            .map(|h| {
                h.trim()
                    .parse::<Ipv4Addr>()
                    .with_context(|| moli_i18n::tr!("pilotes.lifx.adresse_invalide", host = h))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        if hosts.is_empty() && !discovery {
            bail!(moli_i18n::tr!("pilotes.lifx.rien_a_chercher"));
        }
        if !(1..=3600).contains(&poll_secs) {
            bail!(moli_i18n::tr!("pilotes.lifx.poll_secs"));
        }
        Ok(Self {
            hosts,
            discovery,
            poll: Duration::from_secs(poll_secs),
            transition: transition_ms,
            port: proto::PORT,
            search_missing: SEARCH_MISSING,
            search_again: SEARCH_AGAIN,
        })
    }
}

impl Driver for Lifx {
    fn kind(&self) -> &'static str {
        "lifx"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

fn serial_hex(serial: Serial) -> String {
    serial.iter().fold(String::with_capacity(12), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The socket every bulb is reached through, and who we are to them.
struct Link {
    socket: UdpSocket,
    source: u32,
    sequence: u8,
}

impl Link {
    async fn open() -> anyhow::Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
        socket.set_broadcast(true)?;
        let mut random = [0u8; 4];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| anyhow::anyhow!("no randomness"))?;
        // 0 and 1 have a meaning of their own to a bulb (answer everyone).
        let source = u32::from_le_bytes(random).max(2);
        Ok(Self {
            socket,
            source,
            sequence: 0,
        })
    }

    fn next(&mut self) -> u8 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }

    /// One message to one bulb and its answer of type `expect`, from that
    /// serial only; sent up to `tries` times while nothing comes back.
    async fn ask(
        &mut self,
        to: SocketAddrV4,
        target: Serial,
        (kind, payload): (u16, &[u8]),
        reply: Reply,
        expect: u16,
        tries: u8,
    ) -> Option<Message> {
        for _ in 0..tries {
            let sequence = self.next();
            let frame = proto::encode(self.source, Some(target), sequence, kind, payload, reply);
            if self.socket.send_to(&frame, to).await.is_err() {
                continue;
            }
            let deadline = Instant::now() + ANSWER;
            let mut buf = [0u8; 1024];
            while let Ok(Ok((n, _))) =
                tokio::time::timeout_at(deadline, self.socket.recv_from(&mut buf)).await
            {
                if let Some(m) = proto::decode(&buf[..n])
                    && m.source == self.source
                    && m.sequence == sequence
                    && m.target == target
                    && m.kind == expect
                {
                    return Some(m);
                }
            }
        }
        None
    }

    /// Who answers `GetService` at `hosts`, and to the broadcast if asked:
    /// each bulb's serial and where to reach it.
    async fn search(
        &mut self,
        hosts: &[Ipv4Addr],
        broadcast: bool,
        port: u16,
    ) -> HashMap<Serial, SocketAddrV4> {
        let mut found = HashMap::new();
        let sequence = self.next();
        let frame = proto::encode(
            self.source,
            None,
            sequence,
            proto::GET_SERVICE,
            &[],
            Reply::Response,
        );
        let mut to: Vec<Ipv4Addr> = hosts.to_vec();
        if broadcast {
            to.push(Ipv4Addr::BROADCAST);
        }
        for _ in 0..2 {
            for ip in &to {
                let _ = self.socket.send_to(&frame, (*ip, port)).await;
            }
            let deadline = Instant::now() + SEARCH_ROUND;
            let mut buf = [0u8; 1024];
            while let Ok(Ok((n, from))) =
                tokio::time::timeout_at(deadline, self.socket.recv_from(&mut buf)).await
            {
                let (Some(m), SocketAddr::V4(from)) = (proto::decode(&buf[..n]), from) else {
                    continue;
                };
                if m.source != self.source
                    || m.sequence != sequence
                    || m.kind != proto::STATE_SERVICE
                {
                    continue;
                }
                if let Some(port) = proto::udp_port(&m.payload) {
                    found.insert(m.target, SocketAddrV4::new(*from.ip(), port));
                }
            }
        }
        found
    }
}

/// What a bulb said when first met.
struct Found {
    product: Option<&'static products::Product>,
    pid: Option<u32>,
    firmware: Option<(u16, u16)>,
    group: Option<String>,
    rssi: Option<i64>,
    state: LightState,
}

async fn identify(link: &mut Link, serial: Serial, at: SocketAddrV4) -> Option<Found> {
    let state = link
        .ask(
            at,
            serial,
            (proto::LIGHT_GET, &[]),
            Reply::Response,
            proto::LIGHT_STATE,
            TRIES,
        )
        .await
        .and_then(|m| proto::light_state(&m.payload))?;
    let version = link
        .ask(
            at,
            serial,
            (proto::GET_VERSION, &[]),
            Reply::Response,
            proto::STATE_VERSION,
            TRIES,
        )
        .await
        .and_then(|m| proto::version(&m.payload));
    let firmware = link
        .ask(
            at,
            serial,
            (proto::GET_HOST_FIRMWARE, &[]),
            Reply::Response,
            proto::STATE_HOST_FIRMWARE,
            TRIES,
        )
        .await
        .and_then(|m| proto::firmware(&m.payload));
    let group = link
        .ask(
            at,
            serial,
            (proto::GET_GROUP, &[]),
            Reply::Response,
            proto::STATE_GROUP,
            TRIES,
        )
        .await
        .and_then(|m| proto::group(&m.payload));
    let rssi = signal(link, serial, at).await;
    Some(Found {
        product: version.and_then(|(vendor, pid)| products::find(vendor, pid)),
        pid: version.map(|(_, pid)| pid),
        firmware,
        group,
        rssi,
        state,
    })
}

/// The bulb's Wi-Fi signal in dBm, asked once (read again at each search).
async fn signal(link: &mut Link, serial: Serial, at: SocketAddrV4) -> Option<i64> {
    link.ask(
        at,
        serial,
        (proto::GET_WIFI_INFO, &[]),
        Reply::Response,
        proto::STATE_WIFI_INFO,
        1,
    )
    .await
    .and_then(|m| proto::rssi(&m.payload))
}

/// A bulb this run talks to.
struct Bulb {
    serial: Serial,
    id: DeviceId,
    at: SocketAddrV4,
    /// Whether it takes a colour.
    color: bool,
    /// Its kelvin range, if the temperature can be set.
    kelvin: Option<(u16, u16)>,
    name: String,
    room: Option<String>,
    model: String,
    /// Its colour as last read or set.
    last: Option<Hsbk>,
    misses: u32,
    online: bool,
    polls: u32,
}

impl Bulb {
    fn new(ctx: &DriverCtx, serial: Serial, at: SocketAddrV4, found: &Found) -> Self {
        let native = serial_hex(serial);
        let (color, kelvin) = match found.product {
            Some(p) => {
                let (min, max) = p.kelvin(found.firmware);
                (p.color, (min < max).then_some((min, max)))
            }
            None => (false, None),
        };
        let model = match (found.product, found.pid) {
            (Some(p), _) => p.name.to_owned(),
            (None, Some(pid)) => moli_i18n::tr!("pilotes.lifx.produit", pid = pid),
            (None, None) => "LIFX".to_owned(),
        };
        Self {
            serial,
            id: ctx.device_id(&native),
            at,
            color,
            kelvin,
            name: found
                .state
                .label
                .clone()
                .unwrap_or_else(|| moli_i18n::tr!("pilotes.lifx.ampoule", serial = native)),
            room: found.group.clone(),
            model,
            last: Some(found.state.color),
            misses: 0,
            online: true,
            polls: 0,
        }
    }

    fn device(&self, ctx: &DriverCtx) -> Device {
        let mut points = vec![
            spec(
                "on",
                &moli_i18n::tr!("pilotes.lifx.allumee"),
                Kind::Binary,
                None,
                Semantic::OnOff,
            ),
            spec(
                "brightness",
                &moli_i18n::tr!("pilotes.lifx.luminosite"),
                range(1.0, 100.0),
                Some(Unit::Percent),
                Semantic::Brightness,
            ),
        ];
        if let Some((min, max)) = self.kelvin {
            points.push(spec(
                "color_temp",
                &moli_i18n::tr!("pilotes.lifx.temperature_couleur"),
                range(f64::from(min), f64::from(max)),
                Unit::parse("K"),
                Semantic::ColorTemp,
            ));
        }
        if self.color {
            points.push(spec(
                "color",
                &moli_i18n::tr!("pilotes.lifx.couleur"),
                Kind::Text,
                None,
                Semantic::Color,
            ));
        }
        let mut signal = spec(
            "rssi",
            &moli_i18n::tr!("pilotes.lifx.signal"),
            Kind::Numeric {
                min: None,
                max: None,
                step: Some(1.0),
            },
            Unit::parse("dBm"),
            Semantic::SignalStrength,
        );
        signal.access.write = false;
        points.push(signal);
        Device {
            id: self.id.clone(),
            instance: ctx.instance().clone(),
            native_name: self.name.as_str().into(),
            manufacturer: Some("LIFX".into()),
            model: Some(self.model.as_str().into()),
            description: None,
            native_room: self.room.as_deref().map(Into::into),
            members: Vec::new(),
            points,
        }
    }

    /// Its points' values in this reading.
    fn values(&self, state: &LightState) -> Vec<(&'static str, Value)> {
        let mut out = vec![
            ("on", Value::Bool(state.on)),
            (
                "brightness",
                Value::Int(proto::percent(state.color.brightness)),
            ),
        ];
        if self.kelvin.is_some() {
            out.push(("color_temp", Value::Int(i64::from(state.color.kelvin))));
        }
        if self.color {
            out.push(("color", Value::from(proto::hex(state.color).as_str())));
        }
        out
    }
}

fn spec(key: &str, label: &str, kind: Kind, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: true,
        },
        unit,
        semantic,
    }
}

fn range(min: f64, max: f64) -> Kind {
    Kind::Numeric {
        min: Some(min),
        max: Some(max),
        step: Some(1.0),
    }
}

/// The message that carries an order, and the colour the bulb will have.
fn order(
    bulb: &Bulb,
    base: Hsbk,
    key: &str,
    value: &Value,
    transition: u32,
) -> Result<(u16, Vec<u8>, Option<Hsbk>), String> {
    let mut color = base;
    match key {
        "on" => {
            let Value::Bool(on) = *value else {
                return Err(moli_i18n::tr!("pilotes.lifx.ne_se_regle_pas", point = key));
            };
            return Ok((
                proto::LIGHT_SET_POWER,
                proto::set_power(on, transition),
                None,
            ));
        }
        "brightness" => {
            let percent = value
                .as_f64()
                .ok_or_else(|| moli_i18n::tr!("pilotes.lifx.ne_se_regle_pas", point = key))?;
            color.brightness = proto::level(percent);
        }
        "color_temp" if bulb.kelvin.is_some() => {
            let (min, max) = bulb.kelvin.unwrap_or_default();
            let kelvin = value
                .as_f64()
                .ok_or_else(|| moli_i18n::tr!("pilotes.lifx.ne_se_regle_pas", point = key))?;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let kelvin = kelvin.round().clamp(f64::from(min), f64::from(max)) as u16;
            color.kelvin = kelvin;
            color.saturation = 0;
        }
        "color" if bulb.color => {
            let text = match value {
                Value::Text(text) => Some(&**text),
                _ => None,
            };
            let (hue, saturation) = text
                .and_then(proto::hue_saturation)
                .ok_or_else(|| moli_i18n::tr!("pilotes.lifx.couleur_invalide"))?;
            color.hue = hue;
            color.saturation = saturation;
        }
        _ => return Err(moli_i18n::tr!("pilotes.lifx.ne_se_regle_pas", point = key)),
    }
    Ok((
        proto::LIGHT_SET_COLOR,
        proto::set_color(color, transition),
        Some(color),
    ))
}

async fn apply(
    link: &mut Link,
    bulb: &mut Bulb,
    command: &CommandRequest,
    transition: u32,
) -> Result<Value, String> {
    let base = match bulb.last {
        Some(color) => color,
        None => link
            .ask(
                bulb.at,
                bulb.serial,
                (proto::LIGHT_GET, &[]),
                Reply::Response,
                proto::LIGHT_STATE,
                TRIES,
            )
            .await
            .and_then(|m| proto::light_state(&m.payload))
            .map(|s| s.color)
            .ok_or_else(|| moli_i18n::tr!("pilotes.lifx.pas_de_reponse"))?,
    };
    let (kind, payload, after) = order(bulb, base, &command.key, &command.value, transition)?;
    link.ask(
        bulb.at,
        bulb.serial,
        (kind, &payload),
        Reply::Ack,
        proto::ACKNOWLEDGEMENT,
        TRIES,
    )
    .await
    .ok_or_else(|| moli_i18n::tr!("pilotes.lifx.pas_de_reponse"))?;
    // What the bulb now has, as a poll would read it.
    let Some(color) = after else {
        return Ok(command.value.clone());
    };
    bulb.last = Some(color);
    Ok(match &*command.key {
        "brightness" => Value::Int(proto::percent(color.brightness)),
        "color_temp" => Value::Int(i64::from(color.kelvin)),
        _ => Value::from(proto::hex(color).as_str()),
    })
}

/// The bulbs of this run, by device id.
struct Bulbs(HashMap<DeviceId, Bulb>);

impl Bulbs {
    /// Looks for bulbs; new ones are met and published, moved ones
    /// followed. A bulb is its serial: another one answering at a known
    /// address is another bulb and never takes the first one's place. The
    /// signal of the others is read again here, not at each poll.
    async fn search(&mut self, lifx: &Lifx, link: &mut Link, ctx: &DriverCtx) {
        for bulb in self.0.values().filter(|b| b.online) {
            if let Some(rssi) = signal(link, bulb.serial, bulb.at).await {
                ctx.set_state(&bulb.id, "rssi", Value::Int(rssi));
            }
        }
        let found = link.search(&lifx.hosts, lifx.discovery, lifx.port).await;
        for (serial, at) in found {
            if let Some(bulb) = self.0.values_mut().find(|b| b.serial == serial) {
                if bulb.at != at {
                    tracing::info!(instance = %ctx.instance(), bulb = %bulb.id, old = %bulb.at, new = %at, "lifx found at a new address");
                    bulb.at = at;
                    bulb.polls = 0;
                }
                continue;
            }
            let Some(met) = identify(link, serial, at).await else {
                continue;
            };
            if met.product.is_none() {
                ctx.warn(moli_i18n::tr!(
                    "pilotes.lifx.modele_inconnu",
                    pid = met.pid.map_or_else(|| "?".to_owned(), |p| p.to_string())
                ));
            }
            let bulb = Bulb::new(ctx, serial, at, &met);
            ctx.upsert_device(bulb.device(ctx));
            ctx.set_availability(&bulb.id, true);
            for (key, value) in bulb.values(&met.state) {
                ctx.set_state(&bulb.id, key, value);
            }
            if let Some(rssi) = met.rssi {
                ctx.set_state(&bulb.id, "rssi", Value::Int(rssi));
            }
            self.0.insert(bulb.id.clone(), bulb);
        }
    }

    /// Reads every bulb once: one `LightGet`, sent once, per bulb.
    async fn poll(&mut self, link: &mut Link, ctx: &DriverCtx) {
        for bulb in self.0.values_mut() {
            bulb.polls = bulb.polls.wrapping_add(1);
            if !bulb.online && bulb.polls % OFFLINE_EVERY != 1 {
                continue;
            }
            let state = link
                .ask(
                    bulb.at,
                    bulb.serial,
                    (proto::LIGHT_GET, &[]),
                    Reply::Response,
                    proto::LIGHT_STATE,
                    1,
                )
                .await
                .and_then(|m| proto::light_state(&m.payload));
            let Some(state) = state else {
                bulb.misses += 1;
                if bulb.online && bulb.misses >= MISSES {
                    bulb.online = false;
                    ctx.set_availability(&bulb.id, false);
                    tracing::warn!(instance = %ctx.instance(), bulb = %bulb.id, "lifx unreachable");
                }
                continue;
            };
            bulb.misses = 0;
            if !bulb.online {
                bulb.online = true;
                ctx.set_availability(&bulb.id, true);
            }
            bulb.last = Some(state.color);
            if let Some(label) = &state.label
                && *label != bulb.name
            {
                // Renamed in the LIFX app: the suggestion follows.
                label.clone_into(&mut bulb.name);
                ctx.upsert_device(bulb.device(ctx));
            }
            for (key, value) in bulb.values(&state) {
                ctx.set_state(&bulb.id, key, value);
            }
        }
    }

    /// Whether a bulb is missing: one known before this run or at an
    /// address of `hosts` has not answered, or one is offline.
    fn missing(&self, lifx: &Lifx, known: &BTreeSet<DeviceId>) -> bool {
        let answered = |ip: &Ipv4Addr| self.0.values().any(|b| b.at.ip() == ip);
        self.0.values().any(|b| !b.online)
            || known.iter().any(|id| !self.0.contains_key(id))
            || !lifx.hosts.iter().all(answered)
    }

    fn status(&self, ctx: &DriverCtx) {
        if self.0.is_empty() {
            ctx.wait_for(moli_i18n::tr!("pilotes.lifx.aucune"));
        } else {
            ctx.ready();
        }
    }
}

async fn run(lifx: &Lifx, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    // Restored readings are stale until a bulb answers in this run.
    let known: BTreeSet<DeviceId> = ctx.known_devices().into_iter().collect();
    for id in &known {
        ctx.set_availability(id, false);
    }
    let mut link = Link::open().await?;
    let mut bulbs = Bulbs(HashMap::new());
    bulbs.search(lifx, &mut link, ctx).await;
    bulbs.status(ctx);
    let mut tick = tokio::time::interval(lifx.poll);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let after = |bulbs: &Bulbs| {
        Instant::now()
            + if bulbs.missing(lifx, &known) {
                lifx.search_missing
            } else {
                lifx.search_again
            }
    };
    let mut next_search = after(&bulbs);
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                let result = match bulbs.0.get_mut(&command.device.id) {
                    Some(bulb) => apply(&mut link, bulb, &command, lifx.transition).await,
                    None => Err(moli_i18n::tr!("pilotes.lifx.injoignable")),
                };
                let result = result.map(|value| {
                    ctx.set_state(&command.device.id, &command.key, value);
                });
                command.reply(result);
            }
            _ = tick.tick() => bulbs.poll(&mut link, ctx).await,
            () = tokio::time::sleep_until(next_search) => {
                bulbs.search(lifx, &mut link, ctx).await;
                bulbs.status(ctx);
                next_search = after(&bulbs);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> anyhow::Result<Lifx> {
        Lifx::new(toml::from_str(toml)?)
    }

    #[test]
    fn options_are_checked() {
        let lifx = config(r#"hosts = ["192.168.1.10"]"#).unwrap();
        assert_eq!(lifx.hosts, [Ipv4Addr::new(192, 168, 1, 10)]);
        assert!(lifx.discovery);
        assert_eq!(lifx.poll, Duration::from_secs(5));
        assert!(config("discovery = false").is_err());
        assert!(config(r#"hosts = ["lampe.local"]"#).is_err());
        assert!(config("poll_secs = 0").is_err());
        assert!(config("colour = true").is_err());
    }

    fn bulb(color: bool, kelvin: Option<(u16, u16)>) -> Bulb {
        Bulb {
            serial: [0x02, 0, 0, 0, 0, 1],
            id: DeviceId::from("lifx:020000000001"),
            at: SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), proto::PORT),
            color,
            kelvin,
            name: "Lampe".into(),
            room: None,
            model: "LIFX Mini W".into(),
            last: None,
            misses: 0,
            online: true,
            polls: 0,
        }
    }

    const WARM: Hsbk = Hsbk {
        hue: 0,
        saturation: 0,
        brightness: 65_535,
        kelvin: 2700,
    };

    #[test]
    fn a_fixed_white_takes_power_and_brightness_only() {
        let white = bulb(false, None);
        let (kind, payload, after) = order(&white, WARM, "brightness", &Value::Int(40), 0).unwrap();
        assert_eq!(kind, proto::LIGHT_SET_COLOR);
        assert_eq!(after.unwrap().brightness, proto::level(40.0));
        assert_eq!(after.unwrap().kelvin, 2700);
        assert_eq!(payload.len(), 13);
        let (kind, payload, after) = order(&white, WARM, "on", &Value::Bool(false), 250).unwrap();
        assert_eq!(kind, proto::LIGHT_SET_POWER);
        assert_eq!(payload, [0, 0, 250, 0, 0, 0]);
        assert!(after.is_none());
        assert!(order(&white, WARM, "color_temp", &Value::Int(4000), 0).is_err());
        assert!(order(&white, WARM, "color", &Value::from("#ff0000"), 0).is_err());
        assert!(order(&white, WARM, "rssi", &Value::Int(-50), 0).is_err());
    }

    #[test]
    fn a_colour_bulb_takes_temperature_and_colour() {
        let colour = bulb(true, Some((1500, 9000)));
        let (_, _, after) = order(&colour, WARM, "color_temp", &Value::Int(12_000), 0).unwrap();
        assert_eq!(after.unwrap().kelvin, 9000);
        let (_, _, after) = order(&colour, WARM, "color", &Value::from("#0000ff"), 0).unwrap();
        let after = after.unwrap();
        assert_eq!(proto::hex(after), "#0000ff");
        assert_eq!(after.brightness, 65_535);
        assert!(order(&colour, WARM, "color", &Value::from("bleu"), 0).is_err());
    }

    #[test]
    fn values_follow_what_the_model_has() {
        let state = LightState {
            color: Hsbk {
                brightness: 32_768,
                ..WARM
            },
            on: true,
            label: Some("Lampe".into()),
        };
        let white: HashMap<_, _> = bulb(false, None).values(&state).into_iter().collect();
        assert_eq!(white.len(), 2);
        assert_eq!(white["on"], Value::Bool(true));
        assert_eq!(white["brightness"], Value::Int(50));
        let colour: HashMap<_, _> = bulb(true, Some((1500, 9000)))
            .values(&state)
            .into_iter()
            .collect();
        assert_eq!(colour["color_temp"], Value::Int(2700));
        assert_eq!(colour["color"], Value::from("#ffffff"));
    }

    /// The driver against fake bulbs on the loopback (127.0.0.x, one port):
    /// what reaches the hub, and what reaches the bulbs.
    #[cfg(test)]
    mod against_fake_bulbs {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        use moli_core::{InstanceId, Origin, PointId};
        use moli_runtime::{Hub, HubOptions, spawn_driver};
        use tokio_util::sync::CancellationToken;

        use super::*;

        /// A LIFX Mini W (product 66, firmware 3.80): what it was asked, and
        /// whether it acknowledges orders.
        struct Bulb {
            seen: Arc<Mutex<Vec<u16>>>,
            acks: Arc<AtomicBool>,
            stop: CancellationToken,
        }

        impl Drop for Bulb {
            fn drop(&mut self) {
                self.stop.cancel();
            }
        }

        fn label(text: &str) -> [u8; 32] {
            let mut out = [0u8; 32];
            out[..text.len()].copy_from_slice(text.as_bytes());
            out
        }

        #[allow(clippy::too_many_lines)] // one fake bulb, every message it knows
        async fn bulb(at: SocketAddrV4, serial: Serial, name: &'static str) -> Bulb {
            let socket = UdpSocket::bind(at).await.unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let acks = Arc::new(AtomicBool::new(true));
            let stop = CancellationToken::new();
            let (log, ack, done) = (seen.clone(), acks.clone(), stop.clone());
            tokio::spawn(async move {
                let mut color = Hsbk {
                    hue: 0,
                    saturation: 0,
                    brightness: u16::MAX,
                    kelvin: 2700,
                };
                let mut on = true;
                let mut buf = [0u8; 1024];
                loop {
                    let (n, from) = tokio::select! {
                        () = done.cancelled() => return,
                        got = socket.recv_from(&mut buf) => got.unwrap(),
                    };
                    let Some(m) = proto::decode(&buf[..n]) else {
                        continue;
                    };
                    log.lock().unwrap().push(m.kind);
                    let answer: Option<(u16, Vec<u8>)> = match m.kind {
                        proto::GET_SERVICE => {
                            let mut p = vec![1];
                            p.extend_from_slice(&u32::from(at.port()).to_le_bytes());
                            Some((proto::STATE_SERVICE, p))
                        }
                        proto::LIGHT_GET => {
                            let mut p = Vec::new();
                            for v in [
                                color.hue,
                                color.saturation,
                                color.brightness,
                                color.kelvin,
                                0,
                            ] {
                                p.extend_from_slice(&v.to_le_bytes());
                            }
                            p.extend_from_slice(&(if on { u16::MAX } else { 0 }).to_le_bytes());
                            p.extend_from_slice(&label(name));
                            p.extend_from_slice(&[0; 8]);
                            Some((proto::LIGHT_STATE, p))
                        }
                        proto::GET_VERSION => {
                            let mut p = 1u32.to_le_bytes().to_vec();
                            p.extend_from_slice(&66u32.to_le_bytes());
                            p.extend_from_slice(&[0; 4]);
                            Some((proto::STATE_VERSION, p))
                        }
                        proto::GET_HOST_FIRMWARE => {
                            let mut p = vec![0; 16];
                            p.extend_from_slice(&80u16.to_le_bytes());
                            p.extend_from_slice(&3u16.to_le_bytes());
                            Some((proto::STATE_HOST_FIRMWARE, p))
                        }
                        proto::GET_GROUP => {
                            let mut p = vec![0; 16];
                            p.extend_from_slice(&label("Bureau"));
                            p.extend_from_slice(&[0; 8]);
                            Some((proto::STATE_GROUP, p))
                        }
                        proto::GET_WIFI_INFO => {
                            let mut p = 1e-6f32.to_le_bytes().to_vec();
                            p.extend_from_slice(&[0; 10]);
                            Some((proto::STATE_WIFI_INFO, p))
                        }
                        proto::LIGHT_SET_POWER if ack.load(Ordering::SeqCst) => {
                            on = m.payload[..2] != [0, 0];
                            Some((proto::ACKNOWLEDGEMENT, Vec::new()))
                        }
                        proto::LIGHT_SET_COLOR if ack.load(Ordering::SeqCst) => {
                            let word =
                                |i: usize| u16::from_le_bytes([m.payload[i], m.payload[i + 1]]);
                            color = Hsbk {
                                hue: word(1),
                                saturation: word(3),
                                brightness: word(5),
                                kelvin: word(7),
                            };
                            Some((proto::ACKNOWLEDGEMENT, Vec::new()))
                        }
                        _ => None,
                    };
                    if let Some((kind, payload)) = answer {
                        let frame = proto::encode(
                            m.source,
                            Some(serial),
                            m.sequence,
                            kind,
                            &payload,
                            Reply::Response,
                        );
                        let _ = socket.send_to(&frame, from).await;
                    }
                }
            });
            Bulb { seen, acks, stop }
        }

        const ONE: Serial = [0x02, 0, 0, 0, 0, 0x01];
        const TWO: Serial = [0x02, 0, 0, 0, 0, 0x02];

        /// A free port on the loopback for the fake bulbs.
        fn free_port() -> u16 {
            std::net::UdpSocket::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port()
        }

        fn at(last: u8, port: u16) -> SocketAddrV4 {
            SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, last), port)
        }

        fn start(hosts: &[u8], port: u16, search: Duration) -> (Hub, CancellationToken) {
            let hub = Hub::new(HubOptions::default()).unwrap();
            let cancel = CancellationToken::new();
            let lifx = Lifx {
                hosts: hosts.iter().map(|&h| Ipv4Addr::new(127, 0, 0, h)).collect(),
                discovery: false,
                poll: Duration::from_millis(300),
                transition: 0,
                port,
                search_missing: search,
                search_again: search,
            };
            spawn_driver(
                &hub,
                InstanceId::from("lifx"),
                Arc::new(lifx),
                cancel.clone(),
            );
            (hub, cancel)
        }

        fn point(serial: &str, key: &str) -> PointId {
            PointId::from(format!("lifx:{serial}/{key}"))
        }

        fn online(hub: &Hub, id: &str) -> Option<bool> {
            hub.snapshot()
                .devices
                .iter()
                .find(|d| d.device.id.as_str() == id)
                .and_then(|d| d.online)
        }

        async fn until(what: &str, mut ok: impl FnMut() -> bool) {
            for _ in 0..200 {
                if ok() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("never: {what}");
        }

        #[tokio::test]
        async fn an_order_is_published_only_once_acknowledged() {
            let port = free_port();
            let fake = bulb(at(2, port), ONE, "Lampe").await;
            let (hub, _cancel) = start(&[2], port, Duration::from_secs(60));
            until("the bulb is published", || {
                online(&hub, "lifx:020000000001") == Some(true)
            })
            .await;
            let device = hub.snapshot().devices.pop().unwrap();
            assert_eq!(&*device.device.native_name, "Lampe");
            assert_eq!(device.device.native_room.as_deref(), Some("Bureau"));
            assert_eq!(device.device.model.as_deref(), Some("LIFX Mini W"));
            let keys: Vec<&str> = device.device.points.iter().map(|p| &*p.key).collect();
            assert_eq!(keys, ["on", "brightness", "rssi"]);
            assert_eq!(device.state["rssi"].value, Value::Int(-60));

            let brightness = point("020000000001", "brightness");
            hub.command(&brightness, Value::Int(40), Origin::Api, None)
                .await
                .unwrap();
            assert_eq!(hub.state(&brightness).unwrap().value, Value::Int(40));

            fake.acks.store(false, Ordering::SeqCst);
            let refused = hub
                .command(&brightness, Value::Int(10), Origin::Api, None)
                .await;
            assert!(
                refused.is_err(),
                "no acknowledgement, no success: {refused:?}"
            );
            assert_eq!(hub.state(&brightness).unwrap().value, Value::Int(40));
        }

        #[tokio::test]
        async fn a_poll_is_one_light_get_per_bulb() {
            let port = free_port();
            let fake = bulb(at(2, port), ONE, "Lampe").await;
            let (hub, _cancel) = start(&[2], port, Duration::from_secs(60));
            until("the bulb is published", || {
                online(&hub, "lifx:020000000001") == Some(true)
            })
            .await;
            fake.seen.lock().unwrap().clear();
            tokio::time::sleep(Duration::from_millis(1600)).await;
            let seen = fake.seen.lock().unwrap().clone();
            assert!(seen.len() >= 3, "polled: {seen:?}");
            assert!(seen.iter().all(|&k| k == proto::LIGHT_GET), "{seen:?}");
        }

        #[tokio::test]
        async fn another_bulb_at_the_address_never_replaces_the_first() {
            let port = free_port();
            let first = bulb(at(2, port), ONE, "Entrée").await;
            let (hub, _cancel) = start(&[2, 3], port, Duration::from_millis(400));
            until("the first bulb is published", || {
                online(&hub, "lifx:020000000001") == Some(true)
            })
            .await;

            // The first one goes away; another one answers at its address.
            drop(first);
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _second = bulb(at(2, port), TWO, "Cuisine").await;
            until("the second bulb is published", || {
                online(&hub, "lifx:020000000002") == Some(true)
            })
            .await;
            until("the first bulb is offline", || {
                online(&hub, "lifx:020000000001") == Some(false)
            })
            .await;
            let names: Vec<(String, String)> = hub
                .snapshot()
                .devices
                .iter()
                .map(|d| (d.device.id.to_string(), d.device.native_name.to_string()))
                .collect();
            assert!(
                names.contains(&("lifx:020000000001".into(), "Entrée".into())),
                "{names:?}"
            );
            assert!(
                names.contains(&("lifx:020000000002".into(), "Cuisine".into())),
                "{names:?}"
            );

            // The first one comes back elsewhere: same serial, followed.
            let moved = bulb(at(3, port), ONE, "Entrée").await;
            until("the first bulb is back", || {
                online(&hub, "lifx:020000000001") == Some(true)
            })
            .await;
            moved.seen.lock().unwrap().clear();
            hub.command(
                &point("020000000001", "on"),
                Value::Bool(false),
                Origin::Api,
                None,
            )
            .await
            .unwrap();
            assert!(moved.seen.lock().unwrap().contains(&proto::LIGHT_SET_POWER));
        }
    }
}
