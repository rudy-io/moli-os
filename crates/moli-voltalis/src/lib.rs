//! Voltalis heaters through the web API: on/off, mode, setpoint and return to
//! the programme, read and commanded, and the service's own diagnosis.
//!
//! The service has no lasting key: the driver signs in with the account
//! (`username`, `password`, from the encrypted store) and keeps the session
//! token in memory only. It signs in again once when a session ends, and a
//! refused account stops it: repeating a bad password could lock the account.

mod model;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Context as _;
use http::header::{ACCEPT, AUTHORIZATION, HOST, HeaderValue, USER_AGENT};
use model::Heater;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::json;

const API_HOST: &str = "api.myvoltalis.com";
const USERNAME: &str = "username";
const PASSWORD: &str = "password";
const MAX_RESPONSE: usize = 1 << 20;
const TIMEOUT: Duration = Duration::from_secs(20);
/// Failed polls in a row before the heaters are shown as unreachable.
const PATIENCE: u32 = 3;
/// An order answered after the hub gave up would look failed and still run.
const ORDER_BUDGET: Duration = Duration::from_millis(4500);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_poll")]
    pub poll_seconds: u64,
}

fn default_poll() -> u64 {
    300
}

#[derive(Debug)]
pub struct Voltalis {
    config: Config,
}

impl Voltalis {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Voltalis {
    fn kind(&self) -> &'static str {
        "voltalis"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

/// What a request can end in, beyond an answer.
enum Failure {
    /// 401 or 403: the session ended, or the account was refused.
    Refused,
    Other(anyhow::Error),
}

struct Session {
    bearer: HeaderValue,
    site: u64,
}

fn request(path: &str) -> http::request::Builder {
    http::Request::builder()
        .uri(path)
        .header(HOST, API_HOST)
        .header(USER_AGENT, "moli-os")
        .header(ACCEPT, "application/json")
}

async fn exchange(
    built: anyhow::Result<http::Request<http_body_util::Full<moli_net::Body>>>,
) -> Result<Vec<u8>, Failure> {
    let built = built.map_err(Failure::Other)?;
    // Underlying errors are dropped on purpose: no request detail in a status.
    let (status, body) = moli_net::web(API_HOST, 443, true, built, TIMEOUT, MAX_RESPONSE)
        .await
        .map_err(|_| Failure::Other(anyhow::anyhow!(moli_i18n::tr!("pilotes.voltalis.reseau"))))?;
    if matches!(status.as_u16(), 401 | 403) {
        return Err(Failure::Refused);
    }
    if !status.is_success() {
        return Err(Failure::Other(anyhow::anyhow!(moli_i18n::tr!(
            "pilotes.voltalis.statut",
            status = status.as_u16()
        ))));
    }
    Ok(body.to_vec())
}

async fn get(path: &str, bearer: &HeaderValue) -> Result<Vec<u8>, Failure> {
    let builder = request(path)
        .method(http::Method::GET)
        .header(AUTHORIZATION, bearer.clone());
    exchange(moli_net::empty(builder)).await
}

async fn login(username: &str, password: &str) -> Result<Session, Failure> {
    let builder = request("/auth/login").method(http::Method::POST);
    let credentials = json!({ "login": username, "password": password });
    let answer = exchange(moli_net::json_body(builder, &credentials)).await?;
    let token = model::token(&answer).map_err(Failure::Other)?;
    let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))
        .ok()
        .context(moli_i18n::tr!("pilotes.voltalis.reponse_illisible"))
        .map_err(Failure::Other)?;
    bearer.set_sensitive(true);
    let me = get("/api/account/me", &bearer).await?;
    let site = model::site(&me).map_err(Failure::Other)?;
    Ok(Session { bearer, site })
}

/// The heaters and, when the service gives it, the ones it reports a problem on.
/// A diagnosis that fails must not hide the heaters: it is then simply unknown.
async fn read_site(session: &Session) -> Result<(Vec<Heater>, Option<HashSet<u64>>), Failure> {
    let site = session.site;
    let list = get(
        &format!("/api/site/{site}/managed-appliance"),
        &session.bearer,
    )
    .await?;
    let heaters = model::heaters(&list).map_err(Failure::Other)?;
    let faulty = match get(&format!("/api/site/{site}/autodiag"), &session.bearer).await {
        Ok(answer) => model::faulty(&answer).ok(),
        Err(Failure::Refused) => return Err(Failure::Refused),
        Err(Failure::Other(_)) => None,
    };
    Ok((heaters, faulty))
}

/// Sends an order, signing in first if the session ended. The session is tried
/// again once: it can end between two polls, and sending the same setting twice
/// is harmless. Only the sign-in decides that the account itself is refused.
async fn send_order(
    session: &mut Option<Session>,
    username: &str,
    password: &str,
    heater: &Heater,
    key: &str,
    value: &Value,
) -> Result<(), Failure> {
    for _ in 0..2 {
        if session.is_none() {
            *session = Some(login(username, password).await?);
        }
        let Some(open) = session.as_ref() else {
            continue;
        };
        match put_setting(open, heater, key, value).await {
            Err(Failure::Refused) => *session = None,
            other => return other,
        }
    }
    Err(Failure::Other(anyhow::anyhow!(moli_i18n::tr!(
        "pilotes.voltalis.ordre_refuse"
    ))))
}

async fn put_setting(
    session: &Session,
    heater: &Heater,
    key: &str,
    value: &Value,
) -> Result<(), Failure> {
    let site = session.site;
    let now = jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:00Z")
        .to_string();
    let body = model::order(heater, key, value, &now)
        .map_err(|message| Failure::Other(anyhow::anyhow!(message)))?;
    let settings = get(&format!("/api/site/{site}/manualsetting"), &session.bearer).await?;
    let setting = model::setting_of(&settings, heater.id).map_err(Failure::Other)?;
    let builder = request(&format!("/api/site/{site}/manualsetting/{setting}"))
        .method(http::Method::PUT)
        .header(AUTHORIZATION, session.bearer.clone());
    exchange(moli_net::json_body(builder, &body)).await?;
    Ok(())
}

fn spec(
    key: &str,
    label: &str,
    kind: Kind,
    unit: Option<Unit>,
    semantic: Semantic,
    write: bool,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read: true, write },
        unit,
        semantic,
    }
}

fn describe(ctx: &DriverCtx, id: &DeviceId, heater: &Heater) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: heater.name.as_str().into(),
        manufacturer: Some("Voltalis".into()),
        model: None,
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            spec(
                "on",
                &moli_i18n::tr!("pilotes.voltalis.allume"),
                Kind::Binary,
                None,
                Semantic::OnOff,
                true,
            ),
            spec(
                "mode",
                &moli_i18n::tr!("pilotes.voltalis.mode"),
                Kind::Enum {
                    values: heater.modes.iter().map(|mode| (*mode).into()).collect(),
                },
                None,
                Semantic::Other,
                true,
            ),
            spec(
                "target_temperature",
                &moli_i18n::tr!("pilotes.voltalis.consigne"),
                Kind::Numeric {
                    min: Some(0.0),
                    max: Some(50.0),
                    step: None,
                },
                Some(Unit::Celsius),
                Semantic::Temperature,
                heater.modes.contains(&"temperature"),
            ),
            spec(
                "automatic",
                &moli_i18n::tr!("pilotes.voltalis.programme"),
                Kind::Binary,
                None,
                Semantic::Other,
                true,
            ),
            spec(
                "fault",
                &moli_i18n::tr!("pilotes.voltalis.probleme"),
                Kind::Binary,
                None,
                Semantic::Other,
                false,
            ),
        ],
    }
}

fn reading<T: Into<Value>>(value: Option<T>) -> Value {
    value.map_or(Value::Null, Into::into)
}

/// What this run remembers of a heater.
struct Known {
    id: DeviceId,
    /// As last listed: what an order is built from.
    last: Heater,
}

fn publish(
    ctx: &DriverCtx,
    known: &mut HashMap<u64, Known>,
    heaters: &[Heater],
    faulty: Option<&HashSet<u64>>,
) {
    for heater in heaters {
        let id = ctx.device_id(&heater.id.to_string());
        let changed = known
            .get(&heater.id)
            .is_none_or(|seen| seen.last.name != heater.name || seen.last.modes != heater.modes);
        if changed {
            ctx.upsert_device(describe(ctx, &id, heater));
        }
        known.insert(
            heater.id,
            Known {
                id: id.clone(),
                last: heater.clone(),
            },
        );
        ctx.set_state(&id, "on", reading(heater.on));
        ctx.set_state(&id, "mode", reading(heater.mode));
        ctx.set_state(&id, "target_temperature", reading(heater.target));
        ctx.set_state(&id, "automatic", reading(heater.automatic));
        ctx.set_state(
            &id,
            "fault",
            reading(faulty.map(|set| set.contains(&heater.id))),
        );
        ctx.set_availability(&id, true);
    }
    for (native, seen) in known.iter() {
        if !heaters.iter().any(|heater| heater.id == *native) {
            ctx.set_availability(&seen.id, false);
        }
    }
}

async fn park(ctx: &DriverCtx, known: &HashMap<u64, Known>, reason: String) -> anyhow::Result<()> {
    for seen in known.values() {
        ctx.set_availability(&seen.id, false);
    }
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

/// Reads the heaters and shows them; a refused session is closed for the next
/// tick to sign in again.
async fn refresh(
    ctx: &DriverCtx,
    session: &mut Option<Session>,
    known: &mut HashMap<u64, Known>,
    failures: &mut u32,
) {
    let Some(open) = session.as_ref() else {
        return;
    };
    match read_site(open).await {
        Ok((heaters, faulty)) => {
            *failures = 0;
            publish(ctx, known, &heaters, faulty.as_ref());
            ctx.ready();
        }
        Err(Failure::Refused) => *session = None,
        Err(Failure::Other(error)) => {
            *failures += 1;
            tracing::warn!(instance = %ctx.instance(), error = %error, "Voltalis");
        }
    }
}

/// One order, from the hub to the service. Whatever comes of it, the heaters are
/// read again: the service applies a setting at once and says so.
async fn command(
    ctx: &DriverCtx,
    credentials: (&str, &str),
    session: &mut Option<Session>,
    known: &mut HashMap<u64, Known>,
    failures: &mut u32,
    command: CommandRequest,
) -> Result<(), Failure> {
    let Some(heater) = known
        .values()
        .find(|seen| seen.id == command.device.id)
        .map(|seen| seen.last.clone())
    else {
        command.reply(Err(moli_i18n::tr!("pilotes.voltalis.radiateur_inconnu")));
        return Ok(());
    };
    if command.abandoned() {
        command.reply(Err(moli_i18n::tr!("pilotes.voltalis.trop_tard")));
        return Ok(());
    }
    let (username, password) = credentials;
    let outcome = tokio::time::timeout(
        ORDER_BUDGET,
        send_order(
            session,
            username,
            password,
            &heater,
            &command.key,
            &command.value,
        ),
    )
    .await
    .unwrap_or_else(|_| {
        Err(Failure::Other(anyhow::anyhow!(moli_i18n::tr!(
            "pilotes.voltalis.reseau"
        ))))
    });
    let refused = matches!(outcome, Err(Failure::Refused));
    match outcome {
        Ok(()) => command.reply(Ok(())),
        Err(Failure::Refused) => {
            command.reply(Err(moli_i18n::tr!("pilotes.voltalis.compte_refuse")));
        }
        Err(Failure::Other(error)) => command.reply(Err(error.to_string())),
    }
    // Only a sign-in says this: the account itself. Stop, as the tick would.
    if refused {
        return Err(Failure::Refused);
    }
    refresh(ctx, session, known, failures).await;
    Ok(())
}

async fn run(driver: &Voltalis, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    // Restored readings are stale until this run hears from the service.
    for id in ctx.known_devices() {
        ctx.set_availability(&id, false);
    }
    let mut known = HashMap::new();
    let (Some(username), Some(password)) = (ctx.secret(USERNAME), ctx.secret(PASSWORD)) else {
        return park(
            ctx,
            &known,
            moli_i18n::tr!("pilotes.voltalis.compte_manquant"),
        )
        .await;
    };
    let every = Duration::from_secs(driver.config.poll_seconds.clamp(60, 3600));
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut session: Option<Session> = None;
    let mut failures = 0u32;
    loop {
        tokio::select! {
            order = ctx.next_command() => {
                let Some(order) = order else { return Ok(()) };
                let credentials = (username.as_str(), password.as_str());
                let outcome = command(ctx, credentials, &mut session, &mut known, &mut failures, order).await;
                if outcome.is_err() {
                    return park(ctx, &known, moli_i18n::tr!("pilotes.voltalis.compte_refuse")).await;
                }
            }
            _ = tick.tick() => {
                if session.is_none() {
                    match login(username.as_str(), password.as_str()).await {
                        Ok(opened) => session = Some(opened),
                        // Never try a refused account again by itself.
                        Err(Failure::Refused) => {
                            return park(ctx, &known, moli_i18n::tr!("pilotes.voltalis.compte_refuse")).await;
                        }
                        Err(Failure::Other(error)) => {
                            failures += 1;
                            tracing::warn!(instance = %ctx.instance(), error = %error, "Voltalis");
                        }
                    }
                }
                refresh(ctx, &mut session, &mut known, &mut failures).await;
                if failures >= PATIENCE {
                    for seen in known.values() {
                        ctx.set_availability(&seen.id, false);
                    }
                }
            }
        }
    }
}
