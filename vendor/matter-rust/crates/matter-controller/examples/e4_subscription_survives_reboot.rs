//! M8 SH.2b real-device validation: an attribute subscription transparently
//! survives a device reboot via auto-resubscribe.
//!
//! Subscribes to OnOff (ep1), holds the SAME [`Subscription`] handle, and
//! streams events for `--watch-secs`. Reboot the device partway through (the
//! run prints a prompt; on the rig we hard-reset the C6 over serial from a
//! second shell). The expected timeline on ONE handle is:
//!
//! ```text
//!   Established(id=A)                      ← initial subscription
//!   Report ...                             ← steady state
//!   Resubscribing { cause: … }             ← session lost when the device rebooted
//!   Established(id=B)                       ← SH.2b re-established it, no new handle
//!   Report ...                             ← priming reports resume
//! ```
//!
//! ```text
//! cargo run -p matter-controller --example e4_subscription_survives_reboot -- \
//!     --store /tmp/matter-c6-thread.bin --node 2 --watch-secs 150
//! ```

// Operator validation example: one long linear main with verbose prose.
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use matter_controller::{FileStore, MatterController, ReadPath, SubscriptionEvent};

const ONOFF_ENDPOINT: u16 = 1;
const ONOFF_CLUSTER: u32 = 0x0006;

#[derive(Parser)]
#[command(about = "SH.2b subscription-survives-reboot hardware validation")]
struct Args {
    #[arg(long)]
    store: PathBuf,
    #[arg(long)]
    node: u64,
    /// How long to hold the subscription and stream events (seconds). Reboot
    /// the device within this window.
    #[arg(long, default_value_t = 150)]
    watch_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let store = Arc::new(FileStore::new(&args.store));
    let controller = MatterController::builder(store)
        .build()
        .await
        .context("opening controller")?;
    let node = controller.node(args.node);
    println!(
        "== SH.2b subscription-survives-reboot against node 0x{:016X} ==\n",
        args.node
    );

    // Short intervals so priming + steady-state reports arrive promptly and the
    // liveness deadline (≈ max_interval + network slack) trips soon after the
    // device stops answering — no need to wait out a long ceiling to see the
    // resubscribe engine react.
    let mut sub = node
        .subscribe(
            &[ReadPath::cluster(ONOFF_ENDPOINT, ONOFF_CLUSTER)],
            &[],
            1,
            10,
        )
        .await
        .context("subscribe")?;

    println!(
        "[1] subscription opened. Streaming for {}s.",
        args.watch_secs
    );
    println!("    >>> REBOOT THE DEVICE partway through this window <<<");
    println!("    (on the rig: esptool --after hard-reset read-mac, from a second shell)\n");

    // Track the SH.2b lifecycle across the reboot, all on this ONE handle.
    let mut established_ids: Vec<u32> = Vec::new();
    let mut resubscribing_seen = false;
    let mut reports_after_resubscribe = 0usize;
    let start = Instant::now();
    let deadline = start + Duration::from_secs(args.watch_secs);

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let next = tokio::time::timeout(remaining, sub.next()).await;
        let t = start.elapsed().as_secs_f32();
        match next {
            Err(_) => break, // watch window elapsed
            Ok(None) => {
                println!("[{t:6.1}s] subscription ended (handle closed)");
                break;
            }
            Ok(Some(ev)) => match ev {
                SubscriptionEvent::Established {
                    subscription_id,
                    max_interval,
                    ..
                } => {
                    let phase = if established_ids.is_empty() {
                        "initial"
                    } else {
                        "RE-ESTABLISHED (SH.2b auto-resubscribe)"
                    };
                    println!(
                        "[{t:6.1}s] Established id=0x{subscription_id:08X} \
                         max_interval={max_interval}s  <- {phase}"
                    );
                    established_ids.push(subscription_id);
                }
                SubscriptionEvent::Resubscribing { cause } => {
                    resubscribing_seen = true;
                    println!("[{t:6.1}s] Resubscribing  cause: {cause}");
                }
                SubscriptionEvent::Report(r) => {
                    if resubscribing_seen {
                        reports_after_resubscribe += 1;
                    }
                    println!("[{t:6.1}s] Report {:?} = {:?}", r.path, r.value);
                }
                SubscriptionEvent::Event(e) => {
                    println!("[{t:6.1}s] Event {e:?}");
                }
                SubscriptionEvent::Lagged { dropped } => {
                    println!("[{t:6.1}s] Lagged (dropped {dropped})");
                }
                // `SubscriptionEvent` is `#[non_exhaustive]`.
                other => println!("[{t:6.1}s] {other:?}"),
            },
        }
    }

    // ---- Verdict -----------------------------------------------------------
    println!("\n== verdict ==");
    println!(
        "  Established events: {} (ids {:#010X?})",
        established_ids.len(),
        established_ids
    );
    println!("  Resubscribing seen: {resubscribing_seen}");
    println!("  reports after resubscribe: {reports_after_resubscribe}");
    let survived = resubscribing_seen && established_ids.len() >= 2;
    if survived {
        println!("\nPASS — one handle observed Established -> Resubscribing -> Established across the reboot ✓");
    } else if established_ids.len() >= 2 {
        println!("\nPARTIAL — re-established but no Resubscribing control event was observed");
    } else {
        println!(
            "\nINCONCLUSIVE — no re-establishment seen (did the device reboot inside the window?)"
        );
    }
    sub.cancel().await.ok();
    Ok(())
}
