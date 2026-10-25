//! A Micronova board as its cloud describes it: a register map (each value's
//! offset, mask, formula and words) and a reading (offset → raw value). The
//! points a stove shows, and their values.
//!
//! A value is the raw one ANDed with its mask, then put through its formula,
//! where `#` stands for it. Formulas are plain arithmetic (`#/2`, `#+30`,
//! `int(#/4) +1`): evaluated here, never run.
//!
//! Besides its numbers, a board keeps a clock (in binary-coded decimal,
//! its map's `is_hex`: 0x24 is the 24th; the weekday plainly, 1 = Monday)
//! and up to six programs, each switching the stove on then off at times
//! in steps of ten minutes, on the days ticked. Moli sets the clock right
//! (`Map::clock_writes`): it neither follows daylight saving nor keeps
//! time well, and the programs follow it.
//!
//! A command becomes register writes (`Map::writes`): the value through the
//! register's inverse formula, within its bounds, and only what changes.
//! On and off are a button (`status_managed_get`: ON or OFF written, read
//! back at rest): lit only from off, never while it shuts down or in
//! alarm; the point is a machine's (`Semantic::Control`), so an agent's
//! order waits for a person.

use std::collections::HashMap;
use std::sync::Arc;

use jiff::civil::DateTime;
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
    /// From a value to the raw one (`#*2` for `#/2`).
    #[serde(default)]
    pub formula_inverse: Option<String>,
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

/// One register to write: offset, mask, raw value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Write {
    pub offset: u32,
    pub mask: u32,
    pub raw: i64,
}

/// A program as the `programs` point says it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Program {
    on: bool,
    start: i64,
    stop: i64,
    days: Vec<usize>,
}

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
            // Fire: an agent asking gets a person's approval first.
            out.push(spec(
                "on",
                "pilotes.micronova.marche",
                Kind::Binary,
                None,
                Semantic::Control,
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
                // Whole numbers only: a register holds no fraction.
                let step = WRITABLE.contains(key).then_some(1.0);
                let kind = Kind::Numeric {
                    min: r.set_min,
                    max: r.set_max,
                    step,
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

    /// What setting point `key` to `value` asks of the board: the registers
    /// that change, in the order to write them (none: nothing changes), or
    /// why it cannot be done, said for a person.
    ///
    /// # Errors
    /// A point Moli does not set, or a value the stove does not take.
    pub fn writes(
        &self,
        key: &str,
        value: &Value,
        reading: &Reading,
    ) -> Result<Vec<Write>, String> {
        let number = || {
            let n = value
                .as_f64()
                .ok_or_else(|| moli_i18n::tr("pilotes.micronova.nombre_attendu"))?;
            if n.fract() != 0.0 {
                return Err(moli_i18n::tr("pilotes.micronova.nombre_entier"));
            }
            Ok(n)
        };
        let register = match key {
            "target_temperature" => TARGET,
            "power_level" => POWER,
            "eco_stop_delay" => ECO_DELAY,
            "eco_stop" => ECO_STOP,
            "fan" => FAN,
            PROGRAMS => return self.program_writes(value, reading),
            "on" => return self.switch_writes(value, reading),
            _ => {
                return Err(moli_i18n::tr!(
                    "pilotes.micronova.lecture_seule",
                    point = key
                ));
            }
        };
        let wanted = match key {
            "eco_stop" => match value {
                Value::Bool(on) => f64::from(u8::from(*on)),
                _ => return Err(moli_i18n::tr("pilotes.micronova.oui_non_attendu")),
            },
            "fan" => {
                let Value::Text(step) = value else {
                    return Err(moli_i18n::tr("pilotes.micronova.valeur_refusee"));
                };
                let r = self
                    .get(FAN)
                    .ok_or_else(|| moli_i18n::tr("pilotes.micronova.reglage_absent"))?;
                let at = self
                    .steps(FAN)
                    .iter()
                    .position(|s| s.as_str() == &**step)
                    .ok_or_else(|| moli_i18n::tr("pilotes.micronova.valeur_refusee"))?;
                #[allow(clippy::cast_precision_loss)]
                let wanted = r.set_min.unwrap_or(0.0) + at as f64;
                wanted
            }
            _ => number()?,
        };
        Ok(self.write(register, wanted, reading)?.into_iter().collect())
    }

    /// Writing `wanted` to a register: `None` when it holds it already.
    fn write(&self, key: &str, wanted: f64, reading: &Reading) -> Result<Option<Write>, String> {
        let r = self
            .get(key)
            .ok_or_else(|| moli_i18n::tr("pilotes.micronova.reglage_absent"))?;
        let below = r.set_min.is_some_and(|min| wanted < min);
        let above = r.set_max.is_some_and(|max| wanted > max);
        if below || above {
            return Err(moli_i18n::tr("pilotes.micronova.valeur_refusee"));
        }
        if self.value(key, reading) == Some(wanted) {
            return Ok(None);
        }
        let raw = calc(r.formula_inverse.as_deref().unwrap_or("#"), wanted)
            .filter(|raw| raw.fract() == 0.0 && *raw >= 0.0)
            .ok_or_else(|| moli_i18n::tr("pilotes.micronova.valeur_refusee"))?;
        #[allow(clippy::cast_possible_truncation)]
        let raw = raw as i64;
        if raw & !i64::from(r.mask) != 0 {
            return Err(moli_i18n::tr("pilotes.micronova.valeur_refusee"));
        }
        Ok(Some(Write {
            offset: r.offset,
            mask: r.mask,
            raw,
        }))
    }

    /// Lighting or putting out: the button's ON from off only, its OFF unless
    /// already off or going out; nothing in alarm but putting out.
    fn switch_writes(&self, value: &Value, reading: &Reading) -> Result<Vec<Write>, String> {
        let Value::Bool(on) = value else {
            return Err(moli_i18n::tr("pilotes.micronova.oui_non_attendu"));
        };
        let state = self.word(STATE, reading).map(|w| slug(&w));
        let state = state
            .as_deref()
            .ok_or_else(|| moli_i18n::tr("pilotes.micronova.pas_encore_lu"))?;
        let going_out = ["shut_off", "cleaning"].contains(&state);
        let wanted = match (on, state) {
            (true, "off") => "ON",
            (true, "alarm" | "mem_alarm") => {
                return Err(moli_i18n::tr("pilotes.micronova.en_alarme"));
            }
            (true, _) if going_out => return Err(moli_i18n::tr("pilotes.micronova.en_extinction")),
            // Already off, going out, or already on: nothing to send.
            (false, "off") | (true, _) => return Ok(Vec::new()),
            (false, _) if going_out => return Ok(Vec::new()),
            (false, _) => "OFF",
        };
        let r = self
            .get(SWITCH)
            .ok_or_else(|| moli_i18n::tr("pilotes.micronova.reglage_absent"))?;
        let raw = r
            .enc_val
            .iter()
            .find(|w| w.lang == "ENG" && w.description == wanted)
            .and_then(|w| w.value.as_f64())
            .ok_or_else(|| moli_i18n::tr("pilotes.micronova.reglage_absent"))?;
        // A button: it reads back at rest, so it is always written.
        #[allow(clippy::cast_possible_truncation)]
        Ok(vec![Write {
            offset: r.offset,
            mask: r.mask,
            raw: raw as i64,
        }])
    }

    /// The programs as a person set them: each changed register, a program
    /// switched off first, switched on last (never on with old hours).
    fn program_writes(&self, value: &Value, reading: &Reading) -> Result<Vec<Write>, String> {
        let Value::Text(text) = value else {
            return Err(moli_i18n::tr("pilotes.micronova.programmes_illisibles"));
        };
        let programs: Vec<Program> = serde_json::from_str(text)
            .map_err(|_| moli_i18n::tr("pilotes.micronova.programmes_illisibles"))?;
        let count = (1..=MAX_PROGRAMS)
            .take_while(|&n| self.get(&chrono(n, "enable_set")).is_some())
            .count();
        if programs.len() != count {
            return Err(moli_i18n::tr!(
                "pilotes.micronova.programmes_nombre",
                count = count
            ));
        }
        let mut out = Vec::new();
        for (n, p) in (1..=MAX_PROGRAMS).zip(&programs) {
            let ten = |m: i64| (m % 10 == 0 && (0..=1440).contains(&m)).then_some(m / 10);
            let (Some(start), Some(stop)) = (ten(p.start), ten(p.stop)) else {
                return Err(moli_i18n::tr("pilotes.micronova.programmes_heures"));
            };
            if p.on && start >= stop {
                return Err(moli_i18n::tr!("pilotes.micronova.programmes_ordre", n = n));
            }
            if p.days.iter().any(|d| !(1..=7).contains(d)) {
                return Err(moli_i18n::tr("pilotes.micronova.programmes_illisibles"));
            }
            #[allow(clippy::cast_precision_loss)]
            let mut parts = vec![
                (chrono(n, "start_set"), start as f64),
                (chrono(n, "stop_set"), stop as f64),
            ];
            for (i, day) in DAYS.iter().enumerate() {
                let ticked = p.days.contains(&(i + 1));
                parts.push((
                    chrono(n, &format!("day_{day}_set")),
                    f64::from(u8::from(ticked)),
                ));
            }
            let enable = (chrono(n, "enable_set"), f64::from(u8::from(p.on)));
            if p.on {
                parts.push(enable);
            } else {
                parts.insert(0, enable);
            }
            for (key, wanted) in parts {
                out.extend(self.write(&key, wanted, reading)?);
            }
        }
        Ok(out)
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

    /// The alarm, as the board names it (`OK`, `A05`), else its number.
    /// Some maps mask the register so that it hides some of its own codes
    /// (a Camelia Metal Air's `0x801e` hides A09, A03 and A05: 1, 32, 64;
    /// other MCZ maps say `0xffff` for the same words): it is read with
    /// their bits too, then with them alone if the rest names nothing.
    #[must_use]
    pub fn alarm(&self, reading: &Reading) -> Option<String> {
        let r = self.get(ALARM)?;
        let raw = *reading.get(&r.offset)?;
        let words = r
            .enc_val
            .iter()
            .filter(|w| w.lang == "ENG")
            .filter_map(|w| w.value.as_i64())
            .fold(0, |bits, v| bits | (v & 0xFFFF));
        let formula = r.formula.as_deref().unwrap_or("#");
        #[allow(clippy::cast_precision_loss)]
        let calc = |raw: i64| calc(formula, raw as f64);
        let word = |raw: i64| {
            let v = calc(raw)?;
            r.enc_val
                .iter()
                .find(|w| w.lang == "ENG" && w.value.as_f64() == Some(v))
                .map(|w| w.description.trim().to_owned())
        };
        let read = raw & (i64::from(r.mask) | words);
        word(read)
            .or_else(|| word(read & words))
            .or_else(|| calc(read).map(|v| format!("{v}")))
    }

    /// The stove's clock, as it keeps it: `2026-10-24T01:58` (its own time,
    /// the one its programs follow), or `None` if it makes no sense.
    #[must_use]
    pub fn clock(&self, reading: &Reading) -> Option<String> {
        let [year, month, day, hour, minute] = self.clock_parts(reading)?;
        let sane = (2000..2100).contains(&year)
            && (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && (0..24).contains(&hour)
            && (0..60).contains(&minute);
        sane.then(|| format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}"))
    }

    /// Year, month, day, hour, minute, as the registers say them.
    fn clock_parts(&self, reading: &Reading) -> Option<[i64; 5]> {
        let mut parts = [0i64; 5];
        for (part, key) in parts.iter_mut().zip(CLOCK_KEYS) {
            let raw = bcd(self.raw(key, reading)?)?;
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let value = calc(self.get(key)?.formula.as_deref().unwrap_or("#"), raw as f64)? as i64;
            *part = value;
        }
        Some(parts)
    }

    /// How many minutes the stove's clock is from `now` (the house's time),
    /// either way; a clock that makes no sense is as far as can be; `None`
    /// when the board has no clock or the reading does not carry it.
    #[must_use]
    pub fn clock_off(&self, now: DateTime, reading: &Reading) -> Option<i64> {
        if !CLOCK_KEYS.iter().all(|k| self.raw(k, reading).is_some()) {
            return None;
        }
        let stove = self.clock_parts(reading).and_then(|[y, mo, d, h, mi]| {
            let part = |v: i64| i8::try_from(v).ok();
            DateTime::new(
                i16::try_from(y).ok()?,
                part(mo)?,
                part(d)?,
                part(h)?,
                part(mi)?,
                0,
                0,
            )
            .ok()
        });
        let Some(stove) = stove else {
            return Some(i64::MAX);
        };
        let now = DateTime::new(
            now.year(),
            now.month(),
            now.day(),
            now.hour(),
            now.minute(),
            0,
            0,
        )
        .ok()?;
        Some((stove.duration_since(now).as_secs() / 60).abs())
    }

    /// Setting the stove's clock to `now` (the house's time, to the minute):
    /// the registers that are wrong, the hour and the minute first, then the
    /// date and the weekday.
    ///
    /// # Errors
    /// A board without a clock, or a time it does not take.
    pub fn clock_writes(&self, now: DateTime, reading: &Reading) -> Result<Vec<Write>, String> {
        let [hour, minute, day, month, year] = CLOCK_SET;
        let mut out = Vec::new();
        for (key, value) in [
            (hour, i64::from(now.hour())),
            (minute, i64::from(now.minute())),
            (day, i64::from(now.day())),
            (month, i64::from(now.month())),
            (year, i64::from(now.year())),
        ] {
            out.extend(self.clock_write(key, value, true, reading)?);
        }
        if self.get(WEEKDAY).is_some() {
            let weekday = i64::from(now.weekday().to_monday_one_offset());
            out.extend(self.clock_write(WEEKDAY, weekday, false, reading)?);
        }
        Ok(out)
    }

    /// One clock register: the value through its inverse formula, within
    /// its bounds (which hold before the formula: 26 for 2026), in
    /// binary-coded decimal if `bcd`; `None` when it holds it already.
    fn clock_write(
        &self,
        key: &str,
        value: i64,
        bcd: bool,
        reading: &Reading,
    ) -> Result<Option<Write>, String> {
        let refused = || moli_i18n::tr("pilotes.micronova.valeur_refusee");
        let r = self
            .get(key)
            .ok_or_else(|| moli_i18n::tr("pilotes.micronova.reglage_absent"))?;
        #[allow(clippy::cast_precision_loss)]
        let v = calc(r.formula_inverse.as_deref().unwrap_or("#"), value as f64)
            .filter(|v| v.fract() == 0.0)
            .ok_or_else(refused)?;
        if r.set_min.is_some_and(|min| v < min) || r.set_max.is_some_and(|max| v > max) {
            return Err(refused());
        }
        #[allow(clippy::cast_possible_truncation)]
        let v = v as i64;
        let raw = if bcd {
            to_bcd(v).ok_or_else(refused)?
        } else {
            v
        };
        if raw & !i64::from(r.mask) != 0 {
            return Err(refused());
        }
        if self.raw(key, reading) == Some(raw) {
            return Ok(None);
        }
        Ok(Some(Write {
            offset: r.offset,
            mask: r.mask,
            raw,
        }))
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
        if let Some(alarm) = self.alarm(reading) {
            out.push(("alarm".into(), Value::Text(alarm.into())));
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

/// 24 → `0x24`; `None` below zero.
fn to_bcd(value: i64) -> Option<i64> {
    if value < 0 {
        return None;
    }
    let (mut raw, mut shift, mut rest) = (0, 0, value);
    loop {
        raw |= (rest % 10) << shift;
        rest /= 10;
        shift += 4;
        if rest == 0 {
            return Some(raw);
        }
    }
}

/// A program's register: `chrono_p1_enable_set`, `chrono_p3_day_monday_set`.
fn chrono(n: u8, part: &str) -> String {
    format!("chrono_p{n}_{part}")
}

const STATE: &str = "status_get";
const ALARM: &str = "alarms_get";
const ECO_STOP: &str = "eco_stop_set";
const FAN: &str = "vent_main_set";
const TARGET: &str = "temp_air_set";
/// The stove's on and off button.
const SWITCH: &str = "status_managed_get";
const POWER: &str = "power_set";
const ECO_DELAY: &str = "eco_temp_stop";
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
/// The same, in the order they are set: hour, minute, day, month, year.
const CLOCK_SET: [&str; 5] = [
    "clock_hour_set",
    "clock_minute_set",
    "calendar_day_set",
    "calendar_month_set",
    "calendar_year_set",
];
/// The weekday the programs follow, 1 = Monday (not in decimal-coded binary).
const WEEKDAY: &str = "calendar_day_week_set";
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
/// What Moli may set: on and off (a person deciding for an agent), the
/// settings; never the state.
const WRITABLE: [&str; 7] = [
    "on",
    "target_temperature",
    "power_level",
    "fan",
    "eco_stop",
    "eco_stop_delay",
    PROGRAMS,
];

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
        access: Access {
            read: true,
            write: WRITABLE.contains(&key),
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
    fn an_alarm_says_its_code_even_where_the_map_masks_it() {
        // The map's mask (0x801e) hides 1, 32 and 64, its own A09, A03, A05.
        let (map, mut reading) = camelia();
        let alarm = map.get(ALARM).unwrap().offset;
        for (raw, code) in [
            (0, "OK"),
            (1, "A09"),
            (2, "A04"),
            (4, "A08"),
            (8, "A01"),
            (16, "A02"),
            (32, "A03"),
            (64, "A05"),
            // Its high bit set too (the mask keeps it): the code still.
            (0x8000 | 64, "A05"),
        ] {
            reading.insert(alarm, raw);
            assert_eq!(map.alarm(&reading).as_deref(), Some(code), "raw {raw}");
        }
        // Two at once: no word, the number.
        reading.insert(alarm, 3);
        assert_eq!(map.alarm(&reading).as_deref(), Some("3"));
        reading.remove(&alarm);
        assert_eq!(map.alarm(&reading), None, "not read: nothing said");
    }

    fn w(offset: u32, mask: u32, raw: i64) -> Write {
        Write { offset, mask, raw }
    }

    #[test]
    fn a_setting_becomes_its_register() {
        let (map, reading) = camelia();
        let set = |key: &str, value: Value| map.writes(key, &value, &reading);
        assert_eq!(
            set("target_temperature", Value::Float(18.0)),
            Ok(vec![w(32893, 0xff, 18)])
        );
        assert_eq!(
            set("target_temperature", Value::Float(19.0)),
            Ok(vec![]),
            "already so"
        );
        assert!(
            set("target_temperature", Value::Float(18.5)).is_err(),
            "whole degrees"
        );
        assert!(
            set("target_temperature", Value::Float(40.0)).is_err(),
            "within its bounds"
        );
        assert_eq!(
            set("power_level", Value::Float(2.0)),
            Ok(vec![w(32895, 0xff, 2)])
        );
        assert_eq!(
            set("eco_stop_delay", Value::Float(20.0)),
            Ok(vec![w(32802, 0xff, 20)])
        );
        assert_eq!(
            set("eco_stop", Value::Bool(false)),
            Ok(vec![w(32798, 0xffff, 0)])
        );
        assert_eq!(
            set("fan", Value::Text("3".into())),
            Ok(vec![w(32894, 0xff, 3)])
        );
        assert_eq!(
            set("fan", Value::Text("off".into())),
            Ok(vec![w(32894, 0xff, 0)])
        );
        assert_eq!(set("fan", Value::Text("auto".into())), Ok(vec![]));
        assert!(set("fan", Value::Text("turbo".into())).is_err());
        assert!(
            set("mode", Value::Text("off".into())).is_err(),
            "never the state"
        );
    }

    #[test]
    fn the_stove_lights_from_off_only_and_its_switch_is_a_machine() {
        let (map, mut reading) = camelia();
        let on = |reading: &Reading, v: bool| map.writes("on", &Value::Bool(v), reading);
        // Off (the fixture): lit by the button's ON; already off, nothing.
        assert_eq!(on(&reading, true), Ok(vec![w(232, 0xffff, 85)]));
        assert_eq!(on(&reading, false), Ok(vec![]));
        // On, or on without a flame (eco-stop): put out by OFF.
        for state in [4, 7] {
            reading.insert(33, state);
            assert_eq!(on(&reading, false), Ok(vec![w(232, 0xffff, 170)]));
            assert_eq!(on(&reading, true), Ok(vec![]), "already on");
        }
        // Going out: never lit again before it is off.
        for state in [5, 6] {
            reading.insert(33, state);
            assert!(on(&reading, true).is_err());
            assert_eq!(on(&reading, false), Ok(vec![]));
        }
        // In alarm: put out, never lit.
        reading.insert(33, 8);
        assert!(on(&reading, true).is_err());
        assert_eq!(on(&reading, false), Ok(vec![w(232, 0xffff, 170)]));
        let point = map.points().into_iter().find(|p| &*p.key == "on").unwrap();
        assert_eq!(
            point.semantic,
            Semantic::Control,
            "an agent waits for a person"
        );
    }

    /// The fixture's programs with `change` applied to them.
    fn programs_with(map: &Map, reading: &Reading, change: impl Fn(&mut Vec<Json>)) -> Value {
        let mut list: Vec<Json> = serde_json::from_str(&map.programs(reading).unwrap()).unwrap();
        change(&mut list);
        Value::Text(Json::Array(list).to_string().into())
    }

    #[test]
    fn programs_write_only_what_changed_in_a_safe_order() {
        let (map, reading) = camelia();
        let set = |value: Value| map.writes("programs", &value, &reading);
        let same = programs_with(&map, &reading, |_| {});
        assert_eq!(set(same), Ok(vec![]));
        // P1 ends at 23:00 instead of 22:30.
        let later = programs_with(&map, &reading, |l| l[0]["stop"] = 1380.into());
        assert_eq!(set(later), Ok(vec![w(32827, 0xff, 138)]));
        // P3 on, from 13:00, and on Sunday too: its hours and days first.
        let p3 = programs_with(&map, &reading, |l| {
            l[2]["on"] = true.into();
            l[2]["start"] = 780.into();
            l[2]["days"] = serde_json::json!([6, 7]);
        });
        assert_eq!(
            set(p3),
            Ok(vec![w(32846, 0xff, 78), w(32854, 0x1, 1), w(32845, 0x1, 1)])
        );
        // P1 off and its start moved: off first.
        let p1 = programs_with(&map, &reading, |l| {
            l[0]["on"] = false.into();
            l[0]["start"] = 360.into();
        });
        assert_eq!(set(p1), Ok(vec![w(32825, 0x1, 0), w(32826, 0xff, 36)]));
    }

    #[test]
    fn programs_the_stove_cannot_keep_are_refused() {
        let (map, reading) = camelia();
        let set = |value: Value| map.writes("programs", &value, &reading);
        assert!(
            set(programs_with(&map, &reading, |l| {
                l.pop();
            }))
            .is_err(),
            "six"
        );
        assert!(
            set(programs_with(&map, &reading, |l| l[0]["start"] = 425.into())).is_err(),
            "10 min"
        );
        assert!(
            set(programs_with(&map, &reading, |l| l[0]["stop"] = 1450.into())).is_err(),
            "24:00"
        );
        assert!(
            set(programs_with(&map, &reading, |l| l[0]["stop"] = 420.into())).is_err(),
            "ends after"
        );
        assert!(
            set(programs_with(&map, &reading, |l| l[0]["days"] = serde_json::json!([8]))).is_err()
        );
        assert!(set(Value::Text("not json".into())).is_err());
    }

    /// The house's time, to the minute.
    fn at(y: i16, mo: i8, d: i8, h: i8, mi: i8) -> DateTime {
        DateTime::new(y, mo, d, h, mi, 0, 0).unwrap()
    }

    #[test]
    fn the_clock_s_drift_is_measured_in_minutes() {
        // The fixture's stove says Saturday 24 October 2026, 01:58.
        let (map, reading) = camelia();
        assert_eq!(map.clock_off(at(2026, 10, 24, 1, 58), &reading), Some(0));
        assert_eq!(map.clock_off(at(2026, 10, 24, 1, 59), &reading), Some(1));
        assert_eq!(map.clock_off(at(2026, 10, 24, 1, 55), &reading), Some(3));
        // An hour behind across midnight: daylight saving, the night before.
        assert_eq!(map.clock_off(at(2026, 10, 24, 0, 58), &reading), Some(60));
        let seconds = DateTime::new(2026, 10, 24, 1, 58, 59, 0).unwrap();
        assert_eq!(
            map.clock_off(seconds, &reading),
            Some(0),
            "the stove counts minutes"
        );
        let mut nonsense = reading.clone();
        nonsense.insert(map.get("calendar_month_set").unwrap().offset, 0x13);
        assert_eq!(
            map.clock_off(at(2026, 10, 24, 1, 58), &nonsense),
            Some(i64::MAX)
        );
        assert_eq!(
            Map::default().clock_off(at(2026, 10, 24, 1, 58), &reading),
            None
        );
    }

    #[test]
    fn the_clock_is_set_in_binary_coded_decimal_the_wrong_registers_only() {
        let (map, reading) = camelia();
        let offset = |key: &str| map.get(key).unwrap().offset;
        // Right already: nothing to write.
        assert_eq!(
            map.clock_writes(at(2026, 10, 24, 1, 58), &reading),
            Ok(vec![])
        );
        // The minute only.
        let writes = map.clock_writes(at(2026, 10, 24, 1, 36), &reading).unwrap();
        assert_eq!(
            writes,
            vec![Write {
                offset: offset("clock_minute_set"),
                mask: 0xFFFF,
                raw: 0x36
            }]
        );
        // Hour and minute first, then day, month, year and weekday:
        // Thursday 31 December 2026, 23:45.
        let writes = map
            .clock_writes(at(2026, 12, 31, 23, 45), &reading)
            .unwrap();
        let got: Vec<(u32, i64)> = writes.iter().map(|w| (w.offset, w.raw)).collect();
        assert_eq!(
            got,
            vec![
                (offset("clock_hour_set"), 0x23),
                (offset("clock_minute_set"), 0x45),
                (offset("calendar_day_set"), 0x31),
                (offset("calendar_month_set"), 0x12),
                (offset("calendar_day_week_set"), 4),
            ],
            "the year holds 2026 already (0x26, its bounds before +2000)"
        );
        let year = map.clock_writes(at(2027, 10, 24, 1, 58), &reading).unwrap();
        assert_eq!(year[0].raw, 0x27);
        assert_eq!(
            map.clock_writes(at(2051, 10, 24, 1, 58), &reading)
                .map(|_| ()),
            Err(moli_i18n::tr("pilotes.micronova.valeur_refusee")),
            "beyond its bounds"
        );
    }

    #[test]
    fn decimal_coded_binary_both_ways() {
        assert_eq!(to_bcd(0), Some(0));
        assert_eq!(to_bcd(9), Some(9));
        assert_eq!(to_bcd(24), Some(0x24));
        assert_eq!(to_bcd(59), Some(0x59));
        assert_eq!(to_bcd(-1), None);
        for v in 0..100 {
            assert_eq!(bcd(to_bcd(v).unwrap()), Some(v));
        }
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
    fn a_stove_shows_its_states_and_what_moli_sets() {
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
                step: Some(1.0)
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
        let writable: Vec<&str> = points
            .iter()
            .filter(|p| p.access.write)
            .map(|p| &*p.key)
            .collect();
        assert_eq!(
            writable,
            [
                "on",
                "target_temperature",
                "power_level",
                "eco_stop_delay",
                "eco_stop",
                "fan",
                "programs"
            ],
            "on, off and the settings, never the state"
        );
    }

    #[test]
    fn words_become_slugs() {
        assert_eq!(slug("Awaiting flame"), "awaiting_flame");
        assert_eq!(slug("MEM. ALARM"), "mem_alarm");
        assert_eq!(slug("Stand-by"), "stand_by");
        assert_eq!(slug("Off"), "off");
    }
}
