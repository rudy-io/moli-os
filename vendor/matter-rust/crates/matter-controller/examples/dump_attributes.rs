//! Dump every attribute from a persisted device via a full wildcard read.
//!
//! Reconnects to an already-commissioned device (no re-commission) and prints
//! its complete attribute set. With chunked-read reassembly the result spans
//! every endpoint, not just the first `ReportData` chunk.
//!
//! ```text
//! cargo run -p matter-controller --example dump_attributes -- \
//!   --store /tmp/matter-controller.bin --node 2 --paa-dir <paa> --cd-dir <cd>
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{AttestationTrust, FileStore, MatterController, ReadPath};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "matter-controller.bin")]
    store: PathBuf,
    #[arg(long, default_value_t = 2)]
    node: u64,
    #[arg(long, requires = "cd_dir")]
    paa_dir: Option<PathBuf>,
    #[arg(long, requires = "paa_dir")]
    cd_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let trust = match (&args.paa_dir, &args.cd_dir) {
        (Some(p), Some(c)) => AttestationTrust::from_dirs(p, c).context("attestation roots")?,
        _ => AttestationTrust::example_device_roots(),
    };
    let store = Arc::new(FileStore::new(&args.store));
    let controller = MatterController::builder(store)
        .attestation_trust(trust)
        .build()
        .await
        .context("open controller")?;
    let node = controller.node(args.node);

    let attrs = node
        .read(&[ReadPath::all()])
        .await
        .context("wildcard read")?;
    println!("read {} attribute(s):", attrs.len());
    for (path, value) in &attrs {
        println!(
            "  ep{:<3} cluster {:#06x} attr {:#06x} = {value:?}",
            path.endpoint, path.cluster, path.attribute
        );
    }
    Ok(())
}
