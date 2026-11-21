//! « Dehors » (#210): the weather outside as **one** device, `dehors:meteo`,
//! fed for each meaning by the best source there is.
//!
//! A forecast is not a measure: a station near the house says what the
//! weather is, a forecast what a model expects. Automations and the
//! dashboard read `dehors:meteo`; for each meaning (temperature, wind…) it
//! carries the measure of the first station that has it while that measure
//! is fresh, else the forecast while it is fresh, else the latest value
//! known, whatever its source. So there is a value as soon as a source spoke
//! once, and it says where it comes from (`<key>_source`, the source
//! device) and how old it is (its time is the source's reading, not the
//! copy's).
//!
//! Falling back to the forecast is a choice, not a safety: a forecast may say
//! 20 km/h during a 70 km/h gust. An automation that must act on a measure
//! checks `<key>_source`.
//!
//! Moli runs it as instance [`INSTANCE`] whenever `moli.toml` declares it:
//!
//! ```toml
//! [outdoor]
//! forecast = ["meteo:…", "air:…"]   # one device or several
//! forecast_stale_after = "1h"       # optional, 30 min by default
//!
//! [[outdoor.station]]               # in order of preference
//! device = "z2m:0x…"
//! stale_after = "15m"               # optional, 30 min by default
//! ```
//!
//! With a station that measures the temperature, it also says the day's
//! measured extremes, `measured_max` and `measured_min` (#246): since
//! midnight at the house's time, over what the stations measured only (never
//! mixed with the forecast, which keeps its own `today_max` / `today_min`).
//! Nothing measured since midnight: no value. On a restart it starts again
//! from the day's history, not from nothing.
//!
//! Read-only: it takes no order and passes none on. Nothing is converted:
//! a source whose unit differs from the meaning's is left out (and said so).
//!
//! The same instance carries `dehors:soleil` ([`sun`], #258): where the sun
//! is, from the house's position alone, `[outdoor]` or not.

pub mod sun;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use moli_core::{
    Access, Device, DeviceId, Event, InstanceId, Kind, PointId, PointSpec, Semantic, Unit, Value,
    now_ms,
};
use moli_history::History;
use moli_runtime::{BoxFuture, Driver, DriverCtx, Hub};
use serde::{Deserialize, Deserializer};
use tokio::sync::broadcast::error::RecvError;

/// The instance (and the device's prefix: `dehors:meteo`).
pub const INSTANCE: &str = "dehors";
/// The device, inside the instance.
pub const NATIVE: &str = "meteo";
/// A source with no `stale_after` is fresh for half an hour.
pub const DEFAULT_FRESH: Duration = Duration::from_secs(30 * 60);
/// Freshness is the only thing that changes without a source speaking.
const TICK: Duration = Duration::from_secs(60);

/// What the device carries, by meaning, under the key named after it.
pub const MEANINGS: [(Semantic, &str); 10] = [
    (Semantic::Temperature, "temperature"),
    (Semantic::Humidity, "humidity"),
    (Semantic::Pressure, "pressure"),
    (Semantic::Illuminance, "illuminance"),
    (Semantic::WindSpeed, "wind_speed"),
    (Semantic::WindGust, "wind_gust"),
    (Semantic::WindDirection, "wind_direction"),
    (Semantic::RainRate, "rain_rate"),
    (Semantic::RainToday, "rain_today"),
    (Semantic::UvIndex, "uv_index"),
];

/// `[outdoor]`, as `moli.toml` declares it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The forecast: one device id, or several (the weather, the air).
    #[serde(default, deserialize_with = "one_or_many")]
    pub forecast: Vec<String>,
    /// How long a forecast stays fresh (`"1h"`); 30 min by default.
    #[serde(default)]
    pub forecast_stale_after: Option<String>,
    /// The stations, in order of preference.
    #[serde(default)]
    pub station: Vec<Station>,
}

/// A station: a device that measures the weather near the house.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Station {
    pub device: String,
    /// How long its measure stays fresh (`"15m"`, `"2h"`); 30 min by default.
    #[serde(default)]
    pub stale_after: Option<String>,
}

fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(one) => vec![one],
        OneOrMany::Many(many) => many,
    })
}

/// `"90s"`, `"15m"`, `"2h"`, `"1d"` → a duration, between a minute and two
/// days.
#[must_use]
pub fn parse_fresh(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = n.parse().ok()?;
    let secs = n.checked_mul(match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return None,
    })?;
    (60..=2 * 86_400)
        .contains(&secs)
        .then(|| Duration::from_secs(secs))
}

/// `[outdoor]` checked: a source at least, device ids, durations, nobody
/// twice.
pub fn check(config: &Config) -> Result<(), String> {
    if config.forecast.is_empty() && config.station.is_empty() {
        return Err("a forecast, a station, or both".into());
    }
    let mut seen = HashSet::new();
    let ids = config
        .station
        .iter()
        .map(|s| &s.device)
        .chain(&config.forecast);
    for id in ids {
        if !id.contains(':') || id.contains('/') {
            return Err(format!("{id:?} is not a device id"));
        }
        if id.split(':').next() == Some(INSTANCE) {
            return Err(format!(
                "{id:?} is the outdoor weather itself: it cannot be its own source"
            ));
        }
        if !seen.insert(id) {
            return Err(format!("{id:?} is given twice"));
        }
    }
    let durations = config
        .station
        .iter()
        .filter_map(|s| s.stale_after.as_ref())
        .chain(&config.forecast_stale_after);
    for d in durations {
        if parse_fresh(d).is_none() {
            return Err(format!(
                "stale_after {d:?} must be like 90s, 15m, 2h, 1d (1 min to 2 days)"
            ));
        }
    }
    Ok(())
}

/// A source, in the order it is preferred.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub device: DeviceId,
    pub fresh: Duration,
    /// A station (a measure), not a forecast.
    pub measured: bool,
}

impl Config {
    /// The stations in their order, then the forecast.
    #[must_use]
    pub fn sources(&self) -> Vec<Source> {
        let fresh = |d: Option<&String>| d.and_then(|d| parse_fresh(d)).unwrap_or(DEFAULT_FRESH);
        let stations = self.station.iter().map(|s| Source {
            device: DeviceId::from(s.device.as_str()),
            fresh: fresh(s.stale_after.as_ref()),
            measured: true,
        });
        let forecasts = self.forecast.iter().map(|f| Source {
            device: DeviceId::from(f.as_str()),
            fresh: fresh(self.forecast_stale_after.as_ref()),
            measured: false,
        });
        stations.chain(forecasts).collect()
    }
}

// ---- which point of a source gives a meaning ------------------------------------------

/// The point of a device that gives a meaning.
#[derive(Debug, PartialEq)]
pub enum Pick<'a> {
    One(&'a PointSpec),
    None,
    /// Several points have the meaning and none is named after it (a
    /// forecast's temperature, felt temperature, highest and lowest of the
    /// day): the source does not give it.
    Several,
}

/// The readable number with the meaning whose key is the meaning's name,
/// else the only readable number with it.
#[must_use]
pub fn pick<'a>(points: &'a [PointSpec], semantic: Semantic, key: &str) -> Pick<'a> {
    let with: Vec<&PointSpec> = points
        .iter()
        .filter(|p| {
            p.semantic == semantic && p.access.read && matches!(p.kind, Kind::Numeric { .. })
        })
        .collect();
    if let Some(named) = with.iter().find(|p| &*p.key == key) {
        return Pick::One(named);
    }
    match with.as_slice() {
        [] => Pick::None,
        [one] => Pick::One(one),
        _ => Pick::Several,
    }
}

/// A meaning the device carries: its unit and, in order, where to read it.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub semantic: Semantic,
    pub key: &'static str,
    pub unit: Option<Unit>,
    /// (index in the sources, the point there).
    pub points: Vec<(usize, PointId)>,
}

/// What the sources give, meaning by meaning, and what is left out (said in
/// the logs).
#[derive(Debug, Default, PartialEq)]
pub struct Layout {
    pub slots: Vec<Slot>,
    pub notes: Vec<String>,
}

/// The layout from what each source declares (`None`: not there yet).
#[must_use]
pub fn layout(sources: &[Source], devices: &[Option<&Device>]) -> Layout {
    let mut out = Layout::default();
    for (semantic, key) in MEANINGS {
        let mut unit: Option<Option<Unit>> = semantic.fixed_unit();
        let mut points = Vec::new();
        for (i, (source, device)) in sources.iter().zip(devices).enumerate() {
            let Some(device) = device else { continue };
            match pick(&device.points, semantic, key) {
                Pick::None => {}
                Pick::Several => out.notes.push(format!(
                    "{}: several points mean {key} and none is named so: left out",
                    source.device
                )),
                Pick::One(point) => {
                    let want = unit.get_or_insert_with(|| point.unit.clone());
                    if point.unit == *want {
                        points.push((i, PointId::new(&source.device, &point.key)));
                    } else {
                        out.notes.push(format!(
                            "{}/{}: {key} is in {}, not {}: left out",
                            source.device,
                            point.key,
                            point.unit.as_ref().map_or("no unit", Unit::symbol),
                            want.as_ref().map_or("no unit", Unit::symbol),
                        ));
                    }
                }
            }
        }
        if !points.is_empty() {
            out.slots.push(Slot {
                semantic,
                key,
                unit: unit.flatten(),
                points,
            });
        }
    }
    out
}

// ---- which value wins -----------------------------------------------------------------

/// A value a source holds for a meaning.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// The source device.
    pub source: DeviceId,
    pub value: Value,
    /// When the source read it (ms since the epoch).
    pub ts: u64,
    pub fresh: Duration,
}

/// The first fresh candidate (they come in order of preference), else the
/// most recent one, whatever its source.
#[must_use]
pub fn choose(candidates: &[Candidate], now: u64) -> Option<&Candidate> {
    let fresh = |c: &&Candidate| {
        let max = u64::try_from(c.fresh.as_millis()).unwrap_or(u64::MAX);
        now.saturating_sub(c.ts) <= max
    };
    candidates
        .iter()
        .find(fresh)
        .or_else(|| candidates.iter().max_by_key(|c| c.ts))
}

/// What to publish for a meaning, against what was published last.
#[derive(Debug, PartialEq, Eq)]
pub struct Publish {
    /// The value, with its reading's time.
    pub value: bool,
    /// `<key>_source`: only when the source changes, so its time says since
    /// when that source is the one.
    pub source: bool,
}

/// A new value, a new source or another reading is published; the hub tells
/// a newer reading of the same value only as a `Seen`, every five minutes at
/// most (#230), so a station repeating 0 mm writes nothing in the history.
#[must_use]
pub fn publish(last: Option<&Candidate>, chosen: &Candidate) -> Publish {
    let Some(shown) = last else {
        return Publish {
            value: true,
            source: true,
        };
    };
    let source = shown.source != chosen.source;
    Publish {
        value: source || shown.value != chosen.value || shown.ts != chosen.ts,
        source,
    }
}

// ---- the day's measured extremes ------------------------------------------------------

/// The day's measured extremes: their keys on the device.
pub const MEASURED_MAX: &str = "measured_max";
pub const MEASURED_MIN: &str = "measured_min";

/// Midnight at the house's time, for the day `now` is in (ms since the epoch).
#[must_use]
pub fn day_start(now: u64, zone: &jiff::tz::TimeZone) -> u64 {
    let Ok(at) = i64::try_from(now).map(jiff::Timestamp::from_millisecond) else {
        return 0;
    };
    let Ok(at) = at else { return 0 };
    at.to_zoned(zone.clone())
        .start_of_day()
        .ok()
        .and_then(|midnight| u64::try_from(midnight.timestamp().as_millisecond()).ok())
        .unwrap_or(0)
}

/// A measure and the time it was read at, if any.
pub type Reading = Option<(f64, u64)>;

/// The highest and the lowest measure since `start`, each with the time it
/// was read at.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extremes {
    pub start: u64,
    pub max: Reading,
    pub min: Reading,
}

impl Extremes {
    #[must_use]
    pub fn since(start: u64) -> Self {
        Self {
            start,
            ..Self::default()
        }
    }

    /// A measure read at `ts`: counted if it is of the day.
    pub fn see(&mut self, value: f64, ts: u64) {
        self.see_range(value, value, ts);
    }

    /// A span of measures (a history bucket): its lowest and its highest.
    pub fn see_range(&mut self, low: f64, high: f64, ts: u64) {
        if ts < self.start || !low.is_finite() || !high.is_finite() {
            return;
        }
        if self.max.is_none_or(|(max, _)| high > max) {
            self.max = Some((high, ts));
        }
        if self.min.is_none_or(|(min, _)| low < min) {
            self.min = Some((low, ts));
        }
    }
}

/// The stations' temperature points: what the day's extremes are made of.
fn measured_temperature<'a>(sources: &[Source], layout: &'a Layout) -> Vec<&'a PointId> {
    layout
        .slots
        .iter()
        .find(|s| s.semantic == Semantic::Temperature)
        .map(|slot| {
            slot.points
                .iter()
                .filter(|(i, _)| sources[*i].measured)
                .map(|(_, p)| p)
                .collect()
        })
        .unwrap_or_default()
}

// ---- the device -----------------------------------------------------------------------

fn label(semantic: Semantic) -> String {
    match semantic {
        Semantic::Temperature => moli_i18n::tr!("pilotes.dehors.temperature"),
        Semantic::Humidity => moli_i18n::tr!("pilotes.dehors.humidite"),
        Semantic::Pressure => moli_i18n::tr!("pilotes.dehors.pression"),
        Semantic::Illuminance => moli_i18n::tr!("pilotes.dehors.luminosite"),
        Semantic::WindSpeed => moli_i18n::tr!("pilotes.dehors.vent"),
        Semantic::WindGust => moli_i18n::tr!("pilotes.dehors.rafales"),
        Semantic::WindDirection => moli_i18n::tr!("pilotes.dehors.direction"),
        Semantic::RainRate => moli_i18n::tr!("pilotes.dehors.intensite_pluie"),
        Semantic::RainToday => moli_i18n::tr!("pilotes.dehors.pluie_jour"),
        Semantic::UvIndex => moli_i18n::tr!("pilotes.dehors.uv"),
        _ => String::new(),
    }
}

fn read_only(
    key: &str,
    label: &str,
    kind: Kind,
    unit: Option<Unit>,
    semantic: Semantic,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic,
    }
}

/// Where a meaning's value comes from: `<key>_source`.
#[must_use]
pub fn source_key(key: &str) -> String {
    format!("{key}_source")
}

/// The device: a number and its source per meaning, then which devices are
/// stations and which the forecast (for the dashboard's « mesuré » /
/// « prévu »).
#[must_use]
pub fn device(id: DeviceId, instance: &InstanceId, sources: &[Source], layout: &Layout) -> Device {
    let mut points = Vec::new();
    for slot in &layout.slots {
        let label = label(slot.semantic);
        let numeric = Kind::Numeric {
            min: None,
            max: None,
            step: None,
        };
        points.push(read_only(
            slot.key,
            &label,
            numeric,
            slot.unit.clone(),
            slot.semantic,
        ));
        points.push(read_only(
            &source_key(slot.key),
            &moli_i18n::tr!("pilotes.dehors.source", what = label),
            Kind::Text,
            None,
            Semantic::Config,
        ));
    }
    if !measured_temperature(sources, layout).is_empty() {
        let unit = layout
            .slots
            .iter()
            .find(|s| s.semantic == Semantic::Temperature)
            .and_then(|s| s.unit.clone());
        for (key, label) in [
            (MEASURED_MAX, moli_i18n::tr!("pilotes.dehors.max_mesure")),
            (MEASURED_MIN, moli_i18n::tr!("pilotes.dehors.min_mesure")),
        ] {
            let numeric = Kind::Numeric {
                min: None,
                max: None,
                step: None,
            };
            points.push(read_only(
                key,
                &label,
                numeric,
                unit.clone(),
                Semantic::Temperature,
            ));
        }
    }
    points.push(read_only(
        "stations",
        &moli_i18n::tr!("pilotes.dehors.stations"),
        Kind::Text,
        None,
        Semantic::Config,
    ));
    points.push(read_only(
        "forecast",
        &moli_i18n::tr!("pilotes.dehors.previsions"),
        Kind::Text,
        None,
        Semantic::Config,
    ));
    Device {
        id,
        instance: instance.clone(),
        native_name: moli_i18n::tr!("pilotes.dehors.nom").into(),
        manufacturer: Some("Moli".into()),
        // What the dashboard recognizes it by: never translated.
        model: Some("Dehors".into()),
        description: None,
        native_room: None,
        members: sources.iter().map(|s| s.device.clone()).collect(),
        points,
    }
}

/// The ids of the stations (`measured`) or of the forecast, comma-separated.
fn ids(sources: &[Source], measured: bool) -> String {
    sources
        .iter()
        .filter(|s| s.measured == measured)
        .map(|s| s.device.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

// ---- the driver -----------------------------------------------------------------------

pub struct Outdoor {
    hub: Hub,
    /// `[outdoor]` is declared: `dehors:meteo` is there.
    weather: bool,
    sources: Vec<Source>,
    /// Where the house is, for `dehors:soleil` (`None`: no sun, the demo).
    place: Option<sun::Place>,
    /// `[[sun.facade]]`: whether the sun is on each.
    facades: Vec<sun::Facade>,
    /// Where the day's extremes start again after a restart.
    history: Option<History>,
    /// The house's time zone: the day starts at its midnight.
    zone: jiff::tz::TimeZone,
}

impl std::fmt::Debug for Outdoor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outdoor")
            .field("sources", &self.sources)
            .finish_non_exhaustive()
    }
}

impl Outdoor {
    /// `config`: `[outdoor]`, if declared (else only the sun); `zone`: the
    /// house's IANA time zone (UTC if unknown); `place`: where the house is;
    /// `facades`: `[[sun.facade]]`.
    #[must_use]
    pub fn new(
        hub: Hub,
        config: Option<&Config>,
        history: Option<History>,
        zone: &str,
        place: Option<sun::Place>,
        facades: Vec<sun::Facade>,
    ) -> Self {
        Self {
            hub,
            weather: config.is_some(),
            sources: config.map(Config::sources).unwrap_or_default(),
            place,
            facades,
            history,
            zone: jiff::tz::TimeZone::get(zone).unwrap_or(jiff::tz::TimeZone::UTC),
        }
    }
}

/// What the driver last published.
#[derive(Default)]
struct Published {
    device: Option<Device>,
    /// Per meaning: what was published, and when by the clock.
    values: HashMap<&'static str, Candidate>,
    online: Option<bool>,
    /// Notes already logged: each once.
    noted: HashSet<String>,
    /// The day's measured extremes, what of them was published, and the
    /// station points whose day was read again from the history.
    day: Extremes,
    shown: (Reading, Reading),
    restored: HashSet<PointId>,
    /// What `dehors:soleil` shows.
    sun: sun::Shown,
}

impl Outdoor {
    /// Where the sun is now: published as it moves by a step.
    fn sun(&self, ctx: &DriverCtx, published: &mut Published) {
        let place = self.place.as_ref().and_then(|place| place());
        let id = ctx.device_id(sun::NATIVE);
        match sun::tick(&mut published.sun, place, now_ms(), &self.facades) {
            sun::Change::Nothing => {}
            sun::Change::Remove => ctx.remove_device(&id),
            sun::Change::Show {
                declare,
                elevation,
                azimuth,
                facades,
            } => {
                if declare {
                    ctx.upsert_device(sun::device(id.clone(), ctx.instance(), &self.facades));
                    ctx.set_availability(&id, true);
                    if !self.facades.is_empty() {
                        ctx.set_state(
                            &id,
                            sun::FACADES,
                            Value::from(sun::names(&self.facades).as_str()),
                        );
                    }
                }
                for (key, value) in [(sun::ELEVATION, elevation), (sun::AZIMUTH, azimuth)] {
                    if let Some(v) = value {
                        ctx.set_state(&id, key, Value::Float(v));
                    }
                }
                for (facade, (incidence, on)) in self.facades.iter().zip(facades) {
                    if let Some(v) = incidence {
                        ctx.set_state(
                            &id,
                            &format!("{}{}", facade.id, sun::INCIDENCE),
                            Value::Float(v),
                        );
                    }
                    if let Some(on) = on {
                        ctx.set_state(&id, &format!("{}{}", facade.id, sun::ON), Value::Bool(on));
                    }
                }
            }
        }
    }

    async fn refresh(&self, ctx: &DriverCtx, published: &mut Published) {
        if !self.weather {
            return;
        }
        let id = ctx.device_id(NATIVE);
        let views: Vec<_> = self
            .sources
            .iter()
            .map(|s| self.hub.device(&s.device))
            .collect();
        let devices: Vec<Option<&Device>> = views
            .iter()
            .map(|v| v.as_ref().map(|v| &*v.device))
            .collect();
        let layout = layout(&self.sources, &devices);
        for note in &layout.notes {
            if published.noted.insert(note.clone()) {
                tracing::warn!(instance = INSTANCE, "{note}");
            }
        }
        let device = device(id.clone(), ctx.instance(), &self.sources, &layout);
        if published.device.as_ref() != Some(&device) {
            ctx.upsert_device(device.clone());
            ctx.set_state(
                &id,
                "stations",
                Value::from(ids(&self.sources, true).as_str()),
            );
            ctx.set_state(
                &id,
                "forecast",
                Value::from(ids(&self.sources, false).as_str()),
            );
            published.device = Some(device);
            published.values.clear();
            published.shown = (None, None);
        }
        let now = now_ms();
        for slot in &layout.slots {
            let candidates: Vec<Candidate> = slot
                .points
                .iter()
                .filter_map(|(i, point)| {
                    let sample = self.hub.state(point)?;
                    sample.value.as_f64()?;
                    Some(Candidate {
                        source: self.sources[*i].device.clone(),
                        value: sample.value,
                        ts: sample.ts,
                        fresh: self.sources[*i].fresh,
                    })
                })
                .collect();
            let Some(chosen) = choose(&candidates, now) else {
                continue;
            };
            let what = publish(published.values.get(slot.key), chosen);
            if what.source {
                ctx.set_state(
                    &id,
                    &source_key(slot.key),
                    Value::from(chosen.source.as_str()),
                );
            }
            if what.value {
                ctx.set_state_at(&id, slot.key, chosen.value.clone(), chosen.ts);
                published.values.insert(slot.key, chosen.clone());
            }
        }
        self.extremes(ctx, &id, &layout, published).await;
        // Reachable while one of its sources is.
        let online = views.iter().flatten().any(|v| v.online != Some(false));
        if published.online != Some(online) {
            ctx.set_availability(&id, online);
            published.online = Some(online);
        }
    }
}

impl Outdoor {
    /// The day's measured extremes: what the stations measured since
    /// midnight, the day's history first after a restart.
    async fn extremes(
        &self,
        ctx: &DriverCtx,
        id: &DeviceId,
        layout: &Layout,
        published: &mut Published,
    ) {
        let points = measured_temperature(&self.sources, layout);
        if points.is_empty() {
            return;
        }
        let now = now_ms();
        let start = day_start(now, &self.zone);
        if published.day.start != start {
            // A new day: nothing measured yet.
            published.day = Extremes::since(start);
            published.restored.clear();
        }
        for point in points {
            if let Some(history) = &self.history
                && published.restored.insert(point.clone())
            {
                match history.series(point.as_str(), start, now, 5_000).await {
                    Ok(series) => {
                        for (ts, value) in series.raw {
                            if let Some(v) = value.as_f64() {
                                published.day.see(v, ts);
                            }
                        }
                        for bucket in series.buckets {
                            published.day.see_range(bucket.min, bucket.max, bucket.ts);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(instance = INSTANCE, %point, "the day's history: {e}");
                    }
                }
            }
            if let Some(sample) = self.hub.state(point)
                && let Some(v) = sample.value.as_f64()
            {
                published.day.see(v, sample.ts);
            }
        }
        let day = (published.day.max, published.day.min);
        for (key, now_, was) in [
            (MEASURED_MAX, day.0, published.shown.0),
            (MEASURED_MIN, day.1, published.shown.1),
        ] {
            if now_ == was {
                continue;
            }
            match now_ {
                Some((value, ts)) => ctx.set_state_at(id, key, Value::Float(value), ts),
                None => ctx.set_state(id, key, Value::Null),
            }
        }
        published.shown = day;
    }
}

enum Next {
    Refresh,
    Event(Result<Event, RecvError>),
    Stop,
}

impl Driver for Outdoor {
    fn kind(&self) -> &'static str {
        "outdoor"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            // Subscribe before reading: nothing falls in between.
            let mut events = self.hub.subscribe();
            let members: HashSet<DeviceId> =
                self.sources.iter().map(|s| s.device.clone()).collect();
            let mut published = Published::default();
            self.refresh(ctx, &mut published).await;
            self.sun(ctx, &mut published);
            ctx.ready();
            let mut tick = tokio::time::interval(TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let next = tokio::select! {
                    command = ctx.next_command() => match command {
                        Some(command) => {
                            command.reply(Err(moli_i18n::tr!("pilotes.dehors.lecture_seule")));
                            continue;
                        }
                        None => Next::Stop,
                    },
                    event = events.recv() => Next::Event(event),
                    _ = tick.tick() => Next::Refresh,
                };
                let refresh = match next {
                    Next::Stop | Next::Event(Err(RecvError::Closed)) => return Ok(()),
                    Next::Refresh => {
                        self.sun(ctx, &mut published);
                        true
                    }
                    Next::Event(Err(RecvError::Lagged(_))) => true,
                    Next::Event(Ok(event)) => {
                        let device = match &event {
                            Event::State { point, .. } => point.split().map(|(d, _)| d),
                            Event::Availability { device, .. }
                            | Event::DeviceRemoved { device } => Some(device.clone()),
                            Event::DeviceUpserted { device } => Some(device.id.clone()),
                            _ => None,
                        };
                        device.is_some_and(|d| members.contains(&d))
                    }
                };
                if refresh {
                    self.refresh(ctx, &mut published).await;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(key: &str, semantic: Semantic, unit: Option<Unit>) -> PointSpec {
        read_only(
            key,
            key,
            Kind::Numeric {
                min: None,
                max: None,
                step: None,
            },
            unit,
            semantic,
        )
    }

    fn device_with(id: &str, points: Vec<PointSpec>) -> Device {
        Device {
            id: DeviceId::from(id),
            instance: InstanceId::from(id.split(':').next().unwrap()),
            native_name: id.into(),
            manufacturer: None,
            model: None,
            description: None,
            native_room: None,
            members: Vec::new(),
            points,
        }
    }

    fn config(toml: &str) -> Config {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn the_configuration_is_checked() {
        let c = config(
            "forecast = \"meteo:api\"\n[[station]]\ndevice = \"z2m:0x1\"\nstale_after = \"15m\"\n",
        );
        assert!(check(&c).is_ok());
        let sources = c.sources();
        assert_eq!(sources[0].device.as_str(), "z2m:0x1");
        assert!(sources[0].measured);
        assert_eq!(sources[0].fresh, Duration::from_secs(15 * 60));
        assert_eq!(sources[1].fresh, DEFAULT_FRESH);
        assert!(!sources[1].measured);

        let many =
            config("forecast = [\"meteo:api\", \"air:api\"]\nforecast_stale_after = \"1h\"\n");
        assert!(check(&many).is_ok());
        assert_eq!(many.sources()[1].fresh, Duration::from_secs(3600));

        assert!(check(&config("")).is_err());
        assert!(check(&config("forecast = \"meteo:api/temperature\"\n")).is_err());
        // Never its own source.
        assert!(check(&config("[[station]]\ndevice = \"dehors:meteo\"\n")).is_err());
        assert!(check(&config("forecast = \"dehors:meteo\"\n")).is_err());
        assert!(
            check(&config(
                "forecast = \"meteo:api\"\n[[station]]\ndevice = \"meteo:api\"\n"
            ))
            .is_err()
        );
        assert!(
            check(&config(
                "[[station]]\ndevice = \"z2m:0x1\"\nstale_after = \"10\"\n"
            ))
            .is_err()
        );
        assert!(
            check(&config(
                "[[station]]\ndevice = \"z2m:0x1\"\nstale_after = \"5s\"\n"
            ))
            .is_err()
        );
        assert!(toml::from_str::<Config>("forecst = \"meteo:api\"\n").is_err());
    }

    /// A forecast has four temperatures: the one named so is the temperature.
    /// A station's dew point (°C too) never passes for it.
    #[test]
    fn the_point_named_after_the_meaning_wins() {
        let forecast = vec![
            spec("feels_like", Semantic::Temperature, Some(Unit::Celsius)),
            spec("temperature", Semantic::Temperature, Some(Unit::Celsius)),
            spec("today_max", Semantic::Temperature, Some(Unit::Celsius)),
            spec(
                "wind_gusts",
                Semantic::WindGust,
                Some(Unit::KilometrePerHour),
            ),
        ];
        assert!(
            matches!(pick(&forecast, Semantic::Temperature, "temperature"), Pick::One(p) if &*p.key == "temperature")
        );
        // The only one with the meaning, whatever its key.
        assert!(
            matches!(pick(&forecast, Semantic::WindGust, "wind_gust"), Pick::One(p) if &*p.key == "wind_gusts")
        );
        assert_eq!(pick(&forecast, Semantic::Humidity, "humidity"), Pick::None);
        let unnamed = vec![
            spec("dew_point", Semantic::Temperature, Some(Unit::Celsius)),
            spec(
                "apparent_temperature",
                Semantic::Temperature,
                Some(Unit::Celsius),
            ),
        ];
        assert_eq!(
            pick(&unnamed, Semantic::Temperature, "temperature"),
            Pick::Several
        );
    }

    #[test]
    fn a_source_in_another_unit_is_left_out() {
        let sources =
            config("forecast = \"meteo:api\"\n[[station]]\ndevice = \"z2m:0x1\"\n").sources();
        let station = device_with(
            "z2m:0x1",
            vec![
                spec(
                    "temperature",
                    Semantic::Temperature,
                    Some(Unit::Other("°F".into())),
                ),
                spec("humidity", Semantic::Humidity, Some(Unit::Percent)),
            ],
        );
        let forecast = device_with(
            "meteo:api",
            vec![
                spec("temperature", Semantic::Temperature, Some(Unit::Celsius)),
                spec("humidity", Semantic::Humidity, Some(Unit::Percent)),
                spec(
                    "wind_speed",
                    Semantic::WindSpeed,
                    Some(Unit::KilometrePerHour),
                ),
            ],
        );
        let out = layout(&sources, &[Some(&station), Some(&forecast)]);
        let keys: Vec<_> = out.slots.iter().map(|s| s.key).collect();
        assert_eq!(keys, ["temperature", "humidity", "wind_speed"]);
        // The station came first: its °F set the unit, the forecast's °C is left out.
        assert_eq!(out.slots[0].unit, Some(Unit::Other("°F".into())));
        assert_eq!(out.slots[0].points.len(), 1);
        assert_eq!(out.notes.len(), 1);
        assert!(out.notes[0].contains("°C"), "{}", out.notes[0]);
        // Both give the humidity, the station first.
        assert_eq!(out.slots[1].points[0].0, 0);
        assert_eq!(out.slots[1].points[1].0, 1);
        // A meaning with its own unit never takes another.
        let windy = device_with(
            "z2m:0x1",
            vec![spec(
                "wind_speed",
                Semantic::WindSpeed,
                Some(Unit::Other("m/s".into())),
            )],
        );
        let out = layout(&sources, &[Some(&windy), Some(&forecast)]);
        assert_eq!(
            out.slots
                .iter()
                .find(|s| s.key == "wind_speed")
                .unwrap()
                .points,
            vec![(1, PointId::from("meteo:api/wind_speed"))]
        );
        // A source not there yet gives nothing.
        assert_eq!(layout(&sources, &[None, None]), Layout::default());
    }

    fn candidate(source: &str, value: f64, ts: u64, fresh_min: u64) -> Candidate {
        Candidate {
            source: DeviceId::from(source),
            value: Value::Float(value),
            ts,
            fresh: Duration::from_secs(fresh_min * 60),
        }
    }

    #[test]
    fn the_fresh_measure_then_the_fresh_forecast_then_the_latest() {
        let min = 60_000;
        let now = 1_000 * min;
        let station = candidate("z2m:0x1", 11.0, now - 5 * min, 15);
        let forecast = candidate("meteo:api", 12.0, now - 10 * min, 30);
        assert_eq!(
            choose(&[station.clone(), forecast.clone()], now),
            Some(&station)
        );
        // The station went quiet: the forecast, fresh, takes over.
        let quiet = candidate("z2m:0x1", 11.0, now - 20 * min, 15);
        assert_eq!(
            choose(&[quiet.clone(), forecast], now),
            Some(&forecast_of(now - 10 * min))
        );
        // Nobody fresh: the latest value, whatever its source.
        let old = candidate("meteo:api", 12.0, now - 60 * min, 30);
        assert_eq!(choose(&[quiet.clone(), old], now), Some(&quiet));
        // The time is the source's: a value read 5 min ago stays 5 min old.
        assert_eq!(
            choose(std::slice::from_ref(&station), now).unwrap().ts,
            now - 5 * min
        );
        assert_eq!(choose(&[], now), None);
    }

    /// Another reading is handed to the hub, which tells a repeated value
    /// only as a `Seen` (#230); the same reading again is nothing.
    #[test]
    fn a_new_reading_value_or_source_is_published() {
        let min = 60_000;
        let now = 1_000 * min;
        let first = candidate("z2m:0x1", 0.0, now - min, 15);
        assert_eq!(
            publish(None, &first),
            Publish {
                value: true,
                source: true
            }
        );
        // The same reading again: nothing.
        assert_eq!(
            publish(Some(&first), &first),
            Publish {
                value: false,
                source: false
            }
        );
        // Read again a minute later, same value: to the hub, which keeps
        // its age without a line in the history.
        let again = candidate("z2m:0x1", 0.0, now, 15);
        assert_eq!(
            publish(Some(&first), &again),
            Publish {
                value: true,
                source: false
            }
        );
        // A new value.
        let wetter = candidate("z2m:0x1", 0.2, now, 15);
        assert_eq!(
            publish(Some(&first), &wetter),
            Publish {
                value: true,
                source: false
            }
        );
        // Another source, and its source too.
        let forecast = candidate("meteo:api", 0.0, now, 30);
        assert_eq!(
            publish(Some(&first), &forecast),
            Publish {
                value: true,
                source: true
            }
        );
    }

    /// The day starts at midnight, the house's time: Paris is an hour ahead
    /// of UTC in winter, two in summer.
    #[test]
    fn the_day_starts_at_the_house_midnight() {
        let paris = jiff::tz::TimeZone::get("Europe/Paris").unwrap();
        // 2026-11-17 13:30 UTC = 14:30 in Paris → midnight in Paris = 23:00 UTC the day before.
        let now: jiff::Timestamp = "2026-11-17T13:30:00Z".parse().unwrap();
        let midnight: jiff::Timestamp = "2026-11-16T23:00:00Z".parse().unwrap();
        let ms = |t: jiff::Timestamp| u64::try_from(t.as_millisecond()).unwrap();
        assert_eq!(day_start(ms(now), &paris), ms(midnight));
        // 23:30 UTC is already the next day in Paris.
        let late: jiff::Timestamp = "2026-11-17T23:30:00Z".parse().unwrap();
        let next: jiff::Timestamp = "2026-11-17T23:00:00Z".parse().unwrap();
        assert_eq!(day_start(ms(late), &paris), ms(next));
        // Summer: two hours ahead.
        let july: jiff::Timestamp = "2026-07-01T12:00:00Z".parse().unwrap();
        let july_midnight: jiff::Timestamp = "2026-06-30T22:00:00Z".parse().unwrap();
        assert_eq!(day_start(ms(july), &paris), ms(july_midnight));
    }

    /// What was measured since midnight only; nothing measured, no value.
    #[test]
    fn the_day_extremes_count_the_day_only() {
        let mut day = Extremes::since(1_000);
        assert_eq!((day.max, day.min), (None, None));
        // Read before midnight: yesterday's.
        day.see(-3.0, 999);
        assert_eq!((day.max, day.min), (None, None));
        day.see(4.0, 1_100);
        day.see(9.5, 1_200);
        day.see(2.5, 1_300);
        day.see(f64::NAN, 1_400);
        assert_eq!(day.max, Some((9.5, 1_200)));
        assert_eq!(day.min, Some((2.5, 1_300)));
        // A history bucket brings its own lowest and highest.
        day.see_range(1.0, 11.0, 1_500);
        assert_eq!(day.max, Some((11.0, 1_500)));
        assert_eq!(day.min, Some((1.0, 1_500)));
        // The same value again keeps the first time it was reached.
        day.see(11.0, 1_600);
        assert_eq!(day.max, Some((11.0, 1_500)));
    }

    /// The extremes exist with a station that measures the temperature; a
    /// forecast alone has its own `today_max` / `today_min`.
    #[test]
    fn the_extremes_come_with_a_measured_temperature() {
        let both =
            config("forecast = \"meteo:api\"\n[[station]]\ndevice = \"z2m:0x1\"\n").sources();
        let station = device_with(
            "z2m:0x1",
            vec![spec(
                "temperature",
                Semantic::Temperature,
                Some(Unit::Celsius),
            )],
        );
        let forecast = device_with(
            "meteo:api",
            vec![spec(
                "temperature",
                Semantic::Temperature,
                Some(Unit::Celsius),
            )],
        );
        let id = || DeviceId::from("dehors:meteo");
        let instance = InstanceId::from(INSTANCE);
        let with = device(
            id(),
            &instance,
            &both,
            &layout(&both, &[Some(&station), Some(&forecast)]),
        );
        let max = with.point(MEASURED_MAX).unwrap();
        assert_eq!(max.unit, Some(Unit::Celsius));
        assert!(!max.access.write);
        assert!(with.point(MEASURED_MIN).is_some());
        // The station not there yet (or no station): no extremes.
        let without = device(
            id(),
            &instance,
            &both,
            &layout(&both, &[None, Some(&forecast)]),
        );
        assert!(without.point(MEASURED_MAX).is_none());
        let humid = device_with(
            "z2m:0x1",
            vec![spec("humidity", Semantic::Humidity, Some(Unit::Percent))],
        );
        let no_temperature = device(
            id(),
            &instance,
            &both,
            &layout(&both, &[Some(&humid), Some(&forecast)]),
        );
        assert!(no_temperature.point(MEASURED_MAX).is_none());
    }

    fn forecast_of(ts: u64) -> Candidate {
        candidate("meteo:api", 12.0, ts, 30)
    }

    #[test]
    fn the_device_says_its_sources() {
        let sources =
            config("forecast = \"meteo:api\"\n[[station]]\ndevice = \"z2m:0x1\"\n").sources();
        let station = device_with(
            "z2m:0x1",
            vec![spec(
                "temperature",
                Semantic::Temperature,
                Some(Unit::Celsius),
            )],
        );
        let out = layout(&sources, &[Some(&station), None]);
        let d = device(
            DeviceId::from("dehors:meteo"),
            &InstanceId::from(INSTANCE),
            &sources,
            &out,
        );
        let keys: Vec<&str> = d.points.iter().map(|p| &*p.key).collect();
        assert_eq!(
            keys,
            [
                "temperature",
                "temperature_source",
                "measured_max",
                "measured_min",
                "stations",
                "forecast"
            ]
        );
        assert!(d.points.iter().all(|p| !p.access.write));
        let source = d.point("temperature_source").unwrap();
        assert_eq!(source.semantic, Semantic::Config);
        assert_eq!(
            d.point("temperature").unwrap().semantic,
            Semantic::Temperature
        );
        assert_eq!(d.members.len(), 2);
        assert_eq!(d.model.as_deref(), Some("Dehors"));
        assert_eq!(ids(&sources, true), "z2m:0x1");
        assert_eq!(ids(&sources, false), "meteo:api");
    }
}
