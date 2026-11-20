//! Hourly statistics of a step series. Pure: no clock, no storage.
//!
//! History records changes only: a value holds from its time until the next
//! one. An hour's average is therefore weighted by how long each value held
//! (a temperature that stayed at 4 °C for 50 minutes and was 6 °C for 10 is
//! 4.3 °C, not 5). A null or non-numeric value is a gap: it counts for
//! nothing, and an hour with no known value has no statistics at all.

use serde::{Deserialize, Serialize};

pub const HOUR_MS: u64 = 3_600_000;

/// One hour of a point: time-weighted average, lowest and highest value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hour {
    /// Start of the hour (ms since the Unix epoch, a whole hour).
    pub hour: u64,
    pub avg: f64,
    pub min: f64,
    pub max: f64,
}

impl Hour {
    /// What an imported hour must be to be believed.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.hour.is_multiple_of(HOUR_MS)
            && [self.avg, self.min, self.max].iter().all(|v| v.is_finite())
            && self.min <= self.max
            && (self.min - 1e-9..=self.max + 1e-9).contains(&self.avg)
    }
}

#[must_use]
pub fn hour_of(ts: u64) -> u64 {
    ts - ts % HOUR_MS
}

/// The statistics of every whole hour in `[from, to)` (`from` and `to` are
/// whole hours). `before` is the value held when `from` begins; `samples`
/// are the changes from `from` on, oldest first (later ones are ignored).
#[must_use]
pub fn hours(before: Option<f64>, samples: &[(u64, Option<f64>)], from: u64, to: u64) -> Vec<Hour> {
    let mut out = Vec::new();
    let mut held = before.filter(|v| v.is_finite());
    let mut next = samples.iter().peekable();
    let mut hour = from;
    while hour < to {
        let end = hour + HOUR_MS;
        let mut acc = Acc::default();
        let mut since = hour;
        while let Some(&&(ts, value)) = next.peek() {
            if ts >= end {
                break;
            }
            let ts = ts.max(hour);
            acc.add(held, ts - since);
            held = value.filter(|v| v.is_finite());
            since = ts;
            next.next();
        }
        acc.add(held, end - since);
        if let Some(stats) = acc.hour(hour) {
            out.push(stats);
        }
        hour = end;
    }
    out
}

#[derive(Default)]
struct Acc {
    weighted: f64,
    known_ms: u64,
    min: f64,
    max: f64,
}

impl Acc {
    #[allow(clippy::cast_precision_loss)] // ms within an hour
    fn add(&mut self, value: Option<f64>, ms: u64) {
        let Some(v) = value.filter(|_| ms > 0) else {
            return;
        };
        if self.known_ms == 0 {
            (self.min, self.max) = (v, v);
        } else {
            self.min = self.min.min(v);
            self.max = self.max.max(v);
        }
        self.weighted += v * ms as f64;
        self.known_ms += ms;
    }

    #[allow(clippy::cast_precision_loss)]
    fn hour(&self, hour: u64) -> Option<Hour> {
        (self.known_ms > 0).then(|| Hour {
            hour,
            avg: self.weighted / self.known_ms as f64,
            min: self.min,
            max: self.max,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = HOUR_MS;
    const MIN: u64 = 60_000;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn an_hour_is_weighted_by_how_long_each_value_held() {
        // 4 °C held since before, 6 °C for the last 10 minutes.
        let h = hours(Some(4.0), &[(10 * H + 50 * MIN, Some(6.0))], 10 * H, 11 * H);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].hour, 10 * H);
        assert!(close(h[0].avg, (4.0 * 50.0 + 6.0 * 10.0) / 60.0));
        assert!(close(h[0].min, 4.0) && close(h[0].max, 6.0));
    }

    #[test]
    fn a_value_carries_over_the_following_hours() {
        let h = hours(None, &[(10 * H + 30 * MIN, Some(5.0))], 10 * H, 13 * H);
        // Unknown for the first half hour: the average is the known part.
        assert_eq!(
            h.iter().map(|h| h.hour).collect::<Vec<_>>(),
            vec![10 * H, 11 * H, 12 * H]
        );
        assert!(h.iter().all(|h| close(h.avg, 5.0)));
    }

    #[test]
    fn gaps_count_for_nothing_and_unknown_hours_are_left_out() {
        let samples = [
            (10 * H + 30 * MIN, None),
            (12 * H + 15 * MIN, Some(8.0)),
            (12 * H + 45 * MIN, Some(f64::NAN)),
        ];
        let h = hours(Some(2.0), &samples, 10 * H, 13 * H);
        assert_eq!(h.len(), 2);
        assert!(close(h[0].avg, 2.0)); // 10 h: 2 °C, then offline
        assert_eq!(h[1].hour, 12 * H); // 11 h: offline all along
        assert!(close(h[1].avg, 8.0) && close(h[1].min, 8.0));
        assert!(hours(None, &[], 0, 5 * H).is_empty());
    }

    #[test]
    fn changes_outside_the_window_are_harmless() {
        // A change before `from` counts from `from`; one after `to` is ignored.
        let h = hours(
            Some(1.0),
            &[(9 * H, Some(3.0)), (12 * H, Some(9.0))],
            10 * H,
            11 * H,
        );
        assert_eq!(h.len(), 1);
        assert!(close(h[0].avg, 3.0) && close(h[0].max, 3.0));
    }

    #[test]
    fn imported_hours_are_checked() {
        let ok = Hour {
            hour: 3 * H,
            avg: 4.0,
            min: 2.0,
            max: 6.0,
        };
        assert!(ok.is_valid());
        assert!(
            !Hour {
                hour: 3 * H + 1,
                ..ok
            }
            .is_valid()
        );
        assert!(!Hour { avg: 7.0, ..ok }.is_valid());
        assert!(!Hour { min: 7.0, ..ok }.is_valid());
        assert!(
            !Hour {
                max: f64::INFINITY,
                ..ok
            }
            .is_valid()
        );
    }
}
