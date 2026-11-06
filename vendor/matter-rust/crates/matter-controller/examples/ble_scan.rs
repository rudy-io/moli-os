//! Live BLE scan for commissionable Matter devices (feature `ble`).
//!
//! This is a hardware/permission diagnostic, **not** a test. It constructs a
//! real `BleCentral`, which instantiates `CoreBluetooth` on macOS and may raise
//! the one-time Bluetooth permission prompt (attributed to the terminal app).
//! For that reason it refuses to touch Bluetooth unless `MATTER_BLE_LIVE=1` is
//! set — a default run prints the runbook pointer and exits 0.
//!
//! One-time TCC approval + live scan:
//! ```text
//! MATTER_BLE_LIVE=1 cargo run -p matter-controller --example ble_scan --features ble
//! ```
//! Answer the Bluetooth prompt, then verify under System Settings → Privacy &
//! Security → Bluetooth. See `docs/runbooks/ble-commissioning.md`.

use std::time::Duration;

use matter_ble::central::{BleCentral, CentralError};

/// Per-nibble scan budget. `BleCentral` matches by discriminator, so a full
/// sweep probes each of the 16 short-discriminator nibbles in turn; 2 s each
/// keeps the whole pass near the advertised ~30 s.
const PER_NIBBLE: Duration = Duration::from_secs(2);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("MATTER_BLE_LIVE").as_deref() != Ok("1") {
        println!(
            "ble_scan is gated: it opens CoreBluetooth and may raise the macOS \
             Bluetooth permission prompt.\n\
             Set MATTER_BLE_LIVE=1 to run a live scan:\n\
             \n    MATTER_BLE_LIVE=1 cargo run -p matter-controller \
             --example ble_scan --features ble\n\n\
             See docs/runbooks/ble-commissioning.md."
        );
        return Ok(());
    }

    println!("Acquiring Bluetooth adapter (may prompt for permission)...");
    let central = BleCentral::new().await?;

    // `BleCentral` exposes `find_device` (first match for a given
    // discriminator), not a scan-all, so sweep all 16 short-discriminator
    // nibbles and print each device that answers. Each match is reported once
    // per nibble it advertises under.
    println!("Sweeping for commissionable Matter devices (~30 s)...");
    let mut count = 0u32;
    for nibble in 0u16..16 {
        match central.find_device(nibble, true, PER_NIBBLE).await {
            Ok(dev) => {
                count += 1;
                println!(
                    "  found: discriminator=0x{:03x} vid=0x{:04x} pid=0x{:04x} id={:?}",
                    dev.advert.discriminator,
                    dev.advert.vendor_id,
                    dev.advert.product_id,
                    dev.peripheral_id,
                );
            }
            Err(CentralError::ScanTimeout) => {}
            Err(e) => return Err(e.into()),
        }
    }

    println!("Scan complete: {count} device(s) answered.");
    Ok(())
}
