//! (De)serialization of [`matter_crypto::ResumptionRecord`] for the opaque
//! `DeviceEntry.resumption_record` bytes (snapshot context tag 2).
//!
//! The record is persisted after every completed CASE connect so a peer that
//! later initiates CASE *to us* (the OTA requestor querying our provider
//! server is the canonical case) can present its resumption id and be matched
//! against the stored record via `CaseResponder::accept_resumption`.
//!
//! Layout (anonymous TLV structure; all tags context-numbered):
//!
//! | tag | field                | type                     |
//! |-----|----------------------|--------------------------|
//! | 0   | id                   | bytes (16)               |
//! | 1   | `shared_secret`      | bytes (32)               |
//! | 2   | peer `node_id`       | uint                     |
//! | 3   | peer `fabric_id`     | uint                     |
//! | 4   | peer NOC             | bytes (Matter cert TLV)  |
//! | 5   | peer `session_id`    | uint                     |
//! | 6   | `expires_at`         | uint (optional; Matter epoch seconds) |
//! | 7   | local identity       | bytes (32, optional)     |
//!
//! # Tag 7 — the commissioner-identity fingerprint
//!
//! SHA-256 of the commissioner NOC that was in force when the record was
//! written. A record is only usable while that identity still holds.
//!
//! Resumption trades certificate verification for a cached authorization
//! snapshot: the resumed session is bound to the node id and CATs captured at
//! the time, and the resume MIC only proves both sides hold the same ECDH
//! secret. That secret is derived from *ephemeral* keys and stays
//! cryptographically valid across a NOC change, so it cannot detect one. Any
//! event that changes the authorization snapshot — node id, CATs, or the
//! operational key — must therefore invalidate every cached snapshot on that
//! fabric. The specification says as much for the peer's side, and chip quotes
//! it at its own wipe site: *"All internal data reflecting the prior
//! operational identifier of the Node within the Fabric SHALL be revoked and
//! removed."*
//!
//! chip and matter.js both enforce this by *remembering to call* a wipe from
//! every identity-mutating path — chip through a `FabricTable` delegate
//! (`ClearCASEResumptionStateOnFabricChange`, fired on removal AND update),
//! matter.js imperatively from `FailsafeContext.replaceFabric`, whose
//! `SessionManager` subscribes only to fabric *deletion*. Binding needs no
//! delegate and no call-site discipline: a record minted under a superseded
//! identity simply does not match and is never used. It also covers a case
//! neither reference handles — restoring a snapshot taken under a different
//! commissioner identity.
//!
//! The fingerprint is over the whole NOC rather than the identity-relevant
//! fields (node id, CATs, public key). A benign re-issue with an identical
//! identity therefore costs one unnecessary full handshake, which is the safe
//! direction, and the check stays three lines with no false negatives.

use matter_cert::{MatterCertificate, MatterTime};
use matter_codec::{Tag, TlvReader, TlvWriter, Value};
use matter_crypto::{PeerInfo, ResumptionId, ResumptionRecord};

use crate::error::Error;

/// A stored record plus the identity it was minted under.
pub(crate) struct StoredResumption {
    /// The record itself.
    pub record: ResumptionRecord,
    /// SHA-256 of the commissioner NOC in force when it was written, or `None`
    /// for a record persisted before the binding existed.
    ///
    /// A `None` is treated as a mismatch by
    /// [`is_usable_under`](Self::is_usable_under): an unverifiable record is
    /// not a usable one, and the cost of rejecting it is a single full
    /// handshake, once, per device, on upgrade.
    pub local_identity: Option<[u8; 32]>,
}

impl StoredResumption {
    /// Whether this record may be offered or accepted under `current`.
    pub(crate) fn is_usable_under(&self, current: &[u8; 32]) -> bool {
        self.local_identity.as_ref() == Some(current)
    }
}

/// Fingerprint a commissioner identity: SHA-256 over its NOC's Matter TLV.
///
/// # Errors
///
/// [`Error::Cert`] if the certificate fails to re-serialize.
pub(crate) fn identity_fingerprint(noc: &MatterCertificate) -> Result<[u8; 32], Error> {
    let tlv = noc.to_tlv()?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &tlv);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    Ok(out)
}

/// Serialize a [`ResumptionRecord`] into the opaque bytes stored in
/// `DeviceEntry.resumption_record`.
///
/// # Errors
///
/// [`Error::Cert`] if the peer NOC fails to re-serialize, or [`Error::Codec`]
/// on TLV encoding failure.
pub(crate) fn serialize_record(
    record: &ResumptionRecord,
    local_identity: Option<&[u8; 32]>,
) -> Result<Vec<u8>, Error> {
    let mut members = vec![
        (Tag::Context(0), Value::Bytes(record.id.0.to_vec())),
        (Tag::Context(1), Value::Bytes(record.shared_secret.to_vec())),
        (Tag::Context(2), Value::Uint(record.peer.node_id)),
        (Tag::Context(3), Value::Uint(record.peer.fabric_id)),
        (Tag::Context(4), Value::Bytes(record.peer.noc.to_tlv()?)),
        (
            Tag::Context(5),
            Value::Uint(u64::from(record.peer.session_id)),
        ),
    ];
    if let Some(t) = record.expires_at {
        members.push((Tag::Context(6), Value::Uint(t.to_unix_secs())));
    }
    if let Some(fp) = local_identity {
        members.push((Tag::Context(7), Value::Bytes(fp.to_vec())));
    }
    let mut out = Vec::new();
    let mut w = TlvWriter::new(&mut out);
    w.write_value(Tag::Anonymous, &Value::Structure(members))?;
    Ok(out)
}

/// Deserialize the opaque `DeviceEntry.resumption_record` bytes back into a
/// [`ResumptionRecord`].
///
/// # Errors
///
/// [`Error::Snapshot`] if the structure is malformed, [`Error::Codec`] on TLV
/// decode failure, or [`Error::Cert`] if the embedded peer NOC fails to parse.
pub(crate) fn deserialize_record(bytes: &[u8]) -> Result<StoredResumption, Error> {
    let mut r = TlvReader::new(bytes);
    let (_tag, value) = r.read_value()?;
    let Value::Structure(members) = value else {
        return Err(Error::Snapshot(
            "resumption record: expected structure".into(),
        ));
    };
    let get = |ctx: u8| {
        members
            .iter()
            .find(|(t, _)| *t == Tag::Context(ctx))
            .map(|(_, v)| v)
    };
    let get_bytes = |ctx: u8| match get(ctx) {
        Some(Value::Bytes(b)) => Ok(b.as_slice()),
        _ => Err(Error::Snapshot(format!(
            "resumption record: missing or non-bytes at context {ctx}"
        ))),
    };
    let get_uint = |ctx: u8| match get(ctx) {
        Some(Value::Uint(n)) => Ok(*n),
        _ => Err(Error::Snapshot(format!(
            "resumption record: missing or non-uint at context {ctx}"
        ))),
    };

    let id: [u8; 16] = get_bytes(0)?
        .try_into()
        .map_err(|_| Error::Snapshot("resumption record: id must be 16 bytes".into()))?;
    let shared_secret: [u8; 32] = get_bytes(1)?
        .try_into()
        .map_err(|_| Error::Snapshot("resumption record: secret must be 32 bytes".into()))?;
    let node_id = get_uint(2)?;
    let fabric_id = get_uint(3)?;
    let noc = MatterCertificate::from_tlv(get_bytes(4)?)?;
    let session_id = u16::try_from(get_uint(5)?)
        .map_err(|_| Error::Snapshot("resumption record: session_id exceeds u16".into()))?;
    let expires_at = match get(6) {
        Some(Value::Uint(secs)) => Some(MatterTime::from_unix_secs(*secs)),
        _ => None,
    };

    // A tag 7 of the wrong length is treated as absent rather than as an
    // error: an unusable fingerprint and a missing one lead to the same
    // outcome (drop the record, full handshake), and failing the whole
    // deserialize would turn a cosmetic problem into a hard read error.
    let local_identity = match get(7) {
        Some(Value::Bytes(b)) => <[u8; 32]>::try_from(b.as_slice()).ok(),
        _ => None,
    };

    Ok(StoredResumption {
        record: ResumptionRecord {
            id: ResumptionId(id),
            shared_secret,
            peer: PeerInfo {
                node_id,
                fabric_id,
                noc,
                session_id,
            },
            expires_at,
        },
        local_identity,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    /// Build a minimal record around a real certificate (reuses the fabric
    /// factory so the NOC round-trips through `to_tlv`/`from_tlv`).
    fn sample_record(expires_at: Option<MatterTime>) -> ResumptionRecord {
        let fabric = crate::fabric::create_fabric(
            &crate::fabric::FabricConfig {
                fabric_id: 0x1122_3344_5566_7788,
                rcac_id: 1,
                commissioner_node_id: 1,
                validity: (
                    MatterTime::from_unix_secs(1_700_000_000),
                    MatterTime::NO_EXPIRY,
                ),
                issue_icac: false,
            },
            &matter_commissioning::SystemNocRng,
        )
        .unwrap();
        ResumptionRecord {
            id: ResumptionId([0xA5; 16]),
            shared_secret: [0x5A; 32],
            peer: PeerInfo {
                node_id: 0xDEAD_BEEF,
                fabric_id: 0x1122_3344_5566_7788,
                noc: fabric.commissioner.noc.clone(),
                session_id: 0x1234,
            },
            expires_at,
        }
    }

    #[test]
    fn record_round_trips() {
        let record = sample_record(None);
        let bytes = serialize_record(&record, None).unwrap();
        let back = deserialize_record(&bytes).unwrap();
        assert_eq!(back.record.id, record.id);
        assert_eq!(back.record.shared_secret, record.shared_secret);
        assert_eq!(back.record.peer.node_id, record.peer.node_id);
        assert_eq!(back.record.peer.fabric_id, record.peer.fabric_id);
        assert_eq!(back.record.peer.session_id, record.peer.session_id);
        assert_eq!(
            back.record.peer.noc.to_tlv().unwrap(),
            record.peer.noc.to_tlv().unwrap()
        );
        assert_eq!(back.record.expires_at, record.expires_at);
    }

    #[test]
    fn record_round_trips_with_expiry() {
        let record = sample_record(Some(MatterTime::from_unix_secs(2_000_000_000)));
        let bytes = serialize_record(&record, None).unwrap();
        let back = deserialize_record(&bytes).unwrap();
        assert_eq!(back.record.expires_at, record.expires_at);
    }

    /// A record written with a fingerprint must round-trip it, and only match
    /// the identity it was minted under.
    #[test]
    fn identity_fingerprint_round_trips_and_gates_reuse() {
        let record = sample_record(None);
        let mint = [0xABu8; 32];
        let other = [0xCDu8; 32];

        let bytes = serialize_record(&record, Some(&mint)).unwrap();
        let back = deserialize_record(&bytes).unwrap();
        assert_eq!(back.local_identity, Some(mint));
        assert!(back.is_usable_under(&mint));
        assert!(
            !back.is_usable_under(&other),
            "a record from a superseded identity must not be usable"
        );
    }

    /// A record written before the binding existed carries no fingerprint, and
    /// an unverifiable record is not a usable one. The cost of refusing it is
    /// one full handshake, once, per device, on upgrade — which is a bounded
    /// price for never having to reason about whether a legacy record predates
    /// some identity change.
    #[test]
    fn a_record_without_a_fingerprint_is_never_usable() {
        let record = sample_record(None);
        let bytes = serialize_record(&record, None).unwrap();
        let back = deserialize_record(&bytes).unwrap();
        assert_eq!(back.local_identity, None);
        assert!(!back.is_usable_under(&[0xABu8; 32]));
    }

    /// The fingerprint must actually distinguish identities: two different
    /// NOCs must not collide, and the same NOC must hash stably.
    #[test]
    fn fingerprints_are_stable_and_distinguishing() {
        let a = sample_record(None);
        let b = sample_record(None);
        let fa = identity_fingerprint(&a.peer.noc).unwrap();
        assert_eq!(
            fa,
            identity_fingerprint(&a.peer.noc).unwrap(),
            "the same certificate must hash to the same value"
        );
        assert_ne!(
            fa,
            identity_fingerprint(&b.peer.noc).unwrap(),
            "distinct commissioner identities must not collide"
        );
    }

    /// A tag 7 of the wrong length is treated as "no fingerprint", not as a
    /// read error: both lead to the same outcome, and failing the deserialize
    /// would turn a cosmetic problem into an unreadable device entry.
    #[test]
    fn a_wrong_length_fingerprint_reads_as_absent() {
        let record = sample_record(None);
        let members = vec![
            (Tag::Context(0), Value::Bytes(record.id.0.to_vec())),
            (Tag::Context(1), Value::Bytes(record.shared_secret.to_vec())),
            (Tag::Context(2), Value::Uint(record.peer.node_id)),
            (Tag::Context(3), Value::Uint(record.peer.fabric_id)),
            (
                Tag::Context(4),
                Value::Bytes(record.peer.noc.to_tlv().unwrap()),
            ),
            (Tag::Context(5), Value::Uint(0)),
            (Tag::Context(7), Value::Bytes(vec![0xAB; 16])),
        ];
        let mut out = Vec::new();
        let mut w = TlvWriter::new(&mut out);
        w.write_value(Tag::Anonymous, &Value::Structure(members))
            .unwrap();
        let back = deserialize_record(&out).unwrap();
        assert_eq!(back.local_identity, None);
    }

    #[test]
    fn truncated_bytes_are_rejected() {
        let record = sample_record(None);
        let bytes = serialize_record(&record, None).unwrap();
        assert!(deserialize_record(&bytes[..bytes.len() / 2]).is_err());
    }

    #[test]
    fn wrong_secret_length_is_rejected() {
        // Hand-build a structure whose secret is 16 bytes (the pre-widening
        // format) — must be rejected, not silently mis-sized.
        let record = sample_record(None);
        let members = vec![
            (Tag::Context(0), Value::Bytes(record.id.0.to_vec())),
            (Tag::Context(1), Value::Bytes(vec![0x5A; 16])),
            (Tag::Context(2), Value::Uint(record.peer.node_id)),
            (Tag::Context(3), Value::Uint(record.peer.fabric_id)),
            (
                Tag::Context(4),
                Value::Bytes(record.peer.noc.to_tlv().unwrap()),
            ),
            (Tag::Context(5), Value::Uint(0)),
        ];
        let mut out = Vec::new();
        let mut w = TlvWriter::new(&mut out);
        w.write_value(Tag::Anonymous, &Value::Structure(members))
            .unwrap();
        assert!(deserialize_record(&out).is_err());
    }
}
