//! A room's weekly schedule, in the form the dashboard already edits for a
//! Zigbee2MQTT valve: one text per day (`weekly_schedule_monday`…), steps
//! `HH:mm/<°C>` separated by spaces, the first one at 00:00.

/// Day names of the points, Monday first (jiff's `to_monday_zero_offset`).
pub const DAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// Steps a day holds: the dashboard's editor offers six for a valve without
/// schedule groups.
pub const MAX_STEPS: usize = 6;

/// From this minute of the day, the room wants `temp` °C.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub minute: u16,
    pub temp: f64,
}

/// Reads a day. `None` when it is not a schedule the room accepts: a step
/// malformed, out of `min`..=`max`, off the half degree, out of order, more
/// than [`MAX_STEPS`], or a day that does not start at 00:00.
#[must_use]
pub fn parse_day(text: &str, min: f64, max: f64) -> Option<Vec<Step>> {
    let mut steps: Vec<Step> = Vec::new();
    for part in text.split_whitespace() {
        let (time, deg) = part.split_once('/')?;
        let (h, m) = time.split_once(':')?;
        if h.len() != 2 || m.len() != 2 {
            return None;
        }
        let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
        if h > 23 || m > 59 {
            return None;
        }
        let temp: f64 = deg.parse().ok()?;
        if !temp.is_finite() || temp < min || temp > max || !on_half_degree(temp) {
            return None;
        }
        let minute = h * 60 + m;
        if steps.last().is_some_and(|s| s.minute >= minute) {
            return None;
        }
        steps.push(Step { minute, temp });
    }
    (steps.len() <= MAX_STEPS && steps.first().is_some_and(|s| s.minute == 0)).then_some(steps)
}

fn on_half_degree(temp: f64) -> bool {
    ((temp * 2.0).round() - temp * 2.0).abs() < 1e-9
}

/// Writes a day back, as the dashboard writes it (`20`, `20.5`).
#[must_use]
pub fn format_day(steps: &[Step]) -> String {
    steps
        .iter()
        .map(|s| {
            let deg = if s.temp.fract().abs() < 1e-9 {
                format!("{:.0}", s.temp)
            } else {
                format!("{:.1}", s.temp)
            };
            format!("{:02}:{:02}/{deg}", s.minute / 60, s.minute % 60)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The step in force at `minute` of the day: its index and temperature.
#[must_use]
pub fn step_at(steps: &[Step], minute: u16) -> Option<(usize, f64)> {
    steps
        .iter()
        .enumerate()
        .rev()
        .find(|(_, s)| s.minute <= minute)
        .map(|(i, s)| (i, s.temp))
}

/// Minutes from `minute` to the next step of the day, or to midnight (where
/// the next day's first step takes over).
#[must_use]
pub fn to_next_step(steps: &[Step], minute: u16) -> u16 {
    steps
        .iter()
        .map(|s| s.minute)
        .find(|m| *m > minute)
        .unwrap_or(24 * 60)
        .saturating_sub(minute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_reads_writes_and_answers_by_the_minute() {
        let steps = parse_day("00:00/17 06:30/20.5 22:00/17", 5.0, 25.0).unwrap();
        assert_eq!(format_day(&steps), "00:00/17 06:30/20.5 22:00/17");
        assert_eq!(step_at(&steps, 0), Some((0, 17.0)));
        assert_eq!(step_at(&steps, 389), Some((0, 17.0)));
        assert_eq!(step_at(&steps, 390), Some((1, 20.5)));
        assert_eq!(step_at(&steps, 23 * 60 + 59), Some((2, 17.0)));
        assert_eq!(to_next_step(&steps, 0), 390);
        assert_eq!(to_next_step(&steps, 390), 22 * 60 - 390);
        assert_eq!(to_next_step(&steps, 23 * 60), 60);
        assert_eq!(
            parse_day("  00:00/19  ", 5.0, 25.0),
            Some(vec![Step {
                minute: 0,
                temp: 19.0
            }])
        );
    }

    #[test]
    fn a_day_the_room_does_not_accept_is_refused() {
        for text in [
            "",
            "06:00/20",
            "00:00/20 06:00",
            "00:00/20 6:00/21",
            "00:00/20 24:00/21",
            "00:00/20 06:60/21",
            "00:00/20.3",
            "00:00/26",
            "00:00/4.5",
            "00:00/NaN",
            "00:00/inf",
            "00:00/20 06:00/21 06:00/22",
            "00:00/20 07:00/21 06:00/22",
            "00:00/17 01:00/17 02:00/17 03:00/17 04:00/17 05:00/17 06:00/17",
            "00:00/20 a",
        ] {
            assert!(parse_day(text, 5.0, 25.0).is_none(), "{text:?}");
        }
    }
}
