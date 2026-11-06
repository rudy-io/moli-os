//! Error type for `matter-controller`.

use crate::store::StoreError;

/// Errors surfaced by the controller's persistence and identity layer.
///
/// `#[non_exhaustive]` so later sub-phases can add networked variants
/// (e.g. `SessionLost`, `DeviceUnreachable`) without a breaking change.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The backing [`ControllerStore`](crate::store::ControllerStore) failed.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// TLV encode/decode of the snapshot blob failed.
    #[error("TLV codec error: {0}")]
    Codec(#[from] matter_codec::Error),

    /// A certificate failed to parse or serialize.
    #[error("certificate error: {0}")]
    Cert(#[from] matter_cert::Error),

    /// NOC/RCAC issuance failed.
    #[error("NOC issuance error: {0}")]
    Noc(#[from] matter_commissioning::NocError),

    /// A signing key could not be generated or reconstructed.
    #[error("signer error: {0}")]
    Signer(String),

    /// The persisted snapshot was structurally invalid or an unknown version.
    #[error("malformed snapshot: {0}")]
    Snapshot(String),

    /// CASE session establishment failed, or a driver operation errored.
    #[error("driver error: {0}")]
    Driver(#[from] matter_commissioning::driver::DriverError),

    /// A transport / session-manager (framing, MRP) operation failed.
    #[error("transport error: {0}")]
    Transport(#[from] matter_transport::Error),

    /// No fabric exists, or the requested node/fabric is not addressable.
    #[error("not commissioned: {0}")]
    NotCommissioned(String),

    /// The owning controller task has stopped (channel closed).
    #[error("controller task is no longer running")]
    ControllerStopped,

    /// An Interaction-Model request/response failed to build or parse.
    #[error("interaction model error: {0}")]
    InteractionModel(#[from] matter_interaction::ImError),

    /// An operational-path failure with a human-readable detail — a key
    /// derivation (operational IPK / compressed fabric id), a transport/session
    /// send or decode, a request timeout, or a subscription liveness timeout.
    #[error("operational error: {0}")]
    Operational(String),

    /// A device acknowledged an operational request at the transport layer but
    /// never sent the Interaction Model response, and the response deadline
    /// elapsed.
    ///
    /// Distinct from [`Self::Operational`] because the cause is specific and
    /// actionable: MRP confirmed delivery, so this is not packet loss — the
    /// device accepted the request and did not answer it. Observed on bridges
    /// that silently drop a read after several rapid consecutive reads on one
    /// session.
    ///
    /// The request is **not** retried before this is returned: delivery was
    /// confirmed, so re-sending could execute a non-idempotent command twice.
    /// Deciding whether a retry is safe is the caller's.
    ///
    /// Tune the deadline with
    /// [`MatterControllerBuilder::response_deadline`][crate::MatterControllerBuilder::response_deadline].
    #[error("node {node_id:016X} acknowledged the request but sent no response within {after:?}")]
    ResponseTimeout {
        /// The node that failed to answer.
        node_id: u64,
        /// The deadline that elapsed.
        after: std::time::Duration,
    },

    /// The device rejected a subscribe request with an Interaction Model status
    /// code (e.g. 0x89 `ResourceExhausted` when its subscription resources are
    /// full, 0xC8 `PathsExhausted` when a path quota is exceeded, 0x80
    /// `InvalidAction`, 0x7E `UnsupportedAccess`). Raw status preserved.
    ///
    /// Returned by [`Node::subscribe`](crate::Node::subscribe) as soon as the
    /// rejection arrives. A subscription that is already established and is
    /// rejected while re-establishing itself does not surface this: it retries
    /// on its backoff, as chip's `ReadClient` does.
    #[error("subscribe rejected by the device (IM status {0:#04x})")]
    SubscribeRejected(u8),

    /// Attestation trust material could not be loaded.
    #[error("attestation trust error: {0}")]
    Trust(String),

    /// The setup code (QR / manual) could not be parsed.
    #[error("invalid setup code: {0}")]
    SetupCode(String),

    /// No attestation trust configured; commissioning cannot verify the device.
    #[error(
        "no attestation trust configured — commissioning cannot verify the device's \
         attestation. Build the controller with MatterController::builder(store)\
         .attestation_trust(AttestationTrust::from_dirs(paa_dir, cd_dir)).build(), \
         not MatterController::open(store)"
    )]
    NoTrust,

    /// An `AdministratorCommissioning` command returned a non-success IM status
    /// (e.g. 0x02 Busy, 0x03 `PAKEParameterError`, 0x04 `WindowNotOpen` reported as
    /// a cluster status). The raw IM status byte is preserved.
    #[error("commissioning window command rejected (IM status {0:#04x})")]
    CommissioningWindowRejected(u8),

    /// Refused to remove the controller's own fabric (would sever the CASE
    /// session and orphan persisted device state). No `force` override exists.
    #[error("refusing to remove our own fabric (would orphan the device)")]
    WouldRemoveSelf,

    /// An `OperationalCredentials` command returned a non-success
    /// `NodeOperationalCertStatusEnum` (e.g. 7 `InvalidFabricIndex`). Raw code preserved.
    #[error("operational-credentials command rejected (status {0})")]
    OperationalCredentialsRejected(u8),

    /// Refused an ACL write that would strip our own administrative access
    /// (no Administer/CASE entry covering our commissioner node id). Prevents
    /// orphaning the device. Checked before any bytes are sent.
    #[error("refusing ACL write: it would remove our own administrative access")]
    AclWouldLockOut,

    /// A `Groups` / `GroupKeyManagement` command returned a non-success status
    /// (e.g. `ResourceExhausted` from `MaxGroupsPerFabric`). Raw status preserved.
    #[error("group command rejected (status {0})")]
    GroupCommandRejected(u8),

    /// A group send (`invoke_group`) named a `key_set_id` that has not been
    /// provisioned on the controller's fabric (no matching
    /// [`GroupKeySetConfig`](crate::GroupKeySetConfig) in `group_keys`). Call
    /// [`MatterController::create_group`](crate::MatterController::create_group)
    /// first to mint and persist the key set.
    #[error("group key set {0} is not provisioned on this fabric")]
    GroupNotProvisioned(u16),

    /// [`MatterController::create_fabric`](crate::MatterController::create_fabric)
    /// was called with a `fabric_id` that already exists on this controller
    /// (issue #110 — commonly hit by calling `create_fabric` unconditionally
    /// on every startup instead of only on a fresh store). Call
    /// [`MatterController::fabrics`](crate::MatterController::fabrics) first
    /// to check which fabrics already exist.
    ///
    /// To recover: the existing fabric is already usable — just skip the
    /// `create_fabric` call and carry on with it. If you genuinely want a
    /// second fabric, pass a different `fabric_id`; if you want to start over,
    /// point the controller at a fresh store. There is no API to delete a
    /// fabric from the controller's own store, so once a `fabric_id` is in a
    /// store, `create_fabric` refuses it for that store's lifetime.
    /// ([`Node::remove_fabric`](crate::Node::remove_fabric) removes *our*
    /// fabric from a **device**, not from the controller.)
    #[error(
        "fabric {0:#018x} already exists — call MatterController::fabrics() to check before \
         calling create_fabric; to recover, use the existing fabric, pass a different fabric_id, \
         or start from a fresh store"
    )]
    FabricAlreadyExists(u64),

    /// [`FabricConfig::validity`](crate::FabricConfig::validity) names a
    /// window that cannot work on a device (issue #111). Rejected windows:
    ///
    /// - `not_before` at the Matter epoch (`MatterTime(0)`, i.e.
    ///   2000-01-01T00:00:00Z). Not a validity-policy rejection: chip's
    ///   `ChipEpochToASN1Time`
    ///   (`connectedhomeip/src/credentials/CHIPCert.cpp`) encodes epoch 0 as
    ///   `99991231235959Z` for both `notBefore` and `notAfter`, so the X.509
    ///   TBS the device rebuilds from our TLV certificate differs from the one
    ///   we signed and the **signature** check fails — surfacing as an opaque
    ///   `IM status 0x85` on `AddTrustedRootCertificate`.
    /// - `not_before` more than a day ahead of this host's clock — usually a
    ///   millisecond timestamp passed to `MatterTime::from_unix_secs`, which
    ///   saturates to ≈ year 2136. Such a root *installs* (chip's
    ///   `ValidateChipRCAC` skips RCAC validity times) and then fails every
    ///   CASE session with `kNotYetValid`.
    /// - An inverted or empty window (`not_after <= not_before`, excluding
    ///   `MatterTime::NO_EXPIRY`).
    ///
    /// The detail string names which.
    #[error("invalid fabric validity window: {0}")]
    InvalidFabricValidity(String),

    /// The host's wall clock reads before the Matter epoch
    /// (2000-01-01T00:00:00Z) — almost always an **unset system clock** on a
    /// host with no RTC that has not yet reached an NTP server. Payload: the
    /// Unix seconds actually read.
    ///
    /// Refused rather than used, because `MatterTime::from_unix_secs` saturates
    /// such a reading to `MatterTime(0)`, and a certificate minted with
    /// `notBefore == 0` cannot be installed on a device at all: chip re-encodes
    /// epoch 0 as `99991231235959Z` when rebuilding the X.509 TBS, breaking the
    /// signature (`ChipEpochToASN1Time`,
    /// `connectedhomeip/src/credentials/CHIPCert.cpp` — the same root cause as
    /// issue #111). Set the clock (or wait for time sync) and retry.
    #[error(
        "system clock reads {0} (before the Matter epoch, 2000-01-01T00:00:00Z) — it is probably \
         unset; certificates minted against it cannot be installed on a device. Set the host \
         clock or wait for time sync, then retry"
    )]
    SystemClockUnset(u64),
}

impl Error {
    /// If this error is the device rejecting the supplied network-credential
    /// *type* — e.g. Thread credentials handed to a Wi-Fi-only device
    /// (`NetworkCommissioning::FeatureMap` lacks the needed bit) — returns
    /// which network type the credentials required. Use this to route to a
    /// different credential type instead of substring-matching the rendered
    /// message.
    ///
    /// Returns `None` for every other error.
    #[must_use]
    pub fn network_feature_unsupported(&self) -> Option<matter_commissioning::NetworkKind> {
        match self {
            Error::Driver(matter_commissioning::driver::DriverError::Commissioning(
                matter_commissioning::CommissioningError::NetworkFeatureUnsupported { needed },
            )) => Some(*needed),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn no_trust_error_names_the_fix() {
        let msg = crate::error::Error::NoTrust.to_string();
        assert!(
            msg.contains("attestation_trust"),
            "NoTrust must name the builder fix: {msg}"
        );
        assert!(
            msg.contains("from_dirs"),
            "NoTrust must name from_dirs: {msg}"
        );
    }

    #[test]
    fn subscribe_rejected_names_the_status_in_hex() {
        assert_eq!(
            crate::error::Error::SubscribeRejected(0x89).to_string(),
            "subscribe rejected by the device (IM status 0x89)"
        );
    }

    #[test]
    fn network_feature_unsupported_is_typed_through_the_chain() {
        use matter_commissioning::{driver::DriverError, CommissioningError, NetworkKind};

        // The nested chain a commission failure actually produces.
        let e = crate::error::Error::Driver(DriverError::Commissioning(
            CommissioningError::NetworkFeatureUnsupported {
                needed: NetworkKind::Thread,
            },
        ));
        assert_eq!(e.network_feature_unsupported(), Some(NetworkKind::Thread));

        // The substring WeaveHome matched still renders through the chain
        // (belt for the matter-commissioning pin's braces).
        assert!(e
            .to_string()
            .contains("does not support Thread network type"));

        // Unrelated errors: None.
        assert_eq!(
            crate::error::Error::ControllerStopped.network_feature_unsupported(),
            None
        );
        let other = crate::error::Error::Driver(DriverError::Commissioning(
            CommissioningError::CaseEstablishmentFailed,
        ));
        assert_eq!(other.network_feature_unsupported(), None);
    }
}
