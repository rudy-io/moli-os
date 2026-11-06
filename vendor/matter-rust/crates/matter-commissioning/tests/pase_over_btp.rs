//! M9-C1 floor test: the REAL PASE handshake driven through two REAL
//! [`BtpSession`] engines at the degenerate segment size 20, so every SPAKE2+
//! message multi-fragments and the BTP engine's reassembly / window / ack
//! machinery is exercised end to end — hardware-free.
//!
//! This is the BTP-framing floor (contrast [`commission_ble_loopback`], the
//! plain-datagram routing floor). It wires the controller's `run_pase_with`
//! (under [`TransportReliability::TransportProvides`]) to an inline PASE
//! `PaseVerifier` device, but every byte between them passes through a
//! test-local [`BtpPipe`] `AsyncDatagram` shim: two `BtpSession`s (one
//! `Central`, one `Peripheral`) segment each Matter message into ≤ 20-byte BTP
//! fragments, reassemble them on the far side, and flow acks under window
//! pressure. It asserts:
//! - PASE completes: the controller registers keys and the session is flagged
//!   `transport_reliable`; the device's verifier derives matching keys;
//! - **every fragment on the wire is ≤ 20 bytes** (real segmentation happened);
//! - **every reassembled unsecured frame has the R-bit clear** (MRP off over
//!   the reliable transport, re-proven here through framing);
//! - **exactly one `send_to` per Matter message** on each side — no retransmit
//!   fired (BTP self-reliabilizes; the unsecured stop-and-wait is off).

// Uses the `driver`-gated `run_pase_with`. Without the feature the file is
// empty (CI runs `--all-features`; plain `cargo test` skips it cleanly).
#![cfg(feature = "driver")]
// Test-code carve-outs: unwrap/expect (CONTRIBUTING.md); the single end-to-end test
// is deliberately long (full handshake device script inline for readability).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use matter_ble::session::{BtpSession, Role};
use matter_commissioning::driver::{
    decode_unsecured, encode_unsecured, run_pase_with, AsyncDatagram, TransportReliability,
    STREAM_PEER,
};
use matter_crypto::pase::{PasePbkdfParams, PaseVerifier};
use matter_transport::{ExchangeFlags, ProtocolId, SessionManager};
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;

const SEGMENT_SIZE: u16 = 20;
const WINDOW: u8 = 6;

// BTP fragment header flag bits (chip BtpEngine.h; mirrored in matter-ble).
const FLAG_END: u8 = 0x04;
const FLAG_DATA: u8 = 0x07; // START | CONTINUE | END

// SecureChannel opcodes the device emits (the controller's opcodes are produced
// inside run_pase_with).
const OP_PBKDF_RESP: u8 = 0x21;
const OP_PAKE2: u8 = 0x23;
const OP_STATUS_REPORT: u8 = 0x40;

fn io_broken() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "btp pipe closed")
}

fn to_io<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// `SecureChannel` `StatusReport` body: general code, protocol id, protocol
/// code (all little-endian; spec §4.10.1.1).
fn status_report_body(general: u16, protocol_code: u16) -> Vec<u8> {
    let mut b = Vec::with_capacity(8);
    b.extend_from_slice(&general.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes()); // SECURE_CHANNEL
    b.extend_from_slice(&protocol_code.to_le_bytes());
    b
}

/// True when a fully reassembled unsecured (session-id 0) Matter frame has the
/// exchange RELIABLE (`R`) flag set. Over a BTP transport this MUST be false.
fn frame_is_reliable(frame: &[u8]) -> bool {
    let (_hdr, rest) = matter_transport::decode_header(frame).expect("message header");
    let (ph, _payload) = matter_transport::decode_protocol_header(rest).expect("protocol header");
    ph.exchange_flags.contains(ExchangeFlags::RELIABLE)
}

/// One end of an in-memory BTP link: a real [`BtpSession`] plus the two fragment
/// channels that model the C1 (write) and C2 (indication) GATT directions. The
/// in-memory "GATT" completes each write instantly, so `next_gatt_write` is
/// immediately followed by `gatt_write_completed`.
struct BtpPipe {
    session: AsyncMutex<BtpSession>,
    inbound: AsyncMutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    outbound: mpsc::UnboundedSender<Vec<u8>>,
    /// Every fragment length put on the wire from this end (assert all ≤ 20).
    frag_sizes: Arc<StdMutex<Vec<usize>>>,
    /// Count of whole Matter messages this end was asked to send (assert one
    /// per SPAKE2+ message — no retransmit).
    sends: Arc<AtomicUsize>,
}

impl BtpPipe {
    /// Emit any acknowledgement that is due immediately (window pressure sets
    /// `ack_due_at` to the indication time — a past instant — so the peer's
    /// blocked send can resume). A deferred (2.5 s) ack has a future deadline
    /// and is left to be piggybacked on the next outbound fragment; the 15 s
    /// ack-timeout deadline is likewise in the future during a live handshake.
    fn flush_due_acks(&self, s: &mut BtpSession) -> io::Result<()> {
        while let Some(due) = s.poll_timeout() {
            if due > Instant::now() {
                break;
            }
            match s.handle_timeout(Instant::now()).map_err(to_io)? {
                Some(pkt) => {
                    // A standalone ack is a C1 write like any data fragment: it
                    // latches the one-GATT-op-in-flight flag. The in-memory
                    // "GATT" completes instantly, so release the slot at once —
                    // otherwise this side can never send its next fragment.
                    s.gatt_write_completed(Instant::now());
                    self.frag_sizes.lock().unwrap().push(pkt.len());
                    self.outbound.send(pkt).map_err(|_| io_broken())?;
                }
                None => break,
            }
        }
        Ok(())
    }
}

impl AsyncDatagram for BtpPipe {
    async fn send_to(&self, buf: &[u8], _peer: SocketAddr) -> io::Result<()> {
        self.sends.fetch_add(1, Ordering::Relaxed);
        {
            let mut s = self.session.lock().await;
            s.queue_message(buf).map_err(to_io)?;
        }
        loop {
            // Push every fragment the remote window currently permits. The
            // terminal fragment of the message carries the END flag.
            {
                let mut s = self.session.lock().await;
                while let Some(frag) = s.next_gatt_write(Instant::now()) {
                    s.gatt_write_completed(Instant::now());
                    let is_end = frag[0] & FLAG_DATA != 0 && frag[0] & FLAG_END != 0;
                    self.frag_sizes.lock().unwrap().push(frag.len());
                    self.outbound.send(frag).map_err(|_| io_broken())?;
                    if is_end {
                        return Ok(());
                    }
                }
            }
            // The window closed mid-message: wait for the peer's ack to reopen
            // it. During our send phase the peer only ever sends acks (never
            // data), so an inbound frame here credits the window and never
            // completes an SDU.
            let frag = {
                let mut rx = self.inbound.lock().await;
                rx.recv().await.ok_or_else(io_broken)?
            };
            let mut s = self.session.lock().await;
            let sdu = s.on_indication(&frag, Instant::now()).map_err(to_io)?;
            debug_assert!(sdu.is_none(), "unexpected SDU on the wire during send");
        }
    }

    async fn recv_from(&self) -> io::Result<(Vec<u8>, SocketAddr)> {
        loop {
            let frag = {
                let mut rx = self.inbound.lock().await;
                rx.recv().await.ok_or_else(io_broken)?
            };
            let mut s = self.session.lock().await;
            let maybe = s.on_indication(&frag, Instant::now()).map_err(to_io)?;
            // Flush any immediately-due ack back so the peer's blocked send can
            // resume before we return / await the next fragment.
            self.flush_due_acks(&mut s)?;
            if let Some(sdu) = maybe {
                return Ok((sdu, STREAM_PEER));
            }
        }
    }
}

/// Build a wired `Central`/`Peripheral` pipe pair sharing two fragment channels,
/// plus the shared instrumentation each side records into.
struct Wired {
    central: BtpPipe,
    peripheral: BtpPipe,
    frag_sizes: Arc<StdMutex<Vec<usize>>>,
    central_sends: Arc<AtomicUsize>,
    peripheral_sends: Arc<AtomicUsize>,
}

fn wired_pair() -> Wired {
    let now = Instant::now();
    let (c2p_tx, c2p_rx) = mpsc::unbounded_channel();
    let (p2c_tx, p2c_rx) = mpsc::unbounded_channel();
    let frag_sizes = Arc::new(StdMutex::new(Vec::new()));
    let central_sends = Arc::new(AtomicUsize::new(0));
    let peripheral_sends = Arc::new(AtomicUsize::new(0));

    let central = BtpPipe {
        session: AsyncMutex::new(BtpSession::new(Role::Central, SEGMENT_SIZE, WINDOW, now)),
        inbound: AsyncMutex::new(p2c_rx),
        outbound: c2p_tx,
        frag_sizes: Arc::clone(&frag_sizes),
        sends: Arc::clone(&central_sends),
    };
    let peripheral = BtpPipe {
        session: AsyncMutex::new(BtpSession::new(Role::Peripheral, SEGMENT_SIZE, WINDOW, now)),
        inbound: AsyncMutex::new(c2p_rx),
        outbound: p2c_tx,
        frag_sizes: Arc::clone(&frag_sizes),
        sends: Arc::clone(&peripheral_sends),
    };
    Wired {
        central,
        peripheral,
        frag_sizes,
        central_sends,
        peripheral_sends,
    }
}

#[tokio::test]
async fn pase_completes_over_real_btp_framing_at_segment_20() {
    let pin = 20_202_021;
    let params = PasePbkdfParams {
        iterations: 1000,
        salt: vec![0x55; 16],
    };
    let wired = wired_pair();
    let mut sessions = SessionManager::new();

    // Device: an inline PaseVerifier speaking through the peripheral pipe,
    // modelled on driver/pase.rs's TransportProvides test. Under a reliable
    // transport it does NOT wait for its closing StatusReport to be acked.
    let dev = &wired.peripheral;
    let device = async {
        let mut verifier = PaseVerifier::new_from_pin(pin, params, 0x00BB).unwrap();
        let mut ctr: u32 = 100;

        let (p, _) = dev.recv_from().await.unwrap();
        assert!(!frame_is_reliable(&p), "PBKDFParamRequest must not set R");
        let m = decode_unsecured(&p).unwrap();
        verifier.handle_pbkdf_request(&m.payload).unwrap();
        let resp = verifier.next_message().unwrap();
        dev.send_to(
            &encode_unsecured(
                ctr,
                m.exchange_id,
                OP_PBKDF_RESP,
                ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &resp,
            ),
            STREAM_PEER,
        )
        .await
        .unwrap();
        ctr += 1;

        let (p, _) = dev.recv_from().await.unwrap();
        assert!(!frame_is_reliable(&p), "Pake1 must not set R");
        let m = decode_unsecured(&p).unwrap();
        verifier.handle_pake1(&m.payload).unwrap();
        let pake2 = verifier.next_message().unwrap();
        dev.send_to(
            &encode_unsecured(
                ctr,
                m.exchange_id,
                OP_PAKE2,
                ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &pake2,
            ),
            STREAM_PEER,
        )
        .await
        .unwrap();
        ctr += 1;

        let (p, _) = dev.recv_from().await.unwrap();
        assert!(!frame_is_reliable(&p), "Pake3 must not set R");
        let m = decode_unsecured(&p).unwrap();
        verifier.handle_pake3(&m.payload).unwrap();
        dev.send_to(
            &encode_unsecured(
                ctr,
                m.exchange_id,
                OP_STATUS_REPORT,
                ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &status_report_body(0, 0),
            ),
            STREAM_PEER,
        )
        .await
        .unwrap();

        verifier.finish().unwrap()
    };

    let controller = run_pase_with(
        &wired.central,
        &mut sessions,
        STREAM_PEER,
        pin,
        TransportReliability::TransportProvides,
    );

    let (ctrl_result, dev_keys) = tokio::join!(controller, device);
    let sid = ctrl_result.expect("PASE over BTP must establish a session");

    // PASE completed with matching keys on both sides.
    let registered = sessions.get(sid).unwrap();
    assert_eq!(
        registered.keys,
        matter_transport::SessionKeys::from(dev_keys)
    );
    assert_eq!(registered.peer_id, matter_transport::SessionId(0x00BB));
    // The reliable transport must have flagged the session MRP-off.
    assert_eq!(sessions.is_transport_reliable(sid), Some(true));

    // Real segmentation happened and every fragment fit the negotiated size.
    let sizes = wired.frag_sizes.lock().unwrap();
    assert!(
        sizes.iter().all(|&n| n <= SEGMENT_SIZE as usize),
        "every BTP fragment must be <= {SEGMENT_SIZE} bytes; saw {sizes:?}"
    );
    assert!(
        sizes.contains(&(SEGMENT_SIZE as usize)),
        "at least one full-size fragment expected — messages should multi-fragment at segment 20"
    );

    // Exactly one send per SPAKE2+ message on each side: no retransmit fired.
    // Controller: PBKDFParamRequest, Pake1, Pake3. Device: PBKDFParamResponse,
    // Pake2, StatusReport.
    assert_eq!(
        wired.central_sends.load(Ordering::Relaxed),
        3,
        "controller sent each of its 3 handshake messages exactly once"
    );
    assert_eq!(
        wired.peripheral_sends.load(Ordering::Relaxed),
        3,
        "device sent each of its 3 handshake messages exactly once"
    );
}
