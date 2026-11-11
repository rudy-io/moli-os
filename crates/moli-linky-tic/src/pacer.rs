//! Calms the points that change with almost every frame.
//!
//! A meter repeats its frame every second or two, and instantaneous power and
//! currents move a little each time. The history keeps every change, so publishing
//! them all would write one row per frame and per point, for weeks, often on an SD
//! card. A fast point is published at most once per interval, unless it moved by a
//! real step (a kettle switched on): an automation still sees that at once. The
//! energy indexes are not paced: they only move when energy is drawn.

use std::collections::HashMap;
use std::time::Duration;

use moli_core::Value;
use tokio::time::Instant;

/// The step, in the point's unit, from which a change goes out at once.
fn jump(key: &str) -> Option<f64> {
    match key {
        "apparent_power" => Some(200.0),
        "voltage_1" => Some(5.0),
        _ if key.starts_with("current_") => Some(1.0),
        _ => None,
    }
}

#[derive(Debug)]
pub struct Pacer {
    every: Duration,
    last: HashMap<String, (Instant, f64)>,
}

impl Pacer {
    /// `every` zero publishes everything.
    #[must_use]
    pub fn new(every: Duration) -> Self {
        Self {
            every,
            last: HashMap::new(),
        }
    }

    /// Whether `key` takes `value` at `now`. A yes is remembered as the last publication.
    pub fn allow(&mut self, key: &str, value: &Value, now: Instant) -> bool {
        let Some(step) = jump(key) else {
            return true;
        };
        let Some(number) = value.as_f64() else {
            return true;
        };
        if self.every.is_zero() {
            return true;
        }
        let due = match self.last.get(key) {
            Some((at, previous)) => {
                now.duration_since(*at) >= self.every || (number - previous).abs() >= step
            }
            None => true,
        };
        if due {
            self.last.insert(key.to_owned(), (now, number));
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, seconds: u64) -> Instant {
        base + Duration::from_secs(seconds)
    }

    #[test]
    fn a_fast_point_goes_out_once_per_interval() {
        let base = Instant::now();
        let mut pacer = Pacer::new(Duration::from_secs(10));
        let power = |n: f64| Value::Float(n);
        assert!(pacer.allow("apparent_power", &power(500.0), at(base, 0)));
        // Frames every 1.5 s with small moves: nothing goes out for ten seconds.
        for second in 1_u32..10 {
            let value = power(500.0 + 5.0 * f64::from(second));
            assert!(
                !pacer.allow("apparent_power", &value, at(base, u64::from(second))),
                "second {second}"
            );
        }
        assert!(pacer.allow("apparent_power", &power(540.0), at(base, 10)));
        assert!(!pacer.allow("apparent_power", &power(541.0), at(base, 11)));
    }

    #[test]
    fn a_real_step_goes_out_at_once() {
        let base = Instant::now();
        let mut pacer = Pacer::new(Duration::from_secs(10));
        assert!(pacer.allow("apparent_power", &Value::Float(500.0), at(base, 0)));
        // A kettle: +2 kVA two seconds later.
        assert!(pacer.allow("apparent_power", &Value::Float(2500.0), at(base, 2)));
        // Measured against the last published value, not the first.
        assert!(!pacer.allow("apparent_power", &Value::Float(2550.0), at(base, 3)));
    }

    #[test]
    fn each_point_has_its_own_clock() {
        let base = Instant::now();
        let mut pacer = Pacer::new(Duration::from_secs(10));
        assert!(pacer.allow("apparent_power", &Value::Float(500.0), at(base, 0)));
        assert!(pacer.allow("current_1", &Value::Float(2.0), at(base, 1)));
        assert!(!pacer.allow("current_1", &Value::Float(2.0), at(base, 2)));
    }

    #[test]
    fn energy_and_text_are_never_paced() {
        let base = Instant::now();
        let mut pacer = Pacer::new(Duration::from_secs(10));
        for second in 0_u32..5 {
            let when = at(base, u64::from(second));
            let total = Value::Float(1000.0 + f64::from(second));
            assert!(pacer.allow("energy_total", &total, when));
            assert!(pacer.allow("energy_index_02", &Value::Float(1.0), when));
            assert!(pacer.allow("tariff", &Value::Text("HEURE PLEINE".into()), when));
        }
    }

    #[test]
    fn zero_publishes_everything() {
        let base = Instant::now();
        let mut pacer = Pacer::new(Duration::ZERO);
        for second in 0_u64..5 {
            assert!(pacer.allow("apparent_power", &Value::Float(500.0), at(base, second)));
        }
    }
}
