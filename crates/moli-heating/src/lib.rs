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
//! - `valve_override` (writable): `none`, or a valve turned by hand that Moli
//!   leaves as it is for a while (`manual`, `airing`: [`hand`]); writing
//!   `none` gives the valves the room's setpoint again.
//! - `valve_override_until`: when that ends (RFC 3339), empty without one.
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
//! A valve is a writable setpoint in °C with bounds, on a device that is
//! neither an opening nor a lock; anything else is left alone. The valves'
//! devices are the room device's members, so an order to the room is judged
//! on their rooms too (protected rooms, quiet hours).
//!
//! Orders go through the hub as the house's own, like a scene's. The room's
//! sensor quiet, the shift holds and the valves are left as they are unless
//! the room's setpoint or mode changes. A valve is written only on a
//! change of half a degree, at most every [`regulation::MIN_INTERVAL`] unless
//! the room changed, and never in the first minutes after a start, while
//! the drivers come back.

pub mod hand;
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

use regulation::{Shift, valve_heats, valve_setpoint, worth_sending};
use schedule::{DAYS, Step, format_day, parse_day, step_at, to_next_step};

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
    /// [`TICK`], [`hand::HAND_HOLD`], [`hand::AIRING_HOLD`]: shorter in the tests.
    floor: Duration,
    interval: Duration,
    tick: Duration,
    hand_hold: Duration,
    airing_hold: Duration,
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
        }
    }
}

/// What a valve's point is, as the hub knows it.
#[derive(Debug, PartialEq)]
enum Valve {
    /// A setpoint in °C, and its bounds.
    Writable((f64, f64)),
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
    /// Its setpoint at the last look, and the last one Moli wrote (and was
    /// told was taken): a change that is neither was made by hand.
    seen: Option<f64>,
    written: Option<f64>,
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
    // A hold is not kept across a restart: the valves get the room's again.
    show_hold(ctx, &id, None);
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
            .map(|_| ValveRun {
                sent: None,
                urgent: false,
                refused: false,
                seen: None,
                written: None,
                hold: None,
            })
            .collect(),
        shift,
        read: None,
    }
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
                ..
            } if spec.access.write
                && min < max
                && spec.unit == Some(Unit::Celsius)
                && !matches!(spec.semantic, Semantic::Control | Semantic::Lock)
                && !opening =>
            {
                Valve::Writable((min, max))
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

    /// Reads the room and its valves, and writes the valves that need it.
    async fn regulate(&self, ctx: &DriverCtx, room: &Room, run: &mut RoomRun, settled: bool) {
        let temperature = self.fresh(&room.sensor);
        ctx.set_state(
            &run.id,
            "local_temperature",
            temperature.map_or(Value::Null, Value::Float),
        );
        let next_step = self.to_next_step(run);
        // Each valve's hold: one turned by hand since the last look, one over.
        for (valve, state) in room.valves.iter().zip(&mut run.valves) {
            if state.hold.is_some_and(|h| now_ms() >= h.until_ms) {
                state.hold = None;
                state.urgent = true;
            }
            let Valve::Writable((min, _)) = self.bounds(valve) else {
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
            if settled && hand::turned(state.seen, current, state.written) {
                self.turned_by_hand(room, next_step, state, (current, min), temperature);
            }
            state.seen = Some(current);
        }
        let hold = run.hold();
        show_hold(ctx, &run.id, hold);
        // The room's error moves the shift, while heating and the room known,
        // and no valve held by hand (the room is not at its setpoint then).
        if let Some(room_t) = temperature
            && room.correction
            && run.mode != Mode::Off
            && hold.is_none()
        {
            let now = Instant::now();
            let elapsed = run.read.map_or(Duration::ZERO, |t| now - t);
            let heating = self.heating(room, room_t);
            run.shift.update(run.setpoint - room_t, elapsed, heating);
            run.read = Some(now);
        } else {
            run.read = None;
        }
        let shift = run.shift.get();
        for (valve, state) in room.valves.iter().zip(&mut run.valves) {
            // Kept until the valve may be written: a change made while Moli
            // starts, or before its valve is known, is not lost.
            if !settled || state.hold.is_some() {
                continue;
            }
            let bounds = match self.bounds(valve) {
                Valve::Writable(bounds) => bounds,
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
            let next = match run.mode {
                Mode::Off => bounds.0,
                Mode::Heat | Mode::Auto => valve_setpoint(run.setpoint, shift, bounds),
            };
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
                Ok(()) => state.written = Some(next),
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
                    Ok(())
                }
                _ => Err(moli_i18n::tr!("pilotes.chauffage.consigne_refusee")),
            }
        } else if key == "system_mode" {
            match text(&command.value).and_then(Mode::parse) {
                Some(mode) => {
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
