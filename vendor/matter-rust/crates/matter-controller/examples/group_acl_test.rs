//! Positive-control: provision a group, ADD THE GROUP ACL ENTRY, then group-cast
//! OnOff. The missing piece in E1 provisioning was the AccessControl entry that
//! authorizes the group to invoke — without it the device receives + decrypts
//! the group command but DENIES it at access control.

#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::items_after_statements
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{
    AclAuthMode, AclEntry, AclPrivilege, CommandPath, FileStore, GroupKeyMapEntry,
    MatterController, ReadPath, Value,
};

const KEY_SET_ID: u16 = 50;
const GROUP_ID: u16 = 10;
const ONOFF_CLUSTER: u32 = 0x0006;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    store: PathBuf,
    #[arg(long)]
    node: u64,
}

async fn read_onoff(node: &matter_controller::Node) -> Result<Option<bool>> {
    let r = node
        .read(&[ReadPath::concrete(1, ONOFF_CLUSTER, 0)])
        .await
        .context("read OnOff")?;
    Ok(r.iter().find_map(|(p, v)| {
        if p.attribute == 0 {
            if let Value::Bool(b) = v {
                return Some(*b);
            }
        }
        None
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let store = Arc::new(FileStore::new(&args.store));
    let controller = MatterController::builder(store)
        .build()
        .await
        .context("open controller")?;
    let node = controller.node(args.node);

    println!("[0] reachable? OnOff = {:?}", read_onoff(&node).await?);

    // E1 provisioning
    const MATTER_EPOCH_UNIX_SECS: u64 = 946_684_800;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1_800_000_000, |d| d.as_secs());
    let epoch_start_us = now.saturating_sub(MATTER_EPOCH_UNIX_SECS) * 1_000_000;
    let gks = controller
        .create_group(KEY_SET_ID, epoch_start_us)
        .await
        .context("create_group")?;
    node.write_group_key_set(&gks)
        .await
        .context("KeySetWrite")?;
    node.write_group_key_map(&[GroupKeyMapEntry::new(GROUP_ID, KEY_SET_ID)])
        .await
        .context("GroupKeyMap")?;
    node.add_group(1, GROUP_ID, "acl-test")
        .await
        .context("AddGroup")?;
    println!("[1] provisioned key set {KEY_SET_ID} + group {GROUP_ID} membership");

    // THE MISSING PIECE: add an ACL entry granting the group Operate.
    let mut acl = node.read_acl().await.context("read_acl")?;
    println!("[2] existing ACL: {} entry(ies)", acl.len());
    acl.push(AclEntry::new(
        AclPrivilege::Operate,
        AclAuthMode::Group,
        Some(vec![u64::from(GROUP_ID)]),
        None,
    ));
    node.write_acl(&acl)
        .await
        .context("write_acl (+group entry)")?;
    println!("    wrote ACL with group-Operate entry (subjects=[{GROUP_ID}]) ✓");

    // Group-cast OnOff with read-back.
    println!("[3] group-cast OnOff sequence:");
    for on in [false, true, false, true] {
        let cmd = u32::from(on);
        controller
            .invoke_group(
                GROUP_ID,
                KEY_SET_ID,
                CommandPath {
                    endpoint: 1,
                    cluster: ONOFF_CLUSTER,
                    command: cmd,
                },
                Value::Structure(vec![]),
            )
            .await
            .context("invoke_group")?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let got = read_onoff(&node).await?;
        let ok = got == Some(on);
        println!(
            "    OnOff.{} -> read-back {:?}  {}",
            if on { "ON " } else { "OFF" },
            got,
            if ok { "✓" } else { "✗" }
        );
    }
    println!("== done ==");
    Ok(())
}
