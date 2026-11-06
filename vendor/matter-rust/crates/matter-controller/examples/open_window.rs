//! TEMP: open a long enhanced commissioning window on an already-commissioned
//! device and print the manual pairing code, so a SECOND admin (chip-tool) can
//! commission the same device onto its own fabric for the group-multicast
//! reference test. The window stays open device-side after this process exits.
//!
//! Run: `cargo run -p matter-controller --example open_window -- --store <s> --node 2 --timeout 600`

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{FileStore, MatterController, OpenWindowOpts};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    store: PathBuf,
    #[arg(long)]
    node: u64,
    #[arg(long, default_value_t = 600)]
    timeout: u16,
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

    println!(
        "Opening a {}-second commissioning window on node 0x{:016X}…",
        args.timeout, args.node
    );
    let mut opts = OpenWindowOpts::default();
    opts.timeout_s = args.timeout;
    let win = node
        .open_commissioning_window(opts)
        .await
        .context("open_commissioning_window")?;

    println!("\n=== commissioning window OPEN for {}s ===", args.timeout);
    println!("  manual pairing code : {}", win.manual_code);
    println!("  discriminator       : {}", win.discriminator);
    println!("  passcode            : {}", win.passcode);
    println!("  (window stays open device-side after this exits)\n");
    Ok(())
}
