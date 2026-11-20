//! Typed, snapshot-decoupled view of a commissioned node ([`MatterController::nodes`]).

/// Metadata about a node commissioned onto one of the controller's fabrics.
///
/// Returned by [`MatterController::nodes`](crate::MatterController::nodes) and
/// [`MatterController::commission`](crate::MatterController::commission) so
/// callers never have to deserialize the on-disk snapshot to enumerate devices.
///
/// `#[non_exhaustive]`: more fields may be added without a breaking change.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct NodeInfo {
    /// Operational node id on `fabric_id`.
    pub node_id: u64,
    /// The controller-side fabric this node belongs to. This is the stable
    /// operational **Fabric ID** (a `u64`), not the device-side 1-based fabric
    /// index — the controller keys its state by fabric id.
    pub fabric_id: u64,
    /// Device vendor id (`BasicInformation`), captured best-effort after
    /// commissioning. `None` until captured.
    pub vendor_id: Option<u16>,
    /// Device product id (`BasicInformation`), captured best-effort after
    /// commissioning. `None` until captured.
    pub product_id: Option<u16>,
    /// Caller-supplied opaque label from `commission()`. `None` if none given.
    pub label: Option<String>,
}
