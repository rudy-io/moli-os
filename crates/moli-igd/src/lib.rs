//! The internet box (Livebox, Freebox… any UPnP Internet Gateway Device):
//! is the connection up, public address, uptime, line speed. Found by SSDP
//! (or a configured description URL).
//!
//! No throughput: boxes' UPnP byte counters are unreliable (a Livebox
//! reports gigabytes per second on a 1 Gbit/s line). A wrong number is
//! worse than none.

use std::time::Duration;

use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::upnp::{self, Location, tag};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;

const POLL: Duration = Duration::from_secs(10);
const WAN_IP: &str = "urn:schemas-upnp-org:service:WANIPConnection:";
const WAN_COMMON: &str = "urn:schemas-upnp-org:service:WANCommonInterfaceConfig:";

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

async fn find(config: &Config) -> anyhow::Result<(Gateway, String, String)> {
    // The configured description first, then whatever answers a search
    // (the configured path may have changed after a reboot).
    let mut locations: Vec<Location> = config
        .location
        .as_deref()
        .and_then(Location::parse)
        .into_iter()
        .collect();
    // Some boxes (Livebox) only answer the generic search: ask every root
    // device, keep the one with a WAN service.
    locations.extend(
        upnp::discover("upnp:rootdevice", Duration::from_secs(3))
            .await
            .unwrap_or_default(),
    );
    for at in locations {
        let Ok(description) = upnp::fetch(&at.host, at.port, &at.path).await else {
            continue;
        };
        if let Some(ip) = upnp::control_url(&description, WAN_IP) {
            let name = tag(&description, "friendlyName")
                .map_or_else(|| moli_i18n::tr!("pilotes.igd.box_internet"), str::to_owned);
            let serial = tag(&description, "UDN")
                .unwrap_or(&at.host)
                .trim_start_matches("uuid:")
                .to_owned();
            let common = upnp::control_url(&description, WAN_COMMON);
            return Ok((Gateway { at, ip, common }, name, serial));
        }
    }
    anyhow::bail!(moli_i18n::tr!("pilotes.igd.aucune_passerelle"))
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

/// Finds the box, waiting (and refusing orders) while it cannot be found.
/// `None`: the driver is stopping.
async fn locate(config: &Config, ctx: &mut DriverCtx) -> Option<(Gateway, String, String)> {
    loop {
        match find(config).await {
            Ok(found) => return Some(found),
            Err(e) => ctx.wait_for(moli_i18n::tr!(
                "pilotes.igd.nouvel_essai",
                error = format!("{e:#}")
            )),
        }
        let wake = tokio::time::sleep(Duration::from_secs(60));
        tokio::pin!(wake);
        loop {
            tokio::select! {
                () = &mut wake => break,
                command = ctx.next_command() => match command {
                    Some(command) => command.reply(Err(moli_i18n::tr!("pilotes.igd.lecture_seule"))),
                    None => return None,
                },
            }
        }
    }
}

/// Consecutive failed polls before looking for the box again.
const REDISCOVER_AFTER: u32 = 6;

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let Some((mut gw, name, serial)) = locate(config, ctx).await else {
        return Ok(());
    };
    let id = ctx.device_id(&serial);
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
                        if failures.is_multiple_of(REDISCOVER_AFTER)
                            && let Ok((found, _, found_serial)) = find(config).await
                            && found_serial == serial
                        {
                            gw = found;
                        }
                        if online != Some(false) {
                            online = Some(false);
                            ctx.set_availability(&id, false);
                            tracing::warn!(instance = %ctx.instance(), error = %format!("{:#}", e.context("box")), "internet box unreachable");
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
}
