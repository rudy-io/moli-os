//! A Micronova board as its cloud describes it: a register map (each value's
//! offset, mask, formula and words) and a reading (offset → raw value). The
//! points a stove shows, and their values.
//!
//! A value is the raw one ANDed with its mask, then put through its formula,
//! where `#` stands for it. Formulas are plain arithmetic (`#/2`, `#+30`,
//! `int(#/4) +1`): evaluated here, never run.
//!
//! Besides its numbers, a board keeps a clock (in binary-coded decimal,
//! which its map does not say: 0x24 is the 24th) and up to six programs,
//! each switching the stove on then off at times in steps of ten minutes,
//! on the days ticked.

use std::collections::HashMap;
use std::sync::Arc;

use moli_core::{Access, Kind, PointSpec, Semantic, Unit, Value};
use serde::Deserialize;
use serde_json::Value as Json;

#[derive(Clone, Debug, Deserialize)]
pub struct Register {
    pub reg_key: String,
    pub offset: u32,
    #[serde(default = "all_bits")]
    pub mask: u32,
    #[serde(default)]
    pub formula: Option<String>,
    #[serde(default)]
    pub set_min: Option<f64>,
    #[serde(default)]
    pub set_max: Option<f64>,
    #[serde(default, deserialize_with = "nullable")]
    pub enc_val: Vec<Word>,
}

impl Register {
    /// A value as the board says it: its word (`off`, `auto`), else the number.
    fn step(&self, v: i64) -> String {
        #[allow(clippy::cast_precision_loss)]
        let word = self
            .enc_val
            .iter()
            .find(|w| w.lang == "ENG" && w.value.as_f64() == Some(v as f64));
        word.map_or_else(|| v.to_string(), |w| slug(&w.description))
    }
}

fn all_bits() -> u32 {
    0xFFFF
}

fn nullable<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Word>, D::Error> {
    Ok(Option::<Vec<Word>>::deserialize(d)?.unwrap_or_default())
}

/// A value's word, in one language.
#[derive(Clone, Debug, Deserialize)]
pub struct Word {
    pub lang: String,
    pub description: String,
    pub value: Json,
}

/// The registers of one board, by name.
#[derive(Clone, Debug, Default)]
pub struct Map {
    registers: HashMap<String, Register>,
}

/// Offset → raw value, as a reading answers.
pub type Reading = HashMap<u32, i64>;

impl Map {
    #[must_use]
    pub fn new(registers: Vec<Register>) -> Self {
        Self {
            registers: registers
                .into_iter()
                .map(|r| (r.reg_key.clone(), r))
                .collect(),
        }
    }

    fn get(&self, key: &str) -> Option<&Register> {
        self.registers.get(key)
    }

    /// A register's value: masked, through its formula.
    #[must_use]
    pub fn value(&self, key: &str, reading: &Reading) -> Option<f64> {
        let r = self.get(key)?;
        let raw = reading.get(&r.offset)? & i64::from(r.mask);
        #[allow(clippy::cast_precision_loss)]
        let raw = raw as f64;
        calc(r.formula.as_deref().unwrap_or("#"), raw)
    }

    /// A register's word for its value (English), if it has words.
    #[must_use]
    pub fn word(&self, key: &str, reading: &Reading) -> Option<String> {
        let value = self.value(key, reading)?;
        self.get(key)?
            .enc_val
            .iter()
            .find(|w| w.lang == "ENG" && w.value.as_f64() == Some(value))
            .map(|w| w.description.clone())
    }

    fn words(&self, key: &str) -> Vec<String> {
        self.get(key)
            .map(|r| {
                r.enc_val
                    .iter()
                    .filter(|w| w.lang == "ENG")
                    .map(|w| slug(&w.description))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The points a stove with this map shows (only those it has).
    #[must_use]
    pub fn points(&self) -> Vec<PointSpec> {
        let mut out = Vec::new();
        if self.get(STATE).is_some() {
            out.push(spec(
                "on",
                "pilotes.micronova.marche",
                Kind::Binary,
                None,
                Semantic::OnOff,
            ));
            let values = self
                .words(STATE)
                .iter()
                .map(|w| Arc::from(w.as_str()))
                .collect();
            out.push(spec(
                "mode",
                "pilotes.micronova.etat",
                Kind::Enum { values },
                None,
                Semantic::Other,
            ));
        }
        for (key, register, label, semantic) in self.numbers() {
            if let Some(r) = self.get(register) {
                let unit = unit_of(key);
                let kind = Kind::Numeric {
                    min: r.set_min,
                    max: r.set_max,
                    step: None,
                };
                out.push(spec(key, label, kind, unit, *semantic));
            }
        }
        if self.get(ALARM).is_some() {
            out.push(spec(
                "alarm",
                "pilotes.micronova.alarme",
                Kind::Text,
                None,
                Semantic::Other,
            ));
        }
        if self.get(ECO_STOP).is_some() {
            out.push(spec(
                "eco_stop",
                "pilotes.micronova.eco_stop",
                Kind::Binary,
                None,
                Semantic::Other,
            ));
        }
        if self.get(FAN).is_some() {
            let values = self
                .steps(FAN)
                .iter()
                .map(|s| Arc::from(s.as_str()))
                .collect();
            out.push(spec(
                "fan",
                "pilotes.micronova.ventilation",
                Kind::Enum { values },
                None,
                Semantic::Other,
            ));
        }
        if self.get(&chrono(1, "enable_set")).is_some() {
            out.push(spec(
                PROGRAMS,
                "pilotes.micronova.programmes",
                Kind::Text,
                None,
                Semantic::Other,
            ));
        }
        if CLOCK_KEYS.iter().all(|k| self.get(k).is_some()) {
            out.push(spec(
                CLOCK,
                "pilotes.micronova.horloge",
                Kind::Text,
                None,
                Semantic::Other,
            ));
        }
        out
    }

    /// The numbers a stove reads, a register once: another name for one
    /// already read (the front fan, on a stove with one fan) is left out.
    fn numbers(
        &self,
    ) -> impl Iterator<Item = &(&'static str, &'static str, &'static str, Semantic)> {
        NUMBERS.iter().enumerate().filter_map(|(i, n)| {
            let at = |register: &str| self.get(register).map(|r| (r.offset, r.mask));
            let earlier = std::iter::once(FAN)
                .chain(NUMBERS[..i].iter().map(|e| e.1))
                .any(|register| at(register).is_some() && at(register) == at(n.1));
            (!earlier).then_some(n)
        })
    }

    /// A register's raw value, masked.
    fn raw(&self, key: &str, reading: &Reading) -> Option<i64> {
        let r = self.get(key)?;
        Some(reading.get(&r.offset)? & i64::from(r.mask))
    }

    /// Each value a register takes, from its least to its greatest: its
    /// word when it has one (`off`, `auto`), else the number.
    fn steps(&self, key: &str) -> Vec<String> {
        let Some(r) = self.get(key) else {
            return Vec::new();
        };
        #[allow(clippy::cast_possible_truncation)]
        let (min, max) = (
            r.set_min.unwrap_or(0.0) as i64,
            r.set_max.unwrap_or(0.0) as i64,
        );
        (min..=max.min(min + 32)).map(|v| r.step(v)).collect()
    }

    /// The programs: for each, whether it is on, its start and stop in
    /// minutes from midnight, and its days (1 = Monday … 7 = Sunday), as
    /// JSON: `[{"on":true,"start":420,"stop":1350,"days":[1,2,3,4,5]}, …]`.
    #[must_use]
    pub fn programs(&self, reading: &Reading) -> Option<String> {
        let mut out = Vec::new();
        for n in 1..=MAX_PROGRAMS {
            if self.get(&chrono(n, "enable_set")).is_none() {
                break;
            }
            let read = |part: &str| self.value(&chrono(n, part), reading);
            #[allow(clippy::cast_possible_truncation)]
            let minutes = |part: &str| read(part).map(|slot| (slot * 10.0) as i64);
            let days: Vec<usize> = (1..=7)
                .filter(|&d| read(&format!("day_{}_set", DAYS[d - 1])) == Some(1.0))
                .collect();
            out.push(serde_json::json!({
                "on": read("enable_set")? != 0.0,
                "start": minutes("start_set")?,
                "stop": minutes("stop_set")?,
                "days": days,
            }));
        }
        (!out.is_empty()).then(|| Json::Array(out).to_string())
    }

    /// The stove's clock, as it keeps it: `2026-10-24T01:58` (its own time,
    /// the one its programs follow), or `None` if it makes no sense.
    #[must_use]
    pub fn clock(&self, reading: &Reading) -> Option<String> {
        let mut parts = [0i64; 5];
        for (part, key) in parts.iter_mut().zip(CLOCK_KEYS) {
            let raw = bcd(self.raw(key, reading)?)?;
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let value = calc(self.get(key)?.formula.as_deref().unwrap_or("#"), raw as f64)? as i64;
            *part = value;
        }
        let [year, month, day, hour, minute] = parts;
        let sane = (2000..2100).contains(&year)
            && (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && (0..24).contains(&hour)
            && (0..60).contains(&minute);
        sane.then(|| format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}"))
    }

    /// The points' values for a reading (those it can say).
    #[must_use]
    pub fn values(&self, reading: &Reading) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        if let Some(word) = self.word(STATE, reading) {
            let state = slug(&word);
            out.push(("on".into(), Value::Bool(!OFF.contains(&state.as_str()))));
            out.push(("mode".into(), Value::Text(state.into())));
        }
        for (key, register, _, _) in self.numbers() {
            if let Some(v) = self.value(register, reading) {
                out.push(((*key).into(), Value::Float(v)));
            }
        }
        if self.get(ALARM).is_some() {
            let alarm = self
                .word(ALARM, reading)
                .or_else(|| self.value(ALARM, reading).map(|v| format!("{v}")));
            if let Some(alarm) = alarm {
                out.push(("alarm".into(), Value::Text(alarm.into())));
            }
        }
        if let Some(v) = self.value(ECO_STOP, reading) {
            out.push(("eco_stop".into(), Value::Bool(v != 0.0)));
        }
        if let (Some(r), Some(v)) = (self.get(FAN), self.value(FAN, reading)) {
            #[allow(clippy::cast_possible_truncation)]
            out.push(("fan".into(), Value::Text(r.step(v as i64).into())));
        }
        if let Some(programs) = self.programs(reading) {
            out.push((PROGRAMS.into(), Value::Text(programs.into())));
        }
        if let Some(clock) = self.clock(reading) {
            out.push((CLOCK.into(), Value::Text(clock.into())));
        }
        out
    }
}

/// `0x24` → 24; `None` for a nibble above 9.
fn bcd(raw: i64) -> Option<i64> {
    let (mut value, mut scale, mut rest) = (0, 1, raw);
    while rest > 0 {
        let digit = rest & 0xF;
        if digit > 9 {
            return None;
        }
        value += digit * scale;
        scale *= 10;
        rest >>= 4;
    }
    Some(value)
}

/// A program's register: `chrono_p1_enable_set`, `chrono_p3_day_monday_set`.
fn chrono(n: u8, part: &str) -> String {
    format!("chrono_p{n}_{part}")
}

const STATE: &str = "status_get";
const ALARM: &str = "alarms_get";
const ECO_STOP: &str = "eco_stop_set";
const FAN: &str = "vent_main_set";
/// The points of the programs and of the clock.
pub const PROGRAMS: &str = "programs";
pub const CLOCK: &str = "clock";
const MAX_PROGRAMS: u8 = 6;
const DAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];
/// The clock's registers: year, month, day, hour, minute.
const CLOCK_KEYS: [&str; 5] = [
    "calendar_year_set",
    "calendar_month_set",
    "calendar_day_set",
    "clock_hour_set",
    "clock_minute_set",
];
/// States in which the stove is not burning.
const OFF: [&str; 4] = ["off", "shut_off", "alarm", "mem_alarm"];
/// Point, register, label, semantic: what a stove reads in numbers.
const NUMBERS: &[(&str, &str, &str, Semantic)] = &[
    (
        "temperature",
        "temp_air_get",
        "pilotes.micronova.temperature",
        Semantic::Temperature,
    ),
    (
        "target_temperature",
        "temp_air_set",
        "pilotes.micronova.consigne",
        Semantic::Temperature,
    ),
    (
        "flue_temperature",
        "temp_gas_flue_get",
        "pilotes.micronova.fumees",
        Semantic::Other,
    ),
    (
        "power_level",
        "power_set",
        "pilotes.micronova.puissance",
        Semantic::Other,
    ),
    (
        "real_power",
        "real_power_get",
        "pilotes.micronova.puissance_reelle",
        Semantic::Other,
    ),
    (
        "fan_front",
        "vent_front_set",
        "pilotes.micronova.ventilateur_avant",
        Semantic::Other,
    ),
    (
        "eco_stop_delay",
        "eco_temp_stop",
        "pilotes.micronova.eco_stop_delai",
        Semantic::Other,
    ),
];
const CELSIUS: [&str; 3] = ["temperature", "target_temperature", "flue_temperature"];

fn unit_of(key: &str) -> Option<Unit> {
    if CELSIUS.contains(&key) {
        Some(Unit::Celsius)
    } else if key == "eco_stop_delay" {
        Some(Unit::Minute)
    } else {
        None
    }
}

fn spec(key: &str, label: &str, kind: Kind, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: moli_i18n::tr(label).into(),
        kind,
        // Read only until orders are tried with a person watching.
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic,
    }
}

/// `Awaiting flame` → `awaiting_flame`, `MEM. ALARM` → `mem_alarm`.
#[must_use]
pub fn slug(word: &str) -> String {
    let mut out = String::new();
    for c in word.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_owned()
}

/// A formula with `#` as `raw`: numbers, `#`, `+ - * /`, brackets and
/// `int(…)` (towards zero). `None` for anything else, or a division by zero.
#[must_use]
pub fn calc(formula: &str, raw: f64) -> Option<f64> {
    let tokens: Vec<char> = formula.chars().filter(|c| !c.is_whitespace()).collect();
    let mut parser = Parser { tokens, at: 0, raw };
    let value = parser.sum()?;
    (parser.at == parser.tokens.len() && value.is_finite()).then_some(value)
}

struct Parser {
    tokens: Vec<char>,
    at: usize,
    raw: f64,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.tokens.get(self.at).copied()
    }

    fn sum(&mut self) -> Option<f64> {
        let mut value = self.product()?;
        while let Some(op @ ('+' | '-')) = self.peek() {
            self.at += 1;
            let rhs = self.product()?;
            value = if op == '+' { value + rhs } else { value - rhs };
        }
        Some(value)
    }

    fn product(&mut self) -> Option<f64> {
        let mut value = self.atom()?;
        while let Some(op @ ('*' | '/')) = self.peek() {
            self.at += 1;
            let rhs = self.atom()?;
            if op == '/' && rhs == 0.0 {
                return None;
            }
            value = if op == '*' { value * rhs } else { value / rhs };
        }
        Some(value)
    }

    fn atom(&mut self) -> Option<f64> {
        match self.peek()? {
            '#' => {
                self.at += 1;
                Some(self.raw)
            }
            '-' => {
                self.at += 1;
                Some(-self.atom()?)
            }
            '(' => {
                self.at += 1;
                let value = self.sum()?;
                (self.peek() == Some(')')).then(|| self.at += 1)?;
                Some(value)
            }
            'i' if self.tokens[self.at..].starts_with(&['i', 'n', 't', '(']) => {
                self.at += 3;
                Some(self.atom()?.trunc())
            }
            c if c.is_ascii_digit() || c == '.' => {
                let start = self.at;
                while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
                    self.at += 1;
                }
                self.tokens[start..self.at]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .ok()
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real Camelia Metal Air's map and one reading, stove off.
    fn camelia() -> (Map, Reading) {
        let text = include_str!("../../../integrations/micronova/fixtures/camelia-metal-air.json");
        let file: Json = serde_json::from_str(text).unwrap();
        let registers: Vec<Register> = serde_json::from_value(file["registers"].clone()).unwrap();
        let reading = file["reading"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.parse().unwrap(), v.as_i64().unwrap()))
            .collect();
        (Map::new(registers), reading)
    }

    #[test]
    fn formulas_are_arithmetic_only() {
        assert_eq!(calc("#", 19.0), Some(19.0));
        assert_eq!(calc("#/2", 36.0), Some(18.0));
        assert_eq!(calc("#+30", 0.0), Some(30.0));
        assert_eq!(calc("int(#/4) +1", 9.0), Some(3.0));
        assert_eq!(calc("(#-1)*4", 3.0), Some(8.0));
        assert_eq!(calc("(#/8)", 8.0), Some(1.0));
        assert_eq!(calc("#/0", 4.0), None, "no division by zero");
        assert_eq!(calc("__import__('os')", 1.0), None, "never code");
        assert_eq!(calc("#+", 1.0), None);
    }

    #[test]
    fn a_stove_reads_as_its_board_says() {
        let (map, reading) = camelia();
        let values: HashMap<String, Value> = map.values(&reading).into_iter().collect();
        assert_eq!(values["mode"], Value::Text("off".into()));
        assert_eq!(values["on"], Value::Bool(false));
        assert_eq!(values["temperature"], Value::Float(18.0));
        assert_eq!(values["target_temperature"], Value::Float(19.0));
        assert_eq!(values["flue_temperature"], Value::Float(30.0));
        assert_eq!(values["power_level"], Value::Float(3.0));
        assert_eq!(values["real_power"], Value::Float(1.0));
        assert_eq!(values["fan"], Value::Text("auto".into()));
        assert!(!values.contains_key("fan_front"), "the same fan, once");
        assert_eq!(values["alarm"], Value::Text("OK".into()));
        assert_eq!(values["eco_stop"], Value::Bool(true));
        assert_eq!(values["eco_stop_delay"], Value::Float(30.0));
        assert_eq!(values["clock"], Value::Text("2026-10-24T01:58".into()));
    }

    #[test]
    fn the_six_programs_read_as_the_stove_keeps_them() {
        let (map, reading) = camelia();
        let programs: Json = serde_json::from_str(&map.programs(&reading).unwrap()).unwrap();
        let programs = programs.as_array().unwrap();
        assert_eq!(programs.len(), 6);
        assert_eq!(
            programs[0],
            serde_json::json!({ "on": true, "start": 420, "stop": 1350, "days": [1, 2, 3, 4, 5] })
        );
        assert_eq!(
            programs[1],
            serde_json::json!({ "on": true, "start": 510, "stop": 1350, "days": [6, 7] })
        );
        assert_eq!(
            programs[2],
            serde_json::json!({ "on": false, "start": 720, "stop": 1310, "days": [6] })
        );
        assert_eq!(
            programs[5],
            serde_json::json!({ "on": false, "start": 0, "stop": 0, "days": [] })
        );
    }

    #[test]
    fn the_clock_is_in_binary_coded_decimal() {
        assert_eq!(bcd(0x24), Some(24));
        assert_eq!(bcd(0x58), Some(58));
        assert_eq!(bcd(0), Some(0));
        assert_eq!(bcd(0x1A), None, "not a digit");
        let (map, mut reading) = camelia();
        // A minute register that is no clock: no time rather than a wrong one.
        reading.insert(33018, 0x75);
        assert_eq!(map.clock(&reading), None);
    }

    #[test]
    fn a_stove_shows_its_states_and_reads_only() {
        let (map, _) = camelia();
        let points = map.points();
        let mode = points.iter().find(|p| &*p.key == "mode").unwrap();
        let Kind::Enum { values } = &mode.kind else {
            panic!("mode is an enum")
        };
        let states: Vec<&str> = values.iter().map(|v| &**v).collect();
        assert_eq!(
            states,
            [
                "off",
                "ignition",
                "awaiting_flame",
                "starting",
                "on",
                "cleaning",
                "shut_off",
                "stand_by",
                "alarm",
                "mem_alarm"
            ]
        );
        let target = points
            .iter()
            .find(|p| &*p.key == "target_temperature")
            .unwrap();
        assert_eq!(target.unit, Some(Unit::Celsius));
        assert_eq!(
            target.kind,
            Kind::Numeric {
                min: Some(5.0),
                max: Some(35.0),
                step: None
            }
        );
        let fan = points.iter().find(|p| &*p.key == "fan").unwrap();
        let Kind::Enum { values } = &fan.kind else {
            panic!("the fan is an enum")
        };
        let steps: Vec<&str> = values.iter().map(|v| &**v).collect();
        assert_eq!(steps, ["off", "1", "2", "3", "4", "5", "auto"]);
        let delay = points.iter().find(|p| &*p.key == "eco_stop_delay").unwrap();
        assert_eq!(delay.unit, Some(Unit::Minute));
        assert!(points.iter().any(|p| &*p.key == "programs"));
        assert!(points.iter().any(|p| &*p.key == "clock"));
        assert!(points.iter().all(|p| &*p.key != "fan_front"));
        assert!(points.iter().all(|p| !p.access.write), "read only for now");
    }

    #[test]
    fn words_become_slugs() {
        assert_eq!(slug("Awaiting flame"), "awaiting_flame");
        assert_eq!(slug("MEM. ALARM"), "mem_alarm");
        assert_eq!(slug("Stand-by"), "stand_by");
        assert_eq!(slug("Off"), "off");
    }
}
