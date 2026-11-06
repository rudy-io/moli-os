//! Turn a persisted device's `OnOff` state on or off.
//!
//! ```text
//! cargo run -p matter-controller --example power -- --store /tmp/matter-controller.bin --node 2 --on \
//!   --paa-dir <paa> --cd-dir <cd>
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{
    AttestationTrust, CommandPath, FileStore, MatterController, ReadPath, Value,
};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "matter-controller.bin")]
    store: PathBuf,
    #[arg(long, default_value_t = 2)]
    node: u64,
    /// Turn the device on.
    #[arg(long, conflicts_with = "off")]
    on: bool,
    /// Turn the device off.
    #[arg(long)]
    off: bool,
    #[arg(long, requires = "cd_dir")]
    paa_dir: Option<PathBuf>,
    #[arg(long, requires = "paa_dir")]
    cd_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    // OnOff cluster 0x0006: On = 0x01, Off = 0x00, Toggle = 0x02.
    let (cmd, label) = if args.on {
        (0x01, "on")
    } else if args.off {
        (0x00, "off")
    } else {
        (0x02, "toggle")
    };

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

    println!("turning device {label}…");
    node.invoke(
        CommandPath {
            endpoint: 1,
            cluster: 0x0006,
            command: cmd,
        },
        Value::Structure(vec![]),
    )
    .await
    .context("OnOff command")?;

    let after = node
        .read(&[ReadPath::concrete(1, 0x0006, 0x0000)])
        .await
        .context("read-back OnOff")?;
    println!("OnOff now = {:?}", after.first().map(|(_, v)| v));
    Ok(())
}
