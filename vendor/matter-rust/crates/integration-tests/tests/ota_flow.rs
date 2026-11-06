// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

/// OTA provider end-to-end vs chip's ota-requestor-app: commission the
/// requestor, then `serve_ota` — announce ourselves as its OTA provider,
/// accept its inbound CASE session (the requestor resumes the announce
/// session), answer `QueryImage` with the generated `.ota` image, stream it
/// over BDX, and return once the requestor reboots into the new image and
/// sends `NotifyUpdateApplied` over a fresh post-reboot CASE session.
///
/// The DUT is launched by `just integration-ota`, which also generates the
/// image via chip's `ota_image_tool.py` (vendor 0xFFF1 / product 0x8000 /
/// version 2) and hands its path over in `MATTER_INTEGRATION_OTA_IMAGE`.
#[tokio::test]
async fn serve_ota_to_requestor() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("ota") {
        eprintln!("skipped: OTA test needs the ota-requestor-app DUT (`just integration-ota`)");
        return;
    }
    let image_path = std::env::var("MATTER_INTEGRATION_OTA_IMAGE")
        .expect("MATTER_INTEGRATION_OTA_IMAGE not set — run via `just integration-ota`");
    let image = std::fs::read(&image_path).expect("read generated .ota image");

    let (controller, node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    eprintln!(
        "[ota] commissioned node {node_id}; announcing + serving {}-byte image",
        image.len()
    );

    // Commissioning must have closed its own CASE session on the requestor,
    // or the requestor's retry below can pick it.
    assert_commissioning_closed_its_case_session(&cfg).await;

    // Offered SoftwareVersion 2 matches the `.ota` header (`-vn 2`) and
    // exceeds the requestor's running version (1), so it downloads + applies.
    // Budget: the requestor's first QueryImage goes to the controller's own
    // operational session (opened by commission()'s post-commission read and
    // reused by the announce), which ACKs it but has no OTA-provider handler;
    // chip gives up on it after about 10 s and marks it defunct. Its one retry
    // (kMaxInvalidSessionRetries = 1) opens a fresh CASE session to the
    // provider port only if it holds no other active session to the provider
    // node id. The commissioning session would be one, which is why
    // commissioning closes it (checked above); with a second stale session,
    // chip falls back to its 24 h periodic query and no budget helps.
    // Commissioning latency varies run-to-run and the BDX transfer (a
    // few-hundred-byte image) is
    // sub-second, so 180 s absorbs the observed worst case with headroom.
    tokio::time::timeout(
        std::time::Duration::from_secs(180),
        controller.serve_ota(
            node_id, image, /* software_version */ 2, /* port */ 5580,
        ),
    )
    .await
    .expect("OTA flow did not complete within 180s")
    .expect("serve_ota");
    eprintln!("[ota] serve_ota completed — requestor rebooted into the image and sent NotifyUpdateApplied");
}

/// The requestor's log shows it received our CloseSession on the
/// commissioner's CASE session, after our ACK of the CommissioningComplete
/// response, and evicted that session:
///
/// - `New secure session activated for device <node, fabric>, LSID:<n>
///   PSID:<m>!` (Detail; src/protocols/secure_channel/PairingSession.cpp:56
///   at master 5cd2917a, :55 at v1.4.2.0) names each session. The
///   commissioner's CASE session is the first activation after the PASE one
///   (peer `<FFFFFFFB00000000, 0>`).
/// - `>>> [E:… S:<n> …] … StandaloneAck` is our ACK arriving on it.
/// - `Received CloseSession status message, closing session` (Progress;
///   src/protocols/secure_channel/UnsolicitedStatusHandler.cpp:54, both
///   refs), followed by
/// - `SecureSession[…]: MarkForEviction Type:2 LSID:<n>` (Detail;
///   src/transport/SecureSession.cpp:132, both refs; type 2 is CASE).
///
/// Polls for up to 2 s. The CloseSession leaves before `commission()`
/// returns and chip handles it on receipt.
async fn assert_commissioning_closed_its_case_session(cfg: &integration_tests::dut::DutConfig) {
    let path = cfg.dut_dir.join("app.log");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let log = String::from_utf8_lossy(&std::fs::read(&path).expect("app.log")).into_owned();
        let verdict = commissioning_case_lsid(&log)
            .ok_or_else(|| "no commissioning CASE session activation in app.log".to_string())
            .and_then(|lsid| closed_after_ack(&log, &lsid));
        match verdict {
            Ok(()) => return,
            Err(e) if std::time::Instant::now() >= deadline => {
                panic!("commissioning did not close its CASE session on the requestor: {e}")
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
}

/// The digits after `LSID:` in a chip log line.
fn lsid_token(line: &str) -> Option<&str> {
    let rest = line.split("LSID:").nth(1)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// LSID of the first session activated after the PASE session.
fn commissioning_case_lsid(log: &str) -> Option<String> {
    let mut activations = log
        .lines()
        .filter(|l| l.contains("New secure session activated"));
    activations
        .by_ref()
        .find(|l| l.contains("<FFFFFFFB00000000, 0>"))?;
    activations.next().and_then(lsid_token).map(str::to_string)
}

/// Our ACK arrived on `lsid`, then chip received a CloseSession and the next
/// session it evicted was the CASE session `lsid`.
fn closed_after_ack(log: &str, lsid: &str) -> Result<(), String> {
    let lines: Vec<&str> = log.lines().collect();
    let close = lines
        .iter()
        .position(|l| l.contains("Received CloseSession status message, closing session"))
        .ok_or_else(|| "chip never logged a received CloseSession".to_string())?;
    let on_session = format!(" S:{lsid} ");
    if !lines[..close]
        .iter()
        .any(|l| l.contains(">>>") && l.contains(&on_session) && l.contains("StandaloneAck"))
    {
        return Err(format!(
            "no StandaloneAck from us on LSID {lsid} before the CloseSession"
        ));
    }
    let evicted = lines[close..]
        .iter()
        .find(|l| l.contains("MarkForEviction"))
        .ok_or_else(|| "no MarkForEviction after the CloseSession".to_string())?;
    if evicted.contains("Type:2") && lsid_token(evicted) == Some(lsid) {
        Ok(())
    } else {
        Err(format!("evicted the wrong session: {evicted}"))
    }
}
