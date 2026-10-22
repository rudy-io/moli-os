//! Moli OS as a HomeKit bridge: its security stations, detectors and
//! doorbells in Apple's Home app, on the home network (HAP over IP, mDNS).
//!
//! The setup code is the person's: chosen, filed in the vault
//! (`moli-os secrets set homekit setup_code`), never shown. Until it is
//! there and nobody is paired, the bridge does not offer itself. Orders
//! from the Home app arrive as `Origin::HomeKit`: only an Apple device
//! paired with that code reaches the bridge.

mod bridge;
mod mdns;
mod pairing;
mod server;
mod srp;
mod tlv8;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use anyhow::Context as _;
use moli_core::{Event, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx, Hub};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, watch};

use crate::bridge::{Binding, Source};

/// The instance HomeKit runs as (its secrets live there).
pub const INSTANCE: &str = "homekit";
const CODE: &str = "setup_code";
const IDENTITY: &str = "identity";
/// Accessory ids and the configuration number: not secret, but kept with
/// the rest so that HomeKit's memory of rooms and names survives.
const LAYOUT: &str = "layout";
/// Devices changed: rebuilt after things settle.
const SETTLE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The bridge's name in the Home app.
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_name() -> String {
    "Moli".into()
}

fn default_port() -> u16 {
    51826
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Layout {
    aids: BTreeMap<String, u64>,
    config: u32,
    hash: String,
    /// Every device announced so far, as last seen.
    #[serde(default)]
    sources: BTreeMap<String, Source>,
}

#[derive(Debug)]
pub struct HomeKit {
    hub: Hub,
    config: Config,
}

impl HomeKit {
    #[must_use]
    pub fn new(hub: Hub, config: Config) -> Self {
        Self { hub, config }
    }
}

impl Driver for HomeKit {
    fn kind(&self) -> &'static str {
        "homekit"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.hub, &self.config, ctx))
    }
}

/// `XXX-XX-XXX` from 8 digits (with or without dashes); trivial codes,
/// which HomeKit refuses, are not accepted.
fn setup_code(text: &str) -> Option<String> {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 8
        || text
            .chars()
            .any(|c| !c.is_ascii_digit() && c != '-' && !c.is_whitespace())
    {
        return None;
    }
    let trivial = ["12345678", "87654321"].contains(&digits.as_str())
        || digits.chars().all(|c| digits.starts_with(c));
    if trivial {
        return None;
    }
    Some(format!(
        "{}-{}-{}",
        &digits[..3],
        &digits[3..5],
        &digits[5..]
    ))
}

fn sources(hub: &Hub) -> Vec<Source> {
    hub.snapshot()
        .devices
        .into_iter()
        .map(|view| Source {
            name: view
                .label
                .name
                .clone()
                .unwrap_or_else(|| view.device.native_name.to_string()),
            manufacturer: view
                .device
                .manufacturer
                .as_deref()
                .unwrap_or("Moli")
                .to_owned(),
            model: view.device.model.as_deref().unwrap_or("Moli").to_owned(),
            keys: view
                .device
                .points
                .iter()
                .map(|p| p.key.to_string())
                .collect(),
            id: view.device.id.clone(),
        })
        .collect()
}

fn labels() -> bridge::Labels {
    bridge::Labels {
        motion: moli_i18n::tr!("homekit.mouvement"),
        person: moli_i18n::tr!("homekit.personne"),
        doorbell: moli_i18n::tr!("homekit.sonnette"),
    }
}

fn digest(text: &str) -> String {
    use std::fmt::Write as _;
    ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// The accessories now, and the layout kept for them (ids, number).
fn rebuild(hub: &Hub, config: &Config, layout: &mut Layout) -> (bridge::Accessories, bool) {
    let mut changed = false;
    for source in sources(hub).into_iter().filter(Source::shown) {
        let id = source.id.as_str().to_owned();
        if layout.sources.get(&id) != Some(&source) {
            layout.sources.insert(id, source);
            changed = true;
        }
    }
    let sources: Vec<Source> = layout.sources.values().cloned().collect();
    changed |= bridge::assign(&mut layout.aids, &sources);
    let built = bridge::build(&config.name, &sources, &layout.aids, &labels());
    let hash = digest(&built.json.to_string());
    if hash != layout.hash {
        layout.hash = hash;
        // 1 to 65535, then around again.
        layout.config = layout.config % 65535 + 1;
        changed = true;
    }
    (built, changed)
}

fn txt(config: &Config, identity: &pairing::Identity, layout: &Layout) -> Vec<String> {
    vec![
        format!("c#={}", layout.config),
        "ff=0".into(),
        format!("id={}", identity.id),
        format!("md={}", config.name),
        "pv=1.1".into(),
        "s#=1".into(),
        format!("sf={}", u8::from(!identity.paired())),
        "ci=2".into(),
    ]
}

async fn keep<T: Serialize>(ctx: &DriverCtx, name: &str, value: &T) -> anyhow::Result<()> {
    ctx.store_secret(name, &serde_json::to_string(value)?)
        .await
        .with_context(|| format!("keeping the HomeKit {name}"))
}

#[allow(clippy::too_many_lines)]
async fn run(hub: &Hub, config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let instance = ctx.instance().to_string();
    if !ctx.can_store_secrets() {
        anyhow::bail!("HomeKit needs the secret store (its keys and pairings live there)");
    }
    let stored = ctx
        .secret(IDENTITY)
        .and_then(|t| serde_json::from_str(&t).ok());
    let identity = if let Some(identity) = stored {
        identity
    } else {
        let identity = pairing::Identity::new()?;
        keep(ctx, IDENTITY, &identity).await?;
        identity
    };
    let code = ctx.secret(CODE).as_deref().and_then(setup_code);
    if code.is_none() && !identity.paired() {
        let reason = if ctx.secret(CODE).is_some() {
            moli_i18n::tr!("homekit.code_invalide", instance = instance, code = CODE)
        } else {
            moli_i18n::tr!("homekit.code_manquant", instance = instance, code = CODE)
        };
        ctx.wait_for(reason);
        ctx.cancelled().await;
        return Ok(());
    }
    let mut layout: Layout = ctx
        .secret(LAYOUT)
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    // The hub's devices arrive as their drivers start: subscribe first.
    let mut hub_events = hub.subscribe();
    let (accessories, changed) = rebuild(hub, config, &mut layout);
    if changed {
        keep(ctx, LAYOUT, &layout).await?;
    }
    let ip = mdns::local_ip()?;
    let host = format!(
        "moli-{}",
        identity
            .id
            .replace(':', "")
            .to_ascii_lowercase()
            .get(8..)
            .unwrap_or("hk")
    );
    let (record_tx, record_rx) = watch::channel(mdns::Record {
        name: config.name.clone(),
        host,
        ip,
        port: config.port,
        txt: txt(config, &identity, &layout),
    });
    let (events, _) = broadcast::channel(64);
    let (keep_tx, mut keep_rx) = mpsc::channel(4);
    let shared = Arc::new(server::Shared {
        hub: hub.clone(),
        identity: Mutex::new(identity),
        code,
        accessories: RwLock::new(accessories),
        events: events.clone(),
        keep: keep_tx,
        setup: Mutex::new(server::SetupGate::default()),
    });
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port))
        .await
        .with_context(|| format!("HomeKit port {}", config.port))?;
    let responder = Abort(tokio::spawn(async move {
        if let Err(e) = mdns::serve(record_rx).await {
            tracing::warn!(error = format!("{e:#}"), "homekit: mDNS stopped");
        }
    }));
    let mut connections = tokio::task::JoinSet::new();
    let mut settle: Option<tokio::time::Instant> = None;
    tracing::info!(instance, port = config.port, %ip, "homekit: bridge listening");
    ctx.ready();
    let _responder = responder;
    loop {
        let wake =
            settle.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            command = ctx.next_command() => match command {
                Some(command) => command.reply(Err("HomeKit has no points".into())),
                None => return Ok(()),
            },
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let shared = Arc::clone(&shared);
                connections.spawn(async move {
                    if let Err(e) = server::connection(shared, stream).await {
                        tracing::debug!(error = %e, "homekit: connection closed");
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            Some(identity) = keep_rx.recv() => {
                keep(ctx, IDENTITY, &identity).await?;
                tracing::info!(instance, controllers = identity.controllers.len(), "homekit: pairings changed");
                record_tx.send_modify(|r| r.txt = txt(config, &identity, &layout));
            }
            event = hub_events.recv() => match event {
                Ok(Event::State { point, value, .. }) => {
                    let accessories = shared.accessories.read().unwrap_or_else(PoisonError::into_inner);
                    for ((aid, iid), c) in &accessories.chars {
                        let out = match &c.binding {
                            Binding::Press(p) if *p == point => matches!(value, Value::Bool(true)).then(|| serde_json::json!(0)),
                            binding if binding.watches(&point) => {
                                Some(server::value(&shared.hub, &c.binding))
                            }
                            _ => None,
                        };
                        if let Some(out) = out {
                            let _ = events.send((*aid, *iid, out));
                        }
                    }
                }
                Ok(Event::DeviceUpserted { .. } | Event::DeviceRemoved { .. } | Event::LabelChanged { .. }) => {
                    settle = Some(tokio::time::Instant::now() + SETTLE);
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => settle = Some(tokio::time::Instant::now() + SETTLE),
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            () = tokio::time::sleep_until(wake), if settle.is_some() => {
                settle = None;
                let (accessories, changed) = rebuild(hub, config, &mut layout);
                *shared.accessories.write().unwrap_or_else(PoisonError::into_inner) = accessories;
                if changed {
                    keep(ctx, LAYOUT, &layout).await?;
                    let identity = shared.identity.lock().unwrap_or_else(PoisonError::into_inner).clone();
                    record_tx.send_modify(|r| r.txt = txt(config, &identity, &layout));
                }
            }
        }
    }
}

/// Stops the task when the bridge stops.
struct Abort(tokio::task::JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_codes_are_written_like_homekit_s() {
        assert_eq!(setup_code("48213095").as_deref(), Some("482-13-095"));
        assert_eq!(setup_code("482-13-095").as_deref(), Some("482-13-095"));
        assert_eq!(setup_code(" 482 13 095\n").as_deref(), Some("482-13-095"));
        assert!(setup_code("12345678").is_none());
        assert!(setup_code("11111111").is_none());
        assert!(setup_code("4821309").is_none());
        assert!(setup_code("48213O95").is_none());
    }
}
