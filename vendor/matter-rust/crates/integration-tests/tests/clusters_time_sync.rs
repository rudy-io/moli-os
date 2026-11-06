// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use matter_controller::{DstOffsetEntry, TimeGranularity, TimeZoneEntry};

/// TimeSynchronization (ep0, 0x0038) against the live DUT (G-a):
///   - SetUTCTime, then read UTCTime back and assert the clock is set;
///   - SetTimeZone (observe the returned DSTOffsetRequired);
///   - SetDSTOffset.
#[tokio::test]
async fn time_sync_set_and_read() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // A fixed, plausible epoch-µs value (well after the Matter 2000 epoch;
    // ~2024-09 expressed in microseconds since 2000-01-01 UTC).
    let when_us: u64 = 780_000_000_000_000;
    // SetUTCTime acceptance is DEVICE-DISCRETIONARY: per spec §11.16 a node with
    // an equal-or-finer-granularity time source SHALL reject it, and
    // all-clusters-app running on a host with a synced clock does exactly that
    // (a clean `SetUTCTime rejected` IM status). Accept either outcome — the
    // point of the live test is that the command round-trips against the device;
    // only a malformed/transport failure is a real error.
    let accepted = match node.set_utc_time(when_us, TimeGranularity::Seconds).await {
        Ok(()) => true,
        Err(e) => {
            assert!(
                e.to_string().contains("SetUTCTime rejected"),
                "SetUTCTime failed unexpectedly (not a clean device rejection): {e}"
            );
            false
        }
    };

    // Read UTCTime back — this must round-trip regardless. If the device
    // accepted our value the clock must reflect it (allowing drift/advance);
    // otherwise the device reports its own time source (may be null if unset).
    let read = node.read_utc_time().await.expect("read UTCTime");
    if accepted {
        let t = read.expect("UTCTime should be set after an accepted SetUTCTime");
        assert!(
            t + 60_000_000 >= when_us,
            "clock {t} is far behind the value we set ({when_us})"
        );
    }

    // Time zone + DST offset (single entries valid from epoch 0).
    let _dst_required = node
        .set_time_zone(&[TimeZoneEntry::new(3600, 0, None)])
        .await
        .expect("SetTimeZone");
    node.set_dst_offset(&[DstOffsetEntry::new(3600, 0, None)])
        .await
        .expect("SetDSTOffset");
}
