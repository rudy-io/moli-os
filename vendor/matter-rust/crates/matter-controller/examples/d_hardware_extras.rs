//! M9 extra real-device validation: event read (B1), event subscribe (B2),
//! timed interaction (B3, via AdminComm which is timed-required), and a
//! full-capacity / chunked ACL write (B4/D3).
//!
//! Reconnects to an already-commissioned device (no commissioning).
//!
//! ```text
//! cargo run -p matter-controller --example d_hardware_extras -- \
//!     --store /tmp/matter-d-test.bin --node 2
//! ```

// Operator validation example: one long linear main with verbose prose.
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{
    AclAuthMode, AclEntry, AclPrivilege, CommandPath, EventPath, FileStore, MatterController,
    OpenWindowOpts, ReadPath, SubscriptionEvent, Value,
};

const ACCESS_CONTROL_CLUSTER: u32 = 0x001F;
/// `AccessControlEntriesPerFabric` (0x001F / 0x0004).
const ATTR_ENTRIES_PER_FABRIC: u32 = 0x0004;
/// `SubjectsPerAccessControlEntry` (0x001F / 0x0002).
const ATTR_SUBJECTS_PER_ENTRY: u32 = 0x0002;
const ONOFF_ENDPOINT: u16 = 1;
const ONOFF_CLUSTER: u32 = 0x0006;
const ONOFF_CMD_TOGGLE: u32 = 0x02;
const BASIC_INFORMATION: u32 = 0x0028;
const GENERAL_DIAGNOSTICS: u32 = 0x0033;

#[derive(Parser)]
#[command(about = "M9 extra hardware validation (events, timed, chunked ACL)")]
struct Args {
    #[arg(long)]
    store: PathBuf,
    #[arg(long)]
    node: u64,
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
        "== M9 extra validation against node 0x{:016X} ==\n",
        args.node
    );

    // ---- B1: event read ----------------------------------------------------
    println!("[B1] read_events (BasicInformation + GeneralDiagnostics on ep0)");
    let event_paths = [
        EventPath::cluster(0, BASIC_INFORMATION),
        EventPath::cluster(0, GENERAL_DIAGNOSTICS),
    ];
    let events = node
        .read_events(&event_paths, &[])
        .await
        .context("read_events")?;
    println!("    got {} event report(s)", events.len());
    for ev in &events {
        println!("    {ev:?}");
    }
    println!();

    // ---- B2: subscribe with events -----------------------------------------
    println!("[B2] subscribe (OnOff attrs ep1 + BasicInformation events ep0)");
    let mut sub = node
        .subscribe(
            &[ReadPath::cluster(ONOFF_ENDPOINT, ONOFF_CLUSTER)],
            &[EventPath::cluster(0, BASIC_INFORMATION)],
            1,
            30,
        )
        .await
        .context("subscribe")?;
    // Toggle once to generate a live attribute report on the subscription.
    node.invoke(
        CommandPath {
            endpoint: ONOFF_ENDPOINT,
            cluster: ONOFF_CLUSTER,
            command: ONOFF_CMD_TOGGLE,
        },
        Value::Structure(vec![]),
    )
    .await
    .context("toggle to drive a report")?;
    println!("    toggled OnOff; printing up to 4 subscription events…");
    for _ in 0..4 {
        match sub.next().await {
            Some(SubscriptionEvent::Established {
                subscription_id,
                max_interval,
                ..
            }) => {
                println!(
                    "    established (id 0x{subscription_id:08X}, max interval {max_interval}s)"
                );
            }
            Some(SubscriptionEvent::Report(r)) => {
                println!("    attr report: {:?} = {:?}", r.path, r.value);
            }
            Some(SubscriptionEvent::Event(e)) => {
                println!("    EVENT report: {e:?}");
            }
            Some(other) => println!("    {other:?}"),
            None => break,
        }
    }
    sub.cancel().await.ok();
    // Toggle back to leave OnOff as we found it.
    node.invoke(
        CommandPath {
            endpoint: ONOFF_ENDPOINT,
            cluster: ONOFF_CLUSTER,
            command: ONOFF_CMD_TOGGLE,
        },
        Value::Structure(vec![]),
    )
    .await
    .ok();
    println!("    (toggled back)\n");

    // ---- B3: timed interaction via AdminComm (timed-required) --------------
    println!("[B3] timed interaction: open_commissioning_window (timed) then revoke (timed)");
    let win = node
        .open_commissioning_window(OpenWindowOpts::default())
        .await
        .context("open_commissioning_window (timed invoke)")?;
    println!("    window opened via timed invoke ✓");
    println!(
        "    onboarding manual_code={} discriminator={}",
        win.manual_code, win.discriminator
    );
    let st = node
        .commissioning_window_status()
        .await
        .context("status after open")?;
    println!("    WindowStatus now: {:?}", st.status);
    node.revoke_commissioning()
        .await
        .context("revoke_commissioning (timed invoke)")?;
    println!("    window revoked via timed invoke ✓");
    let st2 = node
        .commissioning_window_status()
        .await
        .context("status after revoke")?;
    println!("    WindowStatus now: {:?}\n", st2.status);

    // ---- B4/D3: full-capacity / chunked ACL write --------------------------
    println!("[D3] ACL capacity + (maybe) multi-chunk write");
    let cap = node
        .read(&[
            ReadPath::concrete(0, ACCESS_CONTROL_CLUSTER, ATTR_ENTRIES_PER_FABRIC),
            ReadPath::concrete(0, ACCESS_CONTROL_CLUSTER, ATTR_SUBJECTS_PER_ENTRY),
        ])
        .await
        .context("read ACL capacity")?;
    let entries_per_fabric = cap
        .iter()
        .find(|(p, _)| p.attribute == ATTR_ENTRIES_PER_FABRIC)
        .and_then(|(_, v)| {
            if let Value::Uint(n) = v {
                Some(*n)
            } else {
                None
            }
        })
        .unwrap_or(0);
    let subjects_per_entry = cap
        .iter()
        .find(|(p, _)| p.attribute == ATTR_SUBJECTS_PER_ENTRY)
        .and_then(|(_, v)| {
            if let Value::Uint(n) = v {
                Some(*n)
            } else {
                None
            }
        })
        .unwrap_or(0);
    println!("    AccessControlEntriesPerFabric={entries_per_fabric} SubjectsPerEntry={subjects_per_entry}");

    let original = node
        .read_acl()
        .await
        .context("read_acl (capture original)")?;
    // Keep the original (Administer) entries; append Operate entries packed with
    // subjects to inflate the encoded size. write_acl's internal budget is 800
    // unencrypted bytes, so if the encoded list exceeds that the write MUST be
    // chunked into ≥2 WriteRequestMessages — a successful large write is itself
    // proof the multi-chunk path works on hardware.
    let base: Vec<AclEntry> = original
        .iter()
        .cloned()
        .map(|mut e| {
            e.fabric_index = None;
            e
        })
        .collect();
    let subj = subjects_per_entry.clamp(1, 4) as usize;
    let mut big = base.clone();
    // Fill up to the device's per-fabric capacity with fat Operate entries.
    let target = entries_per_fabric.min(64) as usize;
    let mut next_subject: u64 = 0xCAFE_0000;
    while big.len() < target {
        let subjects: Vec<u64> = (0..subj)
            .map(|_| {
                next_subject += 1;
                next_subject
            })
            .collect();
        big.push(AclEntry::new(
            AclPrivilege::Operate,
            AclAuthMode::Case,
            Some(subjects),
            None,
        ));
    }
    // Rough encoded-size estimate: ~ (8 + 9*subjects) bytes per entry.
    let est = big
        .iter()
        .map(|e| 8 + 9 * e.subjects.as_ref().map_or(0, Vec::len))
        .sum::<usize>();
    println!(
        "    writing {} entries (~{} bytes est; >800 ⇒ multi-chunk)…",
        big.len(),
        est
    );
    match node.write_acl(&big).await {
        Ok(statuses) => {
            let after = node.read_acl().await.context("read_acl after big write")?;
            println!("    write OK -> statuses {statuses:?}");
            println!("    read_acl now: {} entry(ies)", after.len());
            if est > 800 {
                println!("    => encoded list exceeded the 800B budget: MULTI-CHUNK write exercised on hardware ✓");
            } else {
                println!("    => list fit one message (device capacity too small to force multi-chunk; single-chunk validated)");
            }
        }
        Err(e) => {
            println!("    write rejected ({e}) — likely over device ACL capacity; restoring");
        }
    }
    // Restore the original ACL no matter what.
    let restored = node.write_acl(&base).await.context("write_acl restore")?;
    let final_acl = node.read_acl().await.context("read_acl after restore")?;
    println!(
        "    restored original ACL -> statuses {restored:?}; now {} entry(ies)\n",
        final_acl.len()
    );

    println!("== M9 extra validation complete — device left as found ==");
    Ok(())
}
