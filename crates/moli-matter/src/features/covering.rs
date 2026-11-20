//! Standard Window Covering (0x0102); CHIP window-covering.xml defines command fields.
use super::{Cluster, Plan, Report, attribute, uint};
use matter_codec::Tag;
use matter_controller::{AttributePath, ReadPath, Value as Tlv};
use moli_core::{Access, Kind, PointSpec, Semantic, Unit, Value};

pub(crate) const CLUSTERS: &[u32] = &[0x102];
pub(crate) const DISCOVERY: &[(u32, u32)] =
    &[(0x102, 0x0e), (0x102, 7), (0x102, 0x17), (0x102, 0x1a)];
pub(crate) const METADATA: &[(u32, u32)] = &[(0x102, 7), (0x102, 0x17), (0x102, 0x1a)];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Covering {
    endpoint: u16,
    cluster: Cluster,
    operational: bool,
}

impl Covering {
    pub(crate) fn discover(ep: u16, report: &Report) -> Option<Self> {
        let cluster = Cluster::discover(ep, 0x102, report)?;
        let get = |id| {
            cluster
                .attributes
                .contains(&id)
                .then(|| attribute(report, ep, 0x102, id).and_then(uint))
                .flatten()
        };
        let operational = get(7).is_some_and(|n| n <= 0x7f && n & 1 != 0)
            && get(0x17).is_some_and(|n| n <= 0x0f && n & 6 == 0)
            && (!cluster.attributes.contains(&0x1a) || get(0x1a) == Some(0));
        let result = Self {
            endpoint: ep,
            cluster,
            operational,
        };
        (!result.points().is_empty()).then_some(result)
    }

    fn position(&self) -> bool {
        self.cluster.features & 5 == 5 && self.cluster.attributes.contains(&0x0e)
    }

    #[allow(clippy::verbose_bit_mask)] // LF/TL feature bits, not a numeric magnitude.
    fn commands(&self) -> Vec<&'static str> {
        if self.cluster.features & 3 == 0 {
            return Vec::new();
        }
        [(0, "open"), (1, "close"), (2, "stop")]
            .into_iter()
            .filter(|(id, _)| self.cluster.commands.contains(id) && (*id == 2 || self.operational))
            .map(|(_, name)| name)
            .collect()
    }

    pub(crate) fn points(&self) -> Vec<PointSpec> {
        let mut points = Vec::new();
        if self.position() {
            points.push(PointSpec {
                key: "position".into(),
                label: moli_i18n::tr!("pilotes.matter.position").into(),
                kind: Kind::Numeric {
                    min: Some(0.0),
                    max: Some(100.0),
                    step: Some(0.01),
                },
                access: Access {
                    read: true,
                    write: self.operational
                        && self.cluster.attributes.contains(&0x0b)
                        && self.cluster.commands.contains(&5),
                },
                unit: Some(Unit::Percent),
                semantic: Semantic::Position,
            });
        }
        let commands = self.commands();
        if !commands.is_empty() {
            points.push(PointSpec {
                key: "cover_control".into(),
                label: moli_i18n::tr!("pilotes.matter.cover_control").into(),
                kind: Kind::Enum {
                    values: commands.into_iter().map(Into::into).collect(),
                },
                access: Access {
                    read: false,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Other,
            });
        }
        points
    }

    pub(crate) fn paths(&self, ep: u16) -> Vec<ReadPath> {
        self.position()
            .then(|| ReadPath::concrete(ep, 0x102, 0x0e))
            .into_iter()
            .collect()
    }

    pub(crate) fn update(
        &mut self,
        path: AttributePath,
        value: &Tlv,
    ) -> Vec<(&'static str, Value)> {
        if path.endpoint != self.endpoint
            || path.cluster != 0x102
            || path.attribute != 0x0e
            || !self.position()
        {
            return Vec::new();
        }
        vec![(
            "position",
            uint(value)
                .and_then(|n| u16::try_from(n).ok())
                .filter(|n| *n <= 10000)
                .map_or(Value::Null, |n| Value::Float(100.0 - f64::from(n) / 100.0)),
        )]
    }

    pub(crate) fn plan(&self, key: &str, value: &Value) -> Option<Plan> {
        let (command, fields) = match key {
            "cover_control" => {
                let Value::Text(name) = value else {
                    return None;
                };
                if !self.commands().contains(&name.as_ref()) {
                    return None;
                }
                (
                    match name.as_ref() {
                        "open" => 0,
                        "close" => 1,
                        "stop" => 2,
                        _ => return None,
                    },
                    Tlv::Structure(vec![]),
                )
            }
            "position"
                if self.position()
                    && self.operational
                    && self.cluster.attributes.contains(&0x0b)
                    && self.cluster.commands.contains(&5) =>
            {
                let n = value.as_f64()?;
                let hundredths = (100.0 - n) * 100.0;
                if !n.is_finite()
                    || !(0.0..=100.0).contains(&n)
                    || (hundredths - hundredths.round()).abs() > 1e-7
                {
                    return None;
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let position = hundredths.round() as u64;
                (
                    5,
                    Tlv::Structure(vec![(Tag::Context(0), Tlv::Uint(position))]),
                )
            }
            _ => return None,
        };
        Some(Plan::Invoke {
            cluster: 0x102,
            command,
            fields,
            timed: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(id: u32) -> AttributePath {
        AttributePath {
            endpoint: 1,
            cluster: 0x102,
            attribute: id,
        }
    }
    fn report() -> Vec<(AttributePath, Tlv)> {
        [
            (0xfffc, Tlv::Uint(5)),
            (0xfffd, Tlv::Uint(6)),
            (
                0xfffb,
                Tlv::Array([7, 0x17, 0x0b, 0x0e].map(Tlv::Uint).to_vec()),
            ),
            (0xfff9, Tlv::Array([0, 1, 2, 5].map(Tlv::Uint).to_vec())),
            (7, Tlv::Uint(1)),
            (0x17, Tlv::Uint(0)),
        ]
        .into_iter()
        .map(|(id, v)| (path(id), v))
        .collect()
    }
    #[test]
    fn covering_direction_commands_and_invalid_samples() {
        let mut c = Covering::discover(1, &report()).unwrap();
        assert_eq!(
            c.update(path(0x0e), &Tlv::Uint(0)),
            vec![("position", Value::Float(100.0))]
        );
        assert_eq!(
            c.update(path(0x0e), &Tlv::Uint(10000)),
            vec![("position", Value::Float(0.0))]
        );
        for v in [Tlv::Null, Tlv::Uint(65535), Tlv::Int(50)] {
            assert_eq!(c.update(path(0x0e), &v), vec![("position", Value::Null)]);
        }
        assert!(c.update(path(0x0b), &Tlv::Uint(100)).is_empty());
        assert!(
            matches!(c.plan("position", &Value::Float(75.25)), Some(Plan::Invoke {command:5, fields:Tlv::Structure(f),..}) if f == vec![(Tag::Context(0), Tlv::Uint(2475))])
        );
        for n in [f64::NAN, f64::INFINITY, -0.01, 100.01, 0.001] {
            assert!(c.plan("position", &Value::Float(n)).is_none());
        }
        for (name, id) in [("open", 0), ("close", 1), ("stop", 2)] {
            assert!(
                matches!(c.plan("cover_control", &Value::from(name)), Some(Plan::Invoke {command,..}) if command == id)
            );
        }
        c.cluster.commands.remove(&1);
        assert!(c.plan("cover_control", &Value::from("close")).is_none());
    }
    #[test]
    fn covering_metadata_is_fail_closed() {
        let mut r = report();
        r.retain(|(p, _)| p.attribute != 0xfffc);
        assert!(Covering::discover(1, &r).is_none());
        let mut r = report();
        r.retain(|(p, _)| p.attribute != 0x17);
        let c = Covering::discover(1, &r).unwrap();
        assert!(!c.points()[0].access.write);
        assert!(c.plan("cover_control", &Value::from("open")).is_none());
        let mut c = Covering::discover(1, &report()).unwrap();
        assert_eq!(c.paths(1).len(), 1);
        c.cluster.attributes.remove(&0x0b);
        assert!(c.plan("position", &Value::Int(50)).is_none());
        c.cluster.features = 1;
        assert!(!c.position());
    }
}
