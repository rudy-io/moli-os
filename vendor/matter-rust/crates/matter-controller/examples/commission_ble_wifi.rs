//! Live BLE→Wi-Fi commission (feature `ble`): commission a factory-fresh
//! Matter-over-Wi-Fi device over BLE/BTP and provision it onto a Wi-Fi network.
//!
//! This is the M9-C1 counterpart to `commission_ble_thread` — same BLE/BTP path,
//! different network-provisioning arm (`AddOrUpdateWiFiNetwork` instead of
//! `AddOrUpdateThreadNetwork`). It runs the full flow: scan by discriminator →
//! BTP → PASE → attestation → NOC install → `AddOrUpdateWiFiNetwork` →
//! `ConnectNetwork` (keyed by SSID) → operational CASE over IP once the device
//! joins the network. Then it reads and toggles `OnOff` to confirm control.
//!
//! Run it from a host with a working BLE central **on the same IP network the
//! device is joining** — the post-join CASE handshake reaches the device over
//! that network. The Pi's `BlueZ` is the proven BLE path against the C6 rig.
//!
//! ```text
//! MATTER_BLE_LIVE=1 cargo run -p matter-controller --example commission_ble_wifi \
//!     --features ble --release -- \
//!     --store /tmp/matter-c6-wifi.bin \
//!     --commission "MT:-24J0AFN00KA0648G00" \
//!     --ssid "MyNetwork" --password "hunter2" \
//!     --paa-dir ~/paa-roots --cd-dir ~/cd-roots
//! ```
//!
//! **Attestation roots are required for any real device** — see
//! `commission_ble_thread`'s module docs and `docs/tested-devices.md`. The short
//! version: the bundled default carries a *synthetic* CD root that verifies no
//! real device, and the CD that `CONFIG_EXAMPLE_DAC_PROVIDER` devices serve
//! needs the CSA **production** `credentials/production/cd-certs/`, while their
//! DAC needs chip's **test** PAA.
//!
//! The ESP32-C6 is 2.4 GHz-only — a 5 GHz-only SSID will pass provisioning and
//! then fail to associate.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::Parser;
use matter_controller::{
    AttestationTrust, CommandPath, FabricConfig, FileStore, MatterController, MatterTime,
    NetworkCredentials, ReadPath, Value, WiFiCredentials,
};

/// `OnOff` cluster on a typical light endpoint (the esp-matter C6 light).
const ONOFF_ENDPOINT: u16 = 1;
const ONOFF_CLUSTER: u32 = 0x0006;
const ONOFF_ATTR: u32 = 0x0000;
const ONOFF_CMD_TOGGLE: u32 = 0x02;

/// Matter spec bounds, enforced here so a typo fails before we touch the radio
/// rather than mid-commission with an armed failsafe.
const SSID_MAX: usize = 32;
const PASSWORD_MAX: usize = 64;

#[derive(Parser)]
#[command(about = "Commission a Matter-over-Wi-Fi device over BLE and control it")]
struct Args {
    /// Path to the persistent controller snapshot.
    #[arg(long, default_value = "matter-controller.bin")]
    store: PathBuf,

    /// QR (`MT:…`) or manual pairing code advertised by the device.
    #[arg(long)]
    commission: String,

    /// SSID of the Wi-Fi network to provision the device onto (1–32 bytes).
    #[arg(long)]
    ssid: String,

    /// Wi-Fi passphrase (0–64 bytes; omit for an open network). Prefer
    /// `--password-env` to keep the secret out of your shell history.
    #[arg(long, default_value = "")]
    password: String,

    /// Read the passphrase from this environment variable instead of `--password`.
    #[arg(long, conflicts_with = "password")]
    password_env: Option<String>,

    /// Directory of PAA root `.der` certs (device attestation chain).
    #[arg(long, requires = "cd_dir")]
    paa_dir: Option<PathBuf>,

    /// Directory (or file) of CD signing root `.der` certs.
    #[arg(long, requires = "paa_dir")]
    cd_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // 1. Attestation trust: real roots if supplied, else the bundled test roots
    //    — whose synthetic CD root verifies no real device (see module docs).
    let trust = match (&args.paa_dir, &args.cd_dir) {
        (Some(paa), Some(cd)) => {
            AttestationTrust::from_dirs(paa, cd).context("loading attestation roots")?
        }
        _ => AttestationTrust::example_device_roots(),
    };

    // 2. Validate the credentials up front — a bad SSID/password should fail
    //    here, not after PASE with a failsafe armed on the device.
    let password = match &args.password_env {
        Some(var) => {
            std::env::var(var).with_context(|| format!("reading Wi-Fi passphrase from ${var}"))?
        }
        None => args.password.clone(),
    };
    if args.ssid.is_empty() || args.ssid.len() > SSID_MAX {
        bail!("SSID must be 1–{SSID_MAX} bytes (got {})", args.ssid.len());
    }
    if password.len() > PASSWORD_MAX {
        bail!(
            "Wi-Fi passphrase must be 0–{PASSWORD_MAX} bytes (got {})",
            password.len()
        );
    }
    println!(
        "Wi-Fi credentials accepted (SSID {:?}, passphrase <{} bytes>)",
        args.ssid,
        password.len()
    );
    let wifi = WiFiCredentials {
        ssid: args.ssid.clone().into_bytes(),
        credentials: password.into_bytes(),
    };

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

    // 4. Commission over BLE, provisioning Wi-Fi. This drives BTP → PASE →
    //    attestation → NOC → AddOrUpdateWiFiNetwork → ConnectNetwork → CASE.
    println!(
        "commissioning over BLE→Wi-Fi (network-enable waits while the device associates \
         and registers over mDNS)…"
    );
    let node_id = controller
        .commission_ble(&args.commission, NetworkCredentials::WiFi(wifi), None)
        .await
        .context("BLE→Wi-Fi commissioning")?
        .node_id;
    println!("commissioned over BLE as node 0x{node_id:016X}");

    // 5. Confirm operational control over the Wi-Fi-routed path: read OnOff,
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
        bail!("OnOff did not change after Toggle — operational control over Wi-Fi not confirmed");
    }

    println!(
        "SUCCESS — commissioned and controlled node 0x{node_id:016X} over Wi-Fi; persisted to {}",
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
