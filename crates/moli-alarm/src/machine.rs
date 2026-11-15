//! The alarm itself, with no clock and no network: what it is, and what one
//! order, one sensor or the passing of time makes of it. Times are Unix
//! seconds, handed in by the driver.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// How the house is watched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// People are home, awake: the outside only.
    Home,
    /// People are home, asleep.
    Night,
    /// Nobody is home.
    Away,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Home, Mode::Night, Mode::Away];

    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Mode::Home => "home",
            Mode::Night => "night",
            Mode::Away => "away",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.id() == s)
    }
}

/// One mode's delays (seconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delays {
    /// To leave once armed: nothing triggers meanwhile.
    pub exit: u64,
    /// To disarm once a sensor saw something.
    pub entry: u64,
    /// How long it stays triggered (the siren); 0 = until disarmed.
    pub trigger: u64,
}

/// One sensor, as the alarm sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sensor {
    /// Its point (`frigate:allee_portail/person`).
    pub point: String,
    pub name: String,
    pub modes: BTreeSet<Mode>,
    /// Triggers at once, without the entry delay (a sensor nobody passes
    /// to come home).
    pub immediate: bool,
}

/// Where the alarm stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Disarmed,
    /// Armed in `mode` at `until`: leaving.
    Arming {
        mode: Mode,
        until: u64,
    },
    Armed {
        mode: Mode,
    },
    /// A sensor saw something: triggers at `until` unless disarmed.
    Pending {
        mode: Mode,
        until: u64,
        cause: Vec<String>,
    },
    /// Triggered; back to armed at `until` (None: until disarmed).
    Triggered {
        mode: Mode,
        until: Option<u64>,
        cause: Vec<String>,
    },
}

impl Phase {
    /// The state as automations and screens read it.
    #[must_use]
    pub fn state(&self) -> &'static str {
        match self {
            Phase::Disarmed => "disarmed",
            Phase::Arming { .. } => "arming",
            Phase::Armed { mode: Mode::Home } => "armed_home",
            Phase::Armed { mode: Mode::Night } => "armed_night",
            Phase::Armed { mode: Mode::Away } => "armed_away",
            Phase::Pending { .. } => "pending",
            Phase::Triggered { .. } => "triggered",
        }
    }

    /// The mode it is armed (or arming) in.
    #[must_use]
    pub fn mode(&self) -> Option<Mode> {
        match self {
            Phase::Disarmed => None,
            Phase::Arming { mode, .. }
            | Phase::Armed { mode }
            | Phase::Pending { mode, .. }
            | Phase::Triggered { mode, .. } => Some(*mode),
        }
    }

    /// When the current delay ends.
    #[must_use]
    pub fn until(&self) -> Option<u64> {
        match self {
            Phase::Arming { until, .. } | Phase::Pending { until, .. } => Some(*until),
            Phase::Triggered { until, .. } => *until,
            _ => None,
        }
    }

    /// The sensors that caused it.
    #[must_use]
    pub fn cause(&self) -> &[String] {
        match self {
            Phase::Pending { cause, .. } | Phase::Triggered { cause, .. } => cause,
            _ => &[],
        }
    }
}

/// What happened, for automations (a new one each time).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Happening {
    Arming(Mode),
    Armed(Mode),
    /// Not armed: these sensors see something.
    ArmFailure(Mode, Vec<String>),
    Pending(Vec<String>),
    Triggered(Vec<String>),
    Disarmed,
}

impl Happening {
    #[must_use]
    pub fn id(&self) -> &'static str {
        match self {
            Happening::Arming(_) => "arming",
            Happening::Armed(_) => "armed",
            Happening::ArmFailure(..) => "arm_failure",
            Happening::Pending(_) => "pending",
            Happening::Triggered(_) => "triggered",
            Happening::Disarmed => "disarmed",
        }
    }
}

/// The alarm: its settings and where it stands.
#[derive(Clone, Debug)]
pub struct Alarm {
    delays: Vec<(Mode, Delays)>,
    sensors: Vec<Sensor>,
    phase: Phase,
}

impl Alarm {
    #[must_use]
    pub fn new(delays: Vec<(Mode, Delays)>, sensors: Vec<Sensor>) -> Self {
        Self {
            delays,
            sensors,
            phase: Phase::Disarmed,
        }
    }

    #[must_use]
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    #[must_use]
    pub fn sensors(&self) -> &[Sensor] {
        &self.sensors
    }

    /// The modes it can be armed in.
    pub fn modes(&self) -> impl Iterator<Item = Mode> + '_ {
        self.delays.iter().map(|(m, _)| *m)
    }

    fn delays(&self, mode: Mode) -> Option<Delays> {
        self.delays
            .iter()
            .find(|(m, _)| *m == mode)
            .map(|(_, d)| *d)
    }

    /// Back where it was before a restart: armed stays armed (a delay or a
    /// siren under way is not resumed).
    pub fn restore(&mut self, mode: Option<Mode>) {
        self.phase = match mode {
            Some(mode) if self.delays(mode).is_some() => Phase::Armed { mode },
            _ => Phase::Disarmed,
        };
    }

    /// The names of the sensors of `mode` that see something now.
    fn seeing(&self, mode: Mode, active: &dyn Fn(&str) -> bool) -> Vec<String> {
        self.sensors
            .iter()
            .filter(|s| s.modes.contains(&mode) && active(&s.point))
            .map(|s| s.name.clone())
            .collect()
    }

    /// Arm in `mode`. Refused (and said) while one of its sensors sees
    /// something: nobody should leave a door open behind an armed alarm.
    ///
    /// # Errors
    /// The mode is not set up.
    pub fn arm(
        &mut self,
        mode: Mode,
        now: u64,
        active: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<Happening>, String> {
        let delays = self
            .delays(mode)
            .ok_or_else(|| format!("mode {} is not set up", mode.id()))?;
        if matches!(self.phase, Phase::Armed { mode: m } if m == mode) {
            return Ok(Vec::new());
        }
        let open = self.seeing(mode, active);
        if !open.is_empty() {
            return Ok(vec![Happening::ArmFailure(mode, open)]);
        }
        if delays.exit == 0 {
            self.phase = Phase::Armed { mode };
            return Ok(vec![Happening::Armed(mode)]);
        }
        self.phase = Phase::Arming {
            mode,
            until: now + delays.exit,
        };
        Ok(vec![Happening::Arming(mode)])
    }

    pub fn disarm(&mut self) -> Vec<Happening> {
        if self.phase == Phase::Disarmed {
            return Vec::new();
        }
        self.phase = Phase::Disarmed;
        vec![Happening::Disarmed]
    }

    /// A sensor started to see something.
    pub fn sensed(&mut self, point: &str, now: u64) -> Vec<Happening> {
        let Some(sensor) = self.sensors.iter().find(|s| s.point == point) else {
            return Vec::new();
        };
        let name = sensor.name.clone();
        let immediate = sensor.immediate;
        match &mut self.phase {
            Phase::Armed { mode } if sensor.modes.contains(mode) => {
                let mode = *mode;
                let delays = self.delays(mode).unwrap_or(Delays {
                    exit: 0,
                    entry: 0,
                    trigger: 0,
                });
                if immediate || delays.entry == 0 {
                    self.trigger(mode, vec![name], now)
                } else {
                    self.phase = Phase::Pending {
                        mode,
                        until: now + delays.entry,
                        cause: vec![name.clone()],
                    };
                    vec![Happening::Pending(vec![name])]
                }
            }
            Phase::Pending { mode, cause, .. } if sensor.modes.contains(mode) => {
                let mode = *mode;
                if !cause.contains(&name) {
                    cause.push(name);
                }
                if immediate {
                    let cause = cause.clone();
                    self.trigger(mode, cause, now)
                } else {
                    Vec::new()
                }
            }
            Phase::Triggered { mode, cause, .. } if sensor.modes.contains(mode) => {
                if !cause.contains(&name) {
                    cause.push(name);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn trigger(&mut self, mode: Mode, cause: Vec<String>, now: u64) -> Vec<Happening> {
        let trigger = self.delays(mode).map_or(0, |d| d.trigger);
        self.phase = Phase::Triggered {
            mode,
            until: (trigger > 0).then_some(now + trigger),
            cause: cause.clone(),
        };
        vec![Happening::Triggered(cause)]
    }

    /// Time passed: a delay may have ended. Once out of the house, a sensor
    /// that still sees something counts as just seen.
    pub fn tick(&mut self, now: u64, active: &dyn Fn(&str) -> bool) -> Vec<Happening> {
        match self.phase.clone() {
            Phase::Arming { mode, until } if now >= until => {
                self.phase = Phase::Armed { mode };
                let mut out = vec![Happening::Armed(mode)];
                let still: Vec<String> = self
                    .sensors
                    .iter()
                    .filter(|s| s.modes.contains(&mode) && active(&s.point))
                    .map(|s| s.point.clone())
                    .collect();
                for point in still {
                    out.extend(self.sensed(&point, now));
                }
                out
            }
            Phase::Pending { mode, until, cause } if now >= until => self.trigger(mode, cause, now),
            Phase::Triggered {
                mode,
                until: Some(until),
                ..
            } if now >= until => {
                self.phase = Phase::Armed { mode };
                vec![Happening::Armed(mode)]
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alarm() -> Alarm {
        let modes = |m: &[Mode]| m.iter().copied().collect::<BTreeSet<_>>();
        Alarm::new(
            vec![
                (
                    Mode::Night,
                    Delays {
                        exit: 5,
                        entry: 5,
                        trigger: 480,
                    },
                ),
                (
                    Mode::Home,
                    Delays {
                        exit: 0,
                        entry: 0,
                        trigger: 20,
                    },
                ),
            ],
            vec![
                Sensor {
                    point: "z:allee/person".into(),
                    name: "Allée".into(),
                    modes: modes(&[Mode::Home, Mode::Night]),
                    immediate: false,
                },
                Sensor {
                    point: "z:cour/person".into(),
                    name: "Cour".into(),
                    modes: modes(&[Mode::Night]),
                    immediate: false,
                },
                Sensor {
                    point: "z:fenetre/contact".into(),
                    name: "Fenêtre".into(),
                    modes: modes(&[Mode::Night]),
                    immediate: true,
                },
            ],
        )
    }

    const NOBODY: &dyn Fn(&str) -> bool = &|_| false;

    #[test]
    fn night_arms_after_its_exit_delay() {
        let mut a = alarm();
        assert_eq!(
            a.arm(Mode::Night, 100, NOBODY).unwrap(),
            vec![Happening::Arming(Mode::Night)]
        );
        assert_eq!(a.phase().state(), "arming");
        assert_eq!(a.phase().until(), Some(105));
        // Leaving: what the cameras see does not count.
        assert!(a.sensed("z:cour/person", 102).is_empty());
        assert!(a.tick(104, NOBODY).is_empty());
        assert_eq!(a.tick(105, NOBODY), vec![Happening::Armed(Mode::Night)]);
        assert_eq!(a.phase().state(), "armed_night");
    }

    #[test]
    fn it_will_not_arm_while_a_sensor_sees_something() {
        let mut a = alarm();
        let cour: &dyn Fn(&str) -> bool = &|p| p == "z:cour/person";
        assert_eq!(
            a.arm(Mode::Night, 100, cour).unwrap(),
            vec![Happening::ArmFailure(Mode::Night, vec!["Cour".into()])]
        );
        assert_eq!(a.phase(), &Phase::Disarmed);
        // The courtyard is not watched by day.
        assert_eq!(
            a.arm(Mode::Home, 100, cour).unwrap(),
            vec![Happening::Armed(Mode::Home)]
        );
        assert!(
            a.arm(Mode::Away, 100, NOBODY).is_err(),
            "away is not set up"
        );
    }

    #[test]
    fn a_sensor_gives_the_entry_delay_then_it_triggers_then_rearms() {
        let mut a = alarm();
        a.restore(Some(Mode::Night));
        assert_eq!(
            a.sensed("z:allee/person", 200),
            vec![Happening::Pending(vec!["Allée".into()])]
        );
        assert_eq!(a.phase().until(), Some(205));
        assert!(a.sensed("z:cour/person", 201).is_empty());
        assert_eq!(
            a.tick(205, NOBODY),
            vec![Happening::Triggered(vec!["Allée".into(), "Cour".into()])]
        );
        assert_eq!(a.phase().until(), Some(685));
        assert_eq!(a.tick(685, NOBODY), vec![Happening::Armed(Mode::Night)]);
    }

    #[test]
    fn disarmed_in_time_nothing_triggers() {
        let mut a = alarm();
        a.restore(Some(Mode::Night));
        a.sensed("z:allee/person", 200);
        assert_eq!(a.disarm(), vec![Happening::Disarmed]);
        assert!(a.tick(300, NOBODY).is_empty());
        assert!(a.sensed("z:allee/person", 301).is_empty());
        assert!(a.disarm().is_empty(), "already disarmed");
    }

    #[test]
    fn an_immediate_sensor_skips_the_entry_delay() {
        let mut a = alarm();
        a.restore(Some(Mode::Night));
        assert_eq!(
            a.sensed("z:fenetre/contact", 10),
            vec![Happening::Triggered(vec!["Fenêtre".into()])]
        );
        // Day mode has no entry delay at all.
        let mut a = alarm();
        a.restore(Some(Mode::Home));
        assert_eq!(
            a.sensed("z:allee/person", 10),
            vec![Happening::Triggered(vec!["Allée".into()])]
        );
        // A sensor of another mode is not watched.
        let mut a = alarm();
        a.restore(Some(Mode::Home));
        assert!(a.sensed("z:cour/person", 10).is_empty());
    }

    #[test]
    fn someone_still_there_when_the_exit_delay_ends_counts() {
        let mut a = alarm();
        a.arm(Mode::Night, 100, NOBODY).unwrap();
        let cour: &dyn Fn(&str) -> bool = &|p| p == "z:cour/person";
        assert_eq!(
            a.tick(105, cour),
            vec![
                Happening::Armed(Mode::Night),
                Happening::Pending(vec!["Cour".into()])
            ]
        );
    }

    #[test]
    fn a_restart_keeps_it_armed_and_nothing_else() {
        let mut a = alarm();
        a.restore(Some(Mode::Night));
        assert_eq!(a.phase().state(), "armed_night");
        a.restore(Some(Mode::Away));
        assert_eq!(a.phase(), &Phase::Disarmed, "away is not set up");
    }
}
