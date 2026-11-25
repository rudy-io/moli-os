//! An open window or door: the room stops asking for heat and its valves
//! get the opening's setpoint for a while, like airing by hand ([`crate::hand`])
//! but for the whole room.
//!
//! - **A contact** of the room says it is open for `opening_delay`: until it
//!   closes again.
//! - **No contact** (`auto_opening`): the room's sensor, smoothed, falls by
//!   `auto_opening_drop` °C per hour or more. It ends when the temperature
//!   rises again ([`RISE`]) or after `auto_opening_max`; after that, it waits
//!   as long again before it can start again, so it stays bounded.
//!
//! The slope uses the readings the room already gets: each new reading is
//! smoothed by the median of the last three (a single low reading moves
//! nothing), and the slope is taken over [`WINDOW`], on [`SPAN`] at least. A
//! silence longer than the window starts it again from nothing: a sensor
//! going quiet is never a drop.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The slope is taken over the smoothed readings of this long at most.
pub const WINDOW: Duration = Duration::from_secs(20 * 60);

/// …and of this long at least: two readings a minute apart say nothing.
pub const SPAN: Duration = Duration::from_secs(10 * 60);

/// A drop ends once the smoothed temperature is this much (°C) over its
/// lowest since it started.
pub const RISE: f64 = 0.2;

/// Why a room is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Contact,
    Drop,
}

impl Reason {
    /// The room's `opening` point.
    #[must_use]
    pub fn name(reason: Option<Self>) -> &'static str {
        match reason {
            None => "none",
            Some(Self::Contact) => "contact",
            Some(Self::Drop) => "drop",
        }
    }
}

/// The room's sensor, smoothed, and how fast it moves.
#[derive(Clone, Debug, Default)]
pub struct Slope {
    /// The last raw readings (Unix ms, °C), three at most.
    raw: VecDeque<(u64, f64)>,
    /// The smoothed ones, over the window.
    smooth: VecDeque<(u64, f64)>,
}

impl Slope {
    /// A reading taken at `ts` (Unix ms); the same one again is ignored.
    pub fn read(&mut self, ts: u64, temp: f64, window: Duration) {
        let window_ms = u64::try_from(window.as_millis()).unwrap_or(u64::MAX);
        match self.raw.back() {
            Some(&(last, _)) if ts <= last => return,
            Some(&(last, _)) if ts - last > window_ms => self.clear(),
            _ => {}
        }
        self.raw.push_back((ts, temp));
        if self.raw.len() > 3 {
            self.raw.pop_front();
        }
        if self.raw.len() == 3 {
            let mut three: Vec<f64> = self.raw.iter().map(|r| r.1).collect();
            three.sort_by(f64::total_cmp);
            self.smooth.push_back((ts, three[1]));
        }
        while self
            .smooth
            .front()
            .is_some_and(|&(t, _)| ts.saturating_sub(t) > window_ms)
        {
            self.smooth.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.raw.clear();
        self.smooth.clear();
    }

    /// The latest smoothed temperature.
    #[must_use]
    pub fn smoothed(&self) -> Option<f64> {
        self.smooth.back().map(|s| s.1)
    }

    /// °C per hour over the window, once it spans `span` at least.
    #[must_use]
    pub fn per_hour(&self, span: Duration) -> Option<f64> {
        let (&(t0, v0), &(t1, v1)) = (self.smooth.front()?, self.smooth.back()?);
        let elapsed = t1.saturating_sub(t0);
        if elapsed == 0 || u128::from(elapsed) < span.as_millis() {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let hours = elapsed as f64 / 3_600_000.0;
        Some((v1 - v0) / hours)
    }
}

/// A drop of the temperature taken for an open window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drop {
    pub until: Instant,
    /// The lowest smoothed temperature since it started.
    pub lowest: f64,
}

/// What a step of the drop detection did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Started,
    /// The temperature rose again.
    Rose,
    /// `auto_opening_max` reached.
    Expired,
}

/// The detection without a contact.
#[derive(Clone, Debug, Default)]
pub struct Detector {
    pub slope: Slope,
    pub drop: Option<Drop>,
    /// No new drop before then, after one that ran its full length.
    rearm: Option<Instant>,
}

impl Detector {
    /// One look: the slope fast enough under `-drop_per_hour` starts a drop
    /// of `max` at most; a drop under way ends on a rise or at its end.
    pub fn step(
        &mut self,
        now: Instant,
        drop_per_hour: f64,
        max: Duration,
        span: Duration,
    ) -> Option<Change> {
        let smoothed = self.slope.smoothed();
        if let Some(drop) = &mut self.drop {
            if now >= drop.until {
                self.drop = None;
                self.rearm = Some(now + max);
                self.slope.clear();
                return Some(Change::Expired);
            }
            let t = smoothed?;
            if t >= drop.lowest + RISE {
                self.drop = None;
                self.slope.clear();
                return Some(Change::Rose);
            }
            drop.lowest = drop.lowest.min(t);
            return None;
        }
        if self.rearm.is_some_and(|r| now < r) {
            return None;
        }
        self.rearm = None;
        let (Some(t), Some(slope)) = (smoothed, self.slope.per_hour(span)) else {
            return None;
        };
        (slope <= -drop_per_hour).then(|| {
            self.drop = Some(Drop {
                until: now + max,
                lowest: t,
            });
            Change::Started
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;

    fn slope_of(readings: &[(u64, f64)]) -> Slope {
        let mut slope = Slope::default();
        for &(minute, t) in readings {
            slope.read(minute * MIN, t, WINDOW);
        }
        slope
    }

    #[test]
    fn a_fast_fall_is_seen_a_single_low_reading_is_not() {
        // 0.3 °C every 5 min: 3.6 °C per hour.
        let falling = slope_of(&[(0, 20.0), (5, 19.7), (10, 19.4), (15, 19.1), (20, 18.8)]);
        let per_hour = falling.per_hour(SPAN).unwrap();
        assert!((per_hour + 3.6).abs() < 1e-9, "{per_hour}");

        let blip = slope_of(&[(0, 20.0), (5, 20.0), (10, 20.0), (15, 16.0), (20, 20.0)]);
        assert_eq!(blip.per_hour(SPAN), Some(0.0));
        let last = slope_of(&[(0, 20.0), (5, 20.0), (10, 20.0), (15, 20.0), (20, 16.0)]);
        assert_eq!(last.per_hour(SPAN), Some(0.0));
    }

    #[test]
    fn too_short_or_after_a_silence_says_nothing() {
        let short = slope_of(&[(0, 20.0), (1, 19.0), (2, 18.0), (3, 17.0)]);
        assert_eq!(short.per_hour(SPAN), None);
        // Quiet for an hour, back much lower: no slope.
        let quiet = slope_of(&[(0, 20.0), (5, 20.0), (10, 20.0), (70, 16.0), (75, 16.0)]);
        assert_eq!(quiet.per_hour(SPAN), None);
        assert_eq!(quiet.smoothed(), None);
        // The same reading twice is one reading.
        let mut same = slope_of(&[(0, 20.0), (5, 20.0)]);
        same.read(5 * MIN, 20.0, WINDOW);
        assert_eq!(same.smoothed(), None);
    }

    #[test]
    fn a_drop_starts_on_a_fast_fall_and_ends_on_a_rise_or_at_its_end() {
        let max = Duration::from_secs(30 * 60);
        let start = Instant::now();
        let falling = [(0, 20.0), (5, 19.7), (10, 19.4), (15, 19.1), (20, 18.8)];
        let mut detector = Detector {
            slope: slope_of(&falling),
            ..Detector::default()
        };
        assert_eq!(detector.step(start, 4.0, max, SPAN), None, "3.6 < 4");
        assert_eq!(detector.step(start, 3.0, max, SPAN), Some(Change::Started));
        assert_eq!(detector.step(start, 3.0, max, SPAN), None, "under way");
        // Back up by a little: still open; by RISE: over.
        for (minute, t) in [(25, 19.0), (30, 19.1), (35, 19.4)] {
            detector.slope.read(minute * MIN, t, WINDOW);
            assert_eq!(detector.step(start, 3.0, max, SPAN), None, "{minute}");
        }
        detector.slope.read(40 * MIN, 19.5, WINDOW);
        assert_eq!(detector.step(start, 3.0, max, SPAN), Some(Change::Rose));
        assert!(detector.drop.is_none());

        // Falling on and on: over at its end, then not again for as long.
        let mut detector = Detector {
            slope: slope_of(&falling),
            ..Detector::default()
        };
        assert_eq!(detector.step(start, 3.0, max, SPAN), Some(Change::Started));
        let end = start + max;
        assert_eq!(detector.step(end, 3.0, max, SPAN), Some(Change::Expired));
        detector.slope = slope_of(&[(40, 18.0), (45, 17.7), (50, 17.4), (55, 17.1), (60, 16.8)]);
        assert_eq!(detector.step(end + max / 2, 3.0, max, SPAN), None);
        assert_eq!(
            detector.step(end + max, 3.0, max, SPAN),
            Some(Change::Started)
        );
    }
}
