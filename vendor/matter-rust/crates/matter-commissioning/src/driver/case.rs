//! CASE bridge (M6.6.3b): operational discovery + drive the sans-IO
//! `CaseInitiator` over the unsecured datagram path.
//!
//! CASE Sigma1/2/3 are exchanged UNSECURED (session-id 0, `SecureChannel`
//! protocol) — the operational secured session only exists once the handshake
//! derives keys.

use std::collections::BTreeSet;
use std::net::SocketAddr;

use matter_cert::{MatterTime, TrustedRoots};
use matter_crypto::{CaseCredentials, CaseInitiator, ResumptionRecord, SessionParameters};
use matter_transport::{Discovery, MrpConfig, ServiceKind, SessionId, SessionManager, SessionRole};

use crate::driver::datagram::AsyncDatagram;
use crate::driver::error::DriverError;
use crate::driver::unsecured::{
    parse_status_report, random_exchange_id, require_handshake_opcode_any, UnsecuredExchange,
};
use crate::driver::TransportReliability;

/// Build the operational mDNS instance name `<compressed-fabric-id>-<node-id>`,
/// each as fixed-width uppercase hex (16 + 1 + 16 chars), per the Matter
/// operational-discovery instance-name convention.
///
/// FLAGGED: confirm exact casing/width/separator against matter.js byte parity
/// before the first real-device CASE (M6.6.5); this matches the connectedhomeip
/// convention and the in-tree examples.
#[must_use]
pub fn operational_instance_name(compressed_fabric_id: [u8; 8], node_id: u64) -> String {
    let cfid = u64::from_be_bytes(compressed_fabric_id);
    format!("{cfid:016X}-{node_id:016X}")
}

/// How many times [`resolve_operational`] polls discovery before giving up, and
/// the gap between polls (~30 s total) — bounded so the driver doesn't hang
/// forever. The operational record only appears after `AddNOC`, so mDNS
/// propagation can take several seconds on a real LAN (observed during M6.6.5
/// validation); chip's session-establishment discovery budget is of the same
/// order. `pub(crate)` so `commission()` (the IP path) can pass this exact
/// budget into [`resolve_operational_with_attempts`] via `establish_case_session`.
pub(crate) const RESOLVE_POLL_ATTEMPTS: u32 = 300;
const RESOLVE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Operational-resolve budget for the BLE (BTP) commissioning path: ~60 s at
/// `RESOLVE_POLL_INTERVAL` (100 ms) per poll. Over BLE the device has only
/// just been handed Wi-Fi credentials when `commission_ble` reaches
/// `EstablishCase`, so it must still associate to the AP, obtain a DHCP lease,
/// and announce its `_matter._tcp` operational record before the resolve can
/// succeed — a materially longer window than the IP path's ~30 s.
pub const BLE_RESOLVE_POLL_ATTEMPTS: u32 = 600;

/// Pick the most routable address from an mDNS record's address list: the first
/// IPv4, else the first non-link-local IPv6, else the first address of any kind
/// (`None` only for an empty list).
///
/// A `fe80::` IPv6 needs an interface scope id that
/// [`MatterService`](matter_transport::MatterService) does not carry, so a
/// dial-out socket cannot route to it — devices often list it FIRST, ahead of
/// perfectly routable addresses (M6.6.5: closes the previously FLAGGED
/// `.first()` pick).
///
/// Public so a caller that drives its own non-blocking discovery poll — the
/// controller actor resolves operational records on its timer arm rather than
/// inside [`resolve_operational`] — picks the *same* address this crate's
/// resolvers would.
#[must_use]
pub fn preferred_address(addresses: &[std::net::IpAddr]) -> Option<std::net::IpAddr> {
    let is_v6_link_local = |a: &std::net::IpAddr| match a {
        std::net::IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
        std::net::IpAddr::V4(_) => false,
    };
    addresses
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addresses.iter().find(|a| !is_v6_link_local(a)))
        .or_else(|| addresses.first())
        .copied()
}

/// Browse `_matter._tcp` operational records and return the socket address of
/// the node whose instance name matches `(compressed_fabric_id, node_id)`.
///
/// The advertised address list is filtered for routability
/// (IPv4 → non-link-local IPv6 → fallback) — see `preferred_address`.
///
/// # Errors
///
/// - [`DriverError::Transport`] if the discovery query fails.
/// - [`DriverError::Discovery`] if no matching record with an address appears
///   within the poll budget.
pub async fn resolve_operational<D: Discovery>(
    discovery: &mut D,
    compressed_fabric_id: [u8; 8],
    node_id: u64,
) -> Result<SocketAddr, DriverError> {
    // The IP path's ~30 s budget. Delegates to the parameterized form so the
    // BLE path can raise the budget without duplicating the poll loop.
    resolve_operational_with_attempts(
        discovery,
        compressed_fabric_id,
        node_id,
        RESOLVE_POLL_ATTEMPTS,
    )
    .await
}

/// Like [`resolve_operational`], but with an explicit poll-attempt budget
/// (each poll is spaced by `RESOLVE_POLL_INTERVAL` = 100 ms). The IP path uses
/// [`resolve_operational`] (`RESOLVE_POLL_ATTEMPTS` = 300, ~30 s); the BLE path
/// passes [`BLE_RESOLVE_POLL_ATTEMPTS`] (600, ~60 s) because a just-provisioned
/// device needs longer to associate to Wi-Fi and announce its operational
/// record.
///
/// # Errors
///
/// - [`DriverError::Transport`] if the discovery query fails.
/// - [`DriverError::Discovery`] if no matching record with an address appears
///   within the `attempts` budget.
pub async fn resolve_operational_with_attempts<D: Discovery>(
    discovery: &mut D,
    compressed_fabric_id: [u8; 8],
    node_id: u64,
    attempts: u32,
) -> Result<SocketAddr, DriverError> {
    resolve_operational_service(discovery, compressed_fabric_id, node_id, attempts)
        .await
        .map(|(addr, _mrp)| addr)
}

/// Like [`resolve_operational`], but also returns the peer's advertised MRP
/// retransmit config (from its operational mDNS TXT `SII`/`SAI`/`SAT`) so the
/// caller can size retransmits to the device — critical for not hammering a
/// sleepy device. Uses the IP-path poll budget.
///
/// # Errors
///
/// As [`resolve_operational`].
pub async fn resolve_operational_with_mrp<D: Discovery>(
    discovery: &mut D,
    compressed_fabric_id: [u8; 8],
    node_id: u64,
) -> Result<(SocketAddr, MrpConfig), DriverError> {
    resolve_operational_service(
        discovery,
        compressed_fabric_id,
        node_id,
        RESOLVE_POLL_ATTEMPTS,
    )
    .await
}

/// Label for the browse a record came from, used in the resolve's diagnostics.
/// `subtype` is the fabric-scoped `_I<id>._sub._matter._tcp` browse, `base` the
/// unfiltered `_matter._tcp` one.
const BROWSE_SUBTYPE: &str = "subtype";
const BROWSE_BASE: &str = "base";

/// How long the compressed-fabric subtype browse runs **alone** before the
/// base-type `_matter._tcp` browse is opened behind it as a fallback.
///
/// The two browses must not run concurrently from the start. The underlying
/// defect (keepsimple1/mdns-sd#493) is that the resolver completes per-instance
/// SRV/address resolution roughly one instance per query cycle, from a queue
/// shared by every open browse. The subtype helps *only* because it limits how
/// many instances enter that queue — 3 instead of 18 on the reporter's network.
/// Opening the base type alongside it re-discovers all 18 and puts our nodes
/// back at the end of the same slow queue, which is exactly what the first
/// attempt at this fix did: the reporter's log shows the subtype browse
/// surfacing *nothing* in 30 s, with every record credited to the base browse.
///
/// Two seconds is chosen from both ends:
/// - **Long enough** that a healthy subtype responder always answers first. The
///   reporter's three nodes resolved through the subtype in ~266 ms, ours in
///   ~250 ms; 2 s is roughly 8x that, and covers an initial query that has to be
///   retransmitted once.
/// - **Short enough** to be noise against the resolve budget. It costs 2 s of a
///   ~30 s window ([`RESOLVE_POLL_ATTEMPTS`]), leaving ~28 s for a responder
///   that genuinely publishes no subtype PTR — far more than such a responder
///   needs, since it is discovered on the base browse from that browse's first
///   query cycle.
const SUBTYPE_ONLY_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// [`SUBTYPE_ONLY_WINDOW`] expressed in poll iterations of the resolve loop,
/// which is how the delay is actually applied: the fallback is opened from the
/// existing loop rather than from a timer or a second task, so the resolve stays
/// a single flat sequence with no concurrency of its own.
///
/// `u128` only because that is what `Duration::as_millis` returns; the loop
/// counter is widened to compare against it, rather than this being narrowed
/// with a lossy cast.
const SUBTYPE_ONLY_POLLS: u128 =
    SUBTYPE_ONLY_WINDOW.as_millis() / RESOLVE_POLL_INTERVAL.as_millis();

/// Shared resolve loop returning both the preferred address and the peer's
/// parsed MRP config. Both public resolvers delegate here.
///
/// # The subtype first, the base type only as a delayed fallback
///
/// This opens the fabric's compressed-fabric subtype browse
/// ([`Discovery::query_operational_fabric`],
/// `_I<compressed-fabric-id>._sub._matter._tcp.local.`) and, for the first
/// [`SUBTYPE_ONLY_WINDOW`], **nothing else**. Only if no record has matched by
/// then is the plain `_matter._tcp` browse opened behind it; from that point
/// both are polled and whichever delivers the record first settles the resolve.
///
/// The subtype is what makes this fast and reliable on a busy link: it narrows
/// the browse to the nodes of *our* fabric, so a resolver that works through
/// discovered instances one at a time with exponential backoff (mdns-sd does)
/// reaches our node immediately instead of possibly never. Issue #113 is that
/// failure — 18 base-type instances, one resolved per query cycle, and the
/// wanted nodes still unresolved when the ~30 s budget expired; the same three
/// nodes resolved in ~266 ms through the subtype.
///
/// The delay is the whole point, and it is what the first attempt at this fix
/// got wrong. That attempt opened both browses at once, reasoning that an extra
/// browse "cannot lose". It can: the narrowing *is* the fix, and the base-type
/// browse un-narrows it. Re-discovering all 18 instances feeds them back into
/// the resolver's shared, one-per-cycle resolution queue, so our nodes are
/// starved exactly as before — the reporter's log of that build shows the
/// subtype browse surfacing nothing at all in 30 s, with every surfaced record
/// carrying `subtype_browse=false`.
///
/// The base type is nevertheless kept as a *fallback*, because a subtype browse
/// narrows: a responder that (wrongly, per Matter Core Spec §4.3.1, but
/// observably) publishes no subtype PTR would be invisible to it, and regressing
/// from "slow" to "finds nothing" would be far worse than the bug being fixed.
/// Deferring it buys the healthy case its narrow browse while still resolving
/// the subtype-less responder inside the budget.
///
/// A [`Discovery`] implementation that does not override
/// `query_operational_fabric` gets the trait default (a base-type browse), which
/// may hand back the *same* handle twice; equal handles are deduplicated here so
/// such an implementation is polled exactly as often as before.
///
/// Opening a browse can fail (daemon down). If the *subtype* browse cannot be
/// opened the base type is opened immediately instead of after the window —
/// there is nothing to wait for. The resolve continues on whichever browse did
/// open, and fails outright only if both fail.
async fn resolve_operational_service<D: Discovery>(
    discovery: &mut D,
    compressed_fabric_id: [u8; 8],
    node_id: u64,
    attempts: u32,
) -> Result<(SocketAddr, MrpConfig), DriverError> {
    let target = operational_instance_name(compressed_fabric_id, node_id);

    // (label, handle) for every browse to poll. Order matters only for which
    // label a simultaneous hit is credited to; the subtype is polled first
    // because it is the one expected to answer.
    let mut browses: Vec<(&'static str, matter_transport::QueryHandle)> = Vec::new();
    let mut open_error: Option<DriverError> = None;
    match discovery.query_operational_fabric(compressed_fabric_id) {
        Ok(h) => browses.push((BROWSE_SUBTYPE, h)),
        Err(e) => open_error = Some(DriverError::Transport(e)),
    }
    // Whether the base-type browse still has to be opened. It is deliberately
    // NOT opened here when the subtype browse is up: see `SUBTYPE_ONLY_WINDOW`
    // — a concurrent base browse re-floods the resolver's shared resolution
    // queue and starves the very nodes the subtype narrowed down to. It is
    // opened from the poll loop below, once the window has passed with no match.
    //
    // With no subtype browse there is nothing to wait for, so open it at once.
    let mut base_pending = true;
    if browses.is_empty() {
        base_pending = false;
        match discovery.query(ServiceKind::Operational) {
            Ok(h) => browses.push((BROWSE_BASE, h)),
            Err(e) => open_error = Some(DriverError::Transport(e)),
        }
        if browses.is_empty() {
            // Both browses failed to open — surface the daemon error rather
            // than spinning out the budget against nothing.
            return Err(open_error.unwrap_or_else(|| {
                DriverError::Discovery("no mDNS browse could be opened".to_string())
            }));
        }
    }

    // Every operational instance name this search observed, so a failure can
    // say whether the browse produced anything at all (see
    // `discovery_failure_message`). Sorted + deduplicated by the set; capped at
    // `SEEN_TRACK_CAP` so a large or hostile network cannot grow it without
    // bound. Purely diagnostic — nothing below reads it to decide anything.
    let mut seen: BTreeSet<String> = BTreeSet::new();

    let mut found: Option<(SocketAddr, MrpConfig, &'static str)> = None;
    'search: for attempt in 0..attempts {
        // The delayed fallback, driven from this loop rather than a timer or a
        // second task: the subtype has had `SUBTYPE_ONLY_WINDOW` to itself and
        // produced no match, so this responder may be one that publishes no
        // subtype PTR. Open the base type behind it and poll both from here on.
        //
        // Attempted exactly once (`base_pending` is cleared regardless of
        // outcome): a daemon that refuses the browse must not be re-asked on
        // every one of the remaining poll iterations.
        if base_pending && u128::from(attempt) >= SUBTYPE_ONLY_POLLS {
            base_pending = false;
            match discovery.query(ServiceKind::Operational) {
                // Deduplicate: the trait default for `query_operational_fabric`
                // IS a base-type browse, and an implementation may return the
                // same handle for both. Polling one handle twice would consume
                // records twice over.
                Ok(h) if !browses.iter().any(|(_, existing)| *existing == h) => {
                    browses.push((BROWSE_BASE, h));
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        instance = %target,
                        after_secs = SUBTYPE_ONLY_WINDOW.as_secs(),
                        "no subtype match after {}s; opening the base-type \
                         _matter._tcp browse as a fallback",
                        SUBTYPE_ONLY_WINDOW.as_secs(),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    // Not fatal: the subtype browse is still running, and it is
                    // the one expected to answer.
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        instance = %target,
                        error = %e,
                        "base-type fallback browse could not be opened; \
                         continuing on the subtype browse alone",
                    );
                    // Consumed only by the (feature-gated) trace above.
                    let _ = &e;
                }
            }
        }
        for (label, handle) in &browses {
            for svc in discovery.poll_results(*handle) {
                if svc.instance_name.eq_ignore_ascii_case(&target) {
                    if let Some(addr) = preferred_address(&svc.addresses) {
                        found = Some((
                            SocketAddr::new(addr, svc.port),
                            svc.peer_mrp_config(),
                            *label,
                        ));
                        break 'search;
                    }
                }
                if seen.len() < SEEN_TRACK_CAP {
                    seen.insert(svc.instance_name);
                }
            }
        }
        tokio::time::sleep(RESOLVE_POLL_INTERVAL).await;
    }

    for (_, handle) in &browses {
        discovery.stop_query(*handle);
    }

    if let Some((addr, mrp, browse)) = found {
        #[cfg(feature = "tracing")]
        tracing::debug!(
            instance = %target,
            peer = %addr,
            browse,
            "operational record resolved",
        );
        // Consumed only by the (feature-gated) trace above.
        let _ = browse;
        return Ok((addr, mrp));
    }

    let names: Vec<&str> = seen.iter().map(String::as_str).collect();
    Err(DriverError::Discovery(discovery_failure_message(
        &target, &names,
    )))
}

/// How many distinct instance names one resolve tracks for diagnostics.
const SEEN_TRACK_CAP: usize = 64;

/// How many observed instance names a discovery-failure message quotes.
const SEEN_SAMPLE_MAX: usize = 5;

/// Character bound on each quoted instance name. A Matter operational instance
/// name is 33 characters (`<16 hex>-<16 hex>`); anything longer is a foreign
/// record and is elided, so no advertisement on the network can make our error
/// message unbounded.
const SEEN_NAME_MAX: usize = 48;

/// Build the "not found" text for a failed operational resolve, appending a
/// bounded summary of the operational records that *were* seen.
///
/// The `not found via mDNS` substring is preserved verbatim — callers match on
/// it. What follows is diagnosis: a non-empty `seen` means the browse works and
/// this particular node was absent from it (device offline, different fabric,
/// stale node id). `seen` is expected pre-sorted for a stable message.
///
/// An **empty** `seen` is deliberately worded as the weaker claim it is. It
/// means nothing was *counted*, which covers both "no `_matter._tcp` response
/// reached this host" (firewall, no multicast on the interface, wrong network)
/// and "responses arrived and were discarded upstream of the count" — the mDNS
/// adapter drops a record whose `ty_domain` it does not recognise (a Matter
/// `_I<fabric>._sub._matter._tcp` subtype, say) or that resolved with no
/// addresses, and nothing it drops ever reaches this function. The
/// record-by-record trace under `matter_transport::mdns` is what separates the
/// two.
///
/// Bounded on both axes ([`SEEN_SAMPLE_MAX`] names, [`SEEN_NAME_MAX`]
/// characters each) so the message cannot grow with the size of the network.
///
/// A sibling of this lives in `matter_controller`'s actor, which produces the
/// same error from the parked (steady-state) resolve path; the two are
/// deliberately duplicated rather than shared through new public API. The
/// *text* they produce is identical, but the populations they count are **not**:
/// this resolver counts every record its poll returned, while the actor counts
/// only those that survived address selection into its `seen_records` cache. So
/// the two counts are not comparable, and neither is a measure of what arrived
/// on the wire.
fn discovery_failure_message(target: &str, seen: &[&str]) -> String {
    use std::fmt::Write as _;

    let mut msg = format!("operational node {target} not found via mDNS");
    if seen.is_empty() {
        msg.push_str(
            " (saw 0 operational mDNS records — either no _matter._tcp response \
             reached this host, or responses arrived and were discarded before \
             being counted; RUST_LOG=matter_transport::mdns=debug distinguishes \
             the two)",
        );
        return msg;
    }
    // Writing to a String is infallible; the Result exists only to satisfy the
    // `fmt::Write` signature.
    let _ = write!(
        msg,
        " (saw {} operational mDNS record(s), none matching: ",
        seen.len()
    );
    for (i, name) in seen.iter().take(SEEN_SAMPLE_MAX).enumerate() {
        if i > 0 {
            msg.push_str(", ");
        }
        if name.chars().count() > SEEN_NAME_MAX {
            msg.extend(name.chars().take(SEEN_NAME_MAX));
            msg.push('…');
        } else {
            msg.push_str(name);
        }
    }
    if seen.len() > SEEN_SAMPLE_MAX {
        let _ = write!(msg, ", … {} more", seen.len() - SEEN_SAMPLE_MAX);
    }
    msg.push(')');
    msg
}

// SecureChannel opcodes for the CASE handshake (Matter Core Spec §4.14.1).
const OP_SIGMA1: u8 = 0x30;
const OP_SIGMA2: u8 = 0x31;
const OP_SIGMA3: u8 = 0x32;
/// `Sigma2_Resume` — the responder's reply when it accepts resumption. Sent in
/// place of `Sigma2`, never in addition to it.
const OP_SIGMA2_RESUME: u8 = 0x33;
/// `SecureChannel` `StatusReport` opcode (spec §4.10.1.1) — the frame the
/// device sends to close the handshake after the terminal `Sigma3`.
const OP_STATUS_REPORT: u8 = 0x40;

/// Drive a fresh CASE (SIGMA-I) handshake against an already-resolved
/// operational `peer` and register the resulting operational session, returning
/// its local [`SessionId`]. `credentials` is this controller's operational
/// identity; `peer_node_id`/`peer_fabric_id` identify the device. Resumption is
/// not used (a fresh handshake every time); that is M8.
///
/// `now` is the wall-clock instant against which the device's operational
/// certificate chain is checked for temporal validity during Sigma2. The caller
/// (controller layer) supplies the real time; this crate never reads the system
/// clock.
///
/// # Errors
///
/// - [`DriverError::Crypto`] if a SIGMA step fails (peer chain/signature
///   invalid, key mismatch, etc.).
/// - [`DriverError::Io`] / [`DriverError::Transport`] / [`DriverError::Timeout`]
///   on datagram, framing, or reply-timeout failure.
// 8 params: the CASE setup (transport, sessions, peer, credentials, roots,
// node/fabric ids) plus the injected validation clock; bundling them would
// obscure the call site.
#[allow(clippy::too_many_arguments)]
pub async fn run_case<T: AsyncDatagram>(
    transport: &T,
    sessions: &mut SessionManager,
    peer: SocketAddr,
    credentials: CaseCredentials,
    trusted_roots: TrustedRoots,
    peer_node_id: u64,
    peer_fabric_id: u64,
    now: MatterTime,
) -> Result<SessionId, DriverError> {
    // Allocate the local session id up front (advertised in Sigma1), run the
    // handshake, then register the established output. `run_case_establish` is
    // the handshake body without the `SessionManager` coupling; keeping the
    // allocate+register here preserves this function's exact behavior for its
    // existing callers.
    let local = sessions.allocate_session_id();
    // `Box::pin` the extracted handshake future so it lives on the heap rather
    // than being embedded inline in this (and every transitive caller's, e.g.
    // `commission`) future. Without this, nesting `run_case_establish`'s frame
    // inside `run_case` grows the commissioning future past clippy's
    // `large_futures` threshold. CASE connect is not a hot path, so the single
    // allocation is negligible.
    let output = Box::pin(run_case_establish(
        transport,
        peer,
        local.0,
        credentials,
        trusted_roots,
        peer_node_id,
        peer_fabric_id,
        now,
        // No discovery result in hand on this path, so the spec defaults.
        MrpConfig::for_peer(None, None, None),
    ))
    .await?;
    let sid = sessions.register_case(&output, SessionRole::Initiator);
    Ok(sid)
}

/// Everything a CASE connect needs beyond the transport and the peer address.
///
/// Exists because `run_case_establish` had already grown to nine positional
/// arguments under `#[allow(clippy::too_many_arguments)]`, and resumption plus
/// our own session-parameter advertisement would have made it eleven. Future
/// knobs land here instead of extending that list again.
///
/// `#[non_exhaustive]`: build with [`new`](Self::new) and the `with_*` setters.
#[non_exhaustive]
pub struct CaseEstablishOptions {
    /// Local session id to advertise in Sigma1. The caller allocates it so it
    /// can register the finished session under the same id.
    pub local_session_id: u16,
    /// This controller's operational identity on the fabric.
    pub credentials: CaseCredentials,
    /// Roots the device's operational chain is validated against.
    pub trusted_roots: TrustedRoots,
    /// The device's node id.
    pub peer_node_id: u64,
    /// The fabric both sides are on.
    pub peer_fabric_id: u64,
    /// Wall clock the device's certificate chain is checked against. This crate
    /// never reads the system clock.
    pub now: MatterTime,
    /// Retransmit sizing to start from — normally the peer's operational mDNS
    /// TXT `SII`/`SAI`/`SAT`. Refined mid-handshake from the device's own
    /// `SessionParameters` if it sends any.
    pub peer_mrp: MrpConfig,
    /// A stored record from a previous session with this peer. When present,
    /// Sigma1 carries resumption fields and the responder may answer with
    /// `Sigma2_Resume` — or decline, in which case the full handshake proceeds
    /// with no extra round trip.
    pub resumption_record: Option<ResumptionRecord>,
    /// What we advertise about ourselves in Sigma1 (context tag 5).
    pub local_session_params: Option<SessionParameters>,
}

impl CaseEstablishOptions {
    /// The required inputs. Optional behaviour is opted into with the `with_*`
    /// setters; the defaults reproduce a plain, non-resumed handshake that
    /// advertises nothing, sized to the spec defaults.
    #[must_use]
    pub fn new(
        local_session_id: u16,
        credentials: CaseCredentials,
        trusted_roots: TrustedRoots,
        peer_node_id: u64,
        peer_fabric_id: u64,
        now: MatterTime,
    ) -> Self {
        Self {
            local_session_id,
            credentials,
            trusted_roots,
            peer_node_id,
            peer_fabric_id,
            now,
            peer_mrp: MrpConfig::for_peer(None, None, None),
            resumption_record: None,
            local_session_params: None,
        }
    }

    /// Size the handshake's retransmits to the peer's advertised MRP config.
    #[must_use]
    pub fn with_peer_mrp(mut self, peer_mrp: MrpConfig) -> Self {
        self.peer_mrp = peer_mrp;
        self
    }

    /// Attempt resumption using `record`, falling back to a full handshake if
    /// the responder declines.
    #[must_use]
    pub fn with_resumption_record(mut self, record: ResumptionRecord) -> Self {
        self.resumption_record = Some(record);
        self
    }

    /// Advertise `params` about ourselves in Sigma1.
    #[must_use]
    pub fn with_session_params(mut self, params: SessionParameters) -> Self {
        self.local_session_params = Some(params);
        self
    }
}

/// The result of a completed CASE connect.
///
/// `#[non_exhaustive]`: more may be reported as the handshake learns more about
/// the peer.
///
/// `Debug` comes from `CaseSessionOutput`, whose own impl redacts the session
/// keys — nothing here can print key material.
#[derive(Debug)]
#[non_exhaustive]
pub struct CaseEstablished {
    /// Session keys and both identities.
    pub output: matter_crypto::CaseSessionOutput,
    /// Whether the fast path was taken (`Sigma2_Resume`) rather than the full
    /// three-message handshake. False when no record was supplied *and* when
    /// one was supplied but the responder declined it.
    pub resumed: bool,
    /// The device's own `SessionParameters`, if it sent any. More trustworthy
    /// than the mDNS TXT record: an advertisement can be stale, cached, or
    /// absent. Feed it to `MrpConfig::merge_session_params` when registering
    /// the operational session.
    pub peer_session_params: Option<SessionParameters>,
}

/// Drive a fresh CASE (SIGMA-I) handshake to completion over `transport` and
/// return the established [`CaseSessionOutput`](matter_crypto::CaseSessionOutput)
/// **without registering it** — so the caller can register it into its own
/// [`SessionManager`] (e.g. after a spawned, own-socket handshake that hands the
/// session back to a controller actor; see M9-G-d). `local_session_id` is the
/// local session id to advertise in Sigma1 (the caller allocates it).
///
/// [`run_case`] is `allocate_session_id` + this function + `register_case`; the
/// two share this body so their handshake behavior is identical.
///
/// # Errors
///
/// - [`DriverError::Crypto`] if a SIGMA step fails (peer chain/signature
///   invalid, key mismatch, etc.).
/// - [`DriverError::Io`] / [`DriverError::Transport`] / [`DriverError::Timeout`]
///   on datagram, framing, or reply-timeout failure.
/// - [`DriverError::SessionEstablishmentFailed`] if the device closes the
///   handshake with a non-success `StatusReport`.
// Same CASE setup as `run_case`, plus an explicit `local_session_id`
// in place of the `SessionManager` this variant does not touch.
// Same CASE setup as `run_case`, plus an explicit `local_session_id`
// in place of the `SessionManager` this variant does not touch.
#[allow(clippy::too_many_arguments)]
pub async fn run_case_establish<T: AsyncDatagram>(
    transport: &T,
    peer: SocketAddr,
    local_session_id: u16,
    credentials: CaseCredentials,
    trusted_roots: TrustedRoots,
    peer_node_id: u64,
    peer_fabric_id: u64,
    now: MatterTime,
    peer_mrp: MrpConfig,
) -> Result<matter_crypto::CaseSessionOutput, DriverError> {
    let opts = CaseEstablishOptions::new(
        local_session_id,
        credentials,
        trusted_roots,
        peer_node_id,
        peer_fabric_id,
        now,
    )
    .with_peer_mrp(peer_mrp);
    Ok(run_case_establish_with(transport, peer, opts).await?.output)
}

/// Drive a CASE handshake to completion over `transport`, with resumption and
/// our own session-parameter advertisement available via
/// [`CaseEstablishOptions`], and **without registering** the result.
///
/// [`run_case_establish`] is this function with the options left at their
/// defaults; the two share this body so their behaviour cannot diverge.
///
/// # The two paths, and why the fallback is free
///
/// When `opts.resumption_record` is set, Sigma1 carries `resumptionId` +
/// `initiatorResumeMIC` and the responder may reply with either:
///
/// - `Sigma2_Resume` (0x33) — resumption accepted. There is no Sigma3 on this
///   path; the handshake ends with a success `StatusReport` from us.
/// - `Sigma2` (0x31) — declined. The full three-message handshake proceeds
///   exactly as if resumption had never been attempted.
///
/// The decline costs nothing extra because Sigma1 already carried a real
/// ephemeral public key and `destinationId` — resumption only *adds* two
/// optional fields — and because the transcript hashes the Sigma1 bytes as
/// actually emitted, resumption fields included. Both reference
/// implementations rely on the same property (chip hashes at
/// `CASESession.cpp:800` before the responder's decision is known).
///
/// Routing is on the **received** opcode, never on the state machine's
/// `expected_inbound()` hint: calling `handle_sigma2_resume` when no resumption
/// was attempted poisons the machine unrecoverably.
///
/// # When resumption is attempted
///
/// Whenever a record is supplied — no capability probe, no age check, no
/// backoff. That is what both references do (`CASESession::SendSigma1`,
/// matter.js `CaseClient.#doPair`); the policy of *whether* to keep and supply
/// a record belongs to the caller that owns the store.
///
/// # Errors
///
/// - [`DriverError::Crypto`] if a SIGMA step fails. In particular
///   [`matter_crypto::Error::ResumptionMacMismatch`] means the responder
///   accepted our resumption id but proved a different shared secret: the
///   stored record is unusable and the caller should drop it and reconnect.
/// - [`DriverError::Io`] / [`DriverError::Transport`] / [`DriverError::Timeout`]
///   on datagram, framing, or reply-timeout failure.
/// - [`DriverError::SessionEstablishmentFailed`] if the device closes the
///   handshake with a non-success `StatusReport`.
// Both SIGMA paths are kept in one function on purpose: the resumed and full
// branches share the exchange, the Sigma1 that opened it, and the re-sizing
// step, and a reviewer must be able to see that the decline path reuses the
// SAME ephemeral key and the SAME emitted Sigma1 bytes without chasing a
// helper. Splitting on that boundary would hide exactly what makes the
// fallback correct.
#[allow(clippy::too_many_lines)]
pub async fn run_case_establish_with<T: AsyncDatagram>(
    transport: &T,
    peer: SocketAddr,
    opts: CaseEstablishOptions,
) -> Result<CaseEstablished, DriverError> {
    let CaseEstablishOptions {
        local_session_id,
        credentials,
        trusted_roots,
        peer_node_id,
        peer_fabric_id,
        now,
        peer_mrp,
        resumption_record,
        local_session_params,
    } = opts;

    let resuming = resumption_record.is_some();
    let mut initiator = match resumption_record {
        Some(record) => CaseInitiator::new_with_resumption(
            credentials,
            trusted_roots,
            peer_node_id,
            peer_fabric_id,
            record,
            local_session_id,
            now,
        )?,
        None => CaseInitiator::new(
            credentials,
            trusted_roots,
            peer_node_id,
            peer_fabric_id,
            local_session_id,
            now,
        )?,
    };
    if let Some(params) = local_session_params {
        initiator = initiator.with_session_params(params);
    }

    // CSPRNG-seeded counter + ephemeral source node id (spec §4.6.1.1,
    // §4.13.2.1) — same unsecured-header requirements as PASE apply to SIGMA.
    // Retransmits are sized to the peer's advertised SII: the operational layer
    // has always honoured it, but the handshake that gates the operational
    // layer did not, so a sleepy device was given a fixed 1.5 s to wake.
    let mut exch = UnsecuredExchange::new_ephemeral_with(
        random_exchange_id()?,
        TransportReliability::Mrp,
        peer_mrp,
    )?;

    // A resumption Sigma1 admits either reply. Widening the gate is not
    // cosmetic: the exchange DROPS frames whose opcode is unexpected, so an
    // inbound Sigma2_Resume would otherwise never surface and the connect would
    // end in a timeout that looks nothing like "the peer accepted resumption".
    let expected: &[u8] = if resuming {
        &[OP_SIGMA2, OP_SIGMA2_RESUME]
    } else {
        &[OP_SIGMA2]
    };

    let sigma1 = initiator.start()?;
    let sigma2 = exch
        .send_and_recv_any(transport, peer, OP_SIGMA1, expected, &sigma1, None)
        .await?;
    if let Err(e) = require_handshake_opcode_any(&sigma2, expected) {
        // Best-effort ack so a rejecting device stops retransmitting its
        // (reliable) StatusReport before we abort.
        let _ = exch
            .send_standalone_ack(transport, peer, sigma2.message_counter)
            .await;
        return Err(e);
    }

    if sigma2.opcode == OP_SIGMA2_RESUME {
        #[cfg(feature = "tracing")]
        tracing::debug!(
            sigma2_resume = %crate::hexdump::hex(&sigma2.payload),
            "received Sigma2_Resume (resumption accepted)"
        );
        // A MIC mismatch here surfaces to the caller as
        // `Error::ResumptionMacMismatch`; the record it supplied is unusable.
        initiator.handle_sigma2_resume(&sigma2.payload)?;
        if let Some(params) = initiator.peer_session_params() {
            exch.resize_peer_mrp(peer_mrp.merge_session_params(&params));
        }

        // No Sigma3 exists on this path. The handshake closes with a success
        // StatusReport from us, which the responder acks. A lost ack does not
        // invalidate the session — see `send_and_await_ack`.
        let _acked = exch
            .send_and_await_ack(
                transport,
                peer,
                OP_STATUS_REPORT,
                &session_establishment_success_report(),
                Some(sigma2.message_counter),
            )
            .await?;

        let peer_session_params = initiator.peer_session_params();
        return Ok(CaseEstablished {
            output: initiator.finish()?,
            resumed: true,
            peer_session_params,
        });
    }

    #[cfg(feature = "tracing")]
    if resuming {
        tracing::debug!("peer declined resumption; falling back to a full handshake");
    }
    #[cfg(feature = "tracing")]
    tracing::debug!(
        sigma2 = %crate::hexdump::hex(&sigma2.payload),
        "received Sigma2"
    );
    initiator.handle_sigma2(&sigma2.payload)?;

    // Re-size from what the device just said about itself, before Sigma3 goes
    // out. chip does this at the same point (`CASESession::HandleSigma2`); the
    // mDNS record the exchange started from can be stale, cached, or absent.
    if let Some(params) = initiator.peer_session_params() {
        exch.resize_peer_mrp(peer_mrp.merge_session_params(&params));
    }

    // Sigma3 is sent reliably and the device closes the handshake with a
    // SecureChannel StatusReport (success or failure) — consumed and acked
    // here, exactly as in `run_pase` (see that bridge for rationale).
    let sigma3 = initiator.next_message()?;
    let report = exch
        .send_and_recv(
            transport,
            peer,
            OP_SIGMA3,
            OP_STATUS_REPORT,
            &sigma3,
            Some(sigma2.message_counter),
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

    let peer_session_params = initiator.peer_session_params();
    Ok(CaseEstablished {
        output: initiator.finish()?,
        resumed: false,
        peer_session_params,
    })
}

/// The `SecureChannel` `StatusReport` body meaning
/// `SessionEstablishmentSuccess`: `GeneralCode=0`, `ProtocolId=0`
/// (`SecureChannel`), `ProtocolCode=0`, all little-endian (spec §4.10.1.1).
fn session_establishment_success_report() -> [u8; 8] {
    let mut body = [0u8; 8];
    body[0..2].copy_from_slice(&0u16.to_le_bytes());
    body[2..6].copy_from_slice(&0u32.to_le_bytes());
    body[6..8].copy_from_slice(&0u16.to_le_bytes());
    body
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    #[test]
    fn discovery_failure_message_distinguishes_nothing_seen_from_others_seen() {
        let target = "F52AC107C954E38E-0000000000000002";

        // Nothing at all reached this host.
        let none = discovery_failure_message(target, &[]);
        assert!(
            none.contains("not found via mDNS"),
            "substring is load-bearing"
        );
        assert!(
            none.contains("saw 0 operational mDNS records"),
            "got: {none}"
        );

        // Records arrived; none of them was the target.
        let others = discovery_failure_message(
            target,
            &[
                "F52AC107C954E38E-0000000000000003",
                "F52AC107C954E38E-0000000000000004",
            ],
        );
        assert!(
            others.contains("not found via mDNS"),
            "substring is load-bearing"
        );
        assert!(
            others.contains(
                "saw 2 operational mDNS record(s), none matching: \
                             F52AC107C954E38E-0000000000000003, \
                             F52AC107C954E38E-0000000000000004"
            ),
            "got: {others}"
        );
    }

    #[test]
    fn discovery_failure_message_is_bounded_in_names_and_length() {
        // More names than the sample bound: only SEEN_SAMPLE_MAX are quoted and
        // the remainder is summarised, so a big network cannot bloat the error.
        let owned: Vec<String> = (0..20).map(|i| format!("NODE-{i:016X}")).collect();
        let names: Vec<&str> = owned.iter().map(String::as_str).collect();
        let msg = discovery_failure_message("T", &names);
        assert!(
            msg.contains("saw 20 operational mDNS record(s)"),
            "got: {msg}"
        );
        assert!(msg.contains("… 15 more"), "got: {msg}");
        assert_eq!(msg.matches("NODE-").count(), SEEN_SAMPLE_MAX);

        // An over-long (foreign) name is truncated, not quoted whole.
        let long = "X".repeat(500);
        let msg = discovery_failure_message("T", &[&long]);
        assert!(msg.contains('…'), "over-long name must be elided: {msg}");
        assert!(
            msg.len() < 200,
            "message must stay bounded, got {} chars",
            msg.len()
        );
    }

    #[test]
    fn operational_instance_name_formats_16_16_uppercase_hex() {
        let cfid = [0x87, 0xe1, 0xb0, 0x04, 0xe2, 0x35, 0xa1, 0x30];
        let node_id: u64 = 0x0000_0000_0000_0001;
        assert_eq!(
            operational_instance_name(cfid, node_id),
            "87E1B004E235A130-0000000000000001"
        );
    }

    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};

    use matter_transport::{MatterService, QueryHandle};

    struct FakeDiscovery {
        service: MatterService,
    }

    impl Discovery for FakeDiscovery {
        fn publish(&mut self, _s: &MatterService) -> matter_transport::Result<()> {
            Ok(())
        }
        fn unpublish(&mut self, _n: &str, _k: ServiceKind) -> matter_transport::Result<()> {
            Ok(())
        }
        fn query(&mut self, _k: ServiceKind) -> matter_transport::Result<QueryHandle> {
            Ok(QueryHandle(1))
        }
        fn stop_query(&mut self, _h: QueryHandle) {}
        fn poll_results(&mut self, _h: QueryHandle) -> Vec<MatterService> {
            vec![self.service.clone()]
        }
    }

    #[tokio::test]
    async fn resolve_operational_returns_matching_addr() {
        let cfid = [0x87, 0xe1, 0xb0, 0x04, 0xe2, 0x35, 0xa1, 0x30];
        let node_id: u64 = 1;
        let name = operational_instance_name(cfid, node_id);
        let mut disc = FakeDiscovery {
            service: MatterService::new(
                name,
                ServiceKind::Operational,
                vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7))],
                5540,
                HashMap::new(),
            ),
        };
        let addr = resolve_operational(&mut disc, cfid, node_id).await.unwrap();
        assert_eq!(
            addr,
            std::net::SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7)), 5540)
        );
    }

    #[tokio::test]
    async fn resolve_operational_prefers_routable_addresses() {
        // mDNS records list link-local IPv6 first on many devices, but a
        // `fe80::` address without a scope id is unroutable from a dial-out
        // socket. Prefer IPv4, then non-link-local IPv6, and fall back to
        // whatever is left (M6.6.5: closes the FLAGGED `.first()` pick).
        use std::net::Ipv6Addr;
        let cfid = [0x87, 0xe1, 0xb0, 0x04, 0xe2, 0x35, 0xa1, 0x30];
        let node_id: u64 = 1;
        let name = operational_instance_name(cfid, node_id);
        let link_local = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x1d42));
        let ula = IpAddr::V6(Ipv6Addr::new(0xfdfc, 0x20da, 0x4273, 0x126f, 0, 0, 0, 1));
        let v4 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 248));

        // Link-local listed first, IPv4 buried last — IPv4 must win.
        let mut disc = FakeDiscovery {
            service: MatterService::new(
                name.clone(),
                ServiceKind::Operational,
                vec![link_local, ula, v4],
                5540,
                HashMap::new(),
            ),
        };
        let addr = resolve_operational(&mut disc, cfid, node_id).await.unwrap();
        assert_eq!(addr, std::net::SocketAddr::new(v4, 5540));

        // No IPv4: the non-link-local IPv6 must win over fe80.
        let mut disc = FakeDiscovery {
            service: MatterService::new(
                name,
                ServiceKind::Operational,
                vec![link_local, ula],
                5540,
                HashMap::new(),
            ),
        };
        let addr = resolve_operational(&mut disc, cfid, node_id).await.unwrap();
        assert_eq!(addr, std::net::SocketAddr::new(ula, 5540));
    }

    /// The fabric-subtype browse string and the operational instance name must
    /// name the same fabric the same way: `_I` + the instance name's first
    /// field. This pins `matter_transport::operational_fabric_subtype` against
    /// this crate's `operational_instance_name` so the two cannot drift apart
    /// (different casing or width would make the subtype browse silently match
    /// nothing).
    #[test]
    fn fabric_subtype_prefix_matches_the_instance_name_fabric_field() {
        let cfid = [0xF5, 0x2A, 0xC1, 0x07, 0xC9, 0x54, 0xE3, 0x8E];
        let subtype = matter_transport::operational_fabric_subtype(cfid);
        assert_eq!(subtype, "_IF52AC107C954E38E._sub._matter._tcp.local.");

        let instance = operational_instance_name(cfid, 3);
        assert_eq!(instance, "F52AC107C954E38E-0000000000000003");
        let fabric_field = instance.split('-').next().unwrap();
        assert_eq!(
            subtype,
            format!("_I{fabric_field}._sub._matter._tcp.local."),
            "subtype label and instance-name fabric field must agree",
        );
    }

    /// A [`Discovery`] double that answers **only** on the compressed-fabric
    /// subtype browse — the reporter's network in issue #113, where the
    /// base-type browse finds instances but never resolves them inside the
    /// budget while the subtype resolves in ~266 ms.
    ///
    /// Records the compressed fabric id the resolver asked for, so the test can
    /// assert the right fabric was browsed.
    struct SubtypeOnlyDiscovery {
        service: MatterService,
        /// Handle handed out for the subtype browse.
        subtype_handle: QueryHandle,
        /// Compressed fabric ids passed to `query_operational_fabric`.
        fabric_queries: Vec<[u8; 8]>,
        /// Handles that were stopped, so the test can prove the browses it
        /// opened are released.
        stopped: Vec<QueryHandle>,
        /// How many times the BASE-type browse was opened. The delayed-fallback
        /// design turns this into the assertion that matters: a subtype browse
        /// that answers promptly must never cause a base browse to open, or the
        /// starvation issue #113 is about comes straight back.
        base_opens: usize,
    }

    impl Discovery for SubtypeOnlyDiscovery {
        fn publish(&mut self, _s: &MatterService) -> matter_transport::Result<()> {
            Ok(())
        }
        fn unpublish(&mut self, _n: &str, _k: ServiceKind) -> matter_transport::Result<()> {
            Ok(())
        }
        fn query(&mut self, _k: ServiceKind) -> matter_transport::Result<QueryHandle> {
            self.base_opens += 1;
            Ok(QueryHandle(1))
        }
        fn query_operational_fabric(
            &mut self,
            compressed_fabric_id: [u8; 8],
        ) -> matter_transport::Result<QueryHandle> {
            self.fabric_queries.push(compressed_fabric_id);
            Ok(self.subtype_handle)
        }
        fn stop_query(&mut self, h: QueryHandle) {
            self.stopped.push(h);
        }
        fn poll_results(&mut self, h: QueryHandle) -> Vec<MatterService> {
            if h == self.subtype_handle {
                vec![self.service.clone()]
            } else {
                // The base browse sees the record's PTR but never resolves it.
                Vec::new()
            }
        }
    }

    /// A record that only ever arrives on the subtype browse still satisfies the
    /// resolve — the whole point of the #113 fix — **and the base-type browse is
    /// never opened at all**.
    ///
    /// That second half is the correction to the first attempt at this fix. The
    /// base browse re-discovers every operational instance on the link and feeds
    /// them into the resolver's shared one-per-cycle resolution queue, so opening
    /// it concurrently starves the nodes the subtype had narrowed down to. Here
    /// the subtype answers on the first poll, long inside `SUBTYPE_ONLY_WINDOW`,
    /// so the fallback must never fire.
    #[tokio::test(start_paused = true)]
    async fn subtype_match_never_opens_the_base_browse() {
        let cfid = [0xF5, 0x2A, 0xC1, 0x07, 0xC9, 0x54, 0xE3, 0x8E];
        let node_id: u64 = 3;
        let mut disc = SubtypeOnlyDiscovery {
            service: MatterService::new(
                operational_instance_name(cfid, node_id),
                ServiceKind::Operational,
                vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11))],
                5540,
                HashMap::new(),
            ),
            subtype_handle: QueryHandle(42),
            fabric_queries: Vec::new(),
            stopped: Vec::new(),
            base_opens: 0,
        };
        let addr = resolve_operational(&mut disc, cfid, node_id).await.unwrap();
        assert_eq!(
            addr,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11)), 5540)
        );
        assert_eq!(
            disc.fabric_queries,
            vec![cfid],
            "the resolve must browse OUR fabric's subtype",
        );
        assert_eq!(
            disc.base_opens, 0,
            "a promptly-answering subtype browse must NOT open the base-type \
             browse — that is what reintroduces the #113 starvation",
        );
        // The browse that WAS opened is released; the base one was never opened.
        assert_eq!(disc.stopped, vec![QueryHandle(42)]);
    }

    /// The delayed fallback: a responder that publishes no subtype PTR must
    /// still resolve. The subtype browse yields nothing, so after
    /// `SUBTYPE_ONLY_WINDOW` the base-type browse is opened behind it and the
    /// record that arrives there settles the resolve.
    ///
    /// Also pins that the fallback is *delayed* rather than immediate: the base
    /// browse must not have been opened while the window was still running.
    /// Time is the tokio paused clock (`start_paused`), so the 2 s window costs
    /// the test nothing in wall time.
    #[tokio::test(start_paused = true)]
    async fn base_browse_opens_only_after_the_subtype_window() {
        /// Yields the record on the base browse only, and records the poll
        /// iteration at which the base browse was opened.
        struct LateBaseDiscovery {
            service: MatterService,
            /// Poll iterations elapsed, counted on the SUBTYPE browse (polled
            /// every iteration from the first).
            polls: u32,
            /// `polls` at the moment the base browse was opened.
            base_opened_at: Option<u32>,
        }
        impl Discovery for LateBaseDiscovery {
            fn publish(&mut self, _s: &MatterService) -> matter_transport::Result<()> {
                Ok(())
            }
            fn unpublish(&mut self, _n: &str, _k: ServiceKind) -> matter_transport::Result<()> {
                Ok(())
            }
            fn query(&mut self, _k: ServiceKind) -> matter_transport::Result<QueryHandle> {
                self.base_opened_at = Some(self.polls);
                Ok(QueryHandle(1))
            }
            fn query_operational_fabric(
                &mut self,
                _c: [u8; 8],
            ) -> matter_transport::Result<QueryHandle> {
                Ok(QueryHandle(2))
            }
            fn stop_query(&mut self, _h: QueryHandle) {}
            fn poll_results(&mut self, h: QueryHandle) -> Vec<MatterService> {
                if h == QueryHandle(2) {
                    self.polls += 1;
                    Vec::new() // no subtype PTR published by this responder
                } else {
                    vec![self.service.clone()]
                }
            }
        }

        let cfid = [0x87, 0xe1, 0xb0, 0x04, 0xe2, 0x35, 0xa1, 0x30];
        let node_id: u64 = 1;
        let mut disc = LateBaseDiscovery {
            service: MatterService::new(
                operational_instance_name(cfid, node_id),
                ServiceKind::Operational,
                vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 14))],
                5540,
                HashMap::new(),
            ),
            polls: 0,
            base_opened_at: None,
        };
        let addr = resolve_operational(&mut disc, cfid, node_id).await.unwrap();
        assert_eq!(
            addr,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 14)), 5540),
            "a record arriving on the delayed base browse must settle the resolve",
        );
        let opened_at = disc
            .base_opened_at
            .expect("the base browse must be opened once the subtype yields nothing");
        assert_eq!(
            u128::from(opened_at),
            SUBTYPE_ONLY_POLLS,
            "the base browse must open exactly at the end of the subtype window \
             ({SUBTYPE_ONLY_POLLS} polls), not before",
        );
    }

    /// The base-type fallback: a [`Discovery`] that does not override
    /// `query_operational_fabric` (out-of-tree implementations, and any
    /// responder whose subtype never answers) must still resolve exactly as
    /// before — `FakeDiscovery` below uses the trait default and hands the same
    /// handle back for both browses, which the resolver deduplicates.
    #[tokio::test(start_paused = true)]
    async fn resolve_falls_back_to_the_base_type_browse() {
        let cfid = [0x87, 0xe1, 0xb0, 0x04, 0xe2, 0x35, 0xa1, 0x30];
        let node_id: u64 = 1;
        let mut disc = FakeDiscovery {
            service: MatterService::new(
                operational_instance_name(cfid, node_id),
                ServiceKind::Operational,
                vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 12))],
                5540,
                HashMap::new(),
            ),
        };
        let addr = resolve_operational(&mut disc, cfid, node_id).await.unwrap();
        assert_eq!(
            addr,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 12)), 5540)
        );
    }

    /// A subtype browse that yields nothing must not mask a record that only the
    /// base browse produces — the "responder publishes no subtype" case, which
    /// must behave exactly as it did before this change.
    #[tokio::test(start_paused = true)]
    async fn base_browse_still_resolves_when_the_subtype_yields_nothing() {
        struct BaseOnlyDiscovery {
            service: MatterService,
        }
        impl Discovery for BaseOnlyDiscovery {
            fn publish(&mut self, _s: &MatterService) -> matter_transport::Result<()> {
                Ok(())
            }
            fn unpublish(&mut self, _n: &str, _k: ServiceKind) -> matter_transport::Result<()> {
                Ok(())
            }
            fn query(&mut self, _k: ServiceKind) -> matter_transport::Result<QueryHandle> {
                Ok(QueryHandle(1))
            }
            fn query_operational_fabric(
                &mut self,
                _c: [u8; 8],
            ) -> matter_transport::Result<QueryHandle> {
                Ok(QueryHandle(2))
            }
            fn stop_query(&mut self, _h: QueryHandle) {}
            fn poll_results(&mut self, h: QueryHandle) -> Vec<MatterService> {
                if h == QueryHandle(1) {
                    vec![self.service.clone()]
                } else {
                    Vec::new() // no subtype PTR published by this responder
                }
            }
        }

        let cfid = [0x87, 0xe1, 0xb0, 0x04, 0xe2, 0x35, 0xa1, 0x30];
        let node_id: u64 = 1;
        let mut disc = BaseOnlyDiscovery {
            service: MatterService::new(
                operational_instance_name(cfid, node_id),
                ServiceKind::Operational,
                vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 13))],
                5540,
                HashMap::new(),
            ),
        };
        let addr = resolve_operational(&mut disc, cfid, node_id).await.unwrap();
        assert_eq!(
            addr,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 13)), 5540)
        );
    }

    /// A [`Discovery`] stub that returns no results for the first
    /// `succeed_on - 1` polls and the matching operational record on poll
    /// `succeed_on`, counting every `poll_results` call. Lets a test assert the
    /// attempt budget is respected exactly.
    struct CountingDiscovery {
        service: MatterService,
        succeed_on: u32,
        polls: u32,
    }

    impl Discovery for CountingDiscovery {
        fn publish(&mut self, _s: &MatterService) -> matter_transport::Result<()> {
            Ok(())
        }
        fn unpublish(&mut self, _n: &str, _k: ServiceKind) -> matter_transport::Result<()> {
            Ok(())
        }
        fn query(&mut self, _k: ServiceKind) -> matter_transport::Result<QueryHandle> {
            Ok(QueryHandle(1))
        }
        fn stop_query(&mut self, _h: QueryHandle) {}
        fn poll_results(&mut self, _h: QueryHandle) -> Vec<MatterService> {
            self.polls += 1;
            if self.polls >= self.succeed_on {
                vec![self.service.clone()]
            } else {
                vec![]
            }
        }
    }

    fn counting_discovery(cfid: [u8; 8], node_id: u64, succeed_on: u32) -> CountingDiscovery {
        CountingDiscovery {
            service: MatterService::new(
                operational_instance_name(cfid, node_id),
                ServiceKind::Operational,
                vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 9))],
                5540,
                HashMap::new(),
            ),
            succeed_on,
            polls: 0,
        }
    }

    /// The record appears on the 3rd poll; a budget of 3 succeeds and stops
    /// polling exactly when it finds it. Paused time auto-advances the two
    /// 100 ms inter-poll sleeps so the test runs instantly.
    #[tokio::test(start_paused = true)]
    async fn resolve_operational_with_attempts_succeeds_within_budget() {
        let cfid = [0x87, 0xe1, 0xb0, 0x04, 0xe2, 0x35, 0xa1, 0x30];
        let node_id: u64 = 1;
        let mut disc = counting_discovery(cfid, node_id, 3);
        let addr = resolve_operational_with_attempts(&mut disc, cfid, node_id, 3)
            .await
            .unwrap();
        assert_eq!(
            addr,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 9)), 5540)
        );
        assert_eq!(disc.polls, 3, "must poll exactly until the record appears");
    }

    /// The record would appear on the 5th poll, but a budget of 4 gives up
    /// first — proving the attempt count is the hard bound.
    #[tokio::test(start_paused = true)]
    async fn resolve_operational_with_attempts_respects_budget() {
        let cfid = [0x87, 0xe1, 0xb0, 0x04, 0xe2, 0x35, 0xa1, 0x30];
        let node_id: u64 = 1;
        let mut disc = counting_discovery(cfid, node_id, 5);
        let err = resolve_operational_with_attempts(&mut disc, cfid, node_id, 4)
            .await
            .expect_err("a 4-poll budget must give up before the 5th-poll record");
        assert!(matches!(err, DriverError::Discovery(_)), "got {err:?}");
        assert_eq!(disc.polls, 4, "must poll exactly the budget then stop");
    }

    /// The existing `resolve_operational` entry point still resolves via its
    /// delegated 300-attempt budget.
    #[tokio::test]
    async fn resolve_operational_still_resolves_via_delegation() {
        let cfid = [0x87, 0xe1, 0xb0, 0x04, 0xe2, 0x35, 0xa1, 0x30];
        let node_id: u64 = 1;
        let mut disc = counting_discovery(cfid, node_id, 1);
        let addr = resolve_operational(&mut disc, cfid, node_id).await.unwrap();
        assert_eq!(
            addr,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 9)), 5540)
        );
    }

    // -----------------------------------------------------------------------
    // run_case loopback test
    // -----------------------------------------------------------------------

    use matter_cert::test_support::{build_unsigned, with_signature, TestCertFields};
    use matter_cert::{
        BasicConstraints, DistinguishedName, DnAttribute, Extensions, KeyIdentifier, KeyUsage,
        MatterCertificate, MatterTime, PublicKey, Signature, TrustAnchor, TrustedRoots,
    };
    use matter_crypto::{CaseCredentials, CaseResponder, CaseSigner, RingSigner, Sigma1Outcome};
    use matter_transport::{SessionKeys, SessionManager};

    use crate::driver::datagram::InMemoryDatagram;
    use crate::driver::unsecured::{decode_unsecured, encode_unsecured};

    const T_FABRIC_ID: u64 = 0x4242_4242_4242_4242;
    const T_INITIATOR_NODE: u64 = 0xDEAD_BEEF_CAFE_F00D;
    const T_RESPONDER_NODE: u64 = 0xBABE_FEED_1234_5678;
    const T_IPK: [u8; 16] = [0x77; 16];
    const T_RCAC_SKI: [u8; 20] = [0x01; 20];
    const T_NOC_SKI: [u8; 20] = [0x02; 20];

    /// Build a self-signed RCAC and return it with its signer and raw public
    /// key. The caller builds `TrustedRoots` from the returned `&rcac` so
    /// that two independent roots sets (controller + device) can be derived
    /// without requiring `TrustedRoots: Clone` — though it happens to be
    /// `Clone`, both patterns work.
    fn build_test_rcac() -> (MatterCertificate, RingSigner, [u8; 65]) {
        let (rcac_signer, _pkcs8) = RingSigner::generate().unwrap();
        let rcac_pub = *rcac_signer.public_key().as_bytes();
        let rcac_dn = DistinguishedName::new(vec![DnAttribute::RcacId(1)]);
        let extensions = Extensions::builder()
            .basic_constraints(Some(BasicConstraints::new(true, Some(1))))
            .key_usage(Some(KeyUsage::KEY_CERT_SIGN))
            .subject_key_identifier(Some(KeyIdentifier(T_RCAC_SKI)))
            .authority_key_identifier(Some(KeyIdentifier(T_RCAC_SKI)))
            .build();
        let fields = TestCertFields {
            serial: vec![0x01],
            issuer: rcac_dn.clone(),
            not_before: MatterTime::from_unix_secs(1_700_000_000),
            not_after: MatterTime::from_unix_secs(2_500_000_000),
            subject: rcac_dn,
            public_key: PublicKey::new(rcac_pub).unwrap(),
            extensions,
            signature: Signature::new([0u8; 64]),
        };
        let unsigned = build_unsigned(fields);
        let tbs = unsigned.to_x509_tbs_der().unwrap();
        let sig = rcac_signer.sign_p256_sha256(&tbs).unwrap();
        let rcac = with_signature(&unsigned, Signature::new(sig));
        (rcac, rcac_signer, rcac_pub)
    }

    fn roots_for(rcac: &MatterCertificate) -> TrustedRoots {
        let mut roots = TrustedRoots::new();
        roots.add(TrustAnchor::from_root_cert(rcac));
        roots
    }

    fn build_test_noc(rcac_signer: &RingSigner, node_id: u64) -> (MatterCertificate, RingSigner) {
        let (noc_signer, _) = RingSigner::generate().unwrap();
        let noc_pub = *noc_signer.public_key().as_bytes();
        let subject_dn = DistinguishedName::new(vec![
            DnAttribute::FabricId(T_FABRIC_ID),
            DnAttribute::NodeId(node_id),
        ]);
        let issuer_dn = DistinguishedName::new(vec![DnAttribute::RcacId(1)]);
        let extensions = Extensions::builder()
            .basic_constraints(Some(BasicConstraints::new(false, None)))
            .key_usage(Some(KeyUsage::DIGITAL_SIGNATURE))
            .subject_key_identifier(Some(KeyIdentifier(T_NOC_SKI)))
            .authority_key_identifier(Some(KeyIdentifier(T_RCAC_SKI)))
            .build();
        let fields = TestCertFields {
            serial: vec![0x02],
            issuer: issuer_dn,
            not_before: MatterTime::from_unix_secs(1_700_000_000),
            not_after: MatterTime::from_unix_secs(2_500_000_000),
            subject: subject_dn,
            public_key: PublicKey::new(noc_pub).unwrap(),
            extensions,
            signature: Signature::new([0u8; 64]),
        };
        let unsigned = build_unsigned(fields);
        let tbs = unsigned.to_x509_tbs_der().unwrap();
        let sig = rcac_signer.sign_p256_sha256(&tbs).unwrap();
        let noc = with_signature(&unsigned, Signature::new(sig));
        (noc, noc_signer)
    }

    fn creds(
        noc: MatterCertificate,
        signer: RingSigner,
        node_id: u64,
        rcac_pub: [u8; 65],
    ) -> CaseCredentials {
        CaseCredentials {
            noc,
            icac: None,
            signer: Box::new(signer),
            fabric_id: T_FABRIC_ID,
            node_id,
            ipk: T_IPK,
            rcac_public_key: rcac_pub,
        }
    }

    #[tokio::test]
    async fn run_case_surfaces_sigma1_status_report_rejection() {
        // A device that cannot match Sigma1's destination id (e.g. IPK or
        // fabric mismatch) answers with a StatusReport (NoSharedTrustRoots,
        // protocol code 0x0001) instead of Sigma2. run_case must surface the
        // device's codes, not feed the report into the Sigma2 parser
        // (observed: Tapo P110M, M6.6.5 — misparsed as "invalid parameter").
        let (rcac, rcac_signer, rcac_pub) = build_test_rcac();
        let (init_noc, init_signer) = build_test_noc(&rcac_signer, T_INITIATOR_NODE);
        let init_creds = creds(init_noc, init_signer, T_INITIATOR_NODE, rcac_pub);
        let ctrl_roots = roots_for(&rcac);

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut sessions = SessionManager::new();

        let device = async {
            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            // StatusReport: FAILURE / SecureChannel NoSharedTrustRoots.
            let mut body = Vec::new();
            body.extend_from_slice(&1u16.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&0x0001u16.to_le_bytes());
            let report = encode_unsecured(
                200,
                m.exchange_id,
                0x40,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &body,
            );
            dev_io.send_to(&report, ctrl_addr).await.unwrap();
        };

        let controller = run_case(
            &ctrl_io,
            &mut sessions,
            dev_addr,
            init_creds,
            ctrl_roots,
            T_RESPONDER_NODE,
            T_FABRIC_ID,
            MatterTime::from_unix_secs(2_000_000_000),
        );

        let (ctrl_result, ()) = tokio::join!(controller, device);
        let err = ctrl_result.unwrap_err();
        assert!(
            matches!(
                err,
                DriverError::SessionEstablishmentFailed {
                    general_code: 1,
                    protocol_code: 0x0001,
                }
            ),
            "expected SessionEstablishmentFailed, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn run_case_establishes_matching_session() {
        let (rcac, rcac_signer, rcac_pub) = build_test_rcac();
        let (init_noc, init_signer) = build_test_noc(&rcac_signer, T_INITIATOR_NODE);
        let (resp_noc, resp_signer) = build_test_noc(&rcac_signer, T_RESPONDER_NODE);
        let init_creds = creds(init_noc, init_signer, T_INITIATOR_NODE, rcac_pub);
        let resp_creds = creds(resp_noc, resp_signer, T_RESPONDER_NODE, rcac_pub);
        let resp_roots = roots_for(&rcac);
        let ctrl_roots = roots_for(&rcac);

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();
        let mut sessions = SessionManager::new();

        let device = async {
            let mut responder = CaseResponder::new(
                resp_creds,
                resp_roots,
                0x00D2,
                MatterTime::from_unix_secs(2_000_000_000),
            )
            .unwrap();
            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            assert!(matches!(
                responder.handle_sigma1(&m.payload).unwrap(),
                Sigma1Outcome::NewSession
            ));
            let sigma2 = responder.next_message().unwrap();
            let wire = encode_unsecured(
                200,
                m.exchange_id,
                0x31,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &sigma2,
            );
            dev_io.send_to(&wire, ctrl_addr).await.unwrap();
            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            responder.handle_sigma3(&m.payload).unwrap();

            // Close the handshake with a success StatusReport (real-device
            // behaviour) and expect the controller's standalone ack.
            let mut body = Vec::new();
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            let report = encode_unsecured(
                201,
                m.exchange_id,
                0x40,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &body,
            );
            dev_io.send_to(&report, ctrl_addr).await.unwrap();
            let ack = tokio::time::timeout(std::time::Duration::from_secs(2), dev_io.recv_from())
                .await
                .expect("controller must ack the StatusReport")
                .unwrap();
            let ack = decode_unsecured(&ack.0).unwrap();
            assert_eq!(ack.opcode, 0x10);
            assert_eq!(ack.ack_counter, Some(201));

            responder.finish().unwrap()
        };

        let controller = run_case(
            &ctrl_io,
            &mut sessions,
            dev_addr,
            init_creds,
            ctrl_roots,
            T_RESPONDER_NODE,
            T_FABRIC_ID,
            MatterTime::from_unix_secs(2_000_000_000),
        );

        let (ctrl_result, dev_out) = tokio::join!(controller, device);
        let sid = ctrl_result.unwrap();
        let registered = sessions.get(sid).unwrap();
        assert_eq!(registered.keys, SessionKeys::from_case_output(&dev_out));
        assert_eq!(registered.peer_id, matter_transport::SessionId(0x00D2));
    }

    // ---------------------------------------------------------------
    // Initiator-side CASE resumption
    // ---------------------------------------------------------------

    /// A matched pair of resumption records, as a real prior session would
    /// have left on both sides: same id, same secret, each side caching the
    /// other's identity.
    fn matched_resumption_pair(
        init_noc: &MatterCertificate,
        resp_noc: &MatterCertificate,
    ) -> (
        matter_crypto::ResumptionRecord,
        matter_crypto::ResumptionRecord,
    ) {
        use matter_crypto::{PeerInfo, ResumptionId, ResumptionRecord};
        let id = ResumptionId([0x42; 16]);
        let shared_secret = [0x77u8; 32];
        (
            // What the controller stored: the device's identity.
            ResumptionRecord {
                id,
                shared_secret,
                peer: PeerInfo {
                    node_id: T_RESPONDER_NODE,
                    fabric_id: T_FABRIC_ID,
                    noc: resp_noc.clone(),
                    session_id: 0x00D2,
                },
                expires_at: None,
            },
            // What the device stored: the controller's identity.
            ResumptionRecord {
                id,
                shared_secret,
                peer: PeerInfo {
                    node_id: T_INITIATOR_NODE,
                    fabric_id: T_FABRIC_ID,
                    noc: init_noc.clone(),
                    session_id: 0x0001,
                },
                expires_at: None,
            },
        )
    }

    /// The fast path end to end: Sigma1-with-resumption → `Sigma2_Resume` →
    /// success `StatusReport` → ack. No Sigma3 exists on this path.
    #[tokio::test]
    async fn resumption_takes_the_sigma2_resume_fast_path() {
        let (rcac, rcac_signer, rcac_pub) = build_test_rcac();
        let (init_noc, init_signer) = build_test_noc(&rcac_signer, T_INITIATOR_NODE);
        let (resp_noc, resp_signer) = build_test_noc(&rcac_signer, T_RESPONDER_NODE);
        let (ctrl_record, dev_record) = matched_resumption_pair(&init_noc, &resp_noc);
        let init_creds = creds(init_noc, init_signer, T_INITIATOR_NODE, rcac_pub);
        let resp_creds = creds(resp_noc, resp_signer, T_RESPONDER_NODE, rcac_pub);

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let device = async {
            let mut responder = CaseResponder::new(
                resp_creds,
                roots_for(&rcac),
                0x00D2,
                MatterTime::from_unix_secs(2_000_000_000),
            )
            .unwrap();
            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            assert!(
                matches!(
                    responder.handle_sigma1(&m.payload).unwrap(),
                    Sigma1Outcome::ResumptionRequested { .. }
                ),
                "Sigma1 must carry the resumption fields"
            );
            responder.accept_resumption(dev_record).unwrap();
            let sigma2_resume = responder.next_message().unwrap();
            let wire = encode_unsecured(
                200,
                m.exchange_id,
                OP_SIGMA2_RESUME,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &sigma2_resume,
            );
            dev_io.send_to(&wire, ctrl_addr).await.unwrap();

            // The controller closes with a success StatusReport; ack it.
            let (p, _) =
                tokio::time::timeout(std::time::Duration::from_secs(2), dev_io.recv_from())
                    .await
                    .expect("controller must send the closing StatusReport")
                    .unwrap();
            let m = decode_unsecured(&p).unwrap();
            assert_eq!(m.opcode, OP_STATUS_REPORT, "resumed path ends in a report");
            assert_eq!(
                u16::from_le_bytes([m.payload[0], m.payload[1]]),
                0,
                "must report success"
            );
            let ack = encode_unsecured(
                201,
                m.exchange_id,
                0x10,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                false,
                Some(m.message_counter),
                None,
                &[],
            );
            dev_io.send_to(&ack, ctrl_addr).await.unwrap();
            responder.finish().unwrap()
        };

        let controller = run_case_establish_with(
            &ctrl_io,
            dev_addr,
            CaseEstablishOptions::new(
                0x0001,
                init_creds,
                roots_for(&rcac),
                T_RESPONDER_NODE,
                T_FABRIC_ID,
                MatterTime::from_unix_secs(2_000_000_000),
            )
            .with_resumption_record(ctrl_record),
        );

        let (ctrl_result, dev_out) = tokio::join!(controller, device);
        let established = ctrl_result.expect("resumed handshake");
        assert!(
            established.resumed,
            "the fast path must be reported as such"
        );
        assert_eq!(
            SessionKeys::from_case_output(&established.output),
            SessionKeys::from_case_output(&dev_out),
            "both sides must derive the same resumed session keys"
        );
        // Each accept rotates the id; the record handed back is the NEW one.
        let next = established
            .output
            .resumption_record
            .as_ref()
            .expect("resumed handshake yields a record to persist");
        assert_ne!(
            next.id,
            matter_crypto::ResumptionId([0x42; 16]),
            "the responder must have rotated the resumption id"
        );
    }

    /// The decline path: the responder does not know the id, answers with a
    /// plain Sigma2, and the full handshake completes on the SAME exchange
    /// with the SAME ephemeral key.
    ///
    /// This is the regression that matters most. `send_and_recv`'s opcode gate
    /// silently DROPS a frame that is neither the awaited opcode nor a
    /// `StatusReport`, so before `send_and_recv_any` existed a resumption Sigma1
    /// could only ever time out — whichever way the responder replied.
    #[tokio::test]
    async fn declined_resumption_falls_back_to_a_full_handshake() {
        let (rcac, rcac_signer, rcac_pub) = build_test_rcac();
        let (init_noc, init_signer) = build_test_noc(&rcac_signer, T_INITIATOR_NODE);
        let (resp_noc, resp_signer) = build_test_noc(&rcac_signer, T_RESPONDER_NODE);
        let (ctrl_record, _unused) = matched_resumption_pair(&init_noc, &resp_noc);
        let init_creds = creds(init_noc, init_signer, T_INITIATOR_NODE, rcac_pub);
        let resp_creds = creds(resp_noc, resp_signer, T_RESPONDER_NODE, rcac_pub);

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let device = async {
            let mut responder = CaseResponder::new(
                resp_creds,
                roots_for(&rcac),
                0x00D2,
                MatterTime::from_unix_secs(2_000_000_000),
            )
            .unwrap();
            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            assert!(matches!(
                responder.handle_sigma1(&m.payload).unwrap(),
                Sigma1Outcome::ResumptionRequested { .. }
            ));
            // "I have never heard of that resumption id."
            responder.reject_resumption().unwrap();
            let sigma2 = responder.next_message().unwrap();
            let wire = encode_unsecured(
                200,
                m.exchange_id,
                OP_SIGMA2,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &sigma2,
            );
            dev_io.send_to(&wire, ctrl_addr).await.unwrap();

            let (p, _) =
                tokio::time::timeout(std::time::Duration::from_secs(2), dev_io.recv_from())
                    .await
                    .expect("controller must fall back to Sigma3")
                    .unwrap();
            let m = decode_unsecured(&p).unwrap();
            assert_eq!(m.opcode, OP_SIGMA3, "the fallback must send a real Sigma3");
            responder.handle_sigma3(&m.payload).unwrap();

            let mut body = Vec::new();
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            let report = encode_unsecured(
                201,
                m.exchange_id,
                OP_STATUS_REPORT,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &body,
            );
            dev_io.send_to(&report, ctrl_addr).await.unwrap();
            let _ack = tokio::time::timeout(std::time::Duration::from_secs(2), dev_io.recv_from())
                .await
                .expect("controller must ack the StatusReport")
                .unwrap();
            responder.finish().unwrap()
        };

        let controller = run_case_establish_with(
            &ctrl_io,
            dev_addr,
            CaseEstablishOptions::new(
                0x0001,
                init_creds,
                roots_for(&rcac),
                T_RESPONDER_NODE,
                T_FABRIC_ID,
                MatterTime::from_unix_secs(2_000_000_000),
            )
            .with_resumption_record(ctrl_record),
        );

        let (ctrl_result, dev_out) = tokio::join!(controller, device);
        let established = ctrl_result.expect("declined resumption must still connect");
        assert!(
            !established.resumed,
            "a declined attempt is not a resumed session"
        );
        assert_eq!(
            SessionKeys::from_case_output(&established.output),
            SessionKeys::from_case_output(&dev_out),
            "the fallback must agree on keys with the device"
        );
    }

    /// A `Sigma2_Resume` whose MIC does not verify must surface as
    /// `ResumptionMacMismatch`, so the caller can tell "this stored record is
    /// unusable" apart from every other failure and drop it.
    ///
    /// # Why the frame is corrupted rather than the secret
    ///
    /// An honest same-id/different-secret split cannot reach the initiator's
    /// check: the responder validates the *Sigma1* resume MIC first
    /// (`accept_resumption`), fails, and declines to a full handshake. chip's
    /// own unit test asserts exactly that outcome for the "peers both have
    /// record of the same resumption ID, but a different shared secret"
    /// vector. So a bad `Sigma2_Resume` MIC on the wire means a corrupted or
    /// forged frame, and that is what this reproduces.
    #[tokio::test]
    async fn sigma2_resume_mic_mismatch_surfaces_as_resumption_mac_mismatch() {
        let (rcac, rcac_signer, rcac_pub) = build_test_rcac();
        let (init_noc, init_signer) = build_test_noc(&rcac_signer, T_INITIATOR_NODE);
        let (resp_noc, resp_signer) = build_test_noc(&rcac_signer, T_RESPONDER_NODE);
        let (ctrl_record, mut dev_record) = matched_resumption_pair(&init_noc, &resp_noc);
        // Same id, different secret — chip's own test calls this out as the
        // "mismatched session resumption information" case.
        dev_record.shared_secret = [0x99u8; 32];
        let init_creds = creds(init_noc, init_signer, T_INITIATOR_NODE, rcac_pub);
        let resp_creds = creds(resp_noc, resp_signer, T_RESPONDER_NODE, rcac_pub);

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        let device = async {
            let mut responder = CaseResponder::new(
                resp_creds,
                roots_for(&rcac),
                0x00D2,
                MatterTime::from_unix_secs(2_000_000_000),
            )
            .unwrap();
            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            let _ = responder.handle_sigma1(&m.payload).unwrap();
            // Accept with the matching secret (so a well-formed Sigma2_Resume
            // is produced at all), then corrupt the MIC on the wire.
            responder
                .accept_resumption(matter_crypto::ResumptionRecord {
                    id: matter_crypto::ResumptionId([0x42; 16]),
                    shared_secret: [0x77u8; 32],
                    peer: matter_crypto::PeerInfo {
                        node_id: T_INITIATOR_NODE,
                        fabric_id: T_FABRIC_ID,
                        noc: dev_record.peer.noc.clone(),
                        session_id: 0x0001,
                    },
                    expires_at: None,
                })
                .unwrap();
            let mut sigma2_resume = responder.next_message().unwrap();
            // Flip the first byte of `resume_mic` (context tag 2, a 16-byte
            // octet string, so the element header is 0x30 0x02 0x10). Located
            // by that header rather than by a fixed offset: a hard-coded index
            // would silently start corrupting some other field if the encoding
            // ever changed, and the test would keep passing for the wrong
            // reason.
            let mic_at = sigma2_resume
                .windows(3)
                .position(|w| w == [0x30, 0x02, 0x10])
                .expect("resume_mic element header")
                + 3;
            sigma2_resume[mic_at] ^= 0xFF;
            let wire = encode_unsecured(
                200,
                m.exchange_id,
                OP_SIGMA2_RESUME,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &sigma2_resume,
            );
            dev_io.send_to(&wire, ctrl_addr).await.unwrap();
        };

        let controller = run_case_establish_with(
            &ctrl_io,
            dev_addr,
            CaseEstablishOptions::new(
                0x0001,
                init_creds,
                roots_for(&rcac),
                T_RESPONDER_NODE,
                T_FABRIC_ID,
                MatterTime::from_unix_secs(2_000_000_000),
            )
            .with_resumption_record(ctrl_record),
        );

        let (ctrl_result, ()) = tokio::join!(controller, device);
        let err = ctrl_result.expect_err("a bad Sigma2_Resume MIC must fail the connect");
        assert!(
            matches!(
                err,
                DriverError::Crypto(matter_crypto::Error::ResumptionMacMismatch)
            ),
            "the caller must be able to single this out to drop the record; got: {err:?}"
        );
    }

    /// `run_case_establish` drives the same handshake but returns
    /// the [`CaseSessionOutput`] WITHOUT registering it, advertising the caller's
    /// `local_session_id` in Sigma1. The returned output must (a) carry that
    /// local id and (b) register into a fresh `SessionManager` yielding a usable
    /// session whose keys match the device's — i.e. the hand-back path a spawned,
    /// own-socket connect uses to give the actor a ready-to-register session.
    #[tokio::test]
    async fn run_case_establish_returns_registerable_output() {
        let (rcac, rcac_signer, rcac_pub) = build_test_rcac();
        let (init_noc, init_signer) = build_test_noc(&rcac_signer, T_INITIATOR_NODE);
        let (resp_noc, resp_signer) = build_test_noc(&rcac_signer, T_RESPONDER_NODE);
        let init_creds = creds(init_noc, init_signer, T_INITIATOR_NODE, rcac_pub);
        let resp_creds = creds(resp_noc, resp_signer, T_RESPONDER_NODE, rcac_pub);
        let resp_roots = roots_for(&rcac);
        let ctrl_roots = roots_for(&rcac);

        let (ctrl_io, dev_io) = InMemoryDatagram::pair();
        let dev_addr = dev_io.local_addr();
        let ctrl_addr = ctrl_io.local_addr();

        // The local session id the caller allocates and advertises in Sigma1.
        let local_session_id: u16 = 0x0777;

        let device = async {
            let mut responder = CaseResponder::new(
                resp_creds,
                resp_roots,
                0x00D2,
                MatterTime::from_unix_secs(2_000_000_000),
            )
            .unwrap();
            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            assert!(matches!(
                responder.handle_sigma1(&m.payload).unwrap(),
                Sigma1Outcome::NewSession
            ));
            let sigma2 = responder.next_message().unwrap();
            let wire = encode_unsecured(
                200,
                m.exchange_id,
                0x31,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &sigma2,
            );
            dev_io.send_to(&wire, ctrl_addr).await.unwrap();
            let (p, _) = dev_io.recv_from().await.unwrap();
            let m = decode_unsecured(&p).unwrap();
            responder.handle_sigma3(&m.payload).unwrap();
            let mut body = Vec::new();
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            let report = encode_unsecured(
                201,
                m.exchange_id,
                0x40,
                matter_transport::ProtocolId::SECURE_CHANNEL,
                false,
                true,
                Some(m.message_counter),
                None,
                &body,
            );
            dev_io.send_to(&report, ctrl_addr).await.unwrap();
            let ack = tokio::time::timeout(std::time::Duration::from_secs(2), dev_io.recv_from())
                .await
                .expect("controller must ack the StatusReport")
                .unwrap();
            let ack = decode_unsecured(&ack.0).unwrap();
            assert_eq!(ack.opcode, 0x10);
            assert_eq!(ack.ack_counter, Some(201));
            responder.finish().unwrap()
        };

        let controller = run_case_establish(
            &ctrl_io,
            dev_addr,
            local_session_id,
            init_creds,
            ctrl_roots,
            T_RESPONDER_NODE,
            T_FABRIC_ID,
            MatterTime::from_unix_secs(2_000_000_000),
            MrpConfig::for_peer(None, None, None),
        );

        let (ctrl_result, dev_out) = tokio::join!(controller, device);
        let output = ctrl_result.unwrap();

        // (a) The output advertises the caller-chosen local session id.
        assert_eq!(output.local.session_id, local_session_id);

        // (b) The un-registered output registers cleanly and yields a usable
        // session whose keys match the device's derived keys.
        let mut sessions = SessionManager::new();
        let sid = sessions.register_case(&output, SessionRole::Initiator);
        let registered = sessions.get(sid).unwrap();
        assert_eq!(registered.keys, SessionKeys::from_case_output(&dev_out));
        assert_eq!(registered.peer_id, matter_transport::SessionId(0x00D2));
    }
}
