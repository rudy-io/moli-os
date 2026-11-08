//! `[energy]` in `moli.toml`: which points are energy meters, what a kWh
//! costs. Edited by humans; no surface changes it.

use std::collections::{BTreeMap, HashSet};

use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Days and months are cut in this zone (IANA name).
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default = "default_currency")]
    pub currency: String,
    /// The most a meter can draw (kW): a reading implying more is a bad
    /// one. 36 kVA is the largest French residential contract.
    #[serde(default = "default_max_power")]
    pub max_power_kw: f64,
    /// The subscription, per month (taxes included): what the bill adds to
    /// the kWh. Only the « Énergie » device's estimated bill uses it.
    #[serde(default)]
    pub monthly_fee: Option<f64>,
    /// Point holding the monthly budget (a helper, in the currency): the
    /// « Énergie » device says when the estimated bill goes over it.
    #[serde(default)]
    pub budget: Option<String>,
    /// Price by tariff period, for meters without their own price.
    #[serde(default)]
    pub tariff: Option<Tariff>,
    /// What the supplier's meter draws right now, for the live gauge.
    #[serde(default)]
    pub live: Option<Live>,
    #[serde(default, rename = "meter")]
    pub meters: Vec<MeterConfig>,
    /// Loads Moli switches on the panels' surplus and in off-peak hours.
    #[serde(default, rename = "load")]
    pub loads: Vec<crate::load::LoadConfig>,
}

fn default_timezone() -> String {
    "Europe/Paris".into()
}

fn default_currency() -> String {
    "€".into()
}

fn default_max_power() -> f64 {
    36.0
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tariff {
    /// Point holding the current period (Linky PTEC: « HP.. », « HC.. »).
    /// Observed values win; without a point, the `planning` answers alone.
    #[serde(default)]
    pub period: Option<String>,
    /// Price per kWh by period name; names compare without dots, spaces
    /// or case (« HP.. » is « hp »).
    pub prices: BTreeMap<String, f64>,
    /// A fixed schedule (EDF « heures creuses » planning): the period of
    /// every moment of the week, from the wall clock, no meter needed.
    #[serde(default)]
    pub planning: Option<Vec<Segment>>,
}

/// One row of `[energy.tariff] planning`: the period in force from `from`
/// to `to` (local « HH:MM », `to` may be « 24:00 ») on the given days.
/// The windows of each day of the week must tile [00:00, 24:00) exactly.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    /// The days of the week the window runs on.
    pub days: Vec<Weekday>,
    /// Start of the window, local « HH:MM ».
    pub from: String,
    /// End of the window, local « HH:MM » (« 24:00 » = end of day).
    pub to: String,
    /// The period in the window: a name of `prices`.
    pub period: String,
}

/// A day of the week, as a planning names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    /// All days, Monday to Sunday.
    pub const ALL: [Weekday; 7] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];

    /// From a jiff weekday.
    pub fn from_jiff(day: jiff::civil::Weekday) -> Weekday {
        use jiff::civil::Weekday as J;
        match day {
            J::Monday => Weekday::Mon,
            J::Tuesday => Weekday::Tue,
            J::Wednesday => Weekday::Wed,
            J::Thursday => Weekday::Thu,
            J::Friday => Weekday::Fri,
            J::Saturday => Weekday::Sat,
            J::Sunday => Weekday::Sun,
        }
    }
}

/// A planning window, parsed: one period of one day of the week.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub day: Weekday,
    /// Minutes after local midnight; the window is [from, to).
    pub from: u32,
    /// `to` may reach 1440 (end of day; a window never crosses midnight).
    pub to: u32,
    /// The period's key (compared without dots, spaces or case).
    pub period: String,
}

/// The validated planning, ready to answer « what period at this instant? »
/// in the configured time zone: it knows every moment, past or future.
#[derive(Clone, Debug)]
pub(crate) struct Planning {
    windows: Vec<Window>,
    tz: TimeZone,
}

impl Planning {
    #[must_use]
    pub(crate) fn new(windows: Vec<Window>, tz: TimeZone) -> Self {
        Self { windows, tz }
    }

    /// The period at `ts` (every day tiles [00:00, 24:00), so it always
    /// answers for a validated planning).
    pub(crate) fn period_at(&self, ts_ms: u64) -> Option<String> {
        let zoned = self.zoned_at(ts_ms)?;
        let minute = u32::try_from(zoned.hour()).ok()?.checked_mul(60)?
            + u32::try_from(zoned.minute()).ok()?;
        let day = Weekday::from_jiff(zoned.weekday());
        self.windows
            .iter()
            .find(|w| w.day == day && w.from <= minute && minute < w.to)
            .map(|w| w.period.clone())
    }

    /// The ms of `[a, b)` spent in `period`, exactly. Window endpoints are
    /// resolved in local wall-clock time, so a day is never assumed to be
    /// 24 h long (a DST day is 23 h or 25 h).
    pub(crate) fn time_in(&self, period: &str, a: u64, b: u64) -> u64 {
        let wanted = period_key(period);
        let Ok(a) = i64::try_from(a) else {
            return 0;
        };
        let Ok(b) = i64::try_from(b) else {
            return 0;
        };
        let Some(mut day_start) = self.day_start(a) else {
            return 0;
        };
        let mut total: u64 = 0;
        // Record spans are capped at 31 days; the margin covers them all.
        for _ in 0..400 {
            let ds = Self::to_ms(&day_start);
            if ds >= b {
                break;
            }
            let day = Weekday::from_jiff(day_start.weekday());
            for w in &self.windows {
                if w.day != day || w.period != wanted {
                    continue;
                }
                let Some(s) = self.instant_at(day_start.date(), w.from) else {
                    continue;
                };
                // `24:00` means the next local midnight (not +24 h, and not
                // hour 24 of the same day).
                let end_date = if w.to == 1440 {
                    match day_start.date().checked_add(jiff::Span::new().days(1)) {
                        Ok(d) => d,
                        Err(_) => continue,
                    }
                } else {
                    day_start.date()
                };
                let end_minutes = if w.to == 1440 { 0 } else { w.to };
                let Some(e) = self.instant_at(end_date, end_minutes) else {
                    continue;
                };
                let (lo, hi) = (s.max(a), e.min(b));
                if hi > lo {
                    total += u64::try_from(hi - lo).unwrap_or_default();
                }
            }
            day_start = match Self::next_day_start(&day_start) {
                Some(z) => z,
                None => break,
            };
        }
        total
    }

    /// The first instant of the local day containing `ts_ms` (DST-aware:
    /// not necessarily midnight).
    fn day_start(&self, ts_ms: i64) -> Option<jiff::Zoned> {
        let ts = jiff::Timestamp::from_millisecond(ts_ms).ok()?;
        ts.to_zoned(self.tz.clone()).start_of_day().ok()
    }

    /// The first instant of the next local day (the date, not +24 h: a day
    /// is not always 24 h long).
    fn next_day_start(z: &jiff::Zoned) -> Option<jiff::Zoned> {
        z.checked_add(jiff::Span::new().days(1)).ok()
    }

    /// The local wall-clock instant `minutes` (0..=1439) after midnight of
    /// `date`. Ambiguous times (DST fold) resolve to the earlier occurrence,
    /// so a window spanning the fold keeps both repeated hours.
    fn instant_at(&self, date: jiff::civil::Date, minutes: u32) -> Option<i64> {
        let hour = i8::try_from(minutes / 60).ok()?;
        let minute = i8::try_from(minutes % 60).ok()?;
        let time = jiff::civil::Time::new(hour, minute, 0, 0).ok()?;
        let dt = jiff::civil::DateTime::from_parts(date, time);
        Some(
            dt.to_zoned(self.tz.clone())
                .ok()?
                .timestamp()
                .as_millisecond(),
        )
    }

    fn to_ms(z: &jiff::Zoned) -> i64 {
        z.timestamp().as_millisecond()
    }

    fn zoned_at(&self, ts_ms: u64) -> Option<jiff::Zoned> {
        let ts = i64::try_from(ts_ms).ok()?;
        jiff::Timestamp::from_millisecond(ts)
            .ok()
            .map(|t| t.to_zoned(self.tz.clone()))
    }
}

/// The live draw at the supplier's meter (Linky PAPP, in VA).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Live {
    /// Instantaneous power point (W or VA).
    pub power: String,
    /// Full scale, in the point's unit: the contract (9 kVA → 9000).
    pub max: f64,
    /// From here the draw is « soutenue » (the gauge turns orange, then red
    /// at twice as much). By default a quarter of `max`.
    #[serde(default)]
    pub warn: Option<f64>,
}

impl Live {
    #[must_use]
    pub fn warn(&self) -> f64 {
        self.warn.unwrap_or(self.max / 4.0)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterConfig {
    /// Storage key: never rename it (history is filed under it).
    pub id: String,
    pub name: String,
    /// A cumulative counter, in Wh or kWh (an index, or a counter that
    /// restarts each day or month); with `integrate`, a power point (W or
    /// kW) whose energy Moli adds up itself.
    pub point: String,
    pub role: Role,
    /// Fixed price per kWh (a Linky « heures creuses » index).
    #[serde(default)]
    pub price: Option<f64>,
    /// Instantaneous power point (W), for the live view.
    #[serde(default)]
    pub power: Option<String>,
    #[serde(default)]
    pub reset: Reset,
    /// Tariff period this counter runs in (a Linky « HC » index only moves
    /// in « heures creuses »): its energy is spread over that time only.
    #[serde(default)]
    pub period: Option<String>,
    /// `point` is a power: energy = power × time, held between readings.
    /// For plugs whose own counter cannot be trusted (a Tuya `add_ele`
    /// restarts at every report).
    #[serde(default)]
    pub integrate: bool,
    /// An appliance's circuit (a `circuit` or `total` meter id), when it is
    /// known: shown inside it. Without it, it is part of what no circuit
    /// explains.
    #[serde(default)]
    pub within: Option<String>,
    /// Devices this meter feeds (a circuit behind a clim): their cards show
    /// its power. Shared with others when it feeds more than them.
    #[serde(default)]
    pub feeds: Vec<String>,
    /// An appliance that measures nothing: a smart light, whose power Moli
    /// estimates from its model's measured profile and its state (`point`:
    /// any point of the light, or of a fixture `lumieres:…`). Shown « ≈ ».
    #[serde(default)]
    pub estimate: bool,
    /// `point` holds consumption already split by hour, as JSON text
    /// (`[{"hour": ms, "kwh": …}]`): a service that reports a day late. Each
    /// hour is filed as it is, never spread over the time of the report.
    #[serde(default)]
    pub history: bool,
}

impl MeterConfig {
    /// The point that says the power now, for the live views.
    #[must_use]
    pub fn power_point(&self) -> Option<&str> {
        self.power
            .as_deref()
            .or_else(|| self.integrate.then_some(self.point.as_str()))
    }
}

/// What a meter measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Billed by the supplier (Linky indexes): the reference for kWh and cost.
    Grid,
    /// The whole home, measured finely (a clamp on the main feed).
    Total,
    /// One circuit of the panel: part of the total.
    Circuit,
    /// One appliance (a plug, a machine's own meter): shown on its own,
    /// never added to the total nor taken from what circuits leave over.
    Appliance,
    /// What solar panels produce (an inverter's lifetime index): never
    /// billed; with `export`, tells what the home used of it.
    Solar,
    /// What the home sends to the grid (the returned energy of a clamp on
    /// the main feed, a Linky injection index): `price` is what it earns.
    Export,
}

impl Role {
    /// Meters that count energy the home takes (grid, whole home, circuits,
    /// appliances), as opposed to what it makes or sends out.
    #[must_use]
    pub fn consumes(self) -> bool {
        !matches!(self, Self::Solar | Self::Export)
    }
}

/// What a decrease of the counter means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reset {
    /// A fall to under half is a restart (daily or monthly counter, device
    /// reboot); a smaller fall is a bad reading, ignored.
    #[default]
    Auto,
    /// The counter never restarts (an index): every fall is ignored.
    Never,
}

impl Tariff {
    /// The fixed off-peak/planning windows, parsed and validated: every day
    /// of the week must be covered exactly once, no gaps, no overlaps.
    pub fn planning_windows(&self) -> Result<Vec<Window>, String> {
        let Some(segments) = &self.planning else {
            return Ok(Vec::new());
        };
        let mut windows: Vec<Window> = Vec::new();
        for s in segments {
            if s.days.is_empty() {
                return Err("planning: a window needs at least one day".into());
            }
            let from = parse_hhmm(&s.from, false)?;
            let to = parse_hhmm(&s.to, true)?;
            if from >= to {
                return Err(format!("planning: window {} → {} is empty", s.from, s.to));
            }
            if self
                .prices
                .iter()
                .find(|(name, _)| period_key(name) == period_key(&s.period))
                .is_none()
            {
                return Err(format!(
                    "planning: period {:?} has no price in `prices`",
                    s.period
                ));
            }
            for day in &s.days {
                windows.push(Window {
                    day: *day,
                    from,
                    to,
                    period: period_key(&s.period),
                });
            }
        }
        for day in Weekday::ALL {
            let mut day_windows: Vec<&Window> = windows.iter().filter(|w| w.day == day).collect();
            if day_windows.is_empty() {
                return Err(format!(
                    "planning: no windows for {day:?} — every day of the week needs coverage"
                ));
            }
            day_windows.sort_by_key(|w| w.from);
            let mut cursor = 0;
            for w in day_windows {
                if w.from != cursor {
                    return Err(format!(
                        "planning: {day:?} is not covered exactly (expected a window starting at {:02}:{:02})",
                        cursor / 60,
                        cursor % 60
                    ));
                }
                cursor = w.to;
            }
            if cursor != 1440 {
                return Err(format!(
                    "planning: {day:?} windows end at {:02}:{:02}, must reach 24:00",
                    cursor / 60,
                    cursor % 60
                ));
            }
        }
        Ok(windows)
    }
}

/// Parse a local « HH:MM » time into minutes since midnight.
/// At window ends, « 24:00 » is accepted as end of day (1440).
fn parse_hhmm(text: &str, is_end: bool) -> Result<u32, String> {
    let (h, m) = text
        .trim()
        .split_once(':')
        .ok_or_else(|| format!("{text:?} is not « HH:MM »"))?;
    let (h, m) = (
        h.parse::<u32>()
            .map_err(|_| format!("hour {h:?} is not a number"))?,
        m.parse::<u32>()
            .map_err(|_| format!("minute {m:?} is not a number"))?,
    );
    if h > 24 || m > 59 || (h == 24 && m != 0) {
        return Err(format!("{text:?} is out of range"));
    }
    if !is_end && h == 24 {
        return Err(format!("{text:?} can only be a window end"));
    }
    Ok(h * 60 + m)
}

impl Config {
    pub fn check(&self) -> Result<(), String> {
        TimeZone::get(&self.timezone).map_err(|e| format!("timezone {:?}: {e}", self.timezone))?;
        check_meters(&self.meters)?;
        let mut load_ids = HashSet::new();
        for load in &self.loads {
            load.check()?;
            if !load_ids.insert(&load.id) {
                return Err(format!("duplicate load id {:?}", load.id));
            }
        }
        if let Some(live) = &self.live {
            check_point(&live.power).map_err(|e| format!("[energy.live] {e}"))?;
            if !(live.max.is_finite() && live.max > 0.0) {
                return Err(format!("[energy.live] max {} must be above 0", live.max));
            }
            if let Some(warn) = live.warn
                && !(warn.is_finite() && warn > 0.0 && warn <= live.max)
            {
                return Err(format!(
                    "[energy.live] warn {warn} must be above 0 and at most max"
                ));
            }
        }
        if let Some(fee) = self.monthly_fee {
            check_price(fee).map_err(|e| format!("monthly_fee: {e}"))?;
        }
        if let Some(budget) = &self.budget {
            check_point(budget).map_err(|e| format!("budget: {e}"))?;
        }
        if let Some(t) = &self.tariff {
            match (&t.period, &t.planning) {
                (Some(_), Some(_)) => {
                    return Err(
                        "[energy.tariff] `period` and `planning` are mutually exclusive".into(),
                    );
                }
                (None, None) => {
                    return Err("[energy.tariff] needs a `period` point or a `planning`".into());
                }
                _ => {}
            }
            if let Some(point) = &t.period {
                check_point(point).map_err(|e| format!("[energy.tariff] {e}"))?;
            }
            for (period, price) in &t.prices {
                check_price(*price).map_err(|e| format!("[energy.tariff] {period}: {e}"))?;
            }
            t.planning_windows()
                .map_err(|e| format!("[energy.tariff] {e}"))?;
        }
        Ok(())
    }

    /// The price of a kWh in this period, if the tariff knows it.
    #[must_use]
    pub fn period_price(&self, period: &str) -> Option<f64> {
        let wanted = period_key(period);
        self.tariff
            .as_ref()?
            .prices
            .iter()
            .find(|(name, _)| period_key(name) == wanted)
            .map(|(_, price)| *price)
    }
}

/// Period names compare without dots, spaces or case.
#[must_use]
pub fn period_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Per-meter checks: unique valid ids, valid points, and role coherence
/// (`within`, `period`, prices, `estimate`).
fn check_meters(meters: &[MeterConfig]) -> Result<(), String> {
    let mut ids = HashSet::new();
    for m in meters {
        if m.id.is_empty()
            || !m
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!(
                "meter id {:?}: letters, digits, '_' or '-' only",
                m.id
            ));
        }
        if !ids.insert(&m.id) {
            return Err(format!("duplicate meter id {:?}", m.id));
        }
        for point in std::iter::once(&m.point).chain(&m.power) {
            check_point(point).map_err(|e| format!("meter {:?}: {e}", m.id))?;
        }
        if let Some(bad) = m.feeds.iter().find(|d| !d.contains(':') || d.contains('/')) {
            return Err(format!(
                "meter {:?}: feeds {bad:?} is not a device id",
                m.id
            ));
        }
        if let Some(within) = &m.within {
            let inside = meters
                .iter()
                .any(|o| &o.id == within && matches!(o.role, Role::Circuit | Role::Total));
            if m.role != Role::Appliance || !inside {
                return Err(format!(
                    "meter {:?}: `within` is for an appliance, naming a circuit or total meter",
                    m.id
                ));
            }
        }
        if !m.role.consumes() && m.period.is_some() {
            return Err(format!(
                "meter {:?}: `period` is for a meter the supplier bills",
                m.id
            ));
        }
        if m.role == Role::Solar && m.price.is_some() {
            return Err(format!(
                "meter {:?}: solar production has no price (an `export` meter's price is what selling earns)",
                m.id
            ));
        }
        if m.estimate && (m.role != Role::Appliance || m.integrate || m.power.is_some()) {
            return Err(format!(
                "meter {:?}: `estimate` is for an appliance, without `integrate` nor `power`",
                m.id
            ));
        }
        if m.history && (!m.role.consumes() || m.integrate || m.estimate || m.period.is_some()) {
            return Err(format!(
                "meter {:?}: `history` is for a meter that consumes, without `integrate`, `estimate` nor `period`",
                m.id
            ));
        }
        if let Some(price) = m.price {
            check_price(price).map_err(|e| format!("meter {:?}: {e}", m.id))?;
        }
    }
    Ok(())
}

fn check_point(point: &str) -> Result<(), String> {
    match point.split_once('/') {
        Some((device, key)) if device.contains(':') && !key.is_empty() => Ok(()),
        _ => Err(format!("{point:?} is not a point id (instance:device/key)")),
    }
}

pub(crate) fn check_price(price: f64) -> Result<(), String> {
    if price.is_finite() && price >= 0.0 {
        Ok(())
    } else {
        Err(format!("price {price} must be a positive number"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<(), String> {
        toml::from_str::<Config>(text)
            .map_err(|e| e.to_string())?
            .check()
    }

    /// The house's EDF planning: mixed days (Mon/Tue/Thu/Fri), all-HC
    /// Wednesday and weekend.
    const EDF_PLANNING: &str = r#"
        [tariff]
        prices = { "HP" = 0.17, "HC" = 0.14 }
        [[tariff.planning]]
        days = ["mon", "tue", "thu", "fri"]
        from = "00:00"
        to = "01:28"
        period = "HP"
        [[tariff.planning]]
        days = ["mon", "tue", "thu", "fri"]
        from = "01:28"
        to = "06:58"
        period = "HC"
        [[tariff.planning]]
        days = ["mon", "tue", "thu", "fri"]
        from = "06:58"
        to = "13:58"
        period = "HP"
        [[tariff.planning]]
        days = ["mon", "tue", "thu", "fri"]
        from = "13:58"
        to = "16:28"
        period = "HC"
        [[tariff.planning]]
        days = ["mon", "tue", "thu", "fri"]
        from = "16:28"
        to = "24:00"
        period = "HP"
        [[tariff.planning]]
        days = ["wed", "sat", "sun"]
        from = "00:00"
        to = "24:00"
        period = "HC"
    "#;

    #[test]
    fn checks_ids_points_prices_and_zone() {
        let ok = r#"
            [tariff]
            period = "z2m:linky/ptec"
            prices = { "HP" = 0.17, "HC" = 0.13 }
            [[meter]]
            id = "hc"
            name = "Heures creuses"
            point = "z2m:linky/tier1"
            role = "grid"
            price = 0.13
        "#;
        parse(ok).unwrap();
        assert!(parse(&ok.replace("\"hc\"", "\"h c\"")).is_err());
        assert!(parse(&ok.replace("z2m:linky/tier1", "tier1")).is_err());
        assert!(parse(&ok.replace("price = 0.13", "price = -1.0")).is_err());
        assert!(parse(&format!("timezone = \"Mars/Base\"\n{ok}")).is_err());
        assert!(parse(&ok.replace("role = \"grid\"", "role = \"solar\"")).is_err());
        let live = "[live]\npower = \"z2m:linky/apparent_power\"\nmax = 9000\n";
        parse(&format!("{live}{ok}")).unwrap();
        assert!(parse(&format!("{}{ok}", live.replace("9000", "0"))).is_err());
        assert!(parse(&format!("{}{ok}", live.replace("z2m:linky/", ""))).is_err());
        parse(&format!("{live}warn = 2000\n{ok}")).unwrap();
        assert!(parse(&format!("{live}warn = 9500\n{ok}")).is_err());
        let plug = "[[meter]]\nid = \"tv\"\nname = \"Télé\"\npoint = \"tuya:plug/cur_power\"\nrole = \"appliance\"\nintegrate = true\n";
        parse(&format!("{ok}{plug}")).unwrap();
        assert!(
            parse(&format!("{ok}{plug}within = \"hc\"\n")).is_err(),
            "inside a grid meter"
        );
        let circuit = "[[meter]]\nid = \"c3\"\nname = \"Circuit 3\"\npoint = \"em:x/e3\"\nrole = \"circuit\"\n";
        parse(&format!("{ok}{circuit}{plug}within = \"c3\"\n")).unwrap();
        let late = "[[meter]]\nid = \"linky\"\nname = \"Linky\"\npoint = \"enedis:linky/hours\"\nrole = \"grid\"\nhistory = true\n";
        assert!(parse(&format!("{ok}{late}")).is_ok());
        assert!(parse(&format!("{ok}{late}integrate = true\n")).is_err());
        let lamp = "[[meter]]\nid = \"lampe\"\nname = \"Lampe\"\npoint = \"hue:l/on\"\nrole = \"appliance\"\nestimate = true\n";
        parse(&format!("{ok}{lamp}")).unwrap();
        assert!(parse(&format!("{ok}{}", lamp.replace("appliance", "circuit"))).is_err());
        assert!(parse(&format!("{ok}{lamp}integrate = true\n")).is_err());
        let bill = "monthly_fee = 19.56\nbudget = \"maison:budget/value\"\n";
        parse(&format!("{bill}{ok}")).unwrap();
        assert!(parse(&format!("{}{ok}", bill.replace("19.56", "-1.0"))).is_err());
        assert!(parse(&format!("{}{ok}", bill.replace("maison:", ""))).is_err());
    }

    #[test]
    fn period_names_ignore_dots_and_case() {
        let c: Config = toml::from_str(
            "[tariff]\nperiod = \"a:b/c\"\nprices = { \"HP\" = 0.17, \"HC\" = 0.13 }",
        )
        .unwrap();
        assert_eq!(c.period_price("HP.."), Some(0.17));
        assert_eq!(c.period_price(" hc "), Some(0.13));
        assert_eq!(c.period_price("BASE"), None);
    }

    fn planning_segment(days: &[&str], from: &str, to: &str, period: &str) -> String {
        let days_quoted: Vec<String> = days.iter().map(|d| format!("\"{d}\"")).collect();
        format!(
            "[[tariff.planning]]\ndays = [{}]\nfrom = \"{}\"\nto = \"{}\"\nperiod = \"{}\"\n",
            days_quoted.join(", "),
            from,
            to,
            period
        )
    }

    const BASE_PRICES: &str = "[tariff]\nprices = { \"hp\" = 0.17, \"hc\" = 0.14 }\n";

    const ALL_DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

    fn paris_ts(y: i16, m: i8, d: i8, h: i8, min: i8) -> u64 {
        jiff::civil::DateTime::new(y, m, d, h, min, 0, 0)
            .unwrap()
            .in_tz("Europe/Paris")
            .unwrap()
            .timestamp()
            .as_millisecond()
            .try_into()
            .unwrap()
    }

    #[test]
    fn planning_tiles_every_day() {
        // The house's EDF planning: mixed days, all-HC weekends and
        // Wednesday.
        let c: Config = toml::from_str(EDF_PLANNING).unwrap();
        c.check().unwrap();
        let windows = c.tariff.as_ref().unwrap().planning_windows().unwrap();
        assert_eq!(windows.len(), 5 * 4 + 3);
        // A mixed day: off-peak runs 01:28 -> 06:58 and 13:58 -> 16:28.
        let monday = windows.iter().filter(|w| w.day == Weekday::Mon).count();
        assert_eq!(monday, 5);
        let wednesday = windows.iter().find(|w| w.day == Weekday::Wed).unwrap();
        assert_eq!(
            (wednesday.from, wednesday.to, wednesday.period.as_str()),
            (0u32, 1440u32, "hc")
        );
    }

    #[test]
    fn planning_rejects_invalid_windows() {
        let all_days: Vec<&str> = ALL_DAYS.to_vec();
        // A gap at the end of the day: refused.
        assert!(
            parse(&format!(
                "{BASE_PRICES}{}",
                planning_segment(&all_days, "00:00", "23:59", "hc")
            ))
            .is_err()
        );
        // An overlap on the same day: refused.
        assert!(
            parse(&format!(
                "{BASE_PRICES}{}{}",
                planning_segment(&["mon"], "00:00", "12:00", "hc"),
                planning_segment(&["mon"], "11:00", "24:00", "hp")
            ))
            .is_err()
        );
        // A missing day: refused.
        assert!(
            parse(&format!(
                "{BASE_PRICES}{}",
                planning_segment(
                    &["mon", "tue", "wed", "thu", "fri", "sat"],
                    "00:00",
                    "24:00",
                    "hc"
                )
            ))
            .is_err()
        );
        // An unknown period: refused.
        assert!(
            parse(&format!(
                "{BASE_PRICES}{}",
                planning_segment(&all_days, "00:00", "24:00", "BASE")
            ))
            .is_err()
        );
        // An empty window: refused.
        assert!(
            parse(&format!(
                "{BASE_PRICES}{}",
                planning_segment(&all_days, "10:00", "10:00", "hc")
            ))
            .is_err()
        );
        // A bad start time: refused.
        assert!(
            parse(&format!(
                "{BASE_PRICES}{}",
                planning_segment(&all_days, "25:00", "24:00", "hc")
            ))
            .is_err()
        );
        // A bad end time: refused.
        assert!(
            parse(&format!(
                "{BASE_PRICES}{}",
                planning_segment(&all_days, "00:00", "24:30", "hc")
            ))
            .is_err()
        );
        // `24:00` is an end, never a start.
        assert!(
            parse(&format!(
                "{BASE_PRICES}{}",
                planning_segment(&all_days, "24:00", "24:00", "hc")
            ))
            .is_err()
        );
        // Duplicated days in one window: the day tiles twice.
        assert!(
            parse(&format!(
                "{BASE_PRICES}{}",
                planning_segment(
                    &["mon", "mon", "tue", "wed", "thu", "fri", "sat", "sun"],
                    "00:00",
                    "24:00",
                    "hc"
                )
            ))
            .is_err()
        );
        // The period's case and dots do not matter.
        assert!(
            parse(&format!(
                "{BASE_PRICES}{}",
                planning_segment(&all_days, "00:00", "24:00", "Hc.")
            ))
            .is_ok()
        );
    }

    #[test]
    fn planning_period_at_and_time_in() {
        let c: Config = toml::from_str(EDF_PLANNING).unwrap();
        let windows = c.tariff.as_ref().unwrap().planning_windows().unwrap();
        let planning = Planning::new(windows, jiff::tz::TimeZone::get("Europe/Paris").unwrap());

        // Monday 2026-10-19 (mixed day), including the exact boundaries.
        assert_eq!(
            planning.period_at(paris_ts(2026, 10, 19, 0, 30)).as_deref(),
            Some("hp")
        );
        assert_eq!(
            planning.period_at(paris_ts(2026, 10, 19, 1, 27)).as_deref(),
            Some("hp")
        );
        assert_eq!(
            planning.period_at(paris_ts(2026, 10, 19, 1, 28)).as_deref(),
            Some("hc")
        );
        assert_eq!(
            planning.period_at(paris_ts(2026, 10, 19, 6, 58)).as_deref(),
            Some("hp")
        );
        assert_eq!(
            planning.period_at(paris_ts(2026, 10, 19, 8, 0)).as_deref(),
            Some("hp")
        );
        assert_eq!(
            planning
                .period_at(paris_ts(2026, 10, 19, 14, 30))
                .as_deref(),
            Some("hc")
        );
        assert_eq!(
            planning
                .period_at(paris_ts(2026, 10, 19, 16, 28))
                .as_deref(),
            Some("hp")
        );
        assert_eq!(
            planning
                .period_at(paris_ts(2026, 10, 19, 23, 59))
                .as_deref(),
            Some("hp")
        );
        // Wednesday 2026-10-21 and Saturday 2026-10-24: all day HC.
        assert_eq!(
            planning.period_at(paris_ts(2026, 10, 21, 12, 0)).as_deref(),
            Some("hc")
        );
        assert_eq!(
            planning.period_at(paris_ts(2026, 10, 21, 23, 0)).as_deref(),
            Some("hc")
        );
        assert_eq!(
            planning.period_at(paris_ts(2026, 10, 24, 9, 0)).as_deref(),
            Some("hc")
        );

        // Full mixed day: HC = 01:28-06:58 + 13:58-16:28 = 8 h.
        let mon0 = paris_ts(2026, 10, 19, 0, 0);
        let mon1 = paris_ts(2026, 10, 20, 0, 0);
        assert_eq!(planning.time_in("hc", mon0, mon1), 8 * 3_600_000);
        assert_eq!(planning.time_in("hp", mon0, mon1), 16 * 3_600_000);
        // Full Wednesday: 24 h of HC.
        let wed0 = paris_ts(2026, 10, 21, 0, 0);
        let wed1 = paris_ts(2026, 10, 22, 0, 0);
        assert_eq!(planning.time_in("hc", wed0, wed1), 24 * 3_600_000);

        // Crossing midnight: Mon 20:00 -> Tue 20:00 gives the same 8 h HC / 16 h HP.
        let a = paris_ts(2026, 10, 19, 20, 0);
        let b = paris_ts(2026, 10, 20, 20, 0);
        assert_eq!(planning.time_in("hc", a, b), 8 * 3_600_000);
        assert_eq!(planning.time_in("hp", a, b), 16 * 3_600_000);

        // DST autumn: Sunday 2026-10-25 is a 25 h day in Europe/Paris
        // (clocks go back at 03:00 CEST -> 02:00 CET).
        let dst0 = paris_ts(2026, 10, 25, 0, 0);
        let dst1 = paris_ts(2026, 10, 26, 0, 0);
        assert_eq!(planning.time_in("hc", dst0, dst1), 25 * 3_600_000);
        // DST spring: Sunday 2026-03-29 is a 23 h day (clocks go forward).
        let spring0 = paris_ts(2026, 3, 29, 0, 0);
        let spring1 = paris_ts(2026, 3, 30, 0, 0);
        assert_eq!(planning.time_in("hc", spring0, spring1), 23 * 3_600_000);
    }
}
