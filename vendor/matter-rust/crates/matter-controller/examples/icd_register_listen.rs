//! M9-G-c operator harness: register with an ICD, then listen for its Check-In.
//!
//! Registers the controller as a check-in client with an already-commissioned
//! ICD (e.g. connectedhomeip's `lit-icd-app`), then advertises + waits for the
//! device's periodic unsolicited Check-In, verifies it, and (optionally) asks
//! the device to stay active.
//!
//! ```text
//! cargo run -p matter-controller --example icd_register_listen -- \
//!     --store /tmp/matter-icd.bin --node 5 --port 5580
//! ```
//!
//! See docs/runbooks/m9-gc-icd.md for the full live procedure.

#![allow(clippy::too_many_lines, clippy::doc_markdown)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{FileStore, IcdClientType, MatterController};

#[derive(Parser)]
#[command(about = "M9-G-c ICD: register as check-in client + listen for a Check-In")]
struct Args {
    /// Path to a persistent controller snapshot that already commissioned the ICD.
    #[arg(long)]
    store: PathBuf,

    /// Node id of the already-commissioned ICD (e.g. lit-icd-app).
    #[arg(long)]
    node: u64,

    /// UDP port to bind the listener/advertiser socket to.
    #[arg(long, default_value_t = 5580)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let store = Arc::new(FileStore::new(&args.store));
    let controller = MatterController::open(store)
        .await
        .context("open controller")?;
    let node = controller.node(args.node);

    // Register as a check-in client. MonitoredSubject is the subject the ICD
    // watches on our behalf — typically the controller's own node id; the value
    // is stored by the device and does not affect the Check-In crypto, so this
    // demo reuses the device node id as an illustrative subject.
    println!(
        "[icd] registering as a check-in client with node {}…",
        args.node
    );
    let reg = node
        .register_icd_client(args.node, IcdClientType::Permanent)
        .await
        .context("register_icd_client")?;
    println!(
        "[icd] registered (start counter {}); advertising + waiting for a Check-In on port {}…",
        reg.start_counter, args.port
    );

    // Listen for the device's periodic Check-In.
    let checkin = controller
        .listen_for_checkin_once(args.port)
        .await
        .context("listen_for_checkin_once")?;
    println!(
        "[icd] Check-In received from node {} (counter {}, {} bytes app data)",
        checkin.node_id,
        checkin.counter,
        checkin.app_data.len()
    );

    // Ask the device to stay active so we can talk to it.
    match node.stay_active_request(30_000).await {
        Ok(ms) => println!("[icd] device promised to stay active for {ms} ms"),
        Err(e) => println!("[icd] stay_active_request failed (non-fatal): {e}"),
    }
    println!("[icd] done");
    Ok(())
}
