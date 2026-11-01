//! Local Somfy developer-mode HTTPS with optional independent cloud observation.
mod client;
pub mod cloud;
pub mod inventory;
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
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub cloud: Option<cloud::Config>,
    #[serde(default = "port")]
    pub port: u16,
    #[serde(default = "poll")]
    pub poll_secs: u64,
    #[serde(default)]
    pub cert_sha256: Option<String>,
    #[serde(default)]
    pub writable_openers: Vec<ReviewedOpener>,
    #[serde(default)]
    pub read_only: bool,
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
        if (config.host.is_none() && config.cloud.is_none())
            || config.host.as_ref().is_some_and(|host| {
                host.is_empty()
                    || host.len() > 253
                    || !host
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            })
            || config
                .cloud
                .as_ref()
                .is_some_and(|cloud| cloud.validate().is_err())
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
    // Explicit source ownership survives supervisor restarts and config changes.
    for id in ctx.devices() {
        ctx.set_availability(&id, false);
        if (cloud::owns_device(&id) && config.cloud.is_none())
            || (!cloud::owns_device(&id) && config.host.is_none())
        {
            ctx.remove_device(&id);
        }
    }
    let Some(cloud_config) = &config.cloud else {
        return run_native(config, ctx).await;
    };
    let cloud = Some(cloud::worker::Controller::new(ctx, cloud_config));
    run_sources(config, ctx, cloud).await
}

async fn native_client(config: &Config, ctx: &DriverCtx) -> anyhow::Result<Client> {
    let token = ctx
        .secret("token")
        .filter(|t| !t.is_empty() && t.len() <= 8192 && http::HeaderValue::from_str(t).is_ok())
        .ok_or(Failure::Auth)?;
    let cached = ctx.secret("cert_sha256");
    if cached.as_ref().is_some_and(|p| !valid_pin(p)) && config.cert_sha256.is_none() {
        return Err(Failure::Certificate.into());
    }
    let host = config
        .host
        .as_deref()
        .context(moli_i18n::tr!("overkiz.invalid"))?;
    let pin = if let Some(pin) = config
        .cert_sha256
        .clone()
        .or(cached)
        .filter(|p| valid_pin(p))
    {
        pin
    } else {
        if !ctx.can_store_secrets() {
            anyhow::bail!(moli_i18n::tr!("overkiz.vault"));
        }
        let probe = PinnedHttps::new(host, config.port, None)?;
        probe.handshake().await.map_err(|e| client::network(&e))?;
        let pin = probe
            .pinned_fingerprint()
            .context(moli_i18n::tr!("overkiz.cert"))?;
        ctx.store_secret("cert_sha256", &pin)
            .await
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("overkiz.vault")))?;
        pin
    };
    Ok(Client::new(
        PinnedHttps::new(host, config.port, Some(pin))?,
        token,
    ))
}

async fn run_sources(
    config: &Config,
    ctx: &mut DriverCtx,
    mut cloud: Option<cloud::worker::Controller>,
) -> anyhow::Result<()> {
    let mut native = None;
    let mut native_live = false;
    let mut native_warning = None;
    let mut native_parked = false;
    let mut retry = tokio::time::Instant::now();
    let mut native_timer = tokio::time::interval(Duration::from_secs(config.poll_secs));
    native_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut cloud_timer = tokio::time::interval(Duration::from_secs(5));
    cloud_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        reconcile_status(ctx, native_live, native_warning.as_deref(), cloud.as_ref());
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()); };
                let result = if cloud::owns_device(&command.device.id) || config.read_only { Err(anyhow::anyhow!(moli_i18n::tr!("overkiz.refused"))) } else if let Some(client) = &native {
                    let deadline = command.deadline.min(Instant::now() + Duration::from_millis(4500));
                    let operation = tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), apply(client, ctx, &command, deadline, config));
                    cloud::worker::during(operation, &mut cloud, ctx).await.map_err(|_| anyhow::anyhow!(moli_i18n::tr!("overkiz.timeout"))).and_then(|r| r)
                } else { Err(anyhow::anyhow!(moli_i18n::tr!("overkiz.refused"))) };
                // A policy refusal is not a source failure. Transport failures are retried independently.
                if let Err(error) = &result
                    && error.to_string() != moli_i18n::tr!("overkiz.refused")
                {
                    native_warning = Some(error.to_string());
                    native_parked = error.downcast_ref::<Failure>().is_some();
                    native_live = false; native = None;
                    retry = tokio::time::Instant::now() + Duration::from_secs(60);
                }
                command.reply(result.map_err(|error| error.to_string()));
            },
            _ = native_timer.tick(), if config.host.is_some() && !native_parked => {
                if tokio::time::Instant::now() >= retry {
                    let operation = async {
                        let client = if let Some(client) = native.take() { client } else { native_client(config, ctx).await? };
                        refresh(&client, ctx, config).await?;
                        Ok::<_, anyhow::Error>(client)
                    };
                    match cloud::worker::during(operation, &mut cloud, ctx).await {
                        Ok(client) => { native = Some(client); native_live = true; native_warning = None; },
                        Err(error) => {
                            native_parked = error.downcast_ref::<Failure>().is_some();
                            native_warning = Some(error.to_string()); native_live = false;
                            retry = tokio::time::Instant::now() + Duration::from_secs(60);
                        }
                    }
                }
            },
            _ = cloud_timer.tick(), if cloud.is_some() => {
                if let Some(cloud) = &mut cloud { cloud.tick(ctx); }
            },
            event = async { cloud.as_mut().expect("enabled branch").next_event().await }, if cloud.as_ref().is_some_and(cloud::worker::Controller::receiving) => {
                if let Some(cloud) = &mut cloud { cloud.handle(ctx, event); }
            }
        }
    }
}
fn reconcile_status(
    ctx: &DriverCtx,
    native_live: bool,
    native_warning: Option<&str>,
    cloud: Option<&cloud::worker::Controller>,
) {
    // set_availability removes held_offline entries BEFORE instance-wide ready can restore them.
    for id in ctx.devices() {
        if (!cloud::owns_device(&id) && !native_live)
            || (cloud::owns_device(&id) && cloud.is_none_or(|c| !c.live))
        {
            ctx.set_availability(&id, false);
        }
    }
    ctx.ready();
    let warnings: Vec<_> = native_warning
        .into_iter()
        .chain(cloud.and_then(|c| c.warning.as_deref()))
        .collect();
    if warnings.is_empty() {
        ctx.clear_warning();
    } else {
        ctx.warn(warnings.join(" · "));
    }
}

async fn run_native(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let client = match native_client(config, ctx).await {
        Ok(client) => client,
        Err(error) if error.downcast_ref::<Failure>().is_some() => return Err(error),
        Err(error) if !ctx.can_store_secrets() => {
            wait(ctx, error.to_string()).await;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    refresh(&client, ctx, config).await?;
    ctx.ready();
    let mut timer = tokio::time::interval(Duration::from_secs(config.poll_secs));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    timer.tick().await;
    loop {
        tokio::select! {
            command=ctx.next_command()=>{
                let Some(command)=command else {return Ok(());};
                let deadline=command.deadline.min(Instant::now()+Duration::from_millis(4500));
                let result=tokio::time::timeout(deadline.saturating_duration_since(Instant::now()),apply(&client,ctx,&command,deadline,config)).await.map_err(|_|anyhow::anyhow!(moli_i18n::tr!("overkiz.timeout"))).and_then(|r|r);
                let fatal=result.is_err();
                command.reply(result.as_ref().copied().map_err(ToString::to_string));
                if fatal {return result;}
            },
            _=timer.tick()=>{refresh(&client,ctx,config).await?;},
        }
    }
}
async fn refresh(
    client: &Client,
    ctx: &DriverCtx,
    config: &Config,
) -> anyhow::Result<Vec<model::Entry>> {
    let json = client.request(Method::GET, "/setup/devices", None).await?;
    let entries = model::parse_policy(&json, &config.writable_openers, config.read_only)?;
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
        ctx.set_state(&id, "favourite_position", entry.favourite());
        ids.push(id);
    }
    for old in ctx.devices() {
        if !cloud::owns_device(&old) && !ids.contains(&old) {
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
    config: &Config,
) -> anyhow::Result<()> {
    if cloud::owns_device(&command.device.id) || config.read_only {
        anyhow::bail!(moli_i18n::tr!("overkiz.refused"));
    }
    if command.abandoned() {
        anyhow::bail!(moli_i18n::tr!("overkiz.timeout"));
    }
    let entries = refresh(client, ctx, config).await?;
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

#[cfg(test)]
mod runtime_tests;
