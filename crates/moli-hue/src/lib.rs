//! Philips Hue driver (CLIP v2).
//!
//! Pairing is the bridge's own ceremony: the driver asks, a human presses
//! the link button, the bridge answers with an application key. The key and
//! the bridge's certificate fingerprint go to the encrypted secret store;
//! from then on everything is automatic: full load, then the bridge's event
//! stream, with commands sent as `PUT` on light services.
//!
//! The driver never talks to a bridge whose certificate it cannot pin, and
//! never gives up silently: a revoked key triggers a new pairing, a changed
//! certificate or a missing secret store becomes a `waiting` status that
//! says what a human must do.

mod bridge;
mod model;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use http::Method;
use http_body_util::BodyExt;
use moli_core::DeviceId;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};

pub use bridge::{Bridge, PIN_MISMATCH, SseParser};
pub use model::{Index, Load, Update};

const PAIRING_RETRY: Duration = Duration::from_secs(2);
/// No event for this long: reopen the stream and resynchronize.
const STREAM_IDLE: Duration = Duration::from_secs(600);
/// Below the hub's 5 s command timeout, so the answer is always ours.
const COMMAND_BUDGET: Duration = Duration::from_millis(4500);
const APP_KEY: &str = "app_key";
const CLIENT_KEY: &str = "client_key";
const CERT: &str = "cert_sha256";

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Bridge IP or host name.
    pub host: String,
    /// Optional: take the application key from this environment variable
    /// instead of pairing (the key then never touches the disk).
    #[serde(default)]
    pub app_key_env: Option<String>,
    /// Optional: the bridge certificate's SHA-256 (hex), when it cannot be
    /// learned and stored at first contact (no secret store).
    #[serde(default)]
    pub cert_sha256: Option<String>,
}

#[derive(Debug)]
pub struct Hue {
    config: Config,
}

impl Hue {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Hue {
    fn kind(&self) -> &'static str {
        "hue"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// The bridge refused our application key.
#[derive(Debug)]
struct KeyRejected;

impl std::fmt::Display for KeyRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("hue application key rejected")
    }
}

impl std::error::Error for KeyRejected {}

/// Shows `reason` on every surface and idles until shutdown.
async fn park(ctx: &DriverCtx, reason: String) -> anyhow::Result<()> {
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

fn secrets_problem(ctx: &DriverCtx) -> String {
    ctx.secrets_problem().map_or_else(
        || moli_i18n::tr!("pilotes.hue.coffre_indisponible"),
        str::to_owned,
    )
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let pin = match config.cert_sha256.clone().or_else(|| ctx.secret(CERT)) {
        Some(pin) => pin,
        None if !ctx.can_store_secrets() => {
            let reason = moli_i18n::tr!(
                "pilotes.hue.epinglage_impossible",
                problem = secrets_problem(ctx)
            );
            return park(ctx, reason).await;
        }
        // Pinned and stored before any key leaves, whether it comes from
        // pairing or from `app_key_env`.
        None => {
            let seen = Bridge::first_contact(&config.host).await.context("hue")?;
            ctx.store_secret(CERT, &seen).await?;
            tracing::info!(instance = %ctx.instance(), "hue bridge certificate pinned");
            seen
        }
    };

    let env_key = config
        .app_key_env
        .as_ref()
        .and_then(|var| std::env::var(var).ok());
    let from_env = env_key.is_some();
    let key = match env_key.or_else(|| ctx.secret(APP_KEY)) {
        Some(key) => key,
        None => match pair(config, ctx, pin.clone()).await? {
            Some(key) => key,
            None => return Ok(()), // shutting down while waiting
        },
    };

    let bridge = Arc::new(Bridge::new(&config.host, pin, Some(key))?);
    let mut index = match full_load(&bridge, ctx).await {
        Ok(index) => index,
        Err(e) if e.is::<KeyRejected>() && !from_env => {
            // Revoked from the Hue app, bridge reset…: pair again.
            ctx.forget_secret(APP_KEY).await?;
            return Err(e.context("pairing again"));
        }
        Err(e) if e.is::<KeyRejected>() => {
            let source = config
                .app_key_env
                .clone()
                .unwrap_or_else(|| moli_i18n::tr!("pilotes.hue.environnement"));
            let reason = moli_i18n::tr!("pilotes.hue.cle_refusee", source = source);
            return park(ctx, reason).await;
        }
        Err(e) if format!("{e:#}").contains(PIN_MISMATCH) => {
            return park(ctx, pin_changed(config, ctx)).await;
        }
        Err(e) => return Err(e),
    };
    ctx.ready();

    loop {
        let mut stream = bridge.events().await?;
        let mut parser = SseParser::default();
        loop {
            tokio::select! {
                frame = tokio::time::timeout(STREAM_IDLE, stream.frame()) => {
                    let Ok(frame) = frame else {
                        tracing::debug!(instance = %ctx.instance(), "hue event stream idle, reopening");
                        break;
                    };
                    let frame = frame.context("hue event stream closed")??;
                    let Some(data) = frame.data_ref() else { continue };
                    let payloads = match parser.feed(data) {
                        Ok(payloads) => payloads,
                        Err(e) => {
                            tracing::warn!(instance = %ctx.instance(), error = %e, "hue event stream dropped, reopening");
                            break;
                        }
                    };
                    for payload in payloads {
                        if apply_events(ctx, &index, &payload) {
                            tracing::info!(instance = %ctx.instance(), "hue topology changed, reloading");
                            index = full_load(&bridge, ctx).await?;
                        }
                    }
                }
                command = ctx.next_command() => match command {
                    Some(command) => spawn_command(&bridge, &index, command),
                    None => return Ok(()),
                },
            }
        }
        // Whatever happened during the silence, start again from the truth.
        index = full_load(&bridge, ctx).await?;
    }
}

fn pin_changed(config: &Config, ctx: &DriverCtx) -> String {
    moli_i18n::tr!(
        "pilotes.hue.certificat_change",
        host = config.host,
        instance = ctx.instance(),
        key = CERT
    )
}

/// Link-button pairing. Returns the application key, or `None` on shutdown.
async fn pair(config: &Config, ctx: &mut DriverCtx, pin: String) -> anyhow::Result<Option<String>> {
    if !ctx.can_store_secrets() {
        let reason = moli_i18n::tr!(
            "pilotes.hue.cle_non_enregistrable",
            problem = secrets_problem(ctx)
        );
        park(ctx, reason).await?;
        return Ok(None);
    }
    let bridge = Bridge::new(&config.host, pin, None)?;
    let request = json!({
        "devicetype": format!("moli-os#{}", ctx.instance()),
        "generateclientkey": true,
    });
    ctx.wait_for(moli_i18n::tr!(
        "pilotes.hue.appuyer_bouton",
        host = config.host
    ));
    tracing::info!(instance = %ctx.instance(), "waiting for the hue link button");
    loop {
        let answer = match bridge
            .send(Method::POST, "/api", Some(request.to_string().into_bytes()))
            .await
        {
            Ok((_, body)) => serde_json::from_slice::<Json>(&body).context("pairing answer")?,
            Err(e) if format!("{e:#}").contains(PIN_MISMATCH) => {
                park(ctx, pin_changed(config, ctx)).await?;
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let first = &answer[0];
        if let Some(success) = first.get("success") {
            let key = success["username"].as_str().context("no application key")?;
            // Pin first: a key without its pin must never exist.
            if let Some(seen) = bridge.seen_fingerprint() {
                ctx.store_secret(CERT, &seen).await?;
            }
            if let Some(client_key) = success["clientkey"].as_str() {
                ctx.store_secret(CLIENT_KEY, client_key).await?;
            }
            ctx.store_secret(APP_KEY, key).await?;
            tracing::info!(instance = %ctx.instance(), "hue bridge paired");
            return Ok(Some(key.to_owned()));
        }
        // 101 = link button not pressed (yet): keep asking.
        if first["error"]["type"].as_u64() != Some(101) {
            bail!("hue pairing refused: {}", first["error"]["description"]);
        }
        tokio::select! {
            () = ctx.cancelled() => return Ok(None),
            () = tokio::time::sleep(PAIRING_RETRY) => {}
        }
    }
}

/// Loads every resource, publishes devices and their current values.
async fn full_load(bridge: &Bridge, ctx: &DriverCtx) -> anyhow::Result<Index> {
    let (status, body) = bridge.send(Method::GET, "/clip/v2/resource", None).await?;
    if status == http::StatusCode::FORBIDDEN || status == http::StatusCode::UNAUTHORIZED {
        return Err(anyhow!(KeyRejected));
    }
    if !status.is_success() {
        bail!("hue resource listing failed: {status}");
    }
    let json: Json = serde_json::from_slice(&body).context("hue resources")?;
    let resources = json["data"]
        .as_array()
        .context("no data in hue resources")?;
    let Load { devices, index } = model::load(ctx.instance(), resources);

    let keep: Vec<DeviceId> = devices.iter().map(|d| d.id.clone()).collect();
    for id in ctx.devices() {
        if !keep.contains(&id) {
            ctx.remove_device(&id);
        }
    }
    let count = devices.len();
    for device in devices {
        ctx.upsert_device(device);
    }
    for resource in resources {
        if let Some(update) = index.decode(resource) {
            // A load restates the last press; it is not a new one.
            apply(ctx, &update, false);
        }
    }
    tracing::info!(instance = %ctx.instance(), devices = count, "hue bridge loaded");
    Ok(index)
}

fn apply(ctx: &DriverCtx, update: &Update, live: bool) {
    let Some(device) = &update.device else {
        return;
    };
    for (key, value) in &update.values {
        if live && update.pulse {
            ctx.pulse_state(device, key, value.clone());
        } else {
            ctx.set_state(device, key, value.clone());
        }
    }
    if let Some(online) = update.online {
        ctx.set_availability(device, online);
    }
}

/// Applies one SSE payload. Returns `true` when the topology changed
/// (devices or services added/removed, renames, room changes).
fn apply_events(ctx: &DriverCtx, index: &Index, payload: &str) -> bool {
    let Ok(Json::Array(events)) = serde_json::from_str::<Json>(payload) else {
        tracing::debug!("unparsable hue event payload");
        return false;
    };
    let mut changed = false;
    for event in &events {
        let kind = event["type"].as_str().unwrap_or_default();
        for item in event["data"].as_array().into_iter().flatten() {
            let rtype = item["type"].as_str().unwrap_or_default();
            // Renames and membership changes reshape devices: reload.
            let structural = matches!(rtype, "device" | "room" | "zone");
            match kind {
                "update" if structural => changed = true,
                "update" => {
                    if let Some(update) = index.decode(item) {
                        apply(ctx, &update, true);
                    }
                }
                "add" | "delete"
                    if structural || item["id"].as_str().is_some_and(|id| index.knows(id)) =>
                {
                    changed = true;
                }
                _ => {}
            }
        }
    }
    changed
}

/// Commands run beside the event loop: a slow bridge never freezes events.
fn spawn_command(bridge: &Arc<Bridge>, index: &Index, command: CommandRequest) {
    let Some((path, body)) = index.encode(&command.device.id, &command.key, &command.value) else {
        command.reply(Err(moli_i18n::tr!("pilotes.hue.point_non_ecrivable")));
        return;
    };
    let bridge = Arc::clone(bridge);
    tokio::spawn(async move {
        let result = tokio::time::timeout(COMMAND_BUDGET, put(&bridge, &path, &body))
            .await
            .unwrap_or_else(|_| Err(moli_i18n::tr!("pilotes.hue.pont_muet")));
        command.reply(result);
    });
}

async fn put(bridge: &Bridge, path: &str, body: &Json) -> Result<(), String> {
    let (status, body) = bridge
        .send(Method::PUT, path, Some(body.to_string().into_bytes()))
        .await
        .map_err(|e| format!("{e:#}"))?;
    let answer: Json = serde_json::from_slice(&body).unwrap_or_default();
    match (
        status.is_success(),
        answer["errors"].as_array().filter(|e| !e.is_empty()),
    ) {
        (true, None) => Ok(()),
        (_, Some(errors)) => Err(moli_i18n::tr!(
            "pilotes.hue.pont_erreur",
            description = errors[0]["description"]
        )),
        (false, None) => Err(moli_i18n::tr!("pilotes.hue.pont_repond", status = status)),
    }
}
