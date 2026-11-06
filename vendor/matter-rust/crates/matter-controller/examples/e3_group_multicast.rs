//! M9-E3 real-device validation: the full group-multicast loop.
//!
//! Reconnects to an already-commissioned device, provisions a group (E1 verbs),
//! then sends OnOff On/Off to the group over IPv6 MULTICAST (`invoke_group`,
//! fire-and-forget) — and reads OnOff back over unicast after each to confirm
//! the multicast actually took effect (the plug should also PHYSICALLY toggle).
//!
//! ```text
//! cargo run -p matter-controller --example e3_group_multicast -- \
//!     --store /tmp/matter-d-test.bin --node 2
//! ```

// Operator validation example: one long linear main with verbose prose.
#![allow(clippy::too_many_lines, clippy::doc_markdown, clippy::if_not_else)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{
    AclAuthMode, AclEntry, AclPrivilege, CommandPath, FileStore, GroupKeyMapEntry,
    MatterController, ReadPath, Value,
};

const ONOFF_ENDPOINT: u16 = 1;
const ONOFF_CLUSTER: u32 = 0x0006;
const ONOFF_ATTR: u32 = 0x0000;
const ONOFF_CMD_OFF: u32 = 0x00;
const ONOFF_CMD_ON: u32 = 0x01;
const GROUPS_ENDPOINT: u16 = 1;
const KEY_SET_ID: u16 = 43;
const GROUP_ID: u16 = 8;

#[derive(Parser)]
#[command(about = "M9-E3 group-multicast hardware validation")]
struct Args {
    #[arg(long)]
    store: PathBuf,
    #[arg(long)]
    node: u64,
    /// Skip create_group/provision/cleanup — the device is already in the group
    /// (key set already provisioned). Just fire a visible off→on→off sequence.
    #[arg(long)]
    send_only: bool,
}

/// Multicast a single OnOff command, wait, then unicast-read OnOff back.
async fn multicast_set(
    controller: &MatterController,
    node: &matter_controller::Node,
    on: bool,
) -> Result<()> {
    let cmd = if on { ONOFF_CMD_ON } else { ONOFF_CMD_OFF };
    let label = if on { "ON " } else { "OFF" };
    println!("    >>> invoke_group OnOff.{label} over multicast — WATCH THE PLUG <<<");
    controller
        .invoke_group(
            GROUP_ID,
            KEY_SET_ID,
            CommandPath {
                endpoint: ONOFF_ENDPOINT,
                cluster: ONOFF_CLUSTER,
                command: cmd,
            },
            Value::Structure(vec![]),
        )
        .await
        .with_context(|| format!("invoke_group {label}"))?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let got = read_onoff(node).await?;
    let ok = got == Some(on);
    println!(
        "        read-back OnOff = {got:?}  {}\n",
        if ok {
            "✓"
        } else {
            "✗ (multicast didn't take effect)"
        }
    );
    Ok(())
}

async fn read_onoff(node: &matter_controller::Node) -> Result<Option<bool>> {
    let r = node
        .read(&[ReadPath::concrete(
            ONOFF_ENDPOINT,
            ONOFF_CLUSTER,
            ONOFF_ATTR,
        )])
        .await
        .context("read OnOff")?;
    Ok(r.iter().find_map(|(p, v)| {
        if p.attribute == ONOFF_ATTR {
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
        .context("opening controller")?;
    let node = controller.node(args.node);
    println!(
        "== M9-E3 group multicast against node 0x{:016X} ==\n",
        args.node
    );

    // ---- Step 0: reachability + initial state ------------------------------
    println!("[0] reachability check (unicast read OnOff)…");
    let start = read_onoff(&node).await?;
    println!("    device reachable ✓  OnOff = {start:?}\n");

    if !args.send_only {
        // ---- Step 1+2: create + provision the group (E1, software-confirmed) ----
        // EpochStartTime0 is epoch-microseconds since the Matter epoch (2000-01-01).
        // It MUST be non-zero for a non-null epoch key, or the device rejects
        // KeySetWrite with a constraint error (status 0x85). Use "now".
        const MATTER_EPOCH_UNIX_SECS: u64 = 946_684_800;
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1_800_000_000, |d| d.as_secs());
        let epoch_start_us = now_unix.saturating_sub(MATTER_EPOCH_UNIX_SECS) * 1_000_000;
        println!("[1] create_group(key_set={KEY_SET_ID}, epoch_start_us={epoch_start_us})");
        let gks = controller
            .create_group(KEY_SET_ID, epoch_start_us)
            .await
            .context("create_group")?;
        println!(
            "    epoch key generated + persisted (key_set {})\n",
            gks.key_set_id
        );

        println!("[2] provision the device into group {GROUP_ID} (E1 verbs)");
        node.write_group_key_set(&gks)
            .await
            .context("write_group_key_set (KeySetWrite)")?;
        println!("    KeySetWrite ✓");
        node.write_group_key_map(&[GroupKeyMapEntry::new(GROUP_ID, KEY_SET_ID)])
            .await
            .context("write_group_key_map")?;
        println!("    GroupKeyMap (group {GROUP_ID} → key set {KEY_SET_ID}) ✓");
        node.add_group(GROUPS_ENDPOINT, GROUP_ID, "e3-test")
            .await
            .context("add_group")?;
        println!("    AddGroup (ep{GROUPS_ENDPOINT} → group {GROUP_ID}) ✓");

        // Grant `AuthMode=Group, Operate` — without it the device DECRYPTS the
        // group command but drops it with "AccessControl: denied" (Matter §6.6).
        // The subject is the PLAIN group id: the ACL cluster's wire format uses
        // group ids for Group-auth entries (chip converts to the internal
        // 0xFFFF_FFFF_FFFF_xxxx group-node-id form itself; writing that form is
        // rejected with CONSTRAINT_ERROR). List-writes report per-element
        // statuses, so check them — the write succeeds transport-wise even
        // when the device rejects an entry.
        let mut acl: Vec<AclEntry> = node
            .read_acl()
            .await
            .context("read_acl")?
            .into_iter()
            .map(|mut e| {
                e.fabric_index = None; // device assigns the accessing fabric
                e
            })
            .collect();
        acl.retain(|e| e.auth_mode != AclAuthMode::Group); // idempotent re-runs
        acl.push(AclEntry::new(
            AclPrivilege::Operate,
            AclAuthMode::Group,
            Some(vec![u64::from(GROUP_ID)]),
            None,
        ));
        let statuses = node
            .write_acl(&acl)
            .await
            .context("write_acl group grant")?;
        for (path, status) in &statuses {
            anyhow::ensure!(
                status.is_success(),
                "group ACL grant rejected: {path:?} -> {status:?}"
            );
        }
        println!("    Group ACL grant (Operate, subject = group {GROUP_ID}) ✓\n");
    } else {
        println!("[1-2] --send-only: using the already-provisioned group {GROUP_ID} / key set {KEY_SET_ID}\n");
    }

    // ---- Step 3: visible MULTICAST off → on → off → on → off ----------------
    println!("[3] firing a visible multicast sequence (each ~3s apart)…\n");
    for on in [false, true, false, true, false] {
        multicast_set(&controller, &node, on).await?;
    }

    if !args.send_only {
        println!("[4] cleanup: remove the device from group {GROUP_ID}");
        node.remove_group(GROUPS_ENDPOINT, GROUP_ID)
            .await
            .context("remove_group")?;
        println!("    RemoveGroup ✓\n");
    }

    println!("== E3 group-multicast run complete — see the read-back ✓/✗ above ==");
    Ok(())
}
