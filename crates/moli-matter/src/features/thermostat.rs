//! Thermostat 0x0201, based on CHIP thermostat-cluster.xml (Matter 1.4.2).
//! SetpointRaiseLower is relative, so absolute setpoints use attribute writes.
use super::{Cluster, Plan, Report, attribute, uint};
use matter_controller::{AttributePath, ReadPath, Value as Tlv};
use moli_core::{Access, Kind, PointSpec, Semantic, Unit, Value};

pub(crate) const CLUSTERS: &[u32] = &[0x201];
pub(crate) const DISCOVERY: &[(u32, u32)] = &[
    (0x201, 0),
    (0x201, 3),
    (0x201, 4),
    (0x201, 5),
    (0x201, 6),
    (0x201, 0x11),
    (0x201, 0x12),
    (0x201, 0x15),
    (0x201, 0x16),
    (0x201, 0x17),
    (0x201, 0x18),
    (0x201, 0x19),
    (0x201, 0x1b),
    (0x201, 0x1c),
];
pub(crate) const METADATA: &[(u32, u32)] = &[
    (0x201, 3),
    (0x201, 4),
    (0x201, 5),
    (0x201, 6),
    (0x201, 0x15),
    (0x201, 0x16),
    (0x201, 0x17),
    (0x201, 0x18),
    (0x201, 0x19),
    (0x201, 0x1b),
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Thermostat {
    endpoint: u16,
    cluster: Cluster,
    sequence: Option<u64>,
    heat_limits: Option<(i32, i32)>,
    cool_limits: Option<(i32, i32)>,
    deadband: Option<i32>,
    heat: Option<i32>,
    cool: Option<i32>,
    mode: Option<u64>,
}

fn temperature(value: &Tlv) -> Option<i32> {
    let Tlv::Int(n) = value else { return None };
    (-27315..=32767)
        .contains(n)
        .then(|| i32::try_from(*n).ok())
        .flatten()
}

fn reading(value: Option<i32>) -> Value {
    value.map_or(Value::Null, |n| Value::Float(f64::from(n) / 100.0))
}

impl Thermostat {
    pub(crate) fn discover(ep: u16, report: &Report) -> Option<Self> {
        let cluster = Cluster::discover(ep, 0x201, report)?;
        let get = |id| {
            cluster
                .attributes
                .contains(&id)
                .then(|| attribute(report, ep, 0x201, id))
                .flatten()
        };
        // No guessed default limits: missing or invalid optional policy leaves readings usable.
        let limits = |ids: [u32; 4]| -> Option<(i32, i32)> {
            let [a, b, c, d] = ids.map(|id| get(id).and_then(temperature));
            let (a, b, c, d) = (a?, b?, c?, d?);
            let (lo, hi) = (a.max(c), b.min(d));
            (a <= b && c <= d && lo <= hi).then_some((lo, hi))
        };
        let result = Self {
            endpoint: ep,
            sequence: get(0x1b).and_then(uint).filter(|n| *n <= 5),
            heat_limits: limits([3, 4, 0x15, 0x16]),
            cool_limits: limits([5, 6, 0x17, 0x18]),
            deadband: get(0x19).and_then(|v| {
                if let Tlv::Int(n) = v {
                    (0..=127)
                        .contains(n)
                        .then(|| i32::try_from(*n).ok())
                        .flatten()
                        .map(|n| n * 10)
                } else {
                    None
                }
            }),
            heat: get(0x12).and_then(temperature),
            cool: get(0x11).and_then(temperature),
            mode: get(0x1c).and_then(uint),
            cluster,
        };
        (!result.points().is_empty()).then_some(result)
    }

    fn has(&self, id: u32) -> bool {
        self.cluster.attributes.contains(&id)
    }
    fn heat_supported(&self) -> bool {
        self.cluster.features & 1 != 0
    }
    fn cool_supported(&self) -> bool {
        self.cluster.features & 2 != 0
    }
    fn dual(&self) -> bool {
        self.heat_supported() && self.cool_supported()
    }
    fn key(&self, heat: bool) -> &'static str {
        if !self.dual() {
            "target_temperature"
        } else if heat {
            "heat_setpoint"
        } else {
            "cool_setpoint"
        }
    }
    fn policy(&self, heat: bool) -> bool {
        self.sequence.is_some_and(|n| {
            if heat {
                matches!(n, 2..=5)
            } else {
                matches!(n, 0 | 1 | 4 | 5)
            }
        })
    }
    fn modes(&self) -> Vec<(&'static str, u64)> {
        let mut modes = Vec::new();
        if self.sequence.is_none() {
            return modes;
        }
        modes.push(("off", 0));
        if self.heat_supported() && self.policy(true) {
            modes.push(("heat", 4));
        }
        if self.cool_supported() && self.policy(false) {
            modes.push(("cool", 3));
        }
        if self.dual()
            && self.policy(true)
            && self.policy(false)
            && self.cluster.features & 0x20 != 0
        {
            modes.push(("auto", 1));
        }
        modes
    }
    fn writable(&self, heat: bool) -> bool {
        self.policy(heat)
            && self.has(0x1c)
            && self
                .mode
                .is_some_and(|mode| self.modes().iter().any(|(_, n)| *n == mode))
            && (if heat {
                self.heat_limits
            } else {
                self.cool_limits
            })
            .is_some()
            && (!self.dual()
                || (self.deadband.is_some()
                    && (if heat { self.cool } else { self.heat }).is_some()))
    }

    pub(crate) fn points(&self) -> Vec<PointSpec> {
        let mut points = Vec::new();
        if self.has(0) {
            points.push(numeric(
                "temperature",
                moli_i18n::tr!("pilotes.matter.temperature"),
                false,
                None,
            ));
        }
        for (heat, supported, id) in [
            (true, self.heat_supported(), 0x12),
            (false, self.cool_supported(), 0x11),
        ] {
            if !supported || !self.has(id) {
                continue;
            }
            let key = self.key(heat);
            let label = match key {
                "heat_setpoint" => moli_i18n::tr!("pilotes.matter.heat_setpoint"),
                "cool_setpoint" => moli_i18n::tr!("pilotes.matter.cool_setpoint"),
                _ => moli_i18n::tr!("pilotes.matter.target_temperature"),
            };
            points.push(numeric(
                key,
                label,
                self.writable(heat),
                if heat {
                    self.heat_limits
                } else {
                    self.cool_limits
                },
            ));
        }
        if self.has(0x1c) {
            let modes = self.modes();
            points.push(PointSpec {
                key: "mode".into(),
                label: moli_i18n::tr!("pilotes.matter.mode").into(),
                kind: Kind::Enum {
                    values: modes.iter().map(|(name, _)| (*name).into()).collect(),
                },
                access: Access {
                    read: true,
                    write: !modes.is_empty(),
                },
                unit: None,
                semantic: Semantic::Other,
            });
        }
        points
    }

    pub(crate) fn paths(&self, ep: u16) -> Vec<ReadPath> {
        [0, 0x11, 0x12, 0x1c]
            .into_iter()
            .filter(|id| {
                self.has(*id)
                    && (*id != 0x11 || self.cool_supported())
                    && (*id != 0x12 || self.heat_supported())
            })
            .map(|id| ReadPath::concrete(ep, 0x201, id))
            .collect()
    }

    pub(crate) fn update(
        &mut self,
        path: AttributePath,
        value: &Tlv,
    ) -> Vec<(&'static str, Value)> {
        if path.endpoint != self.endpoint || path.cluster != 0x201 || !self.has(path.attribute) {
            return vec![];
        }
        let (key, value) = match path.attribute {
            0 => ("temperature", reading(temperature(value))),
            0x12 if self.heat_supported() => {
                self.heat = temperature(value);
                (self.key(true), reading(self.heat))
            }
            0x11 if self.cool_supported() => {
                self.cool = temperature(value);
                (self.key(false), reading(self.cool))
            }
            0x1c => {
                self.mode = uint(value);
                (
                    "mode",
                    self.modes()
                        .iter()
                        .find(|(_, n)| Some(*n) == self.mode)
                        .map_or(Value::Null, |(name, _)| Value::from(*name)),
                )
            }
            _ => return vec![],
        };
        vec![(key, value)]
    }

    pub(crate) fn plan(&self, key: &str, value: &Value) -> Option<Plan> {
        if key == "mode" && self.has(0x1c) {
            let Value::Text(name) = value else {
                return None;
            };
            let (_, mode) = self
                .modes()
                .into_iter()
                .find(|(s, _)| *s == name.as_ref())?;
            return Some(Plan::Write {
                cluster: 0x201,
                attribute: 0x1c,
                value: Tlv::Uint(mode),
            });
        }
        let heat = if key == self.key(true) && self.heat_supported() {
            true
        } else if key == self.key(false) && self.cool_supported() {
            false
        } else {
            return None;
        };
        let id = if heat { 0x12 } else { 0x11 };
        if !self.has(id) || !self.writable(heat) {
            return None;
        }
        let n = value.as_f64()?;
        let scaled = n * 100.0;
        let (min, max) = (if heat {
            self.heat_limits
        } else {
            self.cool_limits
        })?;
        if !n.is_finite()
            || scaled < f64::from(min)
            || scaled > f64::from(max)
            || (scaled - scaled.round()).abs() > 1e-7
        {
            return None;
        }
        #[allow(clippy::cast_possible_truncation)]
        let target = scaled.round() as i32;
        if self.dual() {
            let gap = self.deadband?;
            if (heat && target + gap > self.cool?) || (!heat && target - gap < self.heat?) {
                return None;
            }
        }
        Some(Plan::Write {
            cluster: 0x201,
            attribute: id,
            value: Tlv::Int(i64::from(target)),
        })
    }
}

fn numeric(key: &'static str, label: String, write: bool, limits: Option<(i32, i32)>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Numeric {
            min: limits.map(|(n, _)| f64::from(n) / 100.0),
            max: limits.map(|(_, n)| f64::from(n) / 100.0),
            step: Some(0.01),
        },
        access: Access { read: true, write },
        unit: Some(Unit::Celsius),
        semantic: Semantic::Temperature,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(id: u32) -> AttributePath {
        AttributePath {
            endpoint: 1,
            cluster: 0x201,
            attribute: id,
        }
    }
    fn report(features: u64) -> Vec<(AttributePath, Tlv)> {
        let mut r = vec![
            (path(0xfffc), Tlv::Uint(features)),
            (path(0xfffd), Tlv::Uint(7)),
            (path(0xfff9), Tlv::Array(vec![])),
            (
                path(0xfffb),
                Tlv::Array(
                    DISCOVERY
                        .iter()
                        .map(|(_, a)| Tlv::Uint(u64::from(*a)))
                        .collect(),
                ),
            ),
            (
                path(0x1b),
                Tlv::Uint(if features & 3 == 1 {
                    2
                } else if features & 3 == 2 {
                    0
                } else {
                    4
                }),
            ),
            (path(0x1c), Tlv::Uint(0)),
        ];
        for (id, n) in [
            (3, 700),
            (4, 3000),
            (5, 1600),
            (6, 3200),
            (0x15, 1000),
            (0x16, 2800),
            (0x17, 1800),
            (0x18, 3000),
            (0x19, 25),
            (0x12, 2000),
            (0x11, 2600),
        ] {
            r.push((path(id), Tlv::Int(n)));
        }
        r
    }
    #[test]
    fn thermostat_absolute_limits_and_representation() {
        let t = Thermostat::discover(1, &report(1)).unwrap();
        assert!(
            t.points()
                .iter()
                .any(|p| p.key.as_ref() == "target_temperature" && p.access.write)
        );
        assert_eq!(
            t.plan("target_temperature", &Value::Float(21.25)),
            Some(Plan::Write {
                cluster: 0x201,
                attribute: 0x12,
                value: Tlv::Int(2125)
            })
        );
        for n in [f64::NAN, f64::INFINITY, 9.99, 28.01, 21.001, 1e20] {
            assert!(t.plan("target_temperature", &Value::Float(n)).is_none());
        }
        let t = Thermostat::discover(1, &report(2)).unwrap();
        assert!(matches!(
            t.plan("target_temperature", &Value::Int(23)),
            Some(Plan::Write {
                attribute: 0x11,
                ..
            })
        ));
    }
    #[test]
    fn thermostat_dual_deadband_cache_and_modes() {
        let mut t = Thermostat::discover(1, &report(35)).unwrap();
        assert!(t.plan("target_temperature", &Value::Int(21)).is_none());
        assert!(t.plan("heat_setpoint", &Value::Float(23.5)).is_some());
        assert!(t.plan("heat_setpoint", &Value::Float(23.51)).is_none());
        assert!(t.plan("cool_setpoint", &Value::Float(22.5)).is_some());
        assert!(t.plan("cool_setpoint", &Value::Float(22.49)).is_none());
        assert!(t.plan("mode", &Value::from("auto")).is_some());
        assert!(t.plan("mode", &Value::from("fan_only")).is_none());
        t.update(path(0x1c), &Tlv::Uint(3));
        assert!(matches!(
            t.plan("heat_setpoint", &Value::Int(21)),
            Some(Plan::Write {
                attribute: 0x12,
                ..
            })
        ));
        assert_eq!(
            t.update(path(0x11), &Tlv::Null),
            vec![("cool_setpoint", Value::Null)]
        );
        assert!(t.plan("heat_setpoint", &Value::Int(21)).is_none());
        t.update(path(0x1c), &Tlv::Uint(99));
        assert!(t.plan("cool_setpoint", &Value::Int(25)).is_none());
        let mut r = report(35);
        r.retain(|(p, _)| p.attribute != 0x19);
        let t = Thermostat::discover(1, &r).unwrap();
        assert!(t.plan("heat_setpoint", &Value::Int(21)).is_none());
        let t = Thermostat::discover(1, &report(3)).unwrap();
        assert!(t.plan("mode", &Value::from("auto")).is_none());
    }
    #[test]
    fn thermostat_missing_policy_and_invalid_samples() {
        for id in [0xfffc, 0xfffb, 0xfff9, 0xfffd] {
            let mut r = report(1);
            r.retain(|(p, _)| p.attribute != id);
            assert!(Thermostat::discover(1, &r).is_none());
        }
        for id in [3, 4, 0x15, 0x16, 0x1b, 0x1c] {
            let mut r = report(1);
            r.retain(|(p, _)| p.attribute != id);
            let mut t = Thermostat::discover(1, &r).unwrap();
            assert!(t.plan("target_temperature", &Value::Int(21)).is_none());
            for v in [
                Tlv::Null,
                Tlv::Int(-32768),
                Tlv::Int(-27316),
                Tlv::Int(32768),
                Tlv::Uint(2000),
            ] {
                assert_eq!(t.update(path(0), &v), vec![("temperature", Value::Null)]);
            }
            assert_eq!(
                t.update(path(0), &Tlv::Int(-123)),
                vec![("temperature", Value::Float(-1.23))]
            );
        }
        let mut r = report(1);
        r.iter_mut().find(|(p, _)| p.attribute == 0x16).unwrap().1 = Tlv::Int(800);
        assert!(
            Thermostat::discover(1, &r)
                .unwrap()
                .plan("target_temperature", &Value::Int(21))
                .is_none()
        );
    }
}
