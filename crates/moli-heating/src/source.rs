//! The heat source: the relay of a boiler, a heat pump or a circulation
//! pump, on while the rooms ask for heat ([`Source`]).
//!
//! With `[[heating_source]]`, Moli adds a device `chauffage:<id>`:
//!
//! - `on`: the state the relay is given.
//! - `demand`: at least `min_rooms` of its rooms ask for heat.
//! - `demanding`: the names of those rooms.
//! - `runtime_today`, `starts_today`: minutes on and starts since local
//!   midnight.
//!
//! The rooms' demand is theirs, on their own sensor (`heat_demand`), never
//! what a valve says of itself. The relay starts after `delay` seconds of
//! demand, and never runs nor rests for less than `min_on` / `min_off`
//! minutes. Its state is sent again every `keepalive` minutes, unchanged
//! too: many boiler receivers fall back to safe mode when no thermostat
//! spoke for a while. An order refused or not answered is tried again at
//! that pace ([`RETRY`] with `keepalive = 0`).
//!
//! ```toml
//! [[heating_source]]
//! id = "chaudiere"                # never rename it
//! name = "Chaudière"
//! switch = "zwave:<relay>/on"     # a writable on/off point
//! rooms = []                      # empty: every [[heating]] room
//! min_rooms = 1
//! delay = 0                       # seconds
//! min_on = 5                      # minutes
//! min_off = 5
//! keepalive = 15                  # minutes, 0: only on a change
//! ```

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::ensure;
use moli_core::{
    Access, Device, DeviceId, Kind, Origin, PointId, PointSpec, Semantic, Unit, Value,
};
use moli_runtime::DriverCtx;
use serde::Deserialize;

use crate::{Heating, House, INSTANCE, Room, RoomRun, house, is_point};

const MAX_SOURCES: usize = 8;
/// Seconds of demand before starting, at most.
pub const MAX_DELAY: u32 = 3600;
/// `min_on`, `min_off`: minutes, at most.
pub const MAX_CYCLE: u32 = 120;
/// `keepalive`: minutes, at most.
pub const MAX_KEEPALIVE: u32 = 240;
/// With `keepalive = 0`, a refused order is tried again this often (minutes).
pub const RETRY: u32 = 15;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Stable id: the device is `chauffage:<id>`. Never rename it.
    pub id: String,
    pub name: String,
    /// The relay's on/off point.
    pub switch: String,
    /// The rooms (`[[heating]]` ids) it heats; empty: all of them.
    #[serde(default)]
    pub rooms: Vec<String>,
    #[serde(default = "default_min_rooms")]
    pub min_rooms: u32,
    /// Seconds of demand before starting.
    #[serde(default)]
    pub delay: u32,
    #[serde(default = "default_cycle")]
    pub min_on: u32,
    #[serde(default = "default_cycle")]
    pub min_off: u32,
    /// Minutes between two orders, the state unchanged (0: never).
    #[serde(default = "default_keepalive")]
    pub keepalive: u32,
}

fn default_min_rooms() -> u32 {
    1
}
fn default_cycle() -> u32 {
    5
}
fn default_keepalive() -> u32 {
    15
}

/// The indexes of the rooms a source heats.
fn covered(source: &Source, rooms: &[Room]) -> Vec<usize> {
    if source.rooms.is_empty() {
        return (0..rooms.len()).collect();
    }
    rooms
        .iter()
        .enumerate()
        .filter(|(_, r)| source.rooms.contains(&r.id))
        .map(|(i, _)| i)
        .collect()
}

/// Checks the sources against the rooms and, with `[heating_house]`, the
/// house's device.
///
/// # Errors
///
/// The first problem found, in words.
pub fn check(sources: &[Source], rooms: &[Room], home: Option<&House>) -> anyhow::Result<()> {
    ensure!(
        sources.len() <= MAX_SOURCES,
        "heating: {MAX_SOURCES} sources at most"
    );
    ensure!(
        sources.is_empty() || !rooms.is_empty(),
        "[[heating_source]] without any [[heating]] room"
    );
    let mut ids = HashSet::new();
    let mut switches = HashSet::new();
    let mut heated: HashSet<&str> = HashSet::new();
    for source in sources {
        let id = &source.id;
        ensure!(
            !id.is_empty() && id.len() <= 64 && !id.contains([':', '/']),
            "heating source id {id:?}: non-empty, without ':' nor '/'"
        );
        ensure!(
            !rooms.iter().any(|r| &r.id == id),
            "heating source id {id:?} is a room's ({INSTANCE}:{id})"
        );
        ensure!(
            home.is_none() || id != house::DEVICE,
            "heating source id {id:?} is the house's device ({INSTANCE}:{id}) with [heating_house]"
        );
        ensure!(ids.insert(id), "heating source id {id:?} twice");
        ensure!(
            !source.name.trim().is_empty() && source.name.len() <= 60,
            "heating source {id}: a name, 60 characters at most"
        );
        ensure!(
            is_point(&source.switch) && !source.switch.starts_with(&format!("{INSTANCE}:")),
            "heating source {id}: switch {:?} is not a point (device/key) of another device",
            source.switch
        );
        ensure!(
            switches.insert(&source.switch),
            "heating source {id}: switch {:?} driven by another source",
            source.switch
        );
        let mut listed = HashSet::new();
        for room in &source.rooms {
            ensure!(
                rooms.iter().any(|r| &r.id == room),
                "heating source {id}: no [[heating]] room {room:?}"
            );
            ensure!(
                listed.insert(room),
                "heating source {id}: room {room:?} twice"
            );
        }
        let mine = covered(source, rooms);
        for &i in &mine {
            ensure!(
                heated.insert(&rooms[i].id),
                "heating source {id}: room {:?} is already another source's",
                rooms[i].id
            );
        }
        ensure!(
            source.min_rooms >= 1 && source.min_rooms as usize <= mine.len(),
            "heating source {id}: 1 <= min_rooms <= its {} rooms",
            mine.len()
        );
        ensure!(
            source.delay <= MAX_DELAY,
            "heating source {id}: delay {MAX_DELAY} seconds at most"
        );
        ensure!(
            source.min_on <= MAX_CYCLE && source.min_off <= MAX_CYCLE,
            "heating source {id}: min_on and min_off {MAX_CYCLE} minutes at most"
        );
        ensure!(
            source.keepalive <= MAX_KEEPALIVE,
            "heating source {id}: keepalive {MAX_KEEPALIVE} minutes at most (0: never)"
        );
    }
    Ok(())
}

/// The relay's state: on after `delay` of demand (`asking`, how long it has
/// lasted), off without; but not before `min_on` / `min_off` since it last
/// switched (`since`, none yet: at once).
#[must_use]
pub fn wanted(
    on: bool,
    asking: Option<Duration>,
    since: Option<Duration>,
    (delay, min_on, min_off): (Duration, Duration, Duration),
) -> bool {
    let want = asking.is_some_and(|d| d >= delay);
    let rest = if on { min_on } else { min_off };
    if want != on && since.is_none_or(|d| d >= rest) {
        want
    } else {
        on
    }
}

pub(crate) struct SourceRun {
    id: DeviceId,
    rooms: Vec<usize>,
    on: bool,
    switched: Option<Instant>,
    asking: Option<Instant>,
    /// The last order tried, and whether the state changed since.
    sent: Option<Instant>,
    unsent: bool,
    /// The last order was refused: said once, until one is taken.
    failing: bool,
    runtime: Duration,
    starts: u32,
}

/// The local day the counters are of, and when time was last counted.
pub(crate) struct Clock {
    last: Instant,
    date: jiff::civil::Date,
}

fn read_only(key: &str, label: String, kind: Kind, unit: Option<Unit>) -> PointSpec {
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

/// Minutes counted today.
pub(crate) fn minutes(key: &str, label: String) -> PointSpec {
    read_only(
        key,
        label,
        Kind::Numeric {
            min: Some(0.0),
            max: None,
            step: Some(1.0),
        },
        Some(Unit::Minute),
    )
}

fn source_device(ctx: &DriverCtx, source: &Source) -> Device {
    let points = vec![
        read_only(
            "on",
            moli_i18n::tr!("pilotes.chauffage.source.marche"),
            Kind::Binary,
            None,
        ),
        read_only(
            "demand",
            moli_i18n::tr!("pilotes.chauffage.demande"),
            Kind::Binary,
            None,
        ),
        read_only(
            "demanding",
            moli_i18n::tr!("pilotes.chauffage.source.pieces"),
            Kind::Text,
            None,
        ),
        minutes(
            "runtime_today",
            moli_i18n::tr!("pilotes.chauffage.source.marche_jour"),
        ),
        read_only(
            "starts_today",
            moli_i18n::tr!("pilotes.chauffage.source.demarrages_jour"),
            Kind::Numeric {
                min: Some(0.0),
                max: None,
                step: Some(1.0),
            },
            None,
        ),
    ];
    Device {
        id: ctx.device_id(&source.id),
        instance: ctx.instance().clone(),
        native_name: source.name.as_str().into(),
        manufacturer: Some("Moli".into()),
        model: Some(moli_i18n::tr!("pilotes.chauffage.source.modele").into()),
        description: None,
        native_room: None,
        members: PointId::from(source.switch.as_str())
            .split()
            .map(|(device, _)| vec![device])
            .unwrap_or_default(),
        points,
    }
}

fn count(value: u32) -> Value {
    Value::Int(i64::from(value))
}

impl Heating {
    fn today(&self) -> jiff::Zoned {
        jiff::Timestamp::now().to_zoned(self.zone.clone())
    }

    pub(crate) fn clock(&self) -> Clock {
        Clock {
            last: Instant::now(),
            date: self.today().date(),
        }
    }

    /// A count kept from before a restart, if it was written today.
    fn kept(&self, id: &DeviceId, key: &str) -> u32 {
        let Some(sample) = self.hub.state(&PointId::new(id, key)) else {
            return 0;
        };
        let written = i64::try_from(sample.ts)
            .ok()
            .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
            .map(|ts| ts.to_zoned(self.zone.clone()).date());
        if written != Some(self.today().date()) {
            return 0;
        }
        match sample.value {
            Value::Int(n) => u32::try_from(n).unwrap_or(0),
            _ => 0,
        }
    }

    fn in_minutes(&self, time: Duration) -> Value {
        let minutes = time.as_millis() / self.minute.as_millis().max(1);
        Value::Int(i64::try_from(minutes).unwrap_or(i64::MAX))
    }

    /// The room's minutes of demand today, kept from before a restart.
    pub(crate) fn restore_heated(&self, ctx: &DriverCtx, run: &mut RoomRun) {
        run.heated = self.minute * self.kept(&run.id, "heating_minutes_today");
        ctx.set_state(
            &run.id,
            "heating_minutes_today",
            self.in_minutes(run.heated),
        );
    }

    /// The source's device, and what it had before a restart: its state, and
    /// today's counts.
    pub(crate) fn restore_source(&self, ctx: &DriverCtx, source: &Source) -> SourceRun {
        ctx.upsert_device(source_device(ctx, source));
        let id = ctx.device_id(&source.id);
        let on = ctx.current(&id, "on") == Some(Value::Bool(true));
        let run = SourceRun {
            rooms: covered(source, &self.rooms),
            on,
            switched: None,
            asking: None,
            sent: None,
            unsent: false,
            failing: false,
            runtime: self.minute * self.kept(&id, "runtime_today"),
            starts: self.kept(&id, "starts_today"),
            id,
        };
        ctx.set_state(&run.id, "on", Value::Bool(on));
        ctx.set_state(&run.id, "demand", Value::Bool(false));
        ctx.set_state(&run.id, "demanding", Value::from(""));
        ctx.set_state(&run.id, "runtime_today", self.in_minutes(run.runtime));
        ctx.set_state(&run.id, "starts_today", count(run.starts));
        ctx.set_availability(&run.id, true);
        run
    }

    /// Counts the time since the last count: rooms asking for heat, sources
    /// on. Back to zero at local midnight.
    pub(crate) fn account(
        &self,
        ctx: &DriverCtx,
        clock: &mut Clock,
        runs: &mut [RoomRun],
        sources: &mut [SourceRun],
    ) {
        let now = Instant::now();
        let mut elapsed = now - clock.last;
        clock.last = now;
        let today = self.today();
        if today.date() != clock.date {
            clock.date = today.date();
            let since_midnight = today
                .start_of_day()
                .ok()
                .and_then(|m| {
                    Duration::try_from(today.timestamp().duration_since(m.timestamp())).ok()
                })
                .unwrap_or_default();
            elapsed = elapsed.min(since_midnight);
            for run in runs.iter_mut() {
                run.heated = Duration::ZERO;
            }
            for srun in sources.iter_mut() {
                srun.runtime = Duration::ZERO;
                srun.starts = 0;
                ctx.set_state(&srun.id, "starts_today", count(0));
            }
        }
        for run in runs.iter_mut() {
            if run.demand == Some(true) {
                run.heated += elapsed;
            }
            ctx.set_state(
                &run.id,
                "heating_minutes_today",
                self.in_minutes(run.heated),
            );
        }
        for srun in sources.iter_mut() {
            if srun.on {
                srun.runtime += elapsed;
            }
            ctx.set_state(&srun.id, "runtime_today", self.in_minutes(srun.runtime));
        }
    }

    /// The source's demand from its rooms, its state, and the order to its
    /// relay when one is due. Nothing is decided nor sent while Moli starts.
    pub(crate) async fn drive(
        &self,
        ctx: &DriverCtx,
        source: &Source,
        srun: &mut SourceRun,
        runs: &[RoomRun],
        settled: bool,
    ) {
        let asking: Vec<&str> = srun
            .rooms
            .iter()
            .filter(|&&i| runs[i].demand == Some(true))
            .map(|&i| self.rooms[i].name.as_str())
            .collect();
        let demand = asking.len() >= source.min_rooms as usize;
        ctx.set_state(&srun.id, "demand", Value::Bool(demand));
        ctx.set_state(
            &srun.id,
            "demanding",
            Value::from(asking.join(", ").as_str()),
        );
        let now = Instant::now();
        if demand {
            srun.asking.get_or_insert(now);
        } else {
            srun.asking = None;
        }
        if !settled {
            return;
        }
        let on = wanted(
            srun.on,
            srun.asking.map(|t| now - t),
            srun.switched.map(|t| now - t),
            (
                self.minute * source.delay / 60,
                self.minute * source.min_on,
                self.minute * source.min_off,
            ),
        );
        if on != srun.on {
            srun.on = on;
            srun.switched = Some(now);
            srun.unsent = true;
            if on {
                srun.starts += 1;
                ctx.set_state(&srun.id, "starts_today", count(srun.starts));
            }
            ctx.set_state(&srun.id, "on", Value::Bool(on));
            tracing::info!(source = %source.id, on, rooms = %asking.join(", "), "heating: heat source switched");
        }
        let pace = match (source.keepalive, srun.failing) {
            (0, false) => None,
            (0, true) => Some(self.minute * RETRY),
            (minutes, _) => Some(self.minute * minutes),
        };
        let due = srun.unsent || srun.sent.is_none_or(|t| pace.is_some_and(|p| now - t >= p));
        if !due {
            return;
        }
        srun.sent = Some(now);
        srun.unsent = false;
        let actor = moli_i18n::tr!("pilotes.chauffage.source.acteur", source = source.name);
        let point = PointId::from(source.switch.as_str());
        match self
            .hub
            .command(&point, Value::Bool(on), Origin::System, Some(actor))
            .await
        {
            Ok(()) => {
                if std::mem::replace(&mut srun.failing, false) {
                    tracing::info!(source = %source.id, "heating: heat source relay answers again");
                }
            }
            Err(e) => {
                if !std::mem::replace(&mut srun.failing, true) {
                    let minutes = pace.unwrap_or(self.minute * RETRY).as_secs_f64()
                        / self.minute.as_secs_f64();
                    tracing::warn!(source = %source.id, error = %e, retry_minutes = minutes, "heating: heat source order refused, tried again at the keepalive pace");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);
    const TIMES: (Duration, Duration, Duration) = (
        Duration::from_secs(30),
        Duration::from_secs(300),
        Duration::from_secs(300),
    );

    #[test]
    fn the_relay_waits_its_delay_and_never_short_cycles() {
        assert!(!wanted(false, None, None, TIMES));
        assert!(
            !wanted(false, Some(Duration::from_secs(10)), None, TIMES),
            "delay"
        );
        assert!(wanted(false, Some(Duration::from_secs(30)), None, TIMES));
        assert!(
            !wanted(false, Some(MINUTE), Some(2 * MINUTE), TIMES),
            "off for 2 minutes only"
        );
        assert!(wanted(false, Some(MINUTE), Some(5 * MINUTE), TIMES));
        assert!(
            wanted(true, None, Some(4 * MINUTE), TIMES),
            "on for 4 minutes only"
        );
        assert!(!wanted(true, None, Some(5 * MINUTE), TIMES));
        assert!(
            !wanted(true, None, None, TIMES),
            "nothing since a start: at once"
        );
        assert!(
            wanted(true, Some(Duration::ZERO), Some(MINUTE), TIMES),
            "still asked"
        );
    }

    fn rooms() -> Vec<Room> {
        #[derive(Deserialize)]
        struct File {
            heating: Vec<Room>,
        }
        toml::from_str::<File>(
            r#"
            [[heating]]
            id = "parents"
            name = "Chambre"
            sensor = "z2m:0x01/temperature"
            valves = ["z2m:0x02/sp"]
            [[heating]]
            id = "bureau"
            name = "Bureau"
            sensor = "z2m:0x03/temperature"
            valves = ["z2m:0x04/sp"]
            "#,
        )
        .unwrap()
        .heating
    }

    fn source() -> Source {
        toml::from_str(
            r#"
            id = "chaudiere"
            name = "Chaudière"
            switch = "zwave:7/on"
            "#,
        )
        .unwrap()
    }

    #[test]
    fn a_source_takes_every_room_by_default_and_its_defaults() {
        let s = source();
        assert_eq!(
            (s.min_rooms, s.delay, s.min_on, s.min_off, s.keepalive),
            (1, 0, 5, 5, 15)
        );
        assert_eq!(covered(&s, &rooms()), [0, 1]);
        let mut one = source();
        one.rooms = vec!["bureau".into()];
        assert_eq!(covered(&one, &rooms()), [1]);
        assert!(check(&[s], &rooms(), None).is_ok());
        assert!(check(&[], &[], None).is_ok());
        assert!(check(&[source()], &[], None).is_err(), "no room");
    }

    #[test]
    fn bad_sources_are_refused() {
        let cases: [fn(&mut Source); 15] = [
            |s| s.id = "a:b".into(),
            |s| s.id = String::new(),
            |s| s.id = "bureau".into(),
            |s| s.name = " ".into(),
            |s| s.switch = "relais".into(),
            |s| s.switch = "chauffage:autre/on".into(),
            |s| s.rooms = vec!["grenier".into()],
            |s| s.rooms = vec!["bureau".into(), "bureau".into()],
            |s| s.min_rooms = 0,
            |s| s.min_rooms = 3,
            |s| {
                s.rooms = vec!["bureau".into()];
                s.min_rooms = 2;
            },
            |s| s.delay = MAX_DELAY + 1,
            |s| s.min_on = MAX_CYCLE + 1,
            |s| s.min_off = MAX_CYCLE + 1,
            |s| s.keepalive = MAX_KEEPALIVE + 1,
        ];
        for (i, change) in cases.iter().enumerate() {
            let mut s = source();
            change(&mut s);
            assert!(check(&[s], &rooms(), None).is_err(), "case {i}");
        }
        // A room in two sources; two sources on one relay; one id twice.
        let mut other = source();
        other.id = "pac".into();
        other.switch = "zwave:8/on".into();
        other.rooms = vec!["bureau".into()];
        assert!(check(&[source(), other.clone()], &rooms(), None).is_err());
        let mut first = source();
        first.rooms = vec!["parents".into()];
        assert!(check(&[first.clone(), other.clone()], &rooms(), None).is_ok());
        other.switch = first.switch.clone();
        assert!(check(&[first.clone(), other.clone()], &rooms(), None).is_err());
        other.switch = "zwave:8/on".into();
        other.id = first.id.clone();
        assert!(check(&[first, other], &rooms(), None).is_err());
        assert!(
            toml::from_str::<Source>(
                "id = \"a\"\nname = \"A\"\nswitch = \"b:c/on\"\nmin_on = -1\n"
            )
            .is_err()
        );
        assert!(
            toml::from_str::<Source>("id = \"a\"\nname = \"A\"\nswitch = \"b:c/on\"\npower = 3\n")
                .is_err()
        );
    }
}
