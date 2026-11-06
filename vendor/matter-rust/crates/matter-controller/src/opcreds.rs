//! `OperationalCredentials` (0x003E) controller support: types and the pure
//! Value codecs the fabric-management `Node` verbs compose.

use crate::error::Error;
use matter_codec::{Tag, Value};
use matter_interaction::AttributePath;

/// Cluster ID for `OperationalCredentials` (Matter spec §11.18).
pub(crate) const OPERATIONAL_CREDENTIALS_CLUSTER: u32 = 0x003E;
/// Command id for `UpdateFabricLabel` (§11.18.6.9).
pub(crate) const CMD_UPDATE_FABRIC_LABEL: u32 = 0x09;
/// Command id for `RemoveFabric` (§11.18.6.10).
pub(crate) const CMD_REMOVE_FABRIC: u32 = 0x0A;
/// Attribute id for `Fabrics` (§11.18.4.5).
pub(crate) const ATTR_FABRICS: u32 = 0x0001;
/// Attribute id for `CurrentFabricIndex` (§11.18.4.6).
pub(crate) const ATTR_CURRENT_FABRIC_INDEX: u32 = 0x0005;

// FabricDescriptorStruct context tags (Matter spec §11.18.4.5).
const TAG_ROOT_PUBLIC_KEY: u8 = 1;
const TAG_VENDOR_ID: u8 = 2;
const TAG_FABRIC_ID: u8 = 3;
const TAG_NODE_ID: u8 = 4;
const TAG_LABEL: u8 = 5;
const TAG_FABRIC_INDEX: u8 = 254;

// NOCResponse context tags (Matter spec §11.18.5.10).
const TAG_NOC_STATUS: u8 = 0;
const TAG_NOC_FABRIC_INDEX: u8 = 1;
const TAG_NOC_DEBUG_TEXT: u8 = 2;

/// One fabric a device belongs to (a decoded `FabricDescriptorStruct`).
///
/// Parsed from the `Fabrics` attribute (0x0001) of the `OperationalCredentials`
/// cluster (0x003E). Unknown fields from the wire are silently ignored —
/// `#[non_exhaustive]` lets us add fields without a breaking change.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FabricDescriptor {
    /// Fabric's root public key (SEC1 uncompressed P-256, 65 bytes).
    pub root_public_key: Vec<u8>,
    /// Vendor ID of the admin that created this fabric.
    pub vendor_id: u16,
    /// 64-bit Fabric ID.
    pub fabric_id: u64,
    /// The device's node ID on this fabric.
    pub node_id: u64,
    /// Operator-assigned label (may be empty).
    pub label: String,
    /// The device-assigned fabric index (1-based, device-local).
    pub fabric_index: u8,
}

/// Decoded `NOCResponse` command fields.
///
/// Returned by the device after an `AddNOC`, `UpdateNOC`, `UpdateFabricLabel`,
/// or `RemoveFabric` command (Matter spec §11.18.5.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NocStatus {
    /// `NodeOperationalCertStatusEnum` — 0 = OK.
    pub status: u8,
    /// The fabric index affected (present on success).
    pub fabric_index: Option<u8>,
    /// Optional human-readable diagnostic text.
    pub debug_text: Option<String>,
}

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

fn parse_fabric_descriptor(v: &Value) -> Option<FabricDescriptor> {
    let m = struct_members(v)?;
    #[allow(clippy::cast_possible_truncation)]
    // FabricDescriptorStruct fields are spec-typed: VendorId=uint16, FabricIndex=uint8.
    // The device MUST send values within range; truncation to the spec width is correct.
    Some(FabricDescriptor {
        root_public_key: match ctx(m, TAG_ROOT_PUBLIC_KEY)? {
            Value::Bytes(b) => b.clone(),
            _ => return None,
        },
        vendor_id: match ctx(m, TAG_VENDOR_ID)? {
            Value::Uint(u) => *u as u16,
            _ => return None,
        },
        fabric_id: match ctx(m, TAG_FABRIC_ID)? {
            Value::Uint(u) => *u,
            _ => return None,
        },
        node_id: match ctx(m, TAG_NODE_ID)? {
            Value::Uint(u) => *u,
            _ => return None,
        },
        label: match ctx(m, TAG_LABEL) {
            Some(Value::Utf8(s)) => s.clone(),
            _ => String::new(),
        },
        fabric_index: match ctx(m, TAG_FABRIC_INDEX)? {
            Value::Uint(u) => *u as u8,
            _ => return None,
        },
    })
}

/// Parse the `Fabrics` list attribute into descriptors. Malformed entries are skipped.
///
/// Returns an empty `Vec` when the attribute is absent or contains no decodable
/// entries (infallible).
pub(crate) fn parse_fabrics(reports: &[(AttributePath, Value)]) -> Vec<FabricDescriptor> {
    for (path, value) in reports {
        if path.attribute == ATTR_FABRICS {
            if let Value::Array(items) = value {
                return items.iter().filter_map(parse_fabric_descriptor).collect();
            }
        }
    }
    Vec::new()
}

/// Parse `CurrentFabricIndex` from a read result.
///
/// Returns `None` when the attribute is absent or has an unexpected type.
pub(crate) fn parse_current_fabric_index(reports: &[(AttributePath, Value)]) -> Option<u8> {
    for (path, value) in reports {
        if path.attribute == ATTR_CURRENT_FABRIC_INDEX {
            if let Value::Uint(u) = value {
                #[allow(clippy::cast_possible_truncation)]
                // CurrentFabricIndex is spec-typed as fabric-idx (uint8); truncation correct.
                return Some(*u as u8);
            }
        }
    }
    None
}

/// Parse a `NOCResponse` command-fields struct.
///
/// Returns a sentinel `NocStatus { status: u8::MAX, .. }` when `fields` is not
/// a struct or the status tag is missing.
pub(crate) fn parse_noc_response(fields: &Value) -> NocStatus {
    let m = struct_members(fields).unwrap_or(&[]);
    #[allow(clippy::cast_possible_truncation)]
    // NOCResponse fields are spec-typed: StatusCode=enum8, FabricIndex=uint8.
    // Truncation to u8 is correct for all valid wire values.
    NocStatus {
        status: match ctx(m, TAG_NOC_STATUS) {
            Some(Value::Uint(u)) => *u as u8,
            _ => u8::MAX,
        },
        fabric_index: match ctx(m, TAG_NOC_FABRIC_INDEX) {
            Some(Value::Uint(u)) => Some(*u as u8),
            _ => None,
        },
        debug_text: match ctx(m, TAG_NOC_DEBUG_TEXT) {
            Some(Value::Utf8(s)) => Some(s.clone()),
            _ => None,
        },
    }
}

/// Map a `NocStatus` to `Ok(())` on success (status 0) or a rejection error.
///
/// # Errors
///
/// Returns [`Error::OperationalCredentialsRejected`] when `s.status != 0`.
pub(crate) fn noc_status_to_result(s: &NocStatus) -> Result<(), Error> {
    if s.status == 0 {
        Ok(())
    } else {
        Err(Error::OperationalCredentialsRejected(s.status))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    fn ap(a: u32) -> AttributePath {
        AttributePath {
            endpoint: 0,
            cluster: OPERATIONAL_CREDENTIALS_CLUSTER,
            attribute: a,
        }
    }

    fn fabric_struct(idx: u8, fid: u64, label: &str) -> Value {
        Value::Structure(vec![
            (
                Tag::Context(TAG_ROOT_PUBLIC_KEY),
                Value::Bytes(vec![4u8; 65]),
            ),
            (Tag::Context(TAG_VENDOR_ID), Value::Uint(0xFFF1)),
            (Tag::Context(TAG_FABRIC_ID), Value::Uint(fid)),
            (Tag::Context(TAG_NODE_ID), Value::Uint(0x1122_3344)),
            (Tag::Context(TAG_LABEL), Value::Utf8(label.into())),
            (Tag::Context(TAG_FABRIC_INDEX), Value::Uint(u64::from(idx))),
        ])
    }

    #[test]
    fn parse_fabrics_decodes_array_of_structs() {
        let reports = vec![(
            ap(ATTR_FABRICS),
            Value::Array(vec![
                fabric_struct(1, 100, "home"),
                fabric_struct(2, 200, ""),
            ]),
        )];
        let f = parse_fabrics(&reports);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].fabric_index, 1);
        assert_eq!(f[0].fabric_id, 100);
        assert_eq!(f[0].label, "home");
        assert_eq!(f[0].root_public_key.len(), 65);
        assert_eq!(f[1].fabric_index, 2);
        assert_eq!(f[1].label, "");
    }

    #[test]
    fn parse_current_fabric_index_reads_u8() {
        let reports = vec![(ap(ATTR_CURRENT_FABRIC_INDEX), Value::Uint(3))];
        assert_eq!(parse_current_fabric_index(&reports), Some(3));
        assert_eq!(parse_current_fabric_index(&[]), None);
    }

    #[test]
    fn noc_response_success_and_failure() {
        let ok = Value::Structure(vec![
            (Tag::Context(0), Value::Uint(0)),
            (Tag::Context(1), Value::Uint(2)),
        ]);
        let s = parse_noc_response(&ok);
        assert_eq!(s.status, 0);
        assert_eq!(s.fabric_index, Some(2));
        assert!(noc_status_to_result(&s).is_ok());

        let bad = Value::Structure(vec![(Tag::Context(0), Value::Uint(7))]);
        let s2 = parse_noc_response(&bad);
        assert!(matches!(
            noc_status_to_result(&s2),
            Err(Error::OperationalCredentialsRejected(7))
        ));
    }
}
