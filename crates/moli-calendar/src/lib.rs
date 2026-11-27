//! What kind of day it is, and the house's calendars.
//!
//! - « Jours »: `weekend`, `holiday` (and `holiday_name`), `workday`
//!   (neither), and `quiet_day`: weekend, holiday, or an event today in a
//!   calendar marked `quiet`. The French public holidays are worked out
//!   here, with no network ([`holidays`]).
//! - One device per calendar declared (`[[driver.options.calendar]]`): its
//!   events come from an iCalendar file in the data folder ([`ics`]), put
//!   there by hand (an export, the school calendar of a zone…). The file is
//!   read again as soon as it changes. Points: `on` (an event today),
//!   `event` (its name), `next`, `next_event` and `next_days`.
//!
//! Nothing leaves the house. The days change at midnight in `timezone`:
//! the points are worked out again every minute.

pub mod holidays;
pub mod ics;

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use jiff::Timestamp;
use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;

const TICK: Duration = Duration::from_secs(60);
/// A calendar file larger than this is not read (a house's calendar is a
/// few hundred kilobytes at most).
const MAX_FILE: u64 = 4 * 1024 * 1024;
/// The device of the days; a calendar cannot take its id.
const DAYS: &str = "days";

fn default_timezone() -> String {
    "Europe/Paris".into()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Alsace and Moselle: Good Friday and St Stephen's Day are holidays too.
    #[serde(default)]
    pub alsace_moselle: bool,
    #[serde(default, rename = "calendar")]
    pub calendars: Vec<CalendarConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarConfig {
    /// The device's id: lower case letters, digits, `_` and `-`.
    pub id: String,
    pub name: String,
    /// The iCalendar file, in the data folder (`calendars/vacances.ics`).
    pub file: String,
    /// An event of this calendar makes the day quiet (`quiet_day`).
    #[serde(default)]
    pub quiet: bool,
}

#[derive(Debug)]
pub struct Calendar {
    config: Config,
    /// Each calendar's file, in the data folder.
    files: Vec<PathBuf>,
}

/// A file of the data folder: a relative path that stays in it.
fn in_data(data_dir: &Path, file: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(file.trim());
    anyhow::ensure!(
        !file.trim().is_empty()
            && path
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
        "file {file:?}: a path in the data folder (calendars/vacances.ics), without .. nor /"
    );
    Ok(data_dir.join(path))
}

fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id != DAYS
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl Calendar {
    /// # Errors
    /// An unknown time zone, a calendar id taken twice or not plain, a file
    /// outside the data folder.
    pub fn new(config: Config, data_dir: &Path) -> anyhow::Result<Self> {
        TimeZone::get(&config.timezone)
            .map_err(|e| anyhow::anyhow!("timezone {:?}: {e}", config.timezone))?;
        let mut files = Vec::new();
        for (i, calendar) in config.calendars.iter().enumerate() {
            let id = calendar.id.as_str();
            anyhow::ensure!(
                plain_id(id),
                "calendar id {id:?}: lower case letters, digits, _ and - (and not {DAYS:?})"
            );
            anyhow::ensure!(
                config.calendars[..i].iter().all(|c| c.id != calendar.id),
                "calendar id {id:?} is given twice"
            );
            anyhow::ensure!(
                !calendar.name.trim().is_empty(),
                "calendar {id:?}: a name is needed"
            );
            files.push(
                in_data(data_dir, &calendar.file).with_context(|| format!("calendar {id:?}"))?,
            );
        }
        Ok(Self { config, files })
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
}

impl Day {
    #[must_use]
    pub fn of(day: Date, alsace_moselle: bool) -> Self {
        Self {
            weekend: matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday),
            holiday: holidays::on(day, alsace_moselle).map(|h| h.key),
        }
    }

    #[must_use]
    pub fn workday(&self) -> bool {
        !self.weekend && self.holiday.is_none()
    }
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

fn tr(key: &str) -> String {
    moli_i18n::tr!(&format!("pilotes.calendar.{key}"))
}

fn days_device(ctx: &DriverCtx, id: &DeviceId) -> Device {
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: tr("jours").into(),
        manufacturer: None,
        model: Some("Calendrier".into()),
        description: Some(tr("jours_description").into()),
        native_room: None,
        members: Vec::new(),
        points: vec![
            point("weekend", tr("week_end"), Kind::Binary),
            point("holiday", tr("ferie"), Kind::Binary),
            point("holiday_name", tr("ferie_nom"), Kind::Text),
            point("workday", tr("jour_ouvre"), Kind::Binary),
            point("quiet_day", tr("jour_calme"), Kind::Binary),
        ],
    }
}

fn calendar_device(ctx: &DriverCtx, id: &DeviceId, calendar: &CalendarConfig) -> Device {
    let mut next_days = point(
        "next_days",
        tr("prochain_jours"),
        Kind::Numeric {
            min: None,
            max: None,
            step: Some(1.0),
        },
    );
    next_days.unit = Some(Unit::Other("j".into()));
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: calendar.name.clone().into(),
        manufacturer: None,
        model: Some("Calendrier".into()),
        description: Some(
            moli_i18n::tr!("pilotes.calendar.description", fichier = calendar.file).into(),
        ),
        native_room: None,
        members: Vec::new(),
        points: vec![
            point("on", tr("aujourdhui"), Kind::Binary),
            point("event", tr("evenement"), Kind::Text),
            point("next", tr("prochain"), Kind::Text),
            point("next_event", tr("prochain_nom"), Kind::Text),
            next_days,
        ],
    }
}

/// A calendar as the driver keeps it: its events, and the file as it
/// was when read.
#[derive(Default)]
struct Loaded {
    events: Option<Vec<ics::Event>>,
    read: Option<(SystemTime, u64)>,
    problem: Option<String>,
}

/// Reads the file again if it changed since. A file that cannot be read
/// keeps the events read before (a file being rewritten), and says why.
async fn refresh(
    loaded: &mut Loaded,
    path: &Path,
    calendar: &CalendarConfig,
    tz: &TimeZone,
    instance: &str,
) {
    let meta = match tokio::fs::metadata(path).await {
        Ok(meta) => meta,
        Err(e) => {
            loaded.problem = Some(moli_i18n::tr!(
                "pilotes.calendar.fichier_absent",
                nom = calendar.name,
                fichier = calendar.file,
                erreur = e
            ));
            loaded.read = None;
            return;
        }
    };
    let stamp = (
        meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        meta.len(),
    );
    if loaded.read == Some(stamp) {
        return;
    }
    let read = async {
        anyhow::ensure!(
            meta.len() <= MAX_FILE,
            "more than {} MiB",
            MAX_FILE / 1024 / 1024
        );
        let bytes = tokio::fs::read(path).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    match read.await {
        Ok(text) => {
            let parsed = ics::parse(&text, tz);
            tracing::info!(
                instance,
                calendar = calendar.id,
                events = parsed.events.len(),
                skipped = parsed.skipped,
                unrepeated = parsed.unrepeated,
                "calendar read"
            );
            loaded.events = Some(parsed.events);
            loaded.read = Some(stamp);
            loaded.problem = None;
        }
        Err(e) => {
            loaded.problem = Some(moli_i18n::tr!(
                "pilotes.calendar.fichier_illisible",
                nom = calendar.name,
                fichier = calendar.file,
                erreur = format!("{e:#}")
            ));
            loaded.read = None;
        }
    }
}

fn publish_calendar(ctx: &DriverCtx, id: &DeviceId, events: &[ics::Event], today: Date) {
    let names = ics::on(events, today);
    ctx.set_state(id, "on", Value::Bool(!names.is_empty()));
    ctx.set_state(id, "event", Value::Text(names.join(", ").into()));
    if let Some((day, names)) = ics::next(events, today) {
        ctx.set_state(id, "next", Value::Text(day.to_string().into()));
        ctx.set_state(id, "next_event", Value::Text(names.join(", ").into()));
        ctx.set_state(
            id,
            "next_days",
            Value::Int(i64::from((day - today).get_days())),
        );
    } else {
        ctx.set_state(id, "next", Value::Text("".into()));
        ctx.set_state(id, "next_event", Value::Text("".into()));
        ctx.set_state(id, "next_days", Value::Null);
    }
}

/// `quiet_day`: weekend or holiday; otherwise an event of a quiet calendar
/// today, unknown while one of them has never been read.
fn quiet_day(day: &Day, quiet_event: Option<bool>) -> Value {
    if day.workday() {
        quiet_event.map_or(Value::Null, Value::Bool)
    } else {
        Value::Bool(true)
    }
}

fn publish_days(ctx: &DriverCtx, id: &DeviceId, day: &Day, quiet_event: Option<bool>) {
    ctx.set_state(id, "weekend", Value::Bool(day.weekend));
    ctx.set_state(id, "holiday", Value::Bool(day.holiday.is_some()));
    ctx.set_state(
        id,
        "holiday_name",
        Value::Text(
            day.holiday
                .map(|k| tr(&format!("f.{k}")))
                .unwrap_or_default()
                .into(),
        ),
    );
    ctx.set_state(id, "workday", Value::Bool(day.workday()));
    ctx.set_state(id, "quiet_day", quiet_day(day, quiet_event));
}

/// Whether a quiet calendar has an event today: `None` while one of them
/// has never been read and none says yes.
fn quiet_event<'a>(quiet: impl Iterator<Item = &'a Loaded> + Clone, day: Date) -> Option<bool> {
    if quiet.clone().any(|l| {
        l.events
            .as_ref()
            .is_some_and(|e| !ics::on(e, day).is_empty())
    }) {
        Some(true)
    } else if quiet.into_iter().all(|l| l.events.is_some()) {
        Some(false)
    } else {
        None
    }
}

async fn run(driver: &Calendar, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let config = &driver.config;
    let tz = TimeZone::get(&config.timezone)?;
    let today = || Timestamp::now().to_zoned(tz.clone()).date();
    let instance = ctx.instance().to_string();
    let days_id = ctx.device_id(DAYS);
    ctx.upsert_device(days_device(ctx, &days_id));
    ctx.set_availability(&days_id, true);
    let ids: Vec<DeviceId> = config
        .calendars
        .iter()
        .map(|c| ctx.device_id(&c.id))
        .collect();
    for (id, calendar) in ids.iter().zip(&config.calendars) {
        ctx.upsert_device(calendar_device(ctx, id, calendar));
    }
    ctx.ready();
    let mut loaded: Vec<Loaded> = config.calendars.iter().map(|_| Loaded::default()).collect();
    let mut said = String::new();
    loop {
        let day = today();
        let mut problems: BTreeMap<usize, &str> = BTreeMap::new();
        for (i, calendar) in config.calendars.iter().enumerate() {
            refresh(&mut loaded[i], &driver.files[i], calendar, &tz, &instance).await;
            ctx.set_availability(&ids[i], loaded[i].events.is_some());
            if let Some(events) = &loaded[i].events {
                publish_calendar(ctx, &ids[i], events, day);
            }
        }
        for (i, state) in loaded.iter().enumerate() {
            if let Some(problem) = &state.problem {
                problems.insert(i, problem);
            }
        }
        let now_said = problems.values().copied().collect::<Vec<_>>().join(" · ");
        if now_said != said {
            if now_said.is_empty() {
                ctx.clear_warning();
            } else {
                ctx.warn(now_said.clone());
            }
            said = now_said;
        }
        let quiet = config
            .calendars
            .iter()
            .zip(&loaded)
            .filter(|(c, _)| c.quiet)
            .map(|(_, l)| l);
        publish_days(
            ctx,
            &days_id,
            &Day::of(day, config.alsace_moselle),
            quiet_event(quiet, day),
        );
        tokio::select! {
            () = tokio::time::sleep(TICK) => {}
            command = ctx.next_command() => {
                if let Some(command) = command {
                    command.reply(Err(tr("lecture_seule")));
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

    fn calendar(id: &str, file: &str) -> CalendarConfig {
        CalendarConfig {
            id: id.into(),
            name: "Vacances scolaires".into(),
            file: file.into(),
            quiet: true,
        }
    }

    fn config(calendars: Vec<CalendarConfig>) -> Config {
        Config {
            timezone: "Europe/Paris".into(),
            alsace_moselle: false,
            calendars,
        }
    }

    #[test]
    fn the_options_are_checked() {
        let data = Path::new("/data");
        let made = Calendar::new(
            config(vec![calendar("vacances", "calendars/zone-a.ics")]),
            data,
        )
        .unwrap();
        assert_eq!(made.files, [PathBuf::from("/data/calendars/zone-a.ics")]);
        assert!(
            Calendar::new(config(vec![]), data).is_ok(),
            "the days alone"
        );
        for wrong in ["../secrets.enc", "/etc/passwd", "calendars/../../x.ics", ""] {
            assert!(
                Calendar::new(config(vec![calendar("v", wrong)]), data).is_err(),
                "{wrong:?}"
            );
        }
        for wrong in ["Vacances", "days", "", "a b"] {
            assert!(
                Calendar::new(config(vec![calendar(wrong, "v.ics")]), data).is_err(),
                "{wrong:?}"
            );
        }
        let twice = config(vec![calendar("v", "a.ics"), calendar("v", "b.ics")]);
        assert!(Calendar::new(twice, data).is_err());
        let mut c = config(vec![]);
        c.timezone = "Europe/Nulle-Part".into();
        assert!(Calendar::new(c, data).is_err());
    }

    #[test]
    fn the_options_read_from_toml() {
        let config: Config = toml::from_str(
            "[[calendar]]\nid = \"vacances\"\nname = \"Vacances scolaires\"\nfile = \"calendars/zone-a.ics\"\nquiet = true\n",
        )
        .unwrap();
        assert_eq!(config.calendars.len(), 1);
        assert!(config.calendars[0].quiet);
        assert_eq!(config.timezone, "Europe/Paris");
        assert!(
            toml::from_str::<Config>("school_zone = \"A\"").is_err(),
            "unknown keys refused"
        );
    }

    #[test]
    fn a_day_says_what_it_is() {
        let friday = Day::of(date(2026, 11, 27), false);
        assert!(friday.workday() && !friday.weekend);
        let saturday = Day::of(date(2026, 11, 28), false);
        assert!(saturday.weekend && !saturday.workday());
        let armistice = Day::of(date(2026, 11, 11), false);
        assert_eq!(armistice.holiday, Some("armistice"));
        assert!(!armistice.workday());
    }

    #[test]
    fn a_quiet_day_is_a_weekend_a_holiday_or_a_quiet_event() {
        let tz = TimeZone::get("Europe/Paris").unwrap();
        let text = std::fs::read_to_string(format!(
            "{}/../../integrations/calendar/fixtures/zone-a.ics",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let school = Loaded {
            events: Some(ics::parse(&text, &tz).events),
            ..Loaded::default()
        };
        let never_read = Loaded::default();
        let quiet = |day: Date, calendars: &[&Loaded]| {
            quiet_day(
                &Day::of(day, false),
                quiet_event(calendars.iter().copied(), day),
            )
        };
        // A Friday at school, then a Tuesday in the Christmas holidays.
        assert_eq!(quiet(date(2026, 11, 27), &[&school]), Value::Bool(false));
        assert_eq!(quiet(date(2026, 12, 22), &[&school]), Value::Bool(true));
        // The weekend, with or without a calendar.
        assert_eq!(quiet(date(2026, 11, 28), &[]), Value::Bool(true));
        // No quiet calendar at all: a workday is not quiet.
        assert_eq!(quiet(date(2026, 11, 27), &[]), Value::Bool(false));
        // A quiet calendar never read: unknown, unless another one says yes.
        assert_eq!(quiet(date(2026, 11, 27), &[&never_read]), Value::Null);
        assert_eq!(
            quiet(date(2026, 12, 22), &[&never_read, &school]),
            Value::Bool(true)
        );
    }
}
