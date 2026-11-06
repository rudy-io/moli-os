// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use std::time::Duration;

use matter_controller::{
    AclAuthMode, AclEntry, AclPrivilege, CommandPath, GroupKeyMapEntry, Node, ReadPath, Value,
};

const KEY_SET_ID: u16 = 50;
const GROUP_ID: u16 = 10;
const ONOFF_CLUSTER: u32 = 0x0006;

/// Matter epoch (2000-01-01) as Unix seconds; group epoch-start times are µs
/// since this epoch.
const MATTER_EPOCH_UNIX_SECS: u64 = 946_684_800;

/// Read back the OnOff attribute (ep1, cluster 0x0006, attr 0x0000).
async fn read_onoff(node: &Node) -> Option<bool> {
    let r = node
        .read(&[ReadPath::concrete(1, ONOFF_CLUSTER, 0x0000)])
        .await
        .expect("read OnOff");
    r.iter().find_map(|(p, v)| {
        if p.attribute == 0x0000 {
            if let Value::Bool(b) = v {
                return Some(*b);
            }
        }
        None
    })
}

/// A non-zero Matter-epoch µs start time for the group key set.
fn epoch_start_us() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1_800_000_000, |d| d.as_secs());
    now.saturating_sub(MATTER_EPOCH_UNIX_SECS) * 1_000_000
}

// ── Group provisioning + ACL grant + group-cast actuation ────────────────────

/// Provision a group key set + membership + the group `Operate` ACL grant, then
/// group-cast OnOff On/Off and verify the DUT actuates each time. This is the
/// positive control mirroring `examples/group_acl_test.rs`.
#[tokio::test]
async fn group_provision_acl_and_multicast() {
    let cfg = integration_tests::dut_or_skip!();
    // Group-cast needs a multicast egress interface; skip the whole test without it.
    if cfg.multicast_if.is_none() {
        eprintln!("skipped: group-cast needs MATTER_MULTICAST_IF (set by `just integration`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // 1. Create the group key set on the controller's fabric.
    let gks = controller
        .create_group(KEY_SET_ID, epoch_start_us())
        .await
        .expect("create_group");

    // 2. Program the key set + group→key-set map + group membership onto the DUT.
    node.write_group_key_set(&gks)
        .await
        .expect("write_group_key_set");
    node.write_group_key_map(&[GroupKeyMapEntry::new(GROUP_ID, KEY_SET_ID)])
        .await
        .expect("write_group_key_map");
    integration_tests::fixture::add_group_or_explain(&node, 1, GROUP_ID, "integ").await;

    // 3. THE required AccessControl entry: grant the group Operate. APPEND only —
    //    never replace/remove the existing admin entry (lockout safety).
    let mut acl = node.read_acl().await.expect("read_acl");
    acl.push(AclEntry::new(
        AclPrivilege::Operate,
        AclAuthMode::Group,
        Some(vec![u64::from(GROUP_ID)]),
        None,
    ));
    node.write_acl(&acl)
        .await
        .expect("write_acl (+group entry)");

    // 4. Group-cast OnOff On → device actuates true.
    controller
        .invoke_group(
            GROUP_ID,
            KEY_SET_ID,
            CommandPath {
                endpoint: 1,
                cluster: ONOFF_CLUSTER,
                command: 0x01,
            },
            Value::Structure(vec![]),
        )
        .await
        .expect("invoke_group On");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        read_onoff(&node).await,
        Some(true),
        "group-cast On did not actuate OnOff"
    );

    // 5. Group-cast OnOff Off → device actuates false.
    controller
        .invoke_group(
            GROUP_ID,
            KEY_SET_ID,
            CommandPath {
                endpoint: 1,
                cluster: ONOFF_CLUSTER,
                command: 0x00,
            },
            Value::Structure(vec![]),
        )
        .await
        .expect("invoke_group Off");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        read_onoff(&node).await,
        Some(false),
        "group-cast Off did not actuate OnOff"
    );
}
