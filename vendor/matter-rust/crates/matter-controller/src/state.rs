//! In-memory controller state. These types are the *persistable* record;
//! live signers are reconstructed from the stored PKCS#8 keys on demand.

use std::sync::Arc;

use matter_cert::MatterCertificate;
use matter_commissioning::FabricRecord;
use matter_crypto::{RingSigner, Signer};

use crate::error::Error;

/// The persisted material for one group key set.
///
/// Stored inside [`FabricEntry::group_keys`] and round-tripped through the
/// TLV snapshot at context tags t6 (key-set array) and t7 (outbound counter).
/// This carries only what the controller needs to *send* group-encrypted
/// messages; a full `GroupKeySet` cluster record lives in the device, not
/// here.
///
/// This is a `pub` type because callers that program group keys (e.g.
/// higher-level fabric-management APIs) need to construct and inspect it.
///
/// `#[non_exhaustive]`: persisted record whose shape may grow (e.g. key policy
/// epoch, security level flags); marking it keeps such additions non-breaking.
/// Construct via [`GroupKeySetConfig::new`].
///
/// `Debug` is written by hand so that `epoch_key` prints as `<redacted>`, as
/// [`FabricEntry`]'s `Debug` already withholds the key sets it holds.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GroupKeySetConfig {
    /// Group Key Set ID (`GrpKeySetID`, 16-bit, spec §4.15).
    pub key_set_id: u16,
    /// 16-byte epoch key (`EpochKey0` / `EpochKey1` / `EpochKey2` per policy).
    /// Secret: this struct's `Debug` output shows `<redacted>` instead.
    pub epoch_key: [u8; 16],
    /// Epoch key start time in Matter epoch seconds (0 = unset / pre-operational).
    pub epoch_start_time: u64,
}

impl std::fmt::Debug for GroupKeySetConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Same name, fields and order as the derive this replaces; only the
        // key's value is withheld.
        f.debug_struct("GroupKeySetConfig")
            .field("key_set_id", &self.key_set_id)
            .field("epoch_key", &"<redacted>")
            .field("epoch_start_time", &self.epoch_start_time)
            .finish()
    }
}

impl GroupKeySetConfig {
    /// Construct a [`GroupKeySetConfig`].
    ///
    /// Required because the struct is `#[non_exhaustive]`; external callers
    /// cannot use struct-literal syntax.
    #[must_use]
    pub fn new(key_set_id: u16, epoch_key: [u8; 16], epoch_start_time: u64) -> Self {
        Self {
            key_set_id,
            epoch_key,
            epoch_start_time,
        }
    }
}

/// A per-fabric intermediate CA (ICAC): the issued ICAC certificate plus the
/// PKCS#8 private key that signs NOCs under it. `None` for a flat RCAC->NOC
/// fabric (the default).
#[derive(Clone)]
#[non_exhaustive]
pub struct IcacIdentity {
    /// The RCAC-signed ICAC certificate.
    pub cert: MatterCertificate,
    /// The ICAC signing key, PKCS#8 DER. `pub(crate)`: raw private-key material
    /// is never exposed on the public surface (persisted via `snapshot`).
    pub(crate) pkcs8: Vec<u8>,
}

impl std::fmt::Debug for IcacIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IcacIdentity")
            .field("cert", &"<MatterCertificate>")
            .field("pkcs8", &"<redacted PKCS#8>")
            .finish()
    }
}

/// A device commissioned onto a fabric.
///
/// `#[non_exhaustive]`: persisted record whose shape may grow (e.g. CAT tags,
/// typed resumption state); marking it keeps such additions non-breaking. Only
/// constructed inside `matter-controller`.
#[derive(Clone)]
#[non_exhaustive]
pub struct DeviceEntry {
    /// The device's operational node ID on this fabric.
    pub node_id: u64,
    /// The device's NOC public key (SEC1 uncompressed, `0x04 || X || Y`).
    pub peer_noc_public_key: [u8; 65],
    /// Cached CASE resumption record (opaque bytes).
    pub resumption_record: Option<Vec<u8>>,
    /// Last operational address we reached the device at (a discovery hint).
    pub last_known_addr: Option<String>,
    /// Device vendor id (`BasicInformation` 0x0028/0x0002), captured
    /// best-effort after commissioning. `None` if the read did not complete.
    pub vendor_id: Option<u16>,
    /// Device product id (`BasicInformation` 0x0028/0x0004), captured
    /// best-effort after commissioning. `None` if the read did not complete.
    pub product_id: Option<u16>,
    /// Caller-supplied opaque label attached at `commission()` time. `None`
    /// if the caller passed none.
    pub label: Option<String>,
}

impl std::fmt::Debug for DeviceEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `resumption_record` is a serialized CASE ResumptionRecord and carries
        // a session shared secret — redact it (matches the redaction discipline
        // in `FabricEntry`/`CommissionerIdentity`). `peer_noc_public_key`,
        // `last_known_addr`, `vendor_id`, `product_id`, and `label` are not
        // secret.
        f.debug_struct("DeviceEntry")
            .field("node_id", &self.node_id)
            .field("peer_noc_public_key", &self.peer_noc_public_key)
            .field(
                "resumption_record",
                &self
                    .resumption_record
                    .as_ref()
                    .map(|_| "<redacted; CASE secret>"),
            )
            .field("last_known_addr", &self.last_known_addr)
            .field("vendor_id", &self.vendor_id)
            .field("product_id", &self.product_id)
            .field("label", &self.label)
            .finish()
    }
}

/// The controller's own stable operational identity on a fabric.
///
/// Minted **once** when the fabric is created (see
/// [`MatterController::create_fabric`](crate::controller::MatterController::create_fabric))
/// and reused for every CASE handshake, replacing the earlier per-call NOC
/// minting.
///
/// `#[non_exhaustive]`: persisted identity record that may grow; marking it
/// keeps additions non-breaking. Only constructed inside `matter-controller`.
#[derive(Clone)]
#[non_exhaustive]
pub struct CommissionerIdentity {
    /// The commissioner's stable node ID on this fabric.
    pub node_id: u64,
    /// The commissioner's operational private key, PKCS#8 DER. `pub(crate)`:
    /// raw private-key material is never exposed on the public surface.
    pub(crate) operational_pkcs8: Vec<u8>,
    /// The commissioner's NOC, signed by the fabric RCAC.
    pub noc: MatterCertificate,
}

impl std::fmt::Debug for CommissionerIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommissionerIdentity")
            .field("node_id", &self.node_id)
            .field("operational_pkcs8", &"<redacted PKCS#8>")
            .field("noc", &"<MatterCertificate>")
            .finish()
    }
}

/// One fabric the controller administers: trust root, IPK, the
/// commissioner identity, and the devices commissioned onto it.
///
/// `#[non_exhaustive]`: persisted record that may gain fields (e.g. an ICAC
/// tier, fabric label); marking it keeps such additions non-breaking. Only
/// constructed inside `matter-controller`.
#[derive(Clone)]
#[non_exhaustive]
pub struct FabricEntry {
    /// Matter fabric identifier.
    pub fabric_id: u64,
    /// 16-byte Identity Protection Key for this fabric.
    pub ipk: [u8; 16],
    /// Self-signed root (RCAC) certificate.
    pub rcac_cert: MatterCertificate,
    /// The RCAC root signing key, PKCS#8 DER. `pub(crate)`: raw private-key
    /// material is never exposed on the public surface.
    pub(crate) rcac_pkcs8: Vec<u8>,
    /// The controller's stable identity on this fabric.
    pub commissioner: CommissionerIdentity,
    /// Devices commissioned onto this fabric.
    pub devices: Vec<DeviceEntry>,
    /// Group key sets programmed on this fabric (persisted for outbound group
    /// message encryption). Empty until the controller programs group keys.
    pub group_keys: Vec<GroupKeySetConfig>,
    /// The reserved **ceiling** of the outbound group message counter for this
    /// fabric: the lowest counter value this controller may not yet have sent.
    ///
    /// Persisted so counters survive restarts (spec §4.6.7 prohibits counter
    /// reuse across sessions / resets). The controller hands out counters below
    /// the ceiling from memory and only persists when it raises the ceiling by a
    /// block, so a restart resumes here — skipping at most a block of never-sent
    /// values, never reusing a sent one.
    ///
    /// Snapshots written before block reservation stored the next counter to
    /// send, which is the same quantity (the smallest never-sent value), so they
    /// are read back correctly with no migration.
    pub outbound_group_counter: u32,
    /// ICD (Intermittently Connected Device) client registrations on this
    /// fabric. Each holds the shared key + counter floor the check-in listener
    /// uses to verify a registered device's Check-In messages. Empty until the
    /// controller calls `register_icd_client`.
    pub icd_clients: Vec<crate::icd::IcdRegistration>,
    /// Optional per-fabric intermediate CA. `None` for a flat RCAC->NOC
    /// fabric (the default); `Some` once the fabric adopts an ICAC tier.
    pub icac: Option<IcacIdentity>,
}

impl std::fmt::Debug for FabricEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FabricEntry")
            .field("fabric_id", &self.fabric_id)
            .field("ipk", &"<redacted; 16 bytes>")
            .field("rcac_cert", &"<MatterCertificate>")
            .field("rcac_pkcs8", &"<redacted PKCS#8>")
            .field("commissioner", &self.commissioner)
            .field("devices", &self.devices)
            .field(
                "group_keys",
                &format!("<{} key sets>", self.group_keys.len()),
            )
            .field("outbound_group_counter", &self.outbound_group_counter)
            .field(
                "icd_clients",
                &format!("<{} registrations>", self.icd_clients.len()),
            )
            .field("icac", &self.icac)
            .finish()
    }
}

impl FabricEntry {
    /// Drop every device's cached CASE resumption record on this fabric,
    /// returning how many were cleared.
    ///
    /// This is chip's `ClearCASEResumptionStateOnFabricChange`
    /// (`Server.h` / `CHIPDeviceControllerFactory.h`), which fires on fabric
    /// **update** as well as removal — the shared helper's name says
    /// `OnFabricChange`, not `OnFabricRemoval`, and that is the design intent.
    ///
    /// # When to call it
    ///
    /// Any event that changes this fabric's authorization snapshot: the
    /// commissioner's node id, its CATs, or its operational key. Resumption
    /// replays a cached snapshot without re-verifying a certificate, and the
    /// ECDH secret it authenticates with is derived from *ephemeral* keys, so
    /// it stays valid across a NOC change and cannot detect one. The
    /// specification requires it for the peer's side, and chip quotes it at
    /// its own wipe site: *"All internal data reflecting the prior operational
    /// identifier of the Node within the Fabric SHALL be revoked and
    /// removed."* A surviving record lets the prior identity be reconstituted
    /// one round trip later.
    ///
    /// # This is defence in depth, not the only defence
    ///
    /// Each stored record also carries a fingerprint of the commissioner NOC
    /// that minted it (the private `crate::resumption` module), and one that no longer
    /// matches is refused at load. That binding is what makes correctness
    /// independent of anyone remembering to call this — chip's model needs a
    /// delegate wired to every mutating path, and matter.js's `SessionManager`
    /// subscribes only to fabric *deletion*, so its update wipe rests entirely
    /// on one imperative call site. This method exists so the stale secret
    /// material is *removed promptly* rather than merely never used.
    pub fn invalidate_resumption_records(&mut self) -> usize {
        let mut cleared = 0usize;
        for dev in &mut self.devices {
            if dev.resumption_record.take().is_some() {
                cleared += 1;
            }
        }
        cleared
    }

    /// Reconstruct the RCAC root signer from the stored PKCS#8 key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Signer`] if the stored key is not valid PKCS#8.
    pub(crate) fn rcac_signer(&self) -> Result<RingSigner, Error> {
        RingSigner::from_pkcs8(&self.rcac_pkcs8).map_err(|e| Error::Signer(e.to_string()))
    }

    /// Reconstruct the commissioner operational signer from PKCS#8.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Signer`] if the stored key is not valid PKCS#8.
    pub(crate) fn commissioner_signer(&self) -> Result<RingSigner, Error> {
        RingSigner::from_pkcs8(&self.commissioner.operational_pkcs8)
            .map_err(|e| Error::Signer(e.to_string()))
    }

    /// Build a [`FabricRecord`] view (used by later sub-phases for NOC
    /// issuance and CASE). Reconstructs the RCAC signer from PKCS#8.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Signer`] if the RCAC key cannot be reconstructed.
    pub(crate) fn to_fabric_record(&self) -> Result<FabricRecord, Error> {
        let signer = self.rcac_signer()?;
        let root_public_key = signer.public_key().clone();
        // Reconstruct the ICAC signer/cert when this fabric has an ICAC
        // tier, so a restored fabric keeps signing NOCs (and CASE
        // credentials, via `FabricRecord.icac_cert`) under the ICAC —
        // matching what `create_fabric` sets up live.
        let (icac_signer, icac_cert) = match &self.icac {
            Some(icac) => {
                let signer = RingSigner::from_pkcs8(&icac.pkcs8)
                    .map_err(|e| Error::Signer(e.to_string()))?;
                (
                    Some(Arc::new(signer) as Arc<dyn Signer>),
                    Some(icac.cert.clone()),
                )
            }
            None => (None, None),
        };
        Ok(FabricRecord {
            fabric_id: self.fabric_id,
            root_public_key,
            root_signer: Arc::new(signer) as Arc<dyn Signer>,
            root_cert: self.rcac_cert.clone(),
            icac_signer,
            icac_cert,
            identity_protection_key: self.ipk,
        })
    }
}

/// The full controller state: all administered fabrics.
///
/// `#[non_exhaustive]`: the persisted top-level record may gain fields (e.g.
/// schema version, controller-wide settings); marking it keeps such additions
/// non-breaking. Construct via [`ControllerState::new`] or
/// [`ControllerState::default`] from outside this crate.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ControllerState {
    /// Fabrics this controller administers.
    pub fabrics: Vec<FabricEntry>,
}

impl ControllerState {
    /// Construct controller state from a list of administered fabrics.
    ///
    /// Supported construction path now that [`ControllerState`] is
    /// `#[non_exhaustive]`; the `fabrics` field stays directly accessible.
    #[must_use]
    pub fn new(fabrics: Vec<FabricEntry>) -> Self {
        Self { fabrics }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn controller_state_new_builds_from_fabrics() {
        // `ControllerState` is `#[non_exhaustive]`; `new` is the supported
        // construction path. An empty list yields no fabrics.
        let state = ControllerState::new(Vec::new());
        assert!(state.fabrics.is_empty());
    }

    #[test]
    fn reconstructed_signer_signs_and_verifies() {
        // A standalone RingSigner round-trips through PKCS#8 and signs.
        let (signer, pkcs8) = RingSigner::generate().expect("generate");
        let entry_key = pkcs8.clone();
        let reloaded = RingSigner::from_pkcs8(&entry_key).expect("reload");
        // Both signers share the same public key.
        assert_eq!(
            signer.public_key().as_bytes(),
            reloaded.public_key().as_bytes()
        );
        // The reloaded signer produces a verifiable signature.
        let msg = b"controller identity";
        let sig_bytes = reloaded.sign_p256_sha256(msg).expect("sign");
        // `PublicKey::verify` takes a `&matter_cert::Signature`, not a raw `[u8; 64]`.
        let sig = matter_cert::Signature::new(sig_bytes);
        reloaded
            .public_key()
            .verify(msg, &sig)
            .expect("reloaded signature verifies");
    }
}
