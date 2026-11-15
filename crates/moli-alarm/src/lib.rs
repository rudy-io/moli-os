//! The house's alarm, kept by Moli itself: modes (home, night, away), the
//! sensors each one watches (a door, a camera zone where someone walks),
//! delays to leave and to come home, and how long it stays triggered.
//!
//! It only decides. What the house does about it (announce on the
//! speakers, sound the siren, tell the phones, arm at 21:00) are
//! automations reading its points:
//!
//! - `mode` (writable): `off` or a mode. Arming is refused, and said, while
//!   a sensor of that mode sees something; the order then fails with the
//!   sensors' names.
//! - `state`: `disarmed`, `arming`, `armed_home`, `armed_night`,
//!   `armed_away`, `pending` (entry delay), `triggered`.
//! - `until`: when the current delay ends (Unix seconds, 0 without one).
//! - `cause`: the sensors that triggered it (or blocked arming).
//! - `event`: what just happened, each time anew (`arming`, `armed`,
//!   `arm_failure`, `pending`, `triggered`, `disarmed`).
//! - `setup`: its modes and sensors (JSON), for the screen.
//!
//! Disarming wants a person: put the device among the guard's protected
//! devices, and only someone with the house's code (or an automation a
//! person approved) changes it. Its orders change nothing outside Moli: a
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
//! point = "frigate:zone_allee_portail/person"
//! name = "Allée du portail"
//! modes = ["home", "night"]
//! immediate = false   # true: no entry delay for it
//! ```

mod machine;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::ensure;
use moli_core::{Access, Device, DeviceId, Event, Kind, PointId, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::json;

pub use machine::{Alarm, Delays, Happening, Mode, Phase, Sensor};

/// The device's own id within its instance.
const DEVICE: &str = "maison";
/// The longest delay accepted (seconds): an hour.
const MAX_DELAY: u64 = 3600;

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

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeConfig {
    #[serde(default)]
    pub exit: u64,
    #[serde(default)]
    pub entry: u64,
    #[serde(default)]
    pub trigger: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensorConfig {
    pub point: String,
    /// Its name in messages; the device's name in Moli by default.
    #[serde(default)]
    pub name: Option<String>,
    pub modes: Vec<Mode>,
    #[serde(default)]
    pub immediate: bool,
}

#[derive(Debug)]
pub struct AlarmDriver {
    config: Config,
}

impl AlarmDriver {
    /// Checks the configuration.
    ///
    /// # Errors
    /// No mode, a delay over an hour, a sensor without a mode set up, or
    /// one point twice.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        ensure!(!config.modes.is_empty(), "alarm: at least one mode");
        for (mode, d) in &config.modes {
            ensure!(
                d.exit <= MAX_DELAY && d.entry <= MAX_DELAY && d.trigger <= MAX_DELAY,
                "alarm: mode {}: delays of an hour at most",
                mode.id()
            );
        }
        let mut seen = BTreeSet::new();
        for s in &config.sensors {
            ensure!(
                PointId::from(s.point.clone()).split().is_some(),
                "alarm: sensor {:?}: a point is <device>/<key>",
                s.point
            );
            ensure!(seen.insert(&s.point), "alarm: sensor {:?} twice", s.point);
            ensure!(!s.modes.is_empty(), "alarm: sensor {:?}: no mode", s.point);
            for m in &s.modes {
                ensure!(
                    config.modes.contains_key(m),
                    "alarm: sensor {:?}: mode {} is not set up",
                    s.point,
                    m.id()
                );
            }
        }
        Ok(Self { config })
    }

    fn alarm(&self, ctx: &DriverCtx) -> Alarm {
        let delays = self
            .config
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
        let sensors = self
            .config
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

    fn device(&self, ctx: &DriverCtx) -> Device {
        let read = Access {
            read: true,
            write: false,
        };
        let point = |key: &str, kind: Kind, access: Access, semantic: Semantic| PointSpec {
            key: key.into(),
            label: key.into(),
            kind,
            access,
            unit: None,
            semantic,
        };
        let modes: Vec<std::sync::Arc<str>> = std::iter::once("off")
            .chain(self.config.modes.keys().map(|m| m.id()))
            .map(Into::into)
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
            native_name: self.config.name.as_str().into(),
            manufacturer: Some("Moli".into()),
            model: Some("Alarme".into()),
            description: None,
            native_room: None,
            members: Vec::new(),
            points: vec![
                point(
                    "mode",
                    Kind::Enum { values: modes },
                    Access {
                        read: true,
                        write: true,
                    },
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
                point("event", Kind::Text, read, Semantic::Other),
                point("setup", Kind::Text, read, Semantic::Config),
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

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The modes and sensors, for the screen.
fn setup(alarm: &Alarm, config: &Config) -> String {
    let modes: serde_json::Map<String, serde_json::Value> = config
        .modes
        .iter()
        .map(|(m, d)| {
            (
                m.id().to_owned(),
                json!({ "exit": d.exit, "entry": d.entry, "trigger": d.trigger }),
            )
        })
        .collect();
    let sensors: Vec<_> = alarm
        .sensors()
        .iter()
        .map(|s| {
            json!({
                "point": s.point,
                "name": s.name,
                "modes": s.modes.iter().map(|m| m.id()).collect::<Vec<_>>(),
                "immediate": s.immediate,
            })
        })
        .collect();
    json!({ "modes": modes, "sensors": sensors }).to_string()
}

fn publish(ctx: &DriverCtx, id: &DeviceId, alarm: &Alarm, happened: &[Happening]) {
    let phase = alarm.phase();
    ctx.set_state(
        id,
        "mode",
        Value::Text(phase.mode().map_or("off", Mode::id).into()),
    );
    ctx.set_state(id, "state", Value::Text(phase.state().into()));
    #[allow(clippy::cast_possible_wrap)]
    ctx.set_state(id, "until", Value::Int(phase.until().unwrap_or(0) as i64));
    for h in happened {
        let cause = match h {
            Happening::ArmFailure(_, c) | Happening::Pending(c) | Happening::Triggered(c) => {
                Some(c.join(", "))
            }
            Happening::Disarmed => Some(String::new()),
            _ => None,
        };
        if let Some(cause) = cause {
            ctx.set_state(id, "cause", Value::Text(cause.into()));
        }
        tracing::info!(instance = %ctx.instance(), event = h.id(), state = phase.state(), "alarm");
        ctx.pulse_state(id, "event", Value::Text(h.id().into()));
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
            let id = ctx.device_id(DEVICE);
            let mut events = ctx.watch();
            ctx.upsert_device(self.device(ctx));
            let mut alarm = self.alarm(ctx);
            let restored = ctx.current(&id, "mode").and_then(|v| match v {
                Value::Text(t) => Mode::parse(&t),
                _ => None,
            });
            alarm.restore(restored);
            ctx.set_state(
                &id,
                "setup",
                Value::Text(setup(&alarm, &self.config).into()),
            );
            ctx.set_availability(&id, true);
            publish(ctx, &id, &alarm, &[]);
            // What each sensor sees now: a change from nothing to something
            // is what counts.
            let watched: BTreeSet<String> = self
                .config
                .sensors
                .iter()
                .map(|s| s.point.clone())
                .collect();
            let mut seeing: HashMap<String, bool> = watched
                .iter()
                .map(|p| {
                    let on = ctx
                        .read(&PointId::from(p.clone()))
                        .is_some_and(|s| active(p, &s.value));
                    (p.clone(), on)
                })
                .collect();
            ctx.ready();
            loop {
                let wake = alarm.phase().until().map(|until| {
                    let wait = until.saturating_sub(now());
                    tokio::time::Instant::now() + Duration::from_secs(wait)
                });
                tokio::select! {
                    command = ctx.next_command() => {
                        let Some(command) = command else { return Ok(()) };
                        let wanted = match &command.value {
                            Value::Text(t) => t.to_string(),
                            other => other.to_json().to_string(),
                        };
                        let result = if wanted == "off" {
                            Ok(alarm.disarm())
                        } else if let Some(mode) = Mode::parse(&wanted) {
                            let snapshot = seeing.clone();
                            alarm.arm(mode, now(), &|p| snapshot.get(p).copied().unwrap_or(false))
                        } else {
                            Err(format!("unknown mode {wanted}"))
                        };
                        match result {
                            Ok(happened) => {
                                publish(ctx, &id, &alarm, &happened);
                                let refused = happened.iter().find_map(|h| match h {
                                    Happening::ArmFailure(_, open) => Some(moli_i18n::tr!(
                                        "pilotes.alarme.armement_refuse",
                                        capteurs = open.join(", ")
                                    )),
                                    _ => None,
                                });
                                command.reply(refused.map_or(Ok(()), Err));
                            }
                            Err(e) => command.reply(Err(e)),
                        }
                    }
                    () = async {
                        if let Some(at) = wake { tokio::time::sleep_until(at).await }
                    }, if wake.is_some() => {
                        let snapshot = seeing.clone();
                        let happened = alarm.tick(now(), &|p| snapshot.get(p).copied().unwrap_or(false));
                        publish(ctx, &id, &alarm, &happened);
                    }
                    event = events.recv() => match event {
                        Ok(Event::State { point, value, .. }) => {
                            let key = point.to_string();
                            if !watched.contains(&key) {
                                continue;
                            }
                            let on = active(&key, &value);
                            let was = seeing.insert(key.clone(), on).unwrap_or(false);
                            if on && !was {
                                let happened = alarm.sensed(&key, now());
                                publish(ctx, &id, &alarm, &happened);
                            }
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(instance = %ctx.instance(), missed = n, "alarm: events missed, sensors read again");
                            for (p, on) in &mut seeing {
                                *on = ctx.read(&PointId::from(p.clone())).is_some_and(|s| active(p, &s.value));
                            }
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
            point = "frigate:zone_cour/person"
            modes = ["night"]
            "#,
        );
        assert!(AlarmDriver::new(ok).is_ok());
        let unknown_mode = parse(
            r#"
            [modes.night]
            [[sensor]]
            point = "frigate:zone_cour/person"
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
}
