//! What the register maps written after Deye and Solis share: the blocks a
//! map asks for, the registers read, and the points' plumbing (pure, no I/O).

use std::time::Instant;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};

/// How a block of registers is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// Modbus function 0x03 (holding registers).
    Holding,
    /// TSUN's own framing: a read code and its sub-code.
    Native(u8, u8),
}

/// One block of registers to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ask {
    pub how: How,
    pub start: u16,
    pub count: u16,
}

impl Ask {
    #[must_use]
    pub const fn holding(start: u16, count: u16) -> Self {
        Self {
            how: How::Holding,
            start,
            count,
        }
    }

    #[must_use]
    pub const fn native(code: u8, sub: u8, start: u16, count: u16) -> Self {
        Self {
            how: How::Native(code, sub),
            start,
            count,
        }
    }
}

/// The registers of one turn: every block read, by its first address.
#[derive(Debug, Default)]
pub struct Regs(Vec<(u16, Vec<u16>)>);

impl Regs {
    pub fn push(&mut self, start: u16, words: Vec<u16>) {
        self.0.push((start, words));
    }

    /// The register at `addr`, if a block read holds it.
    #[must_use]
    pub fn at(&self, addr: u16) -> Option<u16> {
        self.0.iter().find_map(|(start, words)| {
            addr.checked_sub(*start)
                .and_then(|i| words.get(usize::from(i)))
                .copied()
        })
    }

    #[must_use]
    pub fn num(&self, addr: u16) -> Option<f64> {
        self.at(addr).map(f64::from)
    }

    /// A signed 16-bit register.
    #[must_use]
    pub fn signed(&self, addr: u16) -> Option<f64> {
        #[allow(clippy::cast_possible_wrap)]
        self.at(addr).map(|v| f64::from(v as i16))
    }

    /// A 32-bit value from its high and low words.
    #[must_use]
    pub fn wide(&self, high: u16, low: u16) -> Option<f64> {
        Some(f64::from(
            (u32::from(self.at(high)?) << 16) | u32::from(self.at(low)?),
        ))
    }
}

/// One inverter family behind a logger: what to read and what it means.
pub trait Map: Send {
    /// For the log.
    fn name(&self) -> &'static str;
    /// The blocks read at every turn.
    fn asks(&self) -> Vec<Ask>;
    /// A block that says which inverter this is, while that is still unknown.
    fn identity(&self) -> Option<Ask> {
        None
    }
    /// What the identity block gave (`None`: it could not be read). `true`:
    /// the device changed.
    fn learn(&mut self, _regs: Option<&[u16]>) -> bool {
        false
    }
    fn device(&self, instance: &InstanceId, id: DeviceId) -> Device;
    fn readings(&mut self, regs: &Regs, now: Instant) -> Vec<(String, Value)>;
    /// The points that go back to 0 when a new day begins while the inverter
    /// sleeps.
    fn daily(&self) -> Vec<String>;
}

/// A numeric point, read only.
pub fn spec(key: &str, label: &str, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Numeric {
            min: None,
            max: None,
            step: None,
        },
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic,
    }
}

/// The points every inverter has: its production, the grid it feeds and
/// its temperature.
pub fn common_specs() -> Vec<PointSpec> {
    use moli_i18n::tr;
    vec![
        spec(
            "power",
            &tr!("pilotes.solarman.production"),
            Some(Unit::Watt),
            Semantic::Power,
        ),
        // A daily counter goes back to 0 every morning: not an energy meter.
        spec(
            "energy_today",
            &tr!("pilotes.solarman.production_jour"),
            Some(Unit::KiloWattHour),
            Semantic::Other,
        ),
        spec(
            "energy",
            &tr!("pilotes.solarman.production_totale"),
            Some(Unit::KiloWattHour),
            Semantic::Energy,
        ),
        spec(
            "grid_voltage",
            &tr!("pilotes.solarman.tension_reseau"),
            Some(Unit::Volt),
            Semantic::Voltage,
        ),
        spec(
            "grid_current",
            &tr!("pilotes.solarman.courant_reseau"),
            Some(Unit::Ampere),
            Semantic::Current,
        ),
        spec(
            "grid_frequency",
            &tr!("pilotes.solarman.frequence_reseau"),
            Unit::parse("Hz"),
            Semantic::Other,
        ),
        spec(
            "temperature",
            &tr!("pilotes.solarman.temperature"),
            Some(Unit::Celsius),
            Semantic::Temperature,
        ),
    ]
}

/// The state point, one of `values`.
pub fn state_spec(values: &[&str]) -> PointSpec {
    PointSpec {
        kind: Kind::Enum {
            values: values.iter().map(|s| (*s).into()).collect(),
        },
        ..spec(
            "state",
            &moli_i18n::tr!("pilotes.solarman.etat"),
            None,
            Semantic::Other,
        )
    }
}

/// The voltage, current and power points of PV input `n`.
pub fn input_specs(n: u8) -> [PointSpec; 3] {
    use moli_i18n::tr;
    let label = |what: &str| tr!("pilotes.solarman.entree", what = what, n = n);
    [
        spec(
            &format!("pv{n}_voltage"),
            &label(&tr!("pilotes.solarman.tension")),
            Some(Unit::Volt),
            Semantic::Voltage,
        ),
        spec(
            &format!("pv{n}_current"),
            &label(&tr!("pilotes.solarman.courant")),
            Some(Unit::Ampere),
            Semantic::Current,
        ),
        // Part of the inverter's production: never added to it.
        spec(
            &format!("pv{n}_power"),
            &label(&tr!("pilotes.solarman.puissance")),
            Some(Unit::Watt),
            Semantic::Other,
        ),
    ]
}

/// Rounds to the register's resolution (no `0.30000000000000004`).
pub fn round(v: f64, decimals: i32) -> f64 {
    let f = 10f64.powi(decimals);
    (v * f).round() / f
}

/// Adds `value` under `key` unless it is missing or outside what the
/// inverter can read (a misread).
pub fn put(out: &mut Vec<(String, Value)>, key: &str, value: Option<f64>, (min, max): (f64, f64)) {
    if let Some(v) = value.filter(|v| v.is_finite() && (min..=max).contains(v)) {
        out.push((key.to_owned(), Value::Float(v)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_register_is_found_in_the_block_that_holds_it() {
        let mut regs = Regs::default();
        regs.push(0x1100, vec![1, 2]);
        regs.push(0x1200, vec![0xFFFE, 0x0001, 0x86A0]);
        assert_eq!(regs.at(0x1101), Some(2));
        assert_eq!(regs.at(0x1102), None);
        assert_eq!(regs.at(0x10FF), None);
        assert_eq!(regs.signed(0x1200), Some(-2.0));
        assert_eq!(regs.wide(0x1201, 0x1202), Some(100_000.0));
        assert_eq!(regs.wide(0x1201, 0x1203), None);
    }

    #[test]
    fn a_value_out_of_bounds_is_left_out() {
        let mut out = Vec::new();
        put(&mut out, "a", Some(5.0), (0.0, 10.0));
        put(&mut out, "b", Some(11.0), (0.0, 10.0));
        put(&mut out, "c", None, (0.0, 10.0));
        put(&mut out, "d", Some(f64::NAN), (0.0, 10.0));
        assert_eq!(out, vec![("a".to_owned(), Value::Float(5.0))]);
    }
}
