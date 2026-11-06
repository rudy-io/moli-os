//! `Binding` (0x001E) targets and their `Value` encode/decode. The controller
//! hand-builds the `TargetStruct` `Value` (decoder-agnostic, matching the ACL /
//! groups pattern); the generated codec is the read oracle.

use matter_codec::{Tag, Value};
use matter_interaction::AttributePath;

/// Binding cluster id.
pub(crate) const BINDING_CLUSTER: u32 = 0x001E;
/// `Binding` attribute id (the writable list-of-`TargetStruct`).
pub(crate) const ATTR_BINDING: u32 = 0x0000;

/// One `Binding.TargetStruct` — a unicast (`node` + `endpoint` [+ `cluster`]) or
/// group (`group` [+ `cluster`]) binding. The device stamps the fabric index;
/// callers never set it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct BindingTarget {
    /// Target node id (unicast binding).
    pub node: Option<u64>,
    /// Target group id (group binding).
    pub group: Option<u16>,
    /// Target endpoint (unicast binding).
    pub endpoint: Option<u16>,
    /// Optional target cluster; `None` binds all clusters.
    pub cluster: Option<u32>,
}

impl BindingTarget {
    /// Construct a [`BindingTarget`]. Provided because the struct is
    /// `#[non_exhaustive]` (external callers cannot use a struct literal).
    #[must_use]
    pub fn new(
        node: Option<u64>,
        group: Option<u16>,
        endpoint: Option<u16>,
        cluster: Option<u32>,
    ) -> Self {
        Self {
            node,
            group,
            endpoint,
            cluster,
        }
    }
}

/// Build the `TargetStruct` `Value` for one target (ctx1 Node / ctx2 Group /
/// ctx3 Endpoint / ctx4 Cluster; only the present fields are emitted).
pub(crate) fn binding_target_value(t: &BindingTarget) -> Value {
    let mut m = Vec::new();
    if let Some(n) = t.node {
        m.push((Tag::Context(1), Value::Uint(n)));
    }
    if let Some(g) = t.group {
        m.push((Tag::Context(2), Value::Uint(u64::from(g))));
    }
    if let Some(e) = t.endpoint {
        m.push((Tag::Context(3), Value::Uint(u64::from(e))));
    }
    if let Some(c) = t.cluster {
        m.push((Tag::Context(4), Value::Uint(u64::from(c))));
    }
    Value::Structure(m)
}

/// Parse the `Binding` attribute (a `Value::Array` of `TargetStruct`) out of read
/// reports into [`BindingTarget`]s. Unknown members are ignored.
pub(crate) fn parse_bindings(reports: &[(AttributePath, Value)]) -> Vec<BindingTarget> {
    let mut out = Vec::new();
    for (p, v) in reports {
        if p.cluster != BINDING_CLUSTER || p.attribute != ATTR_BINDING {
            continue;
        }
        if let Value::Array(entries) = v {
            for entry in entries {
                if let Value::Structure(members) = entry {
                    let mut t = BindingTarget::new(None, None, None, None);
                    for (tag, val) in members {
                        match (*tag, val) {
                            (Tag::Context(1), Value::Uint(n)) => t.node = Some(*n),
                            (Tag::Context(2), Value::Uint(g)) => t.group = u16::try_from(*g).ok(),
                            (Tag::Context(3), Value::Uint(e)) => {
                                t.endpoint = u16::try_from(*e).ok();
                            }
                            (Tag::Context(4), Value::Uint(c)) => {
                                t.cluster = u32::try_from(*c).ok();
                            }
                            _ => {}
                        }
                    }
                    out.push(t);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;

    #[test]
    fn target_value_and_parse_roundtrip() {
        let targets = vec![
            BindingTarget::new(Some(0x1122), None, Some(1), Some(0x0006)), // unicast
            BindingTarget::new(None, Some(0x0007), None, Some(0x0006)),    // group
        ];
        // Build the list Value, wrap it as a read report, parse it back.
        let list = Value::Array(targets.iter().map(binding_target_value).collect());
        let reports = vec![(
            AttributePath {
                endpoint: 1,
                cluster: BINDING_CLUSTER,
                attribute: ATTR_BINDING,
            },
            list,
        )];
        assert_eq!(parse_bindings(&reports), targets);
    }
}
