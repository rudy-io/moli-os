//! Live BLE→Thread commission (feature `ble`): commission a Matter-over-Thread
//! device over BLE/BTP and provision it onto an existing Thread network from an
//! operational dataset.
//!
//! This is the packaged counterpart to `ble_scan` for the M9-C2 live
//! validation — see `docs/runbooks/c2-thread-commission.md`. It runs the full
//! flow: scan by discriminator → BTP → PASE → attestation → NOC install →
//! `AddOrUpdateThreadNetwork` (the dataset) → `ConnectNetwork` (keyed by the
//! dataset's Extended PAN ID) → operational CASE over IP once the device joins
//! the mesh. Then it reads and toggles `OnOff` to confirm operational control.
//!
//! Run it from the Thread border-router host (the Pi's `BlueZ` is the proven BLE
//! path against the C6 rig). Capture the dataset **fresh** first — the Extended
//! PAN ID rotates if the Thread network re-forms:
//! ```text
//! DATASET=$(sudo ot-ctl dataset active -x | head -1)
//! cargo run -p matter-controller --example commission_ble_thread --features ble --release -- \
//!     --store /tmp/matter-c6.bin \
//!     --commission "MT:-24J0AFN00KA0648G00" \
//!     --dataset "$DATASET"
//! ```
//!
//! **Attestation roots are required for any real device, including the
//! esp-matter C6** — pass `--paa-dir <dir> --cd-dir <dir>`. The bundled default
//! ([`AttestationTrust::example_device_roots`]) carries a *synthetic* CD signing root
//! that no real device's CD is signed against, so it is only good for our own
//! tests. From a connectedhomeip checkout:
//!
//! * `--paa-dir` — `credentials/test/attestation/` (or our vendored
//!   `Chip-Test-PAA-*.der`) for test DACs; `credentials/production/paa-root-certs/`
//!   for certified devices.
//! * `--cd-dir` — `credentials/production/cd-certs/` (**yes, production**: the
//!   VID=0xFFF1 CD that `CONFIG_EXAMPLE_DAC_PROVIDER` devices serve is signed by
//!   the CSA's production "CD Signing Key 001", not by chip's test CD authority
//!   — see `tests/chip_cd_vector.rs` in `matter-commissioning`). Add
//!   `credentials/test/certification-declaration/Chip-Test-CD-Signing-Cert`
//!   (converted to DER) to also accept test-authority-signed CDs.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::Parser;
use matter_controller::{
    AttestationTrust, CommandPath, FabricConfig, FileStore, MatterController, MatterTime,
    NetworkCredentials, ReadPath, ThreadDataset, Value,
};

/// `OnOff` cluster on a typical light endpoint (the esp-matter C6 light).
const ONOFF_ENDPOINT: u16 = 1;
const ONOFF_CLUSTER: u32 = 0x0006;
const ONOFF_ATTR: u32 = 0x0000;
const ONOFF_CMD_TOGGLE: u32 = 0x02;

#[derive(Parser)]
#[command(about = "Commission a Matter-over-Thread device over BLE and control it")]
struct Args {
    /// Path to the persistent controller snapshot.
    #[arg(long, default_value = "matter-controller.bin")]
    store: PathBuf,

    /// QR (`MT:…`) or manual pairing code advertised by the device.
    #[arg(long)]
    commission: String,

    /// Thread operational dataset as hex — the exact output of
    /// `sudo ot-ctl dataset active -x` on the border router. **Capture it
    /// fresh**: the Extended PAN ID rotates if the network re-forms.
    #[arg(long)]
    dataset: String,

    /// Directory of PAA root `.der` certs (production attestation).
    #[arg(long, requires = "cd_dir")]
    paa_dir: Option<PathBuf>,

    /// Directory (or file) of CD signing root `.der` certs.
    #[arg(long, requires = "paa_dir")]
    cd_dir: Option<PathBuf>,
}

/// Decode a hex string (no separators) into bytes. Kept inline to avoid adding
/// a `hex` dependency for a single example.
fn hex_decode(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        bail!(
            "dataset hex has an odd number of characters ({} chars)",
            s.len()
        );
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .with_context(|| format!("invalid hex byte at offset {i}: {:?}", &s[i..i + 2]))
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // 1. Attestation trust: real roots if supplied, else the bundled test roots
    //    — whose synthetic CD root verifies no real device (see module docs).
    let trust = match (&args.paa_dir, &args.cd_dir) {
        (Some(paa), Some(cd)) => {
            AttestationTrust::from_dirs(paa, cd).context("loading production attestation roots")?
        }
        _ => AttestationTrust::example_device_roots(),
    };

    // 2. Validate the Thread dataset up front and echo the Extended PAN ID the
    //    device is expected to attach under — cross-check against the border
    //    router before scanning.
    let dataset = ThreadDataset::new(hex_decode(&args.dataset)?)
        .context("dataset is not a well-formed Thread operational dataset")?;
    println!(
        "Thread dataset accepted ({} bytes); expecting Ext-PAN-ID {:02x?}",
        args.dataset.trim().len() / 2,
        dataset.ext_pan_id()
    );

    // 3. Open the controller over a persistent file store; a fresh store gets a
    //    new fabric with a stable commissioner identity.
    let fresh = !args.store.exists();
    let store = Arc::new(FileStore::new(&args.store));
    let controller = MatterController::builder(store)
        .attestation_trust(trust)
        .build()
        .await
        .context("opening controller")?;

    if fresh {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1_700_000_000, |d| d.as_secs());
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

    // 4. Commission over BLE, provisioning Thread. This drives BTP → PASE →
    //    attestation → NOC → AddOrUpdateThreadNetwork → ConnectNetwork → CASE.
    println!("commissioning over BLE→Thread (this can take up to ~90 s at network-enable while the device attaches to the mesh)…");
    let node_id = controller
        .commission_ble(&args.commission, NetworkCredentials::Thread(dataset), None)
        .await
        .context("BLE→Thread commissioning")?
        .node_id;
    println!("commissioned over BLE as node 0x{node_id:016X}");

    // 5. Confirm operational control over the Thread-routed path: read OnOff,
    //    toggle, read back, confirm it flipped.
    let node = controller.node(node_id);
    let before = read_onoff(&node).await.context("reading OnOff")?;
    println!("OnOff = {before}");

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

    let after = read_onoff(&node).await.context("re-reading OnOff")?;
    println!(
        "OnOff after Toggle = {after} (flipped: {})",
        before != after
    );
    if before == after {
        bail!("OnOff did not change after Toggle — operational control over Thread not confirmed");
    }

    println!(
        "SUCCESS — commissioned and controlled node 0x{node_id:016X} over Thread; persisted to {}",
        args.store.display()
    );
    Ok(())
}

/// Read the boolean `OnOff` attribute on endpoint 1.
async fn read_onoff(node: &matter_controller::Node) -> Result<bool> {
    let report = node
        .read(&[ReadPath::concrete(
            ONOFF_ENDPOINT,
            ONOFF_CLUSTER,
            ONOFF_ATTR,
        )])
        .await?;
    Ok(matches!(
        report.first().map(|(_, v)| v),
        Some(Value::Bool(true))
    ))
}
