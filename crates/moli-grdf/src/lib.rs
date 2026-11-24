//! A Gazpar gas meter's consumption, through the GRDF customer area
//! (`monespace.grdf.fr`), with the customer's own e-mail address and password.
//! Read only, a day late or more: the meter sends its index once a day and GRDF
//! publishes the day's reading during the next one.
//!
//! GRDF has no public API for its customers: Moli signs in as the customer
//! area's page does (see `session`). Twice a day at most, at times proper to
//! the meter; each request covers the last days, so that a gap heals by itself.
//! The very first one brings back a year, for the energy page. A refused
//! password stops the driver until a human fixes it: GRDF locks an account
//! that keeps failing.

mod model;
mod session;

use std::time::Duration;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{ToSpan, Zoned};
use model::Day;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use session::{Failure, Session, Web};

const EMAIL: &str = "email";
const PASSWORD: &str = "password";
/// The meters' time, whatever the home's.
const ZONE: &str = "Europe/Paris";
/// Days asked for each time.
const DAYS: i64 = 7;
/// Days asked the very first time: a year, for the energy page.
const FIRST_DAYS: i64 = 365;
/// Days in one value of the `hours` point (a text point holds 4000 characters).
const DAYS_AT_ONCE: usize = 3;
/// Between two values of `hours`: the energy recorder reads each one, and a
/// recorder that falls behind the hub's events only rereads the last one.
const PACE: Duration = Duration::from_millis(200);
/// The first year, sent once more after this: whatever the recorder missed
/// then is filed now (what is filed stays: nothing counts twice).
const REPLAY_AFTER: Duration = Duration::from_secs(600);
/// Days without news before the meter is shown as unreachable (GRDF is often
/// two days late, sometimes more).
const PATIENCE_DAYS: i32 = 5;
/// The clock can jump: never sleep long without looking at it again.
const LONGEST_SLEEP: Duration = Duration::from_secs(3600);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The meter's 14-digit number (PCE), on the bill or in the customer area.
    pub pce: String,
}

#[derive(Debug)]
pub struct Grdf {
    config: Config,
}

impl Grdf {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Grdf {
    fn kind(&self) -> &'static str {
        "grdf"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

fn valid_pce(pce: &str) -> bool {
    pce.len() == 14 && pce.bytes().all(|b| b.is_ascii_digit())
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
        native_name: moli_i18n::tr!("pilotes.grdf.compteur").into(),
        manufacturer: Some("GRDF".into()),
        model: Some("Gazpar".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            spec(
                "last_day",
                &moli_i18n::tr!("pilotes.grdf.dernier_jour"),
                Kind::Text,
                None,
            ),
            spec(
                "last_day_kwh",
                &moli_i18n::tr!("pilotes.grdf.energie_jour"),
                number(),
                Some(Unit::KiloWattHour),
            ),
            spec(
                "last_day_m3",
                &moli_i18n::tr!("pilotes.grdf.volume_jour"),
                number(),
                Some(Unit::Other("m³".into())),
            ),
            spec(
                "index",
                &moli_i18n::tr!("pilotes.grdf.index"),
                number(),
                Some(Unit::Other("m³".into())),
            ),
            spec(
                "factor",
                &moli_i18n::tr!("pilotes.grdf.coefficient"),
                number(),
                Some(Unit::Other("kWh/m³".into())),
            ),
            spec(
                "hours",
                &moli_i18n::tr!("pilotes.grdf.heures"),
                Kind::Text,
                None,
            ),
        ],
    }
}

/// What a failure says to the home, and whether the driver must stop.
fn said(failure: &Failure) -> (String, bool) {
    match failure {
        Failure::Refused => (moli_i18n::tr!("pilotes.grdf.identifiants_refuses"), true),
        Failure::MoreSteps => (moli_i18n::tr!("pilotes.grdf.autre_etape"), true),
        Failure::Network => (moli_i18n::tr!("pilotes.grdf.reseau"), false),
        Failure::Throttled => (moli_i18n::tr!("pilotes.grdf.limite"), false),
        Failure::SessionLost => (moli_i18n::tr!("pilotes.grdf.session_perdue"), false),
        Failure::Elsewhere(host) => (
            moli_i18n::tr!("pilotes.grdf.ailleurs", host = host.as_str()),
            false,
        ),
        Failure::Status(status) => (
            moli_i18n::tr!("pilotes.grdf.statut", status = *status),
            false,
        ),
        Failure::Unreadable => (moli_i18n::tr!("pilotes.grdf.reponse_illisible"), false),
    }
}

/// The first day to ask for: the last days, or back to the last day received
/// when the driver was stopped longer, or a year the very first time.
fn first_day(today: Date, done: Option<Date>) -> Option<Date> {
    let oldest = today.checked_sub(FIRST_DAYS.days()).ok()?;
    let recent = today.checked_sub(DAYS.days()).ok()?;
    Some(match done {
        None => oldest,
        Some(day) => day.min(recent).max(oldest),
    })
}

async fn fetch(
    web: &dyn Web,
    (email, password): (&str, &str),
    pce: &str,
    first: Date,
    last: Date,
) -> Result<Vec<Day>, Failure> {
    let mut session = Session::open(web, email, password).await?;
    let body = session.informative(pce, first, last).await?;
    model::days(&body, pce).map_err(|_| Failure::Unreadable)
}

/// The days, hour by hour, a few days at a time, for the energy recorder.
async fn publish_hours(ctx: &DriverCtx, id: &DeviceId, days: &[Day]) {
    for (i, chunk) in days.chunks(DAYS_AT_ONCE).enumerate() {
        if i > 0 {
            tokio::time::sleep(PACE).await;
        }
        let hours: Vec<_> = chunk.iter().flat_map(model::hours).collect();
        if let Ok(text) = serde_json::to_string(&hours) {
            ctx.set_state(id, "hours", Value::Text(text.into()));
        }
    }
}

async fn publish(ctx: &DriverCtx, id: &DeviceId, days: &[Day]) {
    publish_hours(ctx, id, days).await;
    let Some(day) = days.last() else { return };
    if last_day(ctx, id).is_some_and(|done| done > day.date) {
        return;
    }
    ctx.set_state(id, "last_day_kwh", Value::Float(day.kwh));
    if let Some(m3) = day.m3 {
        ctx.set_state(id, "last_day_m3", Value::Float(m3));
    }
    if let Some(index) = days.iter().rev().find_map(|d| d.index) {
        ctx.set_state(id, "index", Value::Float(index));
    }
    if let Some(factor) = day.factor {
        ctx.set_state(id, "factor", Value::Float(factor));
    }
    ctx.set_state(id, "last_day", Value::Text(day.date.to_string().into()));
}

fn last_day(ctx: &DriverCtx, id: &DeviceId) -> Option<Date> {
    match ctx.current(id, "last_day") {
        Some(Value::Text(day)) => day.parse().ok(),
        _ => None,
    }
}

fn fresh(done: Option<Date>, now: &Zoned) -> bool {
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

async fn run(driver: &Grdf, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let id = ctx.device_id("gazpar");
    ctx.upsert_device(describe(ctx, &id));
    let pce = driver.config.pce.as_str();
    if !valid_pce(pce) {
        return park(ctx, &id, moli_i18n::tr!("pilotes.grdf.pce_invalide")).await;
    }
    let (Some(email), Some(password)) = (ctx.secret(EMAIL), ctx.secret(PASSWORD)) else {
        return park(
            ctx,
            &id,
            moli_i18n::tr!("pilotes.grdf.identifiants_manquants"),
        )
        .await;
    };
    let tz = TimeZone::get(ZONE)?;
    let spread = model::spread(pce);
    ctx.ready();
    // First start: ask at once, the home wants to see it work. After that, only
    // at this meter's moments: a restart adds no sign-in.
    let mut due = last_day(ctx, &id).is_none();
    loop {
        let now = Zoned::now().with_time_zone(tz.clone());
        if due {
            let today = now.date();
            let done = last_day(ctx, &id);
            let backfill = done.is_none();
            let Some(first) = first_day(today, done) else {
                anyhow::bail!(moli_i18n::tr!("pilotes.grdf.reponse_illisible"));
            };
            match fetch(&session::Internet, (&email, &password), pce, first, today).await {
                Ok(days) => {
                    tracing::info!(
                        instance = %ctx.instance(),
                        days = days.len(),
                        last = %days.last().map(|d| d.date.to_string()).unwrap_or_default(),
                        "GRDF: readings received"
                    );
                    publish(ctx, &id, &days).await;
                    if backfill && !days.is_empty() {
                        // The year once more, from memory (no new request).
                        tokio::select! {
                            () = ctx.cancelled() => return Ok(()),
                            () = tokio::time::sleep(REPLAY_AFTER) => {}
                        }
                        publish_hours(ctx, &id, &days).await;
                    }
                    // A week without a reading is not a late meter: say so.
                    if days.is_empty() {
                        ctx.warn(moli_i18n::tr!("pilotes.grdf.pas_de_donnees"));
                    } else {
                        ctx.clear_warning();
                    }
                }
                Err(failure) => {
                    let (reason, stop) = said(&failure);
                    tracing::warn!(instance = %ctx.instance(), error = %reason, "GRDF");
                    if stop {
                        return park(ctx, &id, reason).await;
                    }
                    ctx.warn(reason);
                }
            }
        }
        let now = Zoned::now().with_time_zone(tz.clone());
        let done = last_day(ctx, &id);
        ctx.set_availability(&id, fresh(done, &now));
        let Some(next) = model::next_try(&now, done, spread) else {
            anyhow::bail!(moli_i18n::tr!("pilotes.grdf.reponse_illisible"));
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
    use jiff::civil::date;

    use super::*;

    #[test]
    fn a_meter_number_has_fourteen_digits() {
        assert!(valid_pce("00000000000000"));
        assert!(!valid_pce("0000000000000"));
        assert!(!valid_pce("0000000000000a"));
        assert!(!valid_pce("00000000000000&x=1"));
    }

    #[test]
    fn asks_a_year_first_then_the_last_days_or_back_to_the_gap() {
        let today = date(2026, 11, 23);
        assert_eq!(first_day(today, None), Some(date(2025, 11, 23)));
        assert_eq!(
            first_day(today, Some(date(2026, 11, 21))),
            Some(date(2026, 11, 16))
        );
        assert_eq!(
            first_day(today, Some(date(2026, 10, 1))),
            Some(date(2026, 10, 1))
        );
        assert_eq!(
            first_day(today, Some(date(2020, 1, 1))),
            Some(date(2025, 11, 23))
        );
    }

    #[test]
    fn only_a_refused_sign_in_stops_the_driver() {
        assert!(said(&Failure::Refused).1);
        assert!(said(&Failure::MoreSteps).1);
        assert!(!said(&Failure::Network).1);
        assert!(!said(&Failure::Throttled).1);
        assert!(!said(&Failure::Status(500)).1);
    }

    #[test]
    fn three_days_of_hours_fit_in_a_text_point() {
        let day = model::Day {
            date: date(2026, 10, 25),
            kwh: 123.456,
            m3: Some(10.9),
            index: Some(12_345.0),
            factor: Some(11.3),
            start: "2026-10-25T04:00:00Z".parse().unwrap(),
            end: "2026-10-26T05:00:00Z".parse().unwrap(),
        };
        let days = vec![day; DAYS_AT_ONCE];
        let hours: Vec<_> = days.iter().flat_map(model::hours).collect();
        let text = serde_json::to_string(&hours).unwrap();
        assert!(text.chars().count() < moli_core::MAX_TEXT, "{}", text.len());
    }

    #[tokio::test]
    async fn a_whole_exchange_gives_the_days() {
        use session::fake::{Fake, reply};
        let fixture = |name: &str| {
            std::fs::read_to_string(format!(
                "{}/../../integrations/grdf/fixtures/{name}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap()
        };
        let fake = Fake::default()
            .on(
                "monespace.grdf.fr/",
                reply(200, r#"{"stateToken":"02.id.x"}"#),
            )
            .on(
                "connexion.grdf.fr/idp/idx/identify",
                reply(200, &fixture("identify.json")),
            )
            .on(
                "connexion.grdf.fr/idp/idx/challenge/answer",
                reply(
                    200,
                    r#"{"success":{"href":"https://monespace.grdf.fr/done"}}"#,
                ),
            )
            .on("monespace.grdf.fr/done", reply(200, ""))
            .on(
                "monespace.grdf.fr/api/",
                reply(200, &fixture("informatives.json")).with("content-type", "application/json"),
            );
        let days = fetch(
            &fake,
            ("user@example.com", "pw"),
            "00000000000000",
            date(2000, 1, 1),
            date(2000, 1, 8),
        )
        .await
        .unwrap();
        assert_eq!(days.len(), 3);
    }
}
