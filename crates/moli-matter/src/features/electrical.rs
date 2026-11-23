//! Electrical Power Measurement 0x0090 and Electrical Energy Measurement 0x0091,
//! based on CHIP ElectricalPowerMeasurement.xml / ElectricalEnergyMeasurement.xml
//! (Matter 1.4.2). Units are fixed by the specification (mV, mA, mW, mVA, mWh):
//! there is no multiplier or divisor to discover, unlike Zigbee 0x0B04.
//! Read-only: a point exists only for an attribute the endpoint lists.
use super::{Cluster, Report};
use matter_codec::Tag;
use matter_controller::{AttributePath, ReadPath, Value as Tlv};
use moli_core::{Access, Kind, PointSpec, Semantic, Unit, Value};

const POWER: u32 = 0x90;
const ENERGY: u32 = 0x91;
pub(crate) const CLUSTERS: &[u32] = &[POWER, ENERGY];
// FeatureMap and AttributeList are enough: no extra attribute gates a point.
pub(crate) const DISCOVERY: &[(u32, u32)] = &[];
pub(crate) const METADATA: &[(u32, u32)] = &[];

/// ElectricalEnergyMeasurement FeatureMap bits.
const IMPORTED: u32 = 1 << 0;
const EXPORTED: u32 = 1 << 1;
const CUMULATIVE: u32 = 1 << 2;

/// A spec value in thousandths (mV, mA, mW, mVA, mWh) and its Moli point.
struct Measure {
    cluster: u32,
    attribute: u32,
    key: &'static str,
    label: &'static str,
    unit: Unit,
    semantic: Semantic,
    /// Feature bits the energy cluster must announce (0 for power).
    features: u32,
    /// Divisor from the spec unit to the point unit.
    divisor: f64,
    /// Imported/exported totals are non-negative by constraint.
    signed: bool,
}

const MEASURES: &[Measure] = &[
    Measure {
        cluster: POWER,
        attribute: 0x08,
        key: "power",
        label: "pilotes.matter.power",
        unit: Unit::Watt,
        semantic: Semantic::Power,
        features: 0,
        divisor: 1e3,
        signed: true,
    },
    Measure {
        cluster: POWER,
        attribute: 0x0a,
        key: "apparent_power",
        label: "pilotes.matter.apparent_power",
        unit: Unit::VoltAmpere,
        semantic: Semantic::ApparentPower,
        features: 0,
        divisor: 1e3,
        signed: true,
    },
    Measure {
        cluster: POWER,
        attribute: 0x04,
        key: "voltage",
        label: "pilotes.matter.voltage",
        unit: Unit::Volt,
        semantic: Semantic::Voltage,
        features: 0,
        divisor: 1e3,
        signed: true,
    },
    Measure {
        cluster: POWER,
        attribute: 0x05,
        key: "current",
        label: "pilotes.matter.current",
        unit: Unit::Ampere,
        semantic: Semantic::Current,
        features: 0,
        divisor: 1e3,
        signed: true,
    },
    // Only cumulative totals are meters: periodic energy restarts every period
    // and would read as a reset to `moli-energy`, so it is never exposed.
    Measure {
        cluster: ENERGY,
        attribute: 0x01,
        key: "energy",
        label: "pilotes.matter.energy",
        unit: Unit::KiloWattHour,
        semantic: Semantic::Energy,
        features: IMPORTED | CUMULATIVE,
        divisor: 1e6,
        signed: false,
    },
    Measure {
        cluster: ENERGY,
        attribute: 0x02,
        key: "energy_returned",
        label: "pilotes.matter.energy_returned",
        unit: Unit::KiloWattHour,
        semantic: Semantic::Energy,
        features: EXPORTED | CUMULATIVE,
        divisor: 1e6,
        signed: false,
    },
];

/// Above 2^53 an `f64` no longer holds every integer: such a reading is not trusted.
const EXACT: i64 = 1 << 53;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Electrical {
    endpoint: u16,
    power: Option<Cluster>,
    energy: Option<Cluster>,
}

impl Electrical {
    pub(crate) fn discover(ep: u16, report: &Report) -> Option<Self> {
        let result = Self {
            endpoint: ep,
            power: Cluster::discover(ep, POWER, report),
            energy: Cluster::discover(ep, ENERGY, report),
        };
        (!result.points().is_empty()).then_some(result)
    }

    fn active(&self) -> impl Iterator<Item = &'static Measure> + '_ {
        MEASURES.iter().filter(|m| {
            let cluster = if m.cluster == POWER {
                &self.power
            } else {
                &self.energy
            };
            cluster.as_ref().is_some_and(|c| {
                c.attributes.contains(&m.attribute) && c.features & m.features == m.features
            })
        })
    }

    pub(crate) fn points(&self) -> Vec<PointSpec> {
        self.active()
            .map(|m| PointSpec {
                key: m.key.into(),
                label: moli_i18n::tr!(m.label).into(),
                kind: Kind::Numeric {
                    min: (!m.signed).then_some(0.0),
                    max: None,
                    step: None,
                },
                access: Access {
                    read: true,
                    write: false,
                },
                unit: Some(m.unit.clone()),
                semantic: m.semantic,
            })
            .collect()
    }

    pub(crate) fn paths(&self, ep: u16) -> Vec<ReadPath> {
        self.active()
            .map(|m| ReadPath::concrete(ep, m.cluster, m.attribute))
            .collect()
    }

    pub(crate) fn update(
        &mut self,
        path: AttributePath,
        value: &Tlv,
    ) -> Vec<(&'static str, Value)> {
        if path.endpoint != self.endpoint {
            return Vec::new();
        }
        self.active()
            .find(|m| m.cluster == path.cluster && m.attribute == path.attribute)
            .map(|m| {
                // Energy comes as EnergyMeasurementStruct, its total in field 0.
                let raw = if m.cluster == ENERGY {
                    field(value, 0).and_then(int)
                } else {
                    int(value)
                };
                let reading = raw
                    .filter(|n| m.signed || *n >= 0)
                    .and_then(|n| scaled(n, m.divisor))
                    .map_or(Value::Null, Value::Float);
                (m.key, reading)
            })
            .into_iter()
            .collect()
    }
}

/// A signed spec integer; TLV encodes a positive one as either signed or unsigned.
fn int(value: &Tlv) -> Option<i64> {
    match value {
        Tlv::Int(n) => Some(*n),
        Tlv::Uint(n) => i64::try_from(*n).ok(),
        _ => None,
    }
}

fn field(value: &Tlv, tag: u8) -> Option<&Tlv> {
    let Tlv::Structure(fields) = value else {
        return None;
    };
    // A struct field appears once; a repeated tag is ambiguous, not a choice.
    let mut found = fields.iter().filter(|(t, _)| *t == Tag::Context(tag));
    let (_, value) = found.next()?;
    found.next().is_none().then_some(value)
}

fn scaled(n: i64, divisor: f64) -> Option<f64> {
    if !(-EXACT..=EXACT).contains(&n) {
        return None;
    }
    #[allow(clippy::cast_precision_loss)] // bounded by 2^53: exact.
    let n = n as f64;
    Some(n / divisor)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(cluster: u32, attribute: u32) -> AttributePath {
        AttributePath {
            endpoint: 1,
            cluster,
            attribute,
        }
    }
    fn globals(cluster: u32, features: u64, attributes: &[u64]) -> Vec<(AttributePath, Tlv)> {
        vec![
            (path(cluster, 0xfffc), Tlv::Uint(features)),
            (path(cluster, 0xfffd), Tlv::Uint(1)),
            (
                path(cluster, 0xfffb),
                Tlv::Array(attributes.iter().copied().map(Tlv::Uint).collect()),
            ),
            (path(cluster, 0xfff9), Tlv::Array(vec![])),
        ]
    }
    /// An AC plug: ALTC power with every measure, imported cumulative energy.
    fn plug() -> Vec<(AttributePath, Tlv)> {
        let mut report = globals(POWER, 0b10, &[0, 1, 2, 4, 5, 8, 0x0a]);
        report.extend(globals(ENERGY, 0b0101, &[0, 1, 5]));
        report
    }
    fn energy(total: Tlv) -> Tlv {
        Tlv::Structure(vec![
            (Tag::Context(0), total),
            (Tag::Context(1), Tlv::Uint(0)),
            (Tag::Context(2), Tlv::Uint(60)),
        ])
    }
    fn keys(e: &Electrical) -> Vec<String> {
        e.points().iter().map(|p| p.key.to_string()).collect()
    }

    #[test]
    fn announced_attributes_become_native_read_only_points() {
        let e = Electrical::discover(1, &plug()).unwrap();
        assert_eq!(
            keys(&e),
            ["power", "apparent_power", "voltage", "current", "energy"]
        );
        for p in e.points() {
            assert!(p.access.read && !p.access.write, "{}", p.key);
            assert_eq!(Semantic::infer(&p.key, p.unit.as_ref()), p.semantic);
        }
        let energy = e
            .points()
            .into_iter()
            .find(|p| &*p.key == "energy")
            .unwrap();
        assert_eq!(energy.unit, Some(Unit::KiloWattHour));
        assert_eq!(
            energy.kind,
            Kind::Numeric {
                min: Some(0.0),
                max: None,
                step: None
            }
        );
        assert_eq!(e.paths(1).len(), 5);
        assert!(
            e.paths(1)
                .iter()
                .all(|p| p.endpoint == Some(1) && p.attribute.is_some())
        );
    }

    #[test]
    fn absent_attributes_features_or_metadata_expose_nothing() {
        // Power alone, without the optional measures.
        let e = Electrical::discover(1, &globals(POWER, 0, &[0, 1, 2, 8])).unwrap();
        assert_eq!(keys(&e), ["power"]);
        // Listed energy without CUME, or periodic only: not a meter.
        for features in [0b0001, 0b1001, 0b0100] {
            assert!(Electrical::discover(1, &globals(ENERGY, features, &[0, 1, 3])).is_none());
        }
        // Exported totals need EXPE.
        let e = Electrical::discover(1, &globals(ENERGY, 0b0111, &[0, 1, 2])).unwrap();
        assert_eq!(keys(&e), ["energy", "energy_returned"]);
        let e = Electrical::discover(1, &globals(ENERGY, 0b0101, &[0, 1, 2])).unwrap();
        assert_eq!(keys(&e), ["energy"]);
        // Fail closed on incomplete metadata or another endpoint.
        let mut r = plug();
        r.retain(|(p, _)| p.attribute != 0xfffc);
        assert!(Electrical::discover(1, &r).is_none());
        assert!(Electrical::discover(2, &plug()).is_none());
        assert!(Electrical::discover(1, &[]).is_none());
    }

    #[test]
    fn readings_convert_from_fixed_spec_units() {
        let mut e = Electrical::discover(1, &plug()).unwrap();
        let cases = [
            (path(POWER, 0x08), Tlv::Int(1_234_567), "power", 1234.567),
            (path(POWER, 0x08), Tlv::Uint(60_000), "power", 60.0),
            (path(POWER, 0x08), Tlv::Int(-1500), "power", -1.5),
            (path(POWER, 0x0a), Tlv::Int(80_250), "apparent_power", 80.25),
            (path(POWER, 0x04), Tlv::Int(231_400), "voltage", 231.4),
            (path(POWER, 0x05), Tlv::Int(260), "current", 0.26),
            (
                path(ENERGY, 0x01),
                energy(Tlv::Int(12_345_678)),
                "energy",
                12.345_678,
            ),
            (path(ENERGY, 0x01), energy(Tlv::Uint(0)), "energy", 0.0),
        ];
        for (p, v, key, expected) in cases {
            let got = e.update(p, &v);
            assert_eq!(got.len(), 1, "{key}");
            assert_eq!(got[0].0, key);
            let Value::Float(n) = got[0].1 else {
                panic!("{key}: {:?}", got[0].1)
            };
            assert!((n - expected).abs() < 1e-9, "{key}: {n} != {expected}");
        }
    }

    #[test]
    fn invalid_or_null_readings_clear_the_point() {
        let mut e = Electrical::discover(1, &plug()).unwrap();
        let null = |key| vec![(key, Value::Null)];
        for v in [
            Tlv::Null,
            Tlv::Bool(true),
            Tlv::Float(1.0),
            Tlv::Uint(u64::MAX),
            Tlv::Int(i64::MIN),
            Tlv::Int(EXACT + 1),
        ] {
            assert_eq!(e.update(path(POWER, 0x08), &v), null("power"), "{v:?}");
        }
        for v in [
            Tlv::Null,
            Tlv::Int(5),
            Tlv::Structure(vec![]),
            Tlv::Structure(vec![(Tag::Context(1), Tlv::Int(5))]),
            energy(Tlv::Null),
            energy(Tlv::Int(-1)),
            Tlv::Structure(vec![
                (Tag::Context(0), Tlv::Int(1)),
                (Tag::Context(0), Tlv::Int(2)),
            ]),
        ] {
            assert_eq!(e.update(path(ENERGY, 0x01), &v), null("energy"), "{v:?}");
        }
    }

    #[test]
    fn unclaimed_reports_are_ignored() {
        let mut e = Electrical::discover(1, &globals(POWER, 0, &[0, 1, 2, 8])).unwrap();
        // Not listed, not mapped, periodic energy, other endpoint or cluster.
        for p in [
            path(POWER, 0x04),
            path(POWER, 0x09),
            path(ENERGY, 0x01),
            path(ENERGY, 0x03),
            path(6, 0x08),
            AttributePath {
                endpoint: 2,
                ..path(POWER, 0x08)
            },
        ] {
            assert!(e.update(p, &Tlv::Int(1000)).is_empty(), "{p:?}");
        }
    }
}
