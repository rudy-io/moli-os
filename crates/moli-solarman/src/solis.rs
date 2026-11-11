//! Solis single-phase string inverters (S6-GR1P) behind a Solarman logger:
//! input registers 3004 to 3043 (Modbus function 0x04), read in two blocks.
//!
//! Register map from ha-solarman's `solis_s6-gr1p.yaml` (MIT, see NOTICE),
//! checked against Home Assistant reading the same inverter: 32-bit values
//! are low word first in the map's order ([3005, 3004] = high, low).

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};

/// The first register read, and the two blocks (the logger answers at most
/// about 40 registers at once).
pub const FIRST: u16 = 3004;
pub const BLOCKS: [(u16, u16); 2] = [(3004, 22), (3026, 18)];
const LAST: u16 = 3043;

/// What the inverter is doing, from register 3043.
pub const STATES: [&str; 4] = ["waiting", "starting", "generating", "fault"];

/// The lifetime counter last accepted: a read that goes back is a misread.
#[derive(Debug, Default)]
pub struct Counters {
    total: Option<f64>,
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

/// The Moli device of the inverter.
#[must_use]
#[allow(clippy::too_many_lines)] // one line per point
pub fn device(instance: &InstanceId, id: DeviceId) -> Device {
    use moli_i18n::tr;
    let mut points = vec![
        spec(
            "power",
            &tr!("pilotes.solarman.production"),
            Some(Unit::Watt),
            Semantic::Power,
        ),
        // The panels' side: part of the same production, never added to it.
        spec(
            "pv_power",
            &tr!("pilotes.solarman.puissance_panneaux"),
            Some(Unit::Watt),
            Semantic::Other,
        ),
        // A daily counter goes back to 0 every morning: not an energy meter.
        spec(
            "energy_today",
            &tr!("pilotes.solarman.production_jour"),
            Some(Unit::KiloWattHour),
            Semantic::Other,
        ),
        spec(
            "energy_month",
            &tr!("pilotes.solarman.production_mois"),
            Some(Unit::KiloWattHour),
            Semantic::Other,
        ),
        spec(
            "energy_year",
            &tr!("pilotes.solarman.production_annee"),
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
            "fault_code",
            &tr!("pilotes.solarman.code_defaut"),
            None,
            Semantic::Other,
        ),
    ];
    for n in 1..=2u8 {
        let label = |what: &str| tr!("pilotes.solarman.entree", what = what, n = n);
        points.push(spec(
            &format!("pv{n}_voltage"),
            &label(&tr!("pilotes.solarman.tension")),
            Some(Unit::Volt),
            Semantic::Voltage,
        ));
        points.push(spec(
            &format!("pv{n}_current"),
            &label(&tr!("pilotes.solarman.courant")),
            Some(Unit::Ampere),
            Semantic::Current,
        ));
        points.push(spec(
            &format!("pv{n}_power"),
            &label(&tr!("pilotes.solarman.puissance")),
            Some(Unit::Watt),
            Semantic::Other,
        ));
    }
    Device {
        id,
        instance: instance.clone(),
        native_name: tr!("pilotes.solarman.onduleur").into(),
        manufacturer: Some("Solis".into()),
        model: Some("S6-GR1P".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

fn round(v: f64, decimals: i32) -> f64 {
    let f = 10f64.powi(decimals);
    (v * f).round() / f
}

/// The points' values from registers 3004..=3043 (`regs[0]` is 3004).
#[must_use]
pub fn readings(regs: &[u16], counters: &mut Counters) -> Vec<(String, Value)> {
    if regs.len() < usize::from(LAST - FIRST + 1) {
        return Vec::new();
    }
    let r = |addr: u16| f64::from(regs[usize::from(addr - FIRST)]);
    let u32_at = |high: u16, low: u16| r(high) * 65_536.0 + r(low);
    let mut out: Vec<(String, Value)> = Vec::new();
    let mut put = |key: &str, v: f64| out.push((key.to_owned(), Value::Float(v)));
    put("power", u32_at(3004, 3005));
    put("pv_power", u32_at(3006, 3007));
    put("energy_today", round(r(3014) / 10.0, 1));
    put("energy_month", u32_at(3010, 3011));
    put("energy_year", u32_at(3016, 3017));
    let total = u32_at(3008, 3009);
    if counters.total.is_none_or(|t| total >= t) {
        counters.total = Some(total);
        put("energy", total);
    }
    let (v1, i1, v2, i2) = (
        r(3021) / 10.0,
        r(3022) / 10.0,
        r(3023) / 10.0,
        r(3024) / 10.0,
    );
    put("pv1_voltage", round(v1, 1));
    put("pv1_current", round(i1, 1));
    put("pv1_power", round(v1 * i1, 0));
    put("pv2_voltage", round(v2, 1));
    put("pv2_current", round(i2, 1));
    put("pv2_power", round(v2 * i2, 0));
    put("grid_voltage", round(r(3035) / 10.0, 1));
    put("grid_current", round(r(3038) / 10.0, 1));
    put("grid_frequency", round(r(3042) / 100.0, 2));
    // A signed temperature, in tenths.
    #[allow(clippy::cast_possible_wrap)]
    put(
        "temperature",
        round(f64::from(regs[usize::from(3041 - FIRST)] as i16) / 10.0, 1),
    );
    let code = regs[usize::from(3043 - FIRST)];
    let state = match code {
        0 => "waiting",
        1 | 2 => "starting",
        3 => "generating",
        _ => "fault",
    };
    out.push(("state".into(), Value::Text(state.into())));
    out.push((
        "fault_code".into(),
        Value::Float(if state == "fault" {
            f64::from(code)
        } else {
            0.0
        }),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registers as the real inverter gave them while Home Assistant read
    /// 50 W out, 0.102 kW of panels, 4352 kWh in all, 22.9 °C.
    fn registers() -> Vec<u16> {
        let mut regs = vec![0u16; usize::from(LAST - FIRST + 1)];
        let mut set = |addr: u16, v: u16| regs[usize::from(addr - FIRST)] = v;
        set(3005, 50);
        set(3007, 102);
        set(3009, 4352);
        set(3011, 21);
        set(3014, 1);
        set(3017, 2302);
        set(3021, 2566);
        set(3022, 4);
        set(3023, 6);
        set(3024, 1);
        set(3035, 2395);
        set(3038, 5);
        set(3041, 229);
        set(3042, 4999);
        set(3043, 3);
        regs
    }

    #[test]
    fn readings_match_what_home_assistant_reads() {
        let mut counters = Counters::default();
        let got = readings(&registers(), &mut counters);
        let get = |k: &str| got.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("power"), Some(Value::Float(50.0)));
        assert_eq!(get("pv_power"), Some(Value::Float(102.0)));
        assert_eq!(get("energy"), Some(Value::Float(4352.0)));
        assert_eq!(get("energy_month"), Some(Value::Float(21.0)));
        assert_eq!(get("energy_year"), Some(Value::Float(2302.0)));
        assert_eq!(get("energy_today"), Some(Value::Float(0.1)));
        assert_eq!(get("pv1_voltage"), Some(Value::Float(256.6)));
        assert_eq!(get("pv1_power"), Some(Value::Float(103.0)));
        assert_eq!(get("grid_voltage"), Some(Value::Float(239.5)));
        assert_eq!(get("grid_frequency"), Some(Value::Float(49.99)));
        assert_eq!(get("temperature"), Some(Value::Float(22.9)));
        assert_eq!(get("state"), Some(Value::Text("generating".into())));
        assert_eq!(get("fault_code"), Some(Value::Float(0.0)));
    }

    #[test]
    fn a_lifetime_counter_never_goes_back_and_a_fault_says_its_code() {
        let mut counters = Counters::default();
        let _ = readings(&registers(), &mut counters);
        let mut back = registers();
        back[usize::from(3009 - FIRST)] = 4000;
        back[usize::from(3043 - FIRST)] = 0x1015;
        let got = readings(&back, &mut counters);
        assert!(
            got.iter().all(|(k, _)| k != "energy"),
            "a misread total is dropped"
        );
        assert!(got.contains(&("state".into(), Value::Text("fault".into()))));
        assert!(got.contains(&("fault_code".into(), Value::Float(4117.0))));
        assert!(readings(&[0; 3], &mut counters).is_empty());
    }
}
