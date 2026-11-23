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
//!
//! The shift only comes down while a valve still heats: a room left warm by
//! a setpoint just lowered (20 at night, 18 by day) cools on its own, valves
//! shut, and a shift lowered all day long would leave them cold when the
//! setpoint goes back up in the evening.

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
/// setpoint changed: its battery, and the radio of a sleepy device. (A valve
/// turned by hand is not written at all for a while: [`crate::hand`].)
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

/// A valve whose thermometer reads more than this over the setpoint it
/// holds (°C) has shut: the room's excess is none of its doing.
pub const SHUT_MARGIN: f64 = 0.5;

/// Whether a valve still heats: its thermometer is not over the setpoint
/// it holds by more than [`SHUT_MARGIN`]. Its thermometer unknown, the
/// room's says it (a valve reads the room or warmer, by its radiator).
#[must_use]
pub fn valve_heats(held: f64, valve_temp: Option<f64>, room_temp: f64) -> bool {
    match valve_temp {
        Some(t) => t <= held + SHUT_MARGIN,
        None => room_temp <= held,
    }
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

    /// The room's error (`setpoint − temperature`), held for `elapsed`;
    /// `heating`: a valve still heats ([`valve_heats`]), or the room's
    /// excess does not lower the shift.
    pub fn update(&mut self, error: f64, elapsed: Duration, heating: bool) -> f64 {
        if error.is_finite() && error.abs() > DEADBAND && (error > 0.0 || heating) {
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

/// Whether the room asks for heat: its error (`setpoint − temperature`)
/// over `hysteresis`, it does; under `−hysteresis`, it does not; in between,
/// as `before` (none yet: whether it is under its setpoint). The sensor's
/// noise does not make it go back and forth.
#[must_use]
pub fn demand(before: Option<bool>, error: f64, hysteresis: f64) -> bool {
    if error > hysteresis {
        true
    } else if error < -hysteresis {
        false
    } else {
        before.unwrap_or(error > 0.0)
    }
}

/// `force`: the setpoint that opens the valve wide while the room asks for
/// heat (the room's plus `offset`), or shuts it (minus `offset`), on the
/// half degree, within the valve's bounds.
#[must_use]
pub fn forced_setpoint(room: f64, offset: f64, demand: bool, bounds: (f64, f64)) -> f64 {
    let target = if demand { room + offset } else { room - offset };
    half_degree(target).clamp(bounds.0, bounds.1)
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

/// `value` on the valve's `step` when it is coarser than the half degree (a
/// valve on the whole degree), within its bounds; else as it is.
#[must_use]
pub fn on_step(value: f64, step: Option<f64>, bounds: (f64, f64)) -> f64 {
    match step {
        Some(step) if step > 0.5 => ((value / step).round() * step).clamp(bounds.0, bounds.1),
        _ => value,
    }
}

/// Whether to write `next`: it differs from what the valve holds by half a
/// degree or more, and the last write is old enough: `interval` (usually
/// [`MIN_INTERVAL`]), or `floor` ([`URGENT_FLOOR`]) when `urgent` (the room's
/// setpoint or mode just changed).
#[must_use]
pub fn worth_sending(
    current: Option<f64>,
    next: f64,
    since_last: Option<Duration>,
    urgent: bool,
    (floor, interval): (Duration, Duration),
) -> bool {
    let moved = current.is_none_or(|c| (c - next).abs() >= 0.5 - 1e-9);
    let interval = if urgent { floor } else { interval };
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
            shift.update(0.5, 10 * MINUTE, true);
        }
        assert!((shift.get() - 0.5).abs() < 1e-9, "{}", shift.get());
        // A degree over for half an hour: − 0.5.
        for _ in 0..3 {
            shift.update(-1.0, 10 * MINUTE, true);
        }
        assert!(shift.get().abs() < 1e-9, "{}", shift.get());
    }

    #[test]
    fn the_shift_holds_near_the_setpoint_and_stays_bounded() {
        let mut shift = Shift::from(1.0);
        shift.update(0.15, 10 * MINUTE, true);
        shift.update(-0.2, 10 * MINUTE, true);
        assert_eq!(shift.get(), 1.0, "inside the deadband");
        // Hours of silence count as a quarter of an hour, never more.
        shift.update(2.0, 5 * 60 * MINUTE, true);
        assert!((shift.get() - 1.5).abs() < 1e-9, "{}", shift.get());
        for _ in 0..100 {
            shift.update(5.0, 15 * MINUTE, true);
        }
        assert_eq!(shift.get(), SHIFT_RANGE.1);
        assert_eq!(Shift::from(-50.0).get(), SHIFT_RANGE.0);
        assert_eq!(Shift::from(f64::NAN).get(), 0.0);
    }

    #[test]
    fn a_room_warm_from_a_lowered_setpoint_does_not_lower_the_shift() {
        // The night's 19.5 learned a shift of 1; at 7:00 the setpoint is 18,
        // the room still at 19.3 and the valve, holding 19, reads 20.4: shut.
        let mut shift = Shift::from(1.0);
        assert!(!valve_heats(19.0, Some(20.4), 19.3));
        for _ in 0..40 {
            shift.update(
                18.0 - 19.3,
                15 * MINUTE,
                valve_heats(19.0, Some(20.4), 19.3),
            );
        }
        assert_eq!(shift.get(), 1.0, "held all day");
        // Still too warm with the valve open (its thermometer at what it
        // holds): the shift comes down.
        assert!(valve_heats(19.0, Some(19.2), 19.3));
        shift.update(-1.0, 15 * MINUTE, true);
        assert!(shift.get() < 1.0);
        // Too cold, shut or not: the shift goes up.
        let before = shift.get();
        shift.update(1.0, 10 * MINUTE, false);
        assert!(shift.get() > before);
        // No thermometer on the valve: the room's.
        assert!(valve_heats(19.0, None, 18.8));
        assert!(!valve_heats(19.0, None, 19.3));
    }

    #[test]
    fn the_demand_goes_back_and_forth_only_past_the_hysteresis() {
        assert!(demand(None, 0.1, 0.3), "under its setpoint");
        assert!(!demand(None, -0.1, 0.3), "over it");
        assert!(demand(Some(false), 0.4, 0.3));
        assert!(!demand(Some(false), 0.3, 0.3), "inside: as before");
        assert!(demand(Some(true), -0.3, 0.3), "inside: as before");
        assert!(!demand(Some(true), -0.4, 0.3));
        assert!(demand(Some(false), 0.01, 0.0));
    }

    #[test]
    fn a_forced_valve_is_sent_far_over_or_under_the_room() {
        let bounds = (5.0, 30.0);
        assert_eq!(forced_setpoint(20.0, 4.0, true, bounds), 24.0);
        assert_eq!(forced_setpoint(20.0, 4.0, false, bounds), 16.0);
        assert_eq!(forced_setpoint(19.5, 2.2, true, bounds), 21.5);
        assert_eq!(
            forced_setpoint(28.0, 4.0, true, bounds),
            30.0,
            "valve bounds"
        );
        assert_eq!(
            forced_setpoint(7.0, 4.0, false, bounds),
            5.0,
            "valve bounds"
        );
    }

    #[test]
    fn a_valve_on_the_whole_degree_is_given_whole_degrees() {
        assert!((on_step(19.5, Some(1.0), (5.0, 30.0)) - 20.0).abs() < 1e-9);
        assert!((on_step(19.0, Some(1.0), (5.0, 30.0)) - 19.0).abs() < 1e-9);
        assert!((on_step(19.5, Some(0.5), (5.0, 30.0)) - 19.5).abs() < 1e-9);
        assert!((on_step(19.5, None, (5.0, 30.0)) - 19.5).abs() < 1e-9);
        assert!((on_step(29.6, Some(2.0), (5.0, 29.0)) - 29.0).abs() < 1e-9);
    }

    #[test]
    fn a_valve_is_written_on_a_real_change_and_not_too_often() {
        let long = Some(MIN_INTERVAL);
        let short = Some(MINUTE);
        assert!(worth_sending(
            None,
            20.0,
            None,
            false,
            (URGENT_FLOOR, MIN_INTERVAL)
        ));
        assert!(worth_sending(
            Some(20.0),
            20.5,
            long,
            false,
            (URGENT_FLOOR, MIN_INTERVAL)
        ));
        assert!(!worth_sending(
            Some(20.0),
            20.0,
            long,
            true,
            (URGENT_FLOOR, MIN_INTERVAL)
        ));
        assert!(
            !worth_sending(Some(20.2), 20.5, long, false, (URGENT_FLOOR, MIN_INTERVAL)),
            "under half a degree"
        );
        assert!(
            !worth_sending(Some(20.0), 21.0, short, false, (URGENT_FLOOR, MIN_INTERVAL)),
            "too soon"
        );
        assert!(
            worth_sending(Some(20.0), 21.0, short, true, (URGENT_FLOOR, MIN_INTERVAL)),
            "the room changed"
        );
        let hurried = Some(Duration::from_secs(5));
        assert!(
            !worth_sending(
                Some(20.0),
                21.0,
                hurried,
                true,
                (URGENT_FLOOR, MIN_INTERVAL)
            ),
            "not twice in a hurry"
        );
        assert!(
            too_soon(hurried, URGENT_FLOOR)
                && !too_soon(short, URGENT_FLOOR)
                && !too_soon(None, URGENT_FLOOR)
        );
    }
}
