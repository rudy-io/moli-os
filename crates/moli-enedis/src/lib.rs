//! A Linky meter's consumption, through the Conso API (`conso.boris.sh`), which
//! relays Enedis with the customer's consent. Read only, one day late: the meter
//! sends its measures once a day.
//!
//! The service is free and its quota shared: one request a day, at a time proper
//! to the meter, and one more in the afternoon if the day is still missing. Each
//! request covers the last days, so that a gap heals by itself. A refused token
//! stops the driver until a human renews the consent.
//!
//! When Enedis does not serve the hourly curve (404, « la sous-ressource
//! demandée n'existe pas »), the same request asks the daily totals instead,
//! over a month so that the gaps heal: each day's consumption is still read,
//! spread evenly over its hours for the energy page, without its peak. The
//! warning says why, in Enedis's own words.

mod model;

use std::time::Duration;

use http::header::{ACCEPT, AUTHORIZATION, HOST, HeaderValue, USER_AGENT};
use jiff::tz::TimeZone;
use jiff::{ToSpan, Zoned};
use model::{Curve, Total};
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
/// Days asked for the daily totals: one request, a month of gaps healed.
const DAILY_DAYS: i64 = 31;
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
    /// 404: Enedis has nothing of this kind for the meter, and why.
    Missing(Option<String>),
    Other(String),
}

/// What is asked: the hourly curve, or the daily totals when there is none.
#[derive(Clone, Copy)]
enum Ask {
    Curve,
    Daily,
}

/// The status, with Enedis's own reason when the answer gives one.
fn said(status: u16, body: &[u8]) -> String {
    match model::reason(body) {
        Some(motif) => moli_i18n::tr!(
            "pilotes.enedis.statut_motif",
            status = status,
            motif = &*motif
        ),
        None => moli_i18n::tr!("pilotes.enedis.statut", status = status),
    }
}

/// One HTTPS exchange with the Conso API: the network, or a test's fake.
trait Web: Send + Sync {
    fn send(
        &self,
        request: http::Request<http_body_util::Full<bytes::Bytes>>,
    ) -> BoxFuture<'_, anyhow::Result<(http::StatusCode, bytes::Bytes)>>;
}

/// The real network, checked against the public CAs.
struct Internet;

impl Web for Internet {
    fn send(
        &self,
        request: http::Request<http_body_util::Full<bytes::Bytes>>,
    ) -> BoxFuture<'_, anyhow::Result<(http::StatusCode, bytes::Bytes)>> {
        Box::pin(moli_net::web(
            API_HOST,
            443,
            true,
            request,
            TIMEOUT,
            MAX_RESPONSE,
        ))
    }
}

async fn fetch(
    web: &dyn Web,
    ask: Ask,
    prm: &str,
    bearer: &HeaderValue,
    now: &Zoned,
) -> Result<Vec<u8>, Failure> {
    let today = now.date();
    let days = match ask {
        Ask::Curve => DAYS,
        Ask::Daily => DAILY_DAYS,
    };
    let first = today
        .checked_sub(days.days())
        .map_err(|_| Failure::Other(moli_i18n::tr!("pilotes.enedis.reponse_illisible")))?;
    let kind = match ask {
        Ask::Curve => "courbe_de_charge_consommation",
        Ask::Daily => "consommation_quotidienne",
    };
    let path = format!("/api/{kind}?pointId={prm}&dateDebut={first}&dateFin={today}");
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
    let (status, body) = web
        .send(built)
        .await
        .map_err(|_| Failure::Other(moli_i18n::tr!("pilotes.enedis.reseau")))?;
    if matches!(status.as_u16(), 401 | 403) {
        return Err(Failure::Refused);
    }
    if status.as_u16() == 404 {
        return Err(Failure::Missing(model::reason(&body)));
    }
    if !status.is_success() {
        return Err(Failure::Other(said(status.as_u16(), &body)));
    }
    Ok(body.to_vec())
}

fn unreadable() -> Failure {
    Failure::Other(moli_i18n::tr!("pilotes.enedis.reponse_illisible"))
}

/// The hourly curve, or, when Enedis has none, the day's total.
enum Got {
    Curve(Curve),
    Daily(Vec<Total>, Option<String>),
}

async fn read(
    web: &dyn Web,
    prm: &str,
    bearer: &HeaderValue,
    now: &Zoned,
    tz: &TimeZone,
) -> Result<Got, Failure> {
    match fetch(web, Ask::Curve, prm, bearer, now).await {
        Ok(body) => model::curve(&body, tz)
            .map(Got::Curve)
            .map_err(|_| unreadable()),
        Err(Failure::Missing(why)) => {
            let body = fetch(web, Ask::Daily, prm, bearer, now)
                .await
                .map_err(|e| match e {
                    Failure::Missing(daily) => Failure::Other(match daily.or(why.clone()) {
                        Some(motif) => moli_i18n::tr!(
                            "pilotes.enedis.statut_motif",
                            status = 404,
                            motif = &*motif
                        ),
                        None => moli_i18n::tr!("pilotes.enedis.statut", status = 404),
                    }),
                    other => other,
                })?;
            let totals = model::totals(&body).map_err(|_| unreadable())?;
            Ok(Got::Daily(totals, why))
        }
        Err(e) => Err(e),
    }
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

/// The days' totals alone, for the energy page: each day whole (its start,
/// its end, its kWh), which the energy recorder spreads over its hours only
/// when it has none of them yet (a day a curve gave even in part is left as
/// it is: nothing counts twice). The last one is shown, without its peak,
/// unless a later day is already known.
fn publish_totals(ctx: &DriverCtx, id: &DeviceId, totals: &[Total], tz: &TimeZone) {
    let days: Vec<_> = totals
        .iter()
        .filter_map(|t| model::whole_day(t, tz))
        .collect();
    if !days.is_empty()
        && let Ok(text) = serde_json::to_string(&days)
    {
        ctx.set_state(id, "hours", Value::Text(text.into()));
    }
    if let Some(total) = totals.last()
        && last_day(ctx, id).is_none_or(|known| known <= total.date)
    {
        publish_total(ctx, id, total);
    }
}

/// The day's total alone: its hours and its peak are not known.
fn publish_total(ctx: &DriverCtx, id: &DeviceId, total: &Total) {
    ctx.set_state(id, "last_day_kwh", Value::Float(total.kwh));
    ctx.set_state(id, "last_day_peak", Value::Null);
    ctx.set_state(id, "last_day", Value::Text(total.date.to_string().into()));
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
            match read(&Internet, prm, &bearer, &now, &tz).await {
                Ok(Got::Daily(totals, why)) => {
                    publish_totals(ctx, &id, &totals, &tz);
                    let motif = why.unwrap_or_else(|| "404".to_owned());
                    ctx.warn(moli_i18n::tr!(
                        "pilotes.enedis.sans_courbe",
                        motif = &*motif
                    ));
                }
                Ok(Got::Curve(curve)) => {
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
                // `read` turns a missing curve into the daily totals, and a
                // missing daily answer into `Other`: never here.
                Err(Failure::Missing(_)) => {}
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

#[cfg(test)]
mod fallback {
    //! Without a curve: the 404, then the daily totals, in one reading.

    use std::sync::Mutex;

    use super::*;

    /// A Conso API that answers from a script, in order, and keeps the paths.
    struct Fake {
        replies: Mutex<Vec<(u16, &'static str)>>,
        asked: Mutex<Vec<String>>,
    }

    impl Web for Fake {
        fn send(
            &self,
            request: http::Request<http_body_util::Full<bytes::Bytes>>,
        ) -> BoxFuture<'_, anyhow::Result<(http::StatusCode, bytes::Bytes)>> {
            self.asked.lock().unwrap().push(request.uri().to_string());
            let (status, body) = self.replies.lock().unwrap().remove(0);
            Box::pin(async move {
                Ok((
                    http::StatusCode::from_u16(status)?,
                    bytes::Bytes::from(body),
                ))
            })
        }
    }

    const MISSING: &str = r#"{"status":404,"error":{"error":"Not Found","error_description":"La sous-ressource demandée n existe pas."}}"#;
    const DAILY: &str = r#"{"grandeur":[{"grandeurPhysique":"EA","unite":"Wh","points":[{"v":"9000","d":"2026-11-20"},{"v":"10625","d":"2026-11-22"}]}]}"#;

    fn ask(replies: Vec<(u16, &'static str)>) -> (Result<Got, Failure>, Vec<String>) {
        let fake = Fake {
            replies: Mutex::new(replies),
            asked: Mutex::new(Vec::new()),
        };
        let tz = TimeZone::get(ZONE).unwrap();
        let now: Zoned = "2026-11-23T08:00:00+01:00[Europe/Paris]".parse().unwrap();
        let bearer = HeaderValue::from_static("Bearer x");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let got = runtime.block_on(read(&fake, "00000000000000", &bearer, &now, &tz));
        (got, fake.asked.into_inner().unwrap())
    }

    #[test]
    fn a_missing_curve_brings_the_daily_totals_over_a_month() {
        let (got, asked) = ask(vec![(404, MISSING), (200, DAILY)]);
        let Ok(Got::Daily(totals, why)) = got else {
            panic!("expected the daily totals");
        };
        assert_eq!(
            totals
                .iter()
                .map(|t| t.date.to_string())
                .collect::<Vec<_>>(),
            ["2026-11-20", "2026-11-22"]
        );
        assert_eq!(
            why.as_deref(),
            Some("La sous-ressource demandée n existe pas")
        );
        assert!(
            asked[0].starts_with("/api/courbe_de_charge_consommation?"),
            "{asked:?}"
        );
        assert!(asked[0].contains("dateDebut=2026-11-16"), "{asked:?}");
        assert!(
            asked[1].starts_with("/api/consommation_quotidienne?"),
            "{asked:?}"
        );
        assert!(asked[1].contains("dateDebut=2026-10-23"), "{asked:?}");
        // What the energy recorder receives: each day whole, midnight to midnight.
        let tz = TimeZone::get(ZONE).unwrap();
        let days: Vec<_> = totals
            .iter()
            .filter_map(|t| model::whole_day(t, &tz))
            .collect();
        let text = serde_json::to_string(&days).unwrap();
        assert!(
            text.starts_with(r#"[{"day":"#) && text.contains(r#""kwh":10.625"#),
            "{text}"
        );
    }

    #[test]
    fn a_curve_served_asks_nothing_more() {
        let curve = r#"{"interval_reading":[]}"#;
        let (got, asked) = ask(vec![(200, curve)]);
        assert!(matches!(got, Ok(Got::Curve(_))) || got.is_err());
        assert_eq!(asked.len(), 1);
    }

    #[test]
    fn a_refused_token_stops_before_the_daily_totals() {
        let (got, asked) = ask(vec![(401, "{}")]);
        assert!(matches!(got, Err(Failure::Refused)));
        assert_eq!(asked.len(), 1);
    }

    #[test]
    fn no_daily_totals_either_says_why() {
        let (got, asked) = ask(vec![(404, MISSING), (404, MISSING)]);
        let Err(Failure::Other(text)) = got else {
            panic!("expected a status");
        };
        assert!(
            text.contains("404") && text.contains("n existe pas"),
            "{text}"
        );
        assert_eq!(asked.len(), 2);
    }
}
