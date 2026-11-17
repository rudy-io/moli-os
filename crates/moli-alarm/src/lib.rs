//! The house's alarm, kept by Moli itself: modes (home, night, away), the
//! sensors each one watches (a door, a camera zone where someone walks),
//! delays to leave and to come home, and how long it stays triggered.
//!
//! It only decides. What the house does about it (announce on the
//! speakers, sound the siren, tell the phones, arm at 21:00) are
//! automations reading its points:
//!
//! - `mode` (writable): `off` or a mode. Arming is refused, and said, while
//!   a sensor of that mode sees something or does not answer; the order
//!   then fails with the sensors' names. Armed, in an entry delay or
//!   triggered, another mode is refused: that would be disarming, which is
//!   `off`, with its code (during the exit delay it may still change).
//! - `state`: `disarmed`, `arming`, `armed_home`, `armed_night`,
//!   `armed_away`, `pending` (entry delay), `triggered`.
//! - `until`: when the current delay ends (Unix seconds, 0 without one).
//!   Delays are counted on a steady clock; the wall's is for showing only.
//! - `cause`: the sensors that triggered it (or blocked arming).
//! - `silent`: the sensors that do not answer (point unknown, device
//!   offline), empty when all do.
//! - `event`: what just happened, each time anew (`arming`, `armed`,
//!   `arm_failure`, `pending`, `triggered`, `disarmed`, and `silent` when,
//!   armed, a sensor stops answering).
//! - `setup`: its modes and sensors (JSON, 4 000 characters at most, about
//!   30 sensors). Written (the alarm disarmed), new settings: checked like
//!   the file's, kept in the instance's vault, used from then on,
//!   `moli.toml` being only the first settings. A control: an agent never
//!   changes them without a person.
//!
//! A restart never silences it: triggered stays triggered, and an entry or
//! exit delay under way starts again in full.
//!
//! In `[guard]` (the recommendation, the same in the README and the
//! onboarding): the code each time on `mode` = `off` and on `setup`
//! (`[[guard.code_each_time]]`); arming by voice is a person's choice
//! (`[[guard.agents_may]]` on `mode`: `home`, `night`, `away`), safe as no
//! mode change silences it. Its orders change nothing outside Moli: a
//! mirror house runs it in the shadow of the real one.
//!
//! ```toml
//! [[driver]]
//! id = "alarme"
//! kind = "alarm"
//! [driver.options.modes.night]
//! exit = 5        # seconds to leave
//! entry = 5       # seconds to disarm on coming home
//! trigger = 480   # seconds triggered (0: until disarmed)
//! [[driver.options.sensor]]
//! point = "frigate:zone_jardin/person"
//! name = "Jardin"
//! modes = ["home", "night"]
//! immediate = false   # true: no entry delay for it
//! ```

mod machine;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

use anyhow::ensure;
use moli_core::{Access, Device, DeviceId, Event, Kind, PointId, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub use machine::{Alarm, ArmError, Delays, Happening, Mode, Phase, Sensor};

/// The device's own id within its instance.
const DEVICE: &str = "maison";
/// The longest delay accepted (seconds): an hour.
const MAX_DELAY: u64 = 3600;
/// Sensors one alarm watches at most. The settings must also fit in one
/// text point (`setup`, [`moli_core::MAX_TEXT`] characters): checked.
const MAX_SENSORS: usize = 32;
/// Where the settings made on the screen are kept (the instance's vault).
const SETTINGS: &str = "settings";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_name")]
    pub name: String,
    pub modes: BTreeMap<Mode, ModeConfig>,
    #[serde(default, rename = "sensor")]
    pub sensors: Vec<SensorConfig>,
}

fn default_name() -> String {
    "Alarme".into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModeConfig {
    #[serde(default)]
    pub exit: u64,
    #[serde(default)]
    pub entry: u64,
    #[serde(default)]
    pub trigger: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SensorConfig {
    pub point: String,
    /// Its name in messages; the device's name in Moli by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub modes: Vec<Mode>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub immediate: bool,
}

/// What a person sets: the modes and the sensors. From `moli.toml` at
/// first; once changed on the screen (point `setup`, the alarm disarmed),
/// kept by Moli in the instance's vault and used instead.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub modes: BTreeMap<Mode, ModeConfig>,
    #[serde(default)]
    pub sensors: Vec<SensorConfig>,
}

impl Settings {
    /// # Errors
    /// No mode, a delay over an hour, too many sensors, a sensor without a
    /// mode set up, a name too long, one point twice, or settings too long
    /// to be written back on `setup`.
    pub fn check(&self) -> anyhow::Result<()> {
        ensure!(!self.modes.is_empty(), "alarm: at least one mode");
        for (mode, d) in &self.modes {
            ensure!(
                d.exit <= MAX_DELAY && d.entry <= MAX_DELAY && d.trigger <= MAX_DELAY,
                "alarm: mode {}: delays of an hour at most",
                mode.id()
            );
        }
        ensure!(
            self.sensors.len() <= MAX_SENSORS,
            "alarm: {MAX_SENSORS} sensors at most"
        );
        let mut seen = BTreeSet::new();
        for s in &self.sensors {
            ensure!(
                s.point.len() <= 200 && PointId::from(s.point.clone()).split().is_some(),
                "alarm: sensor {:?}: a point is <device>/<key>",
                s.point
            );
            ensure!(seen.insert(&s.point), "alarm: sensor {:?} twice", s.point);
            ensure!(!s.modes.is_empty(), "alarm: sensor {:?}: no mode", s.point);
            ensure!(
                s.name.as_ref().is_none_or(|n| n.chars().count() <= 80),
                "alarm: sensor {:?}: a name of 80 characters at most",
                s.point
            );
            for m in &s.modes {
                ensure!(
                    self.modes.contains_key(m),
                    "alarm: sensor {:?}: mode {} is not set up",
                    s.point,
                    m.id()
                );
            }
        }
        let length = serde_json::to_string(self)?.chars().count();
        ensure!(
            length <= moli_core::MAX_TEXT,
            "alarm: settings of {length} characters, {} at most (shorter names, or fewer sensors)",
            moli_core::MAX_TEXT
        );
        Ok(())
    }
}

#[derive(Debug)]
pub struct AlarmDriver {
    name: String,
    initial: Settings,
}

impl AlarmDriver {
    /// Checks the configuration.
    ///
    /// # Errors
    /// See [`Settings::check`].
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let initial = Settings {
            modes: config.modes,
            sensors: config.sensors,
        };
        initial.check()?;
        Ok(Self {
            name: config.name,
            initial,
        })
    }

    fn device(&self, ctx: &DriverCtx) -> Device {
        let read = Access {
            read: true,
            write: false,
        };
        let write = Access {
            read: true,
            write: true,
        };
        let point = |key: &str, kind: Kind, access: Access, semantic: Semantic| PointSpec {
            key: key.into(),
            label: key.into(),
            kind,
            access,
            unit: None,
            semantic,
        };
        // Every mode is offered: one not set up is refused by the alarm,
        // so the settings change without the device changing.
        let modes = ["off", "home", "night", "away"]
            .iter()
            .map(|m| (*m).into())
            .collect();
        let states = [
            "disarmed",
            "arming",
            "armed_home",
            "armed_night",
            "armed_away",
            "pending",
            "triggered",
        ];
        Device {
            id: ctx.device_id(DEVICE),
            instance: ctx.instance().clone(),
            native_name: self.name.as_str().into(),
            manufacturer: Some("Moli".into()),
            model: Some("Alarme".into()),
            description: None,
            native_room: None,
            members: Vec::new(),
            points: vec![
                point(
                    "mode",
                    Kind::Enum { values: modes },
                    write,
                    Semantic::Control,
                ),
                point(
                    "state",
                    Kind::Enum {
                        values: states.iter().map(|s| (*s).into()).collect(),
                    },
                    read,
                    Semantic::Alert,
                ),
                point(
                    "until",
                    Kind::Numeric {
                        min: Some(0.0),
                        max: None,
                        step: Some(1.0),
                    },
                    read,
                    Semantic::Other,
                ),
                point("cause", Kind::Text, read, Semantic::Other),
                point("silent", Kind::Text, read, Semantic::Alert),
                point("event", Kind::Text, read, Semantic::Other),
                // Read: the modes and sensors (JSON); written: new settings.
                // A control: an agent never changes them without a person.
                point("setup", Kind::Text, write, Semantic::Control),
            ],
        }
    }
}

/// Whether a sensor's value says it sees something: on, open, a count of
/// people above zero. A `contact` reads true when closed (the house's
/// convention, as Zigbee2MQTT's): open is what counts.
#[must_use]
pub fn active(point: &str, value: &Value) -> bool {
    if point.rsplit('/').next() == Some("contact")
        && let Value::Bool(closed) = value
    {
        return !closed;
    }
    match value {
        Value::Bool(b) => *b,
        Value::Int(i) => *i > 0,
        Value::Float(f) => *f > 0.0,
        Value::Text(t) => matches!(
            t.to_ascii_lowercase().as_str(),
            "on" | "open" | "true" | "detected" | "motion" | "occupied"
        ),
        Value::Null => false,
    }
}

fn wall() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The alarm the settings describe, each sensor named.
fn alarm_of(ctx: &DriverCtx, settings: &Settings) -> Alarm {
    let delays = settings
        .modes
        .iter()
        .map(|(m, d)| {
            (
                *m,
                Delays {
                    exit: d.exit,
                    entry: d.entry,
                    trigger: d.trigger,
                },
            )
        })
        .collect();
    let sensors = settings
        .sensors
        .iter()
        .map(|s| Sensor {
            point: s.point.clone(),
            name: s.name.clone().unwrap_or_else(|| {
                PointId::from(s.point.clone())
                    .split()
                    .and_then(|(device, _)| ctx.name_of(&device))
                    .unwrap_or_else(|| s.point.clone())
            }),
            modes: s.modes.iter().copied().collect(),
            immediate: s.immediate,
        })
        .collect();
    Alarm::new(delays, sensors)
}

/// The modes and sensors, for the screen (the shape `setup` is written in,
/// each sensor with the name it is said by). Should the names found in the
/// house make it too long for a text point, the settings as written.
fn setup(alarm: &Alarm, settings: &Settings) -> String {
    let sensors: Vec<_> = alarm
        .sensors()
        .iter()
        .map(|s| {
            let mut sensor = json!({
                "point": s.point,
                "name": s.name,
                "modes": s.modes.iter().map(|m| m.id()).collect::<Vec<_>>(),
            });
            if s.immediate {
                sensor["immediate"] = json!(true);
            }
            sensor
        })
        .collect();
    let named = json!({ "modes": settings.modes, "sensors": sensors }).to_string();
    if named.chars().count() <= moli_core::MAX_TEXT {
        named
    } else {
        serde_json::to_string(settings).unwrap_or_default()
    }
}

/// `until` on the wall's clock, for people and screens: the steady one
/// counts, this one shows.
fn wall_until(phase: &Phase, now: u64) -> u64 {
    phase
        .until()
        .map_or(0, |until| wall() + until.saturating_sub(now))
}

fn publish(ctx: &DriverCtx, id: &DeviceId, alarm: &Alarm, happened: &[Happening], now: u64) {
    let phase = alarm.phase();
    ctx.set_state(
        id,
        "mode",
        Value::Text(phase.mode().map_or("off", Mode::id).into()),
    );
    ctx.set_state(id, "state", Value::Text(phase.state().into()));
    #[allow(clippy::cast_possible_wrap)]
    ctx.set_state(id, "until", Value::Int(wall_until(phase, now) as i64));
    for h in happened {
        let cause = match h {
            Happening::ArmFailure(_, c)
            | Happening::ArmSilent(_, c)
            | Happening::Pending(c)
            | Happening::Triggered(c) => Some(c.join(", ")),
            Happening::Disarmed => Some(String::new()),
            Happening::Silent(names) => {
                tracing::warn!(instance = %ctx.instance(), sensors = %names.join(", "), "alarm: armed, sensors no longer answer");
                None
            }
            _ => None,
        };
        if let Some(cause) = cause {
            ctx.set_state(id, "cause", Value::Text(cause.into()));
        }
        tracing::info!(instance = %ctx.instance(), event = h.id(), state = phase.state(), "alarm");
        ctx.pulse_state(id, "event", Value::Text(h.id().into()));
    }
}

/// The running alarm: its settings, its state, and what each sensor sees.
struct Live {
    id: DeviceId,
    settings: Settings,
    alarm: Alarm,
    seeing: HashMap<String, bool>,
    /// The sensors (points) that do not answer.
    silent: BTreeSet<String>,
    /// The steady clock the alarm counts on.
    start: Instant,
}

impl Live {
    fn new(ctx: &DriverCtx, settings: Settings) -> Self {
        let alarm = alarm_of(ctx, &settings);
        let mut live = Self {
            id: ctx.device_id(DEVICE),
            settings,
            alarm,
            seeing: HashMap::new(),
            silent: BTreeSet::new(),
            start: Instant::now(),
        };
        live.look(ctx);
        live.silent = live.not_answering(ctx);
        live
    }

    /// Seconds on the steady clock.
    fn now(&self) -> u64 {
        self.start.elapsed().as_secs()
    }

    /// When the current delay ends, on the steady clock.
    fn wake(&self) -> Option<Instant> {
        let until = self.alarm.phase().until()?;
        Some(self.start + Duration::from_secs(until))
    }

    /// What each sensor sees now: a change from nothing to something is
    /// what counts. Returns the sensors that started to see something since
    /// last looked (events were missed: they count all the same).
    fn look(&mut self, ctx: &DriverCtx) -> Vec<String> {
        let before = std::mem::take(&mut self.seeing);
        self.seeing = self
            .settings
            .sensors
            .iter()
            .map(|s| {
                let on = ctx
                    .read(&PointId::from(s.point.clone()))
                    .is_some_and(|v| active(&s.point, &v.value));
                (s.point.clone(), on)
            })
            .collect();
        self.seeing
            .iter()
            .filter(|(point, on)| **on && !before.get(*point).copied().unwrap_or(false))
            .map(|(point, _)| point.clone())
            .collect()
    }

    fn not_answering(&self, ctx: &DriverCtx) -> BTreeSet<String> {
        self.settings
            .sensors
            .iter()
            .filter(|s| !ctx.answers(&PointId::from(s.point.clone())))
            .map(|s| s.point.clone())
            .collect()
    }

    fn name(&self, point: &str) -> String {
        self.alarm
            .sensors()
            .iter()
            .find(|s| s.point == point)
            .map_or_else(|| point.to_owned(), |s| s.name.clone())
    }

    fn show_silent(&self, ctx: &DriverCtx) {
        let names: Vec<String> = self.silent.iter().map(|p| self.name(p)).collect();
        ctx.set_state(&self.id, "silent", Value::Text(names.join(", ").into()));
    }

    /// A device came, went, or went offline: which sensors answer. Armed,
    /// one that stops answering is said (`event` = `silent`).
    fn recount(&mut self, ctx: &DriverCtx) {
        let silent = self.not_answering(ctx);
        if silent == self.silent {
            return;
        }
        let lost: Vec<String> = silent
            .difference(&self.silent)
            .filter(|p| {
                self.alarm.phase().mode().is_some_and(|m| {
                    self.alarm
                        .sensors()
                        .iter()
                        .any(|s| &s.point == *p && s.modes.contains(&m))
                })
            })
            .map(|p| self.name(p))
            .collect();
        self.silent = silent;
        self.show_silent(ctx);
        if !lost.is_empty() {
            publish(
                ctx,
                &self.id,
                &self.alarm,
                &[Happening::Silent(lost)],
                self.now(),
            );
        }
    }

    fn show(&self, ctx: &DriverCtx, happened: &[Happening]) {
        ctx.set_state(
            &self.id,
            "setup",
            Value::Text(setup(&self.alarm, &self.settings).into()),
        );
        self.show_silent(ctx);
        publish(ctx, &self.id, &self.alarm, happened, self.now());
    }

    /// New settings from the screen: only disarmed, checked, kept, then used.
    async fn resettle(&mut self, ctx: &DriverCtx, value: &Value) -> Result<(), String> {
        if *self.alarm.phase() != Phase::Disarmed {
            return Err(moli_i18n::tr!("pilotes.alarme.desarmer_dabord"));
        }
        let Value::Text(text) = value else {
            return Err(moli_i18n::tr!(
                "pilotes.alarme.reglages_invalides",
                raison = "JSON"
            ));
        };
        let settings: Settings = serde_json::from_str(text)
            .map_err(|e| moli_i18n::tr!("pilotes.alarme.reglages_invalides", raison = e))?;
        settings
            .check()
            .map_err(|e| moli_i18n::tr!("pilotes.alarme.reglages_invalides", raison = e))?;
        let kept = serde_json::to_string(&settings).map_err(|e| e.to_string())?;
        ctx.store_secret(SETTINGS, &kept)
            .await
            .map_err(|e| moli_i18n::tr!("pilotes.alarme.reglages_non_gardes", raison = e))?;
        tracing::info!(instance = %ctx.instance(), sensors = settings.sensors.len(), "alarm: new settings");
        self.alarm = alarm_of(ctx, &settings);
        self.settings = settings;
        self.look(ctx);
        self.silent = self.not_answering(ctx);
        self.show(ctx, &[]);
        Ok(())
    }

    async fn command(&mut self, ctx: &DriverCtx, command: CommandRequest) {
        if &*command.key == "setup" {
            let result = self.resettle(ctx, &command.value).await;
            command.reply(result);
            return;
        }
        let wanted = match &command.value {
            Value::Text(t) => t.to_string(),
            other => other.to_json().to_string(),
        };
        let now = self.now();
        let result = if wanted == "off" {
            Ok(self.alarm.disarm())
        } else if let Some(mode) = Mode::parse(&wanted) {
            let seeing = &self.seeing;
            let silent = &self.silent;
            self.alarm
                .arm(
                    mode,
                    now,
                    &|p| seeing.get(p).copied().unwrap_or(false),
                    &|p| !silent.contains(p),
                )
                .map_err(|e| match e {
                    ArmError::DisarmFirst => moli_i18n::tr!("pilotes.alarme.desarmer_avant_mode"),
                    ArmError::NotSetUp(mode) => format!("mode {} is not set up", mode.id()),
                })
        } else {
            Err(format!("unknown mode {wanted}"))
        };
        match result {
            Ok(happened) => {
                publish(ctx, &self.id, &self.alarm, &happened, now);
                let refused = happened.iter().find_map(|h| match h {
                    Happening::ArmFailure(_, open) => Some(moli_i18n::tr!(
                        "pilotes.alarme.armement_refuse",
                        capteurs = open.join(", ")
                    )),
                    Happening::ArmSilent(_, silent) => Some(moli_i18n::tr!(
                        "pilotes.alarme.armement_muet",
                        capteurs = silent.join(", ")
                    )),
                    _ => None,
                });
                command.reply(refused.map_or(Ok(()), Err));
            }
            Err(e) => command.reply(Err(e)),
        }
    }

    fn sensed(&mut self, ctx: &DriverCtx, point: &str, value: &Value) {
        if !self.seeing.contains_key(point) {
            return;
        }
        let on = active(point, value);
        let was = self.seeing.insert(point.to_owned(), on).unwrap_or(false);
        if on && !was {
            let now = self.now();
            let happened = self.alarm.sensed(point, now);
            publish(ctx, &self.id, &self.alarm, &happened, now);
        }
    }

    /// Events were missed: read every sensor again, and what started to
    /// see something meanwhile counts as just seen. (One that came and went
    /// within the gap left no trace to read.)
    fn caught_up(&mut self, ctx: &DriverCtx) {
        let now = self.now();
        for point in self.look(ctx) {
            let happened = self.alarm.sensed(&point, now);
            publish(ctx, &self.id, &self.alarm, &happened, now);
        }
        self.recount(ctx);
    }

    fn tick(&mut self, ctx: &DriverCtx) {
        let now = self.now();
        let seeing = &self.seeing;
        let happened = self
            .alarm
            .tick(now, &|p| seeing.get(p).copied().unwrap_or(false));
        publish(ctx, &self.id, &self.alarm, &happened, now);
    }
}

impl Driver for AlarmDriver {
    fn kind(&self) -> &'static str {
        "alarm"
    }

    fn stays_inside(&self) -> bool {
        true
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let mut events = ctx.watch();
            ctx.upsert_device(self.device(ctx));
            // Settings made on the screen win over the file's, while valid.
            let settings = ctx
                .secret(SETTINGS)
                .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
                .filter(|s| s.check().is_ok())
                .unwrap_or_else(|| self.initial.clone());
            let mut live = Live::new(ctx, settings);
            let text = |key: &str| match ctx.current(&live.id, key) {
                Some(Value::Text(t)) => t.to_string(),
                _ => String::new(),
            };
            let (mode, state, cause) = (text("mode"), text("state"), text("cause"));
            live.alarm.restore(Mode::parse(&mode));
            let cause = cause
                .split(", ")
                .filter(|c| !c.is_empty())
                .map(str::to_owned)
                .collect();
            let resumed = live.alarm.resume(&state, cause, live.now());
            if !resumed.is_empty() {
                tracing::warn!(instance = %ctx.instance(), was = %state, "alarm: restarted while {state}, resumed");
            }
            ctx.set_availability(&live.id, true);
            live.show(ctx, &resumed);
            ctx.ready();
            loop {
                let wake = live.wake();
                tokio::select! {
                    command = ctx.next_command() => {
                        let Some(command) = command else { return Ok(()) };
                        live.command(ctx, command).await;
                    }
                    () = async {
                        if let Some(at) = wake { tokio::time::sleep_until(at).await }
                    }, if wake.is_some() => live.tick(ctx),
                    event = events.recv() => match event {
                        Ok(Event::State { point, value, .. }) => live.sensed(ctx, point.as_str(), &value),
                        Ok(Event::Availability { .. } | Event::DeviceUpserted { .. }
                            | Event::DeviceRestored { .. } | Event::DeviceRemoved { .. }) => live.recount(ctx),
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(instance = %ctx.instance(), missed = n, "alarm: events missed, sensors read again");
                            live.caught_up(ctx);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                    },
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_counts_as_seeing_something() {
        let p = "x:capteur/occupancy";
        assert!(active(p, &Value::Bool(true)));
        assert!(active(p, &Value::Int(2)));
        assert!(!active(p, &Value::Int(0)));
        assert!(active(p, &Value::Text("open".into())));
        assert!(!active(p, &Value::Text("closed".into())));
        assert!(!active(p, &Value::Null));
        // A door: true is closed.
        assert!(!active("z2m:porte/contact", &Value::Bool(true)));
        assert!(active("z2m:porte/contact", &Value::Bool(false)));
    }

    #[test]
    fn the_setup_is_checked() {
        let parse = |s: &str| toml::from_str::<Config>(s).unwrap();
        let ok = parse(
            r#"
            [modes.night]
            exit = 5
            entry = 5
            trigger = 480
            [[sensor]]
            point = "frigate:zone_terrasse/person"
            modes = ["night"]
            "#,
        );
        assert!(AlarmDriver::new(ok).is_ok());
        let unknown_mode = parse(
            r#"
            [modes.night]
            [[sensor]]
            point = "frigate:zone_terrasse/person"
            modes = ["away"]
            "#,
        );
        assert!(AlarmDriver::new(unknown_mode).is_err());
        let too_long = parse("[modes.night]\ntrigger = 7200\n");
        assert!(AlarmDriver::new(too_long).is_err());
        assert!(
            toml::from_str::<Config>("[modes.night]\ncode = \"1234\"\n").is_err(),
            "no code here"
        );
    }

    #[test]
    fn settings_from_the_screen_are_checked_like_the_file() {
        let json = r#"{"modes":{"night":{"exit":5,"entry":5,"trigger":480}},
            "sensors":[{"point":"z2m:porte/contact","name":"Porte","modes":["night"],"immediate":true}]}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        s.check().unwrap();
        // The shape shown on `setup` is the one written back.
        let again: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(again, s);
        let mut bad = s.clone();
        bad.sensors[0].modes = vec![Mode::Away];
        assert!(bad.check().is_err(), "mode not set up");
        let mut bad = s.clone();
        bad.sensors.push(bad.sensors[0].clone());
        assert!(bad.check().is_err(), "twice");
        let mut bad = s;
        bad.sensors[0].name = Some("x".repeat(81));
        assert!(bad.check().is_err(), "name too long");
        assert!(serde_json::from_str::<Settings>(r#"{"modes":{},"code":"1"}"#).is_err());
    }

    #[test]
    fn the_settings_fit_in_the_point_they_are_written_on() {
        let sensor = |i: usize, name: &str| SensorConfig {
            point: format!("frigate:zone_capteur_{i}/person"),
            name: Some(name.to_owned()),
            modes: vec![Mode::Night, Mode::Away],
            immediate: i.is_multiple_of(2),
        };
        let modes: BTreeMap<Mode, ModeConfig> = [Mode::Night, Mode::Away]
            .into_iter()
            .map(|m| {
                (
                    m,
                    ModeConfig {
                        exit: 30,
                        entry: 30,
                        trigger: 480,
                    },
                )
            })
            .collect();
        // The announced limit, with ordinary names: it fits, and the screen
        // writes it back as shown.
        let full = Settings {
            modes: modes.clone(),
            sensors: (0..MAX_SENSORS)
                .map(|i| sensor(i, &format!("Capteur {i}")))
                .collect(),
        };
        full.check().unwrap();
        let shown = serde_json::to_string(&full).unwrap();
        assert!(shown.chars().count() <= moli_core::MAX_TEXT);
        assert_eq!(serde_json::from_str::<Settings>(&shown).unwrap(), full);
        // One more: refused.
        let mut over = full.clone();
        over.sensors.push(sensor(99, "Un de trop"));
        assert!(over.check().is_err());
        // Long names: refused with the length, not cut on the screen.
        let long = Settings {
            modes,
            sensors: (0..MAX_SENSORS)
                .map(|i| sensor(i, &"Nom très long ".repeat(5)))
                .collect(),
        };
        let error = long.check().unwrap_err().to_string();
        assert!(error.contains("characters"), "{error}");
    }
}
