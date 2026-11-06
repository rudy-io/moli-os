// Integration tests are a binary crate; crate-level docs are not required.
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

//! MRP on a live chip device: every response is ACKed when its exchange
//! closes, as chip's `ExchangeContext::DoClose` → `FlushAcks` does, so
//! neither commissioning nor a controller dropped the instant a read returns
//! leaves the device anything to retransmit.
//!
//! The device's own log is the witness. chip logs:
//!
//! - `??N [E:<exchange> S:<lsid> M:<counter>] (…) Msg Retransmission to …
//!   scheduled for …ms` (Progress) each time it schedules a transmission of
//!   a reliable message: `??1` with the first send, then `??2`…`??5` after
//!   each re-send (src/messaging/ReliableMessageMgr.cpp:590 at master
//!   5cd2917a, :581 at v1.4.2.0);
//! - `<<N [E:… S:<lsid> M:…] (S) Msg Retransmission to …` (Progress) only
//!   when it actually re-sends (:224), and `<<N … failure (max retries:4)`
//!   (Error) when it gives up (:193), at both refs.
//!
//! The line prefix differs by backend: chip master prints
//! `[ts] [pid:tid] [EM] <<1 …` on Linux and `[ts] [pid:tid:queue] [EM] <<1 …`
//! on Darwin, each line wrapped in ANSI colour codes; v1.4.2.0 on Linux
//! prints `[ts][pid:tid] CHIP:EM: <<1 …` (src/platform/Linux/Logging.cpp:46).
//! So the matchers look for the markers anywhere in the line and ignore the
//! prefix and the colour codes. Sessions are named by the LSID chip logs at
//! Detail level when it activates one, `New secure session activated for
//! device <node, fabric>, LSID:<n> PSID:<m>!`
//! (src/protocols/secure_channel/PairingSession.cpp:56 master, :55
//! v1.4.2.0). Each test also counts `??N` lines on its sessions (the
//! commissioning test on each of its two sessions), so a log format or level
//! change, or a test watching the wrong session, fails loudly instead of
//! passing vacuously.
//!
//! Both counts take only exchanges the device is responding on. chip logs
//! the exchange as `E:<id><r|i>`: `ChipLogValueExchange` appends `i` when
//! `ExchangeContext::IsInitiator()` and `r` otherwise
//! (src/lib/support/logging/TextOnlyLogging.h:299-309 at master 5cd2917a,
//! :278-288 at v1.4.2.0), and every `??N`/`<<N` line above formats its
//! exchange with it. Every message these tests ACK is a response, so it sits
//! on an `r` exchange. A device-initiated (`i`) exchange on a test's session
//! is not ours to ACK: chip's ReadHandler keeps its session in a plain
//! `SessionHolder` (src/app/ReadHandler.h:624 master, :604 v1.4.2.0), and
//! each new CASE session shifts every holder on an older CASE session with
//! the same peer node and CATs to the new one
//! (`SecureSession::Activate` → `SecureSessionTable::NewerSessionAvailable`
//! → `SessionHolder::ShiftToSession`: src/transport/SecureSession.cpp:55,
//! src/transport/SecureSessionTable.h:100-116, src/transport/Session.h:129,
//! at both refs; only `ExchangeContext` and `PairingSession` opt out with
//! `kStayAtOldSession`). Each subscription report opens a fresh exchange on
//! that holder's session (`ReadHandler::AcquireExchangeForSend`,
//! src/app/ReadHandler.cpp:285-312 master; `SendReportData`, :306-325
//! v1.4.2.0). Every test binary's controller is node 1 on the same fabric,
//! so a subscription an earlier binary left behind follows each of these
//! tests' sessions in turn, and its next report goes out, unACKed, on
//! whichever one is newest when it comes due. Nightly 38265577415 (v1.4.2.0)
//! failed exactly so: the subscription `im_ops`'s `subscribe_onoff_attribute`
//! made on S:26722 (max interval 10 s) sent its next report, 10 s after the
//! last, as `E:25315i` on S:26728, one of the drop-after-read test's 40
//! sessions, 1.2 s after that session's own read (`E:38184r`, ACKed), and
//! retransmitted it `<<1`..`<<5`. Those reports answer none of our
//! requests, so they are out of these tests' scope.
//!
//! Runs in the all-clusters arm (every binary no other arm lists): locally
//! (`just integration`, chip master) and in the nightly at v1.4.2.0.

use std::time::Duration;

use integration_tests::dut::DutConfig;
use matter_controller::ReadPath;

/// Short-lived controllers in a row. The test detects retransmissions, not
/// buffer exhaustion: all-clusters allocates packet buffers from the heap
/// (`config/standalone/CHIPProjectConfig.h` sets
/// `CHIP_SYSTEM_CONFIG_PACKETBUFFER_POOL_SIZE` 0, :60-61 at master 5cd2917a,
/// :65-66 at v1.4.2.0). Only tv-app at master has just 15 packet buffers
/// (`examples/tv-app/tv-common/include/CHIPProjectAppConfig.h`'s `#ifndef
/// CHIP_DEVICE_LAYER_TARGET_DARWIN` leaves it on the default); 40 is well
/// past that.
const CYCLES: usize = 40;

/// How long to watch after the last message. A chip device's first
/// retransmit comes about 330-550 ms after the original (our advertised
/// 300/500 ms active/idle intervals plus chip's margin; app.log's "scheduled
/// for" lines), so 2 s covers at least two retransmit rounds.
const RETRANSMIT_WATCH: Duration = Duration::from_secs(2);

/// The device's log, written by `xtask integration`
/// (`xtask/src/integration.rs`, `log_path = dut_dir.join("app.log")`), fresh
/// for every run.
fn app_log(cfg: &DutConfig) -> std::path::PathBuf {
    cfg.dut_dir.join("app.log")
}

/// Whether `marker` occurs in `line` immediately followed by an ASCII digit.
fn marker_then_digit(line: &str, marker: &str) -> bool {
    line.match_indices(marker)
        .any(|(i, _)| line[i + marker.len()..].starts_with(|c: char| c.is_ascii_digit()))
}

/// chip re-sent (or gave up on) a reliable message: `<<N … Msg Retransmission
/// to …`, without "scheduled for", whatever the line prefix.
fn is_retransmission(line: &str) -> bool {
    line.contains("Msg Retransmission to")
        && !line.contains("scheduled for")
        && marker_then_digit(line, "<<")
}

/// chip scheduled a transmission of a reliable message: `??N … Msg
/// Retransmission to … scheduled for …`, logged with the first send (`??1`)
/// and again after each re-send (`??2`…): at least one line per message chip
/// sent reliably, which is all the positive controls rely on.
fn is_reliable_send(line: &str) -> bool {
    line.contains("Msg Retransmission to")
        && line.contains("scheduled for")
        && marker_then_digit(line, "??")
}

/// The session (LSID) a chip MRP log line names: the number after ` S:`.
fn lsid_of(line: &str) -> Option<&str> {
    line.split(" S:").nth(1)?.split_whitespace().next()
}

/// `(is the PASE peer, LSID)` of every session activation in `log`, in order.
/// A PASE session's peer is the temporary node id `FFFFFFFB00000000` on
/// fabric index 0.
fn activations(log: &str) -> Vec<(bool, String)> {
    log.lines()
        .filter(|l| l.contains("New secure session activated"))
        .filter_map(|l| {
            let lsid = l.split("LSID:").nth(1)?.split_whitespace().next()?;
            Some((l.contains("<FFFFFFFB00000000, 0>"), lsid.to_string()))
        })
        .collect()
}

/// Whether a chip MRP log line names an exchange the device is responding
/// on: the id after `[E:` is followed by `r`, not `i` (see the module docs).
fn on_responder_exchange(line: &str) -> bool {
    line.split("[E:").nth(1).is_some_and(|rest| {
        let after_id = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        after_id.len() < rest.len() && after_id.starts_with('r')
    })
}

/// `(reliable sends, retransmission lines)` on the sessions `lsids`, on
/// exchanges the device is responding on.
fn mrp_lines_on<'a>(log: &'a str, lsids: &[String]) -> (usize, Vec<&'a str>) {
    let on = |l: &&str| {
        on_responder_exchange(l) && lsid_of(l).is_some_and(|s| lsids.iter().any(|x| x == s))
    };
    let sends = log
        .lines()
        .filter(|l| is_reliable_send(l))
        .filter(on)
        .count();
    let retransmitted = log
        .lines()
        .filter(|l| is_retransmission(l))
        .filter(on)
        .collect();
    (sends, retransmitted)
}

/// The run's commissioning: its PASE session (the first activation, for the
/// PASE peer) and the commissioner's own CASE session (the next activation)
/// must have left no retransmission. That CASE session carries only
/// CommissioningComplete, so a `<<N` line on it is that response unACKed;
/// one on the PASE session is a response whose ACK came too late (the Tapo
/// P110M straggler of M6.6.5, on chip). Reads the whole log: the fixture
/// commissions once per run, in whichever test binary runs first.
///
/// The responder-exchange filter applies here too, and costs nothing: every
/// message chip sends reliably during commissioning answers a commissioner
/// request (nightly 38265577415: ten `??1` sends on the PASE session, one on
/// the CASE session, all on `r` exchanges), so both positive controls and
/// the retransmission check still see every commissioning response. What it
/// drops is a report from a subscription that lands on the commissioning
/// CASE session (one the first binary's test makes over the commissioner's
/// controller, before a newer session takes it over), which is not a
/// commissioning response.
#[tokio::test]
async fn commissioning_leaves_no_retransmission() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: the commissioning ACK check runs against all-clusters");
        return;
    }
    // Commission now if no earlier binary did, then give the device time to
    // retransmit anything it is owed an ACK for.
    let (controller, _node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    drop(controller);
    tokio::time::sleep(RETRANSMIT_WATCH).await;

    let log = std::fs::read(app_log(&cfg)).expect("app.log");
    let log = String::from_utf8_lossy(&log);
    let all = activations(&log);
    let pase = all
        .iter()
        .position(|(is_pase, _)| *is_pase)
        .expect("app.log shows the PASE session activation");
    let commissioning: Vec<String> = all[pase..]
        .iter()
        .take(2)
        .map(|(_, lsid)| lsid.clone())
        .collect();
    assert_eq!(
        commissioning.len(),
        2,
        "PASE and the commissioner's CASE session: {all:?}"
    );
    // Positive control per session: the PASE session carries the
    // commissioning steps before CASE, and the commissioner's CASE session
    // carries CommissioningComplete, so each shows at least one `??N` send.
    // A test watching the wrong (or an empty) session fails here.
    for (role, lsid) in ["PASE", "CASE"].iter().zip(&commissioning) {
        let (sends, _) = mrp_lines_on(&log, std::slice::from_ref(lsid));
        assert!(
            sends >= 1,
            "positive control: chip's `??N` send lines on the {role} session {lsid} \
             (found {sends}; commissioning sessions {commissioning:?})"
        );
    }
    let (_, retransmitted) = mrp_lines_on(&log, &commissioning);
    assert_eq!(
        retransmitted,
        Vec::<&str>::new(),
        "the device retransmitted a commissioning response on {commissioning:?}"
    );
}

#[tokio::test]
async fn controllers_dropped_right_after_a_read_leave_no_retransmission() {
    let cfg = integration_tests::dut_or_skip!();
    if !cfg.is_app("all-clusters") {
        eprintln!("skipped: the drop-after-read check runs against all-clusters");
        return;
    }
    // Commission (if this is the run's first test) and drop: no CASE session
    // is opened until the first exchange.
    let (controller, _node_id) = integration_tests::fixture::connect(&cfg)
        .await
        .expect("connect/commission DUT");
    drop(controller);
    let start = std::fs::metadata(app_log(&cfg)).expect("app.log").len();

    for cycle in 0..CYCLES {
        let (controller, node_id) = integration_tests::fixture::connect(&cfg)
            .await
            .expect("connect DUT");
        let report = controller
            .node(node_id)
            .read(&[ReadPath::concrete(0, 0x0028, 0x0002)]) // BasicInformation.VendorID
            .await
            .unwrap_or_else(|e| panic!("cycle {cycle}: read failed: {e:?}"));
        assert_eq!(report.len(), 1, "cycle {cycle}: VendorID");
        drop(controller); // no pause: the ACK must already be on the wire
    }
    tokio::time::sleep(RETRANSMIT_WATCH).await;

    let log = std::fs::read(app_log(&cfg)).expect("app.log");
    let tail = String::from_utf8_lossy(&log[usize::try_from(start).unwrap()..]);
    let lsids: Vec<String> = activations(&tail).into_iter().map(|(_, l)| l).collect();
    assert_eq!(
        lsids.len(),
        CYCLES,
        "each cycle's controller opens its own CASE session; app.log shows {lsids:?}"
    );
    let (sends, retransmitted) = mrp_lines_on(&tail, &lsids);
    assert!(
        sends >= CYCLES,
        "positive control: one `??N` send line per cycle at least (found {sends})"
    );
    assert_eq!(
        retransmitted,
        Vec::<&str>::new(),
        "the device retransmitted responses on the test's sessions"
    );
}

/// The matcher over lines captured from nightly 38265577415's app.log (chip
/// v1.4.2.0, linux-x64, lines 38180-38557), colour codes included. On
/// S:26728, one of that run's 40 drop-after-read sessions, `E:38184r` is the
/// test's own read (ReportData, ACKed) and `E:25315i` is a subscription
/// report chip started itself and never got ACKed. Only the responder
/// exchange may count: the `??1` send on `E:38184r`, and the synthetic `<<1`
/// on it (made from the real `<<1` line by swapping the exchange).
#[test]
fn only_exchanges_the_device_responds_on_are_counted() {
    const RESPONSE_SEND: &str = "\x1b[0;32m[1791817321.232] [10080:10080] [EM] ??1 [E:38184r S:26728 M:133938514] (S) Msg Retransmission to 1:0000000000000001 scheduled for 395ms from now [State:Active II:500 AI:300 AT:4000]\x1b[0m";
    const RESPONSE_RETRANSMIT: &str = "\x1b[0;32m[1791817322.778] [10080:10080] [EM] <<1 [E:38184r S:26728 M:133938514] (S) Msg Retransmission to 1:0000000000000001\x1b[0m";
    const STALE_REPORT: [&str; 10] = [
        "\x1b[0;32m[1791817322.429] [10080:10080] [EM] ??1 [E:25315i S:26728 M:133938515] (S) Msg Retransmission to 1:0000000000000001 scheduled for 349ms from now [State:Active II:500 AI:300 AT:4000]\x1b[0m",
        "\x1b[0;32m[1791817322.778] [10080:10080] [EM] <<1 [E:25315i S:26728 M:133938515] (S) Msg Retransmission to 1:0000000000000001\x1b[0m",
        "\x1b[0;32m[1791817322.778] [10080:10080] [EM] ??2 [E:25315i S:26728 M:133938515] (S) Msg Retransmission to 1:0000000000000001 scheduled for 385ms from now [State:Active II:500 AI:300 AT:4000]\x1b[0m",
        "\x1b[0;32m[1791817323.164] [10080:10080] [EM] <<2 [E:25315i S:26728 M:133938515] (S) Msg Retransmission to 1:0000000000000001\x1b[0m",
        "\x1b[0;32m[1791817323.164] [10080:10080] [EM] ??3 [E:25315i S:26728 M:133938515] (S) Msg Retransmission to 1:0000000000000001 scheduled for 565ms from now [State:Active II:500 AI:300 AT:4000]\x1b[0m",
        "\x1b[0;32m[1791817323.729] [10080:10080] [EM] <<3 [E:25315i S:26728 M:133938515] (S) Msg Retransmission to 1:0000000000000001\x1b[0m",
        "\x1b[0;32m[1791817323.729] [10080:10080] [EM] ??4 [E:25315i S:26728 M:133938515] (S) Msg Retransmission to 1:0000000000000001 scheduled for 1038ms from now [State:Active II:500 AI:300 AT:4000]\x1b[0m",
        "\x1b[0;32m[1791817324.769] [10080:10080] [EM] <<4 [E:25315i S:26728 M:133938515] (S) Msg Retransmission to 1:0000000000000001\x1b[0m",
        "\x1b[0;32m[1791817324.769] [10080:10080] [EM] ??5 [E:25315i S:26728 M:133938515] (S) Msg Retransmission to 1:0000000000000001 scheduled for 1428ms from now [State:Active II:500 AI:300 AT:4000]\x1b[0m",
        "\x1b[1;31m[1791817326.197] [10080:10080] [EM] <<5 [E:25315i S:26728 M:133938515] (S) Msg Retransmission to 1:0000000000000001 failure (max retries:4)\x1b[0m",
    ];
    let log = [RESPONSE_SEND, RESPONSE_RETRANSMIT]
        .into_iter()
        .chain(STALE_REPORT)
        .collect::<Vec<_>>()
        .join("\n");
    let lsids = ["26728".to_string()];
    // The lines are the right shape: without the role filter every one of
    // them is a send or a retransmission on S:26728.
    assert_eq!(log.lines().filter(|l| is_reliable_send(l)).count(), 6);
    assert_eq!(log.lines().filter(|l| is_retransmission(l)).count(), 6);

    let (sends, retransmitted) = mrp_lines_on(&log, &lsids);
    assert_eq!(sends, 1, "only the `??1` on E:38184r");
    assert_eq!(retransmitted, vec![RESPONSE_RETRANSMIT]);
}
