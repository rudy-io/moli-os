//! The valve's thermometer sits by the radiator: it reads warmer than the room.
//! The gap between the two, smoothed, moves the setpoint the valve is given,
//! so that the valve, regulating on its own thermometer, holds the room's.
//!
//! Example: the room should be at 20 °C, its sensor reads 20.9 and the valve
//! 22: the gap is 1.1, the valve is given 21 (20 + 1.1, on the half degree).

use std::time::Duration;

/// How quickly the gap follows a new reading: about two thirds of a change
/// after this long. Slower than the radiator, so the valve opening and
/// closing does not chase its own heat.
pub const SMOOTHING: Duration = Duration::from_secs(20 * 60);

/// The gap never moves the setpoint by more than this (°C): a sensor in the
/// sun or by an open window must not send the valve to its limits.
pub const GAP_RANGE: (f64, f64) = (-3.0, 6.0);

/// A valve is written again at most this often, except when the room's own
/// setpoint changed: its battery, and the radio of a sleepy device.
pub const MIN_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// The smoothed gap between a valve's thermometer and the room's sensor.
#[derive(Clone, Debug, Default)]
pub struct Gap {
    value: Option<f64>,
}

impl Gap {
    /// A new reading of the gap, `elapsed` after the previous one.
    pub fn update(&mut self, raw: f64, elapsed: Duration) -> f64 {
        let next = match self.value {
            None => raw,
            Some(value) => {
                let weight = 1.0 - (-elapsed.as_secs_f64() / SMOOTHING.as_secs_f64()).exp();
                value + weight * (raw - value)
            }
        };
        self.value = Some(next);
        next
    }

    /// The room's sensor or the valve's thermometer went quiet: start over.
    pub fn forget(&mut self) {
        self.value = None;
    }

    #[must_use]
    pub fn get(&self) -> Option<f64> {
        self.value
    }
}

/// On the half degree.
#[must_use]
pub fn half_degree(temp: f64) -> f64 {
    (temp * 2.0).round() / 2.0
}

/// The setpoint the valve is given: the room's, moved by the gap (bounded by
/// [`GAP_RANGE`]), on the half degree, within the valve's own bounds.
#[must_use]
pub fn valve_setpoint(room: f64, gap: Option<f64>, bounds: (f64, f64)) -> f64 {
    let moved = room + gap.map_or(0.0, |g| g.clamp(GAP_RANGE.0, GAP_RANGE.1));
    half_degree(moved).clamp(bounds.0, bounds.1)
}

/// Whether to write `next`: it differs from what the valve holds by half a
/// degree or more, and the last write is old enough, unless `urgent` (the
/// room's setpoint or mode just changed).
#[must_use]
pub fn worth_sending(
    current: Option<f64>,
    next: f64,
    since_last: Option<Duration>,
    urgent: bool,
) -> bool {
    let moved = current.is_none_or(|c| (c - next).abs() >= 0.5 - 1e-9);
    moved && (urgent || since_last.is_none_or(|d| d >= MIN_INTERVAL))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gap_moves_the_setpoint_on_the_half_degree_within_bounds() {
        let bounds = (5.0, 30.0);
        assert_eq!(valve_setpoint(20.0, Some(22.0 - 20.9), bounds), 21.0);
        assert_eq!(valve_setpoint(20.0, None, bounds), 20.0);
        assert_eq!(valve_setpoint(20.0, Some(-1.3), bounds), 18.5);
        assert_eq!(
            valve_setpoint(20.0, Some(15.0), bounds),
            26.0,
            "gap bounded"
        );
        assert_eq!(
            valve_setpoint(20.0, Some(-9.0), bounds),
            17.0,
            "gap bounded"
        );
        assert_eq!(
            valve_setpoint(28.0, Some(5.0), bounds),
            30.0,
            "valve bounds"
        );
        assert_eq!(valve_setpoint(5.0, Some(-2.0), bounds), 5.0, "valve bounds");
    }

    #[test]
    fn the_gap_follows_slowly() {
        let mut gap = Gap::default();
        assert_eq!(gap.update(2.0, Duration::ZERO), 2.0, "first reading as is");
        let after = gap.update(0.0, SMOOTHING);
        assert!((after - 2.0 * (-1.0f64).exp()).abs() < 1e-9, "{after}");
        assert!(gap.update(0.0, Duration::from_secs(60)) < after);
        gap.forget();
        assert_eq!(gap.get(), None);
    }

    #[test]
    fn a_valve_is_written_on_a_real_change_and_not_too_often() {
        let long = Some(MIN_INTERVAL);
        let short = Some(Duration::from_secs(60));
        assert!(worth_sending(None, 20.0, None, false));
        assert!(worth_sending(Some(20.0), 20.5, long, false));
        assert!(!worth_sending(Some(20.0), 20.0, long, true));
        assert!(
            !worth_sending(Some(20.2), 20.5, long, false),
            "under half a degree"
        );
        assert!(!worth_sending(Some(20.0), 21.0, short, false), "too soon");
        assert!(
            worth_sending(Some(20.0), 21.0, short, true),
            "the room changed"
        );
    }
}
