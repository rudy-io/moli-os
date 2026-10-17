//! IKEA Dirigera driver.
//!
//! Pairing is the hub's own ceremony (OAuth with PKCE): the driver asks
//! for a code, a human presses the action button under the hub, the hub
//! trades the code for a token. The token and the hub's certificate
//! fingerprint go to the encrypted secret store; from then on everything is
//! automatic: full load (`/v1/home`), then the hub's event websocket, with
//! commands sent as `PATCH` on devices.
//!
//! The driver never talks to a hub whose certificate it cannot pin, and
//! never gives up silently: a revoked token triggers a new pairing, a
//! changed certificate or a missing secret store becomes a `waiting` status
//! that says what a human must do.
//!
//! A token belongs to the certificate it was given under: without that very
//! fingerprint (forgotten after a « certificate changed », or another one in
//! the options), the token is forgotten before anyone is contacted, and the
//! button must be pressed again. Whoever answers in the hub's place never
//! gets it.

mod hub;
mod model;
mod pkce;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use moli_core::DeviceId;
use moli_net::ws::WebSocket;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::Value as Json;
use tokio::sync::mpsc;

pub use hub::{Authorize, Grant, Hub, PIN_MISMATCH};
pub use model::{Event, Index, Load, Skipped, Update};

const PAIRING_RETRY: Duration = Duration::from_secs(2);
/// A code lives 60 s; a hub that never says so still gets a new one.
const CODE_LIFETIME: Duration = Duration::from_secs(90);
/// The hub closes a quiet websocket: a ping event this often (lpgera's
/// rhythm), answered by a `pong` event.
const PING_EVERY: Duration = Duration::from_secs(30);
/// Nothing at all for this long, not even a pong: the connection is dead.
const SILENCE: Duration = Duration::from_secs(95);
/// A websocket that dies sooner than this was refused, not cut: the
/// supervisor's backoff takes over instead of reconnecting at once.
const SHORT_LIFE: Duration = Duration::from_secs(10);
const EVENT_QUEUE: usize = 64;
/// Below the hub's 5 s command timeout, so the answer is always ours.
const COMMAND_BUDGET: Duration = Duration::from_millis(4500);
const TOKEN: &str = "token";
/// The fingerprint the token was given under.
const TOKEN_CERT: &str = "token_cert_sha256";
const CERT: &str = "cert_sha256";

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Hub IP or host name.
    pub host: String,
    /// Optional: the hub certificate's SHA-256 (hex), when it cannot be
    /// learned and stored at first contact (no secret store).
    #[serde(default)]
    pub cert_sha256: Option<String>,
}

#[derive(Debug)]
pub struct Dirigera {
    config: Config,
}

impl Dirigera {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Dirigera {
    fn kind(&self) -> &'static str {
        "dirigera"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// The hub refused our token.
#[derive(Debug)]
struct TokenRejected;

impl std::fmt::Display for TokenRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dirigera token rejected")
    }
}

impl std::error::Error for TokenRejected {}

/// Shows `reason` on every surface and idles until shutdown.
async fn park(ctx: &DriverCtx, reason: String) -> anyhow::Result<()> {
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

fn secrets_problem(ctx: &DriverCtx) -> String {
    ctx.secrets_problem().map_or_else(
        || moli_i18n::tr!("pilotes.dirigera.coffre_indisponible"),
        str::to_owned,
    )
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

fn pin_changed(config: &Config, ctx: &DriverCtx, pinned: &str, seen: Option<&str>) -> String {
    moli_i18n::tr!(
        "pilotes.dirigera.certificat_change",
        host = config.host,
        pinned = short(pinned),
        seen = seen.map_or_else(
            || moli_i18n::tr!("pilotes.dirigera.empreinte_inconnue"),
            short
        ),
        instance = ctx.instance(),
        key = CERT
    )
}

/// What a run may use from the secret store.
#[derive(PartialEq, Eq)]
enum Trust {
    /// No fingerprint: any token goes first, then the first contact pins a
    /// certificate, then the button again.
    FirstContact,
    /// Pinned, but no token given under this pin: the button again.
    Pair { pin: String },
    /// Pinned, with the token given under that very pin.
    Ready { pin: String, token: String },
}

impl std::fmt::Debug for Trust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the token.
        match self {
            Self::FirstContact => f.write_str("FirstContact"),
            Self::Pair { pin } => write!(f, "Pair({pin})"),
            Self::Ready { pin, .. } => write!(f, "Ready({pin}, token)"),
        }
    }
}

/// `configured`: the `cert_sha256` option; `stored`, `token`, `token_cert`:
/// the store's. A fingerprint given in the options wins over the stored one.
fn trust(
    configured: Option<&str>,
    stored: Option<&str>,
    token: Option<&str>,
    token_cert: Option<&str>,
) -> Trust {
    let Some(pin) = configured.or(stored).map(normalize) else {
        return Trust::FirstContact;
    };
    match token {
        Some(token) if token_cert.map(normalize).as_deref() == Some(pin.as_str()) => Trust::Ready {
            pin,
            token: token.to_owned(),
        },
        _ => Trust::Pair { pin },
    }
}

/// Forgets the token and the fingerprint it was given under.
async fn forget_token(ctx: &DriverCtx) -> anyhow::Result<()> {
    for name in [TOKEN, TOKEN_CERT] {
        if ctx.secret(name).is_some() {
            ctx.forget_secret(name).await?;
        }
    }
    Ok(())
}

fn is_pin_mismatch(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains(PIN_MISMATCH)
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let trust = trust(
        config.cert_sha256.as_deref(),
        ctx.secret(CERT).as_deref(),
        ctx.secret(TOKEN).as_deref(),
        ctx.secret(TOKEN_CERT).as_deref(),
    );
    let (pin, token) = match trust {
        Trust::Ready { pin, token } => (pin, Some(token)),
        Trust::Pair { pin } => {
            forget_token(ctx).await?;
            (pin, None)
        }
        Trust::FirstContact if !ctx.can_store_secrets() => {
            let reason = moli_i18n::tr!(
                "pilotes.dirigera.epinglage_impossible",
                problem = secrets_problem(ctx)
            );
            return park(ctx, reason).await;
        }
        // Whoever answers now gets trusted: never with the old token. Pinned
        // and stored before any code or token goes through.
        Trust::FirstContact => {
            forget_token(ctx).await?;
            let seen = Hub::first_contact(&config.host).await.context("dirigera")?;
            ctx.store_secret(CERT, &seen).await?;
            tracing::info!(instance = %ctx.instance(), "dirigera hub certificate pinned");
            (normalize(&seen), None)
        }
    };

    let token = if let Some(token) = token {
        token
    } else {
        let hub = Hub::new(&config.host, pin.clone(), None)?;
        let Some(token) = pair(config, ctx, &hub, &pin, PAIRING_RETRY).await? else {
            return Ok(()); // shutting down while waiting
        };
        token
    };

    let hub = Arc::new(Hub::new(&config.host, pin.clone(), Some(token))?);
    let mut index = match full_load(&hub, ctx).await {
        Ok(index) => index,
        Err(e) if e.is::<TokenRejected>() => {
            // Hub reset, or the token revoked: pair again.
            forget_token(ctx).await?;
            return Err(e.context("pairing again"));
        }
        Err(e) if is_pin_mismatch(&e) => {
            let reason = pin_changed(config, ctx, &pin, hub.seen_fingerprint().as_deref());
            return park(ctx, reason).await;
        }
        Err(e) => return Err(e),
    };
    ctx.ready();

    loop {
        let socket = match hub.events().await {
            Ok(socket) => socket,
            Err(e) if is_pin_mismatch(&e) => {
                let reason = pin_changed(config, ctx, &pin, hub.seen_fingerprint().as_deref());
                return park(ctx, reason).await;
            }
            Err(e) => return Err(e.context("dirigera event websocket")),
        };
        let opened = Instant::now();
        let (tx, mut rx) = mpsc::channel(EVENT_QUEUE);
        let reader = tokio::spawn(read_events(socket, tx));
        tracing::debug!(instance = %ctx.instance(), "dirigera events open");
        loop {
            tokio::select! {
                message = rx.recv() => {
                    let Some(text) = message else { break };
                    if apply_message(ctx, &mut index, &text) {
                        tracing::info!(instance = %ctx.instance(), "dirigera devices changed, reloading");
                        index = full_load(&hub, ctx).await?;
                    }
                }
                command = ctx.next_command() => {
                    let Some(command) = command else {
                        reader.abort();
                        return Ok(());
                    };
                    spawn_command(&hub, &index, command);
                }
            }
        }
        reader.abort();
        if opened.elapsed() < SHORT_LIFE {
            bail!("the dirigera event websocket closed at once");
        }
        // The events of the cut are lost: start again from the truth.
        index = full_load(&hub, ctx).await?;
    }
}

/// The hub's pairing calls, behind a trait so the ceremony is tested
/// without a hub.
trait Pairing: Sync {
    fn authorize<'a>(&'a self, challenge: &'a str) -> BoxFuture<'a, anyhow::Result<Authorize>>;
    fn token<'a>(
        &'a self,
        code: &'a str,
        verifier: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Grant>>;
    /// The certificate presented last, for a « certificate changed ».
    fn seen_fingerprint(&self) -> Option<String>;
}

impl Pairing for Hub {
    fn authorize<'a>(&'a self, challenge: &'a str) -> BoxFuture<'a, anyhow::Result<Authorize>> {
        Box::pin(Hub::authorize(self, challenge))
    }

    fn token<'a>(
        &'a self,
        code: &'a str,
        verifier: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Grant>> {
        Box::pin(Hub::token(self, code, verifier, name))
    }

    fn seen_fingerprint(&self) -> Option<String> {
        Hub::seen_fingerprint(self)
    }
}

/// Action-button pairing under `pin` (already stored). Returns the token,
/// or `None` on shutdown. A person is asked to press only while Moli holds
/// a code: during another client's request (409), a press could pair that
/// client instead.
async fn pair(
    config: &Config,
    ctx: &DriverCtx,
    hub: &impl Pairing,
    pin: &str,
    retry: Duration,
) -> anyhow::Result<Option<String>> {
    if !ctx.can_store_secrets() {
        let reason = moli_i18n::tr!(
            "pilotes.dirigera.jeton_non_enregistrable",
            problem = secrets_problem(ctx)
        );
        park(ctx, reason).await?;
        return Ok(None);
    }
    let name = format!("moli-os#{}", ctx.instance());
    loop {
        let verifier = pkce::verifier()?;
        let code = match hub.authorize(&pkce::challenge(&verifier)).await {
            Ok(Authorize::Code(code)) => code,
            // One pairing at a time on the hub: say not to press, and wait
            // for the other request to end (60 s at most).
            Ok(Authorize::Busy) => {
                ctx.wait_for(moli_i18n::tr!(
                    "pilotes.dirigera.appairage_occupe",
                    host = config.host
                ));
                tracing::debug!(instance = %ctx.instance(), "another dirigera pairing request is ongoing");
                tokio::select! {
                    () = ctx.cancelled() => return Ok(None),
                    () = tokio::time::sleep(retry) => {}
                }
                continue;
            }
            Err(e) if is_pin_mismatch(&e) => {
                park(
                    ctx,
                    pin_changed(config, ctx, pin, hub.seen_fingerprint().as_deref()),
                )
                .await?;
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        // Only now does a press pair Moli.
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.dirigera.appuyer_bouton",
            host = config.host
        ));
        tracing::info!(instance = %ctx.instance(), "waiting for the dirigera action button");
        let asked = Instant::now();
        while asked.elapsed() < CODE_LIFETIME {
            tokio::select! {
                () = ctx.cancelled() => return Ok(None),
                () = tokio::time::sleep(retry) => {}
            }
            match hub.token(&code, &verifier, &name).await {
                Ok(Grant::Token(token)) => {
                    // The certificate is stored already; the token goes with
                    // the fingerprint it was given under.
                    ctx.store_secret(TOKEN_CERT, pin).await?;
                    ctx.store_secret(TOKEN, &token).await?;
                    tracing::info!(instance = %ctx.instance(), "dirigera hub paired");
                    return Ok(Some(token));
                }
                Ok(Grant::NotYet) => {}
                Ok(Grant::Expired) => break,
                Ok(Grant::Refused(why)) => bail!("dirigera pairing refused: {why}"),
                Err(e) if is_pin_mismatch(&e) => {
                    park(
                        ctx,
                        pin_changed(config, ctx, pin, hub.seen_fingerprint().as_deref()),
                    )
                    .await?;
                    return Ok(None);
                }
                // A slow answer must not lose a press: ask again.
                Err(e) => {
                    tracing::debug!(instance = %ctx.instance(), error = %format!("{e:#}"), "dirigera token request failed");
                }
            }
        }
    }
}

/// Loads every device, publishes them and their current values.
async fn full_load(hub: &Hub, ctx: &DriverCtx) -> anyhow::Result<Index> {
    let (status, body) = hub.home().await?;
    if status == http::StatusCode::FORBIDDEN || status == http::StatusCode::UNAUTHORIZED {
        return Err(anyhow!(TokenRejected));
    }
    if !status.is_success() {
        bail!("dirigera home failed: {status}");
    }
    let json: Json = serde_json::from_slice(&body).context("dirigera home")?;
    let raw = json["devices"]
        .as_array()
        .context("no devices in the dirigera home")?;
    let Load {
        devices,
        mut index,
        skipped,
    } = model::load(ctx.instance(), raw);
    log_skipped(ctx, &skipped);

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
    for item in raw {
        if let Some(update) = index.decode(item) {
            apply(ctx, &update);
        }
    }
    tracing::info!(instance = %ctx.instance(), devices = count, "dirigera hub loaded");
    Ok(index)
}

/// One line for what is left out: unknown types say so louder.
fn log_skipped(ctx: &DriverCtx, skipped: &[Skipped]) {
    let mut unknown: Vec<&str> = skipped
        .iter()
        .filter(|s| !s.known)
        .map(|s| s.device_type.as_str())
        .collect();
    unknown.sort_unstable();
    unknown.dedup();
    if !unknown.is_empty() {
        tracing::warn!(instance = %ctx.instance(), types = ?unknown, "dirigera device types unknown to the driver, left out");
    }
    let mut known: Vec<&str> = skipped
        .iter()
        .filter(|s| s.known)
        .map(|s| s.device_type.as_str())
        .collect();
    known.sort_unstable();
    known.dedup();
    if !known.is_empty() {
        tracing::debug!(instance = %ctx.instance(), types = ?known, "dirigera devices left out");
    }
}

fn apply(ctx: &DriverCtx, update: &Update) {
    let Some(device) = &update.device else {
        return;
    };
    for (key, value) in &update.values {
        ctx.set_state(device, key, value.clone());
    }
    if let Some(online) = update.online {
        ctx.set_availability(device, online);
    }
}

/// Applies one websocket message. Returns `true` when devices changed shape
/// (added, removed, renamed, moved): a full load is due.
fn apply_message(ctx: &DriverCtx, index: &mut Index, text: &str) -> bool {
    let Ok(message) = serde_json::from_str::<Json>(text) else {
        tracing::debug!("unparsable dirigera event");
        return false;
    };
    match index.classify(&message) {
        Event::State(data) => {
            if let Some(update) = index.decode(data) {
                apply(ctx, &update);
            }
            false
        }
        Event::Press(data) => {
            if let Some((device, pattern)) = index.press(data) {
                ctx.pulse_state(&device, "button", pattern);
            }
            false
        }
        Event::Reload => true,
        Event::Ignore => false,
    }
}

/// Owns the websocket: hands every message over, sends the ping event
/// every 30 s, and ends (closing the channel) when the hub closes the
/// connection or goes silent.
async fn read_events(mut socket: WebSocket, tx: mpsc::Sender<String>) {
    let mut pings = 0u64;
    let mut next_ping = Instant::now() + PING_EVERY;
    let mut heard = Instant::now();
    loop {
        if Instant::now() >= next_ping {
            pings += 1;
            let time = jiff::Timestamp::now().to_string();
            if let Err(e) = socket.send_text(&hub::ping(pings, &time)).await {
                tracing::debug!(error = %format!("{e:#}"), "dirigera ping failed");
                return;
            }
            next_ping = Instant::now() + PING_EVERY;
        }
        if heard.elapsed() > SILENCE {
            tracing::info!("dirigera event websocket silent, reconnecting");
            return;
        }
        let wait = next_ping
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(100));
        match socket.recv_text(wait).await {
            Ok(text) => {
                heard = Instant::now();
                if tx.send(text).await.is_err() {
                    return;
                }
            }
            Err(e) if e.is::<tokio::time::error::Elapsed>() => {}
            Err(e) => {
                tracing::info!(error = %format!("{e:#}"), "dirigera event websocket closed");
                return;
            }
        }
    }
}

/// Commands run beside the event loop: a slow hub never freezes events.
fn spawn_command(hub: &Arc<Hub>, index: &Index, command: CommandRequest) {
    let Some((device, body)) = index.encode(&command.device.id, &command.key, &command.value)
    else {
        command.reply(Err(moli_i18n::tr!("pilotes.dirigera.point_non_ecrivable")));
        return;
    };
    let hub = Arc::clone(hub);
    tokio::spawn(async move {
        let result = tokio::time::timeout(COMMAND_BUDGET, patch(&hub, &device, &body))
            .await
            .unwrap_or_else(|_| Err(moli_i18n::tr!("pilotes.dirigera.hub_muet")));
        command.reply(result);
    });
}

async fn patch(hub: &Hub, device: &str, body: &Json) -> Result<(), String> {
    let (status, answer) = hub
        .patch(device, body)
        .await
        .map_err(|e| format!("{e:#}"))?;
    // A 2xx is a success, with or without a body.
    if status.is_success() {
        Ok(())
    } else {
        Err(moli_i18n::tr!(
            "pilotes.dirigera.hub_repond",
            status = status,
            detail = hub::excerpt(&answer)
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use moli_core::{DriverStatus, Event as Bus, InstanceId};
    use moli_runtime::{HubOptions, MasterKey, spawn_driver};
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn pin() -> String {
        "ab".repeat(32)
    }

    /// A core hub with a real encrypted store, in its own folder.
    fn core(test: &str) -> moli_runtime::Hub {
        let dir = std::env::temp_dir().join(format!("moli-dirigera-{test}-{}", std::process::id()));
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

    #[test]
    fn a_token_counts_only_with_the_certificate_it_was_given_under() {
        let other = "cd".repeat(32);
        let typed = pin().to_uppercase(); // as someone may type it, case aside
        // No fingerprint at all: first contact, the token never goes.
        assert_eq!(
            trust(None, None, Some("t"), Some(&pin())),
            Trust::FirstContact
        );
        // Forgotten fingerprint: same.
        assert_eq!(trust(None, None, Some("t"), None), Trust::FirstContact);
        // The token's own fingerprint, stored or given in the options.
        let ready = Trust::Ready {
            pin: pin(),
            token: "t".into(),
        };
        assert_eq!(trust(None, Some(&pin()), Some("t"), Some(&pin())), ready);
        assert_eq!(trust(Some(&typed), None, Some("t"), Some(&pin())), ready);
        // Another fingerprint in the options, or none recorded for the
        // token: the button again.
        let pair = Trust::Pair { pin: other.clone() };
        assert_eq!(
            trust(Some(&other), Some(&pin()), Some("t"), Some(&pin())),
            pair
        );
        assert_eq!(
            trust(None, Some(&pin()), Some("t"), None),
            Trust::Pair { pin: pin() }
        );
        let secret = Trust::Ready {
            pin: pin(),
            token: "secret-token".into(),
        };
        assert!(!format!("{secret:?}").contains("secret-token"));
    }

    #[tokio::test]
    async fn a_token_without_its_fingerprint_is_forgotten_before_any_contact() {
        let core = core("orphan");
        let ikea = InstanceId::from("ikea");
        core.store_secret(&ikea, TOKEN, "secret-token")
            .await
            .unwrap();
        // Nobody answers there: the first contact fails, so a token still
        // there afterwards would mean it outlived its fingerprint.
        let config = Config {
            host: "127.0.0.1".into(),
            cert_sha256: None,
        };
        let cancel = CancellationToken::new();
        spawn_driver(
            &core,
            ikea.clone(),
            Arc::new(Dirigera::new(config)),
            cancel.clone(),
        );
        until("token forgotten", || core.secret(&ikea, TOKEN).is_none()).await;
        assert_eq!(core.secret(&ikea, CERT), None, "nothing was pinned");
        cancel.cancel();
    }

    #[test]
    fn a_changed_certificate_shows_both_fingerprints() {
        let config = Config {
            host: "192.168.1.x".into(),
            cert_sha256: None,
        };
        let text = moli_i18n::tr!(
            "pilotes.dirigera.certificat_change",
            host = config.host,
            pinned = short(&pin()),
            seen = short(&"cd".repeat(32)),
            instance = "ikea",
            key = CERT
        );
        assert!(text.contains("abababababababab…"), "{text}");
        assert!(text.contains("cdcdcdcdcdcdcdcd…"), "{text}");
        assert_eq!(short("AB:CD"), "abcd…");
    }

    /// A hub that answers from a script.
    struct Script {
        authorize: Mutex<VecDeque<Authorize>>,
        token: Mutex<VecDeque<Grant>>,
    }

    impl Pairing for Script {
        fn authorize<'a>(&'a self, _: &'a str) -> BoxFuture<'a, anyhow::Result<Authorize>> {
            let answer = self.authorize.lock().unwrap().pop_front();
            Box::pin(async move { answer.context("no more authorize answers") })
        }

        fn token<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: &'a str,
        ) -> BoxFuture<'a, anyhow::Result<Grant>> {
            let answer = self.token.lock().unwrap().pop_front();
            Box::pin(async move { answer.context("no more token answers") })
        }

        fn seen_fingerprint(&self) -> Option<String> {
            None
        }
    }

    /// Pairs against the script, then idles.
    struct Pairs(Script, Config);

    impl Driver for Pairs {
        fn kind(&self) -> &'static str {
            "dirigera"
        }

        fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                pair(&self.1, ctx, &self.0, &pin(), Duration::from_millis(5)).await?;
                ctx.cancelled().await;
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn the_button_is_asked_for_only_while_moli_holds_a_code() {
        let core = core("pairing");
        let mut bus = core.subscribe();
        let ikea = InstanceId::from("ikea");
        let config = Config {
            host: "192.168.1.x".into(),
            cert_sha256: None,
        };
        let script = Script {
            // Another client pairs first (409), then the code is ours.
            authorize: Mutex::new([Authorize::Busy, Authorize::Code("c".into())].into()),
            token: Mutex::new([Grant::NotYet, Grant::Token("t".into())].into()),
        };
        let cancel = CancellationToken::new();
        spawn_driver(
            &core,
            ikea.clone(),
            Arc::new(Pairs(script, config.clone())),
            cancel.clone(),
        );
        until("paired", || core.secret(&ikea, TOKEN).is_some()).await;

        let mut waits = Vec::new();
        while let Ok(event) = bus.try_recv() {
            if let Bus::DriverStatus {
                status: DriverStatus::Waiting { reason },
                ..
            } = event
            {
                waits.push(reason);
            }
        }
        assert_eq!(
            waits,
            [
                moli_i18n::tr!("pilotes.dirigera.appairage_occupe", host = config.host),
                moli_i18n::tr!("pilotes.dirigera.appuyer_bouton", host = config.host),
            ]
        );
        assert_eq!(
            core.secret(&ikea, TOKEN_CERT),
            Some(pin()),
            "the token knows its certificate"
        );
        cancel.cancel();
    }
}
