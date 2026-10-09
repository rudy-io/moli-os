//! From counter readings to energy per hour. Pure: no clock, no storage.
//!
//! A meter is a cumulative counter. Energy is the difference between two
//! readings; it was consumed somewhere between them, so it is spread over
//! the hours they span (in proportion to time, or only over the time its
//! tariff period was active). That smooths coarse counters (a Linky index
//! moves by whole kWh) and fills the gap honestly when Moli was stopped.
//!
//! Readings are not trusted blindly: an impossible jump (more than the
//! home's maximum power could draw) or a fall is held as *suspect* until
//! the next readings tell a glitch from a real change.

use crate::config::Reset;

pub const HOUR_MS: u64 = 3_600_000;
/// A longer gap is crammed into its last 31 days (better than nothing).
const MAX_SPREAD_MS: u64 = 31 * 24 * HOUR_MS;
/// Allowance for coarse counters: a Linky index moves by a whole kWh.
const SLACK_KWH: f64 = 1.0;
/// Consistent abnormal readings after which the counter is believed
/// (replaced meter, recalibration): rebased, nothing counted.
const REBASE_AFTER: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub kwh: f64,
    pub ts: u64,
}

/// What a new reading means for the meter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// `kwh` consumed since `from` (the previous reading's time).
    Consumed { from: u64, kwh: f64 },
    /// Nothing to count: unchanged, stale, or suspect (held for now).
    Hold,
    /// The counter jumped to a new baseline: follow it, count nothing.
    Rebase,
}

/// A meter's memory: its last accepted reading and a pending suspect one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tracker {
    pub last: Reading,
    suspect: Option<(Reading, u32)>,
}

#[allow(clippy::cast_precision_loss)] // ms spans: far below 2^52
fn plausible(kwh: f64, from: u64, to: u64, max_kw: f64) -> bool {
    let hours = to.saturating_sub(from) as f64 / HOUR_MS as f64;
    kwh <= max_kw * hours + SLACK_KWH
}

impl Tracker {
    #[must_use]
    pub fn new(last: Reading) -> Self {
        Self {
            last,
            suspect: None,
        }
    }

    /// Takes `next` into account; `self.last` follows unless held.
    pub fn observe(&mut self, next: Reading, reset: Reset, max_kw: f64) -> Outcome {
        let last = self.last;
        if next.ts <= last.ts || !next.kwh.is_finite() || next.kwh < 0.0 {
            return Outcome::Hold;
        }
        let delta = next.kwh - last.kwh;
        if delta >= 0.0 && plausible(delta, last.ts, next.ts, max_kw) {
            // Normal (a pending suspect was a glitch).
            self.suspect = None;
            if delta == 0.0 {
                // Keep the older reading: the next change is spread over
                // the whole time it took.
                return Outcome::Hold;
            }
            self.last = next;
            return Outcome::Consumed {
                from: last.ts,
                kwh: delta,
            };
        }
        // Abnormal: a fall, or more than the home could draw.
        let Some((suspect, seen)) = self.suspect else {
            self.suspect = Some((next, 1));
            return Outcome::Hold;
        };
        let consistent = next.kwh >= suspect.kwh
            && plausible(next.kwh - suspect.kwh, suspect.ts, next.ts, max_kw);
        if !consistent {
            self.suspect = Some((next, 1));
            return Outcome::Hold;
        }
        // Confirmed restart from zero (daily, monthly counter; reboot).
        if reset == Reset::Auto
            && suspect.kwh < last.kwh / 2.0
            && plausible(next.kwh, last.ts, next.ts, max_kw)
        {
            self.suspect = None;
            self.last = next;
            return Outcome::Consumed {
                from: last.ts,
                kwh: next.kwh,
            };
        }
        if seen + 1 >= REBASE_AFTER {
            self.suspect = None;
            self.last = next;
            return Outcome::Rebase;
        }
        self.suspect = Some((next, seen + 1));
        Outcome::Hold
    }
}

#[must_use]
pub fn hour_of(ts: u64) -> u64 {
    ts - ts % HOUR_MS
}

/// `kwh` consumed between `from` and `to`, split over the hours spanned in
/// proportion to time.
#[must_use]
pub fn spread(kwh: f64, from: u64, to: u64) -> Vec<(u64, f64)> {
    spread_weighted(kwh, from, to, |a, b| b - a)
}

/// Same, in proportion to `eligible(a, b)`: the ms of `[a, b)` during which
/// the energy could have been consumed (a « heures creuses » index only
/// moves in HC time). No eligible time at all: in proportion to time.
#[must_use]
#[allow(clippy::cast_precision_loss)] // ms spans
pub fn spread_weighted(
    kwh: f64,
    from: u64,
    to: u64,
    eligible: impl Fn(u64, u64) -> u64,
) -> Vec<(u64, f64)> {
    let from = from.max(to.saturating_sub(MAX_SPREAD_MS));
    if to <= from {
        return vec![(hour_of(to), kwh)];
    }
    let mut pieces = Vec::new();
    let mut t = from;
    while t < to {
        let hour = hour_of(t);
        let end = (hour + HOUR_MS).min(to);
        pieces.push((hour, t, end));
        t = end;
    }
    let weights: Vec<u64> = pieces
        .iter()
        .map(|&(_, a, b)| eligible(a, b).min(b - a))
        .collect();
    let total: u64 = weights.iter().sum();
    pieces
        .iter()
        .zip(&weights)
        .map(|(&(hour, a, b), &w)| {
            let share = if total == 0 {
                (b - a) as f64 / (to - from) as f64
            } else {
                w as f64 / total as f64
            };
            (hour, kwh * share)
        })
        .filter(|&(_, k)| k > 0.0)
        .collect()
}

/// The longest a power is held between two readings: beyond, the reading
/// was missed (Moli busy, a lag), not steady. A tick holds it every few
/// minutes anyway.
pub const MAX_HOLD_MS: u64 = 15 * 60 * 1000;

/// A meter that measures power, not energy: Moli adds power × time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Integral {
    /// Energy so far, a counter that never restarts.
    pub kwh: f64,
    /// The power held (kW) and since when; `None`: unknown (offline,
    /// never seen since Moli started).
    pub held: Option<(f64, u64)>,
}

impl Integral {
    #[must_use]
    pub fn new(kwh: f64) -> Self {
        Self { kwh, held: None }
    }

    /// The power is `kw` at `ts`: what was held until now counts. Times
    /// before `floor` (Moli's start) are never counted.
    #[allow(clippy::cast_precision_loss)] // ms spans
    pub fn observe(&mut self, kw: Option<f64>, ts: u64, floor: u64) -> Reading {
        let ts = ts.max(floor);
        if let Some((was, since)) = self.held {
            let span = ts.saturating_sub(since).min(MAX_HOLD_MS);
            self.kwh += was.max(0.0) * span as f64 / HOUR_MS as f64;
        }
        self.held = kw.filter(|kw| kw.is_finite()).map(|kw| (kw, ts));
        Reading { kwh: self.kwh, ts }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = HOUR_MS;
    const KW: f64 = 36.0;

    fn r(kwh: f64, ts: u64) -> Reading {
        Reading { kwh, ts }
    }

    fn feed(start: Reading, reset: Reset, readings: &[(f64, u64)]) -> (Vec<Outcome>, Reading) {
        let mut t = Tracker::new(start);
        let out = readings
            .iter()
            .map(|&(kwh, ts)| t.observe(r(kwh, ts), reset, KW))
            .collect();
        (out, t.last)
    }

    fn counted(out: &[Outcome]) -> f64 {
        out.iter()
            .map(|o| match o {
                Outcome::Consumed { kwh, .. } => *kwh,
                _ => 0.0,
            })
            .sum()
    }

    #[test]
    fn counts_up_and_keeps_the_older_reading_when_unchanged() {
        let (out, last) = feed(r(10.0, 0), Reset::Auto, &[(10.0, H), (12.5, 2 * H)]);
        assert_eq!(out[0], Outcome::Hold);
        assert_eq!(out[1], Outcome::Consumed { from: 0, kwh: 2.5 });
        assert_eq!(last, r(12.5, 2 * H));
        // Old, simultaneous or broken readings.
        let (out, _) = feed(
            r(1.0, 5 * H),
            Reset::Auto,
            &[(2.0, 5 * H), (f64::NAN, 6 * H), (-1.0, 6 * H)],
        );
        assert!(out.iter().all(|o| *o == Outcome::Hold));
    }

    #[test]
    fn a_daily_restart_counts_once_confirmed() {
        let (out, last) = feed(r(12.0, 0), Reset::Auto, &[(0.25, H), (0.5, 2 * H)]);
        assert_eq!(out[0], Outcome::Hold);
        assert_eq!(out[1], Outcome::Consumed { from: 0, kwh: 0.5 });
        assert_eq!(last, r(0.5, 2 * H));
    }

    #[test]
    fn glitches_count_nothing() {
        // A low reading, then back to normal.
        let (out, _) = feed(r(300.0, 0), Reset::Auto, &[(5.0, H), (300.2, 2 * H)]);
        assert!((counted(&out) - 0.2).abs() < 1e-9);
        // An impossible jump (a Linky index read as 4 million), then normal.
        let (out, last) = feed(
            r(12_000.0, 0),
            Reset::Never,
            &[(4_294_967.0, H), (12_001.0, 2 * H)],
        );
        assert_eq!(counted(&out), 1.0);
        assert_eq!(last, r(12_001.0, 2 * H));
        // An index never restarts: a fall is a glitch.
        let (out, _) = feed(r(12.0, 0), Reset::Never, &[(0.25, H), (12.5, 2 * H)]);
        assert_eq!(counted(&out), 0.5);
    }

    #[test]
    fn a_new_baseline_is_followed_without_counting() {
        // A monthly counter after a month away: 300 → 180, 181, 182.
        let (out, last) = feed(
            r(300.0, 0),
            Reset::Auto,
            &[(180.0, 500 * H), (181.0, 501 * H), (182.0, 502 * H)],
        );
        assert_eq!(out, vec![Outcome::Hold, Outcome::Hold, Outcome::Rebase]);
        assert_eq!(last, r(182.0, 502 * H));
        // A replaced meter with a bigger index: believed after 3 readings.
        let (out, _) = feed(
            r(10.0, 0),
            Reset::Never,
            &[(9_000.0, H), (9_001.0, 2 * H), (9_002.0, 3 * H)],
        );
        assert_eq!(out[2], Outcome::Rebase);
        assert_eq!(counted(&out), 0.0);
    }

    #[test]
    fn spreads_over_hours_in_proportion() {
        let close = |got: &[(u64, f64)], want: &[(u64, f64)]| {
            got.len() == want.len()
                && got
                    .iter()
                    .zip(want)
                    .all(|(g, w)| g.0 == w.0 && (g.1 - w.1).abs() < 1e-9)
        };
        // 3 kWh from 10:30 to 13:00: ½ h, 1 h, 1 h.
        let s = spread(3.0, 10 * H + H / 2, 13 * H);
        assert!(
            close(&s, &[(10 * H, 0.6), (11 * H, 1.2), (12 * H, 1.2)]),
            "{s:?}"
        );
        assert_eq!(spread(0.5, 10 * H + 1, 10 * H + 9), vec![(10 * H, 0.5)]);
        assert!(spread(1.0, 0, 400 * 24 * H).len() <= 31 * 24);
    }

    #[test]
    fn spreads_only_over_eligible_time() {
        // From 20:00 to 24:00, eligible only from 22:00 (heures creuses).
        let s = spread_weighted(2.0, 20 * H, 24 * H, |a, b| b.saturating_sub(a.max(22 * H)));
        assert_eq!(s, vec![(22 * H, 1.0), (23 * H, 1.0)]);
        // Nothing eligible: by time.
        let s = spread_weighted(2.0, 20 * H, 22 * H, |_, _| 0);
        assert_eq!(s, vec![(20 * H, 1.0), (21 * H, 1.0)]);
    }

    #[test]
    fn power_held_over_time_becomes_energy() {
        let min = 60_000;
        let mut i = Integral::new(10.0);
        // First seen: nothing counted yet.
        assert_eq!(i.observe(Some(0.1), 0, 0), r(10.0, 0));
        // 100 W for 6 minutes (a tick re-reads the same power).
        assert_eq!(i.observe(Some(0.1), 6 * min, 0).kwh, 10.0 + 0.01);
        // Then 1 kW for 3 minutes: the 100 W counted until the change.
        let after = i.observe(Some(1.0), 9 * min, 0).kwh;
        assert!((after - (10.01 + 0.005)).abs() < 1e-9, "{after}");
        let after = i.observe(Some(0.0), 12 * min, 0).kwh;
        assert!((after - (10.015 + 0.05)).abs() < 1e-9, "{after}");
    }

    #[test]
    fn gaps_offline_time_and_moli_stopped_are_not_counted() {
        let min = 60_000;
        let mut i = Integral::new(0.0);
        i.observe(Some(1.0), 0, 0);
        // A reading missed for 2 h: at most 15 min held.
        assert!((i.observe(Some(1.0), 120 * min, 0).kwh - 0.25).abs() < 1e-9);
        // Offline: what was held until then counts, nothing after.
        let at_off = i.observe(None, 125 * min, 0).kwh;
        assert!((at_off - (0.25 + 1.0 / 12.0)).abs() < 1e-9);
        assert!((i.observe(Some(1.0), 200 * min, 0).kwh - at_off).abs() < 1e-9);
        // A power known from before Moli started counts from the start only.
        let mut j = Integral::new(0.0);
        j.observe(Some(1.0), 0, 60 * min);
        assert!((j.observe(Some(1.0), 66 * min, 60 * min).kwh - 0.1).abs() < 1e-9);
    }
}
