//! The OTA **provider server**: a dedicated task that advertises our
//! operational service, accepts an inbound CASE session as the responder, and
//! dispatches one server-side `InvokeRequest`. Productionizes the responder
//! accept-flow proven in the actor's loopback tests; hosts it in
//! `matter-controller` so it can reuse the persisted operational identity
//! (`crate::credentials::operational_credentials`) and the existing session /
//! transport / discovery machinery without a new crate boundary.
//!
//! This module is `pub(crate)`; its low-level items are re-exported publicly
//! only under the `unstable-provider` feature (the stable path is
//! `MatterController::serve_ota`). The items stay `pub` so that re-export can
//! widen them, so `unreachable_pub` is a false positive in the feature-off
//! build — allow it module-wide.
#![allow(unreachable_pub)]
// With `ota` off, this module is still compiled — `build_operational_service`
// serves the non-OTA check-in listener, and `unstable-provider` re-exports
// `ProviderServer` from here — but most of the OTA-serving machinery
// (`serve_ota_once` and its helpers) has no caller in that configuration,
// so `dead_code`/`unused_imports` would otherwise fire workspace-wide.
#![cfg_attr(not(feature = "ota"), allow(dead_code, unused_imports))]

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use matter_cert::{MatterTime, TrustedRoots};
use matter_commissioning::driver::{decode_unsecured, encode_unsecured_reply, AsyncDatagram};
use matter_crypto::{CaseCredentials, CaseResponder, ResumptionRecord, Sigma1Outcome};
use matter_interaction::{
    build_invoke_response_command, build_invoke_response_status, parse_invoke_request, CommandPath,
    ImStatus,
};
// Only referenced by the unstable generic-provider `accept_and_dispatch_once`.
#[cfg(any(feature = "unstable-provider", test))]
use matter_interaction::ParsedInvokeRequest;
use matter_transport::{
    DecodeInboundOutput, MatterService, MrpConfig, MrpFlags, MrpProvenance, ProtocolId,
    ServiceKind, SessionId, SessionManager, SessionRole,
};

use crate::error::Error;

// SecureChannel handshake opcodes (Matter Core §4.10 / §4.13).
const OP_SIGMA1: u8 = 0x30;
const OP_SIGMA2: u8 = 0x31;
const OP_SIGMA3: u8 = 0x32;
const OP_SIGMA2_RESUME: u8 = 0x33;
const OP_STATUS_REPORT: u8 = 0x40;
const OP_MRP_STANDALONE_ACK: u8 = 0x10;
// Interaction Model opcodes.
const OP_INVOKE_REQUEST: u8 = 0x08;

/// Frames discarded while awaiting a Sigma1 before the accept fails. Stray
/// LAN datagrams to the advertised port (undecodable noise, stale acks,
/// leftovers from a discarded session) must not consume pooled credentials —
/// but a flooder should still hit a bound rather than spin the accept
/// forever.
/// Frames to absorb while waiting for Sigma3 before giving up. A real
/// initiator sends at most a standalone ack plus a Sigma1 retransmit or two
/// ahead of it; this bounds a peer that never completes the handshake.
const MAX_AWAIT_SIGMA3_FRAMES: usize = 8;

const MAX_AWAIT_SIGMA1_DISCARDS: usize = 64;
const OP_INVOKE_RESPONSE: u8 = 0x09;

// Secure-Channel StatusReport general codes (Matter Core §4.11.6).
const STATUS_GENERAL_FAILURE: u16 = 0x0001;

/// Encode the fixed 8-byte Secure-Channel `StatusReport` body (Matter Core
/// §4.11.6): `GeneralCode` (u16 LE) || `ProtocolId` (u32 LE, `vendor<<16 |
/// protocol`) || `ProtocolStatus` (u16 LE). Used to abort a BDX transfer
/// so the peer learns the failure instead of timing out.
fn encode_status_report_body(general: u16, proto: ProtocolId, protocol_status: u16) -> Vec<u8> {
    let proto_id: u32 = (u32::from(proto.vendor) << 16) | u32::from(proto.protocol);
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&general.to_le_bytes());
    body.extend_from_slice(&proto_id.to_le_bytes());
    body.extend_from_slice(&protocol_status.to_le_bytes());
    body
}

/// Parse the fixed 8-byte Secure-Channel `StatusReport` body into
/// `(general_code, protocol_id, protocol_status)`. Returns `None` if the body
/// is shorter than 8 bytes.
fn parse_status_report_body(payload: &[u8]) -> Option<(u16, u32, u16)> {
    let b: &[u8; 8] = payload.get(..8)?.try_into().ok()?;
    Some((
        u16::from_le_bytes([b[0], b[1]]),
        u32::from_le_bytes([b[2], b[3], b[4], b[5]]),
        u16::from_le_bytes([b[6], b[7]]),
    ))
}

// OtaSoftwareUpdateProvider (0x0029) command ids (Matter Core §11.20).
const OTA_PROVIDER_CLUSTER: u32 = 0x0029;
const CMD_QUERY_IMAGE: u32 = 0x00;
const CMD_QUERY_IMAGE_RESPONSE: u32 = 0x01;
const CMD_APPLY_UPDATE_REQUEST: u32 = 0x02;
const CMD_APPLY_UPDATE_RESPONSE: u32 = 0x03;
const CMD_NOTIFY_UPDATE_APPLIED: u32 = 0x04;

/// True when `frame` is an unsecured (session id 0) message — i.e. a new
/// session-establishment attempt arriving while a secured session is being
/// served. Bytes 1..3 are the little-endian session id (Matter Core §4.4.1).
fn is_unsecured_frame(frame: &[u8]) -> bool {
    frame.len() >= 3 && frame[1] == 0 && frame[2] == 0
}

/// What arrived on the socket after our closing success `StatusReport`.
///
/// The handshake is over at that point, but exactly one more frame is *ours*
/// to consume — the initiator's standalone ack. Anything else belongs to
/// whoever runs next, and must be handed on rather than dropped.
enum PostHandshakeFrame {
    /// The standalone ack we were waiting for (or nothing else of interest).
    Consumed,
    /// A Sigma1 opening a NEW handshake on a different exchange.
    NewHandshake(CarriedFrame),
    /// A **secured** application message on the session just established.
    ///
    /// A real requestor pipelines its first request ahead of the ack — the
    /// observed order from an esp-matter OTA requestor is `StatusReport` →
    /// `QueryImage` → ack. Discarding this loses the request outright.
    Secured(CarriedFrame),
}

/// One accepted CASE session, plus any frame already taken off the socket that
/// the caller must process before receiving anything more.
pub(crate) struct AcceptedSession {
    /// Session manager holding the established session.
    pub sessions: SessionManager,
    /// Local id of the established session.
    pub session_id: SessionId,
    /// The peer's address.
    pub peer: SocketAddr,
    /// A Sigma1 opening a new handshake, seen while closing this one out. Feed
    /// it to the next `accept_case` as `first_frame` so no handshake bytes are
    /// lost across the session boundary.
    pub next_handshake: Option<CarriedFrame>,
    /// A secured application message that arrived ahead of the initiator's
    /// standalone ack. **Must be processed before receiving more**, or the
    /// requestor's first request is silently lost.
    pub pending_secured: Option<CarriedFrame>,
}

/// A raw datagram (frame bytes + sender) handed from one accept to the next,
/// so no handshake bytes are lost across session boundaries.
type CarriedFrame = (Vec<u8>, SocketAddr);

/// Build the operational `_matter._tcp` mDNS record to advertise so a requestor
/// can resolve us. Instance name is `<compressed-fabric-id>-<node-id>` in
/// uppercase hex (Matter Core §4.3.1), matching what the controller's initiator
/// resolves against via `operational_instance_name`.
#[must_use]
pub fn build_operational_service(
    compressed_fabric_id: [u8; 8],
    node_id: u64,
    addresses: Vec<IpAddr>,
    port: u16,
) -> MatterService {
    let instance_name =
        matter_commissioning::driver::operational_instance_name(compressed_fabric_id, node_id);
    // Operational TXT params (SII/SAI/SAT) are optional hints; F3 advertises
    // none (the requestor resolves us by SRV + A/AAAA). F4/hardening can add
    // session-interval hints if a requestor needs them.
    MatterService::new(
        instance_name,
        ServiceKind::Operational,
        addresses,
        port,
        std::collections::HashMap::new(),
    )
}

/// A multi-session OTA provider server: accepts inbound CASE sessions as the
/// responder (one per pooled credential), then dispatches server-side
/// `InvokeRequest`s. Generic over the datagram transport so it runs over
/// `TokioUdpTransport` in production and `InMemoryDatagram` in tests.
///
/// This productionizes the responder accept-flow proven in the actor's loopback
/// tests (`run_loopback_device`): Sigma1→Sigma2→Sigma3→`SessionManager` register,
/// then secured IM dispatch on the established session.
///
/// The credential pool is consumed one entry per `accept_case` call. When the
/// pool is exhausted, `accept_case` (and any caller such as `serve_ota_once`)
/// returns [`Error::Operational`] with the message
/// `"provider server: credential pool exhausted"`. The pool is sized by the
/// caller — `serve_ota` mints four entries (first session + post-reboot session
/// + retry slack) from the persisted fabric.
pub struct ProviderServer<D> {
    io: D,
    /// Pool of operational identities, one consumed per CASE accept (the
    /// responder state machine takes ownership of its credentials).
    /// `serve_ota` mints these from the persisted fabric, sized for the first
    /// session, a post-reboot session and retry slack.
    credentials: Vec<CaseCredentials>,
    roots: TrustedRoots,
    /// Base secured session id; accept N advertises `base.wrapping_add(N)` so
    /// consecutive sessions never share a local id.
    base_session_id: u16,
    /// Number of accepts performed so far (also indexes the session id).
    accepts: u16,
    now: MatterTime,
    /// Next message counter for the unsecured handshake frames this server
    /// sends (Sigma2, `Sigma2_Resume`, status reports). Starts at a CSPRNG
    /// value in `[1, 2^28]` (Matter Core Spec §4.6.1.1, §4.6.1.2; see
    /// [`seed_handshake_counter`]) and advances by one per frame.
    handshake_counter: u32,
    /// When set, an accepted session whose authenticated peer node id is not
    /// this value fails the accept (its pooled credential is consumed — that
    /// is the point: a fabric member other than the OTA target must not be
    /// able to hijack the serve). `serve_ota` pins its `target_node_id`.
    expected_peer: Option<u64>,
    /// Known CASE resumption records. When an inbound Sigma1 carries
    /// resumption fields whose id matches one of these, the session is
    /// resumed (`Sigma2_Resume`) instead of a full handshake — chip's OTA
    /// requestor always requests resumption of the session the controller
    /// just used to announce, so `serve_ota` seeds this with the announce
    /// connect's persisted record. No match falls back to
    /// `reject_resumption` + full handshake.
    resumption_records: Vec<ResumptionRecord>,
    /// Invoked with the fresh [`ResumptionRecord`] each accept produces
    /// (rotated on the resumed path, brand-new on the full path), so the
    /// caller can persist it IMMEDIATELY — a caller-side timeout that drops
    /// the serve future must not lose the rotation. Best-effort: the sink
    /// must not block (spawn if it needs async work).
    record_sink: Option<Box<dyn Fn(ResumptionRecord) + Send + Sync>>,
}

/// Draw the provider server's first unsecured handshake counter:
/// `Crypto_DRBG(len = 28) + 1`, a CSPRNG value in `[1, 2^28]` (Matter Core
/// Spec §4.6.1.1; §4.6.1.2 requires the unencrypted counter to start random).
/// Same mapping as chip's `GlobalUnencryptedMessageCounter::Init`
/// (`GetRandU32() & 0x0FFFFFFF`, then a pre-increment;
/// `src/transport/MessageCounter.h`) and as `matter-transport`'s secure
/// session counters.
///
/// Falls back to 1, with a `warn`, if the system CSPRNG fails, so
/// [`ProviderServer::new`] stays infallible. chip aborts there instead
/// (`VerifyOrDie` in `src/crypto/RandUtils.cpp`); a library must not abort its
/// host. The fallback loses only the traffic-analysis resistance the random
/// start exists for: the counter is in the cleartext header, and these
/// frames are unencrypted anyway. Every accept then runs a CASE handshake
/// that needs the same CSPRNG and fails closed without it.
fn seed_handshake_counter() -> u32 {
    let mut bytes = [0u8; 4];
    if matter_crypto::random_bytes(&mut bytes).is_err() {
        tracing::warn!(
            target: "matter_controller::provider_server",
            "system CSPRNG unavailable seeding the provider server's handshake \
             counter; falling back to 1 (spec §4.6.1.1 wants a random start)"
        );
        return 1;
    }
    (u32::from_le_bytes(bytes) & 0x0FFF_FFFF) + 1
}

impl<D: AsyncDatagram> ProviderServer<D> {
    /// Build a provider server bound to `io`, authenticating from the
    /// `credentials` pool (our operational identities). `roots` and `now` are
    /// used to validate the peer's certificate chain on each accept.
    ///
    /// `base_session_id` is the first secured session id advertised in Sigma2;
    /// the Nth accept uses `base_session_id.wrapping_add(N)` so consecutive
    /// sessions never reuse the same local id.
    ///
    /// The pool is consumed one entry per accept. When it is empty, the next
    /// call to `serve_ota_once` (or any method that calls `accept_case`)
    /// returns an [`Error::Operational`] containing
    /// `"provider server: credential pool exhausted"`.
    #[must_use]
    pub fn new(
        io: D,
        credentials: Vec<CaseCredentials>,
        roots: TrustedRoots,
        base_session_id: u16,
        now: MatterTime,
    ) -> Self {
        Self {
            io,
            credentials,
            roots,
            base_session_id,
            accepts: 0,
            now,
            handshake_counter: seed_handshake_counter(),
            expected_peer: None,
            resumption_records: Vec::new(),
            record_sink: None,
        }
    }

    /// Register a callback that is invoked once per completed accept with the
    /// fresh [`ResumptionRecord`] the handshake produced (rotated on the resumed
    /// path, brand-new on the full path). The caller can use this to persist the
    /// record immediately — a future that is cancelled after `accept_case`
    /// completes but before the caller stores the record would otherwise lose the
    /// rotation. The sink is called synchronously and **must not block**; spawn
    /// an async task if async work is needed.
    #[must_use]
    pub fn with_record_sink(mut self, sink: Box<dyn Fn(ResumptionRecord) + Send + Sync>) -> Self {
        self.record_sink = Some(sink);
        self
    }

    /// Seed the server with known CASE resumption records (see the field
    /// docs). An inbound resumption-requesting Sigma1 matching one of these
    /// by id is accepted via `Sigma2_Resume`; anything else falls back to a
    /// full handshake.
    #[must_use]
    pub fn with_resumption_records(mut self, records: Vec<ResumptionRecord>) -> Self {
        self.resumption_records = records;
        self
    }

    /// Pin the peer: an accepted session must authenticate as `node_id` or
    /// the accept fails (consuming its pooled credential). Without this, any
    /// member of the fabric could consume the serve.
    #[must_use]
    pub fn with_expected_peer(mut self, node_id: u64) -> Self {
        self.expected_peer = Some(node_id);
        self
    }

    fn next_handshake_counter(&mut self) -> u32 {
        let c = self.handshake_counter;
        self.handshake_counter = self.handshake_counter.wrapping_add(1);
        c
    }

    async fn recv(&self) -> Result<(Vec<u8>, SocketAddr), Error> {
        self.io
            .recv_from()
            .await
            .map_err(|e| Error::Operational(format!("provider recv: {e}")))
    }

    async fn send(&self, bytes: &[u8], peer: SocketAddr) -> Result<(), Error> {
        self.io
            .send_to(bytes, peer)
            .await
            .map_err(|e| Error::Operational(format!("provider send: {e}")))
    }

    /// Receive the next datagram while driving the session's MRP timers, so
    /// scheduled standalone acks (and retransmits) fire even while we sit in
    /// `recv`. Load-bearing for the OTA flow: the requestor's `BlockAckEOF`
    /// is MRP-reliable and we reply with nothing — without the pumped
    /// standalone ack, chip retransmits it, marks the session defunct, and
    /// abandons the update before `ApplyUpdateRequest` (observed live).
    async fn recv_secured(
        &self,
        sessions: &mut SessionManager,
        peer: SocketAddr,
    ) -> Result<(Vec<u8>, SocketAddr), Error> {
        use matter_transport::MrpEvent;
        loop {
            let Some(deadline) = sessions.poll_timeout() else {
                return self.recv().await;
            };
            let wait = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(wait, self.recv()).await {
                Ok(result) => return result,
                Err(_deadline_hit) => {
                    for event in sessions.handle_timeout(Instant::now()) {
                        match event {
                            MrpEvent::Retransmit { packet, .. }
                            | MrpEvent::SendStandaloneAck { packet, .. } => {
                                self.send(&packet, peer).await?;
                            }
                            // Single-session server: nothing to resolve on
                            // expiry; `MrpEvent` is non_exhaustive.
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    /// Accept ONE inbound CASE session as the responder, returning an
    /// established [`SessionManager`] + the secured `SessionId` + the peer's
    /// address. Mirrors the proven `run_loopback_device` accept-flow on the full
    /// path; a Sigma1 carrying resumption fields that match a seeded record (see
    /// [`Self::with_resumption_records`]) takes the `Sigma2_Resume` fast path
    /// instead.
    ///
    /// The fresh [`ResumptionRecord`] the handshake produces is handled
    /// internally: it is re-seeded into `self.resumption_records` (so the NEXT
    /// accept can match it) and passed to the `record_sink` (if set) before this
    /// method returns.
    ///
    /// If `first_frame` is `Some`, that datagram is used as the Sigma1 instead
    /// of calling `recv` — useful for callers that have already peeked the first
    /// packet (e.g., a multi-session loop that demuxes by session id).
    ///
    /// The returned [`CarriedFrame`] is `Some` when the full-handshake close
    /// saw a NEW Sigma1 in place of the initiator's standalone ack (see
    /// [`Self::complete_full`]); the caller must feed it into its next accept
    /// or the handshake attempt it opens is lost.
    // One accept from Sigma1 through session registration: the resumption
    // decision, the two completion paths and the post-handshake classification
    // read as one sequence, and splitting them would hide which frames each
    // step is entitled to consume.
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn accept_case(
        &mut self,
        first_frame: Option<CarriedFrame>,
    ) -> Result<AcceptedSession, Error> {
        // Fast-fail an exhausted pool before any IO. The check does NOT pop:
        // a credential is consumed only once a valid Sigma1 is in hand, so
        // stray datagrams to the advertised port cannot burn the pool.
        if self.credentials.is_empty() {
            return Err(Error::Operational(
                "provider server: credential pool exhausted".into(),
            ));
        }

        // Await a valid Sigma1, discarding anything else (undecodable noise,
        // stray acks, stale secured frames) within a bounded budget.
        let mut carried = first_frame;
        let mut discarded = 0usize;
        let (m1, peer) = loop {
            let (bytes, from) = match carried.take() {
                Some(f) => f,
                None => self.recv().await?,
            };
            match decode_unsecured(&bytes) {
                Ok(m) if m.opcode == OP_SIGMA1 => break (m, from),
                _ => {
                    discarded += 1;
                    if discarded >= MAX_AWAIT_SIGMA1_DISCARDS {
                        return Err(Error::Operational(format!(
                            "no Sigma1 within {MAX_AWAIT_SIGMA1_DISCARDS} frames"
                        )));
                    }
                }
            }
        };

        // A real handshake attempt is starting: consume one pooled identity.
        let credentials = self.credentials.remove(0);
        let responder_session_id = self.base_session_id.wrapping_add(self.accepts);
        self.accepts = self.accepts.wrapping_add(1);
        let mut responder = CaseResponder::new(
            credentials,
            self.roots.clone(),
            responder_session_id,
            self.now,
        )
        .map_err(|e| Error::Operational(format!("CASE responder init: {e}")))?;

        let outcome = responder
            .handle_sigma1(&m1.payload)
            .map_err(|e| Error::Operational(format!("handle_sigma1: {e}")))?;

        // Capture the initiator's advertised MRP timings now, before `finish`
        // consumes the responder. As the responder we have no operational mDNS
        // record for this peer, so Sigma1's `SessionParameters` element is the
        // ONLY thing that can tell us it is sleepy — and this session is the
        // one we then push BDX blocks down.
        let peer_session_params = responder.peer_session_params();

        let resumed = match outcome {
            Sigma1Outcome::NewSession => false,
            Sigma1Outcome::ResumptionRequested { id } => {
                if let Some(pos) = self.resumption_records.iter().position(|r| r.id == id) {
                    let record = self.resumption_records.swap_remove(pos);
                    responder
                        .accept_resumption(record)
                        .map_err(|e| Error::Operational(format!("accept_resumption: {e}")))?;
                    true
                } else {
                    // Unknown id — decline and fall back to a full handshake.
                    //
                    // This is the self-healing path for a stale resumption
                    // record: the cost is one wasted round trip, not
                    // unreachability, which is why records are not invalidated
                    // on CASE failure. Logged so that "it fires occasionally"
                    // and "it fires every time for this device" are
                    // distinguishable in the field -- the latter would mean the
                    // record really is stuck and worth revisiting.
                    tracing::debug!(
                        target: "matter_controller::provider_server",
                        "unknown CASE resumption id offered; declining and falling back to a full handshake"
                    );
                    responder
                        .reject_resumption()
                        .map_err(|e| Error::Operational(format!("reject_resumption: {e}")))?;
                    false
                }
            }
        };

        let post = if resumed {
            self.complete_resumed(&mut responder, &m1, peer).await?;
            PostHandshakeFrame::Consumed
        } else {
            self.complete_full(&mut responder, &m1, peer).await?
        };
        let (carry, pending_secured) = match post {
            PostHandshakeFrame::Consumed => (None, None),
            PostHandshakeFrame::NewHandshake(f) => (Some(f), None),
            PostHandshakeFrame::Secured(f) => (None, Some(f)),
        };

        let output = responder
            .finish()
            .map_err(|e| Error::Operational(format!("CASE finish: {e}")))?;
        // Enforce the pin BEFORE re-seeding/sinking the record: a rejected
        // peer must leave no resumption state behind.
        if let Some(expected) = self.expected_peer {
            if output.peer.node_id != expected {
                return Err(Error::Operational(format!(
                    "provider server: accepted peer node {:#x} is not the expected {expected:#x}",
                    output.peer.node_id
                )));
            }
        }
        if let Some(record) = output.resumption_record.clone() {
            // Re-seed so the NEXT accept (the post-reboot requestor resumes
            // with the id rotated during THIS handshake) can match it.
            self.resumption_records.push(record.clone());
            if let Some(sink) = &self.record_sink {
                sink(record);
            }
        }
        // Size the operational session to whatever the initiator advertised.
        // Until this existed the responder role always got `MrpConfig::default()`
        // + `MrpProvenance::Unknown`, so a sleepy requestor was retransmitted at
        // on the spec's 500 ms idle base no matter what it asked for.
        //
        // Provenance is binary here on purpose: the element was present, or it
        // was not. A peer that sends the element but omits an interval gets the
        // spec default for that field via the merge, exactly as the mDNS path
        // treats a missing TXT key.
        let (mrp, provenance) = match peer_session_params {
            Some(p) => (
                MrpConfig::default().merge_session_params(&p),
                MrpProvenance::PeerAdvertised,
            ),
            None => (MrpConfig::default(), MrpProvenance::Unknown),
        };
        let mut sessions = SessionManager::new();
        let session_id =
            sessions.register_case_with_mrp(&output, SessionRole::Responder, mrp, provenance);
        Ok(AcceptedSession {
            sessions,
            session_id,
            peer,
            next_handshake: carry,
            pending_secured,
        })
    }

    /// Resumed path: send `Sigma2_Resume` on Sigma1's exchange, then await the
    /// initiator's success `StatusReport` and standalone-ack it (the report is
    /// MRP-reliable; without our ack chip retransmits it and eventually tears
    /// the exchange down). Tolerates interleaved Sigma1 retransmits (re-sends
    /// `Sigma2_Resume`) and stray standalone acks.
    async fn complete_resumed(
        &mut self,
        responder: &mut CaseResponder,
        m1: &matter_commissioning::driver::UnsecuredMessage,
        peer: SocketAddr,
    ) -> Result<(), Error> {
        let sigma2_resume = responder
            .next_message()
            .map_err(|e| Error::Operational(format!("sigma2_resume: {e}")))?;
        let c = self.next_handshake_counter();
        let wire = encode_unsecured_reply(
            c,
            m1.exchange_id,
            OP_SIGMA2_RESUME,
            ProtocolId::SECURE_CHANNEL,
            true,
            Some(m1.message_counter),
            m1.source_node_id,
            &sigma2_resume,
        );
        self.send(&wire, peer).await?;

        // Await the initiator's SigmaFinished success StatusReport, within a
        // bounded frame budget.
        for _ in 0..8 {
            let (bytes, _) = self.recv().await?;
            let m = decode_unsecured(&bytes)
                .map_err(|e| Error::Operational(format!("post-resume frame: {e}")))?;
            match m.opcode {
                OP_STATUS_REPORT => {
                    // StatusReport body: GeneralCode(u16 LE) || ProtocolId(u32) || ProtocolCode(u16).
                    let general_code = m
                        .payload
                        .get(0..2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .ok_or_else(|| {
                            Error::Operational("truncated resumption StatusReport".into())
                        })?;
                    if general_code != 0 {
                        return Err(Error::Operational(format!(
                            "initiator rejected resumption: StatusReport general code {general_code}"
                        )));
                    }
                    // Ack the reliable report so the initiator's MRP settles.
                    let c = self.next_handshake_counter();
                    let ack = encode_unsecured_reply(
                        c,
                        m.exchange_id,
                        OP_MRP_STANDALONE_ACK,
                        ProtocolId::SECURE_CHANNEL,
                        false,
                        Some(m.message_counter),
                        m.source_node_id.or(m1.source_node_id),
                        &[],
                    );
                    self.send(&ack, peer).await?;
                    return Ok(());
                }
                // Sigma1 retransmit: our Sigma2_Resume (or its ack) was lost —
                // re-send it on the same exchange.
                OP_SIGMA1 => {
                    let c = self.next_handshake_counter();
                    let wire = encode_unsecured_reply(
                        c,
                        m.exchange_id,
                        OP_SIGMA2_RESUME,
                        ProtocolId::SECURE_CHANNEL,
                        true,
                        Some(m.message_counter),
                        m.source_node_id.or(m1.source_node_id),
                        &sigma2_resume,
                    );
                    self.send(&wire, peer).await?;
                }
                // A standalone ack of our Sigma2_Resume — fine, keep waiting.
                OP_MRP_STANDALONE_ACK => {}
                other => {
                    return Err(Error::Operational(format!(
                        "expected resumption StatusReport (0x40), got {other:#04x}"
                    )))
                }
            }
        }
        Err(Error::Operational(
            "no StatusReport after Sigma2_Resume within frame budget".into(),
        ))
    }

    /// Full-handshake path (Sigma2 → Sigma3 → our success `StatusReport`), used
    /// for a plain Sigma1 and as the fallback after `reject_resumption`.
    ///
    /// Returns the frame to carry into the next accept when the closing
    /// ack-absorb `recv` saw a NEW Sigma1 instead of the initiator's
    /// standalone ack: a requestor that applies and reboots fast can have its
    /// next handshake's Sigma1 in flight before the ack — eating it would
    /// force the peer through an MRP retransmit round AND burn one pooled
    /// retry credential on this side. Everything else (the ack, noise, a
    /// same-exchange Sigma1 — a stale duplicate of `m1`, provably already
    /// answered because Sigma3 arrived) is absorbed as before.
    async fn complete_full(
        &mut self,
        responder: &mut CaseResponder,
        m1: &matter_commissioning::driver::UnsecuredMessage,
        peer: SocketAddr,
    ) -> Result<PostHandshakeFrame, Error> {
        let sigma2 = responder
            .next_message()
            .map_err(|e| Error::Operational(format!("sigma2: {e}")))?;
        let c = self.next_handshake_counter();
        let wire = encode_unsecured_reply(
            c,
            m1.exchange_id,
            OP_SIGMA2,
            ProtocolId::SECURE_CHANNEL,
            true,
            Some(m1.message_counter),
            m1.source_node_id,
            &sigma2,
        );
        self.send(&wire, peer).await?;

        // Sigma3 → success StatusReport.
        //
        // A real chip initiator standalone-acks our Sigma2 BEFORE sending
        // Sigma3: Sigma3 costs it an ECDSA signature plus an AEAD seal, which
        // is far longer than MRP's ack deadline, so it acknowledges delivery
        // first and sends the message when it is ready. A single blocking
        // `recv` that demands 0x32 therefore fails against every real device
        // with `expected Sigma3 (0x32), got 0x10` — observed against an
        // esp-matter ESP32-C6 acting as OTA requestor. The resumed path
        // already tolerated this; the full path did not.
        //
        // Also re-send Sigma2 on a Sigma1 retransmit (our Sigma2, or its ack,
        // was lost), mirroring `complete_resumed`.
        let mut m3 = None;
        for _ in 0..MAX_AWAIT_SIGMA3_FRAMES {
            let (bytes, _) = self.recv().await?;
            let m =
                decode_unsecured(&bytes).map_err(|e| Error::Operational(format!("sigma3: {e}")))?;
            match m.opcode {
                OP_SIGMA3 => {
                    m3 = Some(m);
                    break;
                }
                // Delivery acknowledged; the initiator is computing Sigma3.
                OP_MRP_STANDALONE_ACK => {}
                // Our Sigma2 (or its ack) was lost — re-send on the same exchange.
                OP_SIGMA1 => {
                    let c = self.next_handshake_counter();
                    let again = encode_unsecured_reply(
                        c,
                        m.exchange_id,
                        OP_SIGMA2,
                        ProtocolId::SECURE_CHANNEL,
                        true,
                        Some(m.message_counter),
                        m.source_node_id.or(m1.source_node_id),
                        &sigma2,
                    );
                    self.send(&again, peer).await?;
                }
                other => {
                    return Err(Error::Operational(format!(
                        "expected Sigma3 (0x32), got {other:#04x}"
                    )))
                }
            }
        }
        let Some(m3) = m3 else {
            return Err(Error::Operational(
                "no Sigma3 within frame budget".to_string(),
            ));
        };
        responder
            .handle_sigma3(&m3.payload)
            .map_err(|e| Error::Operational(format!("handle_sigma3: {e}")))?;
        let mut body = Vec::with_capacity(8);
        body.extend_from_slice(&0u16.to_le_bytes()); // GeneralCode: success
        body.extend_from_slice(&0u32.to_le_bytes()); // ProtocolId: SecureChannel
        body.extend_from_slice(&0u16.to_le_bytes()); // ProtocolCode: 0
        let c = self.next_handshake_counter();
        let report = encode_unsecured_reply(
            c,
            m3.exchange_id,
            OP_STATUS_REPORT,
            ProtocolId::SECURE_CHANNEL,
            true,
            Some(m3.message_counter),
            m3.source_node_id.or(m1.source_node_id),
            &body,
        );
        self.send(&report, peer).await?;

        // Absorb the initiator's standalone ack of our StatusReport — and ONLY
        // that. This used to consume one frame unconditionally and drop
        // anything that was not a fresh Sigma1, which silently swallowed the
        // requestor's first secured request whenever it was pipelined ahead of
        // the ack. That is the ordering a real device produces (observed from
        // an esp-matter ESP32-C6 OTA requestor: StatusReport → QueryImage →
        // ack), so the first request was lost on every real serve.
        let (bytes, from) = self.recv().await?;
        if !is_unsecured_frame(&bytes) {
            return Ok(PostHandshakeFrame::Secured((bytes, from)));
        }
        if let Ok(m) = decode_unsecured(&bytes) {
            if m.opcode == OP_SIGMA1 && m.exchange_id != m1.exchange_id {
                return Ok(PostHandshakeFrame::NewHandshake((bytes, from)));
            }
        }
        Ok(PostHandshakeFrame::Consumed)
    }

    /// Accept ONE inbound CASE session, then dispatch up to `max_invokes`
    /// server-side `InvokeRequest`s through `handler`, replying to each on its
    /// exchange. Returns the number of invokes dispatched.
    ///
    /// `handler` maps a parsed `InvokeRequest` to the encoded `InvokeResponse`
    /// message bytes (e.g. via `matter_interaction::build_invoke_response_*`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Operational`] on a transport, CASE-handshake, or framing
    /// failure (including a non-`NewSession` Sigma1 or an unexpected opcode), or
    /// [`Error::Transport`] / [`Error::InteractionModel`] from the session / IM
    /// layers.
    ///
    /// Part of the unstable generic-provider surface (see the module docs); the
    /// stable OTA path uses `serve_ota_once`. Compiled only under
    /// `unstable-provider` (its sole caller, `serve_provider_once`) or in tests.
    #[cfg(any(feature = "unstable-provider", test))]
    pub async fn accept_and_dispatch_once<H>(
        mut self,
        mut handler: H,
        max_invokes: usize,
    ) -> Result<usize, Error>
    where
        H: FnMut(&ParsedInvokeRequest) -> Vec<u8>,
    {
        // Single-session API: there is no next accept to feed a carried
        // Sigma1 into, so it is dropped (the peer's MRP retransmit covers it)
        // — the pre-multi-session behavior.
        let AcceptedSession {
            mut sessions,
            session_id: sid,
            peer,
            next_handshake: _fast_sigma1,
            mut pending_secured,
        } = self.accept_case(None).await?;

        let mut dispatched = 0usize;
        while dispatched < max_invokes {
            // Process a request the handshake close-out already pulled off the
            // socket before receiving anything more; a real requestor pipelines
            // its first request ahead of the standalone ack.
            let (wire, _) = match pending_secured.take() {
                Some(f) => f,
                None => self.recv_secured(&mut sessions, peer).await?,
            };
            // Unsecured frames (session id 0) reaching here are handshake
            // traffic, not application traffic — the initiator's standalone ack
            // of our StatusReport is the common one. Skip them rather than
            // feeding them to the secured decoder, which rejects session 0.
            if is_unsecured_frame(&wire) {
                continue;
            }
            if let DecodeInboundOutput::AppMessage {
                exchange_id,
                is_initiator,
                opcode,
                payload,
                ..
            } = sessions.decode_inbound(&wire, Instant::now())?
            {
                if opcode != OP_INVOKE_REQUEST {
                    // Ignore non-invoke app messages in F3 (e.g. reads).
                    continue;
                }
                let parsed = parse_invoke_request(&payload)?;
                let response = handler(&parsed);
                // Answer on the exchange the request arrived on (the
                // requestor's, `I=0` from us, when it set `I`).
                let out = sessions.encode_outbound_with_role(
                    sid,
                    exchange_id,
                    !is_initiator,
                    OP_INVOKE_RESPONSE,
                    ProtocolId::INTERACTION_MODEL,
                    &response,
                    MrpFlags { reliable: false },
                    Instant::now(),
                )?;
                self.send(&out.wire_bytes, peer).await?;
                dispatched += 1;
            }
        }
        Ok(dispatched)
    }

    /// Accept CASE sessions in sequence, serving `image` to the requestor over the
    /// full OTA flow — `QueryImage` → `QueryImageResponse`, a BDX transfer, then
    /// `ApplyUpdateRequest` → `ApplyUpdateResponse` (Proceed) — and completing once
    /// `NotifyUpdateApplied` is received on ANY session. A real requestor downloads
    /// and applies on its first session, reboots into the new image, and sends
    /// `NotifyUpdateApplied` on a fresh session; this method spans that reboot by
    /// running an outer loop over `accept_case` calls.
    ///
    /// Unsecured frames (session id 0) arriving while a secured session is being
    /// served are recognised as new-session-establishment attempts; they are
    /// carried into the next outer iteration as the `first_frame` for the next
    /// `accept_case` call, so no handshake bytes are lost.
    ///
    /// The caller owns the deadline: wrap `serve_ota_once` in
    /// `tokio::time::timeout` (or similar) to bound a requestor that never
    /// returns. Pool exhaustion (all credentials consumed) and a per-session step
    /// budget are the two error paths.
    ///
    /// The fresh [`ResumptionRecord`] the accept handshake produced is re-seeded
    /// and forwarded to the `record_sink` (if set via
    /// [`Self::with_record_sink`]) before the OTA dispatch loop begins — the
    /// caller need not wait for the full OTA flow to persist the rotation.
    ///
    /// `offer` shapes the `QueryImageResponse` (its `ImageURI`/`UpdateToken`);
    /// `max_block_size` caps each BDX block. All replies are unreliable
    /// (piggyback ack) — happy-path, localhost-validated. Messages route by
    /// [`ProtocolId`]: Interaction-Model invokes go to the `matter-ota` handlers,
    /// `ProtocolId::BDX` messages drive a [`matter_bdx::BlockSender`].
    ///
    /// # Errors
    ///
    /// [`Error::Operational`] on a CASE/transport/codec failure, a BDX abort, an
    /// unexpected OTA command, or if a session exhausts its step budget without
    /// an unsecured carry-frame; [`Error::Transport`] / [`Error::InteractionModel`]
    /// from the session / IM layers.
    #[cfg(feature = "ota")]
    #[allow(clippy::too_many_lines)] // Linear OTA protocol-dispatch loop; splitting hurts clarity.
    pub async fn serve_ota_once(
        mut self,
        offer: matter_ota::ImageOffer,
        image: Vec<u8>,
        max_block_size: u16,
    ) -> Result<(), Error> {
        use matter_bdx::{BdxMessage, BlockSender, MessageType, SenderOutcome};

        // Shared once so every `BlockSender` (the initial QueryImage arm and
        // any cross-session ReceiveInit re-arm below) clones the `Arc`
        // handle rather than the image bytes.
        let image: Arc<[u8]> = Arc::from(image);

        // Flow state spans sessions: the requestor downloads + applies on its
        // first session, REBOOTS into the image, and sends NotifyUpdateApplied
        // on a fresh session (usually resuming the record rotated during the
        // first accept — re-seeded by accept_case).
        let mut bdx: Option<BlockSender> = None;
        let mut carried: Option<(Vec<u8>, SocketAddr)> = None;

        // Outer: one iteration per CASE session; bounded by the credential
        // pool (accept_case errors when it is exhausted). A failed mid-flow
        // handshake poisons only that accept (spec: Error handling) — it
        // consumed one pooled credential, and the loop waits for the peer's
        // next attempt; only pool exhaustion (or the caller's deadline) ends
        // the serve.
        loop {
            let AcceptedSession {
                mut sessions,
                session_id: sid,
                peer,
                next_handshake: fast_sigma1,
                mut pending_secured,
            } = match self.accept_case(carried.take()).await {
                Ok(accepted) => accepted,
                Err(e) => {
                    if self.credentials.is_empty() {
                        return Err(e); // exhausted (or the last credential's failure)
                    }
                    continue; // retry with the next pooled credential
                }
            };
            if let Some(frame) = fast_sigma1 {
                // The peer opened a NEW handshake instead of acking this one's
                // close (fast post-reboot Sigma1 in place of the standalone
                // ack): the session just established is already abandoned —
                // roll the Sigma1 straight into the next accept rather than
                // blocking the inner loop on a dead session.
                carried = Some(frame);
                continue;
            }

            // Bound progress and iteration SEPARATELY. `max_progress`
            // caps how many transfer-ADVANCING messages (OTA commands + blocks)
            // we serve; a larger `max_iterations` backstop bounds frames that do
            // NOT advance the transfer (stale prior-session retransmits, the
            // duplicate-reliable ack resends handled below, a peer StatusReport).
            // Counting every frame against one budget — as the old `steps`
            // did — let a lossy mesh's retransmits exhaust it before the last
            // block arrived, turning a recoverable loss into a spurious failure.
            let max_progress = image.len() / usize::from(max_block_size.max(1)) + 64;
            let max_iterations = max_progress.saturating_mul(8).max(1024);
            let mut progress = 0usize;
            let mut iterations = 0usize;

            // Inner: serve this session until Notify (done), a new handshake
            // frame (roll into the next accept), or a bound.
            while progress < max_progress && iterations < max_iterations {
                iterations += 1;
                // A request the handshake close-out already took off the
                // socket is processed first: a real requestor pipelines its
                // QueryImage ahead of the standalone ack, and receiving before
                // handling it would lose the request.
                let (wire, from) = match pending_secured.take() {
                    Some(f) => f,
                    None => self.recv_secured(&mut sessions, peer).await?,
                };
                if is_unsecured_frame(&wire) {
                    carried = Some((wire, from));
                    break;
                }
                // A frame that fails secured decode is a stale leftover — e.g.
                // a late retransmit keyed to a PRIOR session's id after the
                // requestor re-established (the reboot window) — not a fault
                // of the live session. Skip it; the step budget bounds a
                // pathological stream of them.
                let Ok(decoded) = sessions.decode_inbound(&wire, Instant::now()) else {
                    continue;
                };
                let DecodeInboundOutput::AppMessage {
                    exchange_id,
                    is_initiator,
                    protocol_id,
                    opcode,
                    payload,
                    ..
                } = decoded
                else {
                    // The requestor retransmitted a reliable message
                    // (e.g. a BlockQuery) whose ack was lost. decode_inbound has
                    // pre-built the standalone ack to re-send — send it and do
                    // NOT advance BDX state (the block counter already moved).
                    // Dropping it here (the old `continue`) left the requestor
                    // retransmitting forever, stalling the transfer. Other
                    // non-app outcomes (AckOnly) need no response.
                    if let DecodeInboundOutput::DuplicateReliableAckResent { ack_packet, .. } =
                        decoded
                    {
                        self.send(&ack_packet, peer).await?;
                    }
                    continue;
                };

                // Only a message that ADVANCES the transfer (an OTA
                // invoke or a BDX message) counts against `max_progress`. A
                // stale/duplicate frame `continue`s above without reaching here,
                // so it burns only an `iterations` slot, never the progress
                // budget.
                let advanced = (protocol_id == ProtocolId::INTERACTION_MODEL
                    && opcode == OP_INVOKE_REQUEST)
                    || protocol_id == ProtocolId::BDX;

                if protocol_id == ProtocolId::INTERACTION_MODEL && opcode == OP_INVOKE_REQUEST {
                    let parsed = parse_invoke_request(&payload)?;
                    let cmd = parsed
                        .commands
                        .first()
                        .ok_or_else(|| Error::Operational("OTA invoke had no command".into()))?;
                    let response = if cmd.path.command == CMD_QUERY_IMAGE {
                        bdx = Some(BlockSender::from_shared(Arc::clone(&image), max_block_size));
                        let fields = matter_ota::handle_query_image(&cmd.fields_tlv, Some(&offer))
                            .map_err(|e| Error::Operational(format!("QueryImage: {e}")))?;
                        build_invoke_response_command(
                            CommandPath {
                                endpoint: 0,
                                cluster: OTA_PROVIDER_CLUSTER,
                                command: CMD_QUERY_IMAGE_RESPONSE,
                            },
                            &fields,
                        )
                    } else if cmd.path.command == CMD_APPLY_UPDATE_REQUEST {
                        let fields = matter_ota::handle_apply_update_request(&cmd.fields_tlv)
                            .map_err(|e| Error::Operational(format!("ApplyUpdateRequest: {e}")))?;
                        build_invoke_response_command(
                            CommandPath {
                                endpoint: 0,
                                cluster: OTA_PROVIDER_CLUSTER,
                                command: CMD_APPLY_UPDATE_RESPONSE,
                            },
                            &fields,
                        )
                    } else if cmd.path.command == CMD_NOTIFY_UPDATE_APPLIED {
                        matter_ota::parse_notify_update_applied(&cmd.fields_tlv)
                            .map_err(|e| Error::Operational(format!("NotifyUpdateApplied: {e}")))?;
                        let r = build_invoke_response_status(
                            CommandPath {
                                endpoint: 0,
                                cluster: OTA_PROVIDER_CLUSTER,
                                command: CMD_NOTIFY_UPDATE_APPLIED,
                            },
                            ImStatus::Success,
                        );
                        let out = sessions.encode_outbound_with_role(
                            sid,
                            exchange_id,
                            !is_initiator,
                            OP_INVOKE_RESPONSE,
                            ProtocolId::INTERACTION_MODEL,
                            &r,
                            MrpFlags { reliable: false },
                            Instant::now(),
                        )?;
                        self.send(&out.wire_bytes, peer).await?;
                        return Ok(());
                    } else {
                        return Err(Error::Operational(format!(
                            "unexpected OTA command {:#04x}",
                            cmd.path.command
                        )));
                    };
                    let out = sessions.encode_outbound_with_role(
                        sid,
                        exchange_id,
                        !is_initiator,
                        OP_INVOKE_RESPONSE,
                        ProtocolId::INTERACTION_MODEL,
                        &response,
                        MrpFlags { reliable: false },
                        Instant::now(),
                    )?;
                    self.send(&out.wire_bytes, peer).await?;
                } else if protocol_id == ProtocolId::SECURE_CHANNEL && opcode == OP_STATUS_REPORT {
                    // The requestor aborted via a Secure-Channel
                    // StatusReport — e.g. a device-side flash-write failure. End
                    // the transfer with a descriptive error naming the peer's
                    // status, instead of ignoring it and spinning to the "step
                    // budget exceeded" error (chip surfaces the peer status;
                    // TestBdxTransferSession.cpp:629).
                    let (general, proto, code) =
                        parse_status_report_body(&payload).ok_or_else(|| {
                            Error::Operational("BDX StatusReport body truncated".into())
                        })?;
                    return Err(Error::Operational(format!(
                        "BDX transfer aborted by peer: StatusReport general={general:#06x} \
                         protocol={proto:#010x} status={code:#06x}"
                    )));
                } else if protocol_id == ProtocolId::BDX {
                    let mt = MessageType::from_u8(opcode).ok_or_else(|| {
                        Error::Operational(format!("unknown BDX opcode {opcode:#04x}"))
                    })?;
                    let msg = BdxMessage::decode(mt, &payload)
                        .map_err(|e| Error::Operational(format!("BDX decode: {e}")))?;
                    // A `ReceiveInit` is a request to START a transfer. When a
                    // sender is already armed but mid-transfer, the requestor
                    // reconnected mid-download (reboot, link loss) and is
                    // re-initiating BDX from its cached `QueryImageResponse`
                    // URI without re-querying — re-arm and serve from the
                    // start rather than aborting the serve (tolerant choice:
                    // the image is static and the session authenticated — and
                    // peer-pinned under `with_expected_peer` — so re-serving
                    // the same bytes discloses nothing new). The DoS bound is
                    // preserved: BDX still NEVER starts before this serve's
                    // first `QueryImage` (`bdx` stays `None` until then), and
                    // the per-session step budget bounds a requestor that
                    // loops `ReceiveInit`.
                    if matches!(msg, BdxMessage::ReceiveInit(_)) && bdx.is_some() {
                        bdx = Some(BlockSender::from_shared(Arc::clone(&image), max_block_size));
                    }
                    let sender = bdx.as_mut().ok_or_else(|| {
                        Error::Operational("BDX message before QueryImage".into())
                    })?;
                    let outcome = match msg {
                        BdxMessage::ReceiveInit(init) => sender.accept_receive_init(&init),
                        BdxMessage::BlockQuery(q) => sender.handle_block_query(&q),
                        BdxMessage::BlockAckEof(a) => sender.handle_block_ack_eof(&a),
                        _ => {
                            return Err(Error::Operational("unexpected inbound BDX message".into()))
                        }
                    };
                    match outcome {
                        SenderOutcome::Send(out) => {
                            // Send every BDX message MRP-reliable so a
                            // lost block/ReceiveAccept is retransmitted (the
                            // recv_secured loop pumps the MRP retransmit timer).
                            // Over a lossy mesh (Thread) the first lost block
                            // otherwise stalls the transfer forever. chip sends
                            // every BDX message with kExpectResponse / never
                            // kNoAutoRequestAck (AsyncTransferFacilitator.cpp:127).
                            let w = sessions.encode_outbound_with_role(
                                sid,
                                exchange_id,
                                !is_initiator,
                                out.message_type.to_u8(),
                                ProtocolId::BDX,
                                &out.payload,
                                MrpFlags { reliable: true },
                                Instant::now(),
                            )?;
                            self.send(&w.wire_bytes, peer).await?;
                        }
                        SenderOutcome::Done => {}
                        SenderOutcome::Abort(code) => {
                            // Notify the peer with a Secure-Channel
                            // StatusReport before bailing, so the requestor learns
                            // the transfer failed instead of timing out. Best
                            // effort — the abort error is returned regardless.
                            let body = encode_status_report_body(
                                STATUS_GENERAL_FAILURE,
                                ProtocolId::BDX,
                                code.to_u16(),
                            );
                            if let Ok(w) = sessions.encode_outbound_with_role(
                                sid,
                                exchange_id,
                                !is_initiator,
                                OP_STATUS_REPORT,
                                ProtocolId::SECURE_CHANNEL,
                                &body,
                                MrpFlags { reliable: true },
                                Instant::now(),
                            ) {
                                let _ = self.send(&w.wire_bytes, peer).await;
                            }
                            return Err(Error::Operational(format!(
                                "BDX transfer aborted: status {:#06x}",
                                code.to_u16()
                            )));
                        }
                    }
                }

                if advanced {
                    progress += 1;
                }
            }
            if carried.is_none() {
                return Err(Error::Operational(format!(
                    "OTA session ended without completing: served {progress}/{max_progress} \
                     transfer-advancing messages in {iterations}/{max_iterations} iterations"
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;
    use std::net::Ipv6Addr;

    #[test]
    fn operational_service_has_expected_name_kind_and_port() {
        let compressed = [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE];
        let node_id = 0x0000_0000_0000_0001;
        let addr = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let svc = build_operational_service(compressed, node_id, vec![addr], 5540);

        assert_eq!(svc.kind, ServiceKind::Operational);
        assert_eq!(svc.port, 5540);
        // <16-hex compressed>-<16-hex node>, uppercase.
        assert_eq!(svc.instance_name, "DEADBEEFCAFEBABE-0000000000000001");
        assert_eq!(svc.addresses, vec![addr]);
    }

    #[cfg(feature = "ota")]
    #[test]
    fn status_report_body_byte_layout_and_roundtrip() {
        // A BDX abort StatusReport = Failure || BDX proto id (0x00000002)
        // || the 16-bit BDX status. Byte layout is little-endian per field
        // (Matter Core §4.11.6).
        let code = matter_bdx::BdxStatusCode::BadBlockCounter.to_u16(); // 0x0017
        let body = encode_status_report_body(STATUS_GENERAL_FAILURE, ProtocolId::BDX, code);
        assert_eq!(
            body,
            vec![0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x17, 0x00],
            "GeneralCode(LE) || ProtocolId(LE u32) || ProtocolStatus(LE)"
        );
        assert_eq!(
            parse_status_report_body(&body),
            Some((STATUS_GENERAL_FAILURE, 0x0000_0002, code))
        );
    }

    /// The unsecured handshake counter (Sigma2 / `Sigma2_Resume` / status
    /// reports) starts at a CSPRNG value in `[1, 2^28]` per server (Matter
    /// Core Spec §4.6.1.1, §4.6.1.2). Distinctness is what tells this apart
    /// from the old fixed start of 1: 8 draws from 2^28 values all being
    /// equal has probability 2^-196.
    #[test]
    fn handshake_counter_starts_at_a_random_value_in_the_spec_range() {
        let starts: Vec<u32> = (0..8)
            .map(|_| {
                let (io, _peer) = matter_commissioning::driver::InMemoryDatagram::pair();
                let mut server = ProviderServer::new(
                    io,
                    Vec::new(),
                    TrustedRoots::new(),
                    0x10,
                    MatterTime::from_unix_secs(2_000_000_000),
                );
                let first = server.next_handshake_counter();
                assert_eq!(server.next_handshake_counter(), first.wrapping_add(1));
                first
            })
            .collect();
        for c in &starts {
            assert!(
                (1..=1u32 << 28).contains(c),
                "handshake counter {c:#x} outside [1, 2^28]"
            );
        }
        assert!(
            starts.iter().any(|c| *c != starts[0]),
            "8 provider servers all started at {:#x}: the counter is not CSPRNG-seeded",
            starts[0]
        );
    }

    #[test]
    fn status_report_body_rejects_truncated() {
        assert_eq!(parse_status_report_body(&[0x01, 0x00, 0x02]), None);
        assert_eq!(parse_status_report_body(&[]), None);
    }

    /// `is_unsecured_frame` returns true for session id 0 (unsecured), false
    /// for a non-zero session id (secured), and false for a short slice.
    #[test]
    fn is_unsecured_frame_classifies_correctly() {
        // True: encode_unsecured_reply always sets session id to 0.
        let unsecured = encode_unsecured_reply(
            1,
            1,
            0x30,
            ProtocolId::SECURE_CHANNEL,
            false,
            None,
            None,
            &[],
        );
        assert!(
            is_unsecured_frame(&unsecured),
            "unsecured reply must have session id 0"
        );

        // False: hand-built frame with session id 0x1234 (LE at bytes[1..3]).
        let secured = vec![0x00u8, 0x34, 0x12, 0x00, 0x00, 0x00];
        assert!(
            !is_unsecured_frame(&secured),
            "non-zero session id must not be classified as unsecured"
        );

        // False: slice shorter than 3 bytes.
        assert!(
            !is_unsecured_frame(&[0x00u8, 0x00]),
            "2-byte slice must return false"
        );
    }

    /// An empty credential pool must fail fast (before any IO) with the
    /// canonical error message. This exercises the pool-exhaustion guard in
    /// `accept_case` without requiring a real CASE peer.
    #[cfg(feature = "ota")]
    #[tokio::test]
    async fn empty_credential_pool_errors_before_any_io() {
        let (io, _peer) = matter_commissioning::driver::InMemoryDatagram::pair();
        let server = ProviderServer::new(
            io,
            Vec::new(),
            TrustedRoots::new(),
            0x10,
            MatterTime::from_unix_secs(2_000_000_000),
        );
        let offer = matter_ota::ImageOffer {
            software_version: 2,
            software_version_string: "2.0".into(),
            image_uri: "bdx://0/fw.ota".into(),
            update_token: vec![0xAB; 16],
        };
        let err = server
            .serve_ota_once(offer, vec![0u8; 16], 960)
            .await
            .expect_err("empty pool must fail fast");
        assert!(
            err.to_string().contains("credential pool exhausted"),
            "unexpected error: {err}"
        );
    }
}
