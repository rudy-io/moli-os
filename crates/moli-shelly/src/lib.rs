//! Shelly Gen2+ devices (Plus, Pro, Gen3, Gen4), 100 % local: JSON-RPC over
//! the device's websocket, changes pushed by the device (no polling), a full
//! status read again every minute as a safety net.
//!
//! One instance = one Shelly. Outputs (`switch`) are commanded; meters
//! (`em`/`emdata` of a Pro 3EM, `em1`, `pm1`) and inputs are read. The device
//! is followed by its MAC when the box gives it another address (the reason
//! for this driver's first home: a box restarted, both Shellys moved, and
//! Home Assistant lost them for four days).

mod model;
mod rpc;

use std::net::Ipv4Addr;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use anyhow::Context as _;
use moli_core::{DeviceId, Value};
use moli_net::lan::{self, Looked, Pace};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx, KnownAddress};
use serde::Deserialize;
use serde_json::{Value as Json, json};

pub use model::{Layout, Part};
use rpc::{AuthNeeded, Rpc};

/// The device's password (digest authentication), if it has one.
const PASSWORD: &str = "password";
/// A full status this often, whatever was pushed.
const REFRESH: Duration = Duration::from_secs(60);
/// Nothing pushed nor answered for this long: the connection is dead.
const SILENT: Duration = Duration::from_secs(150);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Its MAC address: where to find it if the box gives it another address
    /// before Moli learned it (Moli learns it once the device answered).
    #[serde(default)]
    pub mac: Option<String>,
}

fn default_port() -> u16 {
    80
}

#[derive(Debug)]
pub struct Shelly {
    config: Config,
    /// When the device may next be looked for elsewhere (kept across restarts).
    pace: Mutex<Pace>,
}

impl Shelly {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            pace: Mutex::new(Pace::default()),
        }
    }
}

impl Driver for Shelly {
    fn kind(&self) -> &'static str {
        "shelly"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

/// What a Shelly answers first: who it is, how it is set up, where it stands.
struct First {
    rpc: Rpc,
    info: Json,
    config: Json,
    status: Json,
}

async fn open(host: &str, port: u16, password: Option<String>) -> anyhow::Result<First> {
    let mut rpc = Rpc::open(host, port, password).await?;
    let info = rpc.call("Shelly.GetDeviceInfo", Json::Null).await?;
    let config = rpc.call("Shelly.GetConfig", Json::Null).await?;
    let status = rpc.call("Shelly.GetStatus", Json::Null).await?;
    Ok(First {
        rpc,
        info,
        config,
        status,
    })
}

/// Who the device says it is (its MAC, lower-case hex).
fn identity(info: &Json) -> Option<String> {
    info["mac"].as_str().and_then(lan::mac_hex)
}

/// Asks who answers at `ip` (no password needed: `GetDeviceInfo` is open).
async fn who_is_at(ip: Ipv4Addr, port: u16) -> Option<String> {
    let mut rpc = Rpc::open(&ip.to_string(), port, None).await.ok()?;
    identity(&rpc.call("Shelly.GetDeviceInfo", Json::Null).await.ok()?)
}

impl Shelly {
    /// Looks for the device elsewhere on the home network (by its MAC); an
    /// address is taken only if the device there says it is `mac`.
    async fn look(
        &self,
        ctx: &DriverCtx,
        host: &mut String,
        configured: &str,
        mac: &str,
    ) -> Looked {
        let Ok(old) = host.parse::<Ipv4Addr>() else {
            return Looked::Nowhere;
        };
        let port = self.config.port;
        let looked = lan::relocate(&self.pace, mac, old, |ip| async move {
            who_is_at(ip, port).await.is_some_and(|found| found == mac)
        })
        .await;
        if let Looked::Found(ip) = looked {
            let new = ip.to_string();
            let address = KnownAddress {
                configured: configured.to_owned(),
                host: new.clone(),
                mac: Some(mac.to_owned()),
                identity: Some(mac.to_owned()),
            };
            tracing::info!(instance = %ctx.instance(), old = %host, new = %new, "shelly found at a new address");
            ctx.moved(Some(ctx.device_id(mac)), host, address).await;
            *host = new;
        }
        looked
    }

    /// Opens the device, following it if the box gave it another address,
    /// and waits (refusing orders) while it cannot be reached. `None`: the
    /// driver is stopping.
    async fn reach(
        &self,
        ctx: &mut DriverCtx,
        configured: &str,
        follow: bool,
        password: Option<&str>,
    ) -> Option<(String, First)> {
        let (mut host, expected) = whereabouts(ctx, &self.config, follow);
        loop {
            let error = match open(&host, self.config.port, password.map(str::to_owned)).await {
                Ok(first) => {
                    let who = identity(&first.info);
                    if expected.is_none() || who == expected {
                        return Some((host, first));
                    }
                    moli_i18n::tr!("pilotes.shelly.un_autre")
                }
                Err(e) => {
                    if let Some(auth) = e.downcast_ref::<AuthNeeded>() {
                        let message = if auth.refused {
                            moli_i18n::tr!(
                                "pilotes.shelly.mot_de_passe_refuse",
                                instance = ctx.instance(),
                                key = PASSWORD
                            )
                        } else {
                            moli_i18n::tr!(
                                "pilotes.shelly.mot_de_passe_manquant",
                                instance = ctx.instance(),
                                key = PASSWORD
                            )
                        };
                        ctx.wait_for(message);
                        ctx.cancelled().await;
                        return None;
                    }
                    format!("{e:#}")
                }
            };
            if follow {
                if host != configured {
                    configured.clone_into(&mut host);
                    continue;
                }
                if let Some(mac) = expected.as_deref()
                    && let Looked::Found(_) = self.look(ctx, &mut host, configured, mac).await
                {
                    continue;
                }
            }
            if !wait_a_minute(ctx, &error).await {
                return None;
            }
        }
    }
}

/// Where to reach the device first (where it answered last), and who it is
/// (remembered, or configured). A person who gives another address or
/// another MAC is obeyed.
fn whereabouts(ctx: &DriverCtx, config: &Config, follow: bool) -> (String, Option<String>) {
    let given = config.mac.as_deref().and_then(lan::mac_hex);
    match ctx.known_address().filter(|_| follow) {
        Some(k)
            if k.configured != config.host
                || given.as_ref().is_some_and(|g| k.mac.as_ref() != Some(g)) =>
        {
            (config.host.clone(), given)
        }
        Some(k) => (k.host, k.mac.or(given)),
        None => (config.host.clone(), given),
    }
}

/// Waits a minute for a device that cannot be reached, refusing orders.
/// `false`: the driver is stopping.
async fn wait_a_minute(ctx: &mut DriverCtx, error: &str) -> bool {
    ctx.wait_for(moli_i18n::tr!(
        "pilotes.shelly.injoignable_nouvel_essai",
        error = error
    ));
    let wake = tokio::time::sleep(Duration::from_secs(60));
    tokio::pin!(wake);
    loop {
        tokio::select! {
            () = &mut wake => return true,
            command = ctx.next_command() => match command {
                Some(command) => command.reply(Err(moli_i18n::tr!("pilotes.shelly.injoignable"))),
                None => return false,
            },
        }
    }
}

fn publish(ctx: &DriverCtx, layout: &Layout, status: &Json) {
    for (native, key, value) in model::readings(layout, status) {
        ctx.set_state(&ctx.device_id(&native), &key, value);
    }
}

async fn apply(rpc: &mut Rpc, layout: &Layout, command: &CommandRequest) -> anyhow::Result<()> {
    let native = command
        .device
        .id
        .as_str()
        .split_once(':')
        .map_or("", |(_, n)| n);
    match (
        model::switch_of(layout, native),
        &*command.key,
        &command.value,
    ) {
        (Some(id), "on", Value::Bool(on)) => {
            rpc.call("Switch.Set", json!({ "id": id, "on": on }))
                .await?;
            Ok(())
        }
        (_, key, _) => anyhow::bail!(moli_i18n::tr!(
            "pilotes.shelly.ne_se_regle_pas",
            point = key
        )),
    }
}

async fn run(shelly: &Shelly, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let config = &shelly.config;
    let password = ctx.secret(PASSWORD);
    let configured = config.host.clone();
    let follow = configured
        .parse::<Ipv4Addr>()
        .is_ok_and(|ip| ip.is_private());
    let Some((host, first)) = shelly
        .reach(ctx, &configured, follow, password.as_deref())
        .await
    else {
        return Ok(());
    };
    let First {
        mut rpc,
        info,
        config: device_config,
        status,
    } = first;
    let layout = Layout::read(&info, &device_config, &status)
        .context(moli_i18n::tr!("pilotes.shelly.illisible"))?;
    let devices = layout.devices(ctx.instance(), |native| ctx.device_id(native));
    let ids: Vec<DeviceId> = devices.iter().map(|d| d.id.clone()).collect();
    for device in devices {
        ctx.upsert_device(device);
    }
    for id in &ids {
        ctx.set_availability(id, true);
    }
    publish(ctx, &layout, &status);
    ctx.ready();
    if follow {
        let seen = match host.parse::<Ipv4Addr>() {
            Ok(ip) => lan::mac_of(ip).await,
            Err(_) => None,
        };
        ctx.remember_address(KnownAddress {
            configured: configured.clone(),
            host: host.clone(),
            mac: seen.or_else(|| Some(layout.mac.clone())),
            identity: Some(layout.mac.clone()),
        })
        .await;
    }
    shelly
        .pace
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .reset();

    let mut refresh = tokio::time::interval(REFRESH);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    refresh.tick().await;
    let ended = loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                match apply(&mut rpc, &layout, &command).await {
                    Ok(()) => {
                        ctx.set_state(&command.device.id, &command.key, command.value.clone());
                        command.reply(Ok(()));
                    }
                    Err(e) => {
                        command.reply(Err(format!("{e:#}")));
                        if e.downcast_ref::<AuthNeeded>().is_some() {
                            break e;
                        }
                    }
                }
            }
            pushed = rpc.pushed(SILENT) => match pushed {
                Ok(message) => if let Some("NotifyStatus" | "NotifyFullStatus") = message["method"].as_str() {
                    publish(ctx, &layout, &message["params"]);
                },
                Err(e) => break e,
            },
            _ = refresh.tick() => match rpc.call("Shelly.GetStatus", Json::Null).await {
                Ok(status) => publish(ctx, &layout, &status),
                Err(e) => break e,
            },
        }
    };
    for id in &ids {
        ctx.set_availability(id, false);
    }
    // The supervisor opens it again (and looks for it elsewhere if needed).
    Err(ended.context(moli_i18n::tr!("pilotes.shelly.connexion_perdue")))
}
