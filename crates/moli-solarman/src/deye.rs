//! Deye micro-inverters behind a Solarman logger: which registers mean what
//! (pure, no I/O).
//!
//! The register map follows ha-solarman's `deye_micro.yaml` (MIT, see
//! `licenses/ha-solarman-MIT.txt`), itself from Deye's Modbus protocol
//! V1.18, and was checked value for value against Home Assistant on a
//! 2-input and a 4-input micro-inverter.

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};

/// Who the inverter is: read once at start (and every hour).
pub const INFO: (u16, u16) = (0x0000, 0x13);
/// What it does: read at every poll.
pub const LIVE: (u16, u16) = (0x003B, 0x3A);

/// What the device states mean (register 0x3B).
pub const STATES: [&str; 5] = ["standby", "self_test", "normal", "alarm", "fault"];

/// Energy counters per PV input, lifetime (low word first).
const PV_TOTALS: [u16; 4] = [0x45, 0x47, 0x4A, 0x4D];

/// A lifetime counter that moves by more than this between two reads is a
/// misread (ha-solarman's `dev: 100`).
const MAX_STEP_KWH: f64 = 100.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    /// PV inputs (MPPTs).
    pub inputs: u8,
    /// Rated power (W).
    pub rated: f64,
    /// The inverter's own serial number, as it says it.
    pub serial: String,
}

impl Info {
    /// From the `INFO` registers. `None` if this is not a micro-inverter.
    #[must_use]
    pub fn read(regs: &[u16]) -> Option<Self> {
        let at = |a: u16| regs.get(usize::from(a - INFO.0)).copied();
        // 0x0004 (or 0x0400 on some firmwares): micro-inverter.
        if !matches!(at(0x00)?, 0x0004 | 0x0400) {
            return None;
        }
        let inputs = u8::try_from(at(0x12)? >> 8)
            .ok()
            .filter(|n| (1..=4).contains(n))?;
        let rated = f64::from(u32::from(at(0x10)?) | (u32::from(at(0x11)?) << 16)) / 10.0;
        let serial: String = (0x03..=0x07)
            .filter_map(at)
            .flat_map(u16::to_be_bytes)
            .filter(u8::is_ascii_alphanumeric)
            .map(char::from)
            .collect();
        Some(Self {
            inputs,
            rated: if rated > 0.0 && rated <= 4000.0 {
                rated
            } else {
                4000.0
            },
            serial,
        })
    }
}

fn spec(key: &str, label: &str, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
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

/// The points of PV input `n`.
fn input_points(n: u8) -> Vec<PointSpec> {
    use moli_i18n::tr;
    let mut out = Vec::new();
    let label = |what: &str| tr!("pilotes.solarman.entree", what = what, n = n);
    out.push(spec(
        &format!("pv{n}_voltage"),
        &label(&tr!("pilotes.solarman.tension")),
        Some(Unit::Volt),
        Semantic::Voltage,
    ));
    out.push(spec(
        &format!("pv{n}_current"),
        &label(&tr!("pilotes.solarman.courant")),
        Some(Unit::Ampere),
        Semantic::Current,
    ));
    out.push(spec(
        &format!("pv{n}_power"),
        &label(&tr!("pilotes.solarman.puissance")),
        Some(Unit::Watt),
        // Part of the inverter's production: never added to it.
        Semantic::Other,
    ));
    out.push(spec(
        &format!("pv{n}_energy_today"),
        &label(&tr!("pilotes.solarman.production_jour")),
        Some(Unit::KiloWattHour),
        Semantic::Other,
    ));
    out.push(spec(
        &format!("pv{n}_energy"),
        &label(&tr!("pilotes.solarman.production_totale")),
        Some(Unit::KiloWattHour),
        Semantic::Other,
    ));
    out
}

/// The Moli device of one inverter.
#[must_use]
pub fn device(instance: &InstanceId, id: DeviceId, info: &Info) -> Device {
    use moli_i18n::tr;
    let mut points = vec![
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
        PointSpec {
            kind: Kind::Enum {
                values: STATES.iter().map(|s| (*s).into()).collect(),
            },
            ..spec(
                "state",
                &tr!("pilotes.solarman.etat"),
                None,
                Semantic::Other,
            )
        },
        spec(
            "alarm_code",
            &tr!("pilotes.solarman.code_alarme"),
            None,
            Semantic::Other,
        ),
        spec(
            "fault_code",
            &tr!("pilotes.solarman.code_defaut"),
            None,
            Semantic::Other,
        ),
        spec(
            "rated_power",
            &tr!("pilotes.solarman.puissance_nominale"),
            Some(Unit::Watt),
            Semantic::Other,
        ),
    ];
    for n in 1..=info.inputs {
        points.extend(input_points(n));
    }
    Device {
        id,
        instance: instance.clone(),
        native_name: tr!("pilotes.solarman.onduleur").into(),
        manufacturer: Some("Deye".into()),
        model: Some(tr!("pilotes.solarman.modele", n = info.inputs).into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

/// The lifetime counters last accepted: a new read that goes back, or jumps,
/// is a misread and is dropped (the rest of the read is kept).
#[derive(Debug, Default)]
pub struct Counters {
    last: std::collections::HashMap<String, f64>,
}

impl Counters {
    fn accept(&mut self, key: &str, value: f64) -> bool {
        if value < 0.1 {
            return false;
        }
        let ok = self
            .last
            .get(key)
            .is_none_or(|&prev| value >= prev && value - prev <= MAX_STEP_KWH);
        if ok {
            self.last.insert(key.to_owned(), value);
        }
        ok
    }
}

/// Rounds to the register's resolution (no `0.30000000000000004`).
fn round(v: f64, decimals: i32) -> f64 {
    let f = 10f64.powi(decimals);
    (v * f).round() / f
}

fn put(out: &mut Vec<(String, Value)>, key: String, value: Option<f64>, (min, max): (f64, f64)) {
    if let Some(v) = value.filter(|v| v.is_finite() && (min..=max).contains(v)) {
        out.push((key, Value::Float(v)));
    }
}

/// The values of a `LIVE` read; absurd ones are left out.
#[must_use]
pub fn readings(regs: &[u16], info: &Info, counters: &mut Counters) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let at = |a: u16| regs.get(usize::from(a - LIVE.0)).copied();
    let wide = |a: u16| Some(f64::from(u32::from(at(a)?) | (u32::from(at(a + 1)?) << 16)));

    let day = (0.0, info.rated / 1000.0 * 24.0);
    put(
        &mut out,
        "power".into(),
        wide(0x56).map(|v| round(v / 10.0, 1)),
        (0.0, info.rated * 1.2),
    );
    put(
        &mut out,
        "energy_today".into(),
        at(0x3C).map(|v| f64::from(v) / 10.0),
        day,
    );
    put(
        &mut out,
        "grid_voltage".into(),
        at(0x49).map(|v| f64::from(v) / 10.0),
        (0.0, 300.0),
    );
    #[allow(clippy::cast_possible_wrap)]
    put(
        &mut out,
        "grid_current".into(),
        at(0x4C).map(|v| f64::from(v as i16) / 10.0),
        (-50.0, 50.0),
    );
    put(
        &mut out,
        "grid_frequency".into(),
        at(0x4F).map(|v| f64::from(v) / 100.0),
        (40.0, 70.0),
    );
    put(
        &mut out,
        "temperature".into(),
        at(0x5A).map(|v| round((f64::from(v) - 1000.0) / 100.0, 2)),
        (-40.0, 120.0),
    );
    put(
        &mut out,
        "rated_power".into(),
        Some(info.rated),
        (0.0, 4000.0),
    );
    put(
        &mut out,
        "alarm_code".into(),
        wide(0x65),
        (0.0, f64::from(u32::MAX)),
    );
    let fault = (0x67..=0x6A)
        .filter_map(at)
        .fold(0u64, |acc, w| (acc << 16) | u64::from(w));
    #[allow(clippy::cast_precision_loss)]
    put(
        &mut out,
        "fault_code".into(),
        Some(fault as f64),
        (0.0, f64::MAX),
    );
    for n in 1..=info.inputs {
        let i = u16::from(n - 1);
        let volts = at(0x6D + 2 * i).map(|v| f64::from(v) / 10.0);
        let amps = at(0x6E + 2 * i).map(|v| f64::from(v) / 10.0);
        put(&mut out, format!("pv{n}_voltage"), volts, (0.0, 100.0));
        put(&mut out, format!("pv{n}_current"), amps, (0.0, 30.0));
        put(
            &mut out,
            format!("pv{n}_power"),
            volts.zip(amps).map(|(v, a)| round(v * a, 2)),
            (0.0, info.rated),
        );
        put(
            &mut out,
            format!("pv{n}_energy_today"),
            at(0x41 + i).map(|v| f64::from(v) / 10.0),
            day,
        );
        let total = wide(PV_TOTALS[usize::from(n - 1)]).map(|v| round(v / 10.0, 1));
        if let Some(total) = total.filter(|&t| counters.accept(&format!("pv{n}_energy"), t)) {
            out.push((format!("pv{n}_energy"), Value::Float(total)));
        }
    }
    if let Some(total) = wide(0x3F)
        .map(|v| round(v / 10.0, 1))
        .filter(|&t| counters.accept("energy", t))
    {
        out.push(("energy".into(), Value::Float(total)));
    }
    if let Some(state) = at(0x3B).and_then(|s| STATES.get(usize::from(s))) {
        out.push(("state".into(), Value::Text((*state).into())));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v5::{Frame, parse, tests::fixture};
    use std::collections::HashMap;

    fn blocks(name: &str) -> (Vec<u16>, Vec<u16>) {
        let mut info = Vec::new();
        let mut live = Vec::new();
        for (start, count, sequence, bytes) in fixture(name) {
            let Frame::Registers(r, _) = parse(&bytes, 1_234_567_890, sequence, count).unwrap()
            else {
                panic!()
            };
            if start == INFO.0 { info = r } else { live = r }
        }
        (info, live)
    }

    fn values(name: &str) -> (Info, HashMap<String, Value>) {
        let (info, live) = blocks(name);
        let info = Info::read(&info).unwrap();
        let v = readings(&live, &info, &mut Counters::default())
            .into_iter()
            .collect();
        (info, v)
    }

    // Values checked against Home Assistant (ha-solarman) at the same instant.
    #[test]
    fn a_two_input_micro_inverter() {
        let (info, v) = values("micro-2-inputs");
        assert_eq!(info.inputs, 2);
        assert!((info.rated - 1000.0).abs() < 1e-9);
        assert_eq!(info.serial, "1234567890");
        assert_eq!(v["state"], Value::Text("normal".into()));
        for (key, expected) in [
            ("power", 448.4),
            ("energy_today", 1.3),
            ("energy", 2283.7),
            ("grid_voltage", 233.0),
            ("temperature", 44.3),
            ("pv1_voltage", 29.2),
            ("pv1_current", 10.0),
            ("pv1_energy", 1203.9),
        ] {
            assert_eq!(v[key], Value::Float(expected), "{key}");
        }
        assert!(v.contains_key("pv2_energy"));
        assert!(!v.contains_key("pv3_voltage"));
        let device = device(
            &InstanceId::from("onduleur"),
            DeviceId::new(&InstanceId::from("onduleur"), "1"),
            &info,
        );
        assert!(device.point("pv2_power").is_some());
        assert!(device.point("pv3_power").is_none());
        assert_eq!(device.point("energy").unwrap().semantic, Semantic::Energy);
    }

    #[test]
    fn a_four_input_micro_inverter() {
        let (info, v) = values("micro-4-inputs");
        assert_eq!(info.inputs, 4);
        assert!((info.rated - 2000.0).abs() < 1e-9);
        for (key, expected) in [
            ("power", 1143.7),
            ("energy_today", 3.6),
            ("energy", 4782.6),
            ("temperature", 55.3),
            ("pv1_voltage", 28.5),
            ("pv1_current", 10.5),
            ("pv1_energy", 1242.8),
        ] {
            assert_eq!(v[key], Value::Float(expected), "{key}");
        }
        for key in [
            "power",
            "energy_today",
            "energy",
            "grid_voltage",
            "temperature",
            "pv4_energy",
        ] {
            assert!(v.contains_key(key), "{key}");
        }
    }

    #[test]
    fn a_lifetime_counter_never_goes_back_nor_jumps() {
        let mut c = Counters::default();
        assert!(c.accept("energy", 4782.5));
        assert!(!c.accept("energy", 4700.0));
        assert!(!c.accept("energy", 9999.0));
        assert!(!c.accept("energy", 0.0));
        assert!(c.accept("energy", 4782.6));
    }

    #[test]
    fn absurd_values_are_left_out() {
        let (info, mut live) = blocks("micro-2-inputs");
        let info = Info::read(&info).unwrap();
        live[usize::from(0x56 - LIVE.0)] = 0xFFFF; // power: 6.5 kW on a 1 kW inverter
        live[usize::from(0x57 - LIVE.0)] = 0;
        let v: HashMap<_, _> = readings(&live, &info, &mut Counters::default())
            .into_iter()
            .collect();
        assert!(!v.contains_key("power"));
        assert!(v.contains_key("grid_voltage"));
    }
}
