//! `GroupKeyManagement` (0x003F) + `Groups` (0x0004) controller support: provisioning
//! types and the pure Value codecs the group verbs compose. Decoder-agnostic
//! (hand-built Value); the generated matter-clusters encoders are the byte-parity
//! oracle.

use matter_codec::{Tag, Value};

/// Cluster ID for `GroupKeyManagement` (Matter §11.2).
// Not yet wired to a caller outside tests; will be used by the group verbs task.
#[allow(dead_code)]
pub(crate) const GROUP_KEY_MANAGEMENT_CLUSTER: u32 = 0x003F;
/// Cluster ID for `Groups` (Matter §1.3).
#[allow(dead_code)]
pub(crate) const GROUPS_CLUSTER: u32 = 0x0004;
/// `KeySetWrite` command ID (`GroupKeyManagement`).
#[allow(dead_code)]
pub(crate) const CMD_KEY_SET_WRITE: u32 = 0x00;
/// `KeySetRemove` command ID (`GroupKeyManagement`).
#[allow(dead_code)]
pub(crate) const CMD_KEY_SET_REMOVE: u32 = 0x03;
/// `GroupKeyMap` attribute ID (`GroupKeyManagement`).
#[allow(dead_code)]
pub(crate) const ATTR_GROUP_KEY_MAP: u32 = 0x0000;
/// `AddGroup` command ID (`Groups`).
#[allow(dead_code)]
pub(crate) const CMD_ADD_GROUP: u32 = 0x00;
/// `RemoveGroup` command ID (`Groups`).
#[allow(dead_code)]
pub(crate) const CMD_REMOVE_GROUP: u32 = 0x03;
/// `TrustFirst` group key security policy (the only one we provision).
#[allow(dead_code)]
pub(crate) const SECURITY_POLICY_TRUST_FIRST: u64 = 0;

/// A group key set to provision via `KeySetWrite` (Matter §11.2.6.1).
///
/// Only a single epoch key (key0) is populated; keys 1 and 2 are left as
/// `Null` per the single-epoch provisioning pattern used by most controllers.
///
/// `Debug` is written by hand so that `epoch_key` prints as `<redacted>`:
/// anyone holding the epoch key can read and forge the group's traffic, so it
/// must never reach a log through a stray `{:?}`.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GroupKeySet {
    /// Key set id (must be ≥ 1; 0 is reserved for the IPK).
    pub key_set_id: u16,
    /// 16-byte epoch key (the group's symmetric key material).
    /// Secret: this struct's `Debug` output shows `<redacted>` instead.
    pub epoch_key: Vec<u8>,
    /// Epoch start time (Matter epoch microseconds).
    pub epoch_start_time: u64,
}

impl std::fmt::Debug for GroupKeySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Same name, fields and order as the derive this replaces; only the
        // key's value is withheld.
        f.debug_struct("GroupKeySet")
            .field("key_set_id", &self.key_set_id)
            .field("epoch_key", &"<redacted>")
            .field("epoch_start_time", &self.epoch_start_time)
            .finish()
    }
}

impl GroupKeySet {
    /// Construct a group key set. `epoch_key` must be 16 bytes.
    #[must_use]
    pub fn new(key_set_id: u16, epoch_key: Vec<u8>, epoch_start_time: u64) -> Self {
        Self {
            key_set_id,
            epoch_key,
            epoch_start_time,
        }
    }
}

/// One `GroupKeyMap` entry binding a group id to a key set (Matter §11.2.6.x).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GroupKeyMapEntry {
    /// Group id.
    pub group_id: u16,
    /// Key set id this group uses.
    pub group_key_set_id: u16,
}

impl GroupKeyMapEntry {
    /// Construct a group-key-map entry.
    #[must_use]
    pub fn new(group_id: u16, group_key_set_id: u16) -> Self {
        Self {
            group_id,
            group_key_set_id,
        }
    }
}

/// Build the `KeySetWrite` command fields (one field at tag 0 = the `GroupKeySet`
/// struct). `epochKey1`/2 + `startTime1`/2 are emitted as `Null` (single-key set).
#[allow(dead_code)]
pub(crate) fn key_set_write_fields(set: &GroupKeySet) -> Value {
    let key_set = Value::Structure(vec![
        (Tag::Context(0), Value::Uint(u64::from(set.key_set_id))),
        (Tag::Context(1), Value::Uint(SECURITY_POLICY_TRUST_FIRST)),
        (Tag::Context(2), Value::Bytes(set.epoch_key.clone())),
        (Tag::Context(3), Value::Uint(set.epoch_start_time)),
        (Tag::Context(4), Value::Null),
        (Tag::Context(5), Value::Null),
        (Tag::Context(6), Value::Null),
        (Tag::Context(7), Value::Null),
    ]);
    Value::Structure(vec![(Tag::Context(0), key_set)])
}

/// Build one `GroupKeyMapStruct` element (`group_id` t1, `key_set_id` t2). `fabric_index`
/// (t254) is omitted on write — the device assigns the accessing fabric.
#[allow(dead_code)]
pub(crate) fn group_key_map_entry_value(e: GroupKeyMapEntry) -> Value {
    Value::Structure(vec![
        (Tag::Context(1), Value::Uint(u64::from(e.group_id))),
        (Tag::Context(2), Value::Uint(u64::from(e.group_key_set_id))),
    ])
}

/// Build the `AddGroup` command fields (`group_id` t0, `group_name` t1).
#[allow(dead_code)]
pub(crate) fn add_group_fields(group_id: u16, name: &str) -> Value {
    Value::Structure(vec![
        (Tag::Context(0), Value::Uint(u64::from(group_id))),
        (Tag::Context(1), Value::Utf8(name.to_string())),
    ])
}

/// Build the `RemoveGroup` command fields (`group_id` t0).
#[allow(dead_code)]
pub(crate) fn remove_group_fields(group_id: u16) -> Value {
    Value::Structure(vec![(Tag::Context(0), Value::Uint(u64::from(group_id)))])
}

/// Parse the `status` (context tag 0) from a `Groups` response-command fields struct.
///
/// Returns `u8::MAX` if absent/malformed (treated as a non-success rejection).
#[allow(dead_code)]
pub(crate) fn parse_group_status(fields: &Value) -> u8 {
    let members = match fields {
        Value::Structure(m) | Value::List(m) => m.as_slice(),
        _ => return u8::MAX,
    };
    members
        .iter()
        .find(|(t, _)| *t == Tag::Context(0))
        .and_then(|(_, v)| {
            if let Value::Uint(n) = v {
                u8::try_from(*n).ok()
            } else {
                None
            }
        })
        .unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use matter_codec::TlvWriter;

    use super::*;

    fn enc(v: &Value) -> Vec<u8> {
        let mut b = Vec::new();
        #[allow(clippy::unwrap_used)] // test: Vec writer is infallible
        TlvWriter::new(&mut b)
            .write_value(Tag::Anonymous, v)
            .unwrap();
        b
    }

    #[test]
    fn key_set_write_matches_generated_encoder() {
        use matter_clusters::gen::group_key_management::{
            encode_key_set_write, GroupKeySecurityPolicyEnum, GroupKeySetStruct,
        };
        use matter_clusters::types::Nullable;
        let epoch = vec![0xABu8; 16];
        let ours = enc(&key_set_write_fields(&GroupKeySet::new(
            42,
            epoch.clone(),
            0,
        )));
        let theirs = encode_key_set_write(GroupKeySetStruct {
            group_key_set_id: 42,
            group_key_security_policy: GroupKeySecurityPolicyEnum::TrustFirst,
            epoch_key0: Nullable::Value(epoch),
            epoch_start_time0: Nullable::Value(0),
            epoch_key1: Nullable::Null,
            epoch_start_time1: Nullable::Null,
            epoch_key2: Nullable::Null,
            epoch_start_time2: Nullable::Null,
            group_key_multicast_policy: None,
            fabric_index: None,
        });
        assert_eq!(
            ours, theirs,
            "KeySetWrite fields must byte-match the generated encoder"
        );
    }

    #[test]
    fn group_key_map_entry_matches_generated_encoder() {
        // The generated `GroupKeyMapStruct` encoder writes the entry as a
        // write sends it, without `fabric_index` (t254): the device assigns
        // the accessing fabric. Our builder must produce the same bytes.
        use matter_clusters::clusters::group_key_management::GroupKeyMapStruct;
        let ours = enc(&group_key_map_entry_value(GroupKeyMapEntry::new(7, 42)));
        let theirs = GroupKeyMapStruct {
            group_id: 7,
            group_key_set_id: 42,
            fabric_index: 0, // not written
        }
        .encode();
        assert_eq!(
            ours, theirs,
            "GroupKeyMap entry must byte-match the generated encoder"
        );
        assert!(
            !ours.windows(2).any(|w| w == [0x24, 0xFE]),
            "write must not emit fabric_index"
        );
    }

    #[test]
    fn add_group_and_status() {
        let f = add_group_fields(7, "kitchen");
        let Value::Structure(m) = f else { panic!() };
        assert_eq!(m[0], (Tag::Context(0), Value::Uint(7)));
        assert_eq!(m[1], (Tag::Context(1), Value::Utf8("kitchen".into())));
        let resp = Value::Structure(vec![
            (Tag::Context(0), Value::Uint(0)),
            (Tag::Context(1), Value::Uint(7)),
        ]);
        assert_eq!(parse_group_status(&resp), 0);
        assert_eq!(
            parse_group_status(&Value::Structure(vec![(Tag::Context(0), Value::Uint(137))])),
            137
        );
    }
}
