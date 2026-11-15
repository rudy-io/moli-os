//! ZCS Azzurro inverters (Sofar's, under another name) behind a Solarman
//! logger: which registers mean what (pure, no I/O). Two families, both in
//! holding registers:
//!
//! - **1PH TL-V3** (single-phase grid inverter, one PV input): registers
//!   0x0000 to 0x001C, ha-solarman's `sofar_string.yaml` (what its old
//!   `zcs_azzurro-1ph-tl-v3.yaml` now points to);
//! - **HYD-ZSS-HP** (single-phase hybrid, two PV inputs, a battery):
//!   registers 0x0404 to 0x069B in five blocks, ha-solarman's
//!   `sofar_g3.yaml` (its old `zcs_azzurro-hyd-zss-hp.yaml`).
//!
//! Addresses, scales and units only (MIT, see `NOTICE`). **Never read on a
//! real inverter by this driver**: the tests replay frames built by hand
//! from the map. 32-bit values are high word first.

use std::time::Instant;

use moli_core::{Device, DeviceId, InstanceId, Semantic, Unit, Value};

use crate::regs::{Ask, Map, Regs, common_specs, input_specs, put, round, spec, state_spec};
use crate::{deye, solis};

/// What a hybrid is doing (register 0x0404); a grid inverter says it like a
/// Deye (register 0x0000).
pub const HYBRID_STATES: [&str; 8] = [
    "waiting",
    "checking",
    "on_grid",
    "backup",
    "fault",
    "permanent_fault",
    "upgrade",
    "self_charging",
];

/// The hybrid's lifetime counters besides production: key, high and low
/// registers (tenths of a kWh).
const HYBRID_TOTALS: [(&str, u16, u16); 5] = [
    ("load_energy", 0x068A, 0x068B),
    ("energy_import", 0x068E, 0x068F),
    ("energy_export", 0x0692, 0x0693),
    ("battery_charge_energy", 0x0696, 0x0697),
    ("battery_discharge_energy", 0x069A, 0x069B),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// 1PH TL-V3.
    Tl,
    /// HYD-ZSS-HP.
    Hyd,
}

/// One ZCS Azzurro inverter.
#[derive(Debug)]
pub struct Zcs {
    family: Family,
    /// The grid inverter's production: a lifetime count in whole kWh,
    /// completed by the day's.
    lifetime: solis::Counters,
    /// The hybrid's lifetime counters.
    counters: deye::Counters,
}

impl Zcs {
    #[must_use]
    pub fn new(family: Family) -> Self {
        Self {
            family,
            lifetime: solis::Counters::default(),
            counters: deye::Counters::default(),
        }
    }

    fn inputs(&self) -> u8 {
        match self.family {
            Family::Tl => 1,
            Family::Hyd => 2,
        }
    }

    fn grid_inverter(&mut self, regs: &Regs, now: Instant) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        let scaled =
            |addr: u16, by: f64, decimals: i32| regs.num(addr).map(|v| round(v / by, decimals));
        // Powers are in tens of watts.
        put(
            &mut out,
            "power",
            regs.num(0x000C).map(|v| v * 10.0),
            (0.0, 8000.0),
        );
        put(
            &mut out,
            "pv1_voltage",
            scaled(0x0006, 10.0, 1),
            (0.0, 700.0),
        );
        put(
            &mut out,
            "pv1_current",
            scaled(0x0007, 100.0, 2),
            (0.0, 40.0),
        );
        put(
            &mut out,
            "pv1_power",
            regs.num(0x000A).map(|v| v * 10.0),
            (0.0, 8000.0),
        );
        let today = scaled(0x0019, 100.0, 2);
        put(&mut out, "energy_today", today, (0.0, 100.0));
        if let Some(total) = regs
            .wide(0x0015, 0x0016)
            .zip(today)
            .and_then(|(lifetime, today)| self.lifetime.total(lifetime, today, now))
        {
            out.push(("energy".into(), Value::Float(total)));
        }
        put(
            &mut out,
            "grid_frequency",
            scaled(0x000E, 100.0, 2),
            (0.0, 70.0),
        );
        put(
            &mut out,
            "grid_voltage",
            scaled(0x000F, 10.0, 1),
            (0.0, 300.0),
        );
        put(
            &mut out,
            "grid_current",
            scaled(0x0010, 100.0, 2),
            (0.0, 40.0),
        );
        put(&mut out, "temperature", regs.num(0x001C), (-40.0, 120.0));
        if let Some(state) = regs
            .at(0x0000)
            .and_then(|s| deye::STATES.get(usize::from(s)))
        {
            out.push(("state".into(), Value::Text((*state).into())));
        }
        out
    }

    #[allow(clippy::too_many_lines)] // one line per point
    fn hybrid(&mut self, regs: &Regs) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        let scaled =
            |addr: u16, by: f64, decimals: i32| regs.num(addr).map(|v| round(v / by, decimals));
        // Powers are in tens of watts. `+ 0.0`: never a « -0 ».
        let watts = |addr: u16, sign: f64| regs.signed(addr).map(|v| v * 10.0 * sign + 0.0);
        let (pv1, pv2) = (watts(0x0586, 1.0), watts(0x0589, 1.0));
        // What the panels make: the production the counters count.
        put(
            &mut out,
            "power",
            pv1.zip(pv2).map(|(a, b)| a + b),
            (0.0, 15_000.0),
        );
        for (n, base, power) in [(1, 0x0584, pv1), (2, 0x0587, pv2)] {
            put(
                &mut out,
                &format!("pv{n}_voltage"),
                scaled(base, 10.0, 1),
                (0.0, 700.0),
            );
            put(
                &mut out,
                &format!("pv{n}_current"),
                scaled(base + 1, 100.0, 2),
                (0.0, 40.0),
            );
            put(&mut out, &format!("pv{n}_power"), power, (0.0, 15_000.0));
        }
        put(
            &mut out,
            "inverter_power",
            watts(0x0485, 1.0),
            (-15_000.0, 15_000.0),
        );
        // The map counts what goes out to the grid and into the battery
        // as positive: turned round, so that drawing from either is.
        put(
            &mut out,
            "grid_power",
            watts(0x0488, -1.0),
            (-30_000.0, 30_000.0),
        );
        put(
            &mut out,
            "battery_power",
            watts(0x0606, -1.0),
            (-15_000.0, 15_000.0),
        );
        put(
            &mut out,
            "load_power",
            watts(0x04AF, 1.0),
            (-30_000.0, 30_000.0),
        );
        put(
            &mut out,
            "grid_frequency",
            scaled(0x0484, 100.0, 2),
            (0.0, 70.0),
        );
        put(
            &mut out,
            "grid_voltage",
            scaled(0x048D, 10.0, 1),
            (0.0, 300.0),
        );
        put(
            &mut out,
            "grid_current",
            scaled(0x048E, 100.0, 2),
            (0.0, 100.0),
        );
        put(&mut out, "battery", regs.num(0x0608), (0.0, 100.0));
        put(
            &mut out,
            "battery_voltage",
            scaled(0x0604, 10.0, 1),
            (0.0, 1000.0),
        );
        put(
            &mut out,
            "battery_current",
            regs.signed(0x0605).map(|v| round(v / 100.0, 2)),
            (-100.0, 100.0),
        );
        put(
            &mut out,
            "battery_temperature",
            regs.signed(0x0607),
            (-40.0, 120.0),
        );
        put(&mut out, "temperature", regs.signed(0x0418), (-40.0, 120.0));
        put(
            &mut out,
            "radiator_temperature",
            regs.signed(0x041A),
            (-40.0, 120.0),
        );
        put(
            &mut out,
            "energy_today",
            regs.wide(0x0684, 0x0685).map(|v| round(v / 100.0, 2)),
            (0.0, 200.0),
        );
        for (key, high, low) in std::iter::once(("energy", 0x0686, 0x0687)).chain(HYBRID_TOTALS) {
            if let Some(total) = regs
                .wide(high, low)
                .map(|v| round(v / 10.0, 1))
                .filter(|&t| self.counters.accept(key, t))
            {
                out.push((key.to_owned(), Value::Float(total)));
            }
        }
        if let Some(state) = regs
            .at(0x0404)
            .and_then(|s| HYBRID_STATES.get(usize::from(s)))
        {
            out.push(("state".into(), Value::Text((*state).into())));
        }
        out
    }
}

/// The points only a hybrid has.
fn hybrid_specs() -> Vec<moli_core::PointSpec> {
    use moli_i18n::tr;
    let watt = |key: &str, label: &str| spec(key, label, Some(Unit::Watt), Semantic::Other);
    let mut points = vec![
        // Flows around the production, never added to it.
        watt("inverter_power", &tr!("pilotes.solarman.puissance_sortie")),
        watt("grid_power", &tr!("pilotes.solarman.puissance_reseau")),
        watt("load_power", &tr!("pilotes.solarman.consommation")),
        watt("battery_power", &tr!("pilotes.solarman.puissance_batterie")),
        // The house's battery, not a device's cell: no low-battery alert.
        spec(
            "battery",
            &tr!("pilotes.solarman.batterie"),
            Some(Unit::Percent),
            Semantic::Other,
        ),
        spec(
            "battery_voltage",
            &tr!("pilotes.solarman.tension_batterie"),
            Some(Unit::Volt),
            Semantic::Voltage,
        ),
        spec(
            "battery_current",
            &tr!("pilotes.solarman.courant_batterie"),
            Some(Unit::Ampere),
            Semantic::Current,
        ),
        spec(
            "battery_temperature",
            &tr!("pilotes.solarman.temperature_batterie"),
            Some(Unit::Celsius),
            Semantic::Other,
        ),
        spec(
            "radiator_temperature",
            &tr!("pilotes.solarman.temperature_radiateur"),
            Some(Unit::Celsius),
            Semantic::Other,
        ),
    ];
    // Lifetime counters, but only the production is this device's energy.
    for (key, _, _) in HYBRID_TOTALS {
        let label = match key {
            "load_energy" => tr!("pilotes.solarman.consommation_totale"),
            "energy_import" => tr!("pilotes.solarman.energie_soutiree"),
            "energy_export" => tr!("pilotes.solarman.energie_injectee"),
            "battery_charge_energy" => tr!("pilotes.solarman.energie_chargee"),
            _ => tr!("pilotes.solarman.energie_dechargee"),
        };
        points.push(spec(key, &label, Some(Unit::KiloWattHour), Semantic::Other));
    }
    points
}

impl Map for Zcs {
    fn name(&self) -> &'static str {
        match self.family {
            Family::Tl => "zcs azzurro 1ph tl-v3",
            Family::Hyd => "zcs azzurro hyd-zss-hp",
        }
    }

    fn asks(&self) -> Vec<Ask> {
        match self.family {
            Family::Tl => vec![Ask::holding(0x0000, 0x1D)],
            Family::Hyd => vec![
                Ask::holding(0x0404, 0x1D),
                Ask::holding(0x0484, 0x2C),
                Ask::holding(0x0584, 6),
                Ask::holding(0x0604, 5),
                Ask::holding(0x0684, 0x18),
            ],
        }
    }

    fn device(&self, instance: &InstanceId, id: DeviceId) -> Device {
        use moli_i18n::tr;
        let mut points = common_specs();
        let model = match self.family {
            Family::Tl => {
                points.push(state_spec(&deye::STATES));
                "Azzurro 1PH TL-V3"
            }
            Family::Hyd => {
                points.push(state_spec(&HYBRID_STATES));
                points.extend(hybrid_specs());
                "Azzurro HYD-ZSS-HP"
            }
        };
        for n in 1..=self.inputs() {
            points.extend(input_specs(n));
        }
        Device {
            id,
            instance: instance.clone(),
            native_name: tr!("pilotes.solarman.onduleur").into(),
            manufacturer: Some("ZCS".into()),
            model: Some(model.into()),
            description: None,
            native_room: None,
            members: Vec::new(),
            points,
        }
    }

    fn readings(&mut self, regs: &Regs, now: Instant) -> Vec<(String, Value)> {
        match self.family {
            Family::Tl => self.grid_inverter(regs, now),
            Family::Hyd => self.hybrid(regs),
        }
    }

    fn daily(&self) -> Vec<String> {
        let mut keys = vec!["energy_today".to_owned(), "power".to_owned()];
        for n in 1..=self.inputs() {
            keys.push(format!("pv{n}_power"));
        }
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v5::tests::replay;
    use std::collections::HashMap;

    fn read(name: &str, zcs: &Zcs) -> Regs {
        let mut regs = Regs::default();
        for (ask, words) in replay(name) {
            assert!(zcs.asks().contains(&ask), "{ask:?}");
            regs.push(ask.start, words);
        }
        regs
    }

    fn values(name: &str, family: Family) -> (Zcs, HashMap<String, Value>) {
        let mut zcs = Zcs::new(family);
        let regs = read(name, &zcs);
        let v = zcs.readings(&regs, Instant::now()).into_iter().collect();
        (zcs, v)
    }

    fn check(v: &HashMap<String, Value>, expected: &[(&str, f64)]) {
        for (key, value) in expected {
            assert_eq!(v.get(*key), Some(&Value::Float(*value)), "{key}");
        }
    }

    // Frames built by hand from the register map, never seen on a real
    // inverter: they prove the map is read as written, not that it is right.
    #[test]
    fn a_grid_inverter_frame_built_from_the_map() {
        let (zcs, v) = values("zcs-tl", Family::Tl);
        assert_eq!(v["state"], Value::Text("normal".into()));
        check(
            &v,
            &[
                ("power", 1380.0),
                ("pv1_voltage", 312.4),
                ("pv1_current", 4.52),
                ("pv1_power", 1410.0),
                ("energy_today", 6.37),
                ("energy", 5123.0),
                ("grid_frequency", 50.02),
                ("grid_voltage", 231.8),
                ("grid_current", 5.96),
                ("temperature", 38.0),
            ],
        );
        let instance = InstanceId::from("onduleur");
        let device = zcs.device(&instance, DeviceId::new(&instance, "1"));
        assert_eq!(device.point("energy").unwrap().semantic, Semantic::Energy);
        assert!(device.point("pv1_power").is_some());
        assert!(device.point("pv2_power").is_none());
        assert!(device.point("battery").is_none());
        assert!(device.points.iter().all(|p| !p.access.write));
    }

    #[test]
    fn a_hybrid_frame_built_from_the_map() {
        let (zcs, v) = values("zcs-hyd", Family::Hyd);
        assert_eq!(v["state"], Value::Text("on_grid".into()));
        check(
            &v,
            &[
                ("power", 2180.0),
                ("pv1_voltage", 305.1),
                ("pv1_current", 4.21),
                ("pv1_power", 1280.0),
                ("pv2_power", 900.0),
                ("inverter_power", 1520.0),
                // 350 W out to the grid, 650 W into the battery.
                ("grid_power", -350.0),
                ("battery_power", -650.0),
                ("load_power", 970.0),
                ("grid_frequency", 49.99),
                ("grid_voltage", 233.4),
                ("grid_current", 6.55),
                ("battery", 78.0),
                ("battery_voltage", 210.5),
                ("battery_current", 3.1),
                ("battery_temperature", 24.0),
                ("temperature", 33.0),
                ("radiator_temperature", 39.0),
                ("energy_today", 8.42),
                ("energy", 6234.5),
                ("load_energy", 5120.3),
                ("energy_import", 1830.2),
                ("energy_export", 2944.9),
                ("battery_charge_energy", 1500.4),
                ("battery_discharge_energy", 1322.7),
            ],
        );
        let instance = InstanceId::from("onduleur");
        let device = zcs.device(&instance, DeviceId::new(&instance, "1"));
        // One energy meter and one power: the production.
        for semantic in [Semantic::Energy, Semantic::Power] {
            let n = device
                .points
                .iter()
                .filter(|p| p.semantic == semantic)
                .count();
            assert_eq!(n, 1, "{semantic:?}");
        }
        assert_eq!(device.point("battery").unwrap().semantic, Semantic::Other);
        assert!(device.points.iter().all(|p| !p.access.write));
        for key in v.keys() {
            assert!(device.point(key).is_some(), "{key}");
        }
    }

    #[test]
    fn a_resting_battery_reads_zero_not_minus_zero() {
        let mut zcs = Zcs::new(Family::Hyd);
        let mut regs = Regs::default();
        regs.push(0x0604, vec![2105, 0, 0, 24, 78]);
        let v: HashMap<_, _> = zcs.readings(&regs, Instant::now()).into_iter().collect();
        let Value::Float(power) = v["battery_power"] else {
            panic!()
        };
        assert!(power == 0.0 && power.is_sign_positive());
        // Discharging: the map's negative is Moli's positive.
        let mut regs = Regs::default();
        regs.push(0x0604, vec![2105, 0xFF38, 0xFFD8, 24, 78]);
        let v: HashMap<_, _> = zcs.readings(&regs, Instant::now()).into_iter().collect();
        assert_eq!(v["battery_power"], Value::Float(400.0));
        assert_eq!(v["battery_current"], Value::Float(-2.0));
    }

    #[test]
    fn absurd_values_are_left_out_and_a_counter_never_goes_back() {
        let mut zcs = Zcs::new(Family::Hyd);
        let regs = read("zcs-hyd", &zcs);
        let _ = zcs.readings(&regs, Instant::now());
        let mut regs = Regs::default();
        let mut energy = vec![0u16; 0x18];
        energy[3] = 60_000; // production: 6000.0 kWh, less than before
        energy[0x0B] = 18_310; // import: 0.8 kWh more
        regs.push(0x0684, energy);
        regs.push(0x0604, vec![2105, 310, 65, 24, 250]); // 250 %
        let v: HashMap<_, _> = zcs.readings(&regs, Instant::now()).into_iter().collect();
        assert!(!v.contains_key("energy"));
        assert!(!v.contains_key("battery"));
        assert_eq!(v["energy_import"], Value::Float(1831.0));
        assert_eq!(v["battery_voltage"], Value::Float(210.5));

        // The grid inverter: all zero at dawn gives no total.
        let mut zcs = Zcs::new(Family::Tl);
        let mut regs = Regs::default();
        regs.push(0x0000, vec![0; 0x1D]);
        let v: HashMap<_, _> = zcs.readings(&regs, Instant::now()).into_iter().collect();
        assert!(!v.contains_key("energy"));
        assert_eq!(v["state"], Value::Text("standby".into()));
    }
}
