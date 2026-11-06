//! Pure (IO-free) helpers for the commissioning flow, unit-tested in
//! isolation. The actor (`actor.rs`) calls these around the M6 `commission()`
//! driver.

use matter_commissioning::CommissionedFabric;

use crate::state::{DeviceEntry, FabricEntry};

/// First device node id assigned on a fabric. The commissioner takes node id 1
/// by default; devices start at 2.
pub(crate) const FIRST_DEVICE_NODE_ID: u64 = 2;

/// Allocate the next operational node id for a new device on `fabric`: one past
/// the highest existing device node id, or [`FIRST_DEVICE_NODE_ID`] if none.
/// Skips the commissioner's own node id.
pub(crate) fn next_device_node_id(fabric: &FabricEntry) -> u64 {
    let max = fabric.devices.iter().map(|d| d.node_id).max();
    let mut candidate = match max {
        Some(m) => m + 1,
        None => FIRST_DEVICE_NODE_ID,
    };
    if candidate == fabric.commissioner.node_id {
        candidate += 1;
    }
    candidate
}

/// Build the `DeviceEntry` to persist from a successful commissioning result.
pub(crate) fn device_entry_from_commissioned(commissioned: &CommissionedFabric) -> DeviceEntry {
    DeviceEntry {
        node_id: commissioned.peer_node_id,
        peer_noc_public_key: commissioned.peer_root_public_key,
        resumption_record: None,
        last_known_addr: None,
        vendor_id: None,
        product_id: None,
        label: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests with justification (CONTRIBUTING.md).
mod tests {
    use super::*;
    use crate::fabric::{create_fabric, FabricConfig};
    use matter_cert::MatterTime;
    use matter_commissioning::SystemNocRng;

    fn fabric() -> FabricEntry {
        create_fabric(
            &FabricConfig {
                fabric_id: 1,
                rcac_id: 1,
                commissioner_node_id: 1,
                validity: (
                    MatterTime::from_unix_secs(1_700_000_000),
                    MatterTime::NO_EXPIRY,
                ),
                issue_icac: false,
            },
            &SystemNocRng,
        )
        .unwrap()
    }

    #[test]
    fn first_device_id_is_two() {
        assert_eq!(next_device_node_id(&fabric()), 2);
    }

    #[test]
    fn allocates_one_past_highest_and_skips_commissioner() {
        let mut f = fabric();
        f.devices.push(DeviceEntry {
            node_id: 5,
            peer_noc_public_key: [0x04; 65],
            resumption_record: None,
            last_known_addr: None,
            vendor_id: None,
            product_id: None,
            label: None,
        });
        assert_eq!(next_device_node_id(&f), 6);
    }
}
