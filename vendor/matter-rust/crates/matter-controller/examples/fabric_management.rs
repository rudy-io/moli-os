//! M9-D real-device validation: exercise the multi-admin / ACL / fabric-management
//! verbs against an already-commissioned device (reconnect by node id — no
//! commissioning window needed for any of this).
//!
//! ```text
//! cargo run -p matter-controller --example fabric_management -- \
//!     --store /tmp/matter-d-test.bin --node 2
//! ```
//!
//! It runs a non-destructive sequence and restores anything it changes:
//!   1. commissioning_window_status()
//!   2. list_fabrics()              (shows every fabric the device is on)
//!   3. read_acl()
//!   4. update_fabric_label() round-trip (set → confirm → restore)
//!   5. write_acl() round-trip (append a benign Operate entry → confirm → restore)
//!   6. remove_fabric(<our own index>) → expects WouldRemoveSelf (the guard, on hardware)
//!
//! It never removes our own fabric and never drops our Administer ACL entry, so
//! the device is left exactly as it started (except it remains commissioned onto
//! our fabric — remove that via the device's app when done).

// This is an operator validation example: a single linear `main` with verbose
// prose doc lines is the point, so relax the two pedantic lints that fights that.
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::Parser;
use matter_controller::{
    AclAuthMode, AclEntry, AclPrivilege, Error as ControllerError, FileStore, MatterController,
};

/// Our fabric id (matches `FabricConfig::new(1, 1, 1, ..)` used at commission time).
const OUR_FABRIC_ID: u64 = 1;

#[derive(Parser)]
#[command(about = "M9-D real-device validation (fabric mgmt + ACL)")]
struct Args {
    /// Path to the persistent controller snapshot (the one used to commission).
    #[arg(long)]
    store: PathBuf,

    /// Node id of the already-commissioned device to exercise.
    #[arg(long)]
    node: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if !args.store.exists() {
        bail!(
            "store {} does not exist — commission first",
            args.store.display()
        );
    }
    let store = Arc::new(FileStore::new(&args.store));
    // No attestation trust needed: we reconnect operationally, we don't commission.
    let controller = MatterController::builder(store)
        .build()
        .await
        .context("opening controller")?;
    let node = controller.node(args.node);
    println!("== M9-D validation against node 0x{:016X} ==\n", args.node);

    // ---- 1. Commissioning window status -------------------------------------
    let status = node
        .commissioning_window_status()
        .await
        .context("commissioning_window_status")?;
    println!("[1] WindowStatus: {:?}", status.status);
    println!(
        "    admin_fabric_index={:?} admin_vendor_id={:?}\n",
        status.admin_fabric_index, status.admin_vendor_id
    );

    // ---- 2. List fabrics ----------------------------------------------------
    let fabrics = node.list_fabrics().await.context("list_fabrics")?;
    println!(
        "[2] list_fabrics: {} fabric(s) on the device",
        fabrics.len()
    );
    for f in &fabrics {
        println!(
            "    idx={} fabric_id=0x{:016X} vendor=0x{:04X} node=0x{:016X} label={:?}",
            f.fabric_index, f.fabric_id, f.vendor_id, f.node_id, f.label
        );
    }
    let our = fabrics
        .iter()
        .find(|f| f.fabric_id == OUR_FABRIC_ID)
        .context("could not find our own fabric (fabric_id=1) in the device's fabric list")?;
    let our_index = our.fabric_index;
    let original_label = our.label.clone();
    println!("    -> our fabric index = {our_index} (label {original_label:?})\n");

    // ---- 3. Read ACL --------------------------------------------------------
    let acl = node.read_acl().await.context("read_acl")?;
    println!("[3] read_acl: {} entry(ies)", acl.len());
    for e in &acl {
        println!(
            "    privilege={:?} auth_mode={:?} subjects={:?} targets={:?} fabric_index={:?}",
            e.privilege, e.auth_mode, e.subjects, e.targets, e.fabric_index
        );
    }
    println!();

    // ---- 4. update_fabric_label round-trip ----------------------------------
    println!("[4] update_fabric_label round-trip");
    node.update_fabric_label("matter-rust-d3")
        .await
        .context("update_fabric_label set")?;
    let relabeled = node
        .list_fabrics()
        .await
        .context("list_fabrics after relabel")?;
    let new_label = relabeled
        .iter()
        .find(|f| f.fabric_index == our_index)
        .map(|f| f.label.clone());
    println!("    set 'matter-rust-d3' -> readback {new_label:?}");
    // restore
    node.update_fabric_label(&original_label)
        .await
        .context("update_fabric_label restore")?;
    println!("    restored original label {original_label:?}\n");

    // ---- 5. write_acl round-trip (append a benign Operate entry, then restore)
    println!("[5] write_acl round-trip");
    // Re-write the existing entries (null fabric_index — the device assigns it)
    // plus one extra Operate/CASE entry for a dummy subject. The existing
    // Administer entry is preserved, so the lockout guard passes.
    let base: Vec<AclEntry> = acl
        .iter()
        .cloned()
        .map(|mut e| {
            e.fabric_index = None;
            e
        })
        .collect();
    let extra = AclEntry::new(
        AclPrivilege::Operate,
        AclAuthMode::Case,
        Some(vec![0x0000_0000_0000_ACED]),
        None,
    );
    let mut augmented = base.clone();
    augmented.push(extra);
    let statuses = node
        .write_acl(&augmented)
        .await
        .context("write_acl append")?;
    println!(
        "    wrote {} entries -> statuses {:?}",
        augmented.len(),
        statuses
    );
    let after = node.read_acl().await.context("read_acl after append")?;
    println!("    read_acl now: {} entry(ies)", after.len());
    // restore the original ACL
    let restored = node.write_acl(&base).await.context("write_acl restore")?;
    println!("    restored original ACL -> statuses {restored:?}");
    let final_acl = node.read_acl().await.context("read_acl after restore")?;
    println!(
        "    read_acl after restore: {} entry(ies)\n",
        final_acl.len()
    );

    // ---- 6. remove_fabric self-protection (the guard, on hardware) ----------
    println!("[6] remove_fabric self-protection");
    match node.remove_fabric(our_index).await {
        Err(ControllerError::WouldRemoveSelf) => {
            println!("    remove_fabric({our_index}) -> WouldRemoveSelf  ✓ (guard fired, no fabric removed)\n");
        }
        Err(e) => bail!("expected WouldRemoveSelf removing our own fabric, got: {e}"),
        Ok(()) => bail!("DANGER: remove_fabric removed our own fabric — guard FAILED"),
    }

    println!("== M9-D validation complete — device left as found (still on our fabric) ==");
    Ok(())
}
