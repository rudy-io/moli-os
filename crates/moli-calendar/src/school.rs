//! School holidays, read from the French education ministry's open data
//! (`fr-en-calendrier-scolaire` on data.education.gouv.fr): the periods of
//! one zone, each from its first day off to the day school starts again.

use std::collections::BTreeSet;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde_json::Value as Json;

/// The zones the ministry's calendar knows, as it writes them.
pub const ZONES: [&str; 13] = [
    "Zone A",
    "Zone B",
    "Zone C",
    "Corse",
    "Guadeloupe",
    "Guyane",
    "Martinique",
    "Mayotte",
    "Nouvelle Calédonie",
    "Polynésie",
    "Réunion",
    "Saint Pierre et Miquelon",
    "Wallis et Futuna",
];

/// The zone as the calendar writes it: `A`, `zone b` and `Zone C` are the
/// three zones of mainland France, any other must be one of [`ZONES`]
/// (case aside).
#[must_use]
pub fn zone(given: &str) -> Option<&'static str> {
    let given = given.trim();
    let letter = given
        .strip_prefix("Zone ")
        .or_else(|| given.strip_prefix("zone "))
        .unwrap_or(given);
    match letter {
        "A" | "a" => return Some(ZONES[0]),
        "B" | "b" => return Some(ZONES[1]),
        "C" | "c" => return Some(ZONES[2]),
        _ => {}
    }
    ZONES
        .iter()
        .copied()
        .find(|z| z.to_lowercase() == given.to_lowercase())
}

/// A school holiday: its name, its first day off, and the day school
/// starts again (not a day off).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Period {
    pub from: Date,
    pub until: Date,
    pub name: String,
}

impl Period {
    #[must_use]
    pub fn holds(&self, day: Date) -> bool {
        self.from <= day && day < self.until
    }
}

/// The periods in an answer of the open data API. Each one comes once per
/// académie: they are kept once. The teachers' summer (`population` =
/// « Enseignants », a day shorter) is left out: the pupils' is the one a
/// house lives by. The times are midnight in France, written in UTC: they
/// are read in `tz`.
#[must_use]
pub fn periods(answer: &Json, tz: &TimeZone) -> Vec<Period> {
    let day = |v: &Json| {
        v.as_str()
            .and_then(|s| s.parse::<Timestamp>().ok())
            .map(|t| t.to_zoned(tz.clone()).date())
    };
    let mut found = BTreeSet::new();
    for record in answer["results"].as_array().into_iter().flatten() {
        if record["population"].as_str() == Some("Enseignants") {
            continue;
        }
        let (Some(from), Some(until), Some(name)) = (
            day(&record["start_date"]),
            day(&record["end_date"]),
            record["description"].as_str(),
        ) else {
            continue;
        };
        // A period of a single day is sometimes written with the same start
        // and end (the 2027 Ascension bridge): that day is off.
        let until = if until == from {
            from.tomorrow().unwrap_or(until)
        } else {
            until
        };
        if from < until {
            found.insert(Period {
                from,
                until,
                name: name.trim().to_owned(),
            });
        }
    }
    found.into_iter().collect()
}

/// The school holiday `day` falls in, if any.
#[must_use]
pub fn on(periods: &[Period], day: Date) -> Option<&Period> {
    periods.iter().find(|p| p.holds(day))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn fixture() -> Json {
        let path = format!(
            "{}/../../integrations/calendar/fixtures/records.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn paris() -> TimeZone {
        TimeZone::get("Europe/Paris").unwrap()
    }

    #[test]
    fn a_zone_is_named_as_the_calendar_names_it() {
        assert_eq!(zone("A"), Some("Zone A"));
        assert_eq!(zone("zone b"), Some("Zone B"));
        assert_eq!(zone(" Zone C "), Some("Zone C"));
        assert_eq!(zone("corse"), Some("Corse"));
        assert_eq!(zone("Réunion"), Some("Réunion"));
        assert_eq!(zone("D"), None);
        assert_eq!(zone(""), None);
    }

    #[test]
    fn each_period_is_kept_once_in_local_days() {
        let periods = periods(&fixture(), &paris());
        // 8 periods, each given for two académies, the teachers' summer apart.
        assert_eq!(periods.len(), 7);
        let christmas = &periods[0];
        assert_eq!(christmas.name, "Vacances de Noël");
        assert_eq!(
            christmas.from,
            date(2025, 12, 20),
            "23:00 UTC is midnight in Paris"
        );
        assert_eq!(christmas.until, date(2026, 1, 5));
        let summer = periods.iter().find(|p| p.name == "Vacances d'Été").unwrap();
        assert_eq!(
            summer.until,
            date(2026, 9, 1),
            "the pupils' summer, not the teachers'"
        );
        // Summer time: 22:00 UTC is midnight too.
        let all_saints = periods
            .iter()
            .find(|p| p.name.contains("Toussaint"))
            .unwrap();
        assert_eq!(all_saints.from, date(2026, 10, 17));
        assert_eq!(all_saints.until, date(2026, 11, 2));
    }

    #[test]
    fn a_day_is_in_a_school_holiday_or_not() {
        let periods = periods(&fixture(), &paris());
        let name = |d| on(&periods, d).map(|p| p.name.as_str());
        assert_eq!(
            name(date(2026, 12, 19)),
            Some("Vacances de Noël"),
            "first day off"
        );
        assert_eq!(
            name(date(2027, 1, 3)),
            Some("Vacances de Noël"),
            "last day off"
        );
        assert_eq!(name(date(2027, 1, 4)), None, "back to school");
        assert_eq!(name(date(2026, 12, 18)), None, "the last school day");
        assert_eq!(name(date(2026, 5, 15)), Some("Pont de l'Ascension"));
    }

    #[test]
    fn a_strange_answer_gives_no_period() {
        assert!(periods(&serde_json::json!({}), &paris()).is_empty());
        let odd = serde_json::json!({"results": [
            {"description": "Vacances", "start_date": "demain", "end_date": "2026-01-01T00:00:00+00:00"},
            {"description": "À l'envers", "start_date": "2026-02-01T00:00:00+00:00", "end_date": "2026-01-01T00:00:00+00:00"},
            {"start_date": "2026-01-01T00:00:00+00:00", "end_date": "2026-02-01T00:00:00+00:00"}
        ]});
        assert!(periods(&odd, &paris()).is_empty());
    }

    #[test]
    fn a_period_that_ends_where_it_starts_is_one_day() {
        let bridge = serde_json::json!({"results": [{
            "description": "Pont de l'Ascension", "population": "-",
            "start_date": "2027-05-06T22:00:00+00:00", "end_date": "2027-05-06T22:00:00+00:00"
        }]});
        let periods = periods(&bridge, &paris());
        assert_eq!(
            on(&periods, date(2027, 5, 7)).unwrap().name,
            "Pont de l'Ascension"
        );
        assert!(on(&periods, date(2027, 5, 8)).is_none());
    }
}
