//! End-to-end `matter-controller` quickstart: persist a fabric, commission a
//! device, control it, and stream live reports — then reconnect from the
//! snapshot on a later run without re-commissioning.
//!
//! First run (commission + control):
//! ```text
//! cargo run -p matter-controller --example controller_quickstart -- \
//!     --store /tmp/matter-controller.bin --commission "MT:Y.K90AFN00KA0648G00"
//! ```
//!
//! Later run (reconnect from the snapshot, no commissioning):
//! ```text
//! cargo run -p matter-controller --example controller_quickstart -- \
//!     --store /tmp/matter-controller.bin --node <NODE_ID>
//! ```
//!
//! Real certified devices need production attestation roots — pass
//! `--paa-dir <dir> --cd-dir <dir>` (e.g. connectedhomeip's
//! `credentials/production/{paa-root-certs,cd-certs}`). Without them the bundled
//! CSA **test** roots are used, which reject production devices.

// One long linear main with verbose prose, like the other examples in this crate.
#![allow(clippy::too_many_lines)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{
    AttestationTrust, CommandPath, FabricConfig, FileStore, MatterController, MatterTime, ReadPath,
    SubscriptionEvent, Value,
};

/// `OnOff` cluster on a typical plug/light endpoint.
const ONOFF_ENDPOINT: u16 = 1;
const ONOFF_CLUSTER: u32 = 0x0006;
const ONOFF_ATTR: u32 = 0x0000;
const ONOFF_CMD_TOGGLE: u32 = 0x02;

#[derive(Parser)]
#[command(about = "matter-controller quickstart: commission + control a device")]
struct Args {
    /// Path to the persistent controller snapshot.
    #[arg(long, default_value = "matter-controller.bin")]
    store: PathBuf,

    /// QR (`MT:…`) or manual pairing code to commission a new device.
    #[arg(long, conflicts_with = "node")]
    commission: Option<String>,

    /// Reconnect to an already-commissioned device by node id (no commissioning).
    #[arg(long)]
    node: Option<u64>,

    /// Directory of PAA root `.der` certs (production attestation).
    #[arg(long, requires = "cd_dir")]
    paa_dir: Option<PathBuf>,

    /// Directory (or file) of CD signing root `.der` certs.
    #[arg(long, requires = "paa_dir")]
    cd_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // 1. Attestation trust: production roots if supplied, else bundled CSA test roots.
    let trust = match (&args.paa_dir, &args.cd_dir) {
        (Some(paa), Some(cd)) => {
            AttestationTrust::from_dirs(paa, cd).context("loading production attestation roots")?
        }
        _ => AttestationTrust::example_device_roots(),
    };

    // 2. Open the controller over a persistent file store. A fresh store gets a
    //    new fabric (with a stable commissioner identity); an existing one is
    //    restored as-is.
    let fresh = !args.store.exists();
    let store = Arc::new(FileStore::new(&args.store));
    let controller = MatterController::builder(store)
        .attestation_trust(trust)
        .build()
        .await
        .context("opening controller")?;

    if fresh {
        // RCAC / commissioner-NOC validity: a real `notBefore` (backdated an hour
        // to tolerate device clock skew), no expiry. A zero `notBefore` (the
        // Matter 2000 epoch) re-encodes as the X.509 `99991231235959Z` sentinel
        // inside the device, breaking the certificate's signature — see
        // `FabricConfig::validity`. Take the clock reading as fallible rather
        // than substituting a constant: a wrong `notBefore` is exactly the
        // failure this guards against.
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("reading the system clock")?
            .as_secs();
        controller
            .create_fabric(FabricConfig::new(
                1,
                1,
                1,
                (
                    MatterTime::from_unix_secs(now_unix.saturating_sub(3600)),
                    MatterTime::NO_EXPIRY,
                ),
            ))
            .await
            .context("creating fabric")?;
        println!("created a new fabric (stable commissioner identity persisted)");
    }

    // 3. Get a device node: commission a new one, or reconnect to a persisted one.
    let node_id = match (&args.commission, args.node) {
        (Some(code), _) => {
            println!("commissioning…");
            let info = controller
                .commission(code, Some("quickstart device".into()))
                .await
                .context("commissioning")?;
            println!(
                "commissioned device as node 0x{:016X} (vendor {:?}, product {:?}, label {:?})",
                info.node_id, info.vendor_id, info.product_id, info.label
            );
            info.node_id
        }
        (None, Some(id)) => {
            println!("reconnecting to persisted node 0x{id:016X} (no commissioning)");
            id
        }
        (None, None) => {
            anyhow::bail!("pass --commission <code> for a new device, or --node <id> to reconnect");
        }
    };
    let node = controller.node(node_id);

    // 4. Read the current OnOff state.
    let report = node
        .read(&[ReadPath::concrete(
            ONOFF_ENDPOINT,
            ONOFF_CLUSTER,
            ONOFF_ATTR,
        )])
        .await
        .context("reading OnOff")?;
    let on = matches!(report.first().map(|(_, v)| v), Some(Value::Bool(true)));
    println!("OnOff = {on}");

    // 5. Toggle it (command fields are an empty structure for Toggle).
    node.invoke(
        CommandPath {
            endpoint: ONOFF_ENDPOINT,
            cluster: ONOFF_CLUSTER,
            command: ONOFF_CMD_TOGGLE,
        },
        Value::Structure(vec![]),
    )
    .await
    .context("invoking Toggle")?;
    println!("toggled");

    // 6. Subscribe to live changes (priming report + steady-state updates).
    let mut sub = node
        .subscribe(
            &[ReadPath::cluster(ONOFF_ENDPOINT, ONOFF_CLUSTER)],
            &[],
            1,
            30,
        )
        .await
        .context("subscribing")?;
    println!("subscribed; printing up to 3 reports (Ctrl-C to stop)…");
    for _ in 0..3 {
        match sub.next().await {
            Some(SubscriptionEvent::Report(change)) => {
                println!("  report: {:?} = {:?}", change.path, change.value);
            }
            Some(SubscriptionEvent::Established {
                subscription_id,
                max_interval,
                ..
            }) => {
                println!(
                    "  subscription established (id 0x{subscription_id:08X}, \
                     device reports at least every {max_interval}s)"
                );
            }
            Some(SubscriptionEvent::Resubscribing { cause }) => {
                println!("  resubscribing: {cause}");
            }
            Some(SubscriptionEvent::Lagged { dropped }) => {
                println!("  lagged: dropped {dropped} report(s) (consumer too slow)");
            }
            Some(_) => {}
            None => break,
        }
    }
    sub.cancel().await.ok();

    println!("done — state persisted to {}", args.store.display());
    Ok(())
}
