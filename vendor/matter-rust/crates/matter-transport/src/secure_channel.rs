//! Secure Channel `StatusReport` messages this crate builds and recognises.
//!
//! A `StatusReport` (Matter 1.4 Core §4.11.1.2; format in Appendix D.3) is
//! the Secure Channel message with opcode `0x40`. Its payload is three
//! little-endian fields, `GeneralCode` (u16), `ProtocolId` (u32, vendor in
//! the upper 16 bits) and `ProtocolCode` (u16), followed by optional
//! protocol-specific data.
//!
//! Only `CloseSession` lives here (§4.11.1.4), written in the spec as
//! `StatusReport(GeneralCode: SUCCESS, ProtocolId: SECURE_CHANNEL, ProtocolCode: CLOSE_SESSION)`.
//! A node sends it encrypted, on a new
//! exchange, without the R flag, to say it is removing a PASE or CASE
//! session. A node that has sent or received one SHALL remove all state
//! for that session, and MAY keep what it needs for session resumption.

use std::time::Instant;

use crate::error::Result;
use crate::framing::SessionId;
use crate::mrp::MrpFlags;
use crate::protocol_header::{opcode, ProtocolId};
use crate::session::{EncodeOutboundOutput, SessionManager};

/// `CLOSE_SESSION`, the Secure Channel protocol code a `CloseSession` carries
/// (Matter 1.4 Core §4.11.1.3, Table 19; chip `kProtocolCodeCloseSession`).
pub const PROTOCOL_CODE_CLOSE_SESSION: u16 = 0x0003;

/// The `CloseSession` `StatusReport` payload: `GeneralCode` SUCCESS (0),
/// `ProtocolId` `SECURE_CHANNEL` (vendor 0, protocol 0), `ProtocolCode`
/// `CLOSE_SESSION` (3), little-endian, with no protocol data (Table 19 lists
/// none). Byte-identical to what matter.js 0.17.1 sends.
pub const CLOSE_SESSION_PAYLOAD: [u8; 8] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00];

/// Whether a decrypted message is a `CloseSession`.
///
/// `protocol_id` and `opcode` come from the message's protocol header,
/// `payload` is what follows it. True only for opcode `0x40` under
/// [`ProtocolId::SECURE_CHANNEL`] whose payload starts with
/// [`CLOSE_SESSION_PAYLOAD`].
///
/// - The body's `ProtocolId` must be `SECURE_CHANNEL`. §4.11.1.3 says every
///   Secure Channel status report uses it, and under another protocol id
///   code 3 means something else. chip and matter.js check only the two
///   codes, so this is stricter than both.
/// - Bytes after the first eight are protocol data (Appendix D.3.3) and are
///   ignored, as chip and matter.js ignore them.
///
/// It says nothing about where the message came from. A caller acts on it
/// only for a message `SessionManager::decode_inbound` authenticated on a
/// PASE or CASE session, that starts an exchange the peer opened.
#[must_use]
pub fn is_close_session(protocol_id: ProtocolId, opcode: u8, payload: &[u8]) -> bool {
    protocol_id == ProtocolId::SECURE_CHANNEL
        && opcode == opcode::secure_channel::STATUS_REPORT
        && payload.get(..CLOSE_SESSION_PAYLOAD.len()) == Some(&CLOSE_SESSION_PAYLOAD[..])
}

/// Encode a `CloseSession` on session `session_id`, ready to send.
///
/// §4.11.1.4 requires a new exchange and no R flag, so this is
/// [`SessionManager::encode_outbound`] with no exchange id (a fresh one from
/// the node-wide counter, so `I` is set) and `MrpFlags { reliable: false }`.
/// Being unreliable, the message is never retransmitted and the peer owes no
/// ACK for it. MRP reclaims its exchange as soon as the send is recorded, so
/// nothing is left to pump and no exchange slot stays taken. No ACK can ride
/// it either, because nothing is owed on a new exchange.
///
/// It does not remove the session. The caller sends the bytes, then removes
/// the session, as the spec requires of a sender.
///
/// # Errors
///
/// Those of [`SessionManager::encode_outbound`], notably
/// [`crate::Error::UnknownSession`] for a session not in `sessions` (for
/// example one the peer already closed) and [`crate::Error::CounterOverflow`].
pub fn encode_close_session(
    sessions: &mut SessionManager,
    session_id: SessionId,
    now: Instant,
) -> Result<EncodeOutboundOutput> {
    sessions.encode_outbound(
        session_id,
        None,
        opcode::secure_channel::STATUS_REPORT,
        ProtocolId::SECURE_CHANNEL,
        &CLOSE_SESSION_PAYLOAD,
        MrpFlags { reliable: false },
        now,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use std::time::Instant;

    use matter_crypto::pase::PaseSessionKeys;

    use super::*;
    use crate::protocol_header::{decode_protocol_header, ExchangeFlags};
    use crate::session::{DecodeInboundOutput, PeerHint, SessionRole};
    use crate::{decode_secured, ReplayWindow};

    fn keys() -> PaseSessionKeys {
        PaseSessionKeys {
            ke: [0u8; 16],
            i2r_key: [1u8; 16],
            r2i_key: [2u8; 16],
            attestation_key: [3u8; 16],
        }
    }

    /// Alice (initiator) and Bob (responder) sharing one PASE key set.
    fn paired() -> (SessionManager, SessionId, SessionManager, SessionId) {
        let mut alice = SessionManager::new();
        let mut bob = SessionManager::new();
        let a = alice.register_pase(keys(), SessionRole::Initiator, 1, PeerHint::default());
        let b = bob.register_pase(keys(), SessionRole::Responder, a.0, PeerHint::default());
        (alice, a, bob, b)
    }

    /// The payload reads back as `GeneralCode` SUCCESS, `ProtocolId`
    /// `SECURE_CHANNEL` (`0x0000_0000`), `ProtocolCode` `CLOSE_SESSION`.
    #[test]
    fn close_session_payload_is_success_secure_channel_close_session() {
        let p = CLOSE_SESSION_PAYLOAD;
        assert_eq!(u16::from_le_bytes([p[0], p[1]]), 0);
        assert_eq!(u32::from_le_bytes([p[2], p[3], p[4], p[5]]), 0);
        assert_eq!(
            u16::from_le_bytes([p[6], p[7]]),
            PROTOCOL_CODE_CLOSE_SESSION
        );
    }

    /// Only the `CloseSession` report counts: not another Secure Channel code,
    /// not a failure, not another protocol id in the body, not a vendor
    /// protocol, not a short payload, not another opcode or header protocol.
    /// Trailing protocol data is ignored.
    #[test]
    fn is_close_session_accepts_only_the_close_session_report() {
        let sc = ProtocolId::SECURE_CHANNEL;
        let sr = opcode::secure_channel::STATUS_REPORT;
        assert!(is_close_session(sc, sr, &CLOSE_SESSION_PAYLOAD));
        let mut trailing = CLOSE_SESSION_PAYLOAD.to_vec();
        trailing.extend_from_slice(&[0xAB, 0xCD]);
        assert!(is_close_session(sc, sr, &trailing));

        let rejected: [(&str, ProtocolId, u8, &[u8]); 9] = [
            ("session established", sc, sr, &[0, 0, 0, 0, 0, 0, 0, 0]),
            ("busy", sc, sr, &[8, 0, 0, 0, 0, 0, 4, 0]),
            ("general failure", sc, sr, &[1, 0, 0, 0, 0, 0, 3, 0]),
            (
                "IM protocol id in the body",
                sc,
                sr,
                &[0, 0, 1, 0, 0, 0, 3, 0],
            ),
            (
                "vendor protocol in the body",
                sc,
                sr,
                &[0, 0, 0, 0, 0xF1, 0xFF, 3, 0],
            ),
            ("seven bytes", sc, sr, &[0, 0, 0, 0, 0, 0, 3]),
            ("empty", sc, sr, &[]),
            ("standalone-ack opcode", sc, 0x10, &CLOSE_SESSION_PAYLOAD),
            (
                "IM header protocol",
                ProtocolId::INTERACTION_MODEL,
                sr,
                &CLOSE_SESSION_PAYLOAD,
            ),
        ];
        for (what, protocol, op, payload) in rejected {
            assert!(!is_close_session(protocol, op, payload), "accepted: {what}");
        }
    }

    /// On the wire: I set, R and A clear, opcode 0x40, protocol `SecureChannel`,
    /// the `CloseSession` payload, on a fresh exchange. The peer reads it as an
    /// exchange the sender started and owes no ACK for it.
    #[test]
    fn encode_close_session_opens_a_new_unreliable_exchange() {
        let (mut alice, a, mut bob, b) = paired();
        let out = encode_close_session(&mut alice, a, Instant::now()).unwrap();

        let session = bob.get(b).unwrap();
        let (_, plaintext) = decode_secured(
            &out.wire_bytes,
            &session.keys,
            session.role,
            &mut ReplayWindow::new(),
            session.peer_nonce_node_id,
        )
        .unwrap();
        let (header, payload) = decode_protocol_header(&plaintext).unwrap();
        assert_eq!(header.exchange_flags, ExchangeFlags::INITIATOR);
        assert_eq!(header.ack_counter, None);
        assert_eq!(header.opcode, opcode::secure_channel::STATUS_REPORT);
        assert_eq!(header.protocol_id, ProtocolId::SECURE_CHANNEL);
        assert_eq!(header.exchange_id, out.exchange_id);
        assert_eq!(payload, CLOSE_SESSION_PAYLOAD);

        match bob.decode_inbound(&out.wire_bytes, Instant::now()).unwrap() {
            DecodeInboundOutput::AppMessage { is_initiator, .. } => assert!(is_initiator),
            other => panic!("expected an AppMessage, got {other:?}"),
        }
        assert_eq!(bob.poll_timeout(), None, "R clear: the peer owes no ACK");
        assert_eq!(
            alice.poll_timeout(),
            None,
            "unreliable: nothing to retransmit"
        );
    }

    /// Sending one holds no exchange slot: after more `CloseSession` messages than a
    /// session has exchange slots, the peer can still open an exchange.
    /// A reliable send would hold one slot each and the table would refuse it.
    #[test]
    fn close_sessions_hold_no_exchange_slot() {
        let (mut alice, a, mut bob, b) = paired();
        let now = Instant::now();
        for _ in 0..300 {
            encode_close_session(&mut alice, a, now).unwrap();
        }
        let from_bob = bob
            .encode_outbound(
                b,
                None,
                0x05,
                ProtocolId::INTERACTION_MODEL,
                &[0x15, 0x18],
                MrpFlags { reliable: true },
                now,
            )
            .unwrap();
        alice.decode_inbound(&from_bob.wire_bytes, now).unwrap();
    }

    /// A session the peer already closed (removed) yields `UnknownSession`,
    /// so a sender cannot close a session twice.
    #[test]
    fn encode_close_session_on_a_removed_session_is_unknown_session() {
        let (mut alice, a, _bob, _b) = paired();
        alice.remove(a);
        assert!(matches!(
            encode_close_session(&mut alice, a, Instant::now()),
            Err(crate::Error::UnknownSession(_))
        ));
    }
}
