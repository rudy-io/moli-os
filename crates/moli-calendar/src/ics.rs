//! A calendar's events, read from an iCalendar file (RFC 5545): what the
//! days are made of, not the hours. Each event becomes a span of local
//! days, from its first day to the day after its last (as all-day events
//! are written).
//!
//! Read: `VEVENT`s with `SUMMARY`, `DTSTART` (a date, or a time in UTC, in a
//! `TZID` zone or floating), `DTEND` or `DURATION`, `STATUS:CANCELLED`
//! (left out), and `RRULE:FREQ=YEARLY` (with `INTERVAL`, `COUNT`, `UNTIL`,
//! for a birthday or an anniversary). Anything else in a recurrence rule
//! is not followed: the event counts once, and it is said. Every other
//! component (`VTIMEZONE`, `VTODO`, alarms…) is skipped.

use jiff::civil::{Date, DateTime, datetime};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};

/// Yearly repetition of an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Yearly {
    /// Every `interval` years.
    pub interval: i16,
    /// The number of times it happens, the first one included.
    pub count: Option<u32>,
    /// The last day it may start.
    pub until: Option<Date>,
}

/// An event, as local days: `from` (first day) to `until` (the day after
/// the last one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub name: String,
    pub from: Date,
    pub until: Date,
    pub yearly: Option<Yearly>,
}

/// What a file held.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub events: Vec<Event>,
    /// Events left out: no start, or a start that does not read.
    pub skipped: usize,
    /// Events whose repetition is not followed (counted once).
    pub unrepeated: usize,
}

fn days(n: i64) -> Span {
    Span::new().days(n)
}

fn after(day: Date, n: i64) -> Date {
    day.checked_add(days(n)).unwrap_or(day)
}

impl Event {
    fn length(&self) -> i64 {
        i64::from((self.until - self.from).get_days()).max(1)
    }

    /// The `k`-th time the event happens (0 = the first), if it does.
    fn occurrence(&self, k: i64) -> Option<(Date, Date)> {
        if k == 0 {
            return Some((self.from, self.until));
        }
        let yearly = self.yearly.as_ref()?;
        let count = yearly.count.is_none_or(|c| k < i64::from(c));
        let years = k.checked_mul(i64::from(yearly.interval))?;
        let from = self.from.checked_add(Span::new().years(years)).ok()?;
        let until_ok = yearly.until.is_none_or(|u| from <= u);
        (count && until_ok).then(|| (from, after(from, self.length())))
    }

    /// The occurrences that may hold `day` or come right after it.
    fn around(&self, day: Date) -> impl Iterator<Item = (Date, Date)> + '_ {
        let interval = self
            .yearly
            .as_ref()
            .map_or(1, |y| i64::from(y.interval.max(1)));
        let behind = i64::from(day.year()) - i64::from(self.from.year()) - 1;
        let first = if self.yearly.is_some() {
            (behind.max(0) / interval).max(0)
        } else {
            0
        };
        let last = if self.yearly.is_some() { first + 3 } else { 0 };
        (first..=last).filter_map(|k| self.occurrence(k))
    }

    /// Whether the event happens on `day`.
    #[must_use]
    pub fn on(&self, day: Date) -> bool {
        self.around(day)
            .any(|(from, until)| from <= day && day < until)
    }

    /// The first day the event starts after `day`.
    #[must_use]
    pub fn next_after(&self, day: Date) -> Option<Date> {
        self.around(day)
            .map(|(from, _)| from)
            .find(|from| *from > day)
    }
}

/// The names of the events on `day`, each once, in the file's order.
#[must_use]
pub fn on(events: &[Event], day: Date) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for e in events.iter().filter(|e| e.on(day)) {
        if !names.contains(&e.name.as_str()) {
            names.push(&e.name);
        }
    }
    names
}

/// The next day an event starts after `day`, and the names of those that
/// start then.
#[must_use]
pub fn next(events: &[Event], day: Date) -> Option<(Date, Vec<&str>)> {
    let first = events.iter().filter_map(|e| e.next_after(day)).min()?;
    let mut names: Vec<&str> = Vec::new();
    for e in events.iter().filter(|e| e.next_after(day) == Some(first)) {
        if !names.contains(&e.name.as_str()) {
            names.push(&e.name);
        }
    }
    Some((first, names))
}

/// Lines put back together: a line that starts with a space or a tab
/// goes on the one before.
fn unfolded(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match (line.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ => lines.push(line.to_owned()),
        }
    }
    lines
}

/// A property: its name (upper case), its parameters, its value.
struct Property<'a> {
    name: String,
    params: Vec<(String, &'a str)>,
    value: &'a str,
}

impl Property<'_> {
    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.trim_matches('"'))
    }
}

fn property(line: &str) -> Option<Property<'_>> {
    // The value starts at the first ':' outside a quoted parameter.
    let mut quoted = false;
    let colon = line.char_indices().find_map(|(i, c)| {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => return Some(i),
            _ => {}
        }
        None
    })?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(n, v)| (n.trim().to_ascii_uppercase(), v))
        .collect();
    Some(Property {
        name,
        params,
        value,
    })
}

fn text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push(' '),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_owned()
}

fn number(digits: &str) -> Option<i32> {
    digits
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| digits.parse().ok())
        .flatten()
}

fn civil_date(value: &str) -> Option<Date> {
    if value.len() != 8 {
        return None;
    }
    let year = i16::try_from(number(&value[..4])?).ok()?;
    let month = i8::try_from(number(&value[4..6])?).ok()?;
    let day = i8::try_from(number(&value[6..8])?).ok()?;
    Date::new(year, month, day).ok()
}

/// A start or an end: a day, or a moment.
enum When {
    Day(Date),
    At(Timestamp),
}

fn when(prop: &Property<'_>, tz: &TimeZone) -> Option<When> {
    let value = prop.value.trim();
    if prop.param("VALUE") == Some("DATE") || value.len() == 8 {
        return civil_date(value).map(When::Day);
    }
    let (day, time) = value.split_once('T')?;
    let (time, utc) = match time.strip_suffix('Z') {
        Some(t) => (t, true),
        None => (time, false),
    };
    if time.len() < 6 {
        return None;
    }
    let day = civil_date(day)?;
    let hour = i8::try_from(number(&time[..2])?).ok()?;
    let minute = i8::try_from(number(&time[2..4])?).ok()?;
    let second = i8::try_from(number(&time[4..6])?).ok()?.min(59);
    let civil: DateTime = datetime(day.year(), day.month(), day.day(), hour, minute, second, 0);
    let zone = if utc {
        TimeZone::UTC
    } else {
        prop.param("TZID")
            .and_then(|id| TimeZone::get(id).ok())
            .unwrap_or_else(|| tz.clone())
    };
    zone.to_timestamp(civil).ok().map(When::At)
}

/// `P1D`, `PT2H30M`, `P1W`… as days and seconds (negative ones as none).
fn duration(value: &str) -> Option<(i64, i64)> {
    let value = value.trim();
    let value = value.strip_prefix('+').unwrap_or(value);
    let rest = value.strip_prefix('P')?;
    let (mut days, mut seconds, mut number, mut in_time) = (0i64, 0i64, String::new(), false);
    for c in rest.chars() {
        match c {
            '0'..='9' => number.push(c),
            'T' => in_time = true,
            unit => {
                let n: i64 = number.parse().ok()?;
                number.clear();
                match (unit, in_time) {
                    ('W', false) => days += 7 * n,
                    ('D', false) => days += n,
                    ('H', true) => seconds += 3600 * n,
                    ('M', true) => seconds += 60 * n,
                    ('S', true) => seconds += n,
                    _ => return None,
                }
            }
        }
    }
    number.is_empty().then_some((days, seconds))
}

/// The yearly rule, or `Err(())` for a rule this reader does not follow.
fn yearly(rule: &str) -> Result<Yearly, ()> {
    let mut freq = None;
    let mut yearly = Yearly {
        interval: 1,
        count: None,
        until: None,
    };
    for part in rule.split(';') {
        let (key, value) = part.split_once('=').ok_or(())?;
        match key.trim().to_ascii_uppercase().as_str() {
            "FREQ" => freq = Some(value.trim().to_ascii_uppercase()),
            "INTERVAL" => {
                yearly.interval = value.trim().parse().map_err(|_| ())?;
                if yearly.interval < 1 {
                    return Err(());
                }
            }
            "COUNT" => yearly.count = Some(value.trim().parse().map_err(|_| ())?),
            "UNTIL" => yearly.until = Some(civil_date(value.trim().get(..8).ok_or(())?).ok_or(())?),
            "WKST" => {}
            _ => return Err(()),
        }
    }
    if freq.as_deref() == Some("YEARLY") {
        Ok(yearly)
    } else {
        Err(())
    }
}

/// One event's properties, as they come.
#[derive(Default)]
struct Draft {
    name: Option<String>,
    start: Option<When>,
    end: Option<When>,
    duration: Option<(i64, i64)>,
    rule: Option<String>,
    cancelled: bool,
}

fn local_day(at: Timestamp, tz: &TimeZone) -> Date {
    at.to_zoned(tz.clone()).date()
}

impl Draft {
    fn event(self, tz: &TimeZone, parsed: &mut Parsed) {
        if self.cancelled {
            return;
        }
        let Some(start) = self.start else {
            parsed.skipped += 1;
            return;
        };
        let (from, until) = match start {
            When::Day(from) => {
                let until = match (self.end, self.duration) {
                    (Some(When::Day(end)), _) => end,
                    (Some(When::At(end)), _) => after(local_day(end, tz), 1),
                    (None, Some((d, s))) => after(from, d + i64::from(s > 0)),
                    (None, None) => after(from, 1),
                };
                (from, until)
            }
            When::At(start) => {
                let end = match (self.end, self.duration) {
                    (Some(When::At(end)), _) => end,
                    (Some(When::Day(end)), _) => tz
                        .to_timestamp(end.to_datetime(jiff::civil::Time::midnight()))
                        .unwrap_or(start),
                    (None, Some((d, s))) => start
                        .checked_add(Span::new().hours(d * 24).seconds(s))
                        .unwrap_or(start),
                    (None, None) => start,
                };
                let from = local_day(start, tz);
                // The last moment of the event is in the day before `end`
                // when it ends at midnight.
                let last = end
                    .checked_sub(Span::new().nanoseconds(1))
                    .ok()
                    .filter(|l| *l >= start)
                    .unwrap_or(start);
                (from, after(local_day(last, tz), 1))
            }
        };
        // An end before or at the start: one day (the 2027 Ascension bridge
        // of the school calendar is written so).
        let until = if until > from { until } else { after(from, 1) };
        let yearly = match self.rule.as_deref().map(yearly) {
            None => None,
            Some(Ok(rule)) => Some(rule),
            Some(Err(())) => {
                parsed.unrepeated += 1;
                None
            }
        };
        parsed.events.push(Event {
            name: self.name.unwrap_or_default(),
            from,
            until,
            yearly,
        });
    }
}

/// The events of an iCalendar file; times without a zone are read in `tz`.
#[must_use]
pub fn parse(text_in: &str, tz: &TimeZone) -> Parsed {
    let mut parsed = Parsed::default();
    let mut draft: Option<Draft> = None;
    // Components inside the event (an alarm): their properties are not
    // the event's.
    let mut nested = 0usize;
    for line in unfolded(text_in) {
        let Some(prop) = property(&line) else {
            continue;
        };
        let value = prop.value.trim().to_ascii_uppercase();
        match (prop.name.as_str(), draft.as_mut()) {
            ("BEGIN", None) if value == "VEVENT" => {
                draft = Some(Draft::default());
                nested = 0;
            }
            ("BEGIN", Some(_)) => nested += 1,
            ("END", Some(_)) if nested > 0 => nested -= 1,
            ("END", Some(_)) if value == "VEVENT" => {
                if let Some(done) = draft.take() {
                    done.event(tz, &mut parsed);
                }
            }
            (_, Some(event)) if nested == 0 => match prop.name.as_str() {
                "SUMMARY" => event.name = Some(text(prop.value)),
                "DTSTART" => event.start = when(&prop, tz),
                "DTEND" => event.end = when(&prop, tz),
                "DURATION" => event.duration = duration(prop.value),
                "RRULE" => event.rule = Some(prop.value.trim().to_owned()),
                "STATUS" => event.cancelled = value == "CANCELLED",
                _ => {}
            },
            _ => {}
        }
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date as d;

    fn paris() -> TimeZone {
        TimeZone::get("Europe/Paris").unwrap()
    }

    fn fixture() -> String {
        let path = format!(
            "{}/../../integrations/calendar/fixtures/zone-a.ics",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn the_school_calendar_reads_as_it_is_published() {
        let parsed = parse(&fixture(), &paris());
        assert_eq!(
            (parsed.events.len(), parsed.skipped, parsed.unrepeated),
            (10, 0, 0)
        );
        let christmas = parsed
            .events
            .iter()
            .find(|e| e.name == "Vacances de Noël")
            .unwrap();
        assert_eq!(
            (christmas.from, christmas.until),
            (d(2026, 12, 19), d(2027, 1, 4))
        );
        let names = |day| on(&parsed.events, day);
        assert_eq!(
            names(d(2026, 12, 19)),
            ["Vacances de Noël"],
            "first day off"
        );
        assert_eq!(names(d(2027, 1, 3)), ["Vacances de Noël"], "last day off");
        assert!(names(d(2027, 1, 4)).is_empty(), "back to school");
        assert!(names(d(2026, 11, 27)).is_empty());
        // The teachers' last day of summer is an event of its own.
        assert_eq!(
            names(d(2026, 8, 31)),
            ["Vacances d'Été", "Vacances d'Été(prérentrée Enseignants)"]
        );
        // Written with its end on its start: that one day.
        assert_eq!(names(d(2027, 5, 7)), ["Pont de l'Ascension"]);
        assert!(names(d(2027, 5, 8)).is_empty());
        assert_eq!(
            next(&parsed.events, d(2026, 11, 27)),
            Some((d(2026, 12, 19), vec!["Vacances de Noël"]))
        );
        assert_eq!(
            next(&parsed.events, d(2028, 7, 4)),
            None,
            "nothing after the last one"
        );
    }

    #[test]
    fn times_are_read_in_their_zone_and_cover_their_days() {
        let ics = "BEGIN:VCALENDAR\r\n\
            BEGIN:VEVENT\r\nSUMMARY:Soirée\r\nDTSTART;TZID=Europe/Paris:20261231T200000\r\nDTEND;TZID=Europe/Paris:20270101T020000\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nSUMMARY:Réunion\r\nDTSTART:20261130T230000Z\r\nDURATION:PT1H\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nSUMMARY:Jusqu'à minuit\r\nDTSTART:20261201T180000\r\nDTEND:20261202T000000\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nSUMMARY:Congés\r\nDTSTART;VALUE=DATE:20270301\r\nDURATION:P1W\r\nEND:VEVENT\r\n\
            END:VCALENDAR\r\n";
        let parsed = parse(ics, &paris());
        let span = |name: &str| {
            let e = parsed.events.iter().find(|e| e.name == name).unwrap();
            (e.from, e.until)
        };
        assert_eq!(
            span("Soirée"),
            (d(2026, 12, 31), d(2027, 1, 2)),
            "over midnight"
        );
        assert_eq!(
            span("Réunion"),
            (d(2026, 12, 1), d(2026, 12, 2)),
            "23:00 UTC is midnight in Paris"
        );
        assert_eq!(
            span("Jusqu'à minuit"),
            (d(2026, 12, 1), d(2026, 12, 2)),
            "ends at midnight: one day"
        );
        assert_eq!(span("Congés"), (d(2027, 3, 1), d(2027, 3, 8)));
    }

    #[test]
    fn a_birthday_comes_back_every_year() {
        let ics = "BEGIN:VEVENT\nSUMMARY:Anniversaire\\, Léa\nDTSTART;VALUE=DATE:20190412\nRRULE:FREQ=YEARLY\nEND:VEVENT\n\
                   BEGIN:VEVENT\nSUMMARY:Tous les deux ans\nDTSTART;VALUE=DATE:20200601\nRRULE:FREQ=YEARLY;INTERVAL=2;COUNT=3\nEND:VEVENT\n\
                   BEGIN:VEVENT\nSUMMARY:Chaque lundi\nDTSTART;VALUE=DATE:20261130\nRRULE:FREQ=WEEKLY\nEND:VEVENT\n";
        let parsed = parse(ics, &paris());
        assert_eq!(parsed.unrepeated, 1, "a weekly rule is not followed");
        let events = &parsed.events;
        assert_eq!(on(events, d(2027, 4, 12)), ["Anniversaire, Léa"]);
        assert!(on(events, d(2027, 4, 13)).is_empty());
        assert_eq!(next(events, d(2026, 11, 27)).unwrap().0, d(2026, 11, 30));
        assert_eq!(
            next(events, d(2026, 12, 1)).unwrap(),
            (d(2027, 4, 12), vec!["Anniversaire, Léa"])
        );
        assert_eq!(on(events, d(2024, 6, 1)), ["Tous les deux ans"]);
        assert!(on(events, d(2025, 6, 1)).is_empty(), "every other year");
        assert_eq!(on(events, d(2022, 6, 1)), ["Tous les deux ans"]);
        assert!(
            on(events, d(2026, 6, 1)).is_empty(),
            "three times: 2020, 2022, 2024"
        );
        assert!(on(events, d(2026, 12, 7)).is_empty(), "counted once");
    }

    #[test]
    fn what_does_not_read_is_left_out() {
        let ics = "BEGIN:VEVENT\nSUMMARY:Sans début\nEND:VEVENT\n\
                   BEGIN:VEVENT\nSUMMARY:Date fausse\nDTSTART:20261340\nEND:VEVENT\n\
                   BEGIN:VEVENT\nSUMMARY:Annulé\nDTSTART:20261201\nSTATUS:CANCELLED\nEND:VEVENT\n\
                   BEGIN:VEVENT\nSUMMARY:Avec rappel\nDTSTART:20261201\nBEGIN:VALARM\nDTSTART:20261130\nSUMMARY:Rappel\nEND:VALARM\nEND:VEVENT\n\
                   BEGIN:VEVENT\nSUMMARY:Long\n  titre plié\nDTSTART:20261202\nEND:VEVENT\n";
        let parsed = parse(ics, &paris());
        assert_eq!(parsed.skipped, 2);
        let names: Vec<_> = parsed
            .events
            .iter()
            .map(|e| (e.name.as_str(), e.from))
            .collect();
        assert_eq!(
            names,
            [
                ("Avec rappel", d(2026, 12, 1)),
                ("Long titre plié", d(2026, 12, 2))
            ]
        );
        assert_eq!(parse("pas un calendrier", &paris()), Parsed::default());
    }
}
