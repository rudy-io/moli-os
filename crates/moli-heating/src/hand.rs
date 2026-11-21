//! A valve turned by hand (on its dial, or from the dashboard): Moli keeps
//! that setpoint for a while instead of putting its own back, then goes
//! back to the room's.
//!
//! - **By hand**: the setpoint holds for [`HAND_HOLD`] at most, and only
//!   until the next step of the schedule.
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

/// Whether the valve's setpoint was moved by someone else than Moli: it
/// changed since the last look (`seen`) and is not what Moli last wrote
/// (`sent`). The first look (`seen` unknown, after a start) says nothing.
#[must_use]
pub fn turned(seen: Option<f64>, current: f64, sent: Option<f64>) -> bool {
    seen.is_some_and(|s| (s - current).abs() > 0.05)
        && sent.is_none_or(|w| (w - current).abs() >= 0.25)
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

/// When a hold of `kind` made at `now_ms` ends: its own length, or the next
/// step of the schedule if it comes sooner (`to_next_step`, `None` when the
/// room does not follow its schedule).
#[must_use]
pub fn until(
    kind: Kind,
    now_ms: u64,
    to_next_step: Option<Duration>,
    hand: Duration,
    airing: Duration,
) -> u64 {
    let own = match kind {
        Kind::Hand => hand,
        Kind::Airing => airing,
    };
    let length = to_next_step.map_or(own, |next| own.min(next));
    now_ms.saturating_add(u64::try_from(length.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valve_is_turned_by_hand_only_when_it_moved_without_moli() {
        // The first look after a start says nothing.
        assert!(!turned(None, 12.0, Some(19.0)));
        // Moved, and not what Moli wrote.
        assert!(turned(Some(19.0), 12.0, Some(19.0)));
        assert!(turned(Some(19.0), 19.5, None));
        // Moli's own write coming back.
        assert!(!turned(Some(18.0), 19.0, Some(19.0)));
        // Not moved: a write the valve did not take is not a hand.
        assert!(!turned(Some(18.0), 18.0, Some(19.0)));
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
