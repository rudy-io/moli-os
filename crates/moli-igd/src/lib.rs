//! The internet box (Livebox, Freebox… any UPnP Internet Gateway Device):
//! is the connection up, public address, uptime, line speed. Found where it
//! was last (`data/addresses.json`), at a configured description URL, by an
//! SSDP search, or by listening to its announcements (a Livebox answers no
//! search for its gateway, and moves its description at every restart).
//! Known by its serial number: its UDN changes at every restart too.
//!
//! No throughput: boxes' UPnP byte counters are unreliable (a Livebox
//! reports gigabytes per second on a 1 Gbit/s line). A wrong number is
//! worse than none.

use std::time::Duration;

use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::upnp::{self, Location, tag};
use moli_runtime::{BoxFuture, Driver, DriverCtx, KnownAddress};
use serde::Deserialize;

const POLL: Duration = Duration::from_secs(10);
const WAN_IP: &str = "urn:schemas-upnp-org:service:WANIPConnection:";
const WAN_COMMON: &str = "urn:schemas-upnp-org:service:WANCommonInterfaceConfig:";
const IGD: &str = "urn:schemas-upnp-org:device:InternetGatewayDevice:";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Skip discovery: the box's description URL.
    #[serde(default)]
    pub location: Option<String>,
}

#[derive(Debug)]
pub struct Igd {
    config: Config,
}

impl Igd {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Igd {
    fn kind(&self) -> &'static str {
        "igd"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

struct Gateway {
    at: Location,
    ip: (String, String),
    common: Option<(String, String)>,
}

/// The box as found: where, its name, who it is.
struct Found {
    gw: Gateway,
    name: String,
    serial: String,
}

/// Who the box is, for good: its serial number (a Livebox draws a new UDN at
/// every restart), else its UDN, else its address.
fn identity<'a>(description: &'a str, at: &'a Location) -> &'a str {
    tag(description, "serialNumber")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| tag(description, "UDN").map(|u| u.trim().trim_start_matches("uuid:")))
        .unwrap_or(&at.host)
}

/// The first of `candidates` that is an internet box (and the expected one
/// when it is known: another box answering is not ours).
async fn first_box(candidates: Vec<Location>, expected: Option<&str>) -> Option<Found> {
    for at in candidates {
        let Ok(description) = upnp::fetch(&at.host, at.port, &at.path).await else {
            continue;
        };
        let Some(ip) = upnp::control_url(&description, WAN_IP) else {
            continue;
        };
        let serial = identity(&description, &at).to_owned();
        if expected.is_some_and(|e| e != serial) {
            continue;
        }
        let name = tag(&description, "friendlyName")
            .map_or_else(|| moli_i18n::tr!("pilotes.igd.box_internet"), str::to_owned);
        let common = upnp::control_url(&description, WAN_COMMON);
        return Some(Found {
            gw: Gateway { at, ip, common },
            name,
            serial,
        });
    }
    None
}

/// Where to look: where it was found last, the configured description,
/// then whatever answers a search. `expected`: the box it must be.
async fn find(
    config: &Config,
    known: Option<&KnownAddress>,
    expected: Option<&str>,
) -> Option<Found> {
    let mut candidates: Vec<Location> = known
        .and_then(|k| Location::parse(&k.host))
        .into_iter()
        .chain(config.location.as_deref().and_then(Location::parse))
        .collect();
    // Some boxes answer the generic search; a Livebox only for another of
    // its services (its gateway is known by its announcements).
    candidates.extend(
        upnp::discover("upnp:rootdevice", Duration::from_secs(3))
            .await
            .unwrap_or_default(),
    );
    candidates.dedup();
    match first_box(candidates, expected).await {
        Some(found) => Some(found),
        // The configured description is a person's word: their box.
        None if expected.is_some() => {
            first_box(
                config
                    .location
                    .as_deref()
                    .and_then(Location::parse)
                    .into_iter()
                    .collect(),
                None,
            )
            .await
        }
        None => None,
    }
}

fn spec(key: &str, label: &str, kind: Kind, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        semantic: Semantic::infer(key, unit.as_ref()),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
    }
}

fn numeric() -> Kind {
    Kind::Numeric {
        min: Some(0.0),
        max: None,
        step: None,
    }
}

fn device(ctx: &DriverCtx, id: &DeviceId, name: &str) -> Device {
    let mbps = Unit::parse("Mbit/s");
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: name.into(),
        manufacturer: None,
        model: Some(moli_i18n::tr!("pilotes.igd.box_internet").into()),
        description: None,
        native_room: None,
        parent: None,
        members: Vec::new(),
        points: vec![
            spec(
                "connected",
                &moli_i18n::tr!("pilotes.igd.internet"),
                Kind::Binary,
                None,
            ),
            spec(
                "external_ip",
                &moli_i18n::tr!("pilotes.igd.adresse_publique"),
                Kind::Text,
                None,
            ),
            spec(
                "uptime",
                &moli_i18n::tr!("pilotes.igd.connectee_depuis"),
                numeric(),
                Some(Unit::Second),
            ),
            spec(
                "line",
                &moli_i18n::tr!("pilotes.igd.lien_physique"),
                Kind::Binary,
                None,
            ),
            spec(
                "line_down",
                &moli_i18n::tr!("pilotes.igd.debit_reception"),
                numeric(),
                mbps.clone(),
            ),
            spec(
                "line_up",
                &moli_i18n::tr!("pilotes.igd.debit_emission"),
                numeric(),
                mbps,
            ),
        ],
    }
}

async fn call(
    gw: &Gateway,
    (service, path): &(String, String),
    action: &str,
) -> anyhow::Result<String> {
    upnp::soap(&gw.at.host, gw.at.port, path, service, action, &[]).await
}

/// Bits per second → Mbit/s.
#[allow(clippy::cast_precision_loss)]
fn mbps(bits: u64) -> f64 {
    (bits as f64 / 1_000_000.0 * 10.0).round() / 10.0
}

/// Announcements heard from an internet box (the device or its WAN service).
fn from_a_box(heard: &upnp::Announcement) -> bool {
    heard.kind.starts_with(IGD) || heard.kind.starts_with(WAN_IP)
}

/// How long each listening lasts while the box is not found.
const LISTEN: Duration = Duration::from_secs(60);
/// Rounds (of about a minute) the known box is waited for; after that,
/// another one is taken (the box was replaced).
const GIVE_UP: u32 = 60;

/// Finds the box, waiting (and refusing orders) while it cannot be found.
/// Meanwhile it listens to the boxes announcing themselves: one that
/// restarted announces where its description is now. `None`: stopping.
async fn locate(
    config: &Config,
    ctx: &mut DriverCtx,
    known: Option<&KnownAddress>,
) -> Option<Found> {
    let mut ear = upnp::Announcements::open()
        .inspect_err(|e| tracing::debug!(error = %e, "announcements not heard"))
        .ok();
    let mut heard: Option<Location> = None;
    let mut rounds = 0u32;
    loop {
        let expected = known
            .and_then(|k| k.identity.as_deref())
            .filter(|_| rounds < GIVE_UP);
        let found = match heard.take() {
            Some(at) => first_box(vec![at], expected).await,
            None => None,
        };
        if let Some(found) = match found {
            Some(found) => Some(found),
            None => find(config, known, expected).await,
        } {
            return Some(found);
        }
        rounds = rounds.saturating_add(1);
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.igd.nouvel_essai",
            error = moli_i18n::tr!("pilotes.igd.aucune_passerelle")
        ));
        let listening = async {
            if let Some(ear) = &mut ear {
                ear.next(LISTEN, from_a_box).await.map(|a| a.at)
            } else {
                tokio::time::sleep(LISTEN).await;
                None
            }
        };
        tokio::pin!(listening);
        loop {
            tokio::select! {
                at = &mut listening => {
                    heard = at;
                    break;
                }
                command = ctx.next_command() => match command {
                    Some(command) => command.reply(Err(moli_i18n::tr!("pilotes.igd.lecture_seule"))),
                    None => return None,
                },
            }
        }
    }
}

/// Remembers where the box was found; a description that moved (the box
/// restarted, the configured one is out of date) is journaled.
async fn remember(
    ctx: &DriverCtx,
    configured: &str,
    known: Option<&KnownAddress>,
    found: &Found,
    id: &DeviceId,
) {
    let url = found.gw.at.to_string();
    let address = KnownAddress {
        configured: configured.to_owned(),
        host: url.clone(),
        mac: None,
        identity: Some(found.serial.clone()),
    };
    let before = known
        .map(|k| k.host.as_str())
        .or(Some(configured).filter(|c| !c.is_empty()));
    match before {
        Some(before) if before != url => {
            tracing::info!(instance = %ctx.instance(), old = before, new = %url, "internet box found at a new description");
            ctx.moved(Some(id.clone()), before, address).await;
        }
        _ => ctx.remember_address(address).await,
    }
}

/// Consecutive failed polls before looking for the box again.
const REDISCOVER_AFTER: u32 = 6;

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let configured = config.location.clone().unwrap_or_default();
    let known = ctx.known_address().filter(|k| k.configured == configured);
    let Some(found) = locate(config, ctx, known.as_ref()).await else {
        return Ok(());
    };
    let id = ctx.device_id(&found.serial);
    remember(ctx, &configured, known.as_ref(), &found, &id).await;
    let Found {
        mut gw,
        name,
        serial,
    } = found;
    ctx.upsert_device(device(ctx, &id, &name));
    ctx.ready();
    let mut failures = 0u32;
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut online = None;
    loop {
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => command.reply(Err(moli_i18n::tr!("pilotes.igd.lecture_seule"))),
                None => return Ok(()),
            },
            _ = tick.tick() => {
                let status = call(&gw, &gw.ip, "GetStatusInfo").await;
                match status {
                    Ok(status) => {
                        failures = 0;
                        if online != Some(true) {
                            online = Some(true);
                            ctx.set_availability(&id, true);
                        }
                        ctx.set_state(&id, "connected", Value::Bool(tag(&status, "NewConnectionStatus") == Some("Connected")));
                        if let Some(up) = tag(&status, "NewUptime").and_then(|u| u.parse::<i64>().ok()) {
                            ctx.set_state(&id, "uptime", Value::Int(up));
                        }
                        if let Ok(ip) = call(&gw, &gw.ip, "GetExternalIPAddress").await
                            && let Some(ip) = tag(&ip, "NewExternalIPAddress").filter(|ip| !ip.is_empty())
                        {
                            ctx.set_state(&id, "external_ip", Value::Text(ip.into()));
                        }
                        if let Some(common) = &gw.common
                            && let Ok(link) = call(&gw, common, "GetCommonLinkProperties").await
                        {
                            ctx.set_state(&id, "line", Value::Bool(tag(&link, "NewPhysicalLinkStatus") == Some("Up")));
                            // Standard names, and the Livebox's (no « Max » downstream).
                            for (fields, key) in [
                                (["NewLayer1DownstreamMaxBitRate", "NewLayer1DownstreamBitRate"], "line_down"),
                                (["NewLayer1UpstreamMaxBitRate", "NewLayer1UpstreamBitRate"], "line_up"),
                            ] {
                                let bits = fields.iter().find_map(|f| tag(&link, f)).and_then(|b| b.parse::<u64>().ok());
                                if let Some(bits) = bits {
                                    ctx.set_state(&id, key, Value::Float(mbps(bits)));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        failures += 1;
                        if online != Some(false) {
                            online = Some(false);
                            ctx.set_availability(&id, false);
                            tracing::warn!(instance = %ctx.instance(), error = %format!("{:#}", e.context("box")), "internet box unreachable");
                        }
                        if failures.is_multiple_of(REDISCOVER_AFTER) {
                            // Lost a minute: elsewhere (searched), or restarting
                            // (then the driver listens for its announcement).
                            let known = ctx.known_address().filter(|k| k.configured == configured);
                            match find(config, known.as_ref(), Some(&serial)).await {
                                Some(found) if found.serial == serial => {
                                    remember(ctx, &configured, known.as_ref(), &found, &id).await;
                                    gw = found.gw;
                                }
                                _ => anyhow::bail!(moli_i18n::tr!("pilotes.igd.aucune_passerelle")),
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_speeds_in_mbits() {
        assert_eq!(mbps(1_048_576_000), 1048.6);
    }

    #[test]
    fn a_box_is_known_by_its_serial_number_first() {
        let at = Location::parse("http://192.168.0.1:60000/0a1b2c3d/gatedesc.xml").unwrap();
        let description = "<root><device><UDN>uuid:0a1b2c3d-0000-0000-0000-000000000001</UDN>\
            <serialNumber> AB0000000001 </serialNumber><deviceList><device>\
            <serialNumber>AB0000000001</serialNumber><UDN>uuid:other</UDN></device></deviceList></device></root>";
        assert_eq!(identity(description, &at), "AB0000000001");
        let no_serial = "<root><device><UDN>uuid:0a1b2c3d-0000-0000-0000-000000000001</UDN>\
            <serialNumber></serialNumber></device></root>";
        assert_eq!(
            identity(no_serial, &at),
            "0a1b2c3d-0000-0000-0000-000000000001"
        );
        assert_eq!(identity("<root></root>", &at), "192.168.0.1");
    }

    #[test]
    fn only_a_box_s_announcements_count() {
        let heard = |kind: &str| upnp::Announcement {
            kind: kind.into(),
            at: Location::parse("http://192.168.0.1:60000/x/gatedesc.xml").unwrap(),
        };
        assert!(from_a_box(&heard(
            "urn:schemas-upnp-org:device:InternetGatewayDevice:2"
        )));
        assert!(from_a_box(&heard(
            "urn:schemas-upnp-org:service:WANIPConnection:2"
        )));
        assert!(!from_a_box(&heard("upnp:rootdevice")));
        assert!(!from_a_box(&heard(
            "urn:schemas-wifialliance-org:device:WFADevice:1"
        )));
    }
}
