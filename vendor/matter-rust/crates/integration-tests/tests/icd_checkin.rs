// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use matter_controller::IcdClientType;

/// ICD (Intermittently Connected Device) full client flow vs lit-icd-app:
/// commission → RegisterClient (persists a 16-byte key) → advertise + listen →
/// receive the device's periodic Check-In, decrypt, and verify the counter is
/// above the registration floor. The DUT is launched (`just integration-icd`)
/// with short ICD timers so the first Check-In arrives within seconds.
#[tokio::test]
async fn icd_register_and_receive_checkin() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("icd") {
        eprintln!("skipped: ICD test needs the lit-icd-app DUT (`just integration-icd`)");
        return;
    }
    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    let node = controller.node(node_id);

    // Register as a check-in client. MonitoredSubject = the device node id (as
    // the icd_register_listen example does); the value does not affect the
    // Check-In crypto.
    let reg = node
        .register_icd_client(node_id, IcdClientType::Permanent)
        .await
        .expect("register_icd_client");
    eprintln!("[icd] registered; start counter {}", reg.start_counter);

    // Wait (bounded) for the device's unsolicited Check-In.
    let checkin = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        controller.listen_for_checkin_once(5580),
    )
    .await
    .expect("no Check-In within 45s")
    .expect("listen_for_checkin_once");

    assert_eq!(checkin.node_id, node_id, "Check-In from unexpected node");
    assert!(
        checkin.counter > reg.start_counter,
        "Check-In counter {} not above registration floor {}",
        checkin.counter,
        reg.start_counter
    );
    eprintln!("[icd] Check-In OK: counter {}", checkin.counter);

    // Ask it to stay active (nice-to-have; non-fatal).
    match node.stay_active_request(30_000).await {
        Ok(ms) => eprintln!("[icd] stay-active promised {ms} ms"),
        Err(e) => eprintln!("[icd] stay_active_request failed (non-fatal): {e}"),
    }
}
