//! What kind of day it is: weekend, French public holiday, school holiday.
//!
//! One device, « Calendrier », for automations that do not run the same on
//! a quiet day (announcements later in the morning, another heating
//! programme…):
//!
//! - `weekend`, `holiday` (and `holiday_name`), `workday` (neither);
//! - with a `school_zone`: `school_holidays` (and `school_holidays_name`);
//! - `quiet_day`: any of them.
//!
//! The public holidays are worked out here, with no network (see
//! [`holidays`]). The school holidays come from the education ministry's
//! open data, read once a day: only the zone leaves the house.
//!
//! The days change at midnight in `timezone`: the points are worked out
//! again every minute.

pub mod holidays;
pub mod school;

use std::time::Duration;

use anyhow::Context as _;
use http::header::{ACCEPT, HOST, USER_AGENT};
use jiff::Timestamp;
use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::Value as Json;

const HOST_NAME: &str = "data.education.gouv.fr";
const DATASET: &str = "/api/explore/v2.1/catalog/datasets/fr-en-calendrier-scolaire/records";
const LIMIT: Duration = Duration::from_secs(20);
const MAX_BODY: usize = 256 * 1024;
const READ_EVERY: Duration = Duration::from_secs(24 * 3600);
const RETRY: Duration = Duration::from_secs(3600);
const TICK: Duration = Duration::from_secs(60);
/// How far ahead the school calendar is read (it is read again every day).
const AHEAD_DAYS: i64 = 400;

fn default_timezone() -> String {
    "Europe/Paris".into()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// The school zone: `A`, `B`, `C`, `Corse`, `Réunion`… Without it, no
    /// school holidays (and nothing leaves the house).
    #[serde(default)]
    pub school_zone: Option<String>,
    /// Alsace and Moselle: Good Friday and St Stephen's Day are holidays too.
    #[serde(default)]
    pub alsace_moselle: bool,
}

#[derive(Debug)]
pub struct Calendar {
    config: Config,
    zone: Option<&'static str>,
}

impl Calendar {
    /// # Errors
    /// An unknown time zone or school zone.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        TimeZone::get(&config.timezone)
            .map_err(|e| anyhow::anyhow!("timezone {:?}: {e}", config.timezone))?;
        let zone = match &config.school_zone {
            None => None,
            Some(given) => Some(school::zone(given).with_context(|| {
                format!(
                    "school_zone {given:?}: A, B, C or one of {}",
                    school::ZONES[3..].join(", ")
                )
            })?),
        };
        Ok(Self { config, zone })
    }
}

impl Driver for Calendar {
    fn kind(&self) -> &'static str {
        "calendar"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

/// What `day` is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Day {
    pub weekend: bool,
    /// The holiday's translation key.
    pub holiday: Option<&'static str>,
    /// The school holiday's name; `None` when it is not one, or not known.
    pub school_holidays: Option<String>,
}

impl Day {
    #[must_use]
    pub fn of(day: Date, alsace_moselle: bool, periods: Option<&[school::Period]>) -> Self {
        Self {
            weekend: matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday),
            holiday: holidays::on(day, alsace_moselle).map(|h| h.key),
            school_holidays: periods
                .and_then(|p| school::on(p, day))
                .map(|p| p.name.clone()),
        }
    }

    #[must_use]
    pub fn workday(&self) -> bool {
        !self.weekend && self.holiday.is_none()
    }

    #[must_use]
    pub fn quiet(&self) -> bool {
        !self.workday() || self.school_holidays.is_some()
    }
}

/// The ministry's query: the zone's periods that are not over, up to
/// [`AHEAD_DAYS`] ahead, each once (the answer gives one per académie
/// otherwise).
fn query(zone: &str, today: Date) -> String {
    let since = today
        .checked_sub(jiff::Span::new().days(7))
        .unwrap_or(today);
    let until = today
        .checked_add(jiff::Span::new().days(AHEAD_DAYS))
        .unwrap_or(today);
    let fields = "description,population,start_date,end_date";
    let filter = format!("zones=\"{zone}\" and end_date>=\"{since}\" and start_date<=\"{until}\"");
    format!(
        "{DATASET}?select={f}&group_by={f}&order_by=start_date&limit=100&where={w}",
        f = percent(fields),
        w = percent(&filter)
    )
}

fn percent(text: &str) -> String {
    text.bytes().fold(String::new(), |mut s, b| {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            s.push(char::from(b));
        } else {
            let _ = std::fmt::Write::write_fmt(&mut s, format_args!("%{b:02X}"));
        }
        s
    })
}

async fn read_school(
    zone: &str,
    today: Date,
    tz: &TimeZone,
) -> anyhow::Result<Vec<school::Period>> {
    let request = http::Request::builder()
        .method(http::Method::GET)
        .uri(query(zone, today))
        .header(HOST, HOST_NAME)
        .header(USER_AGENT, "moli-os")
        .header(ACCEPT, "application/json")
        .body(http_body_util::Full::new(moli_net::Body::new()))?;
    let (status, body) = moli_net::web(HOST_NAME, 443, true, request, LIMIT, MAX_BODY).await?;
    anyhow::ensure!(status.is_success(), "{HOST_NAME}: HTTP {}", status.as_u16());
    let answer: Json = serde_json::from_slice(&body)?;
    anyhow::ensure!(
        answer.get("results").is_some_and(Json::is_array),
        "{HOST_NAME}: no results in the answer"
    );
    Ok(school::periods(&answer, tz))
}

fn point(key: &str, label: String, kind: Kind) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        semantic: Semantic::Other,
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit: None,
    }
}

fn device(ctx: &DriverCtx, id: &DeviceId, school: bool) -> Device {
    let tr = |key: &str| moli_i18n::tr!(&format!("pilotes.calendar.{key}"));
    let mut points = vec![
        point("weekend", tr("week_end"), Kind::Binary),
        point("holiday", tr("ferie"), Kind::Binary),
        point("holiday_name", tr("ferie_nom"), Kind::Text),
        point("workday", tr("jour_ouvre"), Kind::Binary),
    ];
    if school {
        points.push(point("school_holidays", tr("vacances"), Kind::Binary));
        points.push(point(
            "school_holidays_name",
            tr("vacances_nom"),
            Kind::Text,
        ));
    }
    points.push(point(
        "quiet_day",
        if school {
            tr("jour_calme")
        } else {
            tr("jour_calme_sans_vacances")
        },
        Kind::Binary,
    ));
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: tr("nom").into(),
        manufacturer: None,
        model: Some("Calendrier".into()),
        description: Some(tr("description").into()),
        native_room: None,
        members: Vec::new(),
        points,
    }
}

fn publish(ctx: &DriverCtx, id: &DeviceId, day: &Day, school_known: Option<bool>) {
    ctx.set_state(id, "weekend", Value::Bool(day.weekend));
    ctx.set_state(id, "holiday", Value::Bool(day.holiday.is_some()));
    ctx.set_state(
        id,
        "holiday_name",
        Value::Text(
            day.holiday
                .map(|k| moli_i18n::tr!(&format!("pilotes.calendar.f.{k}")))
                .unwrap_or_default()
                .into(),
        ),
    );
    ctx.set_state(id, "workday", Value::Bool(day.workday()));
    match school_known {
        // No school zone: the points do not exist.
        None => ctx.set_state(id, "quiet_day", Value::Bool(day.quiet())),
        // The school calendar could not be read yet: unknown, unless the day
        // is quiet anyway.
        Some(false) => {
            ctx.set_state(id, "school_holidays", Value::Null);
            ctx.set_state(id, "school_holidays_name", Value::Null);
            ctx.set_state(
                id,
                "quiet_day",
                if day.workday() {
                    Value::Null
                } else {
                    Value::Bool(true)
                },
            );
        }
        Some(true) => {
            ctx.set_state(
                id,
                "school_holidays",
                Value::Bool(day.school_holidays.is_some()),
            );
            ctx.set_state(
                id,
                "school_holidays_name",
                Value::Text(day.school_holidays.clone().unwrap_or_default().into()),
            );
            ctx.set_state(id, "quiet_day", Value::Bool(day.quiet()));
        }
    }
}

async fn run(calendar: &Calendar, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let config = &calendar.config;
    let tz = TimeZone::get(&config.timezone)?;
    let today = || Timestamp::now().to_zoned(tz.clone()).date();
    let id = ctx.device_id("calendar");
    ctx.upsert_device(device(ctx, &id, calendar.zone.is_some()));
    ctx.set_availability(&id, true);
    ctx.ready();
    let mut periods: Option<Vec<school::Period>> = None;
    let mut read_at: Option<tokio::time::Instant> = None;
    let mut tried_at: Option<tokio::time::Instant> = None;
    let mut warned = false;
    loop {
        if let Some(zone) = calendar.zone {
            let due = read_at.is_none_or(|at| at.elapsed() >= READ_EVERY)
                && tried_at.is_none_or(|at| at.elapsed() >= RETRY);
            if due {
                tried_at = Some(tokio::time::Instant::now());
                match read_school(zone, today(), &tz).await {
                    Ok(found) => {
                        tracing::info!(instance = %ctx.instance(), zone, periods = found.len(), "calendar: school holidays read");
                        periods = Some(found);
                        read_at = tried_at;
                        if warned {
                            warned = false;
                            ctx.clear_warning();
                        }
                    }
                    Err(e) => {
                        tracing::warn!(instance = %ctx.instance(), error = %format!("{e:#}"), "calendar: school holidays not read");
                        // The periods read before stay good: the ministry's
                        // calendar is written years ahead. Without any, say so.
                        if periods.is_none() && !warned {
                            warned = true;
                            ctx.warn(moli_i18n::tr!("pilotes.calendar.vacances_illisibles"));
                        }
                    }
                }
            }
        }
        let day = Day::of(today(), config.alsace_moselle, periods.as_deref());
        publish(ctx, &id, &day, calendar.zone.map(|_| periods.is_some()));
        tokio::select! {
            () = tokio::time::sleep(TICK) => {}
            command = ctx.next_command() => {
                if let Some(command) = command {
                    command.reply(Err(moli_i18n::tr!("pilotes.calendar.lecture_seule")));
                } else {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn config(zone: Option<&str>) -> Config {
        Config {
            timezone: "Europe/Paris".into(),
            school_zone: zone.map(Into::into),
            alsace_moselle: false,
        }
    }

    #[test]
    fn the_options_are_checked() {
        assert!(Calendar::new(config(None)).is_ok());
        assert_eq!(
            Calendar::new(config(Some("a"))).unwrap().zone,
            Some("Zone A")
        );
        assert!(Calendar::new(config(Some("Zone D"))).is_err());
        let mut c = config(None);
        c.timezone = "Europe/Nulle-Part".into();
        assert!(Calendar::new(c).is_err());
    }

    #[test]
    fn a_day_says_what_it_is() {
        let periods = [school::Period {
            from: date(2026, 12, 19),
            until: date(2027, 1, 4),
            name: "Vacances de Noël".into(),
        }];
        // A Friday at school.
        let friday = Day::of(date(2026, 12, 18), false, Some(&periods));
        assert!(friday.workday() && !friday.quiet());
        // A Saturday, first day of the holidays.
        let saturday = Day::of(date(2026, 12, 19), false, Some(&periods));
        assert!(saturday.weekend && saturday.quiet());
        assert_eq!(
            saturday.school_holidays.as_deref(),
            Some("Vacances de Noël")
        );
        // Christmas: a Friday, a holiday, in the school holidays.
        let christmas = Day::of(date(2026, 12, 25), false, Some(&periods));
        assert_eq!(christmas.holiday, Some("noel"));
        assert!(!christmas.workday() && christmas.quiet());
        // A Tuesday in the school holidays: a workday, and a quiet one.
        let tuesday = Day::of(date(2026, 12, 22), false, Some(&periods));
        assert!(tuesday.workday() && tuesday.quiet());
        // Without the school calendar, the same Tuesday is an ordinary day.
        assert!(!Day::of(date(2026, 12, 22), false, None).quiet());
        // Armistice, a Wednesday.
        assert!(Day::of(date(2026, 11, 11), false, None).quiet());
    }

    #[test]
    fn the_query_asks_for_one_zone_each_period_once() {
        let q = query("Zone A", date(2026, 11, 27));
        assert!(q.starts_with(DATASET));
        assert!(q.contains("group_by=description%2Cpopulation%2Cstart_date%2Cend_date"));
        assert!(q.contains("zones%3D%22Zone%20A%22"));
        assert!(q.contains("end_date%3E%3D%222026-11-20%22"));
        assert!(q.contains("start_date%3C%3D%222028-01-01%22"));
        assert!(percent("Réunion").contains("%C3%A9"));
    }
}
