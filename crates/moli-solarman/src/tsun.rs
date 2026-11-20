//! TSUN micro-inverters behind a Solarman logger: which registers mean what
//! (pure, no I/O). Two families:
//!
//! - **TITAN** (TSOL-MP3000, MP2250): TSUN's own framing instead of Modbus
//!   RTU (see `v5::native_request`), decimal registers 3000, 3600 and 3800,
//!   words low byte first;
//! - **GEN4** (TSOL-MX, the MX3000D among them): plain Modbus holding
//!   registers 0x1100, 0x1200 and 0x1300.
//!
//! Both maps come from tsun-gen3-proxy (BSD-3-Clause, see `NOTICE` and
//! `licenses/tsun-gen3-proxy-BSD-3-Clause.txt`): addresses, scales and units
//! only. **Never read on a real inverter by this driver**: the tests replay
//! frames built by hand from the map.

use std::time::Instant;

use moli_core::{Device, DeviceId, InstanceId, Semantic, Unit, Value};

use crate::deye::Counters;
use crate::regs::{Ask, Map, Regs, common_specs, input_specs, put, round, spec, state_spec};

/// What the inverter is doing (the status register).
pub const STATES: [&str; 3] = ["offline", "on_grid", "off_grid"];

/// The largest of both families (W): the bound while the model is unknown.
const LARGEST_W: f64 = 3300.0;
/// Reads of the identity block before the largest model is assumed.
const IDENTITY_TRIES: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Titan,
    Gen4,
}

/// Where the inverter's own values are.
struct Common {
    state: u16,
    alarm: Option<u16>,
    fault: Option<u16>,
    grid_voltage: u16,
    grid_current: u16,
    grid_frequency: u16,
    temperature: u16,
    rated: u16,
    power: u16,
    today: u16,
    /// High word, low word.
    total: (u16, u16),
}

/// Where the values of one PV input are.
struct Input {
    voltage: u16,
    current: u16,
    power: u16,
    /// High word (if the counter has one), low word.
    today: (Option<u16>, u16),
    total: (u16, u16),
}

impl Family {
    const fn common(self) -> Common {
        match self {
            Self::Titan => Common {
                state: 3000,
                alarm: None,
                fault: None,
                grid_voltage: 3012,
                grid_current: 3013,
                grid_frequency: 3015,
                temperature: 3017,
                rated: 3020,
                power: 3021,
                today: 3022,
                total: (3023, 3024),
            },
            Self::Gen4 => Common {
                state: 0x1100,
                alarm: Some(0x1105),
                fault: Some(0x1106),
                grid_voltage: 0x1200,
                grid_current: 0x1201,
                grid_frequency: 0x1209,
                temperature: 0x1218,
                rated: 0x1210,
                power: 0x1202,
                today: 0x1212,
                total: (0x1213, 0x1214),
            },
        }
    }

    /// PV input `n` (1 to 6).
    fn input(self, n: u8) -> Input {
        match self {
            // Inputs 1 to 3 in block 3600, 4 to 6 in block 3800: seven
            // registers each, then the three lifetime counters.
            Self::Titan => {
                let (block, i) = if n <= 3 {
                    (3600, u16::from(n - 1))
                } else {
                    (3800, u16::from(n - 4))
                };
                let base = block + 7 * i;
                Input {
                    voltage: base,
                    current: base + 1,
                    power: base + 2,
                    today: (None, base + 3),
                    total: (block + 24 + 2 * i, block + 25 + 2 * i),
                }
            }
            Self::Gen4 => {
                let base = 0x1302 + 7 * u16::from(n - 1);
                Input {
                    voltage: base,
                    current: base + 1,
                    power: base + 2,
                    today: (Some(base + 3), base + 4),
                    total: (base + 5, base + 6),
                }
            }
        }
    }

    /// The identity block, and where the largest power the inverter was
    /// built for is in it.
    const fn identity(self) -> (Ask, usize) {
        match self {
            Self::Titan => (Ask::native(0xA1, 0x21, 2000, 96), 42),
            Self::Gen4 => (Ask::holding(0x1400, 0x50), 0x37),
        }
    }
}

/// PV inputs of an inverter built for `watts`: 6 from 2400 W, 4 from
/// 1600 W, 2 from 600 W, 1 below.
fn inputs_of(watts: f64) -> u8 {
    if watts >= 2400.0 {
        6
    } else if watts >= 1600.0 {
        4
    } else if watts >= 600.0 {
        2
    } else {
        1
    }
}

/// One TSUN micro-inverter.
#[derive(Debug)]
pub struct Tsun {
    family: Family,
    /// The largest power the inverter was built for (W), once it said it.
    built_for: Option<f64>,
    /// Failed reads of the identity block.
    tries: u8,
    counters: Counters,
}

impl Tsun {
    #[must_use]
    pub fn new(family: Family) -> Self {
        Self {
            family,
            built_for: None,
            tries: 0,
            counters: Counters::default(),
        }
    }

    /// PV inputs: what the model has, or six while it is unknown (both
    /// families' largest).
    fn inputs(&self) -> u8 {
        self.built_for.map_or(6, inputs_of)
    }
}

impl Map for Tsun {
    fn name(&self) -> &'static str {
        match self.family {
            Family::Titan => "tsun titan",
            Family::Gen4 => "tsun gen4",
        }
    }

    fn asks(&self) -> Vec<Ask> {
        match self.family {
            Family::Titan => {
                let mut asks = vec![
                    Ask::native(0xA1, 0x01, 3000, 32),
                    Ask::native(0xA3, 0x03, 3600, 32),
                ];
                if self.inputs() > 3 {
                    asks.push(Ask::native(0xA4, 0x04, 3800, 32));
                }
                asks
            }
            Family::Gen4 => vec![
                Ask::holding(0x1100, 0x10),
                Ask::holding(0x1200, 0x30),
                Ask::holding(0x1300, 0x40),
            ],
        }
    }

    fn identity(&self) -> Option<Ask> {
        (self.built_for.is_none() && self.tries < IDENTITY_TRIES).then(|| self.family.identity().0)
    }

    fn learn(&mut self, regs: Option<&[u16]>) -> bool {
        let before = self.inputs();
        let watts = regs
            .and_then(|r| r.get(self.family.identity().1))
            .map(|w| f64::from(*w))
            .filter(|w| (100.0..=5000.0).contains(w));
        match watts {
            Some(watts) => self.built_for = Some(watts),
            None => self.tries += 1,
        }
        self.inputs() != before
    }

    fn device(&self, instance: &InstanceId, id: DeviceId) -> Device {
        use moli_i18n::tr;
        let mut points = common_specs();
        points.extend([
            state_spec(&STATES),
            spec(
                "rated_power",
                &tr!("pilotes.solarman.puissance_nominale"),
                Some(Unit::Watt),
                Semantic::Other,
            ),
        ]);
        if self.family == Family::Gen4 {
            points.push(spec(
                "alarm_code",
                &tr!("pilotes.solarman.code_alarme"),
                None,
                Semantic::Other,
            ));
            points.push(spec(
                "fault_code",
                &tr!("pilotes.solarman.code_defaut"),
                None,
                Semantic::Other,
            ));
        }
        for n in 1..=self.inputs() {
            let label = |what: &str| tr!("pilotes.solarman.entree", what = what, n = n);
            points.extend(input_specs(n));
            points.push(spec(
                &format!("pv{n}_energy_today"),
                &label(&tr!("pilotes.solarman.production_jour")),
                Some(Unit::KiloWattHour),
                Semantic::Other,
            ));
            points.push(spec(
                &format!("pv{n}_energy"),
                &label(&tr!("pilotes.solarman.production_totale")),
                Some(Unit::KiloWattHour),
                Semantic::Other,
            ));
        }
        let series = match self.family {
            Family::Titan => "TITAN",
            Family::Gen4 => "GEN4",
        };
        Device {
            id,
            instance: instance.clone(),
            native_name: tr!("pilotes.solarman.onduleur").into(),
            manufacturer: Some("TSUN".into()),
            model: Some(
                format!(
                    "{series} · {}",
                    tr!("pilotes.solarman.modele", n = self.inputs())
                )
                .into(),
            ),
            description: None,
            native_room: None,
            members: Vec::new(),
            points,
        }
    }

    fn readings(&mut self, regs: &Regs, _now: Instant) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        let c = self.family.common();
        let largest = self.built_for.unwrap_or(LARGEST_W);
        // What the inverter can make in a day, flat out.
        let day = (0.0, largest / 1000.0 * 24.0);
        let scaled =
            |addr: u16, by: f64, decimals: i32| regs.num(addr).map(|v| round(v / by, decimals));

        put(
            &mut out,
            "power",
            scaled(c.power, 10.0, 1),
            (0.0, largest * 1.2),
        );
        put(&mut out, "energy_today", scaled(c.today, 100.0, 2), day);
        put(
            &mut out,
            "grid_voltage",
            scaled(c.grid_voltage, 10.0, 1),
            (0.0, 300.0),
        );
        put(
            &mut out,
            "grid_current",
            scaled(c.grid_current, 100.0, 2),
            (0.0, 50.0),
        );
        put(
            &mut out,
            "grid_frequency",
            scaled(c.grid_frequency, 100.0, 2),
            (40.0, 70.0),
        );
        // Degrees above -40; a register at 0 is no measure.
        put(
            &mut out,
            "temperature",
            regs.num(c.temperature).map(|v| v - 40.0),
            (-39.0, 120.0),
        );
        put(&mut out, "rated_power", regs.num(c.rated), (1.0, 5000.0));
        for (key, addr) in [("alarm_code", c.alarm), ("fault_code", c.fault)] {
            if let Some(addr) = addr {
                put(&mut out, key, regs.num(addr), (0.0, 65_535.0));
            }
        }
        for n in 1..=self.inputs() {
            let i = self.family.input(n);
            put(
                &mut out,
                &format!("pv{n}_voltage"),
                scaled(i.voltage, 10.0, 1),
                (0.0, 100.0),
            );
            put(
                &mut out,
                &format!("pv{n}_current"),
                scaled(i.current, 100.0, 2),
                (0.0, 30.0),
            );
            put(
                &mut out,
                &format!("pv{n}_power"),
                scaled(i.power, 10.0, 1),
                (0.0, largest),
            );
            let today = match i.today {
                (Some(high), low) => regs.wide(high, low),
                (None, low) => regs.num(low),
            };
            put(
                &mut out,
                &format!("pv{n}_energy_today"),
                today.map(|v| round(v / 100.0, 2)),
                day,
            );
            let key = format!("pv{n}_energy");
            if let Some(total) = regs
                .wide(i.total.0, i.total.1)
                .map(|v| round(v / 100.0, 2))
                .filter(|&t| self.counters.accept(&key, t))
            {
                out.push((key, Value::Float(total)));
            }
        }
        if let Some(total) = regs
            .wide(c.total.0, c.total.1)
            .map(|v| round(v / 100.0, 2))
            .filter(|&t| self.counters.accept("energy", t))
        {
            out.push(("energy".into(), Value::Float(total)));
        }
        if let Some(state) = regs.at(c.state).and_then(|s| STATES.get(usize::from(s))) {
            out.push(("state".into(), Value::Text((*state).into())));
        }
        out
    }

    fn daily(&self) -> Vec<String> {
        let mut keys = vec!["energy_today".to_owned(), "power".to_owned()];
        for n in 1..=self.inputs() {
            keys.push(format!("pv{n}_energy_today"));
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

    /// The fixture's blocks: the identity block apart from the others.
    fn read(name: &str, tsun: &mut Tsun) -> Regs {
        let identity = tsun.family.identity().0;
        let mut regs = Regs::default();
        for (ask, words) in replay(name) {
            if ask == identity {
                assert!(!tsun.learn(Some(&words)), "six inputs, as assumed");
            } else {
                assert!(tsun.asks().contains(&ask), "{ask:?}");
                regs.push(ask.start, words);
            }
        }
        regs
    }

    fn values(name: &str, family: Family) -> (Tsun, HashMap<String, Value>) {
        let mut tsun = Tsun::new(family);
        let regs = read(name, &mut tsun);
        let v = tsun.readings(&regs, Instant::now()).into_iter().collect();
        (tsun, v)
    }

    fn check(v: &HashMap<String, Value>, expected: &[(&str, f64)]) {
        for (key, value) in expected {
            assert_eq!(v.get(*key), Some(&Value::Float(*value)), "{key}");
        }
    }

    // Frames built by hand from the register map, never seen on a real
    // inverter: they prove the map is read as written, not that it is right.
    #[test]
    fn a_titan_frame_built_from_the_map() {
        let (tsun, v) = values("tsun-titan", Family::Titan);
        assert_eq!(tsun.built_for, Some(3000.0));
        assert_eq!(tsun.identity(), None, "asked once");
        assert_eq!(v["state"], Value::Text("on_grid".into()));
        check(
            &v,
            &[
                ("power", 1218.6),
                ("energy_today", 5.12),
                ("energy", 3817.25),
                ("grid_voltage", 236.4),
                ("grid_current", 5.21),
                ("grid_frequency", 50.01),
                ("temperature", 46.0),
                ("rated_power", 3000.0),
                ("pv1_voltage", 36.1),
                ("pv1_current", 5.88),
                ("pv1_power", 212.2),
                ("pv1_energy_today", 0.88),
                ("pv1_energy", 640.1),
                ("pv3_energy", 631.05),
                ("pv4_voltage", 35.2),
                ("pv6_power", 198.1),
                ("pv6_energy", 625.4),
            ],
        );
        assert!(!v.contains_key("alarm_code"));
        let instance = InstanceId::from("onduleur");
        let device = tsun.device(&instance, DeviceId::new(&instance, "1"));
        assert_eq!(device.point("energy").unwrap().semantic, Semantic::Energy);
        assert_eq!(
            device.point("energy_today").unwrap().semantic,
            Semantic::Other
        );
        assert!(device.point("pv6_power").is_some());
        assert!(device.points.iter().all(|p| !p.access.write));
    }

    /// The one answer of a real TITAN this crate has: the inverter's part
    /// of a frame published in tsun-gen3-proxy's tests (BSD-3-Clause),
    /// registers 3000 to 3031. Read by that project, never by this driver.
    #[test]
    fn a_titan_answer_published_by_tsun_gen3_proxy() {
        let body = crate::v5::tests::hex(
            concat!(
                "7ea18101",
                "0bb8",
                "0040",
                "010000 4c0000 000000 000000 000000 00",
                "6e11 0300 1910 0300 2909 4500 2900 8613",
                "0200 5e00 0000 1910 b80b 8c08 8e01 0100",
                "71e8 0000 e880 c605 5c00 0000 0000 0000",
                "fd6e",
            )
            .replace(' ', "")
            .as_str(),
        );
        let frame = crate::v5::tests::wrapped(7, 1, &body);
        let crate::v5::Frame::Registers(words, _) =
            crate::v5::native_parse(&frame, 7, 1, 0xA1, 3000, 32).unwrap()
        else {
            panic!()
        };
        let mut regs = Regs::default();
        regs.push(3000, words);
        let v: HashMap<_, _> = Tsun::new(Family::Titan)
            .readings(&regs, Instant::now())
            .into_iter()
            .collect();
        assert_eq!(v["state"], Value::Text("on_grid".into()));
        check(
            &v,
            &[
                ("grid_voltage", 234.5),
                ("grid_current", 0.69),
                ("grid_frequency", 49.98),
                ("temperature", 54.0),
                ("rated_power", 3000.0),
                ("power", 218.8),
                ("energy_today", 3.98),
                ("energy", 1250.41),
            ],
        );
    }

    #[test]
    fn a_gen4_frame_built_from_the_map() {
        let (tsun, v) = values("tsun-gen4", Family::Gen4);
        assert_eq!(tsun.built_for, Some(3000.0));
        assert_eq!(v["state"], Value::Text("on_grid".into()));
        check(
            &v,
            &[
                ("power", 486.3),
                ("energy_today", 2.41),
                ("energy", 402.18),
                ("grid_voltage", 238.7),
                ("grid_current", 2.05),
                ("grid_frequency", 49.97),
                ("temperature", 35.0),
                ("rated_power", 3000.0),
                ("alarm_code", 0.0),
                ("fault_code", 0.0),
                ("pv1_voltage", 34.2),
                ("pv1_current", 2.41),
                ("pv1_power", 82.4),
                ("pv1_energy_today", 0.41),
                ("pv1_energy", 68.1),
                ("pv6_power", 79.5),
                ("pv6_energy", 66.9),
            ],
        );
    }

    #[test]
    fn the_model_says_how_many_inputs() {
        let mut tsun = Tsun::new(Family::Titan);
        assert_eq!(tsun.inputs(), 6);
        assert_eq!(tsun.asks().len(), 3);
        let mut identity = vec![0u16; 96];
        identity[42] = 800;
        assert!(tsun.learn(Some(&identity)), "the device changed");
        assert_eq!(tsun.inputs(), 2);
        assert_eq!(tsun.asks().len(), 2, "no need for inputs 4 to 6");
        assert_eq!(
            tsun.daily(),
            [
                "energy_today",
                "power",
                "pv1_energy_today",
                "pv1_power",
                "pv2_energy_today",
                "pv2_power"
            ]
        );
        for (watts, inputs) in [(3300.0, 6), (2250.0, 4), (1000.0, 2), (450.0, 1)] {
            assert_eq!(inputs_of(watts), inputs, "{watts}");
        }
        // An identity block that cannot be read: asked three times, then
        // the largest model is assumed for good.
        let mut tsun = Tsun::new(Family::Gen4);
        for _ in 0..3 {
            assert!(tsun.identity().is_some());
            assert!(!tsun.learn(None));
        }
        assert_eq!(tsun.identity(), None);
        assert_eq!(tsun.inputs(), 6);
        // Nor one that reads nonsense.
        let mut tsun = Tsun::new(Family::Gen4);
        assert!(!tsun.learn(Some(&[0; 0x50])));
        assert_eq!(tsun.built_for, None);
    }

    #[test]
    fn absurd_values_are_left_out_and_a_counter_never_goes_back() {
        let mut tsun = Tsun::new(Family::Gen4);
        let mut regs = Regs::default();
        let mut grid = vec![0u16; 0x30];
        grid[0x00] = 2387;
        grid[0x02] = 60_000; // 6 kW from a micro-inverter
        grid[0x09] = 4997;
        grid[0x14] = 40_218;
        regs.push(0x1100, vec![1; 0x10]);
        regs.push(0x1200, grid.clone());
        let v: HashMap<_, _> = tsun.readings(&regs, Instant::now()).into_iter().collect();
        assert!(!v.contains_key("power"));
        assert!(!v.contains_key("temperature"), "0 is no measure");
        assert_eq!(v["grid_voltage"], Value::Float(238.7));
        assert_eq!(v["energy"], Value::Float(402.18));
        assert!(!v.contains_key("pv1_voltage"), "block not read");

        // A lower total, then one far ahead: misreads.
        for misread in [40_000, 60_000] {
            grid[0x14] = misread;
            let mut regs = Regs::default();
            regs.push(0x1200, grid.clone());
            let v: HashMap<_, _> = tsun.readings(&regs, Instant::now()).into_iter().collect();
            assert!(!v.contains_key("energy"), "{misread}");
        }
        // A status the map does not know is not a state.
        let mut regs = Regs::default();
        regs.push(0x1100, vec![9; 0x10]);
        let v: HashMap<_, _> = tsun.readings(&regs, Instant::now()).into_iter().collect();
        assert!(!v.contains_key("state"));
    }
}
