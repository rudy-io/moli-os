//! The ICD **check-in listener** — receives a registered ICD's unsolicited
//! Check-In message (Secure Channel opcode 0x50), verifies it against the stored
//! registration key, enforces counter monotonicity, and reports it so the
//! controller can re-establish a session and read/subscribe while the device is
//! briefly active.
//!
//! Transport-wise this reuses the provider-server pattern: the ICD resolves
//! our `CheckInNodeID` via operational mDNS and sends the Check-In to our
//! advertised address, so the listener binds its own socket + advertises our
//! operational service (see [`crate::controller::MatterController::listen_for_checkin_once`]).
//! No client-actor coupling.

use matter_commissioning::driver::{decode_unsecured, AsyncDatagram};
use matter_transport::ProtocolId;

use crate::error::Error;
use crate::icd::IcdRegistration;

/// Secure Channel `ICD_CheckIn` opcode (Matter Core §4.18).
const ICD_CHECKIN_OPCODE: u8 = 0x50;

/// A verified inbound Check-In from a registered ICD.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CheckIn {
    /// The ICD device that checked in.
    pub node_id: u64,
    /// The Check-In counter (verified strictly greater than the registration's
    /// recorded floor — replay/stale check-ins are rejected).
    pub counter: u32,
    /// Optional application data the ICD included.
    pub app_data: Vec<u8>,
}

/// Receive ONE verifiable Check-In over `io`. Tries every `registrations` key
/// against each inbound Secure-Channel `ICD_CheckIn`; on a successful decrypt
/// that also satisfies `counter > registration.start_counter`, returns it.
/// Ignores unverifiable / stale / non-check-in frames, up to `max_frames`.
///
/// # Errors
/// [`Error::Operational`] on a transport failure or if no verifiable Check-In
/// arrives within `max_frames` inbound datagrams.
pub(crate) async fn recv_checkin_once<D: AsyncDatagram>(
    io: &D,
    registrations: &[IcdRegistration],
    max_frames: usize,
) -> Result<CheckIn, Error> {
    for _ in 0..max_frames {
        let (bytes, _peer) = io
            .recv_from()
            .await
            .map_err(|e| Error::Operational(format!("ICD listener recv: {e}")))?;
        let Ok(m) = decode_unsecured(&bytes) else {
            continue;
        };
        if m.protocol_id != ProtocolId::SECURE_CHANNEL || m.opcode != ICD_CHECKIN_OPCODE {
            continue;
        }
        for reg in registrations {
            if let Ok((counter, app_data)) =
                matter_crypto::checkin::decode_checkin(&reg.key, &m.payload)
            {
                if counter <= reg.start_counter {
                    continue; // stale / replayed
                }
                return Ok(CheckIn {
                    node_id: reg.node_id,
                    counter,
                    app_data,
                });
            }
        }
    }
    Err(Error::Operational(
        "no verifiable ICD check-in received".into(),
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;
    use matter_commissioning::driver::{encode_unsecured, InMemoryDatagram};

    /// Build a Check-In frame (unsecured Secure-Channel op 0x50) carrying the
    /// given counter under `key`, as a real ICD would emit it.
    fn checkin_frame(key: &[u8; 16], counter: u32, icd_node: u64) -> Vec<u8> {
        let payload = matter_crypto::checkin::encode_checkin(key, counter, &[]).unwrap();
        encode_unsecured(
            1,
            1,
            ICD_CHECKIN_OPCODE,
            ProtocolId::SECURE_CHANNEL,
            true,
            false,
            None,
            Some(icd_node),
            &payload,
        )
    }

    #[tokio::test]
    async fn recv_checkin_accepts_valid_and_reports_node() {
        let (a, b) = InMemoryDatagram::pair();
        let reg = IcdRegistration::new(0x0042, 1, 1, [0xCC; 16], 10);
        let regs = vec![reg.clone()];

        let a_addr = a.local_addr();
        let emitter = tokio::spawn(async move {
            b.send_to(&checkin_frame(&reg.key, 11, reg.node_id), a_addr)
                .await
                .unwrap();
        });

        let ci = recv_checkin_once(&a, &regs, 4).await.expect("check-in");
        assert_eq!(ci.node_id, 0x0042);
        assert_eq!(ci.counter, 11);
        assert_eq!(ci.app_data, b"");
        emitter.await.unwrap();
    }

    #[tokio::test]
    async fn recv_checkin_rejects_stale_counter() {
        let (a, b) = InMemoryDatagram::pair();
        // start_counter = 20; a check-in at counter 20 is not strictly newer.
        let reg = IcdRegistration::new(0x0042, 1, 1, [0xCC; 16], 20);
        let regs = vec![reg.clone()];
        let a_addr = a.local_addr();
        tokio::spawn(async move {
            b.send_to(&checkin_frame(&reg.key, 20, reg.node_id), a_addr)
                .await
                .unwrap();
        });
        // Only one (stale) frame arrives → the 1-frame budget is exhausted.
        assert!(recv_checkin_once(&a, &regs, 1).await.is_err());
    }

    #[tokio::test]
    async fn recv_checkin_rejects_wrong_key() {
        let (a, b) = InMemoryDatagram::pair();
        let reg = IcdRegistration::new(0x0042, 1, 1, [0xCC; 16], 10);
        let regs = vec![reg];
        let a_addr = a.local_addr();
        tokio::spawn(async move {
            // Frame signed with a DIFFERENT key — cannot be verified.
            b.send_to(&checkin_frame(&[0xEE; 16], 11, 0x0042), a_addr)
                .await
                .unwrap();
        });
        assert!(recv_checkin_once(&a, &regs, 1).await.is_err());
    }
}
