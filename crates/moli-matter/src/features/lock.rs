//! Standard Door Lock, with no credential handling or inferred command success.
use super::{Cluster, Plan, Report, attribute, uint};
use matter_controller::{AttributePath, ReadPath, Value as Tlv};
use moli_core::{Access, Kind, PointSpec, Semantic, Value};

pub(crate) const CLUSTERS: &[u32] = &[0x0101];
pub(crate) const DISCOVERY: &[(u32, u32)] = &[
    (0x0101, 0),
    (0x0101, 2),
    (0x0101, 0x25),
    (0x0101, 0x26),
    (0x0101, 0x33),
];
pub(crate) const METADATA: &[(u32, u32)] =
    &[(0x0101, 2), (0x0101, 0x25), (0x0101, 0x26), (0x0101, 0x33)];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Lock {
    endpoint: u16,
    cluster: Cluster,
    controllable: bool,
}

impl Lock {
    pub(crate) fn discover(ep: u16, report: &Report) -> Option<Self> {
        let cluster = Cluster::discover(ep, 0x0101, report)?;
        if !cluster.attributes.contains(&0) {
            return None;
        }
        let get = |id| attribute(report, ep, 0x0101, id);
        // Fail closed even when the optional PIN policy is absent. Restrict this
        // credential-free implementation to Normal mode; never override policy.
        let controllable = [2, 0x25, 0x26, 0x33]
            .iter()
            .all(|id| cluster.attributes.contains(id))
            && get(2) == Some(&Tlv::Bool(true))
            && get(0x33) == Some(&Tlv::Bool(false))
            && get(0x25).and_then(uint) == Some(0)
            && get(0x26)
                .and_then(uint)
                .is_some_and(|m| u16::try_from(m).is_ok() && m & 0xffe0 == 0xffe0 && m & 1 == 0);
        Some(Self {
            endpoint: ep,
            cluster,
            controllable,
        })
    }

    pub(crate) fn points(&self) -> Vec<PointSpec> {
        let mut points = vec![
            PointSpec {
                key: "locked".into(),
                label: moli_i18n::tr!("pilotes.matter.locked").into(),
                kind: Kind::Binary,
                access: Access {
                    read: true,
                    write: false,
                },
                unit: None,
                semantic: Semantic::Lock,
            },
            PointSpec {
                key: "lock_state".into(),
                label: moli_i18n::tr!("pilotes.matter.lock_state").into(),
                kind: Kind::Enum {
                    values: ["not_fully_locked", "locked", "unlocked", "unlatched"]
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                },
                access: Access {
                    read: true,
                    write: false,
                },
                unit: None,
                semantic: Semantic::Other,
            },
        ];
        let commands: Vec<_> = [(0, "lock"), (1, "unlock")]
            .into_iter()
            .filter(|(cmd, _)| self.controllable && self.cluster.commands.contains(cmd))
            .map(|(_, name)| name.into())
            .collect();
        if !commands.is_empty() {
            points.push(PointSpec {
                key: "control".into(),
                label: moli_i18n::tr!("pilotes.matter.control").into(),
                kind: Kind::Enum { values: commands },
                access: Access {
                    read: false,
                    write: true,
                },
                unit: None,
                semantic: Semantic::Control,
            });
        }
        points
    }

    pub(crate) fn paths(&self, ep: u16) -> Vec<ReadPath> {
        if ep != self.endpoint {
            return Vec::new();
        }
        vec![ReadPath::concrete(ep, 0x0101, 0)]
    }

    pub(crate) fn update(
        &mut self,
        path: AttributePath,
        value: &Tlv,
    ) -> Vec<(&'static str, Value)> {
        if path.endpoint != self.endpoint || path.cluster != 0x0101 || path.attribute != 0 {
            return vec![];
        }
        let (locked, state) = match uint(value) {
            Some(0) => (Value::Null, Value::from("not_fully_locked")),
            Some(1) => (Value::Bool(true), Value::from("locked")),
            Some(2) => (Value::Bool(false), Value::from("unlocked")),
            Some(3) if self.cluster.features & (1 << 12) != 0 => {
                (Value::Null, Value::from("unlatched"))
            }
            _ => (Value::Null, Value::Null),
        };
        vec![("locked", locked), ("lock_state", state)]
    }

    pub(crate) fn plan(&self, key: &str, value: &Value) -> Option<Plan> {
        if key != "control" || !self.controllable {
            return None;
        }
        let Value::Text(value) = value else {
            return None;
        };
        let command = match value.as_ref() {
            "lock" => 0,
            "unlock" => 1,
            _ => return None,
        };
        self.cluster
            .commands
            .contains(&command)
            .then(|| Plan::Invoke {
                cluster: 0x0101,
                command,
                fields: Tlv::Structure(vec![]),
                timed: true,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Vec<(AttributePath, Tlv)> {
        let ids = |ns: &[u64]| Tlv::Array(ns.iter().map(|n| Tlv::Uint(*n)).collect());
        [
            (0xfffc, Tlv::Uint(1 << 12)),
            (0xfffd, Tlv::Uint(7)),
            (0xfffb, ids(&[0, 2, 0x25, 0x26, 0x33])),
            (0xfff9, ids(&[0, 1, 3, 0x27])),
            (0, Tlv::Null),
            (2, Tlv::Bool(true)),
            (0x25, Tlv::Uint(0)),
            (0x26, Tlv::Uint(0xfff6)),
            (0x33, Tlv::Bool(false)),
        ]
        .into_iter()
        .map(|(attribute, v)| {
            (
                AttributePath {
                    endpoint: 1,
                    cluster: 0x0101,
                    attribute,
                },
                v,
            )
        })
        .collect()
    }

    #[test]
    fn lock_allowlist_and_policy_fail_closed() {
        let report = report();
        let lock = Lock::discover(1, &report).unwrap();
        for (name, command) in [("lock", 0), ("unlock", 1)] {
            assert_eq!(
                lock.plan("control", &Value::from(name)),
                Some(Plan::Invoke {
                    cluster: 0x0101,
                    command,
                    fields: Tlv::Structure(vec![]),
                    timed: true,
                })
            );
        }
        for key in ["locked", "lock_state", "pin", "mode"] {
            assert!(lock.plan(key, &Value::from("unlock")).is_none());
        }
        for value in [
            Value::from("unbolt"),
            Value::from("unlock_timeout"),
            Value::Bool(false),
            Value::Float(f64::NAN),
        ] {
            assert!(lock.plan("control", &value).is_none());
        }
        for id in [2, 0x25, 0x26, 0x33] {
            for replacement in [None, Some(Tlv::Null), Some(Tlv::Uint(u64::MAX))] {
                let mut changed = report.clone();
                changed.retain(|(p, _)| p.attribute != id);
                if let Some(value) = replacement {
                    changed.push((
                        AttributePath {
                            endpoint: 1,
                            cluster: 0x0101,
                            attribute: id,
                        },
                        value,
                    ));
                }
                let restricted = Lock::discover(1, &changed).unwrap();
                assert!(restricted.plan("control", &Value::from("unlock")).is_none());
                assert_eq!(restricted.points().len(), 2);
            }
        }
        for (id, value) in [
            (2, Tlv::Bool(false)),
            (0x33, Tlv::Bool(true)),
            (0x25, Tlv::Uint(1)),
            (0x25, Tlv::Uint(2)),
            (0x25, Tlv::Uint(3)),
            (0x25, Tlv::Uint(4)),
            (0x26, Tlv::Uint(2)),
            (0x26, Tlv::Uint(0xffff)),
        ] {
            let mut changed = report.clone();
            changed
                .iter_mut()
                .find(|(p, _)| p.attribute == id)
                .unwrap()
                .1 = value;
            assert!(
                Lock::discover(1, &changed)
                    .unwrap()
                    .plan("control", &Value::from("lock"))
                    .is_none()
            );
        }
        assert!(METADATA.iter().all(|pair| DISCOVERY.contains(pair)));
        assert!(!METADATA.contains(&(0x0101, 0)));
        assert_eq!(CLUSTERS, &[0x0101]);
    }

    #[test]
    fn lock_subscription_states_never_infer_locked_or_optimistic_success() {
        let mut lock = Lock::discover(1, &report()).unwrap();
        let path = AttributePath {
            endpoint: 1,
            cluster: 0x0101,
            attribute: 0,
        };
        for (n, expected, state) in [
            (0, Value::Null, "not_fully_locked"),
            (1, Value::Bool(true), "locked"),
            (2, Value::Bool(false), "unlocked"),
            (3, Value::Null, "unlatched"),
        ] {
            assert_eq!(
                lock.update(path, &Tlv::Uint(n)),
                vec![("locked", expected), ("lock_state", Value::from(state))]
            );
        }
        for invalid in [
            Tlv::Null,
            Tlv::Uint(4),
            Tlv::Uint(u64::MAX),
            Tlv::Int(1),
            Tlv::Bool(true),
        ] {
            assert_eq!(
                lock.update(path, &invalid),
                vec![("locked", Value::Null), ("lock_state", Value::Null)]
            );
        }
        assert!(
            lock.update(
                AttributePath {
                    endpoint: 2,
                    ..path
                },
                &Tlv::Uint(1)
            )
            .is_empty()
        );
        lock.cluster.features = 0;
        assert_eq!(lock.update(path, &Tlv::Uint(3))[1].1, Value::Null);
        lock.cluster.commands.clear();
        assert_eq!(lock.points().len(), 2);
        assert!(lock.plan("control", &Value::from("lock")).is_none());
        assert_eq!(lock.paths(1).len(), 1);
        assert!(lock.points().iter().all(|p| !p.access.write));
    }
}
