//! The family's Tesla, through Tesla's official owner API: battery, range,
//! charging, doors and sentry mode, plus the six orders the house gives it.
//!
//! The account is connected once, from the dashboard, the way Alexa is: the
//! driver displays the sign-in address (Tesla's own OAuth, PKCE), the person
//! signs in and pastes the last address back into Moli through the
//! owner-only route (`POST /api/integrations/tesla/{instance}/sign-in`) — it
//! carries the sign-in code. That code never reaches the journal, and a
//! verified `state` parameter binds the pasted code to this sign-in. The
//! refresh token Tesla gives in return is single-use and rotates at every
//! exchange; each new one is kept in the encrypted secret store, never in a
//! file. The owner can disconnect at any time: `sign-out` erases the refresh
//! token from the vault and puts the driver back in the waiting state.
//!
//! Tesla has retired its shared public client, so the sign-in runs with the
//! house's own application (developer.tesla.com): its id and secret are kept
//! as secrets of the instance, and the callback address registered on it is
//! the driver's `redirect_uri`.

mod api;
mod model;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use ring::rand::SecureRandom;
use serde::Deserialize;
use tokio::time::MissedTickBehavior;

use crate::api::{Credentials, Token};
use crate::model::Vehicle;

const REFRESH: &str = "refresh_token";
const VERIFIER: &str = "code_verifier";
const CLIENT_ID: &str = "client_id";
const CLIENT_SECRET: &str = "client_secret";
/// The OAuth `state` parameter of the sign-in page, kept until the pasted
/// callback address proves it belongs to this sign-in.
const STATE: &str = "state";
/// The access token is about to end: renew it, restart the session.
const RENEW: &str = "the Tesla session is renewed";
/// The owner signed the account out from the dashboard: a normal, expected
/// transition back to the waiting state (not an error, no backoff).
const SIGNED_OUT: &str = "the Tesla account was signed out by the owner";
/// The rotated refresh token could not be kept in the vault: the account's
/// status already says `token_not_kept`, so the run loop must not cover it
/// with a plain `error` (and must erase the now-spent token in the vault).
const TOKEN_NOT_KEPT: &str = "the rotated Tesla refresh token was not kept";
/// A parked car (awake, already read, not charging) keeps its last readings
/// this long: older than that, `vehicle_data` is pulled again so the values
/// are not frozen for a whole session.
const PARKED_REFRESH: Duration = Duration::from_secs(30 * 60);
const RETRY_MIN: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(15 * 60);
/// The account's vehicles, looked at this often (a car sold, a name changed).
const DEVICES: Duration = Duration::from_secs(30 * 60);
/// A poll period below this would scratch the API's rate limit.
const MIN_PERIOD_S: u64 = 10;
/// While Tesla has the account disabled by its billing limit, retry this
/// often: the state only ends when the limit is raised or the billing cycle
/// turns, so hammering it would only burn calls against a closed door.
const BILLING_RETRY: Duration = Duration::from_secs(6 * 3600);
/// A renewed session that died before running this long was refused right
/// away: the next renewal then waits longer and longer, so a refused token
/// cannot spin the (paid) refresh loop at network speed.
const RENEW_OK_MIN: Duration = Duration::from_secs(20);

/// Tesla's regional Fleet API hosts. `api_host` must be one of these: a
/// copied-over `moli.toml` must never send the client secret and the refresh
/// token to a different host.
const KNOWN_API_HOSTS: [&str; 3] = [
    "fleet-api.prd.eu.vn.cloud.tesla.com",
    "fleet-api.prd.na.vn.cloud.tesla.com",
    "api.tesla.cn",
];
/// Tesla's OAuth hosts, for the sign-in page and the token endpoint.
const KNOWN_AUTH_HOSTS: [&str; 2] = ["auth.tesla.com", "auth.tesla.cn"];

fn default_api_host() -> String {
    "fleet-api.prd.eu.vn.cloud.tesla.com".into()
}

fn default_auth_host() -> String {
    "auth.tesla.com".into()
}

/// The default poll periods — 5 min while charging, 10 min while awake,
/// 1 h while asleep — sized to stay inside the Fleet API's $10 a month credit
/// for personal use: 500 data requests a dollar, a handful of commands and
/// wakes. A parked car gets a full `vehicle_data` read at most every
/// PARKED_REFRESH; between reads only the cheap `/vehicles` probe goes out.
fn default_poll_charging() -> u64 {
    300
}

fn default_poll_idle() -> u64 {
    600
}

fn default_poll_asleep() -> u64 {
    3600
}

/// `[driver.options]`: the hosts (region) and the poll periods.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The vehicles' API, the regional Fleet API (North America / Asia:
    /// `fleet-api.prd.na.vn.cloud.tesla.com`, China: `api.tesla.cn`).
    #[serde(default = "default_api_host")]
    pub api_host: String,
    /// The app's OAuth (the sign-in page, the token endpoint).
    #[serde(default = "default_auth_host")]
    pub auth_host: String,
    /// Where Tesla sends the person after the sign-in: the callback address
    /// registered on the house's application (developer.tesla.com), pasted
    /// back into Moli.
    pub redirect_uri: String,
    /// The domain registered for the house's application in the active Tesla
    /// region (defaults to the host of `redirect_uri`).
    #[serde(default)]
    pub partner_domain: String,
    /// Every N seconds, while a car is charging or plugged in.
    #[serde(default = "default_poll_charging")]
    pub poll_charging_s: u64,
    /// Every N seconds, while a car is awake and idle.
    #[serde(default = "default_poll_idle")]
    pub poll_idle_s: u64,
    /// Every N seconds, when every car is asleep.
    #[serde(default = "default_poll_asleep")]
    pub poll_asleep_s: u64,
}

#[derive(Debug)]
pub struct Tesla {
    config: Config,
}

impl Tesla {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let Config {
            api_host,
            auth_host,
            redirect_uri,
            partner_domain,
            poll_charging_s,
            poll_idle_s,
            poll_asleep_s,
        } = config;
        let api_host = strip_scheme(&api_host);
        let auth_host = strip_scheme(&auth_host);
        let redirect_uri = redirect_uri.trim().to_owned();
        let mut partner_domain = partner_domain.trim().to_owned();
        if partner_domain.is_empty() {
            partner_domain = host_of(&redirect_uri);
        }
        anyhow::ensure!(!api_host.is_empty(), "api_host: a host, without the scheme");
        anyhow::ensure!(
            !auth_host.is_empty(),
            "auth_host: a host, without the scheme"
        );
        // Closed list (like MELCloud and Voltalis): a copied-over moli.toml
        // must not send the client secret and the refresh token to a third
        // party. Refused at load, not at runtime.
        anyhow::ensure!(
            KNOWN_API_HOSTS.contains(&api_host.as_str()),
            moli_i18n::tr!("pilotes.tesla.hote_inconnu", host = api_host)
        );
        anyhow::ensure!(
            KNOWN_AUTH_HOSTS.contains(&auth_host.as_str()),
            moli_i18n::tr!("pilotes.tesla.hote_inconnu", host = auth_host)
        );
        anyhow::ensure!(!redirect_uri.is_empty(), "redirect_uri: an address");
        anyhow::ensure!(
            !partner_domain.is_empty(),
            "partner_domain: a host (derived from redirect_uri when unset)"
        );
        Ok(Self {
            config: Config {
                api_host,
                auth_host,
                redirect_uri,
                partner_domain,
                poll_charging_s,
                poll_idle_s,
                poll_asleep_s,
            },
        })
    }
}

/// A host without its scheme, for `moli_net::web` (`https://a.b/` -> `a.b`).
fn strip_scheme(host: &str) -> String {
    host.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim()
        .trim_end_matches('/')
        .to_owned()
}

/// The host of a callback address (`https://a.b/x` -> `a.b`).
fn host_of(uri: &str) -> String {
    strip_scheme(uri)
        .split(['/', '?', ':'])
        .next()
        .unwrap_or("")
        .to_owned()
}

impl Driver for Tesla {
    fn kind(&self) -> &'static str {
        "tesla"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// The account: its state, and where the dashboard signs it in.
fn account_device(ctx: &DriverCtx, id: &DeviceId) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.tesla.compte").into(),
        manufacturer: Some("Tesla".into()),
        model: None,
        description: Some(moli_i18n::tr!("pilotes.tesla.description").into()),
        native_room: None,
        members: Vec::new(),
        points: vec![
            text_point(
                "status",
                &moli_i18n::tr!("pilotes.tesla.etat_compte"),
                false,
            ),
            // Sign-in plumbing is internal: the URL is served by the owner-only
            // setup route, and the pasted code travels through a redacted
            // command. Making them readable/writable here would let any
            // household member complete a sign-in without a fresh PIN.
            internal_point("sign_in_url", &moli_i18n::tr!("pilotes.tesla.page_tesla")),
            internal_point("sign_in", &moli_i18n::tr!("pilotes.tesla.adresse_collee")),
            internal_point("sign_out", &moli_i18n::tr!("pilotes.tesla.deconnexion")),
        ],
    }
}

/// A text point. A writable one carries `Semantic::Control` on purpose: an
/// agent's write is held until a human validates it.
fn text_point(key: &str, label: &str, write: bool) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Text,
        access: Access { read: true, write },
        unit: None,
        semantic: if write {
            Semantic::Control
        } else {
            Semantic::Config
        },
    }
}

/// An internal point the driver manages itself: nobody reads or writes it
/// through the normal surfaces. The value only moves through the owner-only
/// setup route, which delivers it to the driver as a redacted command, so it
/// never appears in the journal or the approvals list.
fn internal_point(key: &str, label: &str) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Text,
        access: Access {
            read: false,
            write: false,
        },
        unit: None,
        semantic: Semantic::Config,
    }
}

fn set_text(ctx: &DriverCtx, id: &DeviceId, key: &str, text: &str) {
    ctx.set_state(id, key, Value::Text(text.into()));
}

/// The house's application (developer.tesla.com): Tesla retired its shared
/// public client, so a stored client id is required before any sign-in.
fn credentials(ctx: &DriverCtx) -> anyhow::Result<Credentials> {
    let client_id = ctx
        .secret(CLIENT_ID)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!(moli_i18n::tr!("pilotes.tesla.app_requise")))?;
    Ok(Credentials {
        client_id,
        client_secret: ctx.secret(CLIENT_SECRET).filter(|s| !s.trim().is_empty()),
    })
}

/// A fresh random token (32 bytes, base64url, 43 characters) — the PKCE
/// verifier or the OAuth `state`.
fn fresh_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("cannot make the sign-in tokens"))?;
    Ok(api::base64url(&bytes))
}

/// The sign-in page's address: PKCE challenge derived from the verifier, plus
/// the `state` parameter that binds the pasted callback back to this page.
fn authorize_url(
    config: &Config,
    credentials: &Credentials,
    verifier: &str,
    state: &str,
) -> String {
    let challenge = api::challenge_of(verifier);
    let query = api::form(&[
        ("client_id", credentials.client_id.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("redirect_uri", config.redirect_uri.as_str()),
        ("response_type", "code"),
        ("scope", api::SCOPE),
        ("state", state),
    ]);
    format!("https://{}/oauth2/v3/authorize?{query}", config.auth_host)
}

/// One car, as Moli knows it: its id, and what the next poll period waits for.
#[derive(Debug)]
struct Car {
    /// Tesla's id, in the device's id and in the API's paths.
    id: String,
    awake: bool,
    charging: bool,
    /// `vehicle_data` has been read since the car last woke up: an awake,
    /// already-read, not-charging car is parked — it only costs the cheap
    /// `/vehicles` state probe until it charges again or wakes from sleep.
    read: bool,
    /// When `vehicle_data` was last read: a parked car's readings are
    /// refreshed once this timestamp grows older than `PARKED_REFRESH`.
    read_at: Option<Instant>,
}

impl Car {
    /// Whether the next poll must pull `vehicle_data` for this car: asleep
    /// cars report nothing; an unread or a charging car is always read; a
    /// parked car (awake, read, not charging) only once its last read is
    /// older than `PARKED_REFRESH`, so its values do not stay frozen for a
    /// whole session.
    #[must_use]
    fn needs_vehicle_data(&self, now: Instant) -> bool {
        if !self.awake {
            return false;
        }
        if !self.read || self.charging {
            return true;
        }
        matches!(self.read_at, Some(at) if now.duration_since(at) >= PARKED_REFRESH)
    }
}

fn car_device(ctx: &DriverCtx, id: &DeviceId, v: &Vehicle) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: v.name.clone().into(),
        manufacturer: Some("Tesla".into()),
        model: v.badge().map(Into::into),
        description: Some(moli_i18n::tr!("pilotes.tesla.description").into()),
        native_room: None,
        members: Vec::new(),
        points: model::points(),
    }
}

/// The account's vehicles, published in the house (and removed when sold).
async fn discover(
    config: &Config,
    ctx: &DriverCtx,
    token: &Token,
    previous: &HashMap<DeviceId, Car>,
) -> anyhow::Result<HashMap<DeviceId, Car>> {
    let answer = api::vehicles(config.api_host.as_str(), token).await?;
    let vehicles = model::vehicles_of(&answer);
    let mut cars = HashMap::new();
    for v in vehicles {
        let id = ctx.device_id(&v.id.to_string());
        ctx.upsert_device(car_device(ctx, &id, &v));
        ctx.set_availability(&id, true);
        ctx.set_state(&id, "awake", Value::Bool(v.awake));
        if !v.awake {
            // Asleep: the car reports no fresh values, so a stale power or
            // charging flag must not keep integrating energy after a restart.
            ctx.set_state(&id, "charge_power", Value::Float(0.0));
            ctx.set_state(&id, "charging", Value::Bool(false));
        }
        // A car that was awake and is still awake keeps its charging flag,
        // its read flag and its last-read time: the cheap `/vehicles` probe
        // alone tracks its sleep state, a charging car must keep being read,
        // and the parking clock must not restart on every discover. A car
        // that was asleep (or seen for the first time) is read again.
        let carried = previous.get(&id).filter(|p| p.awake && v.awake);
        cars.insert(
            id,
            Car {
                id: v.id.to_string(),
                awake: v.awake,
                charging: carried.is_some_and(|p| p.charging),
                read: carried.is_some_and(|p| p.read),
                read_at: carried.and_then(|p| p.read_at),
            },
        );
    }
    for gone in previous.keys() {
        if !cars.contains_key(gone) {
            ctx.remove_device(gone);
        }
    }
    Ok(cars)
}

/// The awake cars' state, point by point; what the car does not say keeps its
/// last value.
async fn refresh_cars(
    config: &Config,
    ctx: &DriverCtx,
    token: &Token,
    cars: &mut HashMap<DeviceId, Car>,
) -> anyhow::Result<()> {
    for (id, car) in cars.iter_mut() {
        // Asleep cars report nothing; an unread or a charging car is always
        // read; a parked one (awake, read, not charging) only once its last
        // read has grown older than PARKED_REFRESH.
        if !car.needs_vehicle_data(Instant::now()) {
            continue;
        }
        let answer = match api::vehicle_data(config.api_host.as_str(), token, &car.id).await {
            Ok(answer) => answer,
            Err(e) if e.to_string().contains(api::VEHICLE_AWAY) => {
                // A 408 is a per-vehicle condition, not a session failure:
                // mark just this car asleep and keep polling the others.
                tracing::info!(
                    instance = %ctx.instance(),
                    vehicle = %car.id,
                    "tesla vehicle went to sleep (408): session stays connected"
                );
                car.awake = false;
                car.charging = false;
                ctx.set_state(id, "awake", Value::Bool(false));
                ctx.set_state(id, "charge_power", Value::Float(0.0));
                ctx.set_state(id, "charging", Value::Bool(false));
                continue;
            }
            Err(e) => return Err(e),
        };
        car.read = true;
        car.read_at = Some(Instant::now());
        let r = model::readings_of(&answer);
        if let Some(v) = r.soc {
            ctx.set_state(id, "soc", Value::Float(v));
        }
        if let Some(v) = r.range_km {
            ctx.set_state(id, "range", Value::Float(v));
        }
        if let Some(v) = r.charging {
            car.charging = v;
            ctx.set_state(id, "charging", Value::Bool(v));
        }
        if let Some(v) = r.charge_power_w {
            ctx.set_state(id, "charge_power", Value::Float(v));
        }
        if let Some(v) = r.plugged {
            ctx.set_state(id, "plugged_in", Value::Bool(v));
        }
        if let Some(v) = r.doors_locked {
            ctx.set_state(id, "doors_locked", Value::Bool(v));
        }
        if let Some(v) = r.sentry {
            ctx.set_state(id, "sentry", Value::Bool(v));
        }
        if let Some(v) = r.inside_c {
            ctx.set_state(id, "inside_temp", Value::Float(v));
        }
        if let Some(v) = r.outside_c {
            ctx.set_state(id, "outside_temp", Value::Float(v));
        }
    }
    Ok(())
}

/// One poll of the account's cars; an expired access token renews the
/// session (the run loop restarts `connected` from the kept refresh token).
async fn poll_once(
    config: &Config,
    ctx: &DriverCtx,
    token: &Token,
    previous: &HashMap<DeviceId, Car>,
) -> anyhow::Result<HashMap<DeviceId, Car>> {
    let mut cars = match discover(config, ctx, token, previous).await {
        Ok(cars) => cars,
        Err(e) if e.to_string().contains(api::TOKEN_EXPIRED) => anyhow::bail!("{RENEW}"),
        Err(e) => return Err(e),
    };
    match refresh_cars(config, ctx, token, &mut cars).await {
        Ok(()) => Ok(cars),
        Err(e) if e.to_string().contains(api::TOKEN_EXPIRED) => Err(anyhow::anyhow!("{RENEW}")),
        Err(e) => Err(e),
    }
}

/// The pause Tesla asked for after a 429, when there is one.
fn rate_limited_secs(error: &anyhow::Error) -> Option<u64> {
    error.downcast_ref::<api::RateLimited>().map(|r| r.secs)
}

/// What a sign-out does, wherever it comes from (live session or paused
/// driver): erase the vault's tokens, take the cars offline (their last word
/// must not look live) and put the account back to the waiting state.
async fn perform_sign_out(ctx: &mut DriverCtx, account: &DeviceId) -> std::io::Result<()> {
    ctx.forget_secret(REFRESH).await?;
    let _ = ctx.forget_secret(VERIFIER).await;
    let _ = ctx.forget_secret(STATE).await;
    for device in ctx.devices() {
        if device != *account {
            ctx.set_availability(&device, false);
        }
    }
    set_text(ctx, account, "status", "sign_in");
    Ok(())
}

/// How a run-loop pause ended.
enum PauseEnd {
    /// The pause ran its course.
    Elapsed,
    /// Moli stopped (`next_command` saw the cancel token).
    Stopped,
    /// The owner signed the account out while the driver was paused.
    SignedOut,
}

/// Waits `pause`, but keeps listening for the owner's orders: a sign-out must
/// work in every state — during an error backoff, a 429 wait, the 6 h billing
/// retry or a renewal pause. A `sign_out` is honoured in place (see
/// [`perform_sign_out`]); any other order is refused (no live session).
async fn pause_listening(ctx: &mut DriverCtx, account: &DeviceId, pause: Duration) -> PauseEnd {
    let until = tokio::time::Instant::now() + pause;
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(until) => return PauseEnd::Elapsed,
            command = ctx.next_command() => {
                let Some(command) = command else {
                    return PauseEnd::Stopped;
                };
                if command.device.id == *account && &*command.key == "sign_out" {
                    match perform_sign_out(ctx, account).await {
                        Ok(()) => {
                            command.reply(Ok(()));
                            return PauseEnd::SignedOut;
                        }
                        Err(e) => {
                            // The vault write failed: surface it so the owner
                            // can retry; the pause goes on.
                            command.reply(Err(moli_i18n::tr!(
                                "pilotes.tesla.coffre",
                                why = e
                            )));
                        }
                    }
                } else {
                    command.reply(Err(moli_i18n::tr!("pilotes.tesla.pas_connecte")));
                }
            }
        }
    }
}

/// A poll until it goes through: a 429 asks a pause (`Retry-After`, clamped
/// to 60 s–6 h), taken in full before the next try — the API's own clock.
/// The driver keeps listening for the owner's orders during the pause.
async fn poll_with_patience(
    config: &Config,
    ctx: &mut DriverCtx,
    account: &DeviceId,
    token: &Token,
    cars: &mut HashMap<DeviceId, Car>,
) -> anyhow::Result<()> {
    loop {
        match poll_once(config, ctx, token, cars).await {
            Ok(fresh) => {
                *cars = fresh;
                return Ok(());
            }
            Err(e) => {
                let Some(secs) = rate_limited_secs(&e) else {
                    return Err(e);
                };
                tracing::warn!(
                    instance = %ctx.instance(),
                    secs,
                    "tesla rate limited: waiting the requested pause"
                );
                match pause_listening(ctx, account, Duration::from_secs(secs)).await {
                    PauseEnd::Elapsed => {}
                    PauseEnd::Stopped => return Ok(()),
                    PauseEnd::SignedOut => anyhow::bail!("{SIGNED_OUT}"),
                }
            }
        }
    }
}

/// How often to look, from the last words of the cars. The fast period is for
/// active charging only: a car that is plugged in but not drawing charge is
/// parked, and a parked car costs the cheap `/vehicles` state probe, not the
/// full `vehicle_data` pull.
fn period_of(cars: &HashMap<DeviceId, Car>, config: &Config) -> Duration {
    let value = if cars.values().any(|c| c.charging) {
        config.poll_charging_s
    } else if cars.values().any(|c| c.awake) {
        config.poll_idle_s
    } else {
        config.poll_asleep_s
    };
    Duration::from_secs(value.max(MIN_PERIOD_S))
}

/// A fresh sign-in page: a new verifier and a new `state` kept in the store,
/// and the address on the account's `sign_in_url` point.
async fn start_page(
    config: &Config,
    ctx: &DriverCtx,
    account: &DeviceId,
    credentials: &Credentials,
) -> anyhow::Result<()> {
    let v = fresh_token()?;
    let s = fresh_token()?;
    ctx.store_secret(VERIFIER, &v)
        .await
        .map_err(|e| anyhow::anyhow!(moli_i18n::tr!("pilotes.tesla.coffre", why = e)))?;
    ctx.store_secret(STATE, &s)
        .await
        .map_err(|e| anyhow::anyhow!(moli_i18n::tr!("pilotes.tesla.coffre", why = e)))?;
    set_text(
        ctx,
        account,
        "sign_in_url",
        &authorize_url(config, credentials, &v, &s),
    );
    Ok(())
}

/// `Some(key)`: the pasted text must not be exchanged for a token — an
/// address (it carries a `code=` parameter) must carry the `state` this
/// sign-in page was opened with, and a mismatch or a missing `state` on an
/// address is refused. A bare code is accepted without a `state`: PKCE
/// covers it.
fn state_mismatch(pasted: &str, expected_state: Option<&str>) -> Option<&'static str> {
    let pasted_state = model::state_of(pasted);
    let ok = match (expected_state, pasted_state.as_deref()) {
        (Some(expected), Some(pasted_state)) => expected == pasted_state,
        _ => !model::is_address(pasted),
    };
    if ok {
        None
    } else if pasted_state.is_none() {
        Some("pilotes.tesla.adresse_sans_state")
    } else {
        Some("pilotes.tesla.state_invalide")
    }
}

/// Waits for the person to sign in from the dashboard. `false`: stopped.
///
/// While the vault lacks the house's application credentials the account
/// stays `app_required` and re-checks on every wake-up: `moli-os secrets set`
/// makes the sign-in page appear without restarting Moli.
async fn sign_in(config: &Config, ctx: &mut DriverCtx, account: &DeviceId) -> anyhow::Result<bool> {
    let mut app_creds: Option<Credentials> = None;
    loop {
        let creds = match app_creds.as_ref() {
            Some(creds) => creds.clone(),
            None => match credentials(ctx) {
                Ok(creds) => {
                    start_page(config, ctx, account, &creds).await?;
                    set_text(ctx, account, "status", "sign_in");
                    ctx.wait_for(moli_i18n::tr!("pilotes.tesla.a_connecter"));
                    app_creds = Some(creds.clone());
                    creds
                }
                Err(why) => {
                    // Tesla's shared public client is retired: the house's own
                    // application (developer.tesla.com) is required.
                    set_text(ctx, account, "status", "app_required");
                    set_text(ctx, account, "sign_in_url", "");
                    ctx.wait_for(why.to_string());
                    // Poll until the credentials land (no restart needed).
                    tokio::select! {
                        () = tokio::time::sleep(RETRY_MIN) => {}
                        () = ctx.cancelled() => return Ok(false),
                    }
                    continue;
                }
            },
        };
        // `None` once Moli stops.
        let Some(command) = ctx.next_command().await else {
            return Ok(false);
        };
        if command.device.id != *account {
            command.reply(Err(moli_i18n::tr!("pilotes.tesla.pas_connecte")));
            continue;
        }
        if &*command.key == "sign_out" {
            // Sign-out while waiting: the pending page is dropped and a fresh
            // one is shown (its `state` must match the next pasted address).
            start_page(config, ctx, account, &creds).await?;
            command.reply(Ok(()));
            continue;
        }
        if &*command.key != "sign_in" {
            command.reply(Err(moli_i18n::tr!("pilotes.tesla.pas_connecte")));
            continue;
        }
        let Value::Text(pasted) = &command.value else {
            command.reply(Err(moli_i18n::tr!("pilotes.tesla.adresse_fausse")));
            continue;
        };
        let Some(code) = model::code_of(pasted) else {
            command.reply(Err(moli_i18n::tr!("pilotes.tesla.adresse_sans_code")));
            continue;
        };
        // The pasted address must bind to this sign-in: a stale or foreign
        // page must not be exchanged for a token (a bare code is fine —
        // PKCE covers it).
        let expected_state = ctx.secret(STATE);
        if let Some(key) = state_mismatch(pasted, expected_state.as_deref()) {
            start_page(config, ctx, account, &creds).await?;
            command.reply(Err(moli_i18n::tr!(key)));
            continue;
        }
        let stored = ctx.secret(VERIFIER).unwrap_or_default();
        let audience = format!("https://{}", config.api_host);
        match api::code_for(
            config.auth_host.as_str(),
            &creds,
            &code,
            stored.as_str(),
            &config.redirect_uri,
            &audience,
        )
        .await
        {
            Ok(token) => {
                if let Err(e) = ctx.store_secret(REFRESH, &token.refresh).await {
                    command.reply(Err(moli_i18n::tr!("pilotes.tesla.coffre", why = e)));
                    return Err(anyhow::anyhow!("{e}"));
                }
                let _ = ctx.forget_secret(VERIFIER).await;
                let _ = ctx.forget_secret(STATE).await;
                set_text(ctx, account, "sign_in_url", "");
                command.reply(Ok(()));
                tracing::info!(instance = %ctx.instance(), "tesla account connected");
                ctx.ready();
                return Ok(true);
            }
            Err(e) => {
                tracing::warn!(
                    instance = %ctx.instance(),
                    error = %format!("{e:#}"),
                    "tesla sign-in refused"
                );
                // A code serves once: a new page for the next try.
                start_page(config, ctx, account, &creds).await?;
                command.reply(Err(moli_i18n::tr!(
                    "pilotes.tesla.refuse",
                    why = format!("{e:#}")
                )));
            }
        }
    }
}

/// Connected: the cars, their orders, then their state, until stopped.
/// `registered` carries the app-activation across renewals within one run:
/// the registration is one-time per region, not re-sent at every renewal.
async fn connected(
    config: &Config,
    ctx: &mut DriverCtx,
    account: &DeviceId,
    refresh: String,
    registered: &mut bool,
) -> anyhow::Result<()> {
    let credentials = credentials(ctx)?;
    if !*registered {
        // App activation is one-time per region but idempotent: only the
        // first attempt of the run registers; a failed non-billing attempt
        // still counts (a warning is logged, and a driver restart begins
        // fresh).
        match api::partner_register(
            config.auth_host.as_str(),
            config.api_host.as_str(),
            &credentials,
            &config.partner_domain,
        )
        .await
        {
            // A billing-disabled account refuses even registration: bail
            // before burning the token exchange and the first poll against a
            // closed door (the flag stays unset: the retry registers first).
            Err(e) if e.to_string().contains(api::ACCOUNT_DISABLED) => return Err(e),
            other => {
                *registered = true;
                if let Err(e) = other {
                    tracing::warn!(
                        instance = %ctx.instance(),
                        domain = %config.partner_domain,
                        error = %format!("{e:#}"),
                        "tesla partner registration failed: the car may stay unreachable"
                    );
                }
            }
        }
    }
    let mut token = api::refresh(config.auth_host.as_str(), &credentials, &refresh).await?;
    if let Err(e) = ctx.store_secret(REFRESH, &token.refresh).await {
        // The rotated token is lost: the next renewal will be refused and the
        // account must sign in again. The account status says so until then
        // (a later successful renewal clears it); the run loop must not cover
        // it with a plain `error`.
        tracing::warn!(
            instance = %ctx.instance(),
            error = %e,
            "tesla rotated refresh token not kept"
        );
        set_text(ctx, account, "status", "token_not_kept");
        anyhow::bail!("{TOKEN_NOT_KEPT}");
    }
    let mut cars: HashMap<DeviceId, Car> = HashMap::new();
    poll_with_patience(config, ctx, account, &token, &mut cars).await?;
    set_text(ctx, account, "status", "connected");
    ctx.ready();
    tracing::info!(
        instance = %ctx.instance(),
        cars = cars.len(),
        "tesla connected"
    );
    let mut devices = tokio::time::interval(DEVICES);
    devices.set_missed_tick_behavior(MissedTickBehavior::Delay);
    devices.reset();
    loop {
        let period = period_of(&cars, config);
        // Recomputed on every iteration: an order may have renewed the access
        // token while it ran, and the deadline follows the fresh token.
        let renew_at = tokio::time::Instant::from_std(token.expires_at);
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else {
                    return Ok(());
                };
                if command.device.id == *account && &*command.key == "sign_out" {
                    // The owner signed out from the dashboard: erase the
                    // refresh token (and the pending sign-in tokens), take the
                    // cars offline, and let `run` take the driver back to the
                    // waiting state.
                    match perform_sign_out(ctx, account).await {
                        Ok(()) => {
                            command.reply(Ok(()));
                            anyhow::bail!("{SIGNED_OUT}");
                        }
                        Err(e) => {
                            // The session is still valid; surface the failure
                            // so the dashboard can report it and retry.
                            command.reply(Err(moli_i18n::tr!("pilotes.tesla.coffre", why = e)));
                            continue;
                        }
                    }
                }
                let result =
                    order(config, ctx, &credentials, &mut token, &cars, account, &command).await;
                command.reply(result.map_err(|why| {
                    moli_i18n::tr!("pilotes.tesla.echec", why = why)
                }));
            }
            () = tokio::time::sleep(period) => {
                poll_with_patience(config, ctx, account, &token, &mut cars).await?;
            }
            _ = devices.tick() => {
                poll_with_patience(config, ctx, account, &token, &mut cars).await?;
            }
            () = tokio::time::sleep_until(renew_at) => {
                // A fresh token: `connected` starts again.
                anyhow::bail!("{RENEW}");
            }
        }
    }
}

/// One order; an expired access token is renewed once and the order retried.
/// The renewed token replaces the caller's (`token` is `&mut`) so the session
/// stays current; the rotated refresh token is kept in the vault BEFORE the
/// order's deadline is checked, so a late refusal can never lose it.
async fn order(
    config: &Config,
    ctx: &mut DriverCtx,
    credentials: &Credentials,
    token: &mut Token,
    cars: &HashMap<DeviceId, Car>,
    account: &DeviceId,
    command: &CommandRequest,
) -> Result<(), String> {
    let Some(car) = cars.get(&command.device.id) else {
        return Err(moli_i18n::tr!("pilotes.tesla.pas_connecte"));
    };
    let Value::Text(text) = &command.value else {
        return Err(moli_i18n::tr!("pilotes.tesla.commande_inconnue", cmd = "?"));
    };
    let Some(order) = model::order_of(text) else {
        return Err(moli_i18n::tr!(
            "pilotes.tesla.commande_inconnue",
            cmd = text.as_ref()
        ));
    };
    let (cmd, body) = model::api_command(order);
    match api::command(config.api_host.as_str(), token, &car.id, cmd, body).await {
        Ok(result) => result,
        Err(e) if e.to_string().contains(api::TOKEN_EXPIRED) => {
            let fresh =
                match api::refresh(config.auth_host.as_str(), credentials, &token.refresh).await {
                    Ok(fresh) => fresh,
                    Err(e) => return Err(format!("{e:#}")),
                };
            // Keep the rotated token in the vault BEFORE anything else: a
            // late refusal (deadline passed) or a refused retry must not lose
            // it — the next renewal would be refused and the account would
            // have to sign in again.
            if let Err(e) = ctx.store_secret(REFRESH, &fresh.refresh).await {
                // The rotated token is lost: the next renewal will be refused
                // and the account must sign in again. The account status says
                // so until then (a later successful renewal clears it).
                tracing::warn!(
                    instance = %ctx.instance(),
                    error = %e,
                    "tesla rotated refresh token not kept"
                );
                set_text(ctx, account, "status", "token_not_kept");
            }
            // The token dance can take up to ten seconds: the hub's command
            // deadline (five) may have passed while it ran. Keep the renewed
            // token either way; a finished order no one waits for must not
            // reach the car.
            *token = fresh;
            if command.abandoned() {
                return Err(moli_i18n::tr!("pilotes.tesla.trop_tard"));
            }
            match api::command(config.api_host.as_str(), token, &car.id, cmd, body).await {
                Ok(result) => result,
                Err(e) => Err(format!("{e:#}")),
            }
        }
        Err(e) => Err(format!("{e:#}")),
    }
}

/// The life of the driver: sign in, then connect, until stopped.
async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let account = ctx.device_id("account");
    ctx.upsert_device(account_device(ctx, &account));
    ctx.set_availability(&account, true);
    let mut pause = RETRY_MIN;
    // App activation is one-time per region: registered once per run, never
    // re-sent at every renewal.
    let mut registered = false;
    loop {
        let Some(refresh) = ctx.secret(REFRESH) else {
            if !sign_in(config, ctx, &account).await? {
                return Ok(());
            }
            continue;
        };
        set_text(ctx, &account, "status", "connecting");
        let started = Instant::now();
        match connected(config, ctx, &account, refresh, &mut registered).await {
            Ok(()) => return Ok(()),
            Err(e) if format!("{e:#}").contains(RENEW) => {
                // A session that dies before RENEW_OK_MIN was refused right
                // away: grow the backoff so a refused token cannot spin the
                // (paid) refresh loop at network speed. A healthy session
                // (at least RENEW_OK_MIN old) just resets the backoff — the
                // renewal is then the normal token turnover.
                tracing::info!(instance = %ctx.instance(), "tesla session renewed");
                if started.elapsed() < RENEW_OK_MIN {
                    pause = (pause * 2).min(RETRY_MAX);
                } else {
                    pause = RETRY_MIN;
                }
                match pause_listening(ctx, &account, pause).await {
                    PauseEnd::Stopped => return Ok(()),
                    PauseEnd::SignedOut => {
                        tracing::info!(
                            instance = %ctx.instance(),
                            "tesla signed out by the owner: back to sign-in"
                        );
                    }
                    PauseEnd::Elapsed => {}
                }
            }
            Err(e) if format!("{e:#}").contains(SIGNED_OUT) => {
                // A normal owner action, not a failure: back to the waiting
                // state for a fresh sign-in, no backoff.
                tracing::info!(
                    instance = %ctx.instance(),
                    "tesla signed out by the owner: back to sign-in"
                );
            }
            // The rotated refresh token could not be kept in the vault: the
            // account status already says `token_not_kept`, so this arm must
            // not cover it with a plain `error`. The refresh token left in
            // the vault is spent (rotated by Tesla): erase it and go back to
            // the sign-in.
            Err(e) if format!("{e:#}").contains(TOKEN_NOT_KEPT) => {
                tracing::warn!(
                    instance = %ctx.instance(),
                    "tesla rotated token not kept: the vault's refresh token is spent, sign in again"
                );
                ctx.forget_secret(REFRESH).await?;
            }
            Err(e) if format!("{e:#}").contains(api::REFRESH_INVALID) => {
                tracing::warn!(
                    instance = %ctx.instance(),
                    "tesla refresh token invalid: sign in again"
                );
                ctx.forget_secret(REFRESH).await?;
            }
            // Billing limit exceeded: every call is refused (403) until the
            // limit is raised or the billing cycle turns — retrying every
            // 30 s would only burn calls against a closed door.
            Err(e) if format!("{e:#}").contains(api::ACCOUNT_DISABLED) => {
                tracing::warn!(
                    instance = %ctx.instance(),
                    error = %format!("{e:#}"),
                    "tesla account disabled by its billing limit: retrying every 6 h"
                );
                set_text(ctx, &account, "status", "billing_limit");
                pause = RETRY_MIN;
                match pause_listening(ctx, &account, BILLING_RETRY).await {
                    PauseEnd::Stopped => return Ok(()),
                    PauseEnd::SignedOut => {
                        tracing::info!(
                            instance = %ctx.instance(),
                            "tesla signed out by the owner: back to sign-in"
                        );
                    }
                    PauseEnd::Elapsed => {}
                }
            }
            Err(e) => {
                tracing::warn!(
                    instance = %ctx.instance(),
                    error = %format!("{e:#}"),
                    "tesla unreachable"
                );
                set_text(ctx, &account, "status", "error");
                if started.elapsed() > RETRY_MAX {
                    pause = RETRY_MIN;
                }
                match pause_listening(ctx, &account, pause).await {
                    PauseEnd::Stopped => return Ok(()),
                    PauseEnd::SignedOut => {
                        tracing::info!(
                            instance = %ctx.instance(),
                            "tesla signed out by the owner: back to sign-in"
                        );
                    }
                    PauseEnd::Elapsed => {}
                }
                pause = (pause * 2).min(RETRY_MAX);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moli_core::InstanceId;

    #[test]
    fn the_options_default_to_the_european_fleet_hosts() {
        let parsed: Config = serde_json::from_value(serde_json::json!({
            "redirect_uri": "https://example.com/tesla"
        }))
        .unwrap();
        assert_eq!(parsed.api_host, "fleet-api.prd.eu.vn.cloud.tesla.com");
        assert_eq!(parsed.auth_host, "auth.tesla.com");
        assert_eq!(parsed.redirect_uri, "https://example.com/tesla");
        assert_eq!(
            (
                parsed.poll_charging_s,
                parsed.poll_idle_s,
                parsed.poll_asleep_s
            ),
            (300, 600, 3600)
        );
        assert!(
            serde_json::from_value::<Config>(serde_json::json!({ "nope": 1 })).is_err(),
            "unknown options are refused"
        );
        let t = Tesla::new(parsed).unwrap();
        assert_eq!(t.config.partner_domain, "example.com");
    }

    #[test]
    fn the_callback_address_is_required() {
        assert!(
            serde_json::from_value::<Config>(serde_json::json!({})).is_err(),
            "a missing redirect_uri is refused (no default: it must match the app registration)"
        );
    }

    #[test]
    fn a_scheme_is_stripped_from_the_hosts() {
        let config = Config {
            api_host: "https://fleet-api.prd.eu.vn.cloud.tesla.com/".into(),
            auth_host: "http://auth.tesla.com".into(),
            redirect_uri: "https://example.com/moli-tesla".into(),
            partner_domain: String::new(),
            poll_charging_s: 60,
            poll_idle_s: 180,
            poll_asleep_s: 600,
        };
        let t = Tesla::new(config).unwrap();
        assert_eq!(t.config.api_host, "fleet-api.prd.eu.vn.cloud.tesla.com");
        assert_eq!(t.config.auth_host, "auth.tesla.com");
    }

    #[test]
    fn a_schemeless_host_stays_a_host() {
        let config = Config {
            api_host: "fleet-api.prd.eu.vn.cloud.tesla.com".into(),
            auth_host: "auth.tesla.com".into(),
            redirect_uri: "https://example.com/moli-tesla".into(),
            partner_domain: String::new(),
            poll_charging_s: 60,
            poll_idle_s: 180,
            poll_asleep_s: 600,
        };
        let t = Tesla::new(config).unwrap();
        assert_eq!(t.config.api_host, "fleet-api.prd.eu.vn.cloud.tesla.com");
    }

    #[test]
    fn the_period_follows_the_cars() {
        let config = Config {
            api_host: "fleet-api.prd.eu.vn.cloud.tesla.com".into(),
            auth_host: "auth.tesla.com".into(),
            redirect_uri: "https://example.com/moli-tesla".into(),
            partner_domain: String::new(),
            poll_charging_s: 60,
            poll_idle_s: 180,
            poll_asleep_s: 900,
        };
        let empty: HashMap<DeviceId, Car> = HashMap::new();
        assert_eq!(period_of(&empty, &config), Duration::from_secs(900));
        let id = DeviceId::new(&InstanceId::from("tesla"), "1");
        // Awake, read, not charging: the car is parked (plugged or not —
        // `plugged` no longer feeds the period) — idle period applies.
        let awake_read_idle = HashMap::from([(
            id.clone(),
            Car {
                id: "1".into(),
                awake: true,
                charging: false,
                read: true,
                read_at: Some(Instant::now()),
            },
        )]);
        assert_eq!(
            period_of(&awake_read_idle, &config),
            Duration::from_secs(180)
        );
        let charging = HashMap::from([(
            id,
            Car {
                id: "1".into(),
                awake: true,
                charging: true,
                read: true,
                read_at: Some(Instant::now()),
            },
        )]);
        assert_eq!(period_of(&charging, &config), Duration::from_secs(60));
    }

    #[test]
    fn a_parked_car_is_re_read_once_its_last_read_grows_old() {
        let now = Instant::now();
        let mut car = Car {
            id: "1".into(),
            awake: true,
            charging: false,
            read: true,
            read_at: Some(now),
        };
        // Asleep: never read, even with a stale timestamp.
        car.awake = false;
        assert!(!car.needs_vehicle_data(now + Duration::from_secs(7 * 3600)));
        // Awake again, never read in this session: read it.
        car.awake = true;
        car.read = false;
        car.read_at = None;
        assert!(car.needs_vehicle_data(now));
        // Charging: always read, whatever the last read.
        car.read = true;
        car.read_at = Some(now);
        car.charging = true;
        assert!(car.needs_vehicle_data(now));
        // Parked, read a minute ago: the cheap `/vehicles` probe alone.
        car.charging = false;
        assert!(!car.needs_vehicle_data(now + Duration::from_secs(60)));
        // Parked, read long ago (older than PARKED_REFRESH): values are stale,
        // pull them again.
        assert!(car.needs_vehicle_data(now + PARKED_REFRESH + Duration::from_secs(1)));
    }

    #[test]
    fn a_shorter_period_is_refused() {
        let config = Config {
            api_host: "fleet-api.prd.eu.vn.cloud.tesla.com".into(),
            auth_host: "auth.tesla.com".into(),
            redirect_uri: "https://example.com/moli-tesla".into(),
            partner_domain: String::new(),
            poll_charging_s: 2,
            poll_idle_s: 180,
            poll_asleep_s: 900,
        };
        let id = DeviceId::new(&InstanceId::from("tesla"), "1");
        let charging = HashMap::from([(
            id,
            Car {
                id: "1".into(),
                awake: true,
                charging: true,
                read: false,
                read_at: None,
            },
        )]);
        assert_eq!(
            period_of(&charging, &config),
            Duration::from_secs(MIN_PERIOD_S)
        );
    }

    #[test]
    fn an_unknown_host_is_refused_at_load() {
        let config = Config {
            api_host: "evil.example.com".into(),
            auth_host: "auth.tesla.com".into(),
            redirect_uri: "https://example.com/moli-tesla".into(),
            partner_domain: String::new(),
            poll_charging_s: 60,
            poll_idle_s: 180,
            poll_asleep_s: 900,
        };
        let err = Tesla::new(config).unwrap_err();
        assert!(
            err.to_string().contains("evil.example.com"),
            "unknown api_host must be refused at load: {err}"
        );

        let config = Config {
            api_host: "fleet-api.prd.eu.vn.cloud.tesla.com".into(),
            auth_host: "evil.example.com".into(),
            redirect_uri: "https://example.com/moli-tesla".into(),
            partner_domain: String::new(),
            poll_charging_s: 60,
            poll_idle_s: 180,
            poll_asleep_s: 900,
        };
        let err = Tesla::new(config).unwrap_err();
        assert!(
            err.to_string().contains("evil.example.com"),
            "unknown auth_host must be refused at load: {err}"
        );
    }
}
