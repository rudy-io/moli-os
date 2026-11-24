//! Temporary changes to a room, which end on their own:
//!
//! - **A boost**: some degrees added to the room's setpoint, the schedule's
//!   included, for a while (`boost`, `boost_minutes`, `boost_until`).
//! - **A setpoint held**: in `auto`, a setpoint chosen by hand holds for a
//!   while instead of until the next step of the schedule, then the
//!   schedule takes over again (`hold_minutes`, `hold_until`).
//!
//! Their ends are kept across a restart of Moli; a change of the room's mode
//! ends both. Nothing to do with a valve turned by hand ([`crate::hand`]).

use std::time::Duration;

use moli_core::Value;

/// The longest a boost or a hold lasts, in minutes.
pub const MAX_MINUTES: u32 = 720;

/// Something asked for `minutes`, that ends at `until_ms` (Unix ms).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timed {
    pub minutes: u32,
    pub until_ms: u64,
}

impl Timed {
    /// Asked at `now_ms` for `minutes`, each one `minute` long.
    #[must_use]
    pub fn start(minutes: u32, now_ms: u64, minute: Duration) -> Self {
        let length = minute.saturating_mul(minutes).as_millis();
        Self {
            minutes,
            until_ms: now_ms.saturating_add(u64::try_from(length).unwrap_or(u64::MAX)),
        }
    }

    #[must_use]
    pub fn due(self, now_ms: u64) -> bool {
        now_ms >= self.until_ms
    }

    #[must_use]
    pub fn left(self, now_ms: u64) -> Duration {
        Duration::from_millis(self.until_ms.saturating_sub(now_ms))
    }
}

/// Some degrees added to the room's setpoint until its end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Boost {
    pub by: f64,
    pub timed: Timed,
}

/// Whether `by` is a boost the room accepts: 0 to `max`, on the half degree.
#[must_use]
pub fn boost_ok(by: f64, max: f64) -> bool {
    (0.0..=max).contains(&by) && ((by * 2.0).round() - by * 2.0).abs() < 1e-9
}

/// A length in whole minutes, 0 to [`MAX_MINUTES`].
#[must_use]
pub fn minutes(value: &Value) -> Option<u32> {
    let m = value.as_f64()?;
    (0..=MAX_MINUTES).find(|&n| (f64::from(n) - m).abs() < 1e-9)
}

/// An end as the room shows it: RFC 3339, empty without one.
#[must_use]
pub fn show(until_ms: Option<u64>) -> Value {
    until_ms
        .and_then(|ms| jiff::Timestamp::from_millisecond(i64::try_from(ms).ok()?).ok())
        .map_or(Value::Null, |t| Value::from(t.to_string().as_str()))
}

/// An end shown before a restart, back in Unix ms.
#[must_use]
pub fn read(value: Option<&Value>) -> Option<u64> {
    let Some(Value::Text(text)) = value else {
        return None;
    };
    let at: jiff::Timestamp = text.parse().ok()?;
    u64::try_from(at.as_millisecond()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_boost_is_on_the_half_degree_within_its_maximum() {
        assert!(boost_ok(0.0, 3.0));
        assert!(boost_ok(2.5, 3.0));
        assert!(boost_ok(3.0, 3.0));
        assert!(!boost_ok(3.5, 3.0));
        assert!(!boost_ok(1.2, 3.0));
        assert!(!boost_ok(-0.5, 3.0));
        assert!(!boost_ok(f64::NAN, 3.0));
    }

    #[test]
    fn a_length_is_whole_minutes_up_to_twelve_hours() {
        assert_eq!(minutes(&Value::Int(0)), Some(0));
        assert_eq!(minutes(&Value::Float(180.0)), Some(180));
        assert_eq!(minutes(&Value::Int(720)), Some(720));
        assert_eq!(minutes(&Value::Int(721)), None);
        assert_eq!(minutes(&Value::Float(1.5)), None);
        assert_eq!(minutes(&Value::Int(-1)), None);
        assert_eq!(minutes(&Value::from("60")), None);
    }

    #[test]
    fn an_end_is_shown_and_read_back() {
        let timed = Timed::start(60, 1_000, Duration::from_secs(60));
        assert_eq!(timed.until_ms, 3_601_000);
        assert!(!timed.due(3_600_999));
        assert!(timed.due(3_601_000));
        assert_eq!(timed.left(1_000), Duration::from_secs(3600));
        assert_eq!(timed.left(4_000_000), Duration::ZERO);

        let ms = 1_790_000_000_000;
        let shown = show(Some(ms));
        assert_eq!(shown, Value::from("2026-09-21T14:13:20Z"));
        assert_eq!(read(Some(&shown)), Some(ms));
        assert_eq!(show(None), Value::Null);
        assert_eq!(read(Some(&Value::Null)), None);
        assert_eq!(read(Some(&Value::from("tomorrow"))), None);
        assert_eq!(read(None), None);
    }
}
