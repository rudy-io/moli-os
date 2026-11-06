//! M9-F4 operator harness: announce + serve a real `.ota` over BDX to a
//! commissioned requestor (e.g. connectedhomeip's `ota-requestor-app`).
//!
//! ```text
//! cargo run -p matter-controller --example serve_ota -- \
//!     --store /tmp/matter-ota.bin --node 5 --version 2 --image /tmp/test.ota
//! ```
//!
//! See docs/runbooks/m9-f4-ota-end-to-end.md for the full live procedure
//! (generating the image with chip's ota_image_tool.py, commissioning the
//! requestor, and watching it download + apply).

#![allow(clippy::too_many_lines, clippy::doc_markdown)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{FileStore, MatterController};

#[derive(Parser)]
#[command(about = "M9-F4 OTA provider: announce + serve a .ota over BDX")]
struct Args {
    /// Path to a persistent controller snapshot that already holds a fabric and
    /// has commissioned the requestor.
    #[arg(long)]
    store: PathBuf,

    /// Node id of the already-commissioned requestor to announce to.
    #[arg(long)]
    node: u64,

    /// SoftwareVersion to offer (must exceed the requestor's current version and
    /// match the version baked into the `.ota` header).
    #[arg(long)]
    version: u32,

    /// Path to the Matter `.ota` image (generate with chip's ota_image_tool.py).
    #[arg(long)]
    image: PathBuf,

    /// UDP port to bind the provider socket to (0 = ephemeral).
    #[arg(long, default_value_t = 5560)]
    port: u16,

    /// BDX max block size in bytes. 960 (the default) fits the Wi-Fi/IP
    /// secured-payload budget; use ~512 for a Thread-routed requestor so each
    /// block spans fewer 6LoWPAN fragments.
    #[arg(long, default_value_t = 960)]
    block_size: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let image = std::fs::read(&args.image).context("read .ota image")?;
    println!(
        "[ota] loaded {}-byte image; announcing to node {} + serving on port {} (block size {})…",
        image.len(),
        args.node,
        args.port,
        args.block_size
    );
    let store = Arc::new(FileStore::new(&args.store));
    let controller = MatterController::open(store)
        .await
        .context("open controller")?;
    controller
        .serve_ota_with_block_size(args.node, image, args.version, args.port, args.block_size)
        .await
        .context("serve_ota")?;
    println!("[ota] done — requestor downloaded + applied the image");
    Ok(())
}
