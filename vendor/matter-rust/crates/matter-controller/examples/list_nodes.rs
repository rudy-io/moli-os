//! Enumerate commissioned nodes via the typed `nodes()` accessor, and
//! optionally `forget_node` one — the 0.3.0 node-lifecycle API (no snapshot
//! deserialization required).
//!
//! ```text
//! cargo run -p matter-controller --example list_nodes -- --store /tmp/matter.bin
//! cargo run -p matter-controller --example list_nodes -- --store /tmp/matter.bin --forget-node 2
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{FileStore, MatterController};

#[derive(Parser)]
#[command(about = "list commissioned nodes (NodeInfo); optionally forget one")]
struct Args {
    #[arg(long)]
    store: PathBuf,
    /// If set, forget this node id (drop all local state) after listing.
    #[arg(long)]
    forget_node: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let store = Arc::new(FileStore::new(&args.store));
    let controller = MatterController::open(store)
        .await
        .context("opening controller")?;

    let nodes = controller.nodes().await.context("nodes()")?;
    println!("{} commissioned node(s):", nodes.len());
    for n in &nodes {
        println!(
            "  node 0x{:016X}  fabric 0x{:016X}  vendor {:?}  product {:?}  label {:?}",
            n.node_id, n.fabric_id, n.vendor_id, n.product_id, n.label
        );
    }

    if let Some(node_id) = args.forget_node {
        let removed = controller
            .forget_node(node_id)
            .await
            .context("forget_node")?;
        println!(
            "\nforget_node(0x{node_id:016X}) -> {} ({})",
            removed,
            if removed {
                "a node was dropped"
            } else {
                "no such node"
            }
        );
        let after = controller.nodes().await.context("nodes() after forget")?;
        println!("{} node(s) remain after forget", after.len());
    }
    Ok(())
}
