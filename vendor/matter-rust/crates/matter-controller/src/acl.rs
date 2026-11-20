//! `AccessControl` (0x001F) controller support: ACL entry types, the pure Value
//! encode/parse, and the lockout guard. Decoder-agnostic (hand-built Value);
//! the generated matter-clusters decoder is the read byte-parity oracle.

use matter_codec::{Tag, Value};
use matter_interaction::AttributePath;

/// Cluster ID for `AccessControl` (Matter spec §9.10).
pub(crate) const ACCESS_CONTROL_CLUSTER: u32 = 0x001F;
/// Attribute id for the `ACL` list attribute (§9.10.4.1).
pub(crate) const ATTR_ACL: u32 = 0x0000;

// AccessControlEntryStruct context tags (Matter spec §9.10.5.2).
const TAG_PRIVILEGE: u8 = 1;
const TAG_AUTH_MODE: u8 = 2;
const TAG_SUBJECTS: u8 = 3;
const TAG_TARGETS: u8 = 4;
const TAG_FABRIC_INDEX: u8 = 254;

// AccessControlTargetStruct context tags (Matter spec §9.10.5.4).
const TAG_TARGET_CLUSTER: u8 = 0;
const TAG_TARGET_ENDPOINT: u8 = 1;
const TAG_TARGET_DEVICE_TYPE: u8 = 2;

/// ACL privilege level (`AccessControlEntryPrivilegeEnum`, Matter spec §9.10.5.3).
///
/// `#[non_exhaustive]` so future spec revisions can add privilege levels without
/// a breaking change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AclPrivilege {
    /// Can read non-sensitive attributes and invoke non-privileged commands.
    View,
    /// Can act as a proxy for a View-level principal.
    ProxyView,
    /// Can perform data model operations including writes and commands.
    Operate,
    /// Can manage fabric-scoped data.
    Manage,
    /// Full administrative control including fabric membership.
    Administer,
    /// A privilege value not recognised by this version of the library.
    Unknown(u8),
}

impl AclPrivilege {
    #[allow(clippy::cast_possible_truncation)]
    // Privilege enum values are spec-typed as uint8 in range 1–5; to_raw is
    // always within u8 range.
    fn to_raw(self) -> u8 {
        match self {
            Self::View => 1,
            Self::ProxyView => 2,
            Self::Operate => 3,
            Self::Manage => 4,
            Self::Administer => 5,
            Self::Unknown(v) => v,
        }
    }

    fn from_raw(v: u8) -> Self {
        match v {
            1 => Self::View,
            2 => Self::ProxyView,
            3 => Self::Operate,
            4 => Self::Manage,
            5 => Self::Administer,
            o => Self::Unknown(o),
        }
    }
}

/// ACL authentication mode (`AccessControlEntryAuthModeEnum`, Matter spec §9.10.5.3).
///
/// `#[non_exhaustive]` so future spec revisions can add modes without a breaking change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AclAuthMode {
    /// Password Authenticated Session Establishment.
    Pase,
    /// Certificate Authenticated Session Establishment (operational sessions).
    Case,
    /// Group messaging.
    ///
    /// Subjects for Group-auth entries are **plain group ids** on the wire
    /// (e.g. `vec![8]`) — the internal `0xFFFF_FFFF_FFFF_xxxx` group-node-id
    /// form used by the access engine is device-internal, and devices reject
    /// it in a written entry with `CONSTRAINT_ERROR` (verified on real
    /// hardware, 2026-07-20).
    Group,
    /// An auth-mode value not recognised by this version of the library.
    Unknown(u8),
}

impl AclAuthMode {
    #[allow(clippy::cast_possible_truncation)]
    // AuthMode enum values are spec-typed as uint8 in range 1–3; to_raw is
    // always within u8 range.
    fn to_raw(self) -> u8 {
        match self {
            Self::Pase => 1,
            Self::Case => 2,
            Self::Group => 3,
            Self::Unknown(v) => v,
        }
    }

    fn from_raw(v: u8) -> Self {
        match v {
            1 => Self::Pase,
            2 => Self::Case,
            3 => Self::Group,
            o => Self::Unknown(o),
        }
    }
}

/// One ACL target (`AccessControlTargetStruct`, Matter spec §9.10.5.4).
///
/// Any field set to `None` is a wildcard: it matches any cluster, endpoint, or
/// device-type respectively.
///
/// `#[non_exhaustive]` so additional target fields from future spec revisions
/// can be added without a breaking change.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct AclTarget {
    /// Target cluster id (`None` ⇒ all clusters).
    pub cluster: Option<u32>,
    /// Target endpoint id (`None` ⇒ all endpoints).
    pub endpoint: Option<u16>,
    /// Target device-type id (`None` ⇒ all device types).
    pub device_type: Option<u32>,
}

/// One ACL entry (`AccessControlEntryStruct`, Matter spec §9.10.5.2).
///
/// `#[non_exhaustive]` so additional entry fields from future spec revisions
/// can be added without a breaking change.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct AclEntry {
    /// Privilege granted by this entry.
    pub privilege: AclPrivilege,
    /// Authentication mode required to use this entry.
    pub auth_mode: AclAuthMode,
    /// Subject list: `None` ⇒ wildcard (applies to all subjects). `Some(v)` ⇒
    /// specific node IDs, CAT IDs, or group IDs.
    ///
    /// `None` means the device sent Subjects as null (wildcard). An entry
    /// whose Subjects field was absent, i.e. withheld by the device for
    /// another fabric's entry, is never returned.
    pub subjects: Option<Vec<u64>>,
    /// Target list: `None` ⇒ wildcard (all targets). `Some(v)` ⇒ specific targets.
    ///
    /// `None` means the device sent Targets as null (wildcard). An entry
    /// whose Targets field was absent, i.e. withheld by the device for
    /// another fabric's entry, is never returned.
    pub targets: Option<Vec<AclTarget>>,
    /// Fabric index assigned by the device. `None` on write (the device fills
    /// this in for the accessing fabric); always `Some` on read.
    pub fabric_index: Option<u8>,
}

impl AclTarget {
    /// Construct a target restricting to the given cluster / endpoint /
    /// device-type. Any `None` is a wildcard for that dimension.
    ///
    /// Provided because the struct is `#[non_exhaustive]` and so cannot be
    /// built with a struct literal outside this crate.
    #[must_use]
    pub fn new(cluster: Option<u32>, endpoint: Option<u16>, device_type: Option<u32>) -> Self {
        Self {
            cluster,
            endpoint,
            device_type,
        }
    }
}

impl AclEntry {
    /// Construct an ACL entry for a write. `subjects`/`targets` `None` ⇒
    /// wildcard. `fabric_index` is left `None` — the device fills it in for the
    /// accessing fabric.
    ///
    /// Provided because the struct is `#[non_exhaustive]` and so cannot be
    /// built with a struct literal outside this crate (e.g. when assembling an
    /// ACL to pass to [`crate::Node::write_acl`]).
    #[must_use]
    pub fn new(
        privilege: AclPrivilege,
        auth_mode: AclAuthMode,
        subjects: Option<Vec<u64>>,
        targets: Option<Vec<AclTarget>>,
    ) -> Self {
        Self {
            privilege,
            auth_mode,
            subjects,
            targets,
            fabric_index: None,
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn struct_members(v: &Value) -> Option<&[(Tag, Value)]> {
    match v {
        Value::Structure(m) | Value::List(m) => Some(m),
        _ => None,
    }
}

fn ctx(members: &[(Tag, Value)], tag: u8) -> Option<&Value> {
    members
        .iter()
        .find(|(t, _)| *t == Tag::Context(tag))
        .map(|(_, v)| v)
}

fn opt_u64_list(v: Option<&Vec<u64>>) -> Value {
    match v {
        None => Value::Null,
        Some(xs) => Value::Array(xs.iter().map(|x| Value::Uint(*x)).collect()),
    }
}

fn target_value(t: &AclTarget) -> Value {
    Value::Structure(vec![
        (
            Tag::Context(TAG_TARGET_CLUSTER),
            t.cluster.map_or(Value::Null, |c| Value::Uint(u64::from(c))),
        ),
        (
            Tag::Context(TAG_TARGET_ENDPOINT),
            t.endpoint
                .map_or(Value::Null, |e| Value::Uint(u64::from(e))),
        ),
        (
            Tag::Context(TAG_TARGET_DEVICE_TYPE),
            t.device_type
                .map_or(Value::Null, |d| Value::Uint(u64::from(d))),
        ),
    ])
}

// ── encode ───────────────────────────────────────────────────────────────────

/// Encode one ACL entry as an anonymous-tagged `Value::Structure` using the
/// spec context tags (privilege=1, auth-mode=2, subjects=3, targets=4,
/// fabric-index=254). The `fabric_index` field is omitted when `None` (write
/// path: the device fills it in for the accessing fabric).
pub(crate) fn acl_entry_value(e: &AclEntry) -> Value {
    let mut m = vec![
        (
            Tag::Context(TAG_PRIVILEGE),
            Value::Uint(u64::from(e.privilege.to_raw())),
        ),
        (
            Tag::Context(TAG_AUTH_MODE),
            Value::Uint(u64::from(e.auth_mode.to_raw())),
        ),
        (
            Tag::Context(TAG_SUBJECTS),
            opt_u64_list(e.subjects.as_ref()),
        ),
        (
            Tag::Context(TAG_TARGETS),
            match &e.targets {
                None => Value::Null,
                Some(ts) => Value::Array(ts.iter().map(target_value).collect()),
            },
        ),
    ];
    if let Some(fi) = e.fabric_index {
        m.push((Tag::Context(TAG_FABRIC_INDEX), Value::Uint(u64::from(fi))));
    }
    Value::Structure(m)
}

// ── parse ────────────────────────────────────────────────────────────────────

fn parse_target(v: &Value) -> Option<AclTarget> {
    let m = struct_members(v)?;
    #[allow(clippy::cast_possible_truncation)]
    // Target fields are spec-typed: ClusterId=uint32, EndpointNo=uint16,
    // DeviceTypeId=uint32. Truncation from u64 to the spec width is correct for
    // all valid wire values.
    Some(AclTarget {
        cluster: match ctx(m, TAG_TARGET_CLUSTER) {
            Some(Value::Uint(u)) => Some(*u as u32),
            _ => None,
        },
        endpoint: match ctx(m, TAG_TARGET_ENDPOINT) {
            Some(Value::Uint(u)) => Some(*u as u16),
            _ => None,
        },
        device_type: match ctx(m, TAG_TARGET_DEVICE_TYPE) {
            Some(Value::Uint(u)) => Some(*u as u32),
            _ => None,
        },
    })
}

fn parse_entry(v: &Value) -> Option<AclEntry> {
    let m = struct_members(v)?;
    #[allow(clippy::cast_possible_truncation)]
    // ACL entry fields are spec-typed: Privilege/AuthMode = enum8 (uint8);
    // FabricIndex = uint8. Truncation from u64 to u8 is correct for all
    // valid wire values.
    Some(AclEntry {
        privilege: AclPrivilege::from_raw(match ctx(m, TAG_PRIVILEGE)? {
            Value::Uint(u) => *u as u8,
            _ => return None,
        }),
        auth_mode: AclAuthMode::from_raw(match ctx(m, TAG_AUTH_MODE)? {
            Value::Uint(u) => *u as u8,
            _ => return None,
        }),
        // Subjects and Targets are nullable, and present-null means wildcard.
        // An ABSENT tag means the device withheld the field (another fabric's
        // entry on an unfiltered read). Absent must never collapse into
        // wildcard: a caller that wrote the entry back would grant the
        // privilege to every subject/target. So an absent (or wrong-typed)
        // field drops the whole entry. chip always encodes both fields, null
        // or array, for the accessing fabric's own entries.
        subjects: match ctx(m, TAG_SUBJECTS)? {
            Value::Null => None,
            Value::Array(a) => Some(
                a.iter()
                    .filter_map(|x| {
                        if let Value::Uint(u) = x {
                            Some(*u)
                        } else {
                            None
                        }
                    })
                    .collect(),
            ),
            _ => return None,
        },
        targets: match ctx(m, TAG_TARGETS)? {
            Value::Null => None,
            Value::Array(a) => Some(a.iter().filter_map(parse_target).collect()),
            _ => return None,
        },
        fabric_index: match ctx(m, TAG_FABRIC_INDEX) {
            Some(Value::Uint(u)) => Some(*u as u8),
            _ => None,
        },
    })
}

/// Parse the `ACL` list attribute (0x0000) from a read result.
///
/// Searches `reports` for the attribute path whose `cluster` field equals
/// [`ACCESS_CONTROL_CLUSTER`] and whose `attribute` field equals [`ATTR_ACL`],
/// then decodes each `AccessControlEntryStruct` inside it.
/// Malformed entries are silently skipped, and so is any entry missing its
/// Privilege, `AuthMode`, Subjects or Targets tag: a withheld field (another
/// fabric's entry) is never read as a wildcard. Returns an empty `Vec` when the
/// attribute is absent or contains no decodable entries (infallible).
pub(crate) fn parse_acl(reports: &[(AttributePath, Value)]) -> Vec<AclEntry> {
    for (path, value) in reports {
        if path.cluster == ACCESS_CONTROL_CLUSTER && path.attribute == ATTR_ACL {
            if let Value::Array(items) = value {
                return items.iter().filter_map(parse_entry).collect();
            }
        }
    }
    Vec::new()
}

// ── lockout guard ─────────────────────────────────────────────────────────────

/// Returns `true` iff `entries` retains administrative access for `our_node_id`.
///
/// An entry "retains admin" when:
/// - `privilege == Administer`
/// - `auth_mode == Case`
/// - `subjects` is `None` (wildcard — covers all CASE principals) **or**
///   `subjects` contains `our_node_id`
///
/// An empty slice returns `false`.
pub(crate) fn acl_retains_admin(entries: &[AclEntry], our_node_id: u64) -> bool {
    entries.iter().any(|e| {
        e.privilege == AclPrivilege::Administer
            && e.auth_mode == AclAuthMode::Case
            && match &e.subjects {
                None => true,
                Some(s) => s.contains(&our_node_id),
            }
    })
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use matter_codec::{TlvReader, TlvWriter};

    fn admin(node: u64) -> AclEntry {
        AclEntry {
            privilege: AclPrivilege::Administer,
            auth_mode: AclAuthMode::Case,
            subjects: Some(vec![node]),
            targets: None,
            fabric_index: None,
        }
    }

    #[test]
    fn entry_value_uses_spec_tags() {
        let v = acl_entry_value(&admin(0x1234));
        let Value::Structure(m) = v else {
            panic!("expected Structure")
        };
        // Tag::Context(1) = privilege, Administer = 5
        assert_eq!(m[0], (Tag::Context(1), Value::Uint(5)));
        // Tag::Context(2) = auth_mode, CASE = 2
        assert_eq!(m[1], (Tag::Context(2), Value::Uint(2)));
        // Tag::Context(3) = subjects list
        assert_eq!(
            m[2],
            (Tag::Context(3), Value::Array(vec![Value::Uint(0x1234)]))
        );
        // Tag::Context(4) = targets, None → Null
        assert_eq!(m[3], (Tag::Context(4), Value::Null));
        // fabric_index None ⇒ tag 254 omitted
        assert!(m.iter().all(|(t, _)| *t != Tag::Context(254)));
    }

    #[test]
    fn lockout_guard_truth_table() {
        // Our node id is in the subject list ⇒ retained.
        assert!(acl_retains_admin(&[admin(7)], 7));

        // Wildcard subjects (None) ⇒ covers us regardless of node id.
        let wild = AclEntry {
            subjects: None,
            ..admin(0)
        };
        assert!(acl_retains_admin(&[wild], 7));

        // Different node id ⇒ not retained.
        assert!(!acl_retains_admin(&[admin(9)], 7));

        // Empty entry list ⇒ not retained.
        assert!(!acl_retains_admin(&[], 7));

        // Operate privilege (not Administer) ⇒ not retained.
        let op = AclEntry {
            privilege: AclPrivilege::Operate,
            ..admin(7)
        };
        assert!(!acl_retains_admin(&[op], 7));

        // PASE auth mode (not CASE) ⇒ not retained.
        let pase = AclEntry {
            auth_mode: AclAuthMode::Pase,
            ..admin(7)
        };
        assert!(!acl_retains_admin(&[pase], 7));
    }

    #[test]
    fn parse_acl_roundtrips_through_codec() {
        // Build two entries, encode to TLV, decode back to Value, then parse.
        let entries = [
            admin(7),
            AclEntry {
                privilege: AclPrivilege::Operate,
                auth_mode: AclAuthMode::Case,
                subjects: Some(vec![1, 2]),
                targets: Some(vec![AclTarget {
                    cluster: Some(6),
                    endpoint: Some(1),
                    device_type: None,
                }]),
                fabric_index: Some(1),
            },
        ];

        let arr = Value::Array(entries.iter().map(acl_entry_value).collect());

        let mut buf = Vec::new();
        TlvWriter::new(&mut buf)
            .write_value(Tag::Anonymous, &arr)
            .unwrap();

        // read_value() returns Result<(Tag, Value)>; we want only the Value.
        let (_, decoded) = TlvReader::new(&buf).read_value().unwrap();

        let path = AttributePath {
            endpoint: 0,
            cluster: ACCESS_CONTROL_CLUSTER,
            attribute: ATTR_ACL,
        };
        let parsed = parse_acl(&[(path, decoded)]);

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].privilege, AclPrivilege::Administer);
        assert_eq!(parsed[0].auth_mode, AclAuthMode::Case);
        assert_eq!(parsed[0].subjects, Some(vec![7]));
        assert_eq!(parsed[0].targets, None);

        assert_eq!(parsed[1].privilege, AclPrivilege::Operate);
        let targets = parsed[1].targets.as_ref().unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].cluster, Some(6));
        assert_eq!(targets[0].endpoint, Some(1));
        assert_eq!(targets[0].device_type, None);
        assert_eq!(parsed[1].fabric_index, Some(1));
    }

    /// Parse a single raw entry structure through the public-to-crate path
    /// (`parse_acl`), exactly as `Node::read_acl` does.
    fn parse_one(members: Vec<(Tag, Value)>) -> Vec<AclEntry> {
        let path = AttributePath {
            endpoint: 0,
            cluster: ACCESS_CONTROL_CLUSTER,
            attribute: ATTR_ACL,
        };
        parse_acl(&[(path, Value::Array(vec![Value::Structure(members)]))])
    }

    fn privilege_and_auth() -> Vec<(Tag, Value)> {
        vec![
            (Tag::Context(TAG_PRIVILEGE), Value::Uint(5)),
            (Tag::Context(TAG_AUTH_MODE), Value::Uint(2)),
        ]
    }

    #[test]
    fn entry_without_subjects_tag_is_dropped_not_wildcarded() {
        // A withheld Subjects field must never read back as `None` (wildcard):
        // writing it back would grant the privilege to every CASE node.
        let mut m = privilege_and_auth();
        m.push((Tag::Context(TAG_TARGETS), Value::Null));
        m.push((Tag::Context(TAG_FABRIC_INDEX), Value::Uint(2)));
        assert_eq!(parse_one(m), []);
    }

    #[test]
    fn entry_without_targets_tag_is_dropped_not_wildcarded() {
        let mut m = privilege_and_auth();
        m.push((
            Tag::Context(TAG_SUBJECTS),
            Value::Array(vec![Value::Uint(7)]),
        ));
        m.push((Tag::Context(TAG_FABRIC_INDEX), Value::Uint(2)));
        assert_eq!(parse_one(m), []);
    }

    #[test]
    fn present_null_subjects_and_targets_are_wildcards() {
        let mut m = privilege_and_auth();
        m.push((Tag::Context(TAG_SUBJECTS), Value::Null));
        m.push((Tag::Context(TAG_TARGETS), Value::Null));
        m.push((Tag::Context(TAG_FABRIC_INDEX), Value::Uint(1)));
        let parsed = parse_one(m);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].subjects, None);
        assert_eq!(parsed[0].targets, None);
        assert_eq!(parsed[0].fabric_index, Some(1));
    }

    #[test]
    fn chip_shaped_other_fabric_entry_is_dropped() {
        // chip `AccessControl/Structs.ipp` without `includeSensitive`: only
        // the FabricIndex (254) is encoded.
        assert_eq!(
            parse_one(vec![(Tag::Context(TAG_FABRIC_INDEX), Value::Uint(2))]),
            []
        );
    }

    #[test]
    fn wrong_typed_subjects_or_targets_is_dropped_not_wildcarded() {
        // Neither Null nor an array: malformed, so skipped like any other
        // malformed entry rather than read as a wildcard.
        let mut m = privilege_and_auth();
        m.push((Tag::Context(TAG_SUBJECTS), Value::Uint(7)));
        m.push((Tag::Context(TAG_TARGETS), Value::Null));
        assert_eq!(parse_one(m), []);

        let mut m = privilege_and_auth();
        m.push((Tag::Context(TAG_SUBJECTS), Value::Null));
        m.push((Tag::Context(TAG_TARGETS), Value::Bool(true)));
        assert_eq!(parse_one(m), []);
    }

    #[test]
    fn constructors_build_writable_entries() {
        let t = AclTarget::new(Some(6), Some(1), None);
        assert_eq!(t.cluster, Some(6));
        let e = AclEntry::new(
            AclPrivilege::Administer,
            AclAuthMode::Case,
            Some(vec![7]),
            Some(vec![t]),
        );
        assert_eq!(e.privilege, AclPrivilege::Administer);
        assert_eq!(e.subjects, Some(vec![7]));
        // fabric_index defaults to None (device assigns it on write).
        assert_eq!(e.fabric_index, None);
        // The constructed entry round-trips through the encoder.
        assert!(matches!(acl_entry_value(&e), Value::Structure(_)));
    }
}
