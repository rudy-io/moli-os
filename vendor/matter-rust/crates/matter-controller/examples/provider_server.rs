//! M9-F3 operator harness: run the OTA **provider server** once.
//!
//! Advertises our operational `_matter._tcp` service, accepts ONE inbound CASE
//! session, and dispatches one server-side `InvokeRequest` (replying SUCCESS),
//! then exits. This is the manual validation for the F3 "a foreign requestor
//! discovers + CASE-connects to our provider server and gets a response" path
//! (the automated floor is the in-process loopback unit test).
//!
//! ```text
//! # Terminal 1 — our provider server (must point at an already-commissioned store):
//! cargo run -p matter-controller --example provider_server -- \
//!     --store /tmp/matter-d-test.bin --port 5541
//!
//! # Terminal 2 — a second matter-rust controller (or chip-tool) resolves
//! # _matter._tcp and CASE-connects + invokes any command on us. The server
//! # logs one dispatched invoke and exits. See docs/runbooks/m9-f3-provider-server.md.
//! ```

#![allow(clippy::too_many_lines, clippy::doc_markdown)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{FileStore, MatterController};

#[derive(Parser)]
#[command(about = "M9-F3 OTA provider server (advertise + accept one CASE session + dispatch)")]
struct Args {
    /// Path to a persistent controller snapshot that already holds a fabric
    /// (the one used to commission — the provider authenticates as its
    /// commissioner identity).
    #[arg(long)]
    store: PathBuf,

    /// UDP port to bind the provider socket to (0 = ephemeral).
    #[arg(long, default_value_t = 5541)]
    port: u16,

    /// How many server-side invokes to dispatch before exiting.
    #[arg(long, default_value_t = 1)]
    max_invokes: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let store = Arc::new(FileStore::new(&args.store));
    let controller = MatterController::open(store)
        .await
        .context("open controller from store")?;

    println!(
        "[provider] advertising operational service on port {}; waiting for an inbound CASE session (max_invokes={})…",
        args.port, args.max_invokes
    );

    // Reply SUCCESS to whatever the requestor invokes (F3 proves the plumbing;
    // F4 plugs in the real OTA QueryImage handler + BDX transfer).
    let dispatched = controller
        .serve_provider_once(
            args.port,
            |req: &matter_interaction::ParsedInvokeRequest| {
                let path = req.commands[0].path;
                println!(
                    "[provider] dispatching server-side invoke (ep {}, cluster {:#06x}, cmd {:#04x}) → SUCCESS",
                    path.endpoint, path.cluster, path.command
                );
                matter_interaction::build_invoke_response_status(
                    path,
                    matter_interaction::ImStatus::Success,
                )
            },
            args.max_invokes,
        )
        .await
        .context("serve provider once")?;

    println!("[provider] done; dispatched {dispatched} invoke(s)");
    Ok(())
}
