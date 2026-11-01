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
//!
//! A stored key belongs to the certificate it was given under: once that
//! fingerprint is forgotten (after a « certificate changed »), the key is
//! forgotten too, before anyone is contacted, and the button must be
//! pressed again. Whoever answers in the bridge's place never gets it.

mod bridge;
mod model;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use http::Method;
use http_body_util::BodyExt;
use moli_core::{DeviceId, InstanceId};
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
/// The fingerprint the stored key was given under.
const KEY_CERT: &str = "app_key_cert_sha256";
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
        // pairing or from `app_key_env`. Whoever answers now gets trusted:
        // never with a key given to another certificate.
        None => {
            forget_key(ctx).await?;
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
    // A key from the environment wins: the store is not even read (nor
    // written), so a stale stored key never stops it.
    let stored = if from_env {
        None
    } else {
        stored_key(ctx, &pin).await?
    };
    let key = match env_key.or(stored) {
        Some(key) => key,
        None => match pair(config, ctx, pin.clone()).await? {
            Some(key) => key,
            None => return Ok(()), // shutting down while waiting
        },
    };

    let bridge = Arc::new(Bridge::new(&config.host, pin.clone(), Some(key))?);
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
            let seen = bridge.seen_fingerprint();
            let reason = pin_changed(config, ctx.instance(), &pin, seen.as_deref());
            return park(ctx, reason).await;
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

/// Lowercase hex without separators, as `moli-net` compares fingerprints.
fn normalize(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The start of a fingerprint, enough to tell two apart on a status line.
fn short(fingerprint: &str) -> String {
    let mut short: String = normalize(fingerprint).chars().take(16).collect();
    short.push('…');
    short
}

/// What to do when the bridge shows another certificate. Forgetting the
/// pin only helps when Moli can forget the key with it: a key from the
/// environment (configured, even if not set right now) would go to
/// whoever answers next, and a pin from the configuration is not in the
/// store. The full fingerprints, not secret, go to the log.
fn pin_changed(config: &Config, instance: &InstanceId, pinned: &str, seen: Option<&str>) -> String {
    tracing::warn!(
        instance = %instance,
        pinned = %normalize(pinned),
        seen = %seen.map(normalize).unwrap_or_default(),
        "hue bridge certificate changed"
    );
    let host = &config.host;
    let pinned = short(pinned);
    let seen = seen.map_or_else(|| moli_i18n::tr!("pilotes.hue.empreinte_inconnue"), short);
    if config.app_key_env.is_some() {
        let source = config.app_key_env.clone().unwrap_or_default();
        return moli_i18n::tr!(
            "pilotes.hue.certificat_change_env",
            host = host,
            pinned = pinned,
            seen = seen,
            source = source
        );
    }
    if config.cert_sha256.is_some() {
        return moli_i18n::tr!(
            "pilotes.hue.certificat_change_config",
            host = host,
            pinned = pinned,
            seen = seen
        );
    }
    moli_i18n::tr!(
        "pilotes.hue.certificat_change",
        host = host,
        pinned = pinned,
        seen = seen,
        instance = instance,
        key = CERT
    )
}

/// What the store says of a key, given the pin in force.
#[derive(Debug, PartialEq, Eq)]
enum Stored {
    /// No key stored.
    None,
    /// The key was given under this very pin.
    Bound,
    /// Stored before keys were bound to their certificate, with this very
    /// pin still stored beside it (pairing always stored the pin it saw
    /// with the key): adopted.
    Unbound,
    /// Given under another certificate, or its own pin is gone (forgotten,
    /// or replaced by another one in the configuration): never sent.
    Other,
}

/// `key_cert` is the pin the key was given under, `cert` the pin stored in
/// the vault (the one a key stored before this change was given under).
fn stored(pin: &str, key: bool, key_cert: Option<&str>, cert: Option<&str>) -> Stored {
    let same = |other: &str| normalize(other) == normalize(pin);
    match (key, key_cert) {
        (false, _) => Stored::None,
        (true, Some(given)) if same(given) => Stored::Bound,
        (true, None) if cert.is_some_and(same) => Stored::Unbound,
        (true, _) => Stored::Other,
    }
}

/// The stored key, if it belongs to `pin`.
async fn stored_key(ctx: &DriverCtx, pin: &str) -> anyhow::Result<Option<String>> {
    let key = ctx.secret(APP_KEY);
    let key_cert = ctx.secret(KEY_CERT);
    let cert = ctx.secret(CERT);
    match stored(pin, key.is_some(), key_cert.as_deref(), cert.as_deref()) {
        Stored::None => Ok(None),
        Stored::Bound => Ok(key),
        Stored::Unbound => {
            // A store that cannot be written must not stop a working house:
            // the same check runs again at the next start.
            if let Err(e) = ctx.store_secret(KEY_CERT, &normalize(pin)).await {
                tracing::warn!(instance = %ctx.instance(), error = %e, "hue key binding not saved");
            }
            Ok(key)
        }
        Stored::Other => {
            forget_key(ctx).await?;
            Ok(None)
        }
    }
}

/// Forgets the stored keys and the fingerprint they were given under. Asked
/// of the store itself, not of what this run has seen: a key set from the
/// command line while Moli runs is forgotten too.
async fn forget_key(ctx: &DriverCtx) -> anyhow::Result<()> {
    for name in [APP_KEY, CLIENT_KEY, KEY_CERT] {
        ctx.forget_secret(name).await?;
    }
    Ok(())
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
    let bridge = Bridge::new(&config.host, pin.clone(), None)?;
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
                let seen = bridge.seen_fingerprint();
                let reason = pin_changed(config, ctx.instance(), &pin, seen.as_deref());
                park(ctx, reason).await?;
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
            // The key goes with the fingerprint it was given under.
            ctx.store_secret(KEY_CERT, &normalize(&pin)).await?;
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

#[cfg(test)]
mod tests {
    use moli_core::InstanceId;
    use moli_runtime::{HubOptions, MasterKey, spawn_driver};
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn pin() -> String {
        "ab".repeat(32)
    }

    /// A core hub with a real encrypted store, in its own folder.
    fn core(test: &str) -> moli_runtime::Hub {
        let dir = std::env::temp_dir().join(format!("moli-hue-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        moli_runtime::Hub::new(HubOptions {
            secrets: Some((dir.join("secrets.enc"), MasterKey::generate().unwrap())),
            ..HubOptions::default()
        })
        .unwrap()
    }

    async fn until(what: &str, done: impl Fn() -> bool) {
        for _ in 0..500 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("never: {what}");
    }

    fn config() -> Config {
        // A name that never resolves (RFC 6761): a first contact fails,
        // whatever listens on this machine.
        Config {
            host: "hue-test.invalid".into(),
            app_key_env: None,
            cert_sha256: None,
        }
    }

    #[test]
    fn a_key_counts_only_with_the_certificate_it_was_given_under() {
        let typed = pin().to_uppercase();
        let other = "cd".repeat(32);
        assert_eq!(stored(&pin(), false, None, None), Stored::None);
        assert_eq!(stored(&pin(), true, Some(&typed), None), Stored::Bound);
        assert_eq!(
            stored(&pin(), true, Some(&other), Some(&pin())),
            Stored::Other
        );
        // A key stored before this change: its own pin is the one stored.
        assert_eq!(stored(&pin(), true, None, Some(&typed)), Stored::Unbound);
        assert_eq!(stored(&pin(), true, None, Some(&other)), Stored::Other);
        assert_eq!(stored(&pin(), true, None, None), Stored::Other);
        assert_eq!(short("AB:CD"), "abcd…");
    }

    #[tokio::test]
    async fn a_key_without_its_fingerprint_is_forgotten_before_any_contact() {
        // The fingerprint was forgotten, as the « certificate changed »
        // message advised: the key must not go to whoever answers next.
        let core = core("orphan");
        let hue = InstanceId::from("hue");
        core.store_secret(&hue, APP_KEY, "secret-key")
            .await
            .unwrap();
        core.store_secret(&hue, CLIENT_KEY, "secret-client")
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        spawn_driver(
            &core,
            hue.clone(),
            Arc::new(Hue::new(config())),
            cancel.clone(),
        );
        // The first contact fails: a key still there afterwards would mean
        // it outlived its fingerprint.
        until("key forgotten", || {
            core.secret(&hue, APP_KEY).is_none() && core.secret(&hue, CLIENT_KEY).is_none()
        })
        .await;
        assert_eq!(core.secret(&hue, CERT), None, "nothing was pinned");
        cancel.cancel();
    }

    #[tokio::test]
    async fn a_key_paired_before_this_change_is_kept() {
        // An existing house: pin and key stored, no binding yet. Upgrading
        // must not ask for the button again.
        let core = core("upgrade");
        let hue = InstanceId::from("hue");
        core.store_secret(&hue, CERT, &pin()).await.unwrap();
        core.store_secret(&hue, APP_KEY, "secret-key")
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        spawn_driver(
            &core,
            hue.clone(),
            Arc::new(Hue::new(config())),
            cancel.clone(),
        );
        until("key bound", || core.secret(&hue, KEY_CERT).is_some()).await;
        assert_eq!(core.secret(&hue, KEY_CERT), Some(pin()));
        assert_eq!(core.secret(&hue, APP_KEY).as_deref(), Some("secret-key"));
        cancel.cancel();
    }

    #[tokio::test]
    async fn a_key_paired_before_this_change_never_goes_to_another_pin() {
        // An existing house whose configuration now names another
        // certificate (the one presented in its place): the old key was
        // given under the stored pin, not this one.
        let core = core("other-pin");
        let hue = InstanceId::from("hue");
        core.store_secret(&hue, CERT, &pin()).await.unwrap();
        core.store_secret(&hue, APP_KEY, "secret-key")
            .await
            .unwrap();
        let config = Config {
            cert_sha256: Some("cd".repeat(32)),
            ..config()
        };
        let cancel = CancellationToken::new();
        spawn_driver(
            &core,
            hue.clone(),
            Arc::new(Hue::new(config)),
            cancel.clone(),
        );
        until("key forgotten", || core.secret(&hue, APP_KEY).is_none()).await;
        assert_eq!(core.secret(&hue, KEY_CERT), None, "never bound to it");
        cancel.cancel();
    }

    #[test]
    fn a_changed_certificate_never_advises_what_would_send_the_key() {
        let hue = InstanceId::from("hue");
        let stored = pin_changed(&config(), &hue, &pin(), None);
        assert!(
            stored.contains("secrets forget hue cert_sha256"),
            "{stored}"
        );
        // A key from the environment cannot be forgotten by Moli, and a
        // pin from the configuration is not in the store: forgetting the
        // stored pin would help nobody, or send the key to an impostor.
        let env = Config {
            app_key_env: Some("MOLI_HUE_KEY".into()),
            ..config()
        };
        // Configured is enough: the variable may be missing right now and
        // come back later, with the pin the advice would have given away.
        let from_env = pin_changed(&env, &hue, &pin(), None);
        assert!(from_env.contains("MOLI_HUE_KEY"), "{from_env}");
        assert!(!from_env.contains("secrets forget "), "{from_env}");
        let pinned = Config {
            cert_sha256: Some(pin()),
            ..config()
        };
        let configured = pin_changed(&pinned, &hue, &pin(), None);
        assert!(configured.contains("cert_sha256"), "{configured}");
        assert!(!configured.contains("secrets forget "), "{configured}");
    }
}
