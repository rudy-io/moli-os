//! Mitsubishi Electric air conditioners through the classic MELCloud web API:
//! power, mode, setpoint, fan and vanes, read and commanded, and the room's
//! temperature. The service answers from what each unit last reported, so one
//! request describes the whole account.
//!
//! An order reads the unit's state and sends it back with the change flagged.
//! The unit picks it up at its next contact with the service, and the list goes
//! on showing the old values meanwhile: what was ordered is kept for a while.
//!
//! The secret is the account's context key (`token`), never its password.

mod model;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use http::header::{ACCEPT, COOKIE, HOST, HeaderValue, USER_AGENT};
use http_body_util::Full;
use model::AirUnit;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;

const API_HOST: &str = "app.melcloud.com";
const LIST_DEVICES: &str = "/Mitsubishi.Wifi.Client/User/ListDevices";
const GET_STATE: &str = "/Mitsubishi.Wifi.Client/Device/Get";
const SET_STATE: &str = "/Mitsubishi.Wifi.Client/Device/SetAta";
const TOKEN: &str = "token";
const MAX_RESPONSE: usize = 4 << 20;
const TIMEOUT: Duration = Duration::from_secs(20);
/// The service says « offline » for one answer and not the next: only a run of
/// them means a unit is gone. Same for a service that stops answering.
const PATIENCE: u32 = 3;
/// How long the list may lag behind an order accepted by the service.
const SETTLING: Duration = Duration::from_secs(150);
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
pub struct Melcloud {
    config: Config,
}

impl Melcloud {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Melcloud {
    fn kind(&self) -> &'static str {
        "melcloud"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

/// What a request can end in, beyond an answer.
enum Failure {
    /// The context key was refused: only a person can give another one.
    Refused,
    Other(anyhow::Error),
}

fn request(method: http::Method, path: &str, token: &HeaderValue) -> http::request::Builder {
    http::Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, API_HOST)
        .header(USER_AGENT, "moli-os")
        .header(ACCEPT, "application/json")
        .header(COOKIE, "policyaccepted=true")
        .header("X-Requested-With", "XMLHttpRequest")
        .header("X-MitsContextKey", token.clone())
}

async fn exchange(
    built: anyhow::Result<http::Request<Full<moli_net::Body>>>,
) -> Result<Vec<u8>, Failure> {
    let built = built.map_err(Failure::Other)?;
    // Underlying errors are dropped on purpose: no request detail in a status.
    let (status, body) = moli_net::web(API_HOST, 443, true, built, TIMEOUT, MAX_RESPONSE)
        .await
        .map_err(|_| Failure::Other(anyhow::anyhow!(moli_i18n::tr!("pilotes.melcloud.reseau"))))?;
    if matches!(status.as_u16(), 401 | 403) {
        return Err(Failure::Refused);
    }
    if !status.is_success() {
        return Err(Failure::Other(anyhow::anyhow!(moli_i18n::tr!(
            "pilotes.melcloud.statut",
            status = status.as_u16()
        ))));
    }
    Ok(body.to_vec())
}

async fn list_devices(token: &HeaderValue) -> Result<Vec<AirUnit>, Failure> {
    let builder = request(http::Method::GET, LIST_DEVICES, token);
    let body = exchange(moli_net::empty(builder)).await?;
    model::units(&body).map_err(Failure::Other)
}

async fn send_order(
    token: &HeaderValue,
    unit: &AirUnit,
    order: &model::Order,
) -> Result<(), Failure> {
    let path = format!("{GET_STATE}?id={}&buildingID={}", unit.id, unit.building);
    let builder = request(http::Method::GET, &path, token);
    let state = exchange(moli_net::empty(builder)).await?;
    let body = model::with_order(&state, order).map_err(Failure::Other)?;
    let builder = request(http::Method::POST, SET_STATE, token);
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

fn choices<S: AsRef<str>>(values: impl IntoIterator<Item = S>) -> Kind {
    Kind::Enum {
        values: values.into_iter().map(|v| v.as_ref().into()).collect(),
    }
}

fn describe(ctx: &DriverCtx, id: &DeviceId, unit: &AirUnit) -> Device {
    let free = !unit.locks.guest;
    let mut points = vec![
        spec(
            "on",
            &moli_i18n::tr!("pilotes.melcloud.allumee"),
            Kind::Binary,
            None,
            Semantic::OnOff,
            free && !unit.locks.power,
        ),
        spec(
            "mode",
            &moli_i18n::tr!("pilotes.melcloud.mode"),
            choices(&unit.modes),
            None,
            Semantic::Other,
            free && !unit.locks.mode,
        ),
        spec(
            "temperature",
            &moli_i18n::tr!("pilotes.melcloud.temperature"),
            Kind::Numeric {
                min: Some(-100.0),
                max: Some(100.0),
                step: None,
            },
            Some(Unit::Celsius),
            Semantic::Temperature,
            false,
        ),
    ];
    if let Some((min, max)) = unit.limits {
        points.push(spec(
            "target_temperature",
            &moli_i18n::tr!("pilotes.melcloud.consigne"),
            Kind::Numeric {
                min: Some(min),
                max: Some(max),
                step: unit.step,
            },
            Some(Unit::Celsius),
            Semantic::Temperature,
            free && !unit.locks.target,
        ));
    }
    if !unit.fan_speeds.is_empty() {
        points.push(spec(
            "fan_speed",
            &moli_i18n::tr!("pilotes.melcloud.ventilateur"),
            choices(&unit.fan_speeds),
            None,
            Semantic::Other,
            free,
        ));
    }
    if unit.has_vertical_vane {
        points.push(spec(
            "vertical_vane",
            &moli_i18n::tr!("pilotes.melcloud.ailette_verticale"),
            choices(model::VERTICAL_VANE.iter().map(|(name, _)| *name)),
            None,
            Semantic::Other,
            free,
        ));
    }
    if unit.has_horizontal_vane {
        points.push(spec(
            "horizontal_vane",
            &moli_i18n::tr!("pilotes.melcloud.ailette_horizontale"),
            choices(model::HORIZONTAL_VANE.iter().map(|(name, _)| *name)),
            None,
            Semantic::Other,
            free,
        ));
    }
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: unit.name.as_str().into(),
        manufacturer: Some("Mitsubishi Electric".into()),
        model: None,
        description: None,
        native_room: unit.room.as_deref().map(Into::into),
        members: Vec::new(),
        points,
    }
}

fn reading<T: Into<Value>>(value: Option<T>) -> Value {
    value.map_or(Value::Null, Into::into)
}

/// What this run remembers of a unit.
struct Seen {
    id: DeviceId,
    /// What its description was made from: it is published again only if this changes.
    shape: AirUnit,
    offline_answers: u32,
    /// The unit as last listed: what an order is checked against.
    last: AirUnit,
    /// Until when accepted orders win over the list, and those orders.
    settling: Option<Instant>,
    accepted: Vec<(String, Value)>,
}

/// The fields a description depends on, without the readings.
fn shape(unit: &AirUnit) -> AirUnit {
    AirUnit {
        power: None,
        mode: None,
        temperature: None,
        target: None,
        offline: false,
        fan: None,
        vertical_vane: None,
        horizontal_vane: None,
        ..unit.clone()
    }
}

fn publish(ctx: &DriverCtx, seen: &mut HashMap<u64, Seen>, units: &[AirUnit]) {
    for unit in units {
        let id = ctx.device_id(&unit.id.to_string());
        let wanted = shape(unit);
        if seen.get(&unit.id).is_none_or(|known| known.shape != wanted) {
            ctx.upsert_device(describe(ctx, &id, unit));
        }
        let known = seen.entry(unit.id).or_insert_with(|| Seen {
            id: id.clone(),
            shape: wanted.clone(),
            offline_answers: 0,
            last: unit.clone(),
            settling: None,
            accepted: Vec::new(),
        });
        known.shape = wanted;
        known.last = unit.clone();
        known.offline_answers = if unit.offline {
            known.offline_answers + 1
        } else {
            0
        };
        ctx.set_state(&id, "temperature", reading(unit.temperature));
        known.settling = known.settling.filter(|until| Instant::now() < *until);
        if known.settling.is_some() {
            for (key, value) in &known.accepted {
                model::apply(&mut known.last, key, value);
            }
        } else {
            known.accepted.clear();
            ctx.set_state(&id, "on", reading(unit.power));
            ctx.set_state(&id, "mode", reading(unit.mode));
            if unit.limits.is_some() {
                ctx.set_state(&id, "target_temperature", reading(unit.target));
            }
            if !unit.fan_speeds.is_empty() {
                ctx.set_state(&id, "fan_speed", reading(unit.fan.as_deref()));
            }
            if unit.has_vertical_vane {
                ctx.set_state(&id, "vertical_vane", reading(unit.vertical_vane));
            }
            if unit.has_horizontal_vane {
                ctx.set_state(&id, "horizontal_vane", reading(unit.horizontal_vane));
            }
        }
        ctx.set_availability(&id, known.offline_answers < PATIENCE);
    }
    for (native, known) in seen.iter() {
        if !units.iter().any(|unit| unit.id == *native) {
            ctx.set_availability(&known.id, false);
        }
    }
}

/// One order, from the hub to the service. A refusal here is only about this
/// order (a guest, a setting locked in the app): the next poll says if the key
/// itself is gone.
async fn command(
    ctx: &DriverCtx,
    token: &HeaderValue,
    seen: &mut HashMap<u64, Seen>,
    command: CommandRequest,
) {
    let target = seen
        .values_mut()
        .find(|known| known.id == command.device.id);
    let Some(known) = target else {
        command.reply(Err(moli_i18n::tr!("pilotes.melcloud.unite_inconnue")));
        return;
    };
    let order = match model::order(&known.last, &command.key, &command.value) {
        Ok(order) => order,
        Err(message) => {
            command.reply(Err(message));
            return;
        }
    };
    if command.abandoned() {
        command.reply(Err(moli_i18n::tr!("pilotes.melcloud.trop_tard")));
        return;
    }
    let outcome = tokio::time::timeout(ORDER_BUDGET, send_order(token, &known.last, &order)).await;
    match outcome.unwrap_or_else(|_| {
        Err(Failure::Other(anyhow::anyhow!(moli_i18n::tr!(
            "pilotes.melcloud.reseau"
        ))))
    }) {
        Ok(()) => {
            if known.settling.is_none_or(|until| Instant::now() >= until) {
                known.accepted.clear();
            }
            known.settling = Some(Instant::now() + SETTLING);
            model::apply(&mut known.last, &command.key, &command.value);
            known
                .accepted
                .push((command.key.to_string(), command.value.clone()));
            ctx.set_state(&known.id, &command.key, command.value.clone());
            command.reply(Ok(()));
        }
        Err(Failure::Refused) => {
            command.reply(Err(moli_i18n::tr!("pilotes.melcloud.ordre_refuse")));
        }
        Err(Failure::Other(error)) => command.reply(Err(error.to_string())),
    }
}

async fn park(ctx: &DriverCtx, seen: &HashMap<u64, Seen>, reason: String) -> anyhow::Result<()> {
    for known in seen.values() {
        ctx.set_availability(&known.id, false);
    }
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

async fn run(driver: &Melcloud, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    // Restored readings are stale until this run hears from the service.
    for id in ctx.known_devices() {
        ctx.set_availability(&id, false);
    }
    let mut seen = HashMap::new();
    let Some(secret) = ctx.secret(TOKEN) else {
        return park(
            ctx,
            &seen,
            moli_i18n::tr!("pilotes.melcloud.jeton_manquant"),
        )
        .await;
    };
    let mut token = HeaderValue::from_str(secret.as_str())
        .ok()
        .context(moli_i18n::tr!("pilotes.melcloud.jeton_invalide"))?;
    token.set_sensitive(true);
    let every = Duration::from_secs(driver.config.poll_seconds.clamp(60, 3600));
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut failures = 0u32;
    loop {
        tokio::select! {
            order = ctx.next_command() => {
                let Some(order) = order else { return Ok(()) };
                command(ctx, &token, &mut seen, order).await;
            }
            _ = tick.tick() => match list_devices(&token).await {
                Ok(units) => {
                    failures = 0;
                    publish(ctx, &mut seen, &units);
                    ctx.ready();
                }
                Err(Failure::Refused) => {
                    return park(ctx, &seen, moli_i18n::tr!("pilotes.melcloud.jeton_refuse")).await;
                }
                Err(Failure::Other(error)) => {
                    failures += 1;
                    tracing::warn!(instance = %ctx.instance(), error = %error, "MELCloud");
                    if failures >= PATIENCE {
                        for known in seen.values() {
                            ctx.set_availability(&known.id, false);
                        }
                    }
                }
            },
        }
    }
}
