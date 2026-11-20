//! Typed, snapshot-decoupled view of a fabric ([`MatterController::fabrics`]).

/// Metadata about a fabric the controller has created.
///
/// Returned by [`MatterController::fabrics`](crate::MatterController::fabrics)
/// so callers can check which fabrics already exist — in particular, before
/// calling [`MatterController::create_fabric`](crate::MatterController::create_fabric)
/// on every startup. Since issue #110, `create_fabric` refuses to create a
/// second fabric with a `fabric_id` already present and returns
/// [`Error::FabricAlreadyExists`](crate::Error::FabricAlreadyExists) instead
/// of silently duplicating it; `fabrics()` is how a caller checks first.
///
/// `#[non_exhaustive]`: more fields may be added without a breaking change.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FabricInfo {
    /// Matter fabric identifier (spec §6.2.1).
    pub fabric_id: u64,
    /// The stable node ID the controller itself takes on this fabric — its
    /// own commissioner operational identity, minted once by
    /// [`crate::MatterController::create_fabric`] and reused for every CASE
    /// handshake on this fabric.
    pub commissioner_node_id: u64,
    /// Number of devices currently commissioned onto this fabric.
    pub node_count: usize,
    /// Whether this fabric uses a 3-tier RCAC->ICAC->NOC chain (`true`) or
    /// issues NOCs directly under the RCAC (`false`, the default from
    /// [`crate::FabricConfig::new`]).
    pub icac_enabled: bool,
    /// The fabric's 64-bit Compressed Fabric Identifier (Matter Core
    /// §4.3.2.2), derived from the fabric's root public key and `fabric_id`.
    ///
    /// It is the `<CFID>` in the operational mDNS instance names of this
    /// fabric's nodes (`<CFID>-<NODEID>`) and in the fabric's `_I<CFID>`
    /// DNS-SD subtype. To build or recognise those instance names, use
    /// [`operational_instance_name`](crate::operational_instance_name) and
    /// [`parse_operational_instance_name`](crate::parse_operational_instance_name).
    pub compressed_fabric_id: u64,
}
