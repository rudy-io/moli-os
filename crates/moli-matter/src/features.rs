//! Endpoint-local capabilities for standard Matter device families.
pub(crate) mod covering;
pub(crate) mod lighting;
pub(crate) mod lock;
pub(crate) mod thermostat;

use matter_controller::{AttributePath, Value as Tlv};
use std::collections::BTreeSet;

pub(crate) type Report = [(AttributePath, Tlv)];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cluster {
    pub(crate) features: u32,
    pub(crate) attributes: BTreeSet<u32>,
    pub(crate) commands: BTreeSet<u32>,
    pub(crate) revision: u16,
}

pub(crate) fn attribute(report: &Report, endpoint: u16, cluster: u32, id: u32) -> Option<&Tlv> {
    report.iter().find_map(|(path, value)| {
        (path.endpoint == endpoint && path.cluster == cluster && path.attribute == id)
            .then_some(value)
    })
}

pub(crate) fn uint(value: &Tlv) -> Option<u64> {
    if let Tlv::Uint(n) = value {
        Some(*n)
    } else {
        None
    }
}

impl Cluster {
    pub(crate) fn discover(endpoint: u16, cluster: u32, report: &Report) -> Option<Self> {
        let get = |id| attribute(report, endpoint, cluster, id);
        let ids = |id| {
            let Tlv::Array(values) = get(id)? else {
                return None;
            };
            if values.len() > 256 {
                return None;
            }
            let mut result = BTreeSet::new();
            for value in values {
                if !result.insert(u32::try_from(uint(value)?).ok()?) {
                    return None;
                }
            }
            Some(result)
        };
        let revision = u16::try_from(uint(get(0xfffd)?)?).ok()?;
        if revision == 0 {
            return None;
        }
        Some(Self {
            features: u32::try_from(uint(get(0xfffc)?)?).ok()?,
            attributes: ids(0xfffb)?,
            commands: ids(0xfff9)?,
            revision,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Plan {
    Invoke {
        cluster: u32,
        command: u32,
        fields: Tlv,
        timed: bool,
    },
    Write {
        cluster: u32,
        attribute: u32,
        value: Tlv,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Expanded {
    lighting: Option<lighting::Lighting>,
    covering: Option<covering::Covering>,
    thermostat: Option<thermostat::Thermostat>,
    lock: Option<lock::Lock>,
}

impl Expanded {
    pub(crate) fn discover(endpoint: u16, report: &Report) -> Self {
        Self {
            lighting: lighting::Lighting::discover(endpoint, report),
            covering: covering::Covering::discover(endpoint, report),
            thermostat: thermostat::Thermostat::discover(endpoint, report),
            lock: lock::Lock::discover(endpoint, report),
        }
    }
    pub(crate) fn points(&self) -> Vec<moli_core::PointSpec> {
        [
            self.lighting.as_ref().map(lighting::Lighting::points),
            self.covering.as_ref().map(covering::Covering::points),
            self.thermostat.as_ref().map(thermostat::Thermostat::points),
            self.lock.as_ref().map(lock::Lock::points),
        ]
        .into_iter()
        .flatten()
        .flatten()
        .collect()
    }
    pub(crate) fn paths(&self, ep: u16) -> Vec<matter_controller::ReadPath> {
        [
            self.lighting.as_ref().map(|f| f.paths(ep)),
            self.covering.as_ref().map(|f| f.paths(ep)),
            self.thermostat.as_ref().map(|f| f.paths(ep)),
            self.lock.as_ref().map(|f| f.paths(ep)),
        ]
        .into_iter()
        .flatten()
        .flatten()
        .collect()
    }
    pub(crate) fn update(
        &mut self,
        path: AttributePath,
        value: &Tlv,
    ) -> Vec<(&'static str, moli_core::Value)> {
        [
            self.lighting.as_mut().map(|f| f.update(path, value)),
            self.covering.as_mut().map(|f| f.update(path, value)),
            self.thermostat.as_mut().map(|f| f.update(path, value)),
            self.lock.as_mut().map(|f| f.update(path, value)),
        ]
        .into_iter()
        .flatten()
        .flatten()
        .collect()
    }
    pub(crate) fn plan(&self, key: &str, value: &moli_core::Value) -> Option<Plan> {
        let mut plans = [
            self.lighting.as_ref().and_then(|f| f.plan(key, value)),
            self.covering.as_ref().and_then(|f| f.plan(key, value)),
            self.thermostat.as_ref().and_then(|f| f.plan(key, value)),
            self.lock.as_ref().and_then(|f| f.plan(key, value)),
        ]
        .into_iter()
        .flatten();
        let plan = plans.next()?;
        plans.next().is_none().then_some(plan)
    }
}

pub(crate) fn supported_cluster(cluster: u32) -> bool {
    [
        lighting::CLUSTERS,
        covering::CLUSTERS,
        thermostat::CLUSTERS,
        lock::CLUSTERS,
    ]
    .iter()
    .any(|clusters| clusters.contains(&cluster))
}

pub(crate) fn metadata_attribute(path: AttributePath) -> bool {
    (supported_cluster(path.cluster) && [0xfff9, 0xfffb, 0xfffc, 0xfffd].contains(&path.attribute))
        || [
            lighting::METADATA,
            covering::METADATA,
            thermostat::METADATA,
            lock::METADATA,
        ]
        .iter()
        .any(|attrs| attrs.contains(&(path.cluster, path.attribute)))
}

pub(crate) fn policy_paths(endpoint: u16, report: &Report) -> Vec<matter_controller::ReadPath> {
    [
        lighting::METADATA,
        covering::METADATA,
        thermostat::METADATA,
        lock::METADATA,
    ]
    .into_iter()
    .flatten()
    .filter_map(|(cluster, id)| {
        let Tlv::Array(ids) = attribute(report, endpoint, *cluster, 0xfffb)? else {
            return None;
        };
        (ids.len() <= 256 && ids.contains(&Tlv::Uint(u64::from(*id))))
            .then(|| matter_controller::ReadPath::concrete(endpoint, *cluster, *id))
    })
    .collect()
}

/// A mode report can follow coordinate reports; refresh instead of reusing inactive samples.
pub(crate) fn color_refresh_paths(
    path: AttributePath,
    value: &Tlv,
    report: &Report,
) -> Vec<matter_controller::ReadPath> {
    if path.cluster != 0x300
        || ![8, 0x4001].contains(&path.attribute)
        || uint(value).is_none_or(|n| n > 3)
    {
        return vec![];
    }
    let Some(Tlv::Array(ids)) = attribute(report, path.endpoint, 0x300, 0xfffb) else {
        return vec![];
    };
    if ids.len() > 256 {
        return vec![];
    }
    [0, 1, 3, 4, 7, 0x4000]
        .into_iter()
        .filter(|a| ids.contains(&Tlv::Uint(u64::from(*a))))
        .map(|a| matter_controller::ReadPath::concrete(path.endpoint, 0x300, a))
        .collect()
}

/// Bounded queries for new families; absent metadata never grants capabilities.
pub(crate) fn discovery_paths(report: &Report) -> Vec<matter_controller::ReadPath> {
    let mut paths = BTreeSet::new();
    let mut endpoints = BTreeSet::new();
    for (path, value) in report {
        if path.endpoint == 0
            || path.endpoint == u16::MAX
            || path.cluster != 0x1d
            || path.attribute != 1
        {
            continue;
        }
        let Tlv::Array(servers) = value else { continue };
        if servers.len() > 128 {
            continue;
        }
        endpoints.insert(path.endpoint);
        if endpoints.len() > 128 {
            return vec![];
        }
        for cluster in servers
            .iter()
            .filter_map(uint)
            .filter_map(|c| u32::try_from(c).ok())
            .filter(|c| supported_cluster(*c))
        {
            for attr in [0xfff9, 0xfffb, 0xfffc, 0xfffd] {
                paths.insert((path.endpoint, cluster, attr));
            }
            for attrs in [
                lighting::DISCOVERY,
                covering::DISCOVERY,
                thermostat::DISCOVERY,
                lock::DISCOVERY,
            ] {
                for (c, a) in attrs {
                    if *c == cluster {
                        paths.insert((path.endpoint, cluster, *a));
                    }
                }
            }
            if paths.len() > 4096 {
                return vec![];
            }
        }
    }
    paths
        .into_iter()
        .map(|(ep, c, a)| matter_controller::ReadPath::concrete(ep, c, a))
        .collect()
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
    #[test]
    fn mutable_safety_policy_is_subscribed_even_while_read_only() {
        let report = vec![(
            path(0x101, 0xfffb),
            Tlv::Array(vec![Tlv::Uint(0x33), Tlv::Uint(2)]),
        )];
        let paths = policy_paths(1, &report);
        assert!(
            paths
                .iter()
                .any(|p| p.cluster == Some(0x101) && p.attribute == Some(0x33))
        );
        assert!(metadata_attribute(path(0x101, 0x33)));
        assert!(metadata_attribute(path(0x201, 0x19)));
        assert!(!metadata_attribute(path(0x101, 0)));
        assert!(policy_paths(2, &report).is_empty());
    }
    #[test]
    fn mode_refresh_reads_only_advertised_color_samples() {
        let report = vec![(
            path(0x300, 0xfffb),
            Tlv::Array(vec![Tlv::Uint(3), Tlv::Uint(4), Tlv::Uint(8)]),
        )];
        let paths = color_refresh_paths(path(0x300, 8), &Tlv::Uint(1), &report);
        assert_eq!(paths.len(), 2);
        assert!(
            paths
                .iter()
                .all(|p| [Some(3), Some(4)].contains(&p.attribute))
        );
        assert!(color_refresh_paths(path(0x300, 8), &Tlv::Null, &report).is_empty());
        assert!(color_refresh_paths(path(0x300, 8), &Tlv::Uint(255), &report).is_empty());
    }
    #[test]
    fn malformed_or_ambiguous_global_metadata_does_not_activate_features() {
        let mut report = vec![
            (path(8, 0xfffc), Tlv::Uint(2)),
            (path(8, 0xfffd), Tlv::Uint(1)),
            (path(8, 0xfffb), Tlv::Array(vec![Tlv::Uint(0)])),
            (path(8, 0xfff9), Tlv::Array(vec![Tlv::Uint(0)])),
        ];
        assert!(Cluster::discover(1, 8, &report).is_some());
        report[2].1 = Tlv::Array(vec![Tlv::Uint(0), Tlv::Uint(0)]);
        assert!(Cluster::discover(1, 8, &report).is_none());
        report[2].1 = Tlv::Array(vec![Tlv::Uint(0)]);
        report[1].1 = Tlv::Uint(0);
        assert!(Cluster::discover(1, 8, &report).is_none());
    }
}
