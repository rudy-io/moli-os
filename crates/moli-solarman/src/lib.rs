//! Solarman Wi-Fi loggers (protocol V5, TCP 8899) and the Deye
//! micro-inverters behind them, 100 % local and read-only.
//!
//! One instance = one logger. A micro-inverter sleeps at night, its logger
//! with it: the device goes offline without a word (no « waiting », no
//! error), keeps the day's production until midnight, then shows 0 for the
//! new day. The logger is followed by its MAC when the box gives it another
//! address, looked for only by day (at night its silence is normal).

mod deye;
mod v5;

use std::net::Ipv4Addr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use jiff::Zoned;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use moli_core::{DeviceId, Value};
use moli_net::lan::{self, Looked, Pace};
use moli_runtime::{BoxFuture, Driver, DriverCtx, KnownAddress};
use serde::Deserialize;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpStream, UdpSocket};

pub use deye::{Counters, Info};
use v5::{Frame, NoData};

/// How often the inverter is read (ha-solarman: 5 s).
const POLL: Duration = Duration::from_secs(10);
/// Who the inverter is, read again this often.
const INFO_EVERY: Duration = Duration::from_secs(3600);
/// One exchange with the logger, connection included.
const EXCHANGE: Duration = Duration::from_secs(6);
/// Failed reads in a row before the logger is looked for elsewhere.
const LOOK_AFTER: u32 = 3;
/// The hours (local) when a silent logger is surely not asleep.
const DAYLIGHT: std::ops::RangeInclusive<i8> = 10..=16;
/// The logger's discovery port and question.
const DISCOVERY_PORT: u16 = 48899;
const DISCOVERY: &[u8] = b"WIFIKIT-214028-READ";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// The logger's serial number (on its sticker, or in the Solarman app).
    /// Without it, Moli asks the logger on the home network.
    #[serde(default)]
    pub serial: Option<u32>,
    /// Its MAC address: where to find it if the box gives it another address
    /// before Moli learned it.
    #[serde(default)]
    pub mac: Option<String>,
    /// The inverter's Modbus address behind the logger.
    #[serde(default = "default_slave")]
    pub slave: u8,
    /// Where the day ends (the day's production goes back to 0).
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

fn default_port() -> u16 {
    8899
}

fn default_slave() -> u8 {
    1
}

fn default_timezone() -> String {
    "Europe/Paris".to_owned()
}

#[derive(Debug)]
pub struct Solarman {
    config: Config,
    /// When the logger may next be looked for elsewhere (kept across restarts).
    pace: Mutex<Pace>,
}

impl Solarman {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            pace: Mutex::new(Pace::default()),
        }
    }
}

impl Driver for Solarman {
    fn kind(&self) -> &'static str {
        "solarman"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

/// One logger on the network.
#[derive(Debug)]
struct Logger {
    host: String,
    port: u16,
    serial: u32,
    slave: u8,
    sequence: u16,
}

impl Logger {
    /// Reads `count` registers from `start`.
    async fn read(&mut self, (start, count): (u16, u16)) -> anyhow::Result<Vec<u16>> {
        self.sequence = self.sequence % 255 + 1;
        let sequence = self.sequence;
        let request = v5::read_request(self.serial, sequence, self.slave, start, count);
        let exchange = async {
            let mut stream = TcpStream::connect((self.host.as_str(), self.port))
                .await
                .with_context(|| format!("cannot reach {}:{}", self.host, self.port))?;
            stream.write_all(&request).await?;
            let mut buf = Vec::new();
            loop {
                match v5::parse(&buf, self.serial, sequence, count)? {
                    Frame::Registers(registers, _) => return Ok(registers),
                    Frame::Other(n) => {
                        buf.drain(..n);
                        continue;
                    }
                    Frame::Partial => {}
                }
                let mut chunk = [0u8; 1024];
                let n = stream.read(&mut chunk).await?;
                if n == 0 {
                    // The logger hung up without an answer: nobody behind it.
                    return Err(NoData::Asleep.into());
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 4096 {
                    bail!("too much data from the logger");
                }
            }
        };
        tokio::time::timeout(EXCHANGE, exchange)
            .await
            .unwrap_or_else(|_| Err(NoData::Asleep.into()))
    }
}

/// Whether the logger itself answered (the inverter may still be asleep).
fn logger_answered(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NoData>().is_some()
}

/// Asks the loggers of the network who they are; the serial of the one at
/// `host`, if it answers.
async fn discover(host: Ipv4Addr) -> Option<u32> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.ok()?;
    socket.set_broadcast(true).ok()?;
    let [a, b, c, _] = host.octets();
    let broadcast = Ipv4Addr::new(a, b, c, 255);
    for _ in 0..2 {
        socket
            .send_to(DISCOVERY, (broadcast, DISCOVERY_PORT))
            .await
            .ok()?;
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut buf = [0u8; 256];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(Ok((n, from))) = tokio::time::timeout(left, socket.recv_from(&mut buf)).await else {
            break;
        };
        if from.ip() != host {
            continue;
        }
        if let Some(serial) = discovery_serial(&buf[..n], host) {
            return Some(serial);
        }
    }
    None
}

/// `ip,MAC,serial` → the serial, if `ip` is `host`.
fn discovery_serial(answer: &[u8], host: Ipv4Addr) -> Option<u32> {
    let text = std::str::from_utf8(answer).ok()?;
    let mut parts = text.trim().split(',');
    let ip: Ipv4Addr = parts.next()?.parse().ok()?;
    let _mac = parts.next()?;
    let serial = parts.next()?.trim().parse().ok()?;
    (ip == host).then_some(serial)
}

impl Solarman {
    /// Looks for the logger elsewhere on the home network (by its MAC); an
    /// address is taken only if the logger there answers to its serial.
    async fn look(&self, ctx: &DriverCtx, logger: &mut Logger, configured: &str, mac: &str) {
        let Ok(old) = logger.host.parse::<Ipv4Addr>() else {
            return;
        };
        let (port, serial, slave) = (logger.port, logger.serial, logger.slave);
        let looked = lan::relocate(&self.pace, mac, old, |ip| async move {
            let mut there = Logger {
                host: ip.to_string(),
                port,
                serial,
                slave,
                sequence: 0,
            };
            match there.read(deye::INFO).await {
                Ok(_) => true,
                Err(e) => logger_answered(&e),
            }
        })
        .await;
        if let Looked::Found(ip) = looked {
            let host = ip.to_string();
            let address = KnownAddress {
                configured: configured.to_owned(),
                host: host.clone(),
                mac: Some(mac.to_owned()),
                identity: Some(serial.to_string()),
            };
            tracing::info!(instance = %ctx.instance(), old = %logger.host, new = %host, "solarman logger found at a new address");
            ctx.moved(
                Some(ctx.device_id(&serial.to_string())),
                &logger.host,
                address,
            )
            .await;
            logger.host = host;
        }
    }
}

/// Where to reach the logger first, its serial and its MAC: remembered, or
/// configured (a person who gives another address or serial is obeyed).
fn whereabouts(
    ctx: &DriverCtx,
    config: &Config,
    follow: bool,
) -> (String, Option<u32>, Option<String>) {
    let given = config.mac.as_deref().and_then(lan::mac_hex);
    let remembered = ctx.known_address().filter(|k| {
        follow
            && k.configured == config.host
            && given.as_ref().is_none_or(|g| k.mac.as_ref() == Some(g))
    });
    match remembered {
        Some(k) => {
            let serial = config
                .serial
                .or_else(|| k.identity.as_deref().and_then(|s| s.parse().ok()));
            let same =
                config.serial.is_none() || k.identity == config.serial.map(|s| s.to_string());
            if same {
                (k.host, serial, k.mac.or(given))
            } else {
                (config.host.clone(), config.serial, given)
            }
        }
        None => (config.host.clone(), config.serial, given),
    }
}

/// Waits `pause`, refusing orders (this driver takes none). `false`: the
/// driver is stopping.
async fn pause(ctx: &mut DriverCtx, pause: Duration) -> bool {
    let wake = tokio::time::sleep(pause);
    tokio::pin!(wake);
    loop {
        tokio::select! {
            () = &mut wake => return true,
            command = ctx.next_command() => match command {
                Some(command) => command.reply(Err(moli_i18n::tr!("pilotes.solarman.lecture_seule"))),
                None => return false,
            },
        }
    }
}

/// The inverter as it is now: its device and its counters.
struct Inverter {
    id: DeviceId,
    info: Info,
    counters: Counters,
    info_read: Instant,
}

#[allow(clippy::too_many_lines)]
async fn run(solarman: &Solarman, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let config = &solarman.config;
    let Ok(tz) = TimeZone::get(&config.timezone) else {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.solarman.fuseau_inconnu",
            timezone = config.timezone
        ));
        ctx.cancelled().await;
        return Ok(());
    };
    let configured = config.host.clone();
    let address = configured.parse::<Ipv4Addr>().ok();
    let follow = address.is_some_and(|ip| ip.is_private());
    let (first, mut serial, mut mac) = whereabouts(ctx, config, follow);

    // The serial: configured, remembered, or asked on the network (the logger
    // only answers by day).
    let serial = loop {
        if let Some(serial) = serial {
            break serial;
        }
        serial = match first.parse::<Ipv4Addr>() {
            Ok(ip) => discover(ip).await,
            Err(_) => None,
        };
        if serial.is_none() {
            ctx.wait_for(moli_i18n::tr!(
                "pilotes.solarman.numero_inconnu",
                host = first
            ));
            if !pause(ctx, Duration::from_secs(300)).await {
                return Ok(());
            }
        }
    };
    let mut logger = Logger {
        host: first,
        port: config.port,
        serial,
        slave: config.slave,
        sequence: 0,
    };
    let id = ctx.device_id(&serial.to_string());
    let mut inverter: Option<Inverter> = None;
    let mut online: Option<bool> = None;
    let mut failures = 0u32;
    // The day the published « today » values belong to.
    let mut day: Option<Date> = None;
    if !ctx.known_devices().is_empty() {
        // Known from before (a restart at night): ready, just asleep.
        ctx.ready();
    }
    loop {
        let now = Zoned::now().with_time_zone(tz.clone());
        let result = match &inverter {
            Some(inv) if inv.info_read.elapsed() < INFO_EVERY => {
                logger.read(deye::LIVE).await.map(Some)
            }
            _ => match logger.read(deye::INFO).await {
                Ok(regs) => {
                    let Some(info) = Info::read(&regs) else {
                        ctx.wait_for(moli_i18n::tr!("pilotes.solarman.pas_un_micro"));
                        ctx.cancelled().await;
                        return Ok(());
                    };
                    if inverter.as_ref().is_none_or(|i| i.info != info) {
                        ctx.upsert_device(deye::device(ctx.instance(), id.clone(), &info));
                    }
                    let counters = inverter.take().map(|i| i.counters).unwrap_or_default();
                    inverter = Some(Inverter {
                        id: id.clone(),
                        info,
                        counters,
                        info_read: Instant::now(),
                    });
                    ctx.ready();
                    logger.read(deye::LIVE).await.map(Some)
                }
                Err(e) => Err(e),
            },
        };
        match (result, inverter.as_mut()) {
            (Ok(Some(regs)), Some(inv)) => {
                for (key, value) in deye::readings(&regs, &inv.info, &mut inv.counters) {
                    ctx.set_state(&inv.id, &key, value);
                }
                day = Some(now.date());
                if online != Some(true) {
                    online = Some(true);
                    ctx.set_availability(&inv.id, true);
                }
                if failures > 0 {
                    failures = 0;
                    solarman
                        .pace
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .reset();
                }
                if follow && mac.is_none() {
                    mac = match logger.host.parse::<Ipv4Addr>() {
                        Ok(ip) => lan::mac_of(ip).await,
                        Err(_) => None,
                    };
                }
                if follow && ctx.known_address().is_none_or(|k| k.host != logger.host) {
                    ctx.remember_address(KnownAddress {
                        configured: configured.clone(),
                        host: logger.host.clone(),
                        mac: mac.clone(),
                        identity: Some(serial.to_string()),
                    })
                    .await;
                }
            }
            (Ok(_), _) => {}
            (Err(e), _) => {
                failures += 1;
                let asleep = logger_answered(&e);
                if online != Some(false) {
                    online = Some(false);
                    ctx.set_availability(&id, false);
                    // Normal every evening: not a warning.
                    tracing::debug!(instance = %ctx.instance(), error = %format!("{e:#}"), "solarman inverter silent");
                }
                // A new day while asleep: today's production is 0.
                if day.is_some_and(|d| d != now.date())
                    && let Some(inv) = &inverter
                {
                    ctx.set_state(&inv.id, "energy_today", Value::Float(0.0));
                    ctx.set_state(&inv.id, "power", Value::Float(0.0));
                    for n in 1..=inv.info.inputs {
                        ctx.set_state(&inv.id, &format!("pv{n}_energy_today"), Value::Float(0.0));
                        ctx.set_state(&inv.id, &format!("pv{n}_power"), Value::Float(0.0));
                    }
                    day = Some(now.date());
                }
                if follow && !asleep && failures >= LOOK_AFTER && DAYLIGHT.contains(&now.hour()) {
                    if logger.host != configured {
                        configured.clone_into(&mut logger.host);
                    } else if let Some(mac) = mac.as_deref() {
                        solarman.look(ctx, &mut logger, &configured, mac).await;
                    }
                }
            }
        }
        if !pause(ctx, POLL).await {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_discovery_answer_gives_the_serial_of_that_address() {
        let host = Ipv4Addr::new(10, 0, 0, 20);
        assert_eq!(
            discovery_serial(b"10.0.0.20,AABBCCDDEEFF,1234567890", host),
            Some(1_234_567_890)
        );
        assert_eq!(
            discovery_serial(b"10.0.0.21,AABBCCDDEEFF,1234567890", host),
            None
        );
        assert_eq!(discovery_serial(b"garbage", host), None);
    }

    #[test]
    fn options_have_their_defaults() {
        let config: Config =
            serde_json::from_value(serde_json::json!({ "host": "10.0.0.20" })).unwrap();
        assert_eq!(config.port, 8899);
        assert_eq!(config.slave, 1);
        assert!(TimeZone::get(&config.timezone).is_ok());
        assert!(
            serde_json::from_value::<Config>(serde_json::json!({ "host": "x", "every": 3 }))
                .is_err(),
            "unknown options are refused"
        );
    }
}
