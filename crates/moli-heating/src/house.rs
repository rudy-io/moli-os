//! The house's programmes: weeks every room keeps one of, the one they all
//! play, and the house left alone ([`House`]).
//!
//! With `[heating_house]`, Moli adds a device `chauffage:maison`:
//!
//! - `programme` (writable): the week every room plays.
//! - `today_as` (writable): `none`, or the day (`monday`…) every room plays
//!   today; back to `none` at local midnight.
//! - `system_mode` (writable): `auto` or `off`, written on every room.
//! - `away` (with `away`, a read-only point): nobody home for `away_delay`,
//!   so every room plays `away_mode` over what is chosen: the frost guard
//!   (every room, whatever its own mode) or a programme. Nothing chosen is
//!   written over; the return plays the choice again.
//! - `away_lifted`: a person set the house's `programme` or `system_mode`,
//!   which lifts the absence until the next return. A person's order to a
//!   room takes that room out of it (its own `away_lifted`). Both, and the
//!   absence itself, are kept across a restart.
//!
//! Each room keeps one week per programme, `weekly_schedule_<key>_<day>`
//! ([`key`]); a day never set (empty) plays the first programme's. Its
//! `weekly_schedule_<day>` points are the chosen programme's.
//!
//! ```toml
//! [heating_house]
//! programmes = ["Normal", "Télétravail", "Vacances"]  # the first one by default
//! away = "personnes:maison/everyone_away"             # optional
//! away_delay = 30                                     # minutes
//! away_mode = "off"                                   # or a programme's name
//! ```

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use anyhow::ensure;
use moli_core::{Access, Device, DeviceId, Kind, PointId, PointSpec, Semantic, Value};
use moli_runtime::{CommandRequest, DriverCtx};
use serde::Deserialize;

use crate::schedule::DAYS;
use crate::{
    Heating, INSTANCE, Mode, Overlay, Play, Room, RoomRun, by_person, flag, is_point, show_weeks,
    text, valve_devices,
};

/// The house's device: `chauffage:maison`.
pub const DEVICE: &str = "maison";
const MAX_PROGRAMMES: usize = 8;
const MAX_NAME: usize = 40;
const MAX_DELAY: u32 = 24 * 60;
const HOUSE_MODES: [&str; 2] = ["auto", "off"];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct House {
    /// The programmes' names, the first one by default. A name is its
    /// weeks' key ([`key`]): renamed, its weeks start again from the first's.
    pub programmes: Vec<String>,
    /// A boolean point, true while nobody is home.
    #[serde(default)]
    pub away: Option<String>,
    /// Minutes of absence before `away_mode` is applied.
    #[serde(default = "default_delay")]
    pub away_delay: u32,
    /// `off` (every room to frost guard) or a programme's name.
    #[serde(default = "default_away_mode")]
    pub away_mode: String,
}

fn default_delay() -> u32 {
    30
}
fn default_away_mode() -> String {
    "off".into()
}

impl House {
    /// The programme's position, by its name.
    #[must_use]
    pub fn position(&self, name: &str) -> Option<usize> {
        self.programmes.iter().position(|p| p == name)
    }
}

/// A programme's key in its points: lowercase ASCII letters and digits,
/// accents dropped, anything else one `_` (« Télétravail » → `teletravail`).
/// The dashboard computes the same (`web/src/maison/lib/schedule.js`).
#[must_use]
pub fn key(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let folded = match c {
            'a'..='z' | '0'..='9' => c.to_string(),
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => "a".into(),
            'ç' => "c".into(),
            'è' | 'é' | 'ê' | 'ë' => "e".into(),
            'ì' | 'í' | 'î' | 'ï' => "i".into(),
            'ñ' => "n".into(),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => "o".into(),
            'ù' | 'ú' | 'û' | 'ü' => "u".into(),
            'ý' | 'ÿ' => "y".into(),
            'œ' => "oe".into(),
            'æ' => "ae".into(),
            'ß' => "ss".into(),
            _ => "_".into(),
        };
        if folded == "_" && (out.is_empty() || out.ends_with('_')) {
            continue;
        }
        out.push_str(&folded);
    }
    out.trim_end_matches('_').to_owned()
}

/// Checks the house's table against its rooms.
///
/// # Errors
///
/// The first problem found, in words.
pub fn check(house: &House, rooms: &[Room]) -> anyhow::Result<()> {
    ensure!(
        !rooms.is_empty(),
        "[heating_house] without any [[heating]] room"
    );
    ensure!(
        !rooms.iter().any(|r| r.id == DEVICE),
        "[[heating]] id {DEVICE:?} is the house's device ({INSTANCE}:{DEVICE}) with [heating_house]"
    );
    ensure!(
        (1..=MAX_PROGRAMMES).contains(&house.programmes.len()),
        "[heating_house] 1 to {MAX_PROGRAMMES} programmes"
    );
    let mut seen = HashSet::new();
    for name in &house.programmes {
        ensure!(
            !name.trim().is_empty() && name.trim() == name && name.chars().count() <= MAX_NAME,
            "[heating_house] programme {name:?}: a name without spaces around, {MAX_NAME} characters at most"
        );
        let key = key(name);
        ensure!(
            !key.is_empty(),
            "[heating_house] programme {name:?}: at least one letter or digit"
        );
        ensure!(
            seen.insert(key),
            "[heating_house] programme {name:?}: another one has the same name (case and accents aside)"
        );
    }
    if let Some(away) = &house.away {
        ensure!(
            is_point(away) && !away.starts_with(&format!("{INSTANCE}:")),
            "[heating_house] away {away:?} is not a point (device/key) of another device"
        );
    }
    ensure!(
        house.away_delay <= MAX_DELAY,
        "[heating_house] away_delay: {MAX_DELAY} minutes at most"
    );
    ensure!(
        house.away_mode == "off" || house.position(&house.away_mode).is_some(),
        "[heating_house] away_mode {:?}: \"off\" or one of the programmes",
        house.away_mode
    );
    Ok(())
}

/// `chauffage:maison`, with `[heating_house]`.
pub(crate) struct HouseRun {
    pub(crate) id: DeviceId,
    /// What the house chose: its programme and `today_as`.
    chosen: Play,
    /// The local date `today_as` was set on: over the next day.
    today_on: Option<jiff::civil::Date>,
    off: bool,
    /// What the absence plays.
    away_mode: Overlay,
    away: Away,
}

impl HouseRun {
    /// What the house has every room play: its choice, and `away_mode`
    /// over it while the absence holds.
    pub(crate) fn playing(&self) -> Play {
        Play {
            away: self.away.holds().then_some(self.away_mode),
            ..self.chosen
        }
    }
}

/// The detected absence. Whether it plays and whether a person lifted it
/// are points (`away`, `away_lifted`): kept across a restart.
#[derive(Default)]
struct Away {
    /// Nobody home since, as the house's point says.
    since: Option<Instant>,
    /// Nobody home for `away_delay`.
    applied: bool,
    /// A person set the house's programme or mode since: its choice is
    /// played until the next return.
    lifted: bool,
}

impl Away {
    fn holds(&self) -> bool {
        self.applied && !self.lifted
    }
}

fn today(zone: &jiff::tz::TimeZone) -> jiff::civil::Date {
    jiff::Timestamp::now().to_zoned(zone.clone()).date()
}

fn choice(key: &str, label: String, values: Vec<Arc<str>>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Enum { values },
        access: Access {
            read: true,
            write: true,
        },
        unit: None,
        semantic: Semantic::Other,
    }
}

/// `chauffage:maison`. Its members are the rooms and their valves: an
/// order to the house is judged on all of their rooms.
pub(crate) fn house_device(ctx: &DriverCtx, house: &House, rooms: &[Room]) -> Device {
    let mut points = vec![
        choice(
            "programme",
            moli_i18n::tr!("pilotes.chauffage.maison.programme"),
            house.programmes.iter().map(|p| p.as_str().into()).collect(),
        ),
        choice(
            "today_as",
            moli_i18n::tr!("pilotes.chauffage.maison.jour"),
            std::iter::once("none")
                .chain(DAYS)
                .map(Into::into)
                .collect(),
        ),
        choice(
            "system_mode",
            moli_i18n::tr!("pilotes.chauffage.mode"),
            HOUSE_MODES.iter().map(|m| (*m).into()).collect(),
        ),
    ];
    if house.away.is_some() {
        points.push(flag(
            "away",
            moli_i18n::tr!("pilotes.chauffage.maison.absence"),
        ));
        points.push(flag(
            "away_lifted",
            moli_i18n::tr!("pilotes.chauffage.maison.absence_levee"),
        ));
    }
    let mut members: Vec<DeviceId> = rooms.iter().map(|r| ctx.device_id(&r.id)).collect();
    for device in rooms.iter().flat_map(valve_devices) {
        if !members.contains(&device) {
            members.push(device);
        }
    }
    Device {
        id: ctx.device_id(DEVICE),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.chauffage.nom").into(),
        manufacturer: Some("Moli".into()),
        model: Some(moli_i18n::tr!("pilotes.chauffage.maison.modele").into()),
        description: None,
        native_room: None,
        members,
        points,
    }
}

impl Heating {
    /// What the house had before a restart, or the first programme in
    /// `auto`. `today_as` holds only on the day it was set; the absence and
    /// its lifting as they were, until the house's point says otherwise.
    pub(crate) fn restore_house(&self, ctx: &DriverCtx, house: &House) -> HouseRun {
        let id = ctx.device_id(DEVICE);
        let programme = match ctx.current(&id, "programme") {
            Some(Value::Text(t)) => house.position(&t).unwrap_or(0),
            _ => 0,
        };
        let set_on = |ts: u64| {
            let ts = jiff::Timestamp::from_millisecond(i64::try_from(ts).ok()?).ok()?;
            Some(ts.to_zoned(self.zone.clone()).date())
        };
        let (day, today_on) = self
            .hub
            .state(&PointId::new(&id, "today_as"))
            .and_then(|s| {
                let day = DAYS.iter().position(|d| text(&s.value) == Some(*d))?;
                let on = set_on(s.ts).filter(|on| *on == today(&self.zone))?;
                Some((Some(day), Some(on)))
            })
            .unwrap_or_default();
        let off = ctx.current(&id, "system_mode") == Some(Value::from("off"));
        let watched = house.away.is_some();
        let was = |key: &str| watched && ctx.current(&id, key) == Some(Value::Bool(true));
        let applied = was("away");
        let lifted = applied && was("away_lifted");
        let run = HouseRun {
            id,
            chosen: Play {
                programme,
                day,
                away: None,
            },
            today_on,
            off,
            away_mode: house
                .position(&house.away_mode)
                .map_or(Overlay::Off, Overlay::Programme),
            away: Away {
                since: applied.then(Instant::now),
                applied,
                lifted,
            },
        };
        ctx.set_state(
            &run.id,
            "programme",
            Value::from(house.programmes[programme].as_str()),
        );
        ctx.set_state(
            &run.id,
            "today_as",
            Value::from(day.map_or("none", |d| DAYS[d])),
        );
        ctx.set_state(&run.id, "system_mode", Value::from(house_mode(off)));
        if watched {
            ctx.set_state(&run.id, "away", Value::Bool(run.away.applied));
            ctx.set_state(&run.id, "away_lifted", Value::Bool(run.away.lifted));
        }
        ctx.set_availability(&run.id, true);
        run
    }

    /// The first programme's key of a `[heating_house]` removed since the
    /// last start (its device still known from before): each room's one week
    /// is that programme's again, not the one played last. Its device goes.
    pub(crate) fn programmes_removed(&self, ctx: &DriverCtx) -> Option<String> {
        let id = ctx.device_id(DEVICE);
        let view = self.hub.device(&id)?;
        let first = view.device.points.iter().find_map(|p| match &p.kind {
            Kind::Enum { values } if p.key.as_ref() == "programme" => values.first().cloned(),
            _ => None,
        });
        ctx.remove_device(&id);
        let first = first?;
        tracing::warn!(programme = %first, "heating: [heating_house] removed, every room keeps the first programme's week");
        Some(key(&first))
    }

    /// Every room plays the house's programme and day again: its step in
    /// force is applied anew, over a setpoint held too (`all`: in every
    /// room; else in those the absence reaches, a room a person took out of
    /// it keeping what it was given).
    fn replay(&self, ctx: &DriverCtx, play: Play, runs: &mut [RoomRun], all: bool) {
        for (room, run) in self.rooms.iter().zip(runs.iter_mut()) {
            show_weeks(ctx, run, self.house.as_ref(), play.programme);
            if !all && run.outside {
                continue;
            }
            self.drop_setpoint_hold(room, run, "house");
            run.step = None;
            self.follow(ctx, run, play);
        }
    }

    /// The absence starts (`starting`) or ends playing: in each room it
    /// reaches, the valves get what the room now plays soon, whatever was
    /// turned on them by hand or held; its start ends the boosts too.
    fn turn(&self, ctx: &DriverCtx, hrun: &HouseRun, runs: &mut [RoomRun], starting: bool) {
        for (room, run) in self.rooms.iter().zip(runs.iter_mut()) {
            if run.outside {
                continue;
            }
            if starting {
                self.end_boost(room, run, "away");
            }
            run.changed();
        }
        self.replay(ctx, hrun.playing(), runs, false);
    }

    /// A person set the house's programme or mode: the absence, if it
    /// holds, no longer plays until the next return.
    fn lift(&self, ctx: &DriverCtx, hrun: &mut HouseRun, runs: &mut [RoomRun]) {
        if hrun.away.holds() {
            hrun.away.lifted = true;
            ctx.set_state(&hrun.id, "away_lifted", Value::Bool(true));
            tracing::info!(
                "heating: the house set by someone, the away mode lifted until the next return"
            );
            self.turn(ctx, hrun, runs, false);
        }
    }

    /// The whole house to frost guard, or back to its schedule: written on
    /// every room, as a scene would.
    fn set_house_mode(
        &self,
        ctx: &DriverCtx,
        hrun: &mut HouseRun,
        runs: &mut [RoomRun],
        off: bool,
    ) {
        hrun.off = off;
        ctx.set_state(&hrun.id, "system_mode", Value::from(house_mode(off)));
        let mode = if off { Mode::Off } else { Mode::Auto };
        for (room, run) in self.rooms.iter().zip(runs.iter_mut()) {
            self.set_mode(ctx, (room, run), mode, hrun.playing());
        }
        tracing::info!(
            mode = house_mode(off),
            "heating: every room set to this mode"
        );
    }

    /// An order to `chauffage:maison`. Only a person's lifts the absence:
    /// an automation or an agent that sets the house changes the choice
    /// played on return.
    pub(crate) fn command_house(
        &self,
        ctx: &DriverCtx,
        house: &House,
        (hrun, runs): (&mut HouseRun, &mut [RoomRun]),
        command: CommandRequest,
    ) {
        if command.abandoned() {
            return;
        }
        let value = text(&command.value);
        let person = by_person(command.origin);
        let result = match command.key.as_ref() {
            "programme" => match value.and_then(|v| house.position(v)) {
                Some(programme) => {
                    hrun.chosen.programme = programme;
                    let name = house.programmes[programme].as_str();
                    ctx.set_state(&hrun.id, "programme", Value::from(name));
                    tracing::info!(programme = name, "heating: the house's programme");
                    if person {
                        self.lift(ctx, hrun, runs);
                    }
                    self.replay(ctx, hrun.playing(), runs, true);
                    Ok(())
                }
                None => Err(moli_i18n::tr!("pilotes.chauffage.maison.programme_refuse")),
            },
            "today_as" => match value {
                Some("none") => {
                    self.play_today_as(ctx, (hrun, runs), None);
                    Ok(())
                }
                Some(day) if DAYS.contains(&day) => {
                    self.play_today_as(ctx, (hrun, runs), DAYS.iter().position(|d| *d == day));
                    Ok(())
                }
                _ => Err(moli_i18n::tr!("pilotes.chauffage.maison.jour_refuse")),
            },
            "system_mode" => match value {
                Some(mode @ ("auto" | "off")) => {
                    if person {
                        self.lift(ctx, hrun, runs);
                    }
                    self.set_house_mode(ctx, hrun, runs, mode == "off");
                    Ok(())
                }
                _ => Err(moli_i18n::tr!("pilotes.chauffage.maison.mode_refuse")),
            },
            _ => Err(moli_i18n::tr!("pilotes.chauffage.point_inconnu")),
        };
        command.reply(result);
    }

    fn play_today_as(
        &self,
        ctx: &DriverCtx,
        (hrun, runs): (&mut HouseRun, &mut [RoomRun]),
        day: Option<usize>,
    ) {
        hrun.chosen.day = day;
        hrun.today_on = day.map(|_| today(&self.zone));
        ctx.set_state(
            &hrun.id,
            "today_as",
            Value::from(day.map_or("none", |d| DAYS[d])),
        );
        self.replay(ctx, hrun.playing(), runs, true);
    }

    /// `today_as` over at midnight, and the detected absence.
    pub(crate) fn house_tick(
        &self,
        ctx: &DriverCtx,
        house: &House,
        (hrun, runs): (&mut HouseRun, &mut [RoomRun]),
    ) {
        if hrun.chosen.day.is_some() && hrun.today_on != Some(today(&self.zone)) {
            self.play_today_as(ctx, (hrun, runs), None);
        }
        let Some(point) = &house.away else {
            return;
        };
        match self.nobody_home(point) {
            Some(true) => {
                let since = *hrun.away.since.get_or_insert_with(Instant::now);
                if hrun.away.applied || since.elapsed() < self.minute * house.away_delay {
                    return;
                }
                hrun.away.applied = true;
                ctx.set_state(&hrun.id, "away", Value::Bool(true));
                tracing::info!(mode = %house.away_mode, "heating: nobody home, the house plays its away mode");
                self.turn(ctx, hrun, runs, true);
            }
            Some(false) => {
                let away = std::mem::take(&mut hrun.away);
                if away.applied {
                    ctx.set_state(&hrun.id, "away", Value::Bool(false));
                    ctx.set_state(&hrun.id, "away_lifted", Value::Bool(false));
                    tracing::info!("heating: someone home, the house plays its own choice again");
                    // Before the rooms taken out are like the others again:
                    // they keep what a person gave them (a hold, a boost).
                    if !away.lifted {
                        self.turn(ctx, hrun, runs, false);
                    }
                }
                for run in runs.iter_mut().filter(|r| r.outside) {
                    run.outside = false;
                    ctx.set_state(&run.id, "away_lifted", Value::Bool(false));
                }
            }
            // Not known (its driver starts, or it went quiet): nothing changes.
            None => {}
        }
    }

    /// The house's absence point: true when nobody is home, `None` when not
    /// known, or when anyone may write it (an agent would empty the house
    /// without the guard looking at its rooms).
    fn nobody_home(&self, point: &str) -> Option<bool> {
        let point = PointId::from(point);
        let (device, key) = point.split()?;
        let view = self.hub.device(&device)?;
        if view.online == Some(false) {
            return None;
        }
        let spec = view.device.points.iter().find(|p| p.key.as_ref() == key)?;
        if spec.access.write {
            if !self.away_refused.swap(true, Ordering::Relaxed) {
                tracing::warn!(%point, "heating: the absence point is writable, never followed");
            }
            return None;
        }
        match self.hub.state(&point)?.value {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }
}

fn house_mode(off: bool) -> &'static str {
    if off { "off" } else { "auto" }
}
