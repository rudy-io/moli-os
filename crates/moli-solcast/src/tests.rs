use super::model::{self, Cache, STEP, Slot};
use super::{Config, Solcast};
use jiff::tz::TimeZone;
use std::collections::BTreeMap;

const EAST: &[u8] = include_bytes!("../../../integrations/solcast/fixtures/forecasts-east.json");
const WEST: &[u8] = include_bytes!("../../../integrations/solcast/fixtures/forecasts-west.json");

fn paris() -> TimeZone {
    TimeZone::get("Europe/Paris").unwrap()
}

/// 2026-11-20 at `h:m` UTC, in Unix seconds.
fn at(h: i64, m: i64) -> i64 {
    // 2026-11-20T00:00:00Z
    1_795_132_800 + h * 3600 + m * 60
}

fn slots(from: i64, n: i64, kw: f64) -> BTreeMap<i64, Slot> {
    (0..n)
        .map(|i| {
            (
                from + (i + 1) * STEP,
                Slot {
                    kw,
                    lo: kw / 2.0,
                    hi: kw * 1.5,
                },
            )
        })
        .collect()
}

#[test]
fn the_clock_of_these_tests_is_right() {
    assert_eq!(model::utc_day(at(0, 0)), "2026-11-20");
    assert_eq!(model::utc_day(at(23, 59)), "2026-11-20");
}

#[test]
fn an_answer_is_read_half_hour_by_half_hour() {
    let east = model::parse(EAST).unwrap();
    assert_eq!(east.len(), 96);
    assert_eq!(*east.keys().next().unwrap(), at(10, 30));
    let first = east.values().next().unwrap();
    assert!((first.kw - 1.799).abs() < 1e-9);
    assert!(east.values().all(|s| s.lo <= s.kw && s.kw <= s.hi));
    // The night forecasts nothing.
    assert_eq!(east[&at(20, 0)].kw, 0.0);
}

#[test]
fn another_step_or_a_bad_time_is_left_out_never_misplaced() {
    let body = br#"{"forecasts":[
        {"pv_estimate":1.0,"pv_estimate10":0.5,"pv_estimate90":1.5,"period_end":"2026-11-20T10:30:00.0000000Z","period":"PT30M"},
        {"pv_estimate":2.0,"period_end":"2026-11-20T10:45:00.0000000Z","period":"PT15M"},
        {"pv_estimate":3.0,"period_end":"hier","period":"PT30M"},
        {"pv_estimate":-4.0,"pv_estimate10":-5.0,"pv_estimate90":9.0,"period_end":"2026-11-20T11:00:00Z","period":"PT30M"}
    ]}"#;
    let read = model::parse(body).unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(
        read[&at(11, 0)],
        Slot {
            kw: 0.0,
            lo: 0.0,
            hi: 9.0
        }
    );
    assert!(model::parse(b"{\"response_status\":{}}").is_err());
}

#[test]
fn what_has_passed_keeps_its_forecast_what_is_ahead_is_redrawn() {
    let mut cache = Cache::default();
    cache.merge("a", slots(at(8, 0), 8, 1.0), at(8, 0));
    // At noon, a new answer: 3 kW everywhere it speaks of.
    let later = slots(at(9, 0), 8, 3.0);
    cache.merge("a", later, at(10, 0));
    let a = &cache.sites["a"];
    assert_eq!(
        a[&at(8, 30)].kw,
        1.0,
        "before the first answer's start: kept"
    );
    assert_eq!(a[&at(10, 0)].kw, 1.0, "passed when the second came: kept");
    assert_eq!(a[&at(10, 30)].kw, 3.0, "ahead: redrawn");
    assert_eq!(a[&at(13, 0)].kw, 3.0, "new: added");
    assert_eq!(cache.fetched, Some(at(10, 0)));
    // Two days later, nothing of that day is left.
    cache.merge("a", BTreeMap::new(), at(10, 0) + 3 * 86_400);
    assert!(cache.sites["a"].is_empty());
}

#[test]
fn the_roofs_are_summed() {
    let mut cache = Cache::default();
    cache.merge("east", model::parse(EAST).unwrap(), at(10, 15));
    cache.merge("west", model::parse(WEST).unwrap(), at(10, 15));
    let total = cache.total();
    let one = total[&at(12, 0)];
    let (e, w) = (
        cache.sites["east"][&at(12, 0)],
        cache.sites["west"][&at(12, 0)],
    );
    assert!((one.kw - (e.kw + w.kw)).abs() < 1e-9);
    assert!((one.hi - (e.hi + w.hi)).abs() < 1e-9);
    // A site no longer configured no longer counts.
    cache.keep_sites(&["east".into()]);
    assert!((cache.total()[&at(12, 0)].kw - e.kw).abs() < 1e-9);
}

#[test]
fn the_day_tomorrow_what_remains_and_now() {
    // Paris is UTC+1 in November: the local day starts at 23:00 UTC the day before.
    let mut total = slots(at(8, 0), 8, 2.0); // 08:00–12:00 UTC at 2 kW: 8 kWh
    total.extend(slots(at(8, 0) + 86_400, 4, 1.0)); // tomorrow 08:00–10:00 at 1 kW: 2 kWh
    let s = model::summary(&total, at(10, 15), &paris());
    assert_eq!(s.today.unwrap().kw, 8.0);
    assert_eq!(s.today.unwrap().lo, 4.0);
    assert_eq!(s.today.unwrap().hi, 12.0);
    // From 10:15 to 12:00: 1 h 45 at 2 kW.
    assert!((s.remaining.unwrap().kw - 3.5).abs() < 1e-9);
    assert_eq!(s.tomorrow.unwrap().kw, 2.0);
    assert_eq!(s.power_now, Some(2000.0));
    // The next hour, 10:15 → 11:15: 2 kWh.
    assert!((s.next_hour.unwrap() - 2.0).abs() < 1e-9);
    // At night, nothing expected now, the day still said.
    let night = model::summary(&total, at(21, 0), &paris());
    assert_eq!(night.power_now, None);
    assert_eq!(night.remaining, None);
    assert_eq!(night.today.unwrap().kw, 8.0);
}

#[test]
fn the_curve_fits_a_text_point_from_yesterday_to_tomorrow() {
    let mut cache = Cache::default();
    cache.merge("east", model::parse(EAST).unwrap(), at(10, 15));
    cache.merge("west", model::parse(WEST).unwrap(), at(10, 15));
    let text = model::curve(&cache.total(), at(10, 15), &paris()).unwrap();
    assert!(
        text.chars().count() <= moli_core::MAX_TEXT,
        "{} characters",
        text.chars().count()
    );
    let curve: serde_json::Value = serde_json::from_str(&text).unwrap();
    // Yesterday's local midnight: 2026-11-18 23:00 UTC.
    assert_eq!(curve["start"], (at(0, 0) - 86_400 - 3600) * 1000);
    assert_eq!(curve["step"], 1800);
    let kw = curve["kw"].as_array().unwrap();
    assert_eq!(kw.len(), 3 * 48);
    // Yesterday is unknown here; today's noon UTC (13:00 local) is known.
    assert!(kw[0].is_null());
    let noon = (at(12, 0) - (at(0, 0) - 86_400 - 3600)) / 1800 - 1;
    assert!(kw[usize::try_from(noon).unwrap()].as_f64().unwrap() > 0.0);
}

#[test]
fn the_calls_are_spread_over_the_light_within_the_quota() {
    let tz = paris();
    let mut cache = Cache::default();
    // Nothing known: at once.
    assert_eq!(
        model::next_refresh(&cache, 2, 10, at(10, 15), &tz),
        at(10, 15)
    );
    // A first answer at 07:00 UTC (two sites, two calls).
    cache.count_call(at(7, 0));
    cache.count_call(at(7, 0));
    // Light from 07:30 to 16:00 UTC (Paris in late November), the dark
    // half-hour before it known: the day's dawn is.
    let day = |kw: f64| {
        let mut d = slots(at(7, 0), 1, 0.0);
        d.extend(slots(at(7, 30), 17, kw));
        d
    };
    cache.merge("east", day(1.0), at(7, 0));
    cache.merge("west", day(0.5), at(7, 0));
    let next = model::next_refresh(&cache, 2, 10, at(7, 5), &tz);
    // Five refreshes a day over the light (07:30 → 16:00 UTC): the next
    // after 07:00 is the first light, 07:30.
    assert_eq!(next, at(7, 30));
    // Fetched at 07:30: the next is a quarter of the light later.
    cache.fetched = Some(at(7, 30));
    let next = model::next_refresh(&cache, 2, 10, at(7, 35), &tz);
    assert_eq!(next, at(7, 30) + (at(16, 0) - at(7, 30)) / 4);
    // Due when its time has come.
    assert_eq!(
        model::next_refresh(&cache, 2, 10, at(12, 0), &tz),
        at(12, 0)
    );
    // The quota used up: not before Solcast's next day.
    for _ in 0..8 {
        cache.count_call(at(9, 0));
    }
    assert_eq!(
        model::next_refresh(&cache, 2, 10, at(12, 0), &tz),
        at(24, 1)
    );
    let (used, _) = cache.calls_today(at(12, 0));
    assert_eq!(used, 10);
    // A new UTC day: the count starts again.
    assert_eq!(cache.calls_today(at(24, 30)), (0, false));
}

#[test]
fn a_refusal_for_the_day_waits_for_the_next() {
    let mut cache = Cache::default();
    cache.merge("east", model::parse(EAST).unwrap(), at(8, 0));
    cache.mark_exhausted(at(9, 0));
    assert_eq!(
        model::next_refresh(&cache, 1, 10, at(9, 1), &paris()),
        at(24, 1)
    );
}

#[test]
fn only_plain_site_ids_and_sound_settings_are_accepted() {
    let config = |sites: &[&str], calls: u32| Config {
        sites: sites.iter().map(|s| (*s).to_owned()).collect(),
        calls_per_day: calls,
        timezone: "Europe/Paris".into(),
    };
    assert!(Solcast::new(config(&["abcd-1234-ef56-7890"], 10)).is_ok());
    assert!(Solcast::new(config(&["../rooftop"], 10)).is_err());
    assert!(Solcast::new(config(&["a b"], 10)).is_err());
    assert!(Solcast::new(config(&[], 10)).is_err());
    assert!(Solcast::new(config(&["aaaa-1", "bbbb-2", "cccc-3"], 2)).is_err());
    let mut bad_zone = config(&["abcd-1234"], 10);
    bad_zone.timezone = "Mars/Base".into();
    assert!(Solcast::new(bad_zone).is_err());
}

#[test]
fn a_first_answer_in_the_afternoon_does_not_crowd_the_calls() {
    // Moli starts at 14:14 UTC: Solcast answers from 14:30 on, so today's
    // dawn is unknown; tomorrow's whole light is (07:30 → 16:00 UTC).
    let tz = paris();
    let mut cache = Cache::default();
    let mut answer = slots(at(14, 0), 4, 1.0); // 14:00–16:00, lit
    answer.extend(slots(at(16, 0), 16, 0.0)); // the night
    answer.extend(slots(at(7, 30) + 86_400, 17, 1.0)); // tomorrow's light
    cache.count_call(at(14, 14));
    cache.merge("only", answer, at(14, 14));
    let next = model::next_refresh(&cache, 1, 10, at(14, 15), &tz);
    // Today's light is taken from tomorrow's: ten refreshes from 07:30 to
    // 16:00, every 56 min 40 s; the first after 14:14 is 15:03:20.
    assert!(
        next >= at(14, 14) + 1800,
        "next refresh at +{} s",
        next - at(14, 14)
    );
    assert_eq!(next, at(7, 30) + (at(16, 0) - at(7, 30)) * 8 / 9);
}
