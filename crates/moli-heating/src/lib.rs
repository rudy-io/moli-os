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
//!   `auto`, a change holds until the next step of the schedule.
//! - `system_mode` (writable): `auto` follows the schedule, `heat` holds the
//!   setpoint, `off` gives the valves their lowest setpoint (frost guard).
//! - `weekly_schedule_<day>` (writable): `00:00/17 06:30/20 22:00/17`.
//! - `valve_correction`: the shift applied to the valves (°C), learned from
//!   the room's error ([`regulation`]); kept across restarts. 0 with
//!   `correction = false`.
//!
//! ```toml
//! [[heating]]
//! id = "parents"                                # never rename it
//! name = "Chambre parentale"
//! sensor = "matter:<meter>/temperature"
//! valves = ["matter:<valve>/target_temperature"]  # each valve's setpoint
//! # correction = false: the valves get the room's setpoint as it is (a valve
//! #   that already regulates on a sensor of the room, as tado X does with its own)
//! # min = 5.0, max = 25.0: the setpoints the room accepts
//! # initial = 19.0: the setpoint and schedule before anyone set one
//! ```
//!
//! Orders go through the hub as the house's own, like a scene's. The room's
//! sensor quiet, the shift holds and the valves are left as they are unless
//! the room's setpoint or mode changes. A valve is written only on a
//! change of half a degree, at most every [`regulation::MIN_INTERVAL`] unless
//! the room changed, and never in the first minutes after a start, while
//! the drivers come back.

pub mod regulation;
pub mod schedule;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::ensure;
use moli_core::{
    Access, Device, DeviceId, Kind, Origin, PointId, PointSpec, Semantic, Unit, Value, now_ms,
};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx, Hub};
use serde::Deserialize;

use regulation::{Shift, valve_setpoint, worth_sending};
use schedule::{DAYS, Step, format_day, parse_day, step_at};

/// The instance that runs the rooms.
pub const INSTANCE: &str = "chauffage";

/// How often the schedule and the valves are looked at.
const TICK: Duration = Duration::from_secs(60);
/// A reading older than this is no longer the room's temperature.
const FRESH_MS: u64 = 30 * 60 * 1000;
/// No valve is written in this time after a start: the drivers come back
/// and the sensors are read again first.
const SETTLE: Duration = Duration::from_secs(3 * 60);
const MODES: [&str; 3] = ["off", "heat", "auto"];
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
    #[serde(default = "default_min")]
    pub min: f64,
    #[serde(default = "default_max")]
    pub max: f64,
    /// The setpoint and every day's schedule before anyone set them.
    #[serde(default = "default_initial")]
    pub initial: f64,
}

fn default_correction() -> bool {
    true
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
        }
    }
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
    sent: Option<Instant>,
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
    /// The setpoint or mode changed since the valves were last written.
    urgent: bool,
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
        members: Vec::new(),
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
    let shift = if room.correction {
        Shift::from(
            ctx.current(&id, "valve_correction")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
        )
    } else {
        Shift::default()
    };
    ctx.set_state(&id, "valve_correction", Value::Float(shift.get()));
    ctx.set_availability(&id, true);
    RoomRun {
        id,
        mode,
        setpoint,
        week,
        step: None,
        valves: room
            .valves
            .iter()
            .map(|_| ValveRun { sent: None })
            .collect(),
        shift,
        read: None,
        urgent: false,
    }
}

fn text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(t) => Some(t),
        _ => None,
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Off => "off",
        Mode::Heat => "heat",
        Mode::Auto => "auto",
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
        (now_ms().saturating_sub(sample.ts) <= FRESH_MS).then_some(value)
    }

    /// The valve's setpoint bounds, if Moli may write it.
    fn bounds(&self, point: &str) -> Option<(f64, f64)> {
        let point = PointId::from(point);
        let (device, key) = point.split()?;
        let view = self.hub.device(&device)?;
        let spec = view.device.points.iter().find(|p| p.key.as_ref() == key)?;
        match spec.kind {
            Kind::Numeric {
                min: Some(min),
                max: Some(max),
                ..
            } if spec.access.write && min < max => Some((min, max)),
            _ => None,
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
            run.setpoint = temp;
            run.urgent = true;
            ctx.set_state(&run.id, "occupied_heating_setpoint", Value::Float(temp));
        }
    }

    /// Reads the room and its valves, and writes the valves that need it.
    async fn regulate(&self, ctx: &DriverCtx, room: &Room, run: &mut RoomRun, settled: bool) {
        let temperature = self.fresh(&room.sensor);
        ctx.set_state(
            &run.id,
            "local_temperature",
            temperature.map_or(Value::Null, Value::Float),
        );
        // Kept until the valves may be written: a change made while Moli
        // starts is not lost.
        let urgent = if settled {
            std::mem::take(&mut run.urgent)
        } else {
            run.urgent
        };
        // The room's error moves the shift, while heating and the room known.
        if let Some(room_t) = temperature
            && room.correction
            && run.mode != Mode::Off
        {
            let now = Instant::now();
            let elapsed = run.read.map_or(Duration::ZERO, |t| now - t);
            run.shift.update(run.setpoint - room_t, elapsed);
            run.read = Some(now);
        } else {
            run.read = None;
        }
        let shift = run.shift.get();
        for (valve, state) in room.valves.iter().zip(&mut run.valves) {
            let Some(bounds) = self.bounds(valve) else {
                continue;
            };
            // The room unknown: nothing is changed unless asked.
            if !settled || (temperature.is_none() && !urgent) {
                continue;
            }
            let next = match run.mode {
                Mode::Off => bounds.0,
                Mode::Heat | Mode::Auto => valve_setpoint(run.setpoint, shift, bounds),
            };
            let point = PointId::from(valve.as_str());
            let current = self.hub.state(&point).and_then(|s| s.value.as_f64());
            if !worth_sending(current, next, state.sent.map(|t| t.elapsed()), urgent) {
                continue;
            }
            let actor = moli_i18n::tr!("pilotes.chauffage.acteur", piece = room.name);
            match self
                .hub
                .command(&point, Value::Float(next), Origin::System, Some(actor))
                .await
            {
                Ok(()) => state.sent = Some(Instant::now()),
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
        let key = command.key.as_ref();
        let result = if key == "occupied_heating_setpoint" {
            match command.value.as_f64() {
                Some(t) if (room.min..=room.max).contains(&t) => {
                    run.setpoint = regulation::half_degree(t).clamp(room.min, room.max);
                    run.urgent = true;
                    ctx.set_state(&run.id, key, Value::Float(run.setpoint));
                    Ok(())
                }
                _ => Err(moli_i18n::tr!("pilotes.chauffage.consigne_refusee")),
            }
        } else if key == "system_mode" {
            match text(&command.value).and_then(Mode::parse) {
                Some(mode) => {
                    run.mode = mode;
                    run.step = None;
                    run.urgent = true;
                    ctx.set_state(&run.id, key, Value::from(mode_name(mode)));
                    self.follow(ctx, run);
                    Ok(())
                }
                None => Err(moli_i18n::tr!("pilotes.chauffage.mode_refuse")),
            }
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
            let mut tick = tokio::time::interval(TICK);
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
                        self.regulate(ctx, room, run, settled).await;
                    }
                    _ = tick.tick() => {
                        let settled = started.elapsed() >= self.settle;
                        for (room, run) in self.rooms.iter().zip(&mut runs) {
                            self.follow(ctx, run);
                            self.regulate(ctx, room, run, settled).await;
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
