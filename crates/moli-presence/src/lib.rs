//! Presence: who is home, from their phones on the home Wi-Fi. Local, no
//! app, no cloud: what Home Assistant's « ping » and « nmap » trackers do.
//!
//! Every `every` seconds, one tiny UDP datagram goes to each address of the
//! home network (the kernel asks « who has this address? » on the way), then
//! the kernel's neighbour table (`/proc/net/arp`, the host's: Moli runs on the
//! host network) says which hardware addresses answered. A person is home as
//! soon as one of their phones answers, away after `away_after` minutes
//! without a sign (a sleeping phone skips a beat now and then).
//!
//! Machines (a PC, a NAS) are followed the same way by their address on the
//! network: on while they answer, off after two looks without an answer
//! (what Home Assistant's « ping » sensor did for « État PC »).
//!
//! A phone's Wi-Fi address: on an iPhone, Réglages › Wi-Fi › (i) › « Adresse
//! Wi-Fi » (private, but fixed for this network); on Android, Paramètres ›
//! Wi-Fi › the network › « Adresse MAC de l'appareil ».

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use moli_core::{Access, Device, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The home network, `192.168.0.0/24` (a /24 or smaller).
    pub network: String,
    /// Seconds between two looks (default 60).
    #[serde(default = "default_every")]
    pub every: u64,
    /// Minutes without a sign before someone counts as gone (default 10).
    #[serde(default = "default_away")]
    pub away_after: u64,
    #[serde(default, rename = "person")]
    pub people: Vec<Person>,
    #[serde(default, rename = "machine")]
    pub machines: Vec<Machine>,
}

/// A machine followed by its network address (fixed by the box).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    /// Stable id: the device is `<instance>:<id>`.
    pub id: String,
    pub name: String,
    pub ip: Ipv4Addr,
}

fn default_every() -> u64 {
    60
}

fn default_away() -> u64 {
    10
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    /// Stable id: the device is `<instance>:<id>`.
    pub id: String,
    pub name: String,
    /// Their phones' Wi-Fi addresses (`aa:bb:cc:dd:ee:ff`).
    pub macs: Vec<String>,
}

/// `192.168.0.0/24` → its host addresses.
fn hosts(network: &str) -> anyhow::Result<Vec<Ipv4Addr>> {
    let (base, bits) = network.split_once('/').context("network: a.b.c.d/nn")?;
    let base: Ipv4Addr = base.parse().context("network address")?;
    let bits: u32 = bits.parse().context("network prefix")?;
    ensure!(
        (24..=30).contains(&bits),
        "network: /24 to /30 (a home network)"
    );
    let mask = u32::MAX << (32 - bits);
    let start = u32::from(base) & mask;
    Ok((1..(1u32 << (32 - bits)) - 1)
        .map(|i| Ipv4Addr::from(start + i))
        .collect())
}

/// Normalized hardware address: lowercase, colons.
fn mac(s: &str) -> Option<String> {
    let hex: String = s
        .chars()
        .filter(char::is_ascii_hexdigit)
        .collect::<String>()
        .to_ascii_lowercase();
    if hex.len() != 12 {
        return None;
    }
    Some(
        hex.as_bytes()
            .chunks(2)
            .map(|c| std::str::from_utf8(c).unwrap_or("00"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// Hardware addresses the kernel currently knows as answering
/// (`/proc/net/arp`, flags 0x2 = complete).
#[cfg(test)]
fn answering(table: &str) -> Vec<String> {
    answering_at(table).into_iter().map(|(_, m)| m).collect()
}

/// Same, with the network address each one answered at.
fn answering_at(table: &str) -> Vec<(Ipv4Addr, String)> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let complete = cols.get(2).is_some_and(|f| {
                u32::from_str_radix(f.trim_start_matches("0x"), 16).is_ok_and(|f| f & 0x2 != 0)
            });
            let ip = cols.first()?.parse().ok()?;
            complete
                .then(|| cols.get(3).and_then(|m| mac(m)))
                .flatten()
                .map(|m| (ip, m))
        })
        .filter(|(_, m)| m != "00:00:00:00:00:00")
        .collect()
}

#[derive(Debug)]
pub struct Presence {
    config: Config,
    hosts: Vec<Ipv4Addr>,
}

impl Presence {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let hosts = hosts(&config.network)?;
        ensure!(config.every >= 15, "every: 15 s at least");
        for p in &config.people {
            ensure!(
                !p.id.is_empty() && !p.id.contains([':', '/']),
                "person id {:?}: non-empty, without ':' nor '/'",
                p.id
            );
            for m in &p.macs {
                ensure!(
                    mac(m).is_some(),
                    "person {}: {m:?} is not a hardware address",
                    p.id
                );
            }
        }
        for m in &config.machines {
            ensure!(
                !m.id.is_empty() && !m.id.contains([':', '/']),
                "machine id {:?}: non-empty, without ':' nor '/'",
                m.id
            );
            ensure!(
                hosts.contains(&m.ip),
                "machine {}: {} is not on {}",
                m.id,
                m.ip,
                config.network
            );
        }
        Ok(Self { config, hosts })
    }
}

impl Driver for Presence {
    fn kind(&self) -> &'static str {
        "presence"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

async fn run(this: &Presence, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let people: Vec<(moli_core::DeviceId, &Person, Vec<String>)> = this
        .config
        .people
        .iter()
        .map(|p| {
            (
                ctx.device_id(&p.id),
                p,
                p.macs.iter().filter_map(|m| mac(m)).collect(),
            )
        })
        .collect();
    for (id, p, _) in &people {
        ctx.upsert_device(Device {
            id: id.clone(),
            instance: ctx.instance().clone(),
            native_name: p.name.as_str().into(),
            manufacturer: Some("Moli".into()),
            model: Some("Présence".into()),
            description: None,
            native_room: None,
            members: Vec::new(),
            points: vec![PointSpec {
                key: "home".into(),
                label: "À la maison".into(),
                kind: Kind::Binary,
                access: Access {
                    read: true,
                    write: false,
                },
                unit: None,
                semantic: Semantic::Occupancy,
            }],
        });
        ctx.set_availability(id, true);
    }
    let machines: Vec<(moli_core::DeviceId, &Machine)> = this
        .config
        .machines
        .iter()
        .map(|m| (ctx.device_id(&m.id), m))
        .collect();
    for (id, m) in &machines {
        ctx.upsert_device(machine_device(ctx, id, m));
        ctx.set_availability(id, true);
    }
    ctx.ready();
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0")
        .await
        .context("udp socket")?;
    let away = Duration::from_secs(this.config.away_after.saturating_mul(60));
    // Only the phones of the people configured are remembered (the rest of
    // the network is none of Moli's business, and stays bounded).
    let wanted: HashSet<&String> = people.iter().flat_map(|(_, _, macs)| macs).collect();
    let mut seen: HashMap<String, Instant> = HashMap::new();
    let mut machines_seen: HashMap<Ipv4Addr, Instant> = HashMap::new();
    // A machine is off after two looks without an answer.
    let off_after = Duration::from_secs(this.config.every.saturating_mul(2) + 5);
    let started = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(this.config.every));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => command.reply(Err("presence is read-only".into())),
                None => return Ok(()),
            },
            _ = tick.tick() => {
                // A knock on every door: the answer is in the neighbour table.
                for host in &this.hosts {
                    let _ = socket.send_to(&[0], SocketAddrV4::new(*host, 9)).await;
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
                let table = tokio::fs::read_to_string("/proc/net/arp").await.unwrap_or_default();
                let now = Instant::now();
                for (ip, m) in answering_at(&table) {
                    if machines.iter().any(|(_, x)| x.ip == ip) {
                        machines_seen.insert(ip, now);
                    }
                    if wanted.contains(&m) {
                        seen.insert(m, now);
                    }
                }
                for (id, m) in &machines {
                    let on = machines_seen.get(&m.ip).is_some_and(|t| now.duration_since(*t) < off_after);
                    ctx.set_state(id, "online", Value::Bool(on));
                }
                for (id, _, macs) in &people {
                    let last = macs.iter().filter_map(|m| seen.get(m)).max();
                    // Until a whole « away » delay has passed since start,
                    // nobody is declared gone (the first looks may miss).
                    let home = match last {
                        Some(t) => now.duration_since(*t) < away,
                        None if now.duration_since(started) < away => continue,
                        None => false,
                    };
                    ctx.set_state(id, "home", Value::Bool(home));
                }
            }
        }
    }
}

fn machine_device(ctx: &DriverCtx, id: &moli_core::DeviceId, m: &Machine) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: m.name.as_str().into(),
        manufacturer: Some("Moli".into()),
        model: Some("Machine".into()),
        // Its address: where to find it, and how a bench matches HA's « ping ».
        description: Some(m.ip.to_string().into()),
        native_room: None,
        members: Vec::new(),
        points: vec![PointSpec {
            key: "online".into(),
            label: "Allumée".into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: Semantic::Other,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_neighbour_table() {
        let table = "IP address       HW type     Flags       HW address            Mask     Device\n\
                     192.168.0.20     0x1         0x2         AA:BB:cc:dd:ee:01     *        enp2s0\n\
                     192.168.0.43     0x1         0x0         00:00:00:00:00:00     *        enp2s0\n\
                     192.168.0.44     0x1         0x6         aa:bb:cc:dd:ee:02     *        enp2s0\n";
        assert_eq!(
            answering(table),
            vec!["aa:bb:cc:dd:ee:01", "aa:bb:cc:dd:ee:02"]
        );
        // A machine is known by where it answered; .43 did not.
        let at: Vec<Ipv4Addr> = answering_at(table).into_iter().map(|(ip, _)| ip).collect();
        assert_eq!(
            at,
            [
                Ipv4Addr::new(192, 168, 0, 20),
                Ipv4Addr::new(192, 168, 0, 44)
            ]
        );
        assert_eq!(
            mac("AA-BB-CC-DD-EE-FF").as_deref(),
            Some("aa:bb:cc:dd:ee:ff")
        );
        assert_eq!(mac("nope"), None);
    }

    #[test]
    fn a_home_network_is_a_small_one() {
        let h = hosts("192.168.0.0/24").unwrap();
        assert_eq!(h.len(), 254);
        assert_eq!(h[0], Ipv4Addr::new(192, 168, 0, 1));
        assert_eq!(h[253], Ipv4Addr::new(192, 168, 0, 254));
        assert!(hosts("10.0.0.0/8").is_err(), "never knock on a whole /8");
        assert!(
            Presence::new(Config {
                network: "192.168.0.0/24".into(),
                every: 60,
                away_after: 10,
                people: vec![Person {
                    id: "sam".into(),
                    name: "Sam".into(),
                    macs: vec!["zz".into()]
                }],
                machines: Vec::new(),
            })
            .is_err()
        );
        let machine = |ip: &str| Config {
            network: "192.168.0.0/24".into(),
            every: 60,
            away_after: 10,
            people: Vec::new(),
            machines: vec![Machine {
                id: "pc".into(),
                name: "PC".into(),
                ip: ip.parse().unwrap(),
            }],
        };
        assert!(Presence::new(machine("192.168.0.43")).is_ok());
        assert!(
            Presence::new(machine("10.0.0.5")).is_err(),
            "off the home network"
        );
    }
}
