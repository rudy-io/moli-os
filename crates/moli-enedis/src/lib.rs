//! A Linky meter's consumption, through the Conso API (`conso.boris.sh`), which
//! relays Enedis with the customer's consent. Read only, one day late: the meter
//! sends its measures once a day.
//!
//! The service is free and its quota shared: one request a day, at a time proper
//! to the meter, and one more in the afternoon if the day is still missing. Each
//! request covers the last days, so that a gap heals by itself. A refused token
//! stops the driver until a human renews the consent.

mod model;

use std::time::Duration;

use http::header::{ACCEPT, AUTHORIZATION, HOST, HeaderValue, USER_AGENT};
use jiff::tz::TimeZone;
use jiff::{ToSpan, Zoned};
use model::Curve;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;

const API_HOST: &str = "conso.boris.sh";
const TOKEN: &str = "token";
const MAX_RESPONSE: usize = 1 << 20;
const TIMEOUT: Duration = Duration::from_secs(30);
/// The meters' time, whatever the home's.
const ZONE: &str = "Europe/Paris";
/// Days asked for each time.
const DAYS: i64 = 7;
/// Hours in one value of the `hours` point.
const HOURS_AT_ONCE: usize = 24;
/// Days without news before the meter is shown as unreachable.
const PATIENCE_DAYS: i32 = 3;
/// The clock can jump: never sleep long without looking at it again.
const LONGEST_SLEEP: Duration = Duration::from_secs(3600);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The meter's 14-digit number (PRM).
    pub prm: String,
}

#[derive(Debug)]
pub struct Enedis {
    config: Config,
}

impl Enedis {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Enedis {
    fn kind(&self) -> &'static str {
        "enedis"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

fn valid_prm(prm: &str) -> bool {
    prm.len() == 14 && prm.bytes().all(|b| b.is_ascii_digit())
}

fn spec(key: &str, label: &str, kind: Kind, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic: Semantic::Other,
    }
}

fn number() -> Kind {
    Kind::Numeric {
        min: Some(0.0),
        max: None,
        step: None,
    }
}

fn describe(ctx: &DriverCtx, id: &DeviceId) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.enedis.compteur").into(),
        manufacturer: Some("Enedis".into()),
        model: Some("Linky".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            spec(
                "last_day",
                &moli_i18n::tr!("pilotes.enedis.dernier_jour"),
                Kind::Text,
                None,
            ),
            spec(
                "last_day_kwh",
                &moli_i18n::tr!("pilotes.enedis.conso_jour"),
                number(),
                Some(Unit::KiloWattHour),
            ),
            spec(
                "last_day_peak",
                &moli_i18n::tr!("pilotes.enedis.pic_jour"),
                number(),
                Some(Unit::Watt),
            ),
            spec(
                "hours",
                &moli_i18n::tr!("pilotes.enedis.heures"),
                Kind::Text,
                None,
            ),
        ],
    }
}

/// What a request can end in, beyond an answer.
enum Failure {
    /// 401 or 403: the token ended, or does not cover this meter.
    Refused,
    Other(String),
}

async fn fetch(
    prm: &str,
    bearer: &HeaderValue,
    now: &Zoned,
    tz: &TimeZone,
) -> Result<Curve, Failure> {
    let today = now.date();
    let first = today
        .checked_sub(DAYS.days())
        .map_err(|_| Failure::Other(moli_i18n::tr!("pilotes.enedis.reponse_illisible")))?;
    let path = format!(
        "/api/courbe_de_charge_consommation?pointId={prm}&dateDebut={first}&dateFin={today}"
    );
    let builder = http::Request::builder()
        .method(http::Method::GET)
        .uri(path)
        .header(HOST, API_HOST)
        // What the service asks of a public project: who is calling.
        .header(USER_AGENT, "github.com/rudy-io/moli-os")
        .header(ACCEPT, "application/json")
        .header(AUTHORIZATION, bearer.clone());
    let built = moli_net::empty(builder)
        .map_err(|_| Failure::Other(moli_i18n::tr!("pilotes.enedis.reponse_illisible")))?;
    // Underlying errors are dropped on purpose: no request detail in a status.
    let (status, body) = moli_net::web(API_HOST, 443, true, built, TIMEOUT, MAX_RESPONSE)
        .await
        .map_err(|_| Failure::Other(moli_i18n::tr!("pilotes.enedis.reseau")))?;
    if matches!(status.as_u16(), 401 | 403) {
        return Err(Failure::Refused);
    }
    if !status.is_success() {
        return Err(Failure::Other(moli_i18n::tr!(
            "pilotes.enedis.statut",
            status = status.as_u16()
        )));
    }
    model::curve(&body, tz)
        .map_err(|_| Failure::Other(moli_i18n::tr!("pilotes.enedis.reponse_illisible")))
}

fn publish(ctx: &DriverCtx, id: &DeviceId, curve: &Curve) {
    // A text point holds a few kB: the days go one after the other.
    for day in curve.hours.chunks(HOURS_AT_ONCE) {
        if let Ok(hours) = serde_json::to_string(day) {
            ctx.set_state(id, "hours", Value::Text(hours.into()));
        }
    }
    if let Some(day) = &curve.last_day {
        ctx.set_state(id, "last_day_kwh", Value::Float(day.kwh));
        ctx.set_state(id, "last_day_peak", Value::Float(day.peak_w));
        ctx.set_state(id, "last_day", Value::Text(day.date.to_string().into()));
    }
}

fn last_day(ctx: &DriverCtx, id: &DeviceId) -> Option<jiff::civil::Date> {
    match ctx.current(id, "last_day") {
        Some(Value::Text(day)) => day.parse().ok(),
        _ => None,
    }
}

fn fresh(done: Option<jiff::civil::Date>, now: &Zoned) -> bool {
    done.is_some_and(|day| {
        now.date()
            .checked_sub(PATIENCE_DAYS.days())
            .is_ok_and(|oldest| day >= oldest)
    })
}

async fn park(ctx: &DriverCtx, id: &DeviceId, reason: String) -> anyhow::Result<()> {
    ctx.set_availability(id, false);
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

async fn run(driver: &Enedis, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let id = ctx.device_id("linky");
    ctx.upsert_device(describe(ctx, &id));
    let prm = driver.config.prm.as_str();
    if !valid_prm(prm) {
        return park(ctx, &id, moli_i18n::tr!("pilotes.enedis.prm_invalide")).await;
    }
    let Some(token) = ctx.secret(TOKEN) else {
        return park(ctx, &id, moli_i18n::tr!("pilotes.enedis.jeton_manquant")).await;
    };
    let Ok(mut bearer) = HeaderValue::from_str(&format!("Bearer {token}")) else {
        return park(ctx, &id, moli_i18n::tr!("pilotes.enedis.jeton_refuse")).await;
    };
    bearer.set_sensitive(true);
    let tz = TimeZone::get(ZONE)?;
    let spread = model::spread(prm);
    ctx.ready();
    // First start: ask at once, the home wants to see it work. After that, only
    // at this meter's moments: a restart adds no request.
    let mut due = last_day(ctx, &id).is_none();
    loop {
        let now = Zoned::now().with_time_zone(tz.clone());
        if due {
            match fetch(prm, &bearer, &now, &tz).await {
                Ok(curve) => {
                    publish(ctx, &id, &curve);
                    // A week without a measure is not a late meter: say why.
                    if curve.hours.is_empty() {
                        ctx.warn(moli_i18n::tr!("pilotes.enedis.pas_de_donnees"));
                    } else {
                        ctx.clear_warning();
                    }
                }
                Err(Failure::Refused) => {
                    return park(ctx, &id, moli_i18n::tr!("pilotes.enedis.jeton_refuse")).await;
                }
                Err(Failure::Other(error)) => {
                    tracing::warn!(instance = %ctx.instance(), error = %error, "Enedis");
                    ctx.warn(error);
                }
            }
        }
        let now = Zoned::now().with_time_zone(tz.clone());
        let done = last_day(ctx, &id);
        ctx.set_availability(&id, fresh(done, &now));
        let Some(next) = model::next_try(&now, done, spread) else {
            anyhow::bail!(moli_i18n::tr!("pilotes.enedis.reponse_illisible"));
        };
        let wait = (next.timestamp() - now.timestamp())
            .total(jiff::Unit::Second)
            .ok()
            .and_then(|s| Duration::try_from_secs_f64(s).ok())
            .unwrap_or(Duration::ZERO);
        due = wait <= LONGEST_SLEEP;
        tokio::select! {
            () = ctx.cancelled() => return Ok(()),
            () = tokio::time::sleep(wait.min(LONGEST_SLEEP)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meter_number_has_fourteen_digits() {
        assert!(valid_prm("00000000000000"));
        assert!(!valid_prm("0000000000000"));
        assert!(!valid_prm("0000000000000a"));
        assert!(!valid_prm("00000000000000&x=1"));
    }
}
