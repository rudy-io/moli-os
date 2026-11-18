//! A valve regulates on its own thermometer, by the radiator: given the
//! room's setpoint, it stops too early (it feels warmer than the room) or,
//! far from the radiator, too late. Moli moves the setpoint it gives the
//! valve by a **shift** learned from the room's own error, read on the
//! room's sensor:
//!
//! - the room stays under its setpoint: the shift grows, slowly;
//! - the room goes over it: the shift comes down;
//! - the room is where it should be: the shift holds.
//!
//! The valve's thermometer takes no part: a radiator heating up can never
//! push the valve to heat more (what a gap « valve − room » did: the valve
//! warms, the gap grows, the setpoint follows, the valve stays open).
//!
//! Example: the room should be at 20 °C and stays at 19.5 for an hour: the
//! shift grows by about 0.5, the valve is given 20.5.

use std::time::Duration;

/// How fast the shift follows the room's error: °C of shift per hour, per
/// °C of error. Slower than a radiator warms a room, so the valve has time
/// to act on one shift before the next.
pub const GAIN_PER_HOUR: f64 = 1.0;

/// An error this small (°C) moves nothing: a sensor's noise, half a step of
/// the setpoint.
pub const DEADBAND: f64 = 0.2;

/// The shift never moves the setpoint by more than this (°C): a sensor in
/// the sun or by an open window must not send the valve to its limits.
pub const SHIFT_RANGE: (f64, f64) = (-3.0, 6.0);

/// At most this much time counts at once (a sensor silent for hours, then
/// back, does not move the shift by hours at a stroke).
pub const MAX_STEP: Duration = Duration::from_secs(15 * 60);

/// A valve is written again at most this often, except when the room's own
/// setpoint changed: its battery, and the radio of a sleepy device.
pub const MIN_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Even when the room changed, a valve is not written again sooner than
/// this: an order repeated in a hurry is not sent to it each time.
pub const URGENT_FLOOR: Duration = Duration::from_secs(30);

/// The last write is too recent for even an urgent one (`floor`: usually
/// [`URGENT_FLOOR`]).
#[must_use]
pub fn too_soon(since_last: Option<Duration>, floor: Duration) -> bool {
    since_last.is_some_and(|d| d < floor)
}

/// The shift a room's valves are given, learned from the room's error.
#[derive(Clone, Debug, Default)]
pub struct Shift {
    value: f64,
}

impl Shift {
    /// Starts from a shift kept from before (bounded).
    #[must_use]
    pub fn from(value: f64) -> Self {
        let value = if value.is_finite() { value } else { 0.0 };
        Self {
            value: value.clamp(SHIFT_RANGE.0, SHIFT_RANGE.1),
        }
    }

    /// The room's error (`setpoint − temperature`), held for `elapsed`.
    pub fn update(&mut self, error: f64, elapsed: Duration) -> f64 {
        if error.is_finite() && error.abs() > DEADBAND {
            let hours = elapsed.min(MAX_STEP).as_secs_f64() / 3600.0;
            self.value =
                (self.value + GAIN_PER_HOUR * error * hours).clamp(SHIFT_RANGE.0, SHIFT_RANGE.1);
        }
        self.value
    }

    #[must_use]
    pub fn get(&self) -> f64 {
        self.value
    }
}

/// On the half degree.
#[must_use]
pub fn half_degree(temp: f64) -> f64 {
    (temp * 2.0).round() / 2.0
}

/// The setpoint the valve is given: the room's, moved by the shift, on the
/// half degree, within the valve's own bounds.
#[must_use]
pub fn valve_setpoint(room: f64, shift: f64, bounds: (f64, f64)) -> f64 {
    half_degree(room + shift.clamp(SHIFT_RANGE.0, SHIFT_RANGE.1)).clamp(bounds.0, bounds.1)
}

/// Whether to write `next`: it differs from what the valve holds by half a
/// degree or more, and the last write is old enough: [`MIN_INTERVAL`], or
/// `floor` ([`URGENT_FLOOR`]) when `urgent` (the room's setpoint or mode just
/// changed).
#[must_use]
pub fn worth_sending(
    current: Option<f64>,
    next: f64,
    since_last: Option<Duration>,
    urgent: bool,
    floor: Duration,
) -> bool {
    let moved = current.is_none_or(|c| (c - next).abs() >= 0.5 - 1e-9);
    let interval = if urgent { floor } else { MIN_INTERVAL };
    moved && since_last.is_none_or(|d| d >= interval)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn the_shift_moves_the_setpoint_on_the_half_degree_within_bounds() {
        let bounds = (5.0, 30.0);
        assert_eq!(valve_setpoint(20.0, 0.0, bounds), 20.0);
        assert_eq!(valve_setpoint(20.0, 1.1, bounds), 21.0);
        assert_eq!(valve_setpoint(20.0, -1.3, bounds), 18.5);
        assert_eq!(valve_setpoint(20.0, 15.0, bounds), 26.0, "shift bounded");
        assert_eq!(valve_setpoint(20.0, -9.0, bounds), 17.0, "shift bounded");
        assert_eq!(valve_setpoint(28.0, 5.0, bounds), 30.0, "valve bounds");
        assert_eq!(valve_setpoint(5.0, -2.0, bounds), 5.0, "valve bounds");
    }

    #[test]
    fn a_cold_room_raises_the_shift_slowly_a_warm_one_lowers_it() {
        let mut shift = Shift::default();
        // Half a degree under for an hour (in 10-minute readings): + 0.5.
        for _ in 0..6 {
            shift.update(0.5, 10 * MINUTE);
        }
        assert!((shift.get() - 0.5).abs() < 1e-9, "{}", shift.get());
        // A degree over for half an hour: − 0.5.
        for _ in 0..3 {
            shift.update(-1.0, 10 * MINUTE);
        }
        assert!(shift.get().abs() < 1e-9, "{}", shift.get());
    }

    #[test]
    fn the_shift_holds_near_the_setpoint_and_stays_bounded() {
        let mut shift = Shift::from(1.0);
        shift.update(0.15, 10 * MINUTE);
        shift.update(-0.2, 10 * MINUTE);
        assert_eq!(shift.get(), 1.0, "inside the deadband");
        // Hours of silence count as a quarter of an hour, never more.
        shift.update(2.0, 5 * 60 * MINUTE);
        assert!((shift.get() - 1.5).abs() < 1e-9, "{}", shift.get());
        for _ in 0..100 {
            shift.update(5.0, 15 * MINUTE);
        }
        assert_eq!(shift.get(), SHIFT_RANGE.1);
        assert_eq!(Shift::from(-50.0).get(), SHIFT_RANGE.0);
        assert_eq!(Shift::from(f64::NAN).get(), 0.0);
    }

    #[test]
    fn a_valve_is_written_on_a_real_change_and_not_too_often() {
        let long = Some(MIN_INTERVAL);
        let short = Some(MINUTE);
        assert!(worth_sending(None, 20.0, None, false, URGENT_FLOOR));
        assert!(worth_sending(Some(20.0), 20.5, long, false, URGENT_FLOOR));
        assert!(!worth_sending(Some(20.0), 20.0, long, true, URGENT_FLOOR));
        assert!(
            !worth_sending(Some(20.2), 20.5, long, false, URGENT_FLOOR),
            "under half a degree"
        );
        assert!(
            !worth_sending(Some(20.0), 21.0, short, false, URGENT_FLOOR),
            "too soon"
        );
        assert!(
            worth_sending(Some(20.0), 21.0, short, true, URGENT_FLOOR),
            "the room changed"
        );
        let hurried = Some(Duration::from_secs(5));
        assert!(
            !worth_sending(Some(20.0), 21.0, hurried, true, URGENT_FLOOR),
            "not twice in a hurry"
        );
        assert!(
            too_soon(hurried, URGENT_FLOOR)
                && !too_soon(short, URGENT_FLOOR)
                && !too_soon(None, URGENT_FLOOR)
        );
    }
}
