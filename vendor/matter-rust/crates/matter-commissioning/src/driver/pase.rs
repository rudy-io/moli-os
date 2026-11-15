//! PASE bridge (M6.6.3b): drive the sans-IO [`PaseProver`] over the unsecured
//! datagram path and register the resulting secured session.
//!
//! PASE runs UNSECURED (session-id 0, `SecureChannel` protocol). The bridge
//! allocates a local session id, threads it into the prover (so the device is
//! told to address us by it), drives the 5-message handshake over
//! [`UnsecuredExchange`], then registers via
//! [`SessionManager::register_pase_with_local_id`].

use std::net::SocketAddr;

use matter_crypto::pase::PaseProver;
use matter_transport::{MrpConfig, PeerHint, SessionId, SessionManager, SessionRole};

use crate::driver::datagram::AsyncDatagram;
use crate::driver::error::DriverError;
use crate::driver::unsecured::{
    parse_status_report, random_exchange_id, require_handshake_opcode, UnsecuredExchange,
};
use crate::driver::TransportReliability;

// SecureChannel opcodes for the PASE handshake (Matter Core Spec §4.14.1).
const OP_PBKDF_PARAM_REQUEST: u8 = 0x20;
const OP_PBKDF_PARAM_RESPONSE: u8 = 0x21;
const OP_PASE_PAKE1: u8 = 0x22;
const OP_PASE_PAKE2: u8 = 0x23;
const OP_PASE_PAKE3: u8 = 0x24;
/// `SecureChannel` `StatusReport` opcode (spec §4.10.1.1) — the frame the
/// device sends to close the handshake after the terminal `Pake3`.
const OP_STATUS_REPORT: u8 = 0x40;

/// Drive a full PASE handshake against `peer` and register the resulting
/// secured session in `sessions`, returning its local [`SessionId`].
///
/// # Errors
///
/// - [`DriverError::Crypto`] if a SPAKE2+ step fails (e.g. wrong passcode).
/// - [`DriverError::Io`] / [`DriverError::Transport`] / [`DriverError::Timeout`]
///   on datagram, framing, or reply-timeout failure.
/// - [`DriverError::Handshake`] if negotiation produced no responder session
///   id or the handshake did not close with a `StatusReport`.
/// - [`DriverError::SessionEstablishmentFailed`] if the device's closing
///   `StatusReport` reports failure.
pub async fn run_pase<T: AsyncDatagram>(
    transport: &T,
    sessions: &mut SessionManager,
    peer: SocketAddr,
    passcode: u32,
) -> Result<SessionId, DriverError> {
    // Delegate with MRP — the historical UDP behavior, byte-for-byte unchanged
    // for existing callers.
    run_pase_with(
        transport,
        sessions,
        peer,
        passcode,
        TransportReliability::Mrp,
    )
    .await
}

/// Drive a full PASE handshake against `peer` with explicit
/// [`TransportReliability`] and register the resulting secured session in
/// `sessions`, returning its local [`SessionId`].
///
/// Under [`TransportReliability::TransportProvides`] the unsecured handshake
/// frames carry no R-flag, are sent exactly once (the transport is reliable),
/// and no standalone acks are emitted; on success the new session is marked
/// [`SessionManager::set_transport_reliable`] so the secured phase likewise
/// suppresses MRP (Matter spec §4.12: MRP off over BLE/BTP).
///
/// # Errors
///
/// - [`DriverError::Crypto`] if a SPAKE2+ step fails (e.g. wrong passcode).
/// - [`DriverError::Io`] / [`DriverError::Transport`] / [`DriverError::Timeout`]
///   on datagram, framing, or reply-timeout failure.
/// - [`DriverError::Handshake`] if negotiation produced no responder session
///   id or the handshake did not close with a `StatusReport`.
/// - [`DriverError::SessionEstablishmentFailed`] if the device's closing
///   `StatusReport` reports failure.
pub async fn run_pase_with<T: AsyncDatagram>(
    transport: &T,
    sessions: &mut SessionManager,
    peer: SocketAddr,
    passcode: u32,
    reliability: TransportReliability,
) -> Result<SessionId, DriverError> {
    let local = sessions.allocate_session_id();
    let mut prover = PaseProver::new_with_negotiation(passcode, local.0)?;
    // CSPRNG-seeded counter + ephemeral source node id (spec §4.6.1.1,
    // §4.13.2.1) — devices drop session-establishment frames without them.
    // PASE talks to a device in an open commissioning window, which is actively
    // advertising and therefore awake, so the spec defaults are the right size.
    // (The BLE path runs TransportReliability::TransportProvides and does not
    // retransmit at all -- MRP is off over BTP, spec §4.12.)
    let mut exch = UnsecuredExchange::new_ephemeral_with(
        random_exchange_id()?,
        reliability,
        MrpConfig::for_peer(None, None, None),
    )?;

    let request = prover.start()?;
    let resp = exch
        .send_and_recv(
            transport,
            peer,
            OP_PBKDF_PARAM_REQUEST,
            OP_PBKDF_PARAM_RESPONSE,
            &request,
            None,
        )
        .await?;
    if let Err(e) = require_handshake_opcode(&resp, OP_PBKDF_PARAM_RESPONSE) {
        // Best-effort ack so a rejecting device stops retransmitting its
        // (reliable) StatusReport before we abort.
        let _ = exch
            .send_standalone_ack(transport, peer, resp.message_counter)
            .await;
        return Err(e);
    }
    prover.handle_pbkdf_response(&resp.payload)?;

    let pake1 = prover.next_message()?;
    let pake2 = exch
        .send_and_recv(
            transport,
            peer,
            OP_PASE_PAKE1,
            OP_PASE_PAKE2,
            &pake1,
            Some(resp.message_counter),
        )
        .await?;
    if let Err(e) = require_handshake_opcode(&pake2, OP_PASE_PAKE2) {
        // A wrong passcode surfaces here: the device rejects Pake1 with a
        // StatusReport (InvalidParameter) instead of sending Pake2.
        let _ = exch
            .send_standalone_ack(transport, peer, pake2.message_counter)
            .await;
        return Err(e);
    }
    prover.handle_pake2(&pake2.payload)?;

    // Pake3 is sent reliably and the device closes the handshake with a
    // SecureChannel StatusReport (success or failure), which must be consumed
    // and acked here — otherwise its retransmits straggle into the secured IM
    // phase (observed: Tapo P110M, M6.6.5 validation).
    let pake3 = prover.next_message()?;
    let report = exch
        .send_and_recv(
            transport,
            peer,
            OP_PASE_PAKE3,
            OP_STATUS_REPORT,
            &pake3,
            Some(pake2.message_counter),
        )
        .await?;
    let status = parse_status_report(&report)?;
    exch.send_standalone_ack(transport, peer, report.message_counter)
        .await?;
    if !status.is_session_establishment_success() {
        return Err(DriverError::SessionEstablishmentFailed {
            general_code: status.general_code,
            protocol_code: status.protocol_code,
        });
    }

    // Capture the responder session id before `finish` consumes `prover`.
    let peer_session_id = prover.responder_session_id().ok_or(DriverError::Handshake(
        "PASE negotiation produced no responder session id",
    ))?;
    let keys = prover.finish()?;
    sessions.register_pase_with_local_id(
        local,
        keys,
        SessionRole::Initiator,
        peer_session_id,
        PeerHint::default(),
    );
    // On a reliable transport, flag the freshly registered session so the
    // secured phase suppresses MRP too (R-flag, retransmits, standalone acks).
    if reliability == TransportReliability::TransportProvides {
        sessions.set_transport_reliable(local, true)?;
    }
    Ok(local)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use matter_crypto::pase::{PasePbkdfParams, PaseVerifier};
    use matter_transport::{SessionKeys, SessionManager};

    use super::*;
    use crate::driver::datagram::{AsyncDatagram, InMemoryDatagram};
    use crate::driver::unsecured::{decode_unsecured, encode_unsecured};

    const OP_PBKDF_RESP: u8 = 0x21;
    const OP_PAKE2: u8 = 0x23;
    const OP_STANDALONE_ACK: u8 = 0x10;

    /// `SecureChannel` `StatusReport` body: general code, protocol id,
    /// protocol code (all little-endian; spec §4.10.1.1).
    fn status_report_body(general: u16, protocol_code: u16) -> Vec<u8> {
        let mut b = Vec::with_capacity(8);
        b.extend_from_slice(&general.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // SECURE_CHANNEL
        b.extend_from_slice(&protocol_code.to_le_bytes());
        b
    }

    #[tokio::test]
    async fn run_pase_establishes_matching_session() {
        let pin = 20_202_021;
        let params = PasePbkdfParams {
            iterations: 1000,
            salt: vec![0x55; 16],
        };

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut sessions = SessionManager::new();

        let device = async {
            let mut verifier = PaseVerifier::new_from_pin(pin, params, 0x00BB).unwrap();
            let mut ctr: u32 = 100;
            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            verifier.handle_pbkdf_request(&m.payload).unwrap();
            let resp = verifier.next_message().unwrap();
            let wire = encode_unsecured(
                ctr,
                m.exchange_id,
                OP_PBKDF_RESP,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &resp,
            );
            ctr += 1;
            dev_io.send_to(&wire, ctrl_addr).await.unwrap();

            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            verifier.handle_pake1(&m.payload).unwrap();
            let pake2 = verifier.next_message().unwrap();
            let wire = encode_unsecured(
                ctr,
                m.exchange_id,
                OP_PAKE2,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &pake2,
            );
            dev_io.send_to(&wire, ctrl_addr).await.unwrap();

            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            verifier.handle_pake3(&m.payload).unwrap();

            // Real devices close the handshake with a reliable StatusReport
            // (SessionEstablishmentSuccess) and expect it to be acked
            // (observed: Tapo P110M, M6.6.5 validation).
            ctr += 1;
            let report = encode_unsecured(
                ctr,
                m.exchange_id,
                OP_STATUS_REPORT,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &status_report_body(0, 0),
            );
            dev_io.send_to(&report, ctrl_addr).await.unwrap();

            let ack = tokio::time::timeout(std::time::Duration::from_secs(2), dev_io.recv_from())
                .await
                .expect("controller must ack the StatusReport")
                .unwrap();
            let ack = decode_unsecured(&ack.0).unwrap();
            assert_eq!(ack.opcode, OP_STANDALONE_ACK);
            assert_eq!(ack.ack_counter, Some(ctr));

            verifier.finish().unwrap()
        };

        let controller = run_pase(&ctrl_io, &mut sessions, dev_addr, pin);
        let (ctrl_result, dev_keys) = tokio::join!(controller, device);
        let sid = ctrl_result.unwrap();

        let registered = sessions.get(sid).unwrap();
        assert_eq!(registered.keys, SessionKeys::from(dev_keys));
        assert_eq!(registered.peer_id, matter_transport::SessionId(0x00BB));
        // The default UDP/MRP path must NOT flag the session reliable.
        assert_eq!(sessions.is_transport_reliable(sid), Some(false));
    }

    #[tokio::test]
    async fn run_pase_with_marks_session_reliable() {
        // Under TransportProvides the PASE handshake runs without R-flags,
        // retransmits, or standalone acks, and on success the new session is
        // flagged transport_reliable so the secured phase suppresses MRP too.
        let pin = 20_202_021;
        let params = PasePbkdfParams {
            iterations: 1000,
            salt: vec![0x55; 16],
        };

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut sessions = SessionManager::new();

        let device = async {
            let mut verifier = PaseVerifier::new_from_pin(pin, params, 0x00BB).unwrap();
            let mut ctr: u32 = 100;

            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            verifier.handle_pbkdf_request(&m.payload).unwrap();
            let resp = verifier.next_message().unwrap();
            dev_io
                .send_to(
                    &encode_unsecured(
                        ctr,
                        m.exchange_id,
                        OP_PBKDF_RESP,
                        matter_transport::ProtocolId::SECURE_CHANNEL,
                        false,
                        true,
                        Some(m.message_counter),
                        None,
                        &resp,
                    ),
                    ctrl_addr,
                )
                .await
                .unwrap();
            ctr += 1;

            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            verifier.handle_pake1(&m.payload).unwrap();
            let pake2 = verifier.next_message().unwrap();
            dev_io
                .send_to(
                    &encode_unsecured(
                        ctr,
                        m.exchange_id,
                        OP_PAKE2,
                        matter_transport::ProtocolId::SECURE_CHANNEL,
                        false,
                        true,
                        Some(m.message_counter),
                        None,
                        &pake2,
                    ),
                    ctrl_addr,
                )
                .await
                .unwrap();
            ctr += 1;

            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            verifier.handle_pake3(&m.payload).unwrap();
            // Close with a success StatusReport. Under TransportProvides the
            // controller does NOT ack it (no standalone acks on a reliable
            // transport), so the device does not wait for one.
            dev_io
                .send_to(
                    &encode_unsecured(
                        ctr,
                        m.exchange_id,
                        OP_STATUS_REPORT,
                        matter_transport::ProtocolId::SECURE_CHANNEL,
                        false,
                        true,
                        Some(m.message_counter),
                        None,
                        &status_report_body(0, 0),
                    ),
                    ctrl_addr,
                )
                .await
                .unwrap();

            verifier.finish().unwrap()
        };

        let controller = run_pase_with(
            &ctrl_io,
            &mut sessions,
            dev_addr,
            pin,
            TransportReliability::TransportProvides,
        );
        let (ctrl_result, _dev_keys) = tokio::join!(controller, device);
        let sid = ctrl_result.unwrap();

        assert_eq!(
            sessions.is_transport_reliable(sid),
            Some(true),
            "a TransportProvides PASE must flag the session transport_reliable"
        );
    }

    #[tokio::test]
    async fn run_pase_surfaces_status_report_rejection() {
        // A device that rejects session establishment (e.g. too many failed
        // attempts) answers Pake3 with a failure StatusReport; run_pase must
        // surface it as an error, not register a session.
        let pin = 20_202_021;
        let params = PasePbkdfParams {
            iterations: 1000,
            salt: vec![0x55; 16],
        };

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut sessions = SessionManager::new();

        let device = async {
            let mut verifier = PaseVerifier::new_from_pin(pin, params, 0x00BB).unwrap();
            let mut ctr: u32 = 100;
            for op in [OP_PBKDF_RESP, OP_PAKE2] {
                let (p, _) = dev_io.recv_from().await.unwrap();
                let m = decode_unsecured(&p).unwrap();
                match op {
                    OP_PBKDF_RESP => verifier.handle_pbkdf_request(&m.payload).unwrap(),
                    _ => verifier.handle_pake1(&m.payload).unwrap(),
                }
                let reply = verifier.next_message().unwrap();
                let wire = encode_unsecured(
                    ctr,
                    m.exchange_id,
                    op,
                    matter_transport::ProtocolId::SECURE_CHANNEL,
                    false,
                    true,
                    Some(m.message_counter),
                    None,
                    &reply,
                );
                ctr += 1;
                dev_io.send_to(&wire, ctrl_addr).await.unwrap();
            }
            // Pake3 arrives; reject with FAILURE / InvalidParameter (0x0002).
            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            let report = encode_unsecured(
                ctr,
                m.exchange_id,
                OP_STATUS_REPORT,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &status_report_body(1, 0x0002),
            );
            dev_io.send_to(&report, ctrl_addr).await.unwrap();
        };

        let controller = run_pase(&ctrl_io, &mut sessions, dev_addr, pin);
        let (ctrl_result, ()) = tokio::join!(controller, device);
        let err = ctrl_result.unwrap_err();
        assert!(
            matches!(
                err,
                DriverError::SessionEstablishmentFailed {
                    general_code: 1,
                    protocol_code: 0x0002,
                }
            ),
            "expected SessionEstablishmentFailed, got: {err:?}"
        );
    }
}
