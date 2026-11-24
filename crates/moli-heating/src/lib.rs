//! Heating: a room's own thermostat, kept by Moli. It reads the room's sensor
//! (a thermometer away from the radiator), follows a weekly schedule, and
//! gives each valve of the room the setpoint that makes it hold the room's
//! temperature, its own thermometer being by the radiator
//! ([`regulation`]).
//!
//! The house declares its rooms in `moli.toml`; Moli runs them as instance
//! `chauffage`, one device per room, `chauffage:<id>`, with the points of a
//! valve the dashboard already shows and edits:
//!
//! - `local_temperature`: the room's sensor (unknown when it went quiet).
//! - `occupied_heating_setpoint` (writable): what the room should be at. In
//!   `auto`, a change holds until the next step of the schedule, or for
//!   `hold_default_minutes` when set.
//! - `system_mode` (writable): `auto` follows the schedule, `heat` holds the
//!   setpoint, `off` gives the valves their lowest setpoint (frost guard).
//! - `weekly_schedule_<day>` (writable): `00:00/17 06:30/20 22:00/17`.
//! - `valve_correction`: the shift applied to the valves (°C), learned from
//!   the room's error ([`regulation`]); kept across restarts. 0 with
//!   `correction = false`.
//! - `valve_override` (writable): `none`, or a valve turned by hand that Moli
//!   leaves as it is for a while (`manual`, `airing`: [`hand`]); writing
//!   `none` gives the valves the room's setpoint again.
//! - `valve_override_until`: when that ends (RFC 3339), empty without one.
//! - `heat_demand`: the room asks for heat, its sensor under the setpoint
//!   by more than `hysteresis`, until it is over it by as much
//!   ([`regulation::demand`]); never in `off` nor with the sensor quiet.
//! - `boost` (writable, °C): added to the setpoint, the schedule's included,
//!   for `boost_minutes` (writable; `boost_default_minutes` when only `boost`
//!   is written), until `boost_until` (RFC 3339). Not in `off`.
//! - `hold_minutes` (writable): in `auto`, how long the setpoint holds before
//!   the schedule takes over again (0: until the next step), until
//!   `hold_until` (RFC 3339). See [`temporary`].
//!
//! ```toml
//! [[heating]]
//! id = "parents"                                # never rename it
//! name = "Chambre parentale"
//! sensor = "matter:<meter>/temperature"
//! valves = ["matter:<valve>/target_temperature"]  # each valve's setpoint
//! # regulation = "shift": the valves get the room's setpoint moved by the
//! #   learned shift; "none": as it is (a valve that already regulates on a
//! #   sensor of the room, as tado X does with its own; `correction = false`
//! #   says the same); "force": the room's setpoint plus `force_offset` while
//! #   the room asks for heat, minus it otherwise (a valve that regulates
//! #   badly on its own thermometer)
//! # force_offset = 4.0, hysteresis = 0.3
//! # min = 5.0, max = 25.0: the setpoints the room accepts
//! # initial = 19.0: the setpoint and schedule before anyone set one
//! # max_boost = 3.0, boost_default_minutes = 60, hold_default_minutes = 0
//! ```
//!
//! A valve is a writable setpoint in °C with bounds, on a device that is
//! neither an opening nor a lock; anything else is left alone. The valves'
//! devices are the room device's members, so an order to the room is judged
//! on their rooms too (protected rooms, quiet hours).
//!
//! Orders go through the hub as the house's own, like a scene's. The room's
//! sensor quiet, the shift holds and the valves are left as they are unless
//! the room's setpoint or mode changes; in `force`, the valves get the
//! room's setpoint as it is, once. A valve is written only on a
//! change of half a degree, at most every [`regulation::MIN_INTERVAL`] unless
//! the room changed, and never in the first minutes after a start, while
//! the drivers come back.

pub mod hand;
pub mod regulation;
pub mod schedule;
pub mod temporary;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::ensure;
use moli_core::{
    Access, Device, DeviceId, Kind, Origin, PointId, PointSpec, Semantic, Unit, Value, now_ms,
};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx, Hub};
use serde::Deserialize;

use regulation::{Shift, valve_heats, valve_setpoint, worth_sending};
use schedule::{DAYS, Step, format_day, parse_day, step_at, to_next_step};
use temporary::{Boost, MAX_MINUTES, Timed};

/// The instance that runs the rooms.
pub const INSTANCE: &str = "chauffage";

/// How often the schedule and the valves are looked at.
const TICK: Duration = Duration::from_secs(60);
/// A reading older than this is no longer the room's temperature.
const FRESH: Duration = Duration::from_secs(30 * 60);
/// No valve is written in this time after a start: the drivers come back
/// and the sensors are read again first.
const SETTLE: Duration = Duration::from_secs(3 * 60);
const MODES: [&str; 3] = ["off", "heat", "auto"];
const OVERRIDES: [&str; 3] = ["none", "manual", "airing"];
const MAX_ROOMS: usize = 32;
const MAX_VALVES: usize = 8;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Room {
    /// Stable id: the device is `chauffage:<id>`. Never rename it.
    pub id: String,
    /// The room's name, which is also its room in Moli.
    pub name: String,
    /// The room's temperature point (°C).
    pub sensor: String,
    /// Each valve's setpoint point (°C).
    pub valves: Vec<String>,
    /// Shift the valves' setpoint from the room's error (`false`: the valves
    /// get the room's setpoint as it is).
    #[serde(default = "default_correction")]
    pub correction: bool,
    /// How the valves are driven ([`Regulation`]); unset: `shift`, or
    /// `none` with `correction = false`.
    #[serde(default)]
    pub regulation: Option<Regulation>,
    /// `force`: how far over or under the room's setpoint the valves are
    /// sent (°C).
    #[serde(default = "default_force_offset")]
    pub force_offset: f64,
    /// The room asks for heat under its setpoint by more than this, and no
    /// longer over it by more than this (°C).
    #[serde(default = "default_hysteresis")]
    pub hysteresis: f64,
    #[serde(default = "default_min")]
    pub min: f64,
    #[serde(default = "default_max")]
    pub max: f64,
    /// The setpoint and every day's schedule before anyone set them.
    #[serde(default = "default_initial")]
    pub initial: f64,
    /// The highest `boost` (°C), on the half degree.
    #[serde(default = "default_max_boost")]
    pub max_boost: f64,
    /// How long a boost lasts when only `boost` is written.
    #[serde(default = "default_boost_minutes")]
    pub boost_default_minutes: u32,
    /// In `auto`, how long a setpoint changed by hand holds (0: until the
    /// next step of the schedule).
    #[serde(default)]
    pub hold_default_minutes: u32,
}

/// How a room's valves are given their setpoint.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Regulation {
    /// The room's setpoint, moved by a shift learned from the room's error
    /// ([`regulation::Shift`]).
    Shift,
    /// The room's setpoint plus [`Room::force_offset`] while the room asks
    /// for heat, minus it otherwise: the valve opens wide or shuts.
    Force,
    /// The room's setpoint as it is.
    #[serde(rename = "none")]
    AsIs,
}

impl Room {
    /// The room's [`Regulation`], `correction = false` included.
    #[must_use]
    pub fn strategy(&self) -> Regulation {
        self.regulation.unwrap_or(if self.correction {
            Regulation::Shift
        } else {
            Regulation::AsIs
        })
    }
}

fn default_correction() -> bool {
    true
}
fn default_force_offset() -> f64 {
    4.0
}
fn default_hysteresis() -> f64 {
    0.3
}
fn default_min() -> f64 {
    5.0
}
fn default_max() -> f64 {
    25.0
}
fn default_initial() -> f64 {
    19.0
}
fn default_max_boost() -> f64 {
    3.0
}
fn default_boost_minutes() -> u32 {
    60
}

fn is_point(text: &str) -> bool {
    PointId::from(text)
        .split()
        .is_some_and(|(d, _)| d.as_str().contains(':'))
}

/// Checks the rooms: ids, points, bounds.
///
/// # Errors
///
/// The first problem found, in words.
pub fn check(rooms: &[Room]) -> anyhow::Result<()> {
    ensure!(
        rooms.len() <= MAX_ROOMS,
        "heating: {MAX_ROOMS} rooms at most"
    );
    let mut seen = HashSet::new();
    for room in rooms {
        let id = &room.id;
        ensure!(
            !id.is_empty() && id.len() <= 64 && !id.contains([':', '/']),
            "heating room id {id:?}: non-empty, without ':' nor '/'"
        );
        ensure!(seen.insert(id), "heating room id {id:?} twice");
        ensure!(
            !room.name.trim().is_empty() && room.name.len() <= 60,
            "heating room {id}: a name, 60 characters at most"
        );
        ensure!(
            is_point(&room.sensor),
            "heating room {id}: sensor {:?} is not a point (device/key)",
            room.sensor
        );
        ensure!(
            !room.valves.is_empty() && room.valves.len() <= MAX_VALVES,
            "heating room {id}: 1 to {MAX_VALVES} valves"
        );
        for valve in &room.valves {
            ensure!(
                is_point(valve),
                "heating room {id}: valve {valve:?} is not a point (device/key)"
            );
            // The rooms' own points: the heating would wait for itself.
            ensure!(
                !valve.starts_with(&format!("{INSTANCE}:")),
                "heating room {id}: valve {valve:?} is a heating room, not a valve"
            );
        }
        ensure!(
            (5.0..=30.0).contains(&room.min)
                && (5.0..=30.0).contains(&room.max)
                && room.min < room.max,
            "heating room {id}: 5 <= min < max <= 30"
        );
        ensure!(
            parse_day(&format_day(&[day_of(room.initial)]), room.min, room.max).is_some(),
            "heating room {id}: initial within min..max, on the half degree"
        );
        ensure!(
            room.correction || room.regulation.is_none(),
            "heating room {id}: correction = false or regulation, not both"
        );
        ensure!(
            (0.5..=10.0).contains(&room.force_offset),
            "heating room {id}: 0.5 <= force_offset <= 10"
        );
        ensure!(
            (0.0..=2.0).contains(&room.hysteresis),
            "heating room {id}: 0 <= hysteresis <= 2"
        );
        ensure!(
            room.max_boost >= 0.5 && temporary::boost_ok(room.max_boost, 10.0),
            "heating room {id}: max_boost from 0.5 to 10, on the half degree"
        );
        ensure!(
            (1..=MAX_MINUTES).contains(&room.boost_default_minutes),
            "heating room {id}: boost_default_minutes from 1 to {MAX_MINUTES}"
        );
        ensure!(
            room.hold_default_minutes <= MAX_MINUTES,
            "heating room {id}: hold_default_minutes from 0 to {MAX_MINUTES}"
        );
    }
    Ok(())
}

fn day_of(temp: f64) -> Step {
    Step { minute: 0, temp }
}

#[derive(Debug)]
pub struct Heating {
    hub: Hub,
    rooms: Vec<Room>,
    zone: jiff::tz::TimeZone,
    settle: Duration,
    /// [`regulation::URGENT_FLOOR`], [`regulation::MIN_INTERVAL`],
    /// [`TICK`], [`hand::HAND_HOLD`], [`hand::AIRING_HOLD`], [`FRESH`], and a
    /// minute of a boost or a hold: shorter in the tests.
    floor: Duration,
    interval: Duration,
    tick: Duration,
    hand_hold: Duration,
    airing_hold: Duration,
    fresh: Duration,
    minute: Duration,
}

impl Heating {
    /// `zone`: the house's time zone (the schedule follows local time).
    #[must_use]
    pub fn new(hub: Hub, rooms: Vec<Room>, zone: jiff::tz::TimeZone) -> Self {
        Self {
            hub,
            rooms,
            zone,
            settle: SETTLE,
            floor: regulation::URGENT_FLOOR,
            interval: regulation::MIN_INTERVAL,
            tick: TICK,
            hand_hold: hand::HAND_HOLD,
            airing_hold: hand::AIRING_HOLD,
            fresh: FRESH,
            minute: Duration::from_secs(60),
        }
    }
}

/// What a valve's point is, as the hub knows it.
#[derive(Debug, PartialEq)]
enum Valve {
    /// A setpoint in °C, its bounds, and its step when it says.
    Writable((f64, f64), Option<f64>),
    /// Its device not there yet (its driver starts).
    Unknown,
    /// Something else: never written.
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    Heat,
    Auto,
}

impl Mode {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "off" => Some(Self::Off),
            "heat" => Some(Self::Heat),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

struct ValveRun {
    /// The last write tried (refused or not: a sleeping valve is not asked
    /// again every minute).
    sent: Option<Instant>,
    /// The room's setpoint or mode changed since this valve was last
    /// written: kept until it is (a valve not known yet keeps it).
    urgent: bool,
    /// Its point said once not to be a valve's setpoint.
    refused: bool,
    /// Its setpoint at the last look, and what Moli wrote lately (and was
    /// told was taken): a change that is neither was made by hand.
    seen: Option<f64>,
    written: hand::Written,
    /// Turned by hand: left as it is until then.
    hold: Option<hand::Hold>,
}

struct RoomRun {
    id: DeviceId,
    mode: Mode,
    setpoint: f64,
    week: Vec<Vec<Step>>,
    /// The step in force (day, index), so a new one is applied once.
    step: Option<(usize, usize)>,
    valves: Vec<ValveRun>,
    /// Learned from the room's error, and when it was last read.
    shift: Shift,
    read: Option<Instant>,
    /// The room asks for heat; none in `off` or with the sensor quiet.
    demand: Option<bool>,
    boost: Option<Boost>,
    /// In `auto`, the setpoint held until then, whatever the schedule says.
    setpoint_hold: Option<Timed>,
}

impl RoomRun {
    /// The setpoint or mode changed: each valve has it soon, whatever was
    /// turned on it by hand.
    fn changed(&mut self) {
        for valve in &mut self.valves {
            valve.urgent = true;
            valve.hold = None;
        }
    }

    /// What the room should be at: its setpoint and its boost, within its
    /// bounds.
    fn target(&self, max: f64) -> f64 {
        (self.setpoint + self.boost.map_or(0.0, |b| b.by)).min(max)
    }

    /// The hold the room shows: airing first, then by hand; and its end (the
    /// latest of its kind).
    fn hold(&self) -> Option<hand::Hold> {
        let holds = self.valves.iter().filter_map(|v| v.hold);
        let kind = holds
            .clone()
            .map(|h| h.kind)
            .min_by_key(|k| *k != hand::Kind::Airing)?;
        let until_ms = holds.filter(|h| h.kind == kind).map(|h| h.until_ms).max()?;
        Some(hand::Hold { kind, until_ms })
    }
}

/// The local day (Monday 0) and minute of the day.
fn local(zone: &jiff::tz::TimeZone) -> (usize, u16) {
    let now = jiff::Timestamp::now().to_zoned(zone.clone());
    let day = usize::try_from(now.weekday().to_monday_zero_offset()).unwrap_or(0);
    let minute = u16::from(now.hour().unsigned_abs()) * 60 + u16::from(now.minute().unsigned_abs());
    (day, minute)
}

fn numeric(
    key: &str,
    label: String,
    write: bool,
    bounds: Option<(f64, f64)>,
    semantic: Semantic,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Numeric {
            min: bounds.map(|b| b.0),
            max: bounds.map(|b| b.1),
            step: bounds.map(|_| 0.5),
        },
        access: Access { read: true, write },
        unit: Some(Unit::Celsius),
        semantic,
    }
}

/// A writable length in whole minutes (0 to [`MAX_MINUTES`]).
fn minutes(key: &str, label: String) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Numeric {
            min: Some(0.0),
            max: Some(f64::from(MAX_MINUTES)),
            step: Some(1.0),
        },
        access: Access {
            read: true,
            write: true,
        },
        unit: Some(Unit::Minute),
        semantic: Semantic::Other,
    }
}

/// A read-only end (RFC 3339).
fn until(key: &str, label: String) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Text,
        access: Access {
            read: true,
            write: false,
        },
        unit: None,
        semantic: Semantic::Other,
    }
}

/// The room's boost and setpoint held ([`temporary`]).
fn temporary_points(room: &Room) -> [PointSpec; 5] {
    [
        numeric(
            "boost",
            moli_i18n::tr!("pilotes.chauffage.boost"),
            true,
            Some((0.0, room.max_boost)),
            Semantic::Other,
        ),
        minutes(
            "boost_minutes",
            moli_i18n::tr!("pilotes.chauffage.boost_duree"),
        ),
        until("boost_until", moli_i18n::tr!("pilotes.chauffage.boost_fin")),
        minutes("hold_minutes", moli_i18n::tr!("pilotes.chauffage.maintien")),
        until(
            "hold_until",
            moli_i18n::tr!("pilotes.chauffage.maintien_fin"),
        ),
    ]
}

/// The devices of the room's valves, each once.
fn valve_devices(room: &Room) -> Vec<DeviceId> {
    let mut devices: Vec<DeviceId> = Vec::new();
    for valve in &room.valves {
        if let Some((device, _)) = PointId::from(valve.as_str()).split()
            && !devices.contains(&device)
        {
            devices.push(device);
        }
    }
    devices
}

fn device(ctx: &DriverCtx, room: &Room) -> Device {
    let mut points = vec![
        numeric(
            "local_temperature",
            moli_i18n::tr!("pilotes.chauffage.temperature"),
            false,
            None,
            Semantic::Temperature,
        ),
        numeric(
            "occupied_heating_setpoint",
            moli_i18n::tr!("pilotes.chauffage.consigne"),
            true,
            Some((room.min, room.max)),
            Semantic::Other,
        ),
        PointSpec {
            key: "system_mode".into(),
            label: moli_i18n::tr!("pilotes.chauffage.mode").into(),
            kind: Kind::Enum {
                values: MODES.iter().map(|m| (*m).into()).collect(),
            },
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: Semantic::Other,
        },
    ];
    for day in DAYS {
        points.push(PointSpec {
            key: format!("weekly_schedule_{day}").into(),
            label: moli_i18n::tr!(
                "pilotes.chauffage.programme",
                jour = moli_i18n::tr(&format!("pilotes.chauffage.jour.{day}"))
            )
            .into(),
            kind: Kind::Text,
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: Semantic::Config,
        });
    }
    points.push(PointSpec {
        key: "valve_override".into(),
        label: moli_i18n::tr!("pilotes.chauffage.reglage_vanne").into(),
        kind: Kind::Enum {
            values: OVERRIDES.iter().map(|m| (*m).into()).collect(),
        },
        access: Access {
            read: true,
            write: true,
        },
        unit: None,
        semantic: Semantic::Other,
    });
    points.push(PointSpec {
        key: "valve_override_until".into(),
        label: moli_i18n::tr!("pilotes.chauffage.reglage_vanne_fin").into(),
        kind: Kind::Text,
        access: Access {
            read: true,
            write: false,
        },
        unit: None,
        semantic: Semantic::Other,
    });
    points.push(PointSpec {
        key: "heat_demand".into(),
        label: moli_i18n::tr!("pilotes.chauffage.demande").into(),
        kind: Kind::Binary,
        access: Access {
            read: true,
            write: false,
        },
        unit: None,
        semantic: Semantic::Other,
    });
    points.extend(temporary_points(room));
    points.push(numeric(
        "valve_correction",
        moli_i18n::tr!("pilotes.chauffage.correction"),
        false,
        None,
        Semantic::Other,
    ));
    Device {
        id: ctx.device_id(&room.id),
        instance: ctx.instance().clone(),
        native_name: room.name.as_str().into(),
        manufacturer: Some("Moli".into()),
        model: Some(moli_i18n::tr!("pilotes.chauffage.modele").into()),
        description: None,
        native_room: Some(room.name.as_str().into()),
        // The valves' devices: an order to the room is judged on their rooms
        // too (protected rooms, quiet hours), since the room writes them.
        members: valve_devices(room),
        points,
    }
}

/// What the room had before a restart, or its first settings.
fn restore(ctx: &DriverCtx, room: &Room) -> RoomRun {
    let id = ctx.device_id(&room.id);
    let mode = match ctx.current(&id, "system_mode") {
        Some(Value::Text(t)) => Mode::parse(&t).unwrap_or(Mode::Auto),
        _ => Mode::Auto,
    };
    let setpoint = ctx
        .current(&id, "occupied_heating_setpoint")
        .and_then(|v| v.as_f64())
        .filter(|t| (room.min..=room.max).contains(t))
        .unwrap_or(room.initial);
    let week = DAYS
        .iter()
        .map(
            |day| match ctx.current(&id, &format!("weekly_schedule_{day}")) {
                Some(Value::Text(t)) => parse_day(&t, room.min, room.max),
                _ => None,
            },
        )
        .map(|steps| steps.unwrap_or_else(|| vec![day_of(room.initial)]))
        .collect::<Vec<_>>();
    ctx.set_state(&id, "system_mode", Value::from(mode_name(mode)));
    ctx.set_state(&id, "occupied_heating_setpoint", Value::Float(setpoint));
    for (day, steps) in DAYS.iter().zip(&week) {
        ctx.set_state(
            &id,
            &format!("weekly_schedule_{day}"),
            Value::from(format_day(steps).as_str()),
        );
    }
    // The shift learned before a restart, not learned again from zero.
    let shift = if room.strategy() == Regulation::Shift {
        Shift::from(
            ctx.current(&id, "valve_correction")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
        )
    } else {
        Shift::default()
    };
    ctx.set_state(&id, "valve_correction", Value::Float(shift.get()));
    // A hold is not kept across a restart: the valves get the room's again.
    show_hold(ctx, &id, None);
    ctx.set_state(&id, "heat_demand", Value::Bool(false));
    // A boost or a setpoint held is, until its end (gone by: ended at the
    // first look, and said so).
    let minutes = |key: &str| ctx.current(&id, key).as_ref().and_then(temporary::minutes);
    let until = |key: &str| temporary::read(ctx.current(&id, key).as_ref());
    let boost = ctx
        .current(&id, "boost")
        .and_then(|v| v.as_f64())
        .filter(|b| *b > 0.0 && temporary::boost_ok(*b, room.max_boost) && mode != Mode::Off)
        .zip(until("boost_until"))
        .map(|(by, until_ms)| Boost {
            by,
            timed: Timed {
                minutes: minutes("boost_minutes").unwrap_or(0),
                until_ms,
            },
        });
    let setpoint_hold = until("hold_until")
        .filter(|_| mode == Mode::Auto)
        .map(|until_ms| Timed {
            minutes: minutes("hold_minutes").unwrap_or(0),
            until_ms,
        });
    ctx.set_availability(&id, true);
    let run = RoomRun {
        id,
        mode,
        setpoint,
        week,
        step: None,
        valves: room
            .valves
            .iter()
            .map(|_| ValveRun {
                sent: None,
                urgent: false,
                refused: false,
                seen: None,
                written: hand::Written::default(),
                hold: None,
            })
            .collect(),
        shift,
        read: None,
        demand: None,
        boost,
        setpoint_hold,
    };
    show_temporary(ctx, &run);
    run
}

fn text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(t) => Some(t),
        _ => None,
    }
}

/// The room's `valve_override` and `valve_override_until`.
fn show_hold(ctx: &DriverCtx, id: &DeviceId, hold: Option<hand::Hold>) {
    let kind = hold.map_or("none", |h| h.kind.name());
    let until = hold
        .and_then(|h| jiff::Timestamp::from_millisecond(i64::try_from(h.until_ms).ok()?).ok())
        .map_or(Value::Null, |t| Value::from(t.to_string().as_str()));
    ctx.set_state(id, "valve_override", Value::from(kind));
    ctx.set_state(id, "valve_override_until", until);
}

/// The room's boost and setpoint held, and their ends.
fn show_temporary(ctx: &DriverCtx, run: &RoomRun) {
    let id = &run.id;
    let minutes = |t: Option<Timed>| Value::Int(t.map_or(0, |t| i64::from(t.minutes)));
    let boost = run.boost.map(|b| b.timed);
    ctx.set_state(id, "boost", Value::Float(run.boost.map_or(0.0, |b| b.by)));
    ctx.set_state(id, "boost_minutes", minutes(boost));
    ctx.set_state(
        id,
        "boost_until",
        temporary::show(boost.map(|t| t.until_ms)),
    );
    ctx.set_state(id, "hold_minutes", minutes(run.setpoint_hold));
    ctx.set_state(
        id,
        "hold_until",
        temporary::show(run.setpoint_hold.map(|t| t.until_ms)),
    );
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Off => "off",
        Mode::Heat => "heat",
        Mode::Auto => "auto",
    }
}

/// Whether the room asks for heat. In `force`, a change is the valves'
/// to have soon; the sensor quiet, they get the room's setpoint as it is.
fn demand(
    ctx: &DriverCtx,
    room: &Room,
    run: &mut RoomRun,
    temperature: Option<f64>,
    strategy: Regulation,
) {
    let demand = match temperature {
        Some(room_t) if run.mode != Mode::Off => Some(regulation::demand(
            run.demand,
            run.target(room.max) - room_t,
            room.hysteresis,
        )),
        _ => None,
    };
    if demand == run.demand {
        return;
    }
    if strategy == Regulation::Force {
        for valve in &mut run.valves {
            valve.urgent = true;
        }
        if demand.is_none() && run.mode != Mode::Off {
            tracing::warn!(room = %room.id, "heating: room sensor quiet, the valves get the room's setpoint as it is");
        }
    }
    run.demand = demand;
    ctx.set_state(&run.id, "heat_demand", Value::Bool(demand == Some(true)));
}

/// The setpoint a valve within `bounds` is given: the room's `mode`,
/// `setpoint`, `demand` and learned `shift`.
fn target(
    room: &Room,
    strategy: Regulation,
    (mode, setpoint, demand): (Mode, f64, Option<bool>),
    shift: f64,
    bounds: (f64, f64),
) -> f64 {
    match (mode, strategy, demand) {
        (Mode::Off, ..) => bounds.0,
        (_, Regulation::Force, Some(demand)) => {
            regulation::forced_setpoint(setpoint, room.force_offset, demand, bounds)
        }
        _ => valve_setpoint(setpoint, shift, bounds),
    }
}

impl Heating {
    /// A temperature that is still the room's: recent, from a device that
    /// answers.
    fn fresh(&self, point: &str) -> Option<f64> {
        let point = PointId::from(point);
        let (device, _) = point.split()?;
        if self
            .hub
            .device(&device)
            .is_some_and(|d| d.online == Some(false))
        {
            return None;
        }
        let sample = self.hub.state(&point)?;
        let value = sample.value.as_f64().filter(|t| t.is_finite())?;
        let age = u128::from(now_ms().saturating_sub(sample.ts));
        (age <= self.fresh.as_millis()).then_some(value)
    }

    /// Whether one of the room's valves still heats (none known: yes, as
    /// before): the room's excess lowers the shift only then.
    fn heating(&self, room: &Room, room_t: f64) -> bool {
        let mut known = false;
        for valve in &room.valves {
            let point = PointId::from(valve.as_str());
            let Some(held) = self.hub.state(&point).and_then(|s| s.value.as_f64()) else {
                continue;
            };
            known = true;
            if valve_heats(held, self.valve_thermometer(&point), room_t) {
                return true;
            }
        }
        !known
    }

    /// A valve's own thermometer: the read-only temperature in °C on the
    /// device of its setpoint, if fresh.
    fn valve_thermometer(&self, setpoint: &PointId) -> Option<f64> {
        let (device, key) = setpoint.split()?;
        let view = self.hub.device(&device)?;
        let spec = view.device.points.iter().find(|p| {
            p.key.as_ref() != key
                && !p.access.write
                && p.semantic == Semantic::Temperature
                && p.unit == Some(Unit::Celsius)
        })?;
        self.fresh(PointId::new(&device, spec.key.as_ref()).as_str())
    }

    /// The valve's setpoint bounds, if Moli may write it: a writable
    /// temperature in °C with its bounds, on a device that is not an opening
    /// nor a lock (a shutter's position, a dimmer, a volume never are).
    fn bounds(&self, point: &str) -> Valve {
        let point = PointId::from(point);
        let Some((device, key)) = point.split() else {
            return Valve::Other;
        };
        let Some(view) = self.hub.device(&device) else {
            return Valve::Unknown;
        };
        let points = &view.device.points;
        let Some(spec) = points.iter().find(|p| p.key.as_ref() == key) else {
            return Valve::Unknown;
        };
        let opening = points
            .iter()
            .any(|p| matches!(p.semantic, Semantic::Position | Semantic::Lock));
        match spec.kind {
            Kind::Numeric {
                min: Some(min),
                max: Some(max),
                step,
            } if spec.access.write
                && min < max
                && spec.unit == Some(Unit::Celsius)
                && !matches!(spec.semantic, Semantic::Control | Semantic::Lock)
                && !opening =>
            {
                Valve::Writable((min, max), step.filter(|s| s.is_finite() && *s > 0.0))
            }
            _ => Valve::Other,
        }
    }

    /// In `auto`, a new step of the schedule becomes the room's setpoint.
    fn follow(&self, ctx: &DriverCtx, run: &mut RoomRun) {
        if run.mode != Mode::Auto {
            return;
        }
        let (day, minute) = local(&self.zone);
        let Some((index, temp)) = step_at(&run.week[day], minute) else {
            return;
        };
        if run.step != Some((day, index)) {
            run.step = Some((day, index));
            // Held: the step is applied when the hold ends.
            if run.setpoint_hold.is_some() {
                return;
            }
            run.setpoint = temp;
            run.changed();
            ctx.set_state(&run.id, "occupied_heating_setpoint", Value::Float(temp));
        }
    }

    /// How long before the next step of the schedule, when the room follows it.
    fn to_next_step(&self, run: &RoomRun) -> Option<Duration> {
        if run.mode != Mode::Auto {
            return None;
        }
        let now = jiff::Timestamp::now().to_zoned(self.zone.clone());
        let (day, minute) = local(&self.zone);
        let minutes = u64::from(to_next_step(&run.week[day], minute));
        let seconds = u64::from(now.second().unsigned_abs());
        Some(Duration::from_secs((minutes * 60).saturating_sub(seconds)))
    }

    /// How long before the room's own target changes: its setpoint held
    /// ends, or else the next step of the schedule; or its boost ends.
    fn to_next_change(&self, run: &RoomRun) -> Option<Duration> {
        let now = now_ms();
        let setpoint = match run.setpoint_hold {
            Some(hold) => Some(hold.left(now)),
            None => self.to_next_step(run),
        };
        let boost = run.boost.map(|b| b.timed.left(now));
        setpoint.into_iter().chain(boost).min()
    }

    /// Journals a change the room made on its own, as « Heating · <room> ».
    fn journal(&self, room: &Room, run: &RoomRun, key: &str, from: Value, to: Value) {
        let actor = moli_i18n::tr!("pilotes.chauffage.acteur", piece = room.name);
        self.hub.record_setting(
            Origin::System,
            Some(actor),
            PointId::new(&run.id, key).as_str(),
            Some(from),
            to,
        );
    }

    /// Ends the boost; journaled unless asked for (the order already is).
    fn end_boost(&self, room: &Room, run: &mut RoomRun, why: &str) {
        if let Some(boost) = run.boost.take() {
            run.changed();
            if why != "asked" {
                self.journal(
                    room,
                    run,
                    "boost",
                    Value::Float(boost.by),
                    Value::Float(0.0),
                );
            }
            tracing::info!(room = %room.id, by = boost.by, why, "heating: boost over");
        }
    }

    fn end_setpoint_hold(&self, ctx: &DriverCtx, room: &Room, run: &mut RoomRun, why: &str) {
        if let Some(hold) = run.setpoint_hold.take() {
            let minutes = Value::Int(i64::from(hold.minutes));
            self.journal(room, run, "hold_minutes", minutes, Value::Int(0));
            tracing::info!(room = %room.id, why, "heating: setpoint hold over, the schedule again");
            run.step = None;
            self.follow(ctx, run);
        }
    }

    /// A boost or a setpoint held whose end has come.
    fn expire(&self, ctx: &DriverCtx, room: &Room, run: &mut RoomRun) {
        let now = now_ms();
        if run.boost.is_some_and(|b| b.timed.due(now)) {
            self.end_boost(room, run, "due");
        }
        if run.setpoint_hold.is_some_and(|h| h.due(now)) {
            self.end_setpoint_hold(ctx, room, run, "due");
        }
    }

    /// A boost of `by` °C: the one in force changed, or a new one for
    /// `boost_default_minutes`; 0 ends it.
    fn set_boost(&self, room: &Room, run: &mut RoomRun, by: f64) {
        if by <= 0.0 {
            self.end_boost(room, run, "asked");
            return;
        }
        run.changed();
        if let Some(boost) = &mut run.boost {
            boost.by = by;
            return;
        }
        let minutes = room.boost_default_minutes;
        let timed = Timed::start(minutes, now_ms(), self.minute);
        run.boost = Some(Boost { by, timed });
        self.journal(
            room,
            run,
            "boost_minutes",
            Value::Int(0),
            Value::Int(i64::from(minutes)),
        );
        tracing::info!(room = %room.id, by, minutes, "heating: boost");
    }

    /// A setpoint changed by hand in `auto`: held for `hold_default_minutes`
    /// unless already held (its end kept, whatever the order of a scene).
    fn hold_by_default(&self, room: &Room, run: &mut RoomRun) {
        let minutes = room.hold_default_minutes;
        if run.mode != Mode::Auto || run.setpoint_hold.is_some() || minutes == 0 {
            return;
        }
        run.setpoint_hold = Some(Timed::start(minutes, now_ms(), self.minute));
        self.journal(
            room,
            run,
            "hold_minutes",
            Value::Int(0),
            Value::Int(i64::from(minutes)),
        );
        tracing::info!(room = %room.id, minutes, "heating: setpoint held");
    }

    /// A valve's setpoint moved by someone else than Moli: held as it is
    /// ([`hand`]), or the room's again when it is turned up after airing.
    fn turned_by_hand(
        &self,
        room: &Room,
        run_next: Option<Duration>,
        state: &mut ValveRun,
        (value, valve_min): (f64, f64),
        temperature: Option<f64>,
    ) {
        let airing = hand::airing(value, valve_min, temperature);
        if let Some(kind) = hand::after_turn(state.hold.map(|h| h.kind), airing) {
            let until_ms = hand::until(kind, now_ms(), run_next, self.hand_hold, self.airing_hold);
            state.hold = Some(hand::Hold { kind, until_ms });
            tracing::info!(room = %room.id, value, kind = kind.name(), "heating: valve turned by hand, left as it is for a while");
        } else {
            state.hold = None;
            state.urgent = true;
            tracing::info!(room = %room.id, value, "heating: valve turned up after airing, the room's setpoint again");
        }
    }

    /// The room's error moves the shift, while heating and the room known,
    /// and `learning` (`shift`, no valve held by hand: the room is not at
    /// its setpoint then).
    fn learn(&self, room: &Room, run: &mut RoomRun, temperature: Option<f64>, learning: bool) {
        if let Some(room_t) = temperature
            && learning
            && run.mode != Mode::Off
        {
            let now = Instant::now();
            let elapsed = run.read.map_or(Duration::ZERO, |t| now - t);
            let heating = self.heating(room, room_t);
            run.shift
                .update(run.target(room.max) - room_t, elapsed, heating);
            run.read = Some(now);
        } else {
            run.read = None;
        }
    }

    /// Ends what is due, then regulates the room.
    async fn look(&self, ctx: &DriverCtx, room: &Room, run: &mut RoomRun, settled: bool) {
        self.expire(ctx, room, run);
        show_temporary(ctx, run);
        self.regulate(ctx, room, run, settled).await;
    }

    /// Reads the room and its valves, and writes the valves that need it.
    async fn regulate(&self, ctx: &DriverCtx, room: &Room, run: &mut RoomRun, settled: bool) {
        let temperature = self.fresh(&room.sensor);
        ctx.set_state(
            &run.id,
            "local_temperature",
            temperature.map_or(Value::Null, Value::Float),
        );
        let next_step = self.to_next_change(run);
        // Each valve's hold: one turned by hand since the last look, one over.
        for (valve, state) in room.valves.iter().zip(&mut run.valves) {
            if state.hold.is_some_and(|h| now_ms() >= h.until_ms) {
                state.hold = None;
                state.urgent = true;
            }
            let Valve::Writable((min, _), step) = self.bounds(valve) else {
                continue;
            };
            let Some(current) = self
                .hub
                .state(&PointId::from(valve.as_str()))
                .and_then(|s| s.value.as_f64())
                .filter(|v| v.is_finite())
            else {
                continue;
            };
            if settled && hand::turned(state.seen, current, &mut state.written, step) {
                self.turned_by_hand(room, next_step, state, (current, min), temperature);
            }
            state.seen = Some(current);
        }
        let hold = run.hold();
        show_hold(ctx, &run.id, hold);
        let strategy = room.strategy();
        demand(ctx, room, run, temperature, strategy);
        self.learn(
            room,
            run,
            temperature,
            hold.is_none() && strategy == Regulation::Shift,
        );
        let shift = run.shift.get();
        let aim = run.target(room.max);
        for (valve, state) in room.valves.iter().zip(&mut run.valves) {
            // Kept until the valve may be written: a change made while Moli
            // starts, or before its valve is known, is not lost.
            if !settled || state.hold.is_some() {
                continue;
            }
            let (bounds, step) = match self.bounds(valve) {
                Valve::Writable(bounds, step) => (bounds, step),
                Valve::Unknown => continue,
                Valve::Other => {
                    if !std::mem::replace(&mut state.refused, true) {
                        tracing::warn!(room = %room.id, %valve, "heating: not a valve's setpoint (°C), left alone");
                    }
                    continue;
                }
            };
            let urgent = state.urgent;
            // The room unknown: nothing is changed unless asked.
            if temperature.is_none() && !urgent {
                continue;
            }
            let next = target(room, strategy, (run.mode, aim, run.demand), shift, bounds);
            // On the valve's own step, coarser than the half degree: what it holds.
            let next = regulation::on_step(next, step, bounds);
            let point = PointId::from(valve.as_str());
            let current = self.hub.state(&point).and_then(|s| s.value.as_f64());
            let since = state.sent.map(|t| t.elapsed());
            if !worth_sending(current, next, since, urgent, (self.floor, self.interval)) {
                // Nothing to change: done. Too soon after the last write:
                // kept for the next look.
                if !regulation::too_soon(since, self.floor) {
                    state.urgent = false;
                }
                continue;
            }
            state.urgent = false;
            state.sent = Some(Instant::now());
            let actor = moli_i18n::tr!("pilotes.chauffage.acteur", piece = room.name);
            match self
                .hub
                .command(&point, Value::Float(next), Origin::System, Some(actor))
                .await
            {
                // Its report coming back is Moli's, not a hand's.
                Ok(()) => state.written.sent(next),
                Err(e) => {
                    tracing::warn!(room = %room.id, error = %e, "heating: valve setpoint refused");
                }
            }
        }
        ctx.set_state(
            &run.id,
            "valve_correction",
            Value::Float((shift * 10.0).round() / 10.0),
        );
    }

    /// An order to a room's own points (already checked against them by the
    /// hub, except a schedule's text).
    fn command(&self, ctx: &DriverCtx, room: &Room, run: &mut RoomRun, command: CommandRequest) {
        // Given up by the hub (told the caller it failed): not applied after all.
        if command.abandoned() {
            return;
        }
        let key = command.key.as_ref();
        let result = if key == "occupied_heating_setpoint" {
            match command.value.as_f64() {
                Some(t) if (room.min..=room.max).contains(&t) => {
                    run.setpoint = regulation::half_degree(t).clamp(room.min, room.max);
                    run.changed();
                    ctx.set_state(&run.id, key, Value::Float(run.setpoint));
                    self.hold_by_default(room, run);
                    Ok(())
                }
                _ => Err(moli_i18n::tr!("pilotes.chauffage.consigne_refusee")),
            }
        } else if key == "system_mode" {
            match text(&command.value).and_then(Mode::parse) {
                Some(mode) => {
                    self.end_boost(room, run, "mode");
                    if let Some(hold) = run.setpoint_hold.take() {
                        let minutes = Value::Int(i64::from(hold.minutes));
                        self.journal(room, run, "hold_minutes", minutes, Value::Int(0));
                    }
                    run.mode = mode;
                    run.step = None;
                    run.changed();
                    ctx.set_state(&run.id, key, Value::from(mode_name(mode)));
                    self.follow(ctx, run);
                    Ok(())
                }
                None => Err(moli_i18n::tr!("pilotes.chauffage.mode_refuse")),
            }
        } else if key == "valve_override" {
            // Only « none »: the valves get the room's setpoint again.
            if text(&command.value) == Some("none") {
                run.changed();
                show_hold(ctx, &run.id, None);
                Ok(())
            } else {
                Err(moli_i18n::tr!("pilotes.chauffage.reglage_vanne_refuse"))
            }
        } else if let Some(result) = self.temporary_command(room, run, key, &command.value) {
            result
        } else if let Some(day) = key
            .strip_prefix("weekly_schedule_")
            .and_then(|d| DAYS.iter().position(|x| *x == d))
        {
            match text(&command.value).and_then(|t| parse_day(t, room.min, room.max)) {
                Some(steps) => {
                    ctx.set_state(&run.id, key, Value::from(format_day(&steps).as_str()));
                    run.week[day] = steps;
                    // Today's day changed: its step in force is applied again.
                    run.step = None;
                    self.follow(ctx, run);
                    Ok(())
                }
                None => Err(moli_i18n::tr!("pilotes.chauffage.programme_refuse")),
            }
        } else {
            Err(moli_i18n::tr!("pilotes.chauffage.point_inconnu"))
        };
        command.reply(result);
    }

    /// An order to the room's boost or setpoint held; `None` for another point.
    fn temporary_command(
        &self,
        room: &Room,
        run: &mut RoomRun,
        key: &str,
        value: &Value,
    ) -> Option<Result<(), String>> {
        Some(match key {
            "boost" => match value.as_f64() {
                Some(by) if temporary::boost_ok(by, room.max_boost) => {
                    if by > 0.0 && run.mode == Mode::Off {
                        Err(moli_i18n::tr!("pilotes.chauffage.boost_coupe"))
                    } else {
                        self.set_boost(room, run, by);
                        Ok(())
                    }
                }
                _ => Err(moli_i18n::tr!(
                    "pilotes.chauffage.boost_refuse",
                    max = room.max_boost
                )),
            },
            "boost_minutes" => match temporary::minutes(value) {
                Some(0) => {
                    self.end_boost(room, run, "asked");
                    Ok(())
                }
                Some(minutes) => match &mut run.boost {
                    Some(boost) => {
                        boost.timed = Timed::start(minutes, now_ms(), self.minute);
                        Ok(())
                    }
                    None => Err(moli_i18n::tr!("pilotes.chauffage.boost_absent")),
                },
                None => Err(moli_i18n::tr!("pilotes.chauffage.duree_refusee")),
            },
            "hold_minutes" => match temporary::minutes(value) {
                Some(0) => {
                    // The setpoint holds until the next step, as without a hold.
                    run.setpoint_hold = None;
                    Ok(())
                }
                Some(_) if run.mode != Mode::Auto => {
                    Err(moli_i18n::tr!("pilotes.chauffage.maintien_auto"))
                }
                Some(minutes) => {
                    run.setpoint_hold = Some(Timed::start(minutes, now_ms(), self.minute));
                    Ok(())
                }
                None => Err(moli_i18n::tr!("pilotes.chauffage.duree_refusee")),
            },
            _ => return None,
        })
    }
}

impl Driver for Heating {
    fn kind(&self) -> &'static str {
        "heating"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let mut runs = Vec::with_capacity(self.rooms.len());
            for room in &self.rooms {
                ctx.upsert_device(device(ctx, room));
                runs.push(restore(ctx, room));
            }
            ctx.ready();
            let started = Instant::now();
            let mut tick = tokio::time::interval(self.tick);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    command = ctx.next_command() => {
                        let Some(command) = command else { return Ok(()) };
                        let Some(index) = runs.iter().position(|r| r.id == command.device.id) else {
                            command.reply(Err(moli_i18n::tr!("pilotes.chauffage.point_inconnu")));
                            continue;
                        };
                        let (room, run) = (&self.rooms[index], &mut runs[index]);
                        self.command(ctx, room, run, command);
                        let settled = started.elapsed() >= self.settle;
                        self.look(ctx, room, run, settled).await;
                    }
                    _ = tick.tick() => {
                        let settled = started.elapsed() >= self.settle;
                        for (room, run) in self.rooms.iter().zip(&mut runs) {
                            self.follow(ctx, run);
                            self.look(ctx, room, run, settled).await;
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
