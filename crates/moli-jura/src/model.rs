//! A Jura machine as its Wi-Fi Connect dongle tells it: the status bits it
//! pushes every two seconds (`@TF:`), what it is doing while busy (`@TV:`),
//! its maintenance counters and percentages (`@TG:43`, `@TG:C0`), a counter
//! per drink (`@TR:32`, sixteen pages of four), a recipe per drink
//! (`@TM:41,<code>`) and its settings (`@TM:<argument>`). The points it
//! shows, their values, and what an order becomes on the wire.
//!
//! Codes and bits are the ones the J.O.E. app uses; most are shared by the
//! whole family, drink names and settings are not: a model known here
//! (`model = "EF1092"`, the E8 EC) gets its names and settings, any other
//! the shared bits, its drinks under their codes, and no settings.
//!
//! A drink is brewed with the recipe the machine keeps for it (strength,
//! water, milk foam, temperature as set at its display or in J.O.E.): that
//! recipe, read back, is the brew command's own layout, byte 8 set to 1.

use std::collections::BTreeMap;
use std::sync::Arc;

use moli_core::{Access, Kind, PointSpec, Semantic, Unit, Value};

use crate::wire::{checksum, hex};

/// One drink: its code, its stable name (the value of the `brew` point and
/// the end of its counter's key), whether it needs milk, and for a double
/// the single drink whose recipe it brews twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drink {
    pub code: u8,
    pub slug: &'static str,
    pub milk: bool,
    pub single: Option<u8>,
    /// Counted, never brewed from here (ground coffee needs a hand).
    pub counted_only: bool,
}

const fn drink(code: u8, slug: &'static str, milk: bool) -> Drink {
    Drink {
        code,
        slug,
        milk,
        single: None,
        counted_only: false,
    }
}

/// The E8 (EC), J.O.E. group EF1092: its fifteen drinks.
const E8_EC: &[Drink] = &[
    drink(0x02, "espresso", false),
    drink(0x03, "coffee", false),
    drink(0x04, "cappuccino", true),
    drink(0x06, "espresso_macchiato", true),
    drink(0x07, "latte_macchiato", true),
    drink(0x08, "milk_foam", true),
    drink(0x0D, "hot_water", false),
    drink(0x28, "cafe_barista", false),
    drink(0x29, "barista_lungo", false),
    drink(0x2B, "cortado", true),
    drink(0x2E, "flat_white", true),
    drink(0x30, "espresso_doppio", false),
    Drink {
        single: Some(0x02),
        ..drink(0x31, "two_espressi", false)
    },
    Drink {
        single: Some(0x03),
        ..drink(0x36, "two_coffees", false)
    },
    Drink {
        counted_only: true,
        ..drink(0x0F, "ground_coffee", false)
    },
];

/// Codes the whole family shares, for a model not described here.
const SHARED: &[Drink] = &[
    drink(0x01, "ristretto", false),
    drink(0x02, "espresso", false),
    drink(0x03, "coffee", false),
    drink(0x04, "cappuccino", true),
    drink(0x05, "milk_coffee", true),
    drink(0x06, "espresso_macchiato", true),
    drink(0x07, "latte_macchiato", true),
    drink(0x08, "milk_foam", true),
    drink(0x0A, "milk_portion", true),
    drink(0x0D, "hot_water", false),
    Drink {
        counted_only: true,
        ..drink(0x0F, "ground_coffee", false)
    },
    drink(0x30, "espresso_doppio", false),
];

/// A maintenance program: its name and the J.O.E. command that starts it
/// (`@TG:<code>`; the machine leads the rest on its display).
pub const PROGRAMS: &[(&str, u8)] = &[
    ("cleaning", 0x24),
    ("descaling", 0x25),
    ("filter_change", 0x26),
    ("milk_rinse", 0x23),
    ("milk_cleaning", 0x21),
    ("coffee_rinse", 0x22),
];

/// Status bits worth a point of their own (bit, key).
const ALERTS: &[(usize, &str)] = &[
    (1, "water_empty"),
    (2, "grounds_full"),
    (3, "drip_tray_full"),
    (0, "drip_tray_missing"),
    (4, "grounds_container_missing"),
    (10, "beans_empty"),
    (34, "cleaning_due"),
    (33, "descaling_due"),
    (32, "filter_due"),
    (35, "milk_rinse_due"),
    (41, "milk_cleaning_due"),
    (37, "filter_fitted"),
    (30, "machine_error"),
];

/// Bits that keep the machine from brewing (J.O.E.'s `Type="block"`).
const BLOCKING: [usize; 12] = [0, 1, 2, 3, 4, 8, 17, 29, 30, 40, 47, 48];
/// Bits that keep it from a milk drink (`Blocked="M"`).
const NO_MILK: [usize; 3] = [14, 15, 16];
const HEATING: usize = 12;
const RINSING: [usize; 3] = [9, 18, 22];
const ENERGY_SAVING: usize = 36;
const PROGRAMMING: usize = 29;
const SWITCHING_OFF: usize = 47;

/// The machine's state, as `status` shows it.
pub const STATES: [&str; 10] = [
    "ready",
    "energy_saving",
    "heating_up",
    "rinsing",
    "brewing",
    "maintenance",
    "programming",
    "busy",
    "switching_off",
    "attention",
];

/// Maintenance counters in the order `@TG:43` gives them (the E8's and
/// most machines'), then the percentages of `@TG:C0`.
const COUNTERS: [&str; 6] = [
    "cleanings",
    "filter_changes",
    "descalings",
    "milk_rinses",
    "coffee_rinses",
    "milk_cleanings",
];
const PERCENTS: [&str; 3] = ["cleaning_need", "filter_wear", "descaling_need"];

/// A setting: its key, its `@TM:` argument, and its values (name, hex) or
/// a range for a plain number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting {
    pub key: &'static str,
    pub argument: &'static str,
    pub values: &'static [(&'static str, &'static str)],
    pub range: Option<(u8, u8)>,
    pub unit: Option<&'static str>,
}

const E8_EC_SETTINGS: &[Setting] = &[
    Setting {
        key: "water_hardness",
        argument: "02",
        values: &[],
        range: Some((1, 30)),
        unit: Some("°dH"),
    },
    Setting {
        key: "auto_off",
        argument: "13",
        values: &[
            ("15min", "0F"),
            ("30min", "1E"),
            ("1h", "213C"),
            ("2h", "2178"),
            ("3h", "21B4"),
            ("4h", "21F0"),
            ("5h", "22012C"),
            ("6h", "220168"),
            ("7h", "2201A4"),
            ("8h", "2201E0"),
            ("9h", "22021C"),
        ],
        range: None,
        unit: None,
    },
    Setting {
        key: "units",
        argument: "08",
        values: &[("ml", "00"), ("oz", "01")],
        range: None,
        unit: None,
    },
    Setting {
        key: "language",
        argument: "09",
        values: &[
            ("german", "01"),
            ("english", "02"),
            ("french", "03"),
            ("italian", "04"),
            ("dutch", "05"),
            ("spanish", "06"),
            ("portuguese", "07"),
            ("russian", "08"),
            ("swedish", "09"),
            ("polish", "0A"),
            ("estonian", "0B"),
        ],
        range: None,
        unit: None,
    },
    Setting {
        key: "milk_rinsing",
        argument: "04",
        values: &[("automatic", "00"), ("manual", "01")],
        range: None,
        unit: None,
    },
];

/// What is known of one machine's model.
#[derive(Clone, Debug)]
pub struct Model {
    /// The J.O.E. group (`EF1092`), when known.
    pub group: Option<String>,
    pub drinks: Vec<Drink>,
    pub settings: &'static [Setting],
}

impl Model {
    #[must_use]
    pub fn new(group: Option<&str>) -> Self {
        let group = group.map(|g| g.trim().to_ascii_uppercase());
        let (drinks, settings) = match group.as_deref() {
            Some("EF1092") => (E8_EC, E8_EC_SETTINGS),
            _ => (SHARED, &[] as &[Setting]),
        };
        Self {
            group,
            drinks: drinks.to_vec(),
            settings,
        }
    }

    /// Milliseconds to leave between two sessions: the dongle refuses one
    /// opened right after the last. A real E8 (EC) took one again after
    /// 1 s (slowly) and 2 s (at once); other models wanted 3 s, a Z10 20 s.
    #[must_use]
    pub fn gap_ms(&self) -> u64 {
        match self.group.as_deref() {
            Some("EF1092") => 1500,
            _ => 3000,
        }
    }

    /// The commercial name, for the device.
    #[must_use]
    pub fn name(&self) -> Option<&'static str> {
        match self.group.as_deref() {
            Some("EF1092") => Some("E8 (EC)"),
            _ => None,
        }
    }

    /// Once the counters are read: a drink of an unknown model the machine
    /// counts is added under its code (`drink_28`); the known ones stay.
    pub fn learn(&mut self, counted: &BTreeMap<u8, u32>) -> bool {
        let mut changed = false;
        for &code in counted.keys() {
            if code != 0 && !self.drinks.iter().any(|d| d.code == code) {
                // Once per code, at most 63: a name that lives as long as Moli.
                let slug: &'static str = Box::leak(format!("drink_{code:02x}").into_boxed_str());
                self.drinks.push(drink(code, slug, false));
                changed = true;
            }
        }
        changed
    }

    fn drink(&self, slug: &str) -> Option<&Drink> {
        self.drinks.iter().find(|d| d.slug == slug)
    }

    fn drink_by_code(&self, code: u8) -> Option<&Drink> {
        self.drinks.iter().find(|d| d.code == code)
    }

    fn brewable(&self) -> impl Iterator<Item = &Drink> {
        self.drinks.iter().filter(|d| !d.counted_only)
    }

    /// Every point of the machine.
    #[must_use]
    pub fn points(&self) -> Vec<PointSpec> {
        let mut p = vec![
            spec(
                "status",
                &tr("etat"),
                enumeration(STATES.iter().copied()),
                None,
                Semantic::Other,
            ),
            spec(
                "activity",
                &tr("activite"),
                Kind::Text,
                None,
                Semantic::Other,
            ),
            spec(
                "progress",
                &tr("avancement"),
                percent(),
                Some(Unit::Percent),
                Semantic::Other,
            ),
        ];
        for (_, key) in ALERTS {
            p.push(spec(key, &tr(key), Kind::Binary, None, Semantic::Other));
        }
        self.counting(&mut p);
        for s in self.settings {
            let kind = match s.range {
                Some((min, max)) => Kind::Numeric {
                    min: Some(f64::from(min)),
                    max: Some(f64::from(max)),
                    step: Some(1.0),
                },
                None => enumeration(s.values.iter().map(|(name, _)| *name)),
            };
            let unit = s.unit.and_then(Unit::parse);
            p.push(writable(spec(
                s.key,
                &tr(s.key),
                kind,
                unit,
                Semantic::Config,
            )));
        }
        let drinks = enumeration(self.brewable().map(|d| d.slug));
        let programs = enumeration(PROGRAMS.iter().map(|(name, _)| *name));
        p.extend([
            writable(spec(
                "brew",
                &tr("preparer"),
                drinks,
                None,
                Semantic::Control,
            )),
            writable(spec(
                "maintenance",
                &tr("entretien"),
                programs,
                None,
                Semantic::Control,
            )),
        ]);
        p
    }

    /// The cockpit: maintenance needs and counters, drinks counted.
    fn counting(&self, p: &mut Vec<PointSpec>) {
        for key in PERCENTS {
            p.push(spec(
                key,
                &tr(key),
                percent(),
                Some(Unit::Percent),
                Semantic::Other,
            ));
        }
        for key in COUNTERS {
            p.push(spec(key, &tr(key), counter(), None, Semantic::Other));
        }
        p.push(spec(
            "brews_total",
            &tr("boissons_total"),
            counter(),
            None,
            Semantic::Other,
        ));
        for d in &self.drinks {
            let label = moli_i18n::tr!("pilotes.jura.compteur", drink = drink_label(d));
            let key = format!("brews.{}", d.slug);
            p.push(spec(&key, &label, counter(), None, Semantic::Other));
        }
    }
}

fn writable(mut point: PointSpec) -> PointSpec {
    point.access.write = true;
    point
}

fn tr(key: &str) -> String {
    moli_i18n::tr(&format!("pilotes.jura.{key}"))
}

/// A drink's name for people: translated when known, its code otherwise.
fn drink_label(d: &Drink) -> String {
    if d.slug.starts_with("drink_") {
        moli_i18n::tr!(
            "pilotes.jura.boisson_code",
            code = format!("{:02X}", d.code)
        )
    } else {
        tr(&format!("boisson.{}", d.slug))
    }
}

fn spec(key: &str, label: &str, kind: Kind, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic,
    }
}

fn enumeration<'a>(values: impl Iterator<Item = &'a str>) -> Kind {
    Kind::Enum {
        values: values.map(Arc::from).collect(),
    }
}

fn percent() -> Kind {
    Kind::Numeric {
        min: Some(0.0),
        max: Some(100.0),
        step: None,
    }
}

fn counter() -> Kind {
    Kind::Numeric {
        min: Some(0.0),
        max: None,
        step: Some(1.0),
    }
}

// ------------------------------------------------------------------ reading

/// What the machine says it is: idle with its status bits, or busy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// `@TF:` — every bit, in the app's order (most significant first).
    Idle(Vec<bool>),
    /// `@TV:` — what it does (state, drink or program code, percent).
    Busy {
        state: u8,
        code: u8,
        percent: Option<u8>,
    },
}

impl Status {
    /// A pushed frame's status, if it is one.
    #[must_use]
    pub fn parse(frame: &str) -> Option<Self> {
        if let Some(hex) = frame.strip_prefix("@TF:") {
            let bytes = unhex(hex)?;
            let bits = (0..bytes.len() * 8)
                .map(|n| bytes[n / 8] & (1 << (7 - n % 8)) != 0)
                .collect();
            return Some(Self::Idle(bits));
        }
        let hex = frame.strip_prefix("@TV:")?;
        // `@TV:81,…` and the like are a language download or a timer's clock.
        if hex.contains(',') {
            return None;
        }
        let bytes = unhex(hex)?;
        let (&state, rest) = bytes.split_first()?;
        if matches!(state, 0x81 | 0x82 | 0x84) {
            return None;
        }
        let code = rest.first().copied().unwrap_or(0);
        // The value window starts at byte 2, or 3 after an `8F` marker;
        // the app reads the percent from its slot 12.
        let window = if bytes.get(2) == Some(&0x8F) { 3 } else { 2 };
        let percent = match state {
            0x3E => Some(100),
            _ => bytes.get(window + 12).copied().filter(|p| *p <= 100),
        };
        Some(Self::Busy {
            state,
            code,
            percent,
        })
    }

    fn bit(&self, n: usize) -> bool {
        matches!(self, Self::Idle(bits) if bits.get(n).copied().unwrap_or(false))
    }

    /// The `status` point's value.
    #[must_use]
    pub fn state(&self, model: &Model) -> &'static str {
        match self {
            Self::Busy { state: 0xFF, .. } => "programming",
            Self::Busy { state, code, .. } => {
                if *state != 0xFE && model.drink_by_code(*code).is_some() {
                    "brewing"
                } else if PROGRAMS.iter().any(|(_, c)| c == code) {
                    "maintenance"
                } else {
                    "busy"
                }
            }
            Self::Idle(_) if self.bit(PROGRAMMING) => "programming",
            Self::Idle(_) if self.bit(SWITCHING_OFF) => "switching_off",
            Self::Idle(_) if self.blocked() => "attention",
            Self::Idle(_) if self.bit(HEATING) => "heating_up",
            Self::Idle(_) if RINSING.iter().any(|&b| self.bit(b)) => "rinsing",
            Self::Idle(_) if self.bit(ENERGY_SAVING) => "energy_saving",
            Self::Idle(_) => "ready",
        }
    }

    /// A bit that keeps the machine from brewing is set.
    #[must_use]
    pub fn blocked(&self) -> bool {
        BLOCKING.iter().any(|&b| self.bit(b))
    }

    /// The values it gives: state, activity, progress, and the alerts while
    /// idle (a busy machine does not say them; they keep their last value).
    #[must_use]
    pub fn values(&self, model: &Model) -> Vec<(String, Value)> {
        let mut out = vec![("status".to_owned(), Value::from(self.state(model)))];
        match self {
            Self::Idle(_) => {
                out.push(("activity".into(), Value::from("")));
                out.push(("progress".into(), Value::Int(0)));
                for (bit, key) in ALERTS {
                    out.push(((*key).into(), Value::Bool(self.bit(*bit))));
                }
            }
            Self::Busy { code, percent, .. } => {
                let doing = model
                    .drink_by_code(*code)
                    .map(|d| d.slug)
                    .or_else(|| PROGRAMS.iter().find(|(_, c)| c == code).map(|(n, _)| *n))
                    .unwrap_or("");
                out.push(("activity".into(), Value::from(doing)));
                if let Some(p) = percent {
                    out.push(("progress".into(), Value::Int(i64::from(*p))));
                }
            }
        }
        out
    }
}

fn unhex(hex: &str) -> Option<Vec<u8>> {
    let hex = hex.trim();
    if !hex.len().is_multiple_of(2) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// `@tg:43…`: the maintenance counters.
#[must_use]
pub fn counters(reply: &str) -> Vec<(String, Value)> {
    let Some(bytes) = reply.strip_prefix("@tg:43").and_then(unhex) else {
        return Vec::new();
    };
    COUNTERS
        .iter()
        .zip(bytes.as_chunks::<2>().0)
        .map(|(key, w)| {
            (
                (*key).to_owned(),
                Value::Int(i64::from(u16::from_be_bytes(*w))),
            )
        })
        .collect()
}

/// `@tg:C0…`: the maintenance percentages (255: not for this machine now,
/// such as descaling while a filter is fitted; left unsaid).
#[must_use]
pub fn percents(reply: &str) -> Vec<(String, Value)> {
    let Some(bytes) = reply.strip_prefix("@tg:C0").and_then(unhex) else {
        return Vec::new();
    };
    PERCENTS
        .iter()
        .zip(bytes)
        .filter(|(_, v)| *v <= 100)
        .map(|(key, v)| ((*key).to_owned(), Value::Int(i64::from(v))))
        .collect()
}

/// `@tr:32,<page>,<4 × u16>`: the drinks counted on this page (code →
/// count; `FFFF` = no such drink here).
#[must_use]
pub fn counted(reply: &str, page: u8) -> BTreeMap<u8, u32> {
    let mut out = BTreeMap::new();
    let mut parts = reply.split(',');
    if parts.next() != Some("@tr:32") || parts.next() != Some(&format!("{page:02X}")) {
        return out;
    }
    let Some(bytes) = parts.next().and_then(unhex) else {
        return out;
    };
    for (i, w) in bytes.as_chunks::<2>().0.iter().take(4).enumerate() {
        let n = u16::from_be_bytes(*w);
        if n != 0xFFFF {
            #[allow(clippy::cast_possible_truncation)]
            out.insert(page * 4 + i as u8, u32::from(n));
        }
    }
    out
}

/// The counters' values: the total (code 0), then each drink's.
#[must_use]
pub fn brews(model: &Model, counted: &BTreeMap<u8, u32>) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for (code, n) in counted {
        let key = match code {
            0 => "brews_total".to_owned(),
            _ => match model.drink_by_code(*code) {
                Some(d) => format!("brews.{}", d.slug),
                None => continue,
            },
        };
        out.push((key, Value::Int(i64::from(*n))));
    }
    out
}

/// A reply that carries a checksum (`@tm:<head>,<payload><sum>`): the
/// payload, if the sum is right.
fn checked(reply: &str, head: &str) -> Option<String> {
    let body = reply
        .strip_prefix("@tm:")?
        .strip_prefix(head)?
        .strip_prefix(',')?;
    if body.len() < 4 || !body.len().is_multiple_of(2) {
        return None;
    }
    let (payload, sum) = body.split_at(body.len() - 2);
    (checksum(&format!("{head},{payload}")) == sum.to_ascii_uppercase())
        .then(|| payload.to_ascii_uppercase())
}

/// `@tm:41,<recipe><sum>` for drink `code`: the recipe's bytes, F1 (the
/// code) first. `None`: refused (`@tm:C1`), damaged, or another drink's.
#[must_use]
pub fn recipe(reply: &str, code: u8) -> Option<Vec<u8>> {
    let bytes = unhex(&checked(reply, "41")?)?;
    (bytes.first() == Some(&code)).then_some(bytes)
}

/// The brew command for `drink` from the recipe the machine keeps: the
/// recipe's sixteen bytes, the drink's code first (a double's own code on
/// its single's recipe), byte 8 set to 1 (« recipe valid »; without it the
/// machine acknowledges and brews nothing).
#[must_use]
pub fn brew_command(drink: &Drink, recipe: &[u8]) -> String {
    let mut blob = [0u8; 16];
    for (b, r) in blob.iter_mut().zip(recipe) {
        *b = *r;
    }
    blob[0] = drink.code;
    blob[8] = 1;
    format!("@TP:{}", hex(&blob))
}

/// `@tm:<argument>,<value><sum>`: a setting's value.
#[must_use]
pub fn setting_value(setting: &Setting, reply: &str) -> Option<Value> {
    let raw = checked(reply, setting.argument)?;
    if setting.range.is_some() {
        let n = u8::from_str_radix(raw.get(..2)?, 16).ok()?;
        return Some(Value::Int(i64::from(n)));
    }
    setting
        .values
        .iter()
        .find(|(_, v)| *v == raw)
        .map(|(name, _)| Value::from(*name))
}

/// What an order asks the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Order {
    /// Read the single's recipe (`@TM:41,<code>`), then brew `drink`.
    Brew(Drink),
    /// `@TG:<code>`, answered by its echo.
    Program(u8),
    /// Write a setting (keys held around it), then read it back.
    Set { setting: Setting, value: String },
}

/// An order for point `key`, weighed against the machine's last status.
pub fn order(
    model: &Model,
    status: Option<&Status>,
    key: &str,
    value: &Value,
) -> Result<Order, String> {
    let text = |v: &Value| match v {
        Value::Text(t) => Some(t.to_string()),
        _ => None,
    };
    let Some(status) = status else {
        return Err(tr("pas_joignable"));
    };
    let idle = matches!(status.state(model), "ready" | "energy_saving");
    match key {
        "brew" => {
            let slug = text(value).unwrap_or_default();
            let drink = *model
                .drink(&slug)
                .filter(|d| !d.counted_only)
                .ok_or_else(|| tr("boisson_inconnue"))?;
            if !idle {
                return Err(moli_i18n::tr!(
                    "pilotes.jura.occupee",
                    state = tr(&format!("etat.{}", status.state(model)))
                ));
            }
            if drink.milk && NO_MILK.iter().any(|&b| status.bit(b)) {
                return Err(tr("pas_de_lait"));
            }
            Ok(Order::Brew(drink))
        }
        "maintenance" => {
            let name = text(value).unwrap_or_default();
            let (_, code) = PROGRAMS
                .iter()
                .find(|(n, _)| *n == name)
                .ok_or_else(|| tr("entretien_inconnu"))?;
            if !idle {
                return Err(moli_i18n::tr!(
                    "pilotes.jura.occupee",
                    state = tr(&format!("etat.{}", status.state(model)))
                ));
            }
            Ok(Order::Program(*code))
        }
        _ => {
            let setting = *model
                .settings
                .iter()
                .find(|s| s.key == key)
                .ok_or_else(|| tr("lecture_seule"))?;
            if !idle {
                return Err(moli_i18n::tr!(
                    "pilotes.jura.occupee",
                    state = tr(&format!("etat.{}", status.state(model)))
                ));
            }
            let raw = match setting.range {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Some(_) => value.as_f64().map(|n| format!("{:02X}", n.round() as u8)),
                None => text(value).and_then(|t| {
                    setting
                        .values
                        .iter()
                        .find(|(n, _)| *n == t)
                        .map(|(_, v)| (*v).to_owned())
                }),
            };
            Ok(Order::Set {
                setting,
                value: raw.ok_or_else(|| tr("valeur_refusee"))?,
            })
        }
    }
}

/// The single drink whose recipe `drink` brews.
#[must_use]
pub fn recipe_code(drink: &Drink) -> u8 {
    drink.single.unwrap_or(drink.code)
}

/// The command writing a setting, with its checksum.
#[must_use]
pub fn set_command(setting: &Setting, value: &str) -> String {
    let text = format!("{},{value}", setting.argument);
    format!("@TM:{text}{}", checksum(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e8() -> Model {
        Model::new(Some("EF1092"))
    }

    fn get<'a>(values: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
        values.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    #[test]
    fn a_real_e8_idle() {
        // Pushed by a real E8 (EC): coffee ready, energy saving, filter fitted.
        let status = Status::parse("@TF:000400000C0000").unwrap();
        assert_eq!(status.state(&e8()), "energy_saving");
        assert!(!status.blocked());
        let values = status.values(&e8());
        assert_eq!(get(&values, "filter_fitted"), Some(&Value::Bool(true)));
        assert_eq!(get(&values, "water_empty"), Some(&Value::Bool(false)));
        // The same machine a moment before, its drip tray to empty.
        let full = Status::parse("@TF:100000000C0000").unwrap();
        assert_eq!(full.state(&e8()), "attention");
        assert_eq!(
            get(&full.values(&e8()), "drip_tray_full"),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn a_brew_in_progress() {
        // From a real S8 brewing a Café Barista (jura-connect's capture).
        let status = Status::parse("@TV:41280000091E0000FF00000000003200").unwrap();
        let mut model = e8();
        assert_eq!(status.state(&model), "brewing");
        let values = status.values(&model);
        assert_eq!(get(&values, "activity"), Some(&Value::from("cafe_barista")));
        assert_eq!(get(&values, "progress"), Some(&Value::Int(50)));
        assert_eq!(get(&values, "water_empty"), None);
        assert_eq!(
            Status::parse("@TV:3E28").unwrap().values(&model)[2].1,
            Value::Int(100)
        );
        model.drinks.clear();
        assert_eq!(
            Status::parse("@TV:742400").unwrap().state(&model),
            "maintenance"
        );
        assert_eq!(Status::parse("@TV:84,12:00"), None);
    }

    #[test]
    fn a_real_e8s_counters() {
        let c = counters("@tg:43000300030000004002110000");
        assert_eq!(get(&c, "cleanings"), Some(&Value::Int(3)));
        assert_eq!(get(&c, "milk_rinses"), Some(&Value::Int(64)));
        assert_eq!(get(&c, "coffee_rinses"), Some(&Value::Int(529)));
        let p = percents("@tg:C00000FF");
        assert_eq!(get(&p, "filter_wear"), Some(&Value::Int(0)));
        assert_eq!(get(&p, "descaling_need"), None);
        let page = counted("@tr:32,00,02C4FFFF00980186", 0);
        assert_eq!(page.get(&0), Some(&708));
        assert_eq!(page.get(&3), Some(&390));
        assert_eq!(page.get(&1), None);
        assert!(counted("@tr:32,01,0010", 0).is_empty());
        let b = brews(&e8(), &page);
        assert_eq!(get(&b, "brews_total"), Some(&Value::Int(708)));
        assert_eq!(get(&b, "brews.espresso"), Some(&Value::Int(152)));
    }

    #[test]
    fn a_real_e8s_recipes_brew_as_the_reference_says() {
        let model = e8();
        let espresso = *model.drink("espresso").unwrap();
        let r = recipe("@tm:41,020008090000020000000000000019", 2).unwrap();
        // The same bytes an E6's espresso was brewed with (jura-connect).
        assert_eq!(
            brew_command(&espresso, &r),
            "@TP:02000809000002000100000000000000"
        );
        let double = *model.drink("two_espressi").unwrap();
        assert_eq!(recipe_code(&double), 2);
        assert_eq!(
            brew_command(&double, &r),
            "@TP:31000809000002000100000000000000"
        );
        let barista = recipe("@tm:41,2800060C0000010000080000000002", 0x28).unwrap();
        assert_eq!(
            brew_command(model.drink("cafe_barista").unwrap(), &barista),
            "@TP:2800060C000001000108000000000000"
        );
        assert_eq!(recipe("@tm:C1", 0x31), None);
        assert_eq!(recipe("@tm:41,020008090000020000000000000018", 2), None);
        assert_eq!(recipe("@tm:41,020008090000020000000000000019", 3), None);
    }

    #[test]
    fn a_real_e8s_settings() {
        let s = |k: &str| *E8_EC_SETTINGS.iter().find(|s| s.key == k).unwrap();
        assert_eq!(
            setting_value(&s("water_hardness"), "@tm:02,0A00"),
            Some(Value::Int(10))
        );
        assert_eq!(
            setting_value(&s("auto_off"), "@tm:13,21F096"),
            Some(Value::from("4h"))
        );
        assert_eq!(
            setting_value(&s("language"), "@tm:09,0307"),
            Some(Value::from("french"))
        );
        assert_eq!(
            setting_value(&s("milk_rinsing"), "@tm:04,000F"),
            Some(Value::from("automatic"))
        );
        assert_eq!(
            setting_value(&s("units"), "@tm:08,000B"),
            Some(Value::from("ml"))
        );
        assert_eq!(setting_value(&s("units"), "@tm:08,000C"), None);
        assert_eq!(set_command(&s("auto_off"), "21F0"), "@TM:13,21F096");
    }

    #[test]
    fn orders_wait_for_an_idle_machine() {
        let model = e8();
        let ready = Status::parse("@TF:000400000C0000").unwrap();
        let busy = Status::parse("@TV:41280000091E0000FF00000000003200").unwrap();
        let no_milk = Status::parse("@TF:000200000C0000").unwrap();
        let brew = |s: Option<&Status>, d: &str| order(&model, s, "brew", &Value::from(d));
        assert!(matches!(brew(Some(&ready), "cappuccino"), Ok(Order::Brew(d)) if d.code == 4));
        assert!(brew(Some(&busy), "coffee").is_err());
        assert!(brew(None, "coffee").is_err());
        assert!(brew(Some(&ready), "ground_coffee").is_err());
        assert!(brew(Some(&no_milk), "cappuccino").is_err());
        assert!(brew(Some(&no_milk), "coffee").is_ok());
        // No cancel: `@TG:FF` is acknowledged by an E8 and stops nothing.
        assert!(order(&model, Some(&busy), "cancel", &Value::Bool(true)).is_err());
        assert_eq!(
            order(
                &model,
                Some(&ready),
                "maintenance",
                &Value::from("cleaning")
            ),
            Ok(Order::Program(0x24))
        );
        assert!(matches!(
            order(&model, Some(&ready), "auto_off", &Value::from("2h")),
            Ok(Order::Set { value, .. }) if value == "2178"
        ));
        assert!(matches!(
            order(&model, Some(&ready), "water_hardness", &Value::Float(12.0)),
            Ok(Order::Set { value, .. }) if value == "0C"
        ));
        assert!(order(&model, Some(&ready), "brews_total", &Value::Int(1)).is_err());
    }

    /// Every answer of a real E8 (EC) in the fixture, read as Moli reads it.
    #[test]
    fn the_fixture_reads_whole() {
        let text = include_str!("../../../integrations/jura/fixtures/e8-ec.txt");
        let model = e8();
        let mut all = BTreeMap::new();
        let mut recipes = 0;
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            match line.split_once(' ') {
                None => assert!(Status::parse(line).is_some(), "{line}"),
                Some((q, a)) if q.starts_with("@TR:32,") => {
                    let page = u8::from_str_radix(&q[7..], 16).unwrap();
                    all.extend(counted(a, page));
                }
                Some((q, a)) if q.starts_with("@TM:41,") => {
                    let code = u8::from_str_radix(&q[7..], 16).unwrap();
                    let d = model.drink_by_code(code).unwrap();
                    match recipe(a, code) {
                        Some(r) => {
                            recipes += 1;
                            let command = brew_command(d, &r);
                            assert_eq!(command.len(), 4 + 32, "{command}");
                            assert_eq!(&command[20..22], "01", "{command}");
                        }
                        None => assert!(d.single.is_some(), "{line}"),
                    }
                }
                Some((q, a)) if q.starts_with("@TM:") => {
                    let s = model
                        .settings
                        .iter()
                        .find(|s| q[4..] == *s.argument)
                        .unwrap();
                    assert!(setting_value(s, a).is_some(), "{line}");
                }
                Some((_, a)) => {
                    assert!(!counters(a).is_empty() || !percents(a).is_empty(), "{line}");
                }
            }
        }
        assert_eq!(recipes, 12);
        // The machine's total is the sum of its drinks', each one known.
        let total = all[&0];
        let sum: u32 = all.iter().filter(|(c, _)| **c != 0).map(|(_, n)| n).sum();
        assert_eq!((total, sum), (708, 708));
        assert!(
            all.keys()
                .all(|c| *c == 0 || model.drink_by_code(*c).is_some())
        );
    }

    #[test]
    fn an_e8_is_joined_again_sooner() {
        assert_eq!(e8().gap_ms(), 1500);
        assert_eq!(Model::new(None).gap_ms(), 3000);
    }

    #[test]
    fn an_unknown_model_learns_its_drinks() {
        let mut model = Model::new(None);
        assert!(model.settings.is_empty());
        let counted = BTreeMap::from([(0, 10), (2, 4), (0x28, 6)]);
        assert!(model.learn(&counted));
        assert!(!model.learn(&counted));
        assert_eq!(
            get(&brews(&model, &counted), "brews.drink_28"),
            Some(&Value::Int(6))
        );
    }

    #[test]
    fn every_point_is_unique() {
        let points = e8().points();
        let mut keys: Vec<_> = points.iter().map(|p| p.key.clone()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), points.len());
        let control: Vec<_> = points
            .iter()
            .filter(|p| p.semantic == Semantic::Control)
            .map(|p| &*p.key)
            .collect();
        assert_eq!(control, ["brew", "maintenance"]);
    }
}
