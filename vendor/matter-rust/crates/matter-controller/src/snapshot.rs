//! Versioned TLV serialization of [`ControllerState`].
//!
//! Layout (all members context-tagged):
//! ```text
//! root: Structure { C0: version(u8), C1: Array[fabric] }
//! fabric: Structure {
//!   C0: fabric_id(uint), C1: ipk(bytes16), C2: rcac_cert(bytes,TLV),
//!   C3: rcac_pkcs8(bytes), C4: commissioner(struct), C5: Array[device],
//!   C6: Array[group_key_set] (optional — absent in v1 snapshots written
//!       before group-key support; defaults to empty on read),
//!   C7: outbound_group_counter(uint, optional — same default rule),
//!   C8: Array[icd_registration] (optional — same default rule),
//!   C9: icac_cert(bytes,TLV, optional — present only when the fabric has
//!       an ICAC), C10: icac_pkcs8(bytes, optional — present iff C9 is)
//! }
//! commissioner: Structure { C0: node_id(uint), C1: op_pkcs8(bytes), C2: noc(bytes,TLV) }
//! device: Structure {
//!   C0: node_id(uint), C1: peer_noc_public_key(bytes65),
//!   C2: resumption_record(bytes, optional), C3: last_known_addr(utf8, optional),
//!   C4: vendor_id(uint, optional), C5: product_id(uint, optional),
//!   C6: label(utf8, optional) — all three additive/ICAC-style, absent means
//!       `None`
//! }
//! group_key_set: Structure {
//!   C0: key_set_id(uint), C1: epoch_key(bytes16), C2: epoch_start_time(uint)
//! }
//! ```
//!
//! ## Backward compatibility
//!
//! Fabric-struct C6, C7, C8, C9, and C10 are optional tags: a v1 snapshot
//! without them (written by an older binary) still deserializes cleanly —
//! `group_keys` defaults to `Vec::new()`, `outbound_group_counter` to `0`,
//! `icd_clients` to `Vec::new()`, and `icac` to `None`.  C9/C10 are
//! themselves only written when a fabric has an ICAC, so a fabric without
//! one round-trips to byte-identical output regardless of ICAC support
//! existing in the binary.
//!
//! Device-struct C4, C5, and C6 (`vendor_id`/`product_id`/`label`) follow the same
//! rule: they're only written when the corresponding field is `Some`, and a
//! device struct without them deserializes to `vendor_id`/`product_id`/
//! `label` all `None`.
//!
//! No version bump is needed for any of the above because the tag-keyed
//! deserializer simply treats absent tags as defaults.

use matter_cert::MatterCertificate;
use matter_codec::{Tag, TlvWriter, Value};

use crate::error::Error;
use crate::state::{
    CommissionerIdentity, ControllerState, DeviceEntry, FabricEntry, GroupKeySetConfig,
    IcacIdentity,
};

/// Current snapshot schema version.
pub(crate) const SNAPSHOT_VERSION: u8 = 1;

/// Serialize controller state into an opaque TLV blob.
///
/// # Errors
///
/// Returns [`Error::Cert`] if a certificate fails to serialize, or
/// [`Error::Codec`] if TLV encoding fails.
pub(crate) fn serialize(state: &ControllerState) -> Result<Vec<u8>, Error> {
    let mut fabrics = Vec::with_capacity(state.fabrics.len());
    for f in &state.fabrics {
        fabrics.push(fabric_to_value(f)?);
    }
    let root = Value::Structure(vec![
        (Tag::Context(0), Value::Uint(u64::from(SNAPSHOT_VERSION))),
        (Tag::Context(1), Value::Array(fabrics)),
    ]);

    let mut out = Vec::new();
    let mut w = TlvWriter::new(&mut out);
    w.write_value(Tag::Anonymous, &root)?;
    Ok(out)
}

fn fabric_to_value(f: &FabricEntry) -> Result<Value, Error> {
    let devices = f.devices.iter().map(device_to_value).collect();
    let group_keys: Vec<Value> = f.group_keys.iter().map(group_key_to_value).collect();
    let icd_clients: Vec<Value> = f
        .icd_clients
        .iter()
        .map(icd_registration_to_value)
        .collect();
    let mut members = vec![
        (Tag::Context(0), Value::Uint(f.fabric_id)),
        (Tag::Context(1), Value::Bytes(f.ipk.to_vec())),
        (Tag::Context(2), Value::Bytes(f.rcac_cert.to_tlv()?)),
        (Tag::Context(3), Value::Bytes(f.rcac_pkcs8.clone())),
        (Tag::Context(4), commissioner_to_value(&f.commissioner)?),
        (Tag::Context(5), Value::Array(devices)),
        (Tag::Context(6), Value::Array(group_keys)),
        (
            Tag::Context(7),
            Value::Uint(u64::from(f.outbound_group_counter)),
        ),
        (Tag::Context(8), Value::Array(icd_clients)),
    ];
    // C9/C10 (ICAC) are omitted entirely when the fabric has no ICAC, so a
    // fabric without one serializes to byte-identical output regardless of
    // ICAC support existing in the binary (backward compatibility).
    if let Some(icac) = &f.icac {
        members.push((Tag::Context(9), Value::Bytes(icac.cert.to_tlv()?)));
        members.push((Tag::Context(10), Value::Bytes(icac.pkcs8.clone())));
    }
    Ok(Value::Structure(members))
}

fn icd_registration_to_value(r: &crate::icd::IcdRegistration) -> Value {
    Value::Structure(vec![
        (Tag::Context(0), Value::Uint(r.node_id)),
        (Tag::Context(1), Value::Uint(r.check_in_node_id)),
        (Tag::Context(2), Value::Uint(r.monitored_subject)),
        (Tag::Context(3), Value::Bytes(r.key.to_vec())),
        (Tag::Context(4), Value::Uint(u64::from(r.start_counter))),
    ])
}

fn group_key_to_value(k: &GroupKeySetConfig) -> Value {
    Value::Structure(vec![
        (Tag::Context(0), Value::Uint(u64::from(k.key_set_id))),
        (Tag::Context(1), Value::Bytes(k.epoch_key.to_vec())),
        (Tag::Context(2), Value::Uint(k.epoch_start_time)),
    ])
}

fn commissioner_to_value(c: &CommissionerIdentity) -> Result<Value, Error> {
    Ok(Value::Structure(vec![
        (Tag::Context(0), Value::Uint(c.node_id)),
        (Tag::Context(1), Value::Bytes(c.operational_pkcs8.clone())),
        (Tag::Context(2), Value::Bytes(c.noc.to_tlv()?)),
    ]))
}

fn device_to_value(d: &DeviceEntry) -> Value {
    let mut members = vec![
        (Tag::Context(0), Value::Uint(d.node_id)),
        (
            Tag::Context(1),
            Value::Bytes(d.peer_noc_public_key.to_vec()),
        ),
    ];
    if let Some(rr) = &d.resumption_record {
        members.push((Tag::Context(2), Value::Bytes(rr.clone())));
    }
    if let Some(addr) = &d.last_known_addr {
        members.push((Tag::Context(3), Value::Utf8(addr.clone())));
    }
    if let Some(vid) = d.vendor_id {
        members.push((Tag::Context(4), Value::Uint(u64::from(vid))));
    }
    if let Some(pid) = d.product_id {
        members.push((Tag::Context(5), Value::Uint(u64::from(pid))));
    }
    if let Some(label) = &d.label {
        members.push((Tag::Context(6), Value::Utf8(label.clone())));
    }
    Value::Structure(members)
}

/// Deserialize a snapshot blob into [`ControllerState`].
///
/// # Errors
///
/// Returns [`Error::Snapshot`] if the structure or version is invalid,
/// [`Error::Codec`] on TLV decode failure, or [`Error::Cert`] if an
/// embedded certificate fails to parse.
pub(crate) fn deserialize(bytes: &[u8]) -> Result<ControllerState, Error> {
    use matter_codec::TlvReader;

    let mut r = TlvReader::new(bytes);
    let (_tag, value) = r.read_value()?;
    let root = as_struct(&value)?;

    let version = get_uint(root, 0)?;
    if version != u64::from(SNAPSHOT_VERSION) {
        return Err(Error::Snapshot(format!(
            "unsupported snapshot version {version}"
        )));
    }

    let fabrics_val =
        get(root, 1).ok_or_else(|| Error::Snapshot("missing fabrics array".into()))?;
    let mut fabrics = Vec::new();
    for fv in as_array(fabrics_val)? {
        fabrics.push(fabric_from_value(fv)?);
    }
    Ok(ControllerState { fabrics })
}

fn fabric_from_value(v: &Value) -> Result<FabricEntry, Error> {
    let m = as_struct(v)?;
    let commissioner_val =
        get(m, 4).ok_or_else(|| Error::Snapshot("missing commissioner".into()))?;
    let devices_val = get(m, 5).ok_or_else(|| Error::Snapshot("missing devices array".into()))?;
    let mut devices = Vec::new();
    for dv in as_array(devices_val)? {
        devices.push(device_from_value(dv)?);
    }

    // t6 / t7 are optional (absent in v1 snapshots written before group-key
    // support was added).  Default to empty / 0 when missing — this keeps old
    // stores loadable without a version bump.
    let group_keys = match get(m, 6) {
        Some(arr) => {
            let mut keys = Vec::new();
            for kv in as_array(arr)? {
                keys.push(group_key_from_value(kv)?);
            }
            keys
        }
        None => Vec::new(),
    };
    let outbound_group_counter = match get(m, 7) {
        Some(Value::Uint(n)) => u32::try_from(*n)
            .map_err(|_| Error::Snapshot("outbound_group_counter exceeds u32 range".into()))?,
        _ => 0,
    };
    // t8 (ICD registrations) is optional — absent in snapshots written before
    // ICD support. Default to empty (no version bump).
    let icd_clients = match get(m, 8) {
        Some(arr) => {
            let mut regs = Vec::new();
            for rv in as_array(arr)? {
                regs.push(icd_registration_from_value(rv)?);
            }
            regs
        }
        None => Vec::new(),
    };

    // C9/C10 (ICAC) are optional and only present together — absent in
    // snapshots written before ICAC support, or for a fabric that never
    // adopted one. Only C9-present-and-C10-present decodes to `Some`;
    // anything else (both absent, or one without the other) decodes to
    // `None` rather than erroring, matching the other optional tags' rule
    // of "absent means default".
    let icac = match (get(m, 9), get(m, 10)) {
        (Some(Value::Bytes(cert_tlv)), Some(Value::Bytes(pkcs8))) => Some(IcacIdentity {
            cert: MatterCertificate::from_tlv(cert_tlv)?,
            pkcs8: pkcs8.clone(),
        }),
        _ => None,
    };

    Ok(FabricEntry {
        fabric_id: get_uint(m, 0)?,
        ipk: byte_array::<16>(get_bytes(m, 1)?, "ipk")?,
        rcac_cert: MatterCertificate::from_tlv(get_bytes(m, 2)?)?,
        rcac_pkcs8: get_bytes(m, 3)?.to_vec(),
        commissioner: commissioner_from_value(commissioner_val)?,
        devices,
        group_keys,
        outbound_group_counter,
        icd_clients,
        icac,
    })
}

fn icd_registration_from_value(v: &Value) -> Result<crate::icd::IcdRegistration, Error> {
    let m = as_struct(v)?;
    let start_counter = u32::try_from(get_uint(m, 4)?)
        .map_err(|_| Error::Snapshot("icd start_counter exceeds u32 range".into()))?;
    Ok(crate::icd::IcdRegistration::new(
        get_uint(m, 0)?,
        get_uint(m, 1)?,
        get_uint(m, 2)?,
        byte_array::<16>(get_bytes(m, 3)?, "icd key")?,
        start_counter,
    ))
}

fn group_key_from_value(v: &Value) -> Result<GroupKeySetConfig, Error> {
    let m = as_struct(v)?;
    let key_set_id = u16::try_from(get_uint(m, 0)?)
        .map_err(|_| Error::Snapshot("key_set_id exceeds u16 range".into()))?;
    let epoch_key = byte_array::<16>(get_bytes(m, 1)?, "epoch_key")?;
    let epoch_start_time = get_uint(m, 2)?;
    Ok(GroupKeySetConfig::new(
        key_set_id,
        epoch_key,
        epoch_start_time,
    ))
}

fn commissioner_from_value(v: &Value) -> Result<CommissionerIdentity, Error> {
    let m = as_struct(v)?;
    Ok(CommissionerIdentity {
        node_id: get_uint(m, 0)?,
        operational_pkcs8: get_bytes(m, 1)?.to_vec(),
        noc: MatterCertificate::from_tlv(get_bytes(m, 2)?)?,
    })
}

fn device_from_value(v: &Value) -> Result<DeviceEntry, Error> {
    let m = as_struct(v)?;
    let resumption_record = match get(m, 2) {
        Some(Value::Bytes(b)) => Some(b.clone()),
        _ => None,
    };
    let last_known_addr = match get(m, 3) {
        Some(Value::Utf8(s)) => Some(s.clone()),
        _ => None,
    };
    // C4/C5/C6 are optional tags (absent in snapshots written before device
    // metadata support): a v1 device struct with only t0..t3 still
    // deserializes cleanly, defaulting vendor_id/product_id/label to `None`
    // — same additive-optional-tag discipline as C6/C7 (group keys) and
    // C9/C10 (ICAC) above.
    let vendor_id = match get(m, 4) {
        Some(Value::Uint(n)) => u16::try_from(*n).ok(),
        _ => None,
    };
    let product_id = match get(m, 5) {
        Some(Value::Uint(n)) => u16::try_from(*n).ok(),
        _ => None,
    };
    let label = match get(m, 6) {
        Some(Value::Utf8(s)) => Some(s.clone()),
        _ => None,
    };
    Ok(DeviceEntry {
        node_id: get_uint(m, 0)?,
        peer_noc_public_key: byte_array::<65>(get_bytes(m, 1)?, "peer_noc_public_key")?,
        resumption_record,
        last_known_addr,
        vendor_id,
        product_id,
        label,
    })
}

// --- small TLV-Value accessors ---

fn as_struct(v: &Value) -> Result<&[(Tag, Value)], Error> {
    match v {
        Value::Structure(members) => Ok(members),
        _ => Err(Error::Snapshot("expected structure".into())),
    }
}

fn as_array(v: &Value) -> Result<&[Value], Error> {
    match v {
        Value::Array(items) => Ok(items),
        _ => Err(Error::Snapshot("expected array".into())),
    }
}

fn get(members: &[(Tag, Value)], ctx: u8) -> Option<&Value> {
    members
        .iter()
        .find(|(t, _)| *t == Tag::Context(ctx))
        .map(|(_, v)| v)
}

fn get_uint(members: &[(Tag, Value)], ctx: u8) -> Result<u64, Error> {
    match get(members, ctx) {
        Some(Value::Uint(n)) => Ok(*n),
        _ => Err(Error::Snapshot(format!(
            "missing or non-uint at context {ctx}"
        ))),
    }
}

fn get_bytes(members: &[(Tag, Value)], ctx: u8) -> Result<&[u8], Error> {
    match get(members, ctx) {
        Some(Value::Bytes(b)) => Ok(b),
        _ => Err(Error::Snapshot(format!(
            "missing or non-bytes at context {ctx}"
        ))),
    }
}

fn byte_array<const N: usize>(b: &[u8], field: &str) -> Result<[u8; N], Error> {
    b.try_into()
        .map_err(|_| Error::Snapshot(format!("{field}: expected {N} bytes, got {}", b.len())))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests with justification (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::fabric::{create_fabric, FabricConfig};
    use crate::store::{ControllerStore, FileStore};
    use matter_cert::MatterTime;
    use matter_commissioning::SystemNocRng;
    use matter_crypto::Signer as _;

    /// A unique temp path per (process, call) — a fixed shared path races when
    /// two test processes (or an overlapping re-run) touch the same file.
    fn temp_path(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let uniq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "matter-controller-restart-{name}-{}-{uniq}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("tmp"));
        p
    }

    /// Acceptance test (white-box: exercises the crate-internal `create_fabric`,
    /// `serialize`/`deserialize`, and signer reconstruction): a fabric minted,
    /// persisted via `FileStore`, and reloaded yields a byte-identical
    /// commissioner identity whose key still signs.
    #[test]
    fn commissioner_identity_is_stable_across_restart() {
        let cfg = FabricConfig::new(
            0x0102_0304_0506_0708,
            1,
            0x0000_0000_0000_0001,
            (
                MatterTime::from_unix_secs(1_700_000_000),
                MatterTime::NO_EXPIRY,
            ),
        );
        let fabric = create_fabric(&cfg, &SystemNocRng).expect("create_fabric");
        let original_state = ControllerState::new(vec![fabric]);

        // "First boot": serialize and persist.
        let path = temp_path("identity");
        let store = FileStore::new(&path);
        store
            .save(&serialize(&original_state).expect("serialize"))
            .expect("save");

        // "Second boot": load and deserialize from disk.
        let loaded = store.load().expect("load").expect("snapshot present");
        let restored = deserialize(&loaded).expect("deserialize");

        let before = &original_state.fabrics[0];
        let after = &restored.fabrics[0];

        assert_eq!(after.commissioner.node_id, before.commissioner.node_id);
        assert_eq!(
            after.commissioner.noc.to_tlv().unwrap(),
            before.commissioner.noc.to_tlv().unwrap(),
            "commissioner NOC must survive restart byte-for-byte"
        );

        // The reloaded operational key still signs and matches the NOC.
        let signer = after
            .commissioner_signer()
            .expect("reload commissioner signer");
        assert_eq!(
            signer.public_key().as_bytes(),
            after.commissioner.noc.public_key().as_bytes()
        );
        let sig_bytes = signer.sign_p256_sha256(b"post-restart").expect("sign");
        let sig = matter_cert::Signature::new(sig_bytes);
        signer
            .public_key()
            .verify(b"post-restart", &sig)
            .expect("post-restart signature verifies");

        // The reconstructed FabricRecord is usable (RCAC signer reloads).
        let record = after.to_fabric_record().expect("to_fabric_record");
        assert_eq!(record.fabric_id, cfg.fabric_id);

        let _ = std::fs::remove_file(&path);
    }

    fn sample_state() -> ControllerState {
        let cfg = FabricConfig {
            fabric_id: 0x1122_3344_5566_7788,
            rcac_id: 1,
            commissioner_node_id: 0x0000_0000_0000_0001,
            validity: (
                MatterTime::from_unix_secs(1_700_000_000),
                MatterTime::NO_EXPIRY,
            ),
            issue_icac: false,
        };
        let mut fabric = create_fabric(&cfg, &SystemNocRng).expect("create_fabric");
        fabric.devices.push(DeviceEntry {
            node_id: 0xABCD,
            peer_noc_public_key: [0x04; 65],
            resumption_record: Some(vec![1, 2, 3, 4]),
            last_known_addr: Some("[fe80::1]:5540".to_string()),
            vendor_id: None,
            product_id: None,
            label: None,
        });
        fabric.devices.push(DeviceEntry {
            node_id: 0xBEEF,
            peer_noc_public_key: [0x04; 65],
            resumption_record: None,
            last_known_addr: None,
            vendor_id: None,
            product_id: None,
            label: None,
        });
        ControllerState {
            fabrics: vec![fabric],
        }
    }

    #[test]
    fn round_trips_a_full_state() {
        let state = sample_state();
        let bytes = serialize(&state).expect("serialize");
        let back = deserialize(&bytes).expect("deserialize");

        assert_eq!(back.fabrics.len(), 1);
        let (a, b) = (&state.fabrics[0], &back.fabrics[0]);
        assert_eq!(a.fabric_id, b.fabric_id);
        assert_eq!(a.ipk, b.ipk);
        assert_eq!(a.rcac_pkcs8, b.rcac_pkcs8);
        assert_eq!(a.rcac_cert.to_tlv().unwrap(), b.rcac_cert.to_tlv().unwrap());
        assert_eq!(a.commissioner.node_id, b.commissioner.node_id);
        assert_eq!(
            a.commissioner.operational_pkcs8,
            b.commissioner.operational_pkcs8
        );
        assert_eq!(
            a.commissioner.noc.to_tlv().unwrap(),
            b.commissioner.noc.to_tlv().unwrap()
        );
        assert_eq!(a.devices.len(), b.devices.len());
        assert_eq!(a.devices[0].node_id, b.devices[0].node_id);
        assert_eq!(
            a.devices[0].resumption_record,
            b.devices[0].resumption_record
        );
        assert_eq!(a.devices[0].last_known_addr, b.devices[0].last_known_addr);
        assert_eq!(a.devices[1].resumption_record, None);
        assert_eq!(a.devices[1].last_known_addr, None);
    }

    #[test]
    fn empty_state_round_trips() {
        let bytes = serialize(&ControllerState::default()).expect("serialize");
        assert!(deserialize(&bytes).expect("deserialize").fabrics.is_empty());
    }

    #[test]
    fn rejects_unknown_version() {
        // Hand-build a root with version 99.
        let root = Value::Structure(vec![
            (Tag::Context(0), Value::Uint(99)),
            (Tag::Context(1), Value::Array(vec![])),
        ]);
        let mut out = Vec::new();
        let mut w = TlvWriter::new(&mut out);
        w.write_value(Tag::Anonymous, &root).unwrap();
        let err = deserialize(&out).expect_err("must reject");
        assert!(matches!(err, Error::Snapshot(_)));
    }

    // --- property-based round-trip (every encoder gets a proptest) ---

    use proptest::prelude::*;
    use std::sync::OnceLock;

    /// Mint one real fabric and reuse it across all proptest cases — key
    /// generation is expensive, but cloning a `FabricEntry` is cheap.
    fn shared_fabric() -> &'static FabricEntry {
        static FABRIC: OnceLock<FabricEntry> = OnceLock::new();
        FABRIC.get_or_init(|| {
            let cfg = FabricConfig {
                fabric_id: 0x0102_0304_0506_0708,
                rcac_id: 1,
                commissioner_node_id: 0x0000_0000_0000_0001,
                validity: (
                    MatterTime::from_unix_secs(1_700_000_000),
                    MatterTime::NO_EXPIRY,
                ),
                issue_icac: false,
            };
            create_fabric(&cfg, &SystemNocRng).expect("mint shared fabric")
        })
    }

    prop_compose! {
        fn arb_device()(
            node_id in any::<u64>(),
            pk in prop::collection::vec(any::<u8>(), 65),
            rr in prop::option::of(prop::collection::vec(any::<u8>(), 0..40)),
            addr in prop::option::of("[ -~]{0,32}"),
        ) -> DeviceEntry {
            let mut peer_noc_public_key = [0u8; 65];
            peer_noc_public_key.copy_from_slice(&pk);
            DeviceEntry {
                node_id,
                peer_noc_public_key,
                resumption_record: rr,
                last_known_addr: addr,
                vendor_id: None,
                product_id: None,
                label: None,
            }
        }
    }

    proptest! {
        /// `deserialize(serialize(state)) == state` for arbitrary device lists.
        #[test]
        fn snapshot_round_trips(devices in prop::collection::vec(arb_device(), 0..6)) {
            let mut fabric = shared_fabric().clone();
            fabric.devices = devices.clone();
            let state = ControllerState { fabrics: vec![fabric] };

            let bytes = serialize(&state).expect("serialize");
            let back = deserialize(&bytes).expect("deserialize");

            prop_assert_eq!(back.fabrics.len(), 1);
            let dev_back = &back.fabrics[0].devices;
            prop_assert_eq!(dev_back.len(), devices.len());
            for (a, b) in devices.iter().zip(dev_back.iter()) {
                prop_assert_eq!(a.node_id, b.node_id);
                prop_assert_eq!(a.peer_noc_public_key, b.peer_noc_public_key);
                prop_assert_eq!(&a.resumption_record, &b.resumption_record);
                prop_assert_eq!(&a.last_known_addr, &b.last_known_addr);
            }
        }
    }

    // --- group-key persistence tests ---

    #[test]
    fn group_keys_round_trip() {
        // A FabricEntry WITH group_keys and a non-zero counter must survive
        // serialize → deserialize with all group fields preserved.
        let mut fabric = shared_fabric().clone();
        fabric.group_keys = vec![
            GroupKeySetConfig::new(0x0001, [0xAA; 16], 1_700_000_000),
            GroupKeySetConfig::new(0x0002, [0xBB; 16], 1_700_100_000),
        ];
        fabric.outbound_group_counter = 42;
        let state = ControllerState {
            fabrics: vec![fabric.clone()],
        };

        let bytes = serialize(&state).expect("serialize");
        let back = deserialize(&bytes).expect("deserialize");

        assert_eq!(back.fabrics.len(), 1);
        let f = &back.fabrics[0];
        assert_eq!(f.outbound_group_counter, 42);
        assert_eq!(f.group_keys.len(), 2);
        assert_eq!(f.group_keys[0].key_set_id, 0x0001);
        assert_eq!(f.group_keys[0].epoch_key, [0xAA; 16]);
        assert_eq!(f.group_keys[0].epoch_start_time, 1_700_000_000);
        assert_eq!(f.group_keys[1].key_set_id, 0x0002);
        assert_eq!(f.group_keys[1].epoch_key, [0xBB; 16]);
        assert_eq!(f.group_keys[1].epoch_start_time, 1_700_100_000);
    }

    #[test]
    fn icd_clients_round_trip() {
        // A FabricEntry WITH ICD registrations must survive serialize →
        // deserialize with all fields preserved (additive t8, no version bump).
        let mut fabric = shared_fabric().clone();
        fabric.icd_clients = vec![
            crate::icd::IcdRegistration::new(0x0042, 1, 1, [0xCC; 16], 7),
            crate::icd::IcdRegistration::new(0x0043, 1, 2, [0xDD; 16], 99),
        ];
        let state = ControllerState {
            fabrics: vec![fabric.clone()],
        };
        let bytes = serialize(&state).expect("serialize");
        let back = deserialize(&bytes).expect("deserialize");
        assert_eq!(back.fabrics[0].icd_clients, fabric.icd_clients);
    }

    // --- ICAC persistence (C9/C10) ---

    /// Build a throwaway ICAC cert (signed by `fabric`'s RCAC key) + a fresh
    /// PKCS#8 signing key for it, wrapped as an [`IcacIdentity`].
    fn sample_icac(fabric: &FabricEntry) -> crate::state::IcacIdentity {
        use matter_cert::operational::{icac, sign_with_ring, IcacParams};
        use matter_cert::PublicKey;
        use matter_crypto::{RingSigner, Signer};

        let (icac_signer, icac_pkcs8) = RingSigner::generate().expect("generate icac key");
        let icac_public_key =
            PublicKey::new(*icac_signer.public_key().as_bytes()).expect("valid P-256 public key");
        let issuer_skid = fabric
            .rcac_cert
            .extensions()
            .subject_key_identifier
            .expect("rcac has SKID");

        let unsigned = icac(IcacParams::new(
            0x0000_0000_0000_0099,
            fabric.rcac_cert.subject().clone(),
            issuer_skid,
            icac_public_key,
            vec![0x01],
            MatterTime::from_unix_secs(1_700_000_000),
            MatterTime::NO_EXPIRY,
        ))
        .expect("build unsigned icac");
        let cert = sign_with_ring(unsigned, &fabric.rcac_pkcs8).expect("sign icac");

        crate::state::IcacIdentity {
            cert,
            pkcs8: icac_pkcs8,
        }
    }

    #[test]
    fn snapshot_round_trips_fabric_with_icac() {
        // A FabricEntry WITH an ICAC must survive serialize -> deserialize
        // with both the cert and the signing key preserved (C9/C10).
        let mut fabric = shared_fabric().clone();
        let icac_identity = sample_icac(&fabric);
        fabric.icac = Some(icac_identity);

        let state = ControllerState {
            fabrics: vec![fabric.clone()],
        };
        let bytes = serialize(&state).expect("serialize");
        let back = deserialize(&bytes).expect("deserialize");

        let want = fabric.icac.as_ref().expect("icac set on input fabric");
        let got = back.fabrics[0]
            .icac
            .as_ref()
            .expect("icac must round-trip as Some");
        assert_eq!(got.cert.to_tlv().unwrap(), want.cert.to_tlv().unwrap());
        assert_eq!(got.pkcs8, want.pkcs8);
    }

    #[test]
    fn snapshot_without_icac_is_backward_compatible() {
        // A FabricEntry with icac = None must (a) round-trip to None, and
        // (b) serialize to bytes containing no C9/C10 tags at all — the
        // encoding must be identical to what pre-ICAC code produced.
        let fabric = shared_fabric().clone();
        assert!(fabric.icac.is_none());
        let state = ControllerState {
            fabrics: vec![fabric],
        };

        let bytes = serialize(&state).expect("serialize");
        let back = deserialize(&bytes).expect("deserialize");
        assert!(back.fabrics[0].icac.is_none());

        // Context tags 9 and 10 must not appear anywhere in the fabric
        // structure's encoded member list. Re-decode the fabric Value
        // directly and check its tag set rather than grepping raw bytes
        // (TLV tag/length bytes can coincidentally match arbitrary byte
        // patterns elsewhere in the blob).
        let mut r = matter_codec::TlvReader::new(&bytes);
        let (_tag, root) = r.read_value().expect("read root");
        let root_members = as_struct(&root).expect("root struct");
        let fabrics_arr = get(root_members, 1).expect("fabrics array");
        let first_fabric = &as_array(fabrics_arr).expect("fabrics array")[0];
        let fabric_members = as_struct(first_fabric).expect("fabric struct");
        assert!(
            get(fabric_members, 9).is_none(),
            "C9 (icac_cert) must be absent when icac is None"
        );
        assert!(
            get(fabric_members, 10).is_none(),
            "C10 (icac_pkcs8) must be absent when icac is None"
        );
    }

    // --- device metadata (vendor_id/product_id/label, C4/C5/C6) ---

    #[test]
    fn device_metadata_round_trips_and_defaults_none() {
        // A DeviceEntry with vendor_id/product_id/label set must survive
        // serialize -> deserialize.
        let mut fabric = shared_fabric().clone();
        fabric.devices = vec![DeviceEntry {
            node_id: 0x1234,
            peer_noc_public_key: [0x04; 65],
            resumption_record: None,
            last_known_addr: None,
            vendor_id: Some(0xFFF1),
            product_id: Some(0x8000),
            label: Some("kitchen plug".to_string()),
        }];
        let state = ControllerState {
            fabrics: vec![fabric],
        };
        let bytes = serialize(&state).expect("serialize");
        let back = deserialize(&bytes).expect("deserialize");
        let d = &back.fabrics[0].devices[0];
        assert_eq!(d.vendor_id, Some(0xFFF1));
        assert_eq!(d.product_id, Some(0x8000));
        assert_eq!(d.label.as_deref(), Some("kitchen plug"));

        // A device Value::Structure carrying ONLY tags 0+1 (the old layout,
        // written before vendor_id/product_id/label existed) must
        // deserialize to all three fields `None` — this is the back-compat
        // proof for the additive C4/C5/C6 tags.
        let old_device_val = Value::Structure(vec![
            (Tag::Context(0), Value::Uint(0x9999)),
            (Tag::Context(1), Value::Bytes(vec![0x04; 65])),
        ]);
        let old_device = device_from_value(&old_device_val).expect("old device must load");
        assert_eq!(old_device.vendor_id, None);
        assert_eq!(old_device.product_id, None);
        assert_eq!(old_device.label, None);
    }

    #[test]
    fn old_snapshot_without_t6_t7_loads_with_defaults() {
        // Simulate a v1 snapshot written by old code: a fabric value that has
        // only t0..t5 (no t6/t7).  The deserializer must accept it and default
        // group_keys to [] and outbound_group_counter to 0.
        let fabric = shared_fabric().clone();

        // Build the fabric Value manually with only t0..t5 (the old layout).
        let old_fabric_val = Value::Structure(vec![
            (Tag::Context(0), Value::Uint(fabric.fabric_id)),
            (Tag::Context(1), Value::Bytes(fabric.ipk.to_vec())),
            (
                Tag::Context(2),
                Value::Bytes(fabric.rcac_cert.to_tlv().expect("rcac tlv")),
            ),
            (Tag::Context(3), Value::Bytes(fabric.rcac_pkcs8.clone())),
            (
                Tag::Context(4),
                commissioner_to_value(&fabric.commissioner).expect("commissioner"),
            ),
            (Tag::Context(5), Value::Array(vec![])), // no devices
        ]);

        let root = Value::Structure(vec![
            (Tag::Context(0), Value::Uint(u64::from(SNAPSHOT_VERSION))),
            (Tag::Context(1), Value::Array(vec![old_fabric_val])),
        ]);

        let mut out = Vec::new();
        let mut w = TlvWriter::new(&mut out);
        w.write_value(Tag::Anonymous, &root).unwrap();

        let back = deserialize(&out).expect("old snapshot must load without error");
        assert_eq!(back.fabrics.len(), 1);
        let f = &back.fabrics[0];
        assert!(
            f.group_keys.is_empty(),
            "group_keys must default to empty for old snapshot"
        );
        assert_eq!(
            f.outbound_group_counter, 0,
            "outbound_group_counter must default to 0 for old snapshot"
        );
    }
}
