//! BLE/BTP commissioning wiring for [`MatterController`](crate::MatterController)
//! (feature `ble`).
//!
//! This module layers the [`matter_ble`] central role under the commissioning
//! driver. It provides:
//!
//! - [`BtpDatagram`] — an [`AsyncDatagram`] adapter over a live
//!   [`BtpChannel`](matter_ble::central::BtpChannel), so the driver's PASE and
//!   pre-operational stages run over BTP exactly as they run over UDP.
//! - [`run_commission_ble_task`] — the off-actor task that scans for the device,
//!   opens the BTP session, binds an operational UDP socket + mDNS discovery, and
//!   drives [`commission_ble`](matter_commissioning::driver::commission_ble).
//!
//! Layering mirrors `handshake_socket`: an `AsyncDatagram` impl for a foreign
//! transport lives here (above the trait), never inside `matter-ble` — which
//! must not depend on `matter-commissioning`.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use matter_ble::central::{BleCentral, BtpChannel, CentralError};
use matter_commissioning::driver::{commission_ble, AsyncDatagram, BleDriverConfig, STREAM_PEER};
use matter_commissioning::NetworkCredentials;

use crate::error::Error;

/// How long a single BLE scan waits for a matching advertisement before giving
/// up. Exactly one scan runs per commission — either the exact 12-bit match
/// (QR / full setup code, e.g. the Pi DUT's `3840` = `0xF00`) or the upper-4-bit
/// match (manual pairing code) — chosen by the discriminator's provenance, so
/// this bounds the whole discovery phase rather than half of it.
const SCAN_TIMEOUT: Duration = Duration::from_secs(60);

/// Map a [`CentralError`] to the controller error type. BLE-layer failures are
/// operational (transient link / permission / scan) rather than protocol
/// errors, so they surface as [`Error::Operational`].
fn ble_err(e: &CentralError) -> Error {
    Error::Operational(format!("ble: {e}"))
}

/// An [`AsyncDatagram`] over a live [`BtpChannel`].
///
/// One [`recv_from`](AsyncDatagram::recv_from) return is exactly one complete
/// reassembled Matter message — BTP owns segmentation and reassembly, so the
/// driver (which parses headers from byte 0) sees whole messages just as it does
/// over UDP. The BTP channel ignores the peer address on send; every inbound
/// message is reported from the sentinel [`STREAM_PEER`], the documented
/// send-destination the BTP driver funnels PASE/pre-operational traffic to.
///
/// [`BtpChannel::recv`] takes `&mut self`, but [`AsyncDatagram::recv_from`] is
/// `&self`, so the channel is held behind a [`Mutex`]. `send_to` takes the same
/// lock: the driver calls `send_to`/`recv_from` strictly sequentially
/// (ping-pong), so the lock is never contended and `recv_from` is
/// single-consumer as the trait requires.
pub(crate) struct BtpDatagram {
    channel: Mutex<BtpChannel>,
}

impl BtpDatagram {
    /// Wrap a connected [`BtpChannel`] as an [`AsyncDatagram`].
    pub(crate) fn new(channel: BtpChannel) -> Self {
        Self {
            channel: Mutex::new(channel),
        }
    }

    /// Recover the wrapped [`BtpChannel`] to close it after commissioning.
    ///
    /// The BTP session must stay open until `commission_ble` returns — a
    /// rollback over PASE (on a CASE failure) still needs it — so the caller
    /// pulls the channel back out here and calls
    /// [`close`](BtpChannel::close) only after the driver is done.
    pub(crate) fn into_channel(self) -> BtpChannel {
        self.channel.into_inner()
    }
}

impl AsyncDatagram for BtpDatagram {
    /// Send `buf` as one whole Matter message over BTP; `peer` is ignored (the
    /// channel is already bound to the target peripheral). A dead link / stopped
    /// pump maps to [`io::ErrorKind::BrokenPipe`], which the driver treats as
    /// the transport-failure rollback trigger.
    async fn send_to(&self, buf: &[u8], _peer: SocketAddr) -> io::Result<()> {
        let channel = self.channel.lock().await;
        channel
            .send(buf)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))
    }

    /// Await the next whole Matter message reassembled from the device,
    /// reported from the sentinel [`STREAM_PEER`]. A dropped link / stopped pump
    /// maps to [`io::ErrorKind::BrokenPipe`].
    async fn recv_from(&self) -> io::Result<(Vec<u8>, SocketAddr)> {
        let mut channel = self.channel.lock().await;
        let msg = channel
            .recv()
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))?;
        Ok((msg, STREAM_PEER))
    }
}

/// Run a full BLE commission on a **freshly-constructed `BleCentral` + BTP
/// channel + operational UDP socket**, off the actor loop (mirrors
/// `run_commission_task` for the IP path). Takes only owned inputs so the
/// future is `'static` + `Send` and can be `tokio::spawn`ed.
///
/// The `BleCentral` is constructed **inside** this task: that is the macOS TCC
/// trigger, deliberately deferred to a user-initiated commissioning flow. The
/// BTP channel is closed only after `commission_ble` returns — success
/// or failure — so a rollback over PASE (when CASE fails) still has a live link.
///
/// # Errors
///
/// [`Error::Operational`] for any BLE-layer failure (no adapter / permission,
/// scan timeout, connect, GATT, or BTP handshake) or an operational-socket bind
/// failure; [`Error::Driver`] for any commissioning-protocol failure. A BLE
/// connect failure is retried once before surfacing.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_commission_ble_task(
    setup_payload: matter_commissioning::SetupPayload,
    trust: Arc<crate::trust::AttestationTrust>,
    fabric_record: matter_commissioning::FabricRecord,
    commissioner_node_id: u64,
    ipk_epoch_key: [u8; 16],
    commissioner_noc: matter_cert::MatterCertificate,
    commissioner_pkcs8: Vec<u8>,
    assigned_node_id: u64,
    admin_vendor_id: u16,
    now: matter_cert::MatterTime,
    rng: Arc<dyn matter_commissioning::NocRng>,
    network: NetworkCredentials,
) -> Result<matter_commissioning::CommissionedFabric, Error> {
    use matter_commissioning::CommissionerConfig;

    // 1. Acquire the adapter (TCC trigger) and locate the device by discriminator.
    let central = BleCentral::new().await.map_err(|e| ble_err(&e))?;

    // Which scan we run is decided by the discriminator's provenance, matching
    // `resolve_commissionable` on the IP path. A QR / full setup code knows all
    // 12 bits and must match exactly; a manual pairing code knows only the
    // upper 4 and can do no better than the short match.
    //
    // Gating on provenance rather than falling back on timeout matters twice
    // over. A long discriminator must never settle for a device that merely
    // shares its upper nibble — that is the wrong device, and it surfaces as a
    // confusing PASE failure. And a manual code no longer pays a doomed
    // full-length long scan before the short one: it goes straight to the scan
    // that can actually succeed.
    //
    // The short branch takes the *extracted* 4-bit value (`short()`), not the
    // raw u16: `advert_matches(short=true)` reads the requested short from the
    // request's low nibble.
    let device = if setup_payload.discriminator.is_short() {
        let short_disc = u16::from(setup_payload.discriminator.short());
        central
            .find_device(short_disc, true, SCAN_TIMEOUT)
            .await
            .map_err(|e| ble_err(&e))?
    } else {
        central
            .find_device(setup_payload.discriminator.as_u16(), false, SCAN_TIMEOUT)
            .await
            .map_err(|e| ble_err(&e))?
    };

    // 2. Open the BTP session (connect + GATT + BTP handshake) and adapt it to
    //    the driver's datagram seam.
    // 2a. One retry on a *connect* failure only: transient local aborts (e.g.
    // BlueZ `le-connection-abort-by-local`) routinely succeed on an immediate
    // retry, and no BTP state exists yet so a full re-open is safe. Other
    // variants (notably `ServiceDiscovery`, the known-hopeless ~25 s macOS
    // stall) surface immediately.
    let channel = match central.open_btp(&device).await {
        Ok(channel) => channel,
        Err(CentralError::Connect(first)) => {
            tracing::warn!(error = %first, "BLE connect failed; retrying once");
            central.open_btp(&device).await.map_err(|e| ble_err(&e))?
        }
        Err(e) => return Err(ble_err(&e)),
    };
    let btp = BtpDatagram::new(channel);

    // 3. Bind this task's own operational transport + discovery (copied from the
    //    IP commission task): CASE and post-CASE traffic run over UDP, and mDNS
    //    resolves the just-provisioned device's operational address.
    let udp = matter_transport::TokioUdpTransport::bind(0)
        .await
        .map_err(|e| Error::Operational(format!("commission bind: {e}")))?;
    let mut discovery = matter_transport::MdnsSdDiscovery::new()
        .map_err(|e| Error::Operational(format!("commission mdns: {e}")))?;

    // 4. Drive the BLE commissioning run. Network credentials are required: a
    //    BLE-only device with no Wi-Fi/Thread credentials to install is
    //    unprovisionable — `network` may still be `AlreadyOnNetwork` for a
    //    device that's already joined.
    let commissioner = CommissionerConfig {
        pase_attestation_challenge: [0u8; 16], // commission_ble overwrites from live PASE
        fabric: &fabric_record,
        setup_payload: &setup_payload,
        paa_trust_store: &trust.paa,
        cd_signing_roots: &trust.cd,
        commissioner_node_id,
        assigned_node_id,
        ipk_epoch_key,
        case_admin_subject: commissioner_node_id,
        admin_vendor_id,
        now,
        rng,
        network,
    };
    let config = BleDriverConfig {
        commissioner,
        passcode: setup_payload.passcode.as_u32(),
        commissioner_noc: &commissioner_noc,
        commissioner_signer_pkcs8: &commissioner_pkcs8,
    };

    let result = commission_ble(&btp, &udp, &mut discovery, config)
        .await
        .map_err(Error::from);

    // 5. Close the BTP channel only now — after the driver has fully returned, so
    //    a rollback over PASE (on a CASE failure) still had a live link. Best
    //    effort; the result above is authoritative either way.
    btp.into_channel().close().await;

    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use matter_ble::central::PumpCommand;
    use tokio::sync::mpsc;

    /// `send_to` hands the whole message to the channel's command side and
    /// resolves once the pump acks; `recv_from` surfaces an inbound message
    /// tagged with the sentinel `STREAM_PEER`.
    #[tokio::test]
    async fn send_and_recv_round_trip() {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<PumpCommand>(8);
        let (inbound_tx, inbound_rx) = mpsc::channel::<Result<Vec<u8>, CentralError>>(8);
        let dg = BtpDatagram::new(BtpChannel::from_channels(cmd_tx, inbound_rx));

        // send_to blocks on the pump ack, so drive both sides concurrently: the
        // "pump" recvs the command, asserts the bytes, and acks.
        let sent = tokio::join!(
            async { dg.send_to(b"hello-btp", STREAM_PEER).await },
            async {
                match cmd_rx.recv().await.unwrap() {
                    PumpCommand::Send(bytes, ack) => {
                        assert_eq!(bytes, b"hello-btp");
                        ack.send(Ok(())).unwrap();
                    }
                    PumpCommand::Close => panic!("unexpected Close"),
                }
            }
        );
        sent.0.unwrap();

        // Inbound: a whole message arrives on the channel's inbound side.
        inbound_tx.send(Ok(b"from-device".to_vec())).await.unwrap();
        let (msg, peer) = dg.recv_from().await.unwrap();
        assert_eq!(msg, b"from-device");
        assert_eq!(peer, STREAM_PEER);
    }

    /// A closed inbound side (pump gone / link dropped) surfaces on `recv_from`
    /// as `BrokenPipe`, the driver's transport-failure rollback trigger.
    #[tokio::test]
    async fn recv_on_closed_channel_is_broken_pipe() {
        let (cmd_tx, _cmd_rx) = mpsc::channel::<PumpCommand>(8);
        let (inbound_tx, inbound_rx) = mpsc::channel::<Result<Vec<u8>, CentralError>>(8);
        let dg = BtpDatagram::new(BtpChannel::from_channels(cmd_tx, inbound_rx));

        drop(inbound_tx); // pump gone: no more inbound messages
        let err = dg.recv_from().await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    /// A dropped command receiver (pump gone) surfaces on `send_to` as
    /// `BrokenPipe`.
    #[tokio::test]
    async fn send_on_closed_channel_is_broken_pipe() {
        let (cmd_tx, cmd_rx) = mpsc::channel::<PumpCommand>(8);
        let (_inbound_tx, inbound_rx) = mpsc::channel::<Result<Vec<u8>, CentralError>>(8);
        let dg = BtpDatagram::new(BtpChannel::from_channels(cmd_tx, inbound_rx));

        drop(cmd_rx); // pump gone: sends can no longer be delivered
        let err = dg.send_to(b"x", STREAM_PEER).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }
}
