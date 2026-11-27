//! French public holidays, worked out with no network: the fixed days, and
//! those that follow Easter (Easter Monday, Ascension, Whit Monday). Alsace
//! and Moselle add Good Friday and St Stephen's Day.

use jiff::civil::{Date, date};

/// A public holiday: its day and its name's translation key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Holiday {
    pub day: Date,
    pub key: &'static str,
}

/// Easter Sunday of `year` (Gregorian calendar; the anonymous algorithm,
/// as given by Meeus). The letters are the algorithm's own, kept so it reads
/// against its source.
#[must_use]
#[allow(clippy::many_single_char_names)]
pub fn easter(year: i16) -> Date {
    let y = i32::from(year);
    let a = y % 19;
    let b = y / 100;
    let c = y % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    // month is 3 or 4 and day 1 to 31: both fit.
    date(
        year,
        i8::try_from(month).unwrap_or(4),
        i8::try_from(day).unwrap_or(1),
    )
}

fn after(day: Date, days: i64) -> Date {
    day.checked_add(jiff::Span::new().days(days)).unwrap_or(day)
}

/// The public holidays of `year`, in date order.
#[must_use]
pub fn of_year(year: i16, alsace_moselle: bool) -> Vec<Holiday> {
    let easter = easter(year);
    let mut days = vec![
        Holiday {
            day: date(year, 1, 1),
            key: "jour_de_l_an",
        },
        Holiday {
            day: after(easter, 1),
            key: "lundi_de_paques",
        },
        Holiday {
            day: date(year, 5, 1),
            key: "fete_du_travail",
        },
        Holiday {
            day: date(year, 5, 8),
            key: "victoire_1945",
        },
        Holiday {
            day: after(easter, 39),
            key: "ascension",
        },
        Holiday {
            day: after(easter, 50),
            key: "lundi_de_pentecote",
        },
        Holiday {
            day: date(year, 7, 14),
            key: "fete_nationale",
        },
        Holiday {
            day: date(year, 8, 15),
            key: "assomption",
        },
        Holiday {
            day: date(year, 11, 1),
            key: "toussaint",
        },
        Holiday {
            day: date(year, 11, 11),
            key: "armistice",
        },
        Holiday {
            day: date(year, 12, 25),
            key: "noel",
        },
    ];
    if alsace_moselle {
        days.push(Holiday {
            day: after(easter, -2),
            key: "vendredi_saint",
        });
        days.push(Holiday {
            day: date(year, 12, 26),
            key: "saint_etienne",
        });
    }
    days.sort_by_key(|h| h.day);
    days
}

/// The public holiday `day` is, if it is one.
#[must_use]
pub fn on(day: Date, alsace_moselle: bool) -> Option<Holiday> {
    of_year(day.year(), alsace_moselle)
        .into_iter()
        .find(|h| h.day == day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easter_falls_where_the_calendars_say() {
        assert_eq!(easter(2024), date(2024, 3, 31));
        assert_eq!(easter(2025), date(2025, 4, 20));
        assert_eq!(easter(2026), date(2026, 4, 5));
        assert_eq!(easter(2027), date(2027, 3, 28));
        assert_eq!(easter(2038), date(2038, 4, 25), "the latest there can be");
        assert_eq!(easter(2285), date(2285, 3, 22), "the earliest there can be");
    }

    #[test]
    fn a_year_has_eleven_holidays_and_alsace_moselle_two_more() {
        let days = of_year(2026, false);
        assert_eq!(days.len(), 11);
        let keyed = |k: &str| days.iter().find(|h| h.key == k).unwrap().day;
        assert_eq!(keyed("lundi_de_paques"), date(2026, 4, 6));
        assert_eq!(keyed("ascension"), date(2026, 5, 14));
        assert_eq!(keyed("lundi_de_pentecote"), date(2026, 5, 25));
        assert!(
            days.windows(2).all(|w| w[0].day < w[1].day),
            "in date order"
        );

        let local = of_year(2026, true);
        assert_eq!(local.len(), 13);
        assert!(local.contains(&Holiday {
            day: date(2026, 4, 3),
            key: "vendredi_saint"
        }));
        assert!(local.contains(&Holiday {
            day: date(2026, 12, 26),
            key: "saint_etienne"
        }));
    }

    #[test]
    fn a_day_is_a_holiday_or_not() {
        assert_eq!(on(date(2026, 11, 11), false).unwrap().key, "armistice");
        assert_eq!(on(date(2026, 11, 12), false), None);
        assert_eq!(on(date(2026, 12, 26), false), None);
        assert_eq!(on(date(2026, 12, 26), true).unwrap().key, "saint_etienne");
    }
}
