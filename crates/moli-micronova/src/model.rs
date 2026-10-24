//! A Micronova board as its cloud describes it: a register map (each value's
//! offset, mask, formula and words) and a reading (offset → raw value). The
//! points a stove shows, and their values.
//!
//! A value is the raw one ANDed with its mask, then put through its formula,
//! where `#` stands for it. Formulas are plain arithmetic (`#/2`, `#+30`,
//! `int(#/4) +1`): evaluated here, never run.

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
        for (key, register, label, semantic) in NUMBERS {
            if let Some(r) = self.get(register) {
                let unit = CELSIUS.contains(key).then_some(Unit::Celsius);
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
        out
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
        for (key, register, _, _) in NUMBERS {
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
        out
    }
}

const STATE: &str = "status_get";
const ALARM: &str = "alarms_get";
const ECO_STOP: &str = "eco_stop_set";
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
        "fan_main",
        "vent_main_set",
        "pilotes.micronova.ventilateur",
        Semantic::Other,
    ),
    (
        "fan_front",
        "vent_front_set",
        "pilotes.micronova.ventilateur_avant",
        Semantic::Other,
    ),
];
const CELSIUS: [&str; 3] = ["temperature", "target_temperature", "flue_temperature"];

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
        assert_eq!(values["fan_main"], Value::Float(6.0));
        assert_eq!(values["alarm"], Value::Text("OK".into()));
        assert_eq!(values["eco_stop"], Value::Bool(true));
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
