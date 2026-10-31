//! Native Somfy developer-mode local HTTPS. No cloud login, no inferred positions.
mod client;
mod model;
use anyhow::Context as _;
use client::{Client, Failure};
use moli_core::DeviceId;
use moli_net::{Method, PinnedHttps};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedOpener {
    pub device_url: String,
    pub ui_class: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "port")]
    pub port: u16,
    #[serde(default = "poll")]
    pub poll_secs: u64,
    #[serde(default)]
    pub cert_sha256: Option<String>,
    #[serde(default)]
    pub writable_openers: Vec<ReviewedOpener>,
}
fn port() -> u16 {
    8443
}
fn poll() -> u64 {
    5
}
#[derive(Debug)]
pub struct Overkiz {
    config: Config,
}
impl Overkiz {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        if config.host.is_empty()
            || config.host.len() > 253
            || !config
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            || config.port == 0
            || !(1..=300).contains(&config.poll_secs)
            || config.cert_sha256.as_ref().is_some_and(|p| !valid_pin(p))
        {
            anyhow::bail!(moli_i18n::tr!("overkiz.invalid"));
        }
        let mut seen = std::collections::HashSet::new();
        if config.writable_openers.len() > 128
            || config.writable_openers.iter().any(|p| {
                !matches!(p.ui_class.as_str(), "Gate" | "Window")
                    || p.device_url.len() > 512
                    || !(p.device_url.starts_with("io://") || p.device_url.starts_with("rts://"))
                    || p.device_url.chars().any(char::is_control)
                    || !seen.insert(&p.device_url)
            })
        {
            anyhow::bail!(moli_i18n::tr!("overkiz.invalid"));
        }
        Ok(Self { config })
    }
}
fn valid_pin(pin: &str) -> bool {
    pin.len() == 64 && pin.bytes().all(|b| b.is_ascii_hexdigit())
}
impl Driver for Overkiz {
    fn kind(&self) -> &'static str {
        "overkiz"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let outcome = run(&self.config, ctx).await;
            if let Err(error) = &outcome {
                for id in ctx.devices() {
                    ctx.set_availability(&id, false);
                }
                if error.downcast_ref::<Failure>().is_some() {
                    ctx.wait_for(error.to_string());
                    ctx.cancelled().await;
                    return Ok(());
                }
            }
            outcome
        })
    }
}
async fn wait(ctx: &DriverCtx, message: String) {
    ctx.wait_for(message);
    ctx.cancelled().await;
}
async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let Some(token) = ctx
        .secret("token")
        .filter(|t| !t.is_empty() && t.len() <= 8192 && http::HeaderValue::from_str(t).is_ok())
    else {
        wait(ctx, moli_i18n::tr!("overkiz.auth")).await;
        return Ok(());
    };
    let cached = ctx.secret("cert_sha256");
    if cached.as_ref().is_some_and(|p| !valid_pin(p)) && config.cert_sha256.is_none() {
        return Err(Failure::Certificate.into());
    }
    let pin = config
        .cert_sha256
        .clone()
        .or(cached)
        .filter(|p| valid_pin(p));
    let pin = if let Some(pin) = pin {
        pin
    } else {
        if !ctx.can_store_secrets() {
            wait(ctx, moli_i18n::tr!("overkiz.vault")).await;
            return Ok(());
        }
        let probe = PinnedHttps::new(&config.host, config.port, None)?;
        probe.handshake().await.map_err(|e| client::network(&e))?;
        let pin = probe
            .pinned_fingerprint()
            .context(moli_i18n::tr!("overkiz.cert"))?;
        ctx.store_secret("cert_sha256", &pin)
            .await
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("overkiz.vault")))?;
        pin
    };
    let client = Client::new(
        PinnedHttps::new(&config.host, config.port, Some(pin))?,
        token,
    );
    refresh(&client, ctx, &config.writable_openers).await?;
    ctx.ready();
    let mut timer = tokio::time::interval(Duration::from_secs(config.poll_secs));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    timer.tick().await;
    loop {
        tokio::select! {
            command=ctx.next_command()=>{
                let Some(command)=command else {return Ok(());};
                let deadline=command.deadline.min(Instant::now()+Duration::from_millis(4500));
                let result=tokio::time::timeout(deadline.saturating_duration_since(Instant::now()),apply(&client,ctx,&command,deadline,&config.writable_openers)).await.map_err(|_|anyhow::anyhow!(moli_i18n::tr!("overkiz.timeout"))).and_then(|r|r);
                let fatal=result.is_err();
                command.reply(result.as_ref().copied().map_err(ToString::to_string));
                if fatal {return result;}
            },
            _=timer.tick()=>{refresh(&client,ctx,&config.writable_openers).await?;},
        }
    }
}
async fn refresh(
    client: &Client,
    ctx: &DriverCtx,
    reviewed: &[ReviewedOpener],
) -> anyhow::Result<Vec<model::Entry>> {
    let json = client.request(Method::GET, "/setup/devices", None).await?;
    let entries = model::parse_reviewed(&json, reviewed)?;
    let mut ids = vec![];
    for entry in &entries {
        let device = entry.device(ctx.instance());
        if device.points.is_empty() {
            continue;
        }
        let id = device.id.clone();
        ctx.upsert_device(device);
        ctx.set_availability(&id, entry.online);
        ctx.set_state(&id, "percent_control", entry.position());
        ctx.set_state(&id, "opening_state", entry.opening());
        ids.push(id);
    }
    for old in ctx.devices() {
        if !ids.contains(&old) {
            ctx.remove_device(&old);
        }
    }
    Ok(entries)
}
async fn apply(
    client: &Client,
    ctx: &DriverCtx,
    command: &CommandRequest,
    deadline: Instant,
    reviewed: &[ReviewedOpener],
) -> anyhow::Result<()> {
    if command.abandoned() {
        anyhow::bail!(moli_i18n::tr!("overkiz.timeout"));
    }
    let entries = refresh(client, ctx, reviewed).await?;
    let entry = entries
        .iter()
        .find(|e| DeviceId::new(ctx.instance(), &e.native()) == command.device.id)
        .context(moli_i18n::tr!("overkiz.refused"))?;
    let fresh_device = entry.device(ctx.instance());
    let fresh = fresh_device
        .point(&command.key)
        .filter(|p| p.access.write)
        .context(moli_i18n::tr!("overkiz.refused"))?;
    let admitted = command
        .device
        .point(&command.key)
        .context(moli_i18n::tr!("overkiz.refused"))?;
    if fresh.semantic != admitted.semantic
        || moli_core::validate(&fresh.kind, &command.value).is_err()
    {
        anyhow::bail!(moli_i18n::tr!("overkiz.refused"));
    }
    let body = entry.apply(&command.key, &command.value)?;
    if command.abandoned() {
        anyhow::bail!(moli_i18n::tr!("overkiz.timeout"));
    }
    client.apply_before(&body, deadline).await
}
#[cfg(test)]
mod tests;
