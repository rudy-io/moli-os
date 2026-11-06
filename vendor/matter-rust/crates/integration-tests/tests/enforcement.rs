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

// Distinct key-set / group ids from groups_acl.rs so device state is
// unambiguous when both run in the same `just integration` process.
const KEY_SET_ID: u16 = 51;
const GROUP_ID: u16 = 11;
const ONOFF_CLUSTER: u32 = 0x0006;

/// Matter epoch (2000-01-01) as Unix seconds.
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

/// Group-cast OnOff `command` (0x01 On / 0x00 Off) on ep1.
async fn group_cast_onoff(controller: &matter_controller::MatterController, command: u32) {
    controller
        .invoke_group(
            GROUP_ID,
            KEY_SET_ID,
            CommandPath {
                endpoint: 1,
                cluster: ONOFF_CLUSTER,
                command,
            },
            Value::Structure(vec![]),
        )
        .await
        .expect("invoke_group");
}

// ── AccessControl enforcement (ACE): deny without grant, allow with grant ────

/// Prove the device enforces AccessControl on group commands: a group-cast is
/// DENIED (no actuation) when the group has no ACL entry, and ALLOWED once the
/// group `Operate` ACL grant is added. Isolates the exact failure mode found
/// 2026-06-28 (the missing AccessControl entry).
#[tokio::test]
async fn group_cast_denied_without_acl_then_allowed_with_it() {
    let cfg = integration_tests::dut_or_skip!();
    if cfg.multicast_if.is_none() {
        eprintln!("skipped: group-cast needs MATTER_MULTICAST_IF (set by `just integration`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // 0. Known starting state: unicast Off (authorized — admin entry covers it).
    node.invoke(
        CommandPath {
            endpoint: 1,
            cluster: ONOFF_CLUSTER,
            command: 0x00,
        },
        Value::Structure(vec![]),
    )
    .await
    .expect("unicast Off (reset state)");
    assert_eq!(
        read_onoff(&node).await,
        Some(false),
        "failed to reset OnOff to false before the test"
    );

    // 1. Provision group keys + membership WITHOUT any group ACL entry.
    let gks = controller
        .create_group(KEY_SET_ID, epoch_start_us())
        .await
        .expect("create_group");
    node.write_group_key_set(&gks)
        .await
        .expect("write_group_key_set");
    node.write_group_key_map(&[GroupKeyMapEntry::new(GROUP_ID, KEY_SET_ID)])
        .await
        .expect("write_group_key_map");
    integration_tests::fixture::add_group_or_explain(&node, 1, GROUP_ID, "ace").await;

    // 2. DENY leg: group-cast On with NO ACL grant. The device receives and
    //    decrypts the group command but denies it at AccessControl, so OnOff
    //    must NOT change.
    group_cast_onoff(&controller, 0x01).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        read_onoff(&node).await,
        Some(false),
        "group-cast On WITHOUT an ACL grant must be denied (OnOff stayed false)"
    );

    // 3. GRANT leg: append the group Operate ACL entry (APPEND only — never
    //    remove the admin entry).
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

    // 4. ALLOW leg: same group-cast On now actuates the device.
    group_cast_onoff(&controller, 0x01).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        read_onoff(&node).await,
        Some(true),
        "group-cast On WITH the ACL grant must be allowed (OnOff turned true)"
    );
    // No teardown: the harness clears all DUT state before the next run, and
    // tests that need a known OnOff baseline reset it themselves.
}
