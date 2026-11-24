//! A valve turned by hand (on its dial, or from the dashboard): Moli keeps
//! that setpoint for a while instead of putting its own back, then goes
//! back to the room's.
//!
//! - **By hand**: the setpoint holds for [`HAND_HOLD`] at most, and only
//!   until the room's own setpoint changes (the next step of the schedule,
//!   the end of a setpoint held or of a boost: [`crate::temporary`]).
//! - **Airing**: a valve turned down to its lowest, or well under the room
//!   ([`AIRING_BELOW`]), is someone opening the window. It holds for
//!   [`AIRING_HOLD`], and ends as soon as the valve is turned up again.
//!
//! A change of the room's own setpoint, mode or schedule, or « resume the
//! schedule », ends every hold of the room. Holds are not kept across a
//! restart of Moli: the valves get the room's setpoint again.

use std::time::Duration;

/// How long a setpoint turned by hand holds at most.
pub const HAND_HOLD: Duration = Duration::from_secs(2 * 3600);

/// How long a valve turned down for airing holds at most.
pub const AIRING_HOLD: Duration = Duration::from_secs(30 * 60);

/// A valve turned this much (°C) under the room's temperature, or more, is
/// turned down for airing.
pub const AIRING_BELOW: f64 = 3.0;

/// What a hold is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A setpoint chosen on the valve.
    Hand,
    /// The valve turned down while the window is open.
    Airing,
}

impl Kind {
    /// The room's `valve_override` point.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Hand => "manual",
            Self::Airing => "airing",
        }
    }
}

/// A valve held at a setpoint someone chose, until `until_ms` (Unix ms).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hold {
    pub kind: Kind,
    pub until_ms: u64,
}

/// What Moli wrote to a valve lately, and what the valve made of it.
///
/// A valve may not hold exactly what it is given: one on the whole degree
/// reads 19 back for a 19.5, another rounds its own way. And a valve asleep
/// (Zigbee on a battery) may report an older order after a newer one. Both
/// are Moli's, not a hand's.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Written {
    /// The last two writes the valve took, the latest first.
    last: [Option<f64>; 2],
    /// What the valve read back after one of them, its own rounding.
    held: Option<f64>,
    /// A write not read back yet: the next change may be its rounding.
    echo: bool,
}

impl Written {
    /// A write the valve took.
    pub fn sent(&mut self, value: f64) {
        self.last = [Some(value), self.last[0]];
        self.echo = true;
    }

    /// Whether `current`, the valve's setpoint, is Moli's: one of its last
    /// two writes (to a quarter of a degree), what the valve held of one, or
    /// the first change after a write, within the valve's `step` (half a
    /// degree when not known): then it is what the valve holds of it.
    pub fn moli(&mut self, current: f64, step: Option<f64>) -> bool {
        let near = |v: Option<f64>, by: f64| v.is_some_and(|w| (w - current).abs() < by);
        if self.last.iter().chain([&self.held]).any(|&w| near(w, 0.25)) {
            self.echo = false;
            return true;
        }
        let rounding = step.filter(|s| s.is_finite()).unwrap_or(0.5).max(0.5) + 0.01;
        if std::mem::replace(&mut self.echo, false) && near(self.last[0], rounding) {
            self.held = Some(current);
            return true;
        }
        false
    }
}

/// Whether the valve's setpoint was moved by someone else than Moli: it
/// changed since the last look (`seen`) and is not Moli's ([`Written`]).
/// The first look (`seen` unknown, after a start) says nothing.
#[must_use]
pub fn turned(seen: Option<f64>, current: f64, written: &mut Written, step: Option<f64>) -> bool {
    seen.is_some_and(|s| (s - current).abs() > 0.05) && !written.moli(current, step)
}

/// Whether a setpoint chosen on the valve is airing: its lowest (within half
/// a degree), or [`AIRING_BELOW`] under the room's temperature.
#[must_use]
pub fn airing(value: f64, valve_min: f64, room: Option<f64>) -> bool {
    value <= valve_min + 0.5 || room.is_some_and(|t| value <= t - AIRING_BELOW)
}

/// What a valve turned to `value` becomes, given its hold so far:
/// `Some(kind)` to hold it, `None` to go back to the room's setpoint (a
/// valve turned up again after airing).
#[must_use]
pub fn after_turn(previous: Option<Kind>, airing_now: bool) -> Option<Kind> {
    match (previous, airing_now) {
        (_, true) => Some(Kind::Airing),
        (Some(Kind::Airing), false) => None,
        (_, false) => Some(Kind::Hand),
    }
}

/// When a hold of `kind` made at `now_ms` ends: its own length, or the
/// room's next change if it comes sooner (`to_next_change`: the next step of
/// the schedule, the end of a setpoint held or of a boost; `None` without
/// one).
#[must_use]
pub fn until(
    kind: Kind,
    now_ms: u64,
    to_next_change: Option<Duration>,
    hand: Duration,
    airing: Duration,
) -> u64 {
    let own = match kind {
        Kind::Hand => hand,
        Kind::Airing => airing,
    };
    let length = to_next_change.map_or(own, |next| own.min(next));
    now_ms.saturating_add(u64::try_from(length.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrote(values: &[f64]) -> Written {
        let mut written = Written::default();
        for &v in values {
            written.sent(v);
        }
        written
    }

    #[test]
    fn a_valve_is_turned_by_hand_only_when_it_moved_without_moli() {
        // The first look after a start says nothing.
        assert!(!turned(None, 12.0, &mut wrote(&[19.0]), None));
        // Moved, and not what Moli wrote.
        assert!(turned(Some(19.0), 12.0, &mut wrote(&[19.0]), None));
        assert!(turned(Some(19.0), 19.5, &mut wrote(&[]), None));
        // Moli's own write coming back.
        assert!(!turned(Some(18.0), 19.0, &mut wrote(&[19.0]), None));
        // Not moved: a write the valve did not take is not a hand.
        assert!(!turned(Some(18.0), 18.0, &mut wrote(&[19.0]), None));
    }

    #[test]
    fn a_valve_that_rounds_is_not_a_hand() {
        // On the whole degree: 19.5 written, 19 read back, again and again.
        let mut written = wrote(&[19.5]);
        assert!(!turned(Some(16.0), 19.0, &mut written, Some(1.0)));
        written.sent(19.5);
        assert!(!turned(Some(18.0), 19.0, &mut written, Some(1.0)));
        // Then turned a notch on its dial: a hand.
        assert!(turned(Some(19.0), 18.0, &mut written, Some(1.0)));

        // Its step not known: rounded to the half degree, then a notch.
        let mut written = wrote(&[19.25]);
        assert!(!turned(Some(16.0), 19.5, &mut written, None));
        assert!(turned(Some(19.5), 19.0, &mut written, None));

        // Far from the write, even right after it: a hand.
        assert!(turned(Some(16.0), 22.0, &mut wrote(&[19.5]), Some(1.0)));
    }

    #[test]
    fn a_late_echo_of_an_older_write_is_not_a_hand() {
        // 19 then 20 written to a valve asleep: it reports 19, then 20.
        let mut written = wrote(&[19.0, 20.0]);
        assert!(!turned(Some(16.0), 19.0, &mut written, None));
        assert!(!turned(Some(19.0), 20.0, &mut written, None));
        // Three writes back is too old.
        assert!(turned(
            Some(20.0),
            17.0,
            &mut wrote(&[17.0, 19.0, 20.0]),
            None
        ));
    }

    #[test]
    fn airing_is_the_lowest_or_well_under_the_room() {
        assert!(airing(5.0, 5.0, None));
        assert!(airing(5.5, 5.0, Some(19.0)));
        assert!(
            airing(12.0, 5.0, Some(18.2)),
            "the morning's 12 °C, the room at 18.2"
        );
        assert!(
            !airing(17.0, 5.0, Some(18.2)),
            "turned down a little: by hand"
        );
        assert!(
            !airing(12.0, 5.0, None),
            "the room unknown: only the lowest"
        );
    }

    #[test]
    fn airing_ends_when_the_valve_is_turned_up_again() {
        assert_eq!(after_turn(None, false), Some(Kind::Hand));
        assert_eq!(after_turn(None, true), Some(Kind::Airing));
        assert_eq!(after_turn(Some(Kind::Hand), true), Some(Kind::Airing));
        assert_eq!(after_turn(Some(Kind::Hand), false), Some(Kind::Hand));
        assert_eq!(after_turn(Some(Kind::Airing), false), None);
        assert_eq!(after_turn(Some(Kind::Airing), true), Some(Kind::Airing));
    }

    #[test]
    fn a_hold_ends_at_its_length_or_at_the_next_step() {
        let min = |m: u64| Duration::from_secs(m * 60);
        let at = |kind, next| until(kind, 1_000, next, HAND_HOLD, AIRING_HOLD);
        assert_eq!(at(Kind::Hand, None), 1_000 + 7_200_000);
        assert_eq!(at(Kind::Hand, Some(min(45))), 1_000 + 2_700_000);
        assert_eq!(at(Kind::Hand, Some(min(300))), 1_000 + 7_200_000);
        assert_eq!(at(Kind::Airing, Some(min(300))), 1_000 + 1_800_000);
        assert_eq!(at(Kind::Airing, Some(min(10))), 1_000 + 600_000);
    }
}
