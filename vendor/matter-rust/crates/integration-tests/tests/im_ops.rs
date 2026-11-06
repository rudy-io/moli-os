// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use std::time::Duration;

use matter_codec::Tag;
use matter_controller::{
    AttributePath, CommandPath, EventPath, EventReport, ImStatus, InvokeResult, ReadPath,
    SubscriptionEvent, Value,
};

// ── 1. read ───────────────────────────────────────────────────────────────────

/// Read BasicInformation::VendorName (ep0, 0x0028, 0x0001) — asserts a
/// non-empty attribute report list.
#[tokio::test]
async fn read_basic_information_vendor_name() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let reports = node
        .read(&[ReadPath::concrete(0, 0x0028, 0x0001)])
        .await
        .expect("read VendorName");

    assert!(
        !reports.is_empty(),
        "expected at least one attribute report for VendorName"
    );
}

// ── 2. write + read-back ──────────────────────────────────────────────────────

/// Write BasicInformation::NodeLabel (ep0, 0x0028, 0x0005), then read it back
/// and verify the round-trip value.
#[tokio::test]
async fn write_and_read_back_node_label() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let write_path = AttributePath {
        endpoint: 0,
        cluster: 0x0028,
        attribute: 0x0005,
    };
    let statuses = node
        .write(&[(write_path, Value::Utf8("integ".into()))])
        .await
        .expect("write NodeLabel");

    for (_, status) in &statuses {
        assert!(
            matches!(status, ImStatus::Success),
            "expected Success status, got {status:?}"
        );
    }

    let reports = node
        .read(&[ReadPath::concrete(0, 0x0028, 0x0005)])
        .await
        .expect("read NodeLabel back");

    let found = reports
        .iter()
        .any(|(_, v)| matches!(v, Value::Utf8(s) if s == "integ"));
    assert!(
        found,
        "read-back did not return Value::Utf8(\"integ\"); got: {reports:?}"
    );
}

// ── 3. invoke ─────────────────────────────────────────────────────────────────

/// Invoke Identify.Identify (ep1, 0x0003, 0x00) with IdentifyTime=1 and
/// assert a success result.
#[tokio::test]
async fn invoke_identify() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let result = node
        .invoke(
            CommandPath {
                endpoint: 1,
                cluster: 0x0003,
                command: 0x00,
            },
            Value::Structure(vec![(Tag::Context(0), Value::Uint(1))]),
        )
        .await
        .expect("invoke Identify");

    match result {
        // Identify returns a bare success status; Data is also valid (some devices echo).
        InvokeResult::Status(ImStatus::Success) | InvokeResult::Data { .. } => {}
        other => panic!("expected Identify success, got {other:?}"),
    }
}

// ── 4. subscribe ──────────────────────────────────────────────────────────────

/// Subscribe to OnOff (ep1, 0x0006, attr 0x0000), drain priming reports, then
/// toggle the attribute via invoke and assert a matching Report arrives.
#[tokio::test]
async fn subscribe_onoff_attribute() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let mut sub = node
        .subscribe(&[ReadPath::concrete(1, 0x0006, 0x0000)], &[], 0, 10)
        .await
        .expect("subscribe OnOff");

    // Drain priming reports (and the Established event) before toggling.
    // Wait up to 5 s for an Established so we know the subscription is live.
    let drain_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = drain_deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, sub.next()).await {
            // Timeout or Established both signal the end of priming.
            Err(_) | Ok(Some(SubscriptionEvent::Established { .. })) => break,
            Ok(None) => panic!("subscription ended unexpectedly during priming"),
            Ok(Some(_)) => {} // priming Report / Lagged / Resubscribing / etc.
        }
    }

    // Toggle OnOff so the subscribed attribute changes.
    node.invoke(
        CommandPath {
            endpoint: 1,
            cluster: 0x0006,
            command: 0x02,
        },
        Value::Structure(vec![]),
    )
    .await
    .expect("invoke OnOff Toggle");

    // Wait up to 15 s for a Report on the OnOff attribute.
    let deadline = Duration::from_secs(15);
    #[allow(clippy::match_wild_err_arm)]
    loop {
        match tokio::time::timeout(deadline, sub.next()).await {
            Err(_elapsed) => panic!("timeout: no OnOff Report arrived within 15 s"),
            Ok(None) => panic!("subscription ended without a Report"),
            Ok(Some(SubscriptionEvent::Report(report))) => {
                if report.path.cluster == 0x0006 && report.path.attribute == 0x0000 {
                    // Matched — OnOff changed.
                    break;
                }
            }
            // Event / Lagged / Established / Resubscribing / future variants
            Ok(Some(_)) => {}
        }
    }

    // End the subscription with this test through the public cancel API,
    // instead of leaving it to `Drop`'s best-effort cancel. Cancelling is
    // controller-side: the controller forgets the subscription and answers
    // any later report for it with InvalidSubscription.
    sub.cancel().await.expect("cancel subscription");
}

// ── 5. events ─────────────────────────────────────────────────────────────────

/// Read BasicInformation::StartUp event (ep0, 0x0028, event 0x00) and assert
/// at least one EventReport::Data(_) is returned (all-clusters-app emits it
/// at boot).
#[tokio::test]
async fn read_startup_event() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let events = node
        .read_events(&[EventPath::concrete(0, 0x0028, 0x00)], &[])
        .await
        .expect("read_events StartUp");

    let has_data = events.iter().any(|e| matches!(e, EventReport::Data(_)));
    assert!(
        has_data,
        "expected at least one EventReport::Data for StartUp; got: {events:?}"
    );
}

// ── 6. timed ─────────────────────────────────────────────────────────────────

/// Invoke Identify.Identify via `invoke_timed` (TimedRequest handshake) and
/// assert success — exercises the timed-write path against the live device.
#[tokio::test]
async fn invoke_timed_identify() {
    let cfg = integration_tests::dut_or_skip!();
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    let result = node
        .invoke_timed(
            CommandPath {
                endpoint: 1,
                cluster: 0x0003,
                command: 0x00,
            },
            Value::Structure(vec![(Tag::Context(0), Value::Uint(1))]),
            Some(3000),
        )
        .await
        .expect("invoke_timed Identify");

    match result {
        // Identify returns a bare success status; Data is also acceptable.
        InvokeResult::Status(ImStatus::Success) | InvokeResult::Data { .. } => {}
        other => panic!("expected timed Identify success, got {other:?}"),
    }
}
