//! Test-only construction helpers for downstream crates (feature
//! `test-support`). Mirrors `matter_cert::test_support`: enable this feature
//! only in `[dev-dependencies]`, never in production. NOT part of the stable
//! public API — signatures here may change without a semver bump.

use crate::state_machine::{CommissionedFabric, Stage};
use crate::FabricRecord;

/// Build a canned successful commissioning outcome.
///
/// [`CommissionedFabric`] is `#[non_exhaustive]`, so no crate other than this
/// one can construct one by struct literal — which blocks downstream tests
/// (e.g. `matter-controller`'s actor tests) that want to exercise
/// "commissioning succeeded" plumbing (persisting the resulting device,
/// resolving a completion channel, ...) without driving a full PASE/CASE
/// handshake over a real or loopback transport. This fills in a fixed
/// `terminated_at: Stage::Cleanup` (the only value a real success ever
/// carries) and takes the rest as parameters.
#[must_use]
pub fn commissioned_fabric_for_test(
    fabric: FabricRecord,
    peer_node_id: u64,
    peer_root_public_key: [u8; 65],
) -> CommissionedFabric {
    CommissionedFabric {
        fabric,
        peer_node_id,
        peer_root_public_key,
        terminated_at: Stage::Cleanup,
    }
}
