//! Default mdns-sd-based discovery adapter for Matter.
//!
//! Wraps [`mdns_sd::ServiceDaemon`] (which spawns its own background
//! thread on creation) and translates between its [`ServiceEvent`]
//! stream and our [`MatterService`] shape.
//!
//! # One browse per service type, shared by every handle
//!
//! `mdns-sd` keeps **one querier per service type**: its
//! `service_queriers` map is keyed by `ty_domain`, so a second
//! `browse("_matter._tcp.local.")` *replaces* the sender of the first and the
//! first `Receiver` never sees another event; and `stop_browse` removes that
//! shared querier — and drops the daemon's cached records for the type —
//! whoever asks for it. (The cache is dropped only if the querier is still
//! there when the daemon runs the stop: mdns-sd 0.21.6 removes a querier
//! whose receiver is gone or full without touching the cache, so a stop that
//! follows such a removal leaves the records to expire on their own TTLs.)
//!
//! [`QueryHandle`]s are therefore **not** independent browses. This adapter
//! models what the daemon actually provides: it opens at most one browse per
//! service type, reference-counts the handles attached to it, fans every
//! surfaced record out to *all* of them, and calls `stop_browse` only when the
//! last handle for that type is released. Without that, two concurrent
//! `query(ServiceKind::Operational)` callers silently sabotage each other —
//! the second one's `browse` orphans the first's receiver, and its
//! `stop_query` tears down the browse the first is still waiting on, so the
//! first never resolves again for the life of the process (issue #113).
//!
//! Sharing the browse means a second handle never calls `browse`, so it never
//! gets the cache replay a real `browse` performs — and mdns-sd emits a
//! `ServiceResolved` only when a record *changes*, so a stable service that
//! resolved before the handle attached would produce no event for the life of
//! the browse. The adapter therefore keeps its own replay cache of the last
//! record surfaced per instance name — dropped again when the daemon reports
//! the instance removed — and seeds each newly attached handle from it, so
//! attaching late is equivalent to having been attached all along.
//!
//! # Compressed-fabric subtype browses
//!
//! [`Discovery::query_operational_fabric`] browses
//! `_I<compressed-fabric-id>._sub._matter._tcp.local.` instead of the base
//! `_matter._tcp.local.`, so a controller sees only the operational nodes of
//! its own fabric. mdns-sd keys its querier, its PTR cache and `stop_browse` by
//! the browsed string, so a subtype browse is a **different key** from the base
//! type: the two coexist under the same refcounting, with independent record
//! streams and independent teardown. Records that arrive that way carry the
//! subtype as their `ty_domain`; it is stripped during translation, so they
//! surface as ordinary [`ServiceKind::Operational`] records.
//!
//! This matters because mdns-sd resolves the instances it discovers one at a
//! time with an exponential backoff: on a link with many operational nodes, a
//! base-type browse can consume a caller's whole resolve budget without ever
//! reaching the wanted node (issue #113).
//!
//! One caveat if you hold the two browses on different lifetimes:
//! `stop_browse(<type>)` drops the daemon's PTR entries for that type *and* the
//! SRV/TXT records of the instances they named — records a surviving browse of
//! the other string still needs, and must re-query for. The controller does
//! hold them on different lifetimes: while a subscription is waiting to be
//! re-established it keeps the fabric-subtype browse open alone, and opens the
//! base type only as a short-lived resolve fallback. When that base browse
//! stops, the surviving subtype browse re-resolves the wiped instances and
//! emits fresh `ServiceResolved` events for records that did not change. That
//! is harmless to [`Discovery::poll_results`] callers (a re-delivered record is
//! still a record), and it is why the controller's "device is back" signal is
//! [`Discovery::poll_found`] — a **new PTR** — and not a resolved record: the
//! subtype's own PTR entries survive a base-type stop, so the re-resolution
//! never produces a `ServiceFound`. This adapter's own `surfaced` replay cache
//! is per browse and is unaffected either way.
//!
//! # Found events
//!
//! [`Discovery::poll_found`] reports mdns-sd's `ServiceFound`, which the daemon
//! emits when a **new** PTR record enters its cache — an instance appearing,
//! or re-appearing after its record expired — and also for every PTR it
//! already caches when a browse is (re-)issued. The adapter tells the two
//! apart: it remembers, per browse, the instances it has reported and not
//! since seen removed (`ServiceRemoved`), and does not report those again.
//! Each event is buffered per handle as an instance name, fanned out to every
//! handle of the browse like a record, never replayed to a handle attached
//! later, and capped at 256 names per handle (oldest dropped). `poll_results`
//! and `poll_found` share one drain of the browse, so the order in which a
//! caller makes the two calls does not matter.
//!
//! # Re-query
//!
//! [`Discovery::requery`] re-issues a browse with `daemon.browse`, so mdns-sd
//! sends a fresh PTR query now rather than at the next step of its own
//! doubling schedule (1 s, 2 s, 4 s, … capped at an hour). mdns-sd accepts a
//! response only if it answers one of its active queriers, so a device whose
//! announcement it did not take is otherwise seen only at that next query. The
//! query carries known answers — the cached PTRs not yet past half their TTL —
//! so devices whose fresh PTR the daemon holds stay silent (a live device past
//! that half-life may answer anyway, which is harmless), and a device whose PTR
//! expired while it was away answers with a new one: a `ServiceFound`.
//!
//! The swap is lossless. `daemon.browse` only queues a command, so the old
//! receiver is kept and drained ahead of the new one until the new one's
//! first event; mdns-sd sends `SearchStarted` first on a new listener, in the
//! same step that makes it the live one, so nothing the old receiver carries
//! can be overtaken. The re-issued browse replays every cached PTR as
//! `ServiceFound` plus `ServiceResolved`: found events for instances already
//! present are suppressed (above), resolved records are re-delivered (latest
//! wins).
//!
//! Costs per requery, for `N` cached instances of the type:
//!
//! - **A replay to drain.** Up to `1 + 2N` events, queued on the browse's
//!   channel until the caller's next [`Discovery::poll_results`] /
//!   [`Discovery::poll_found`]. The daemon never waits on that channel, but
//!   it holds at most 1000 events: one that fills is closed and must be
//!   re-opened (see "A browse the daemon closed" below), so a caller with a
//!   very large fabric should poll briskly for a while after a requery
//!   (`matter-controller` drains every 250 ms for 15 s after each one).
//! - **A restarted query chain.** The first query goes out after 10–50 ms,
//!   then 1, 2, 4, 8, 16 s … apart, doubling up to an hour: about five small
//!   multicast PTR queries in the first 15 s and about nine in the first
//!   5 minutes. A caller that requeries every 5 minutes therefore sends about
//!   nine queries per 5 minutes instead of the one an hour a settled browse
//!   sends. The old chain is cancelled when the daemon runs the new browse.
//! - **Resolve queries at each step of that chain** for every instance cached
//!   but not yet resolved: SRV and TXT, or A and AAAA when only the host's
//!   address is missing. mdns-sd keeps re-querying those for as long as the
//!   browse is active.
//!
//! # A browse the daemon closed
//!
//! mdns-sd closes a browse whose event channel fills (1000 events): it stops
//! that service type's querier, keeps its cached records, and drops its end of
//! the channel, sending no `SearchStopped`. The next drain finds the receiver
//! closed, opens the browse again with the same string, and logs that at
//! `warn`, because events were lost. The fresh browse's cache replay rebuilds
//! what the adapter knows about the type: every instance still cached is
//! reported found once more, and every cached record is re-delivered.
//!
//! mdns-sd pushes that whole replay in one step, so a type with about 500 or
//! more cached instances fills the channel again at once and can never stay
//! open. Re-opens therefore back off: one that closes again waits 1, 2, 4, …
//! drains (at most 256) before the next attempt, logged at `debug`. The
//! backoff resets once a re-opened browse delivers events and then stays open
//! for 16 drains in a row.
//!
//! # Diagnostics
//!
//! Discovery is the one part of the stack whose failures are invisible from the
//! outside: a record that never arrives, and a record that arrives but is
//! discarded during translation, look identical to a caller (a resolve that
//! simply never completes). This adapter therefore traces every browse it
//! starts, every record it surfaces, and — individually, with the offending
//! value — every record it drops, under the `matter_transport::mdns` target:
//!
//! - `debug`: browse started, record surfaced, record dropped (no addresses,
//!   unrecognised service type), `ServiceFound` ignored because its fullname
//!   is malformed (no leading instance label).
//! - `warn`: record dropped because it is malformed (fullname with no instance
//!   label) — that is a peer bug, not a normal condition.
//! - `trace`: every non-`ServiceResolved` browse event, by variant. A
//!   `ServiceFound` with no matching `ServiceResolved` means the resolver never
//!   completed SRV/address resolution for that instance, which is otherwise
//!   undiagnosable. Also: a handle's found-event buffer dropping its oldest
//!   name at the 256-name cap.
//!
//! Nothing is logged unless the caller installs a `tracing` subscriber; with
//! none installed each site is a cheap dispatcher check.
//!
//! ```text
//! RUST_LOG=matter_transport::mdns=trace,matter_controller::actor=debug
//! ```
//!
//! [`ServiceEvent`]: mdns_sd::ServiceEvent

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use mdns_sd::{Receiver, ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo, TxtProperty};

use crate::discovery::{
    operational_fabric_subtype, Discovery, MatterService, QueryHandle, ServiceKind,
};
use crate::error::{Error, Result};

/// `tracing` target for this adapter's diagnostics, so discovery can be turned
/// up on its own (`RUST_LOG=matter_transport::mdns=trace`) without enabling
/// every other target in the process.
const LOG_TARGET: &str = "matter_transport::mdns";

/// How many found-event names one handle buffers before the oldest is dropped.
///
/// [`Discovery::poll_found`] is an event stream, and a handle whose owner never
/// calls it (the controller's base-type handle is one) would otherwise
/// accumulate a name per new PTR for as long as it lives. 256 is far above any
/// realistic burst between two polls of a handle that IS polled, so the cap
/// only ever bites the unpolled ones, and there it costs bounded memory and a
/// `trace` line per drop.
const FOUND_BUFFER_CAP: usize = 256;

/// The most drains of a closed browse skipped between two attempts to re-open
/// it ([`ReopenBackoff`]).
///
/// A drain is any `poll_results`, `poll_found` or `requery` call on any handle
/// of the browse, so in time this depends on the caller: a single handle
/// drained once a second, `matter-controller`'s slowest cadence, skips for at
/// most about four minutes; more handles, or both polls in one pass, make it
/// shorter. Long enough that a browse which can never stay open (its cache
/// replay alone fills the channel) costs a re-open and a `debug` line every
/// few minutes, short enough that one which recovers is back soon.
const REOPEN_BACKOFF_MAX_SKIPS: u32 = 256;

/// How many drains in a row a re-opened browse must stay open, starting with
/// one that delivered an event, before its backoff resets ([`ReopenBackoff`]).
///
/// One open drain is not enough: a replay that only just fits the channel is
/// drained once, and then live answers fill the rest and close it again. A
/// browse that closes within this many drains carries on with its backoff.
const REOPEN_HEALTHY_DRAINS: u32 = 16;

/// How soon a closed browse is re-opened again, counted in drains.
///
/// Drains, not wall time: this adapter has no clock of its own, and drains
/// are when a closure is noticed anyway. The first closure of an episode is
/// re-opened at once; each further one waits twice as many drains as the
/// last (1, 2, 4, … up to [`REOPEN_BACKOFF_MAX_SKIPS`]). The episode ends,
/// and the backoff resets, once the re-opened browse has stayed open for
/// [`REOPEN_HEALTHY_DRAINS`] drains in a row, the first of which delivered an
/// event: the daemon ran the browse, and it held.
#[derive(Debug, Default)]
struct ReopenBackoff {
    /// Drains of the closed browse still to skip before the next attempt.
    skip: u32,
    /// The skip set after the last attempt; 0 while no episode is running.
    last: u32,
    /// Open drains in a row since the re-opened receiver first delivered an
    /// event; 0 until it has.
    healthy: u32,
}

impl ReopenBackoff {
    /// Whether a drain that found the browse closed should try to re-open it
    /// now. A `false` uses up one skipped drain.
    fn ready(&mut self) -> bool {
        if self.skip == 0 {
            return true;
        }
        self.skip -= 1;
        false
    }

    /// Record an attempt (successful or not), lengthening the next wait.
    /// Returns whether it was the first attempt of its episode.
    fn attempted(&mut self) -> bool {
        let first = self.last == 0;
        self.last = if first {
            1
        } else {
            (self.last * 2).min(REOPEN_BACKOFF_MAX_SKIPS)
        };
        self.skip = self.last;
        first
    }

    /// A drain found the browse closed: any healthy run is over.
    fn closed(&mut self) {
        self.healthy = 0;
    }

    /// A drain found the browse open, having taken an event from the current
    /// receiver if `delivered`. Ends the episode once the browse has stayed
    /// open for [`REOPEN_HEALTHY_DRAINS`] drains in a row. Outside an episode
    /// it does nothing.
    fn open_drain(&mut self, delivered: bool) {
        if self.last == 0 || (self.healthy == 0 && !delivered) {
            return;
        }
        self.healthy += 1;
        if self.healthy >= REOPEN_HEALTHY_DRAINS {
            *self = Self::default();
        }
    }
}

impl From<mdns_sd::Error> for Error {
    fn from(e: mdns_sd::Error) -> Self {
        Self::Mdns(e.to_string())
    }
}

/// Where one browse's events are read from.
///
/// In production always the `Receiver` `ServiceDaemon::browse` returned. Its
/// `Sender` lives inside the daemon thread and `flume` is not a dependency of
/// this crate, so a test cannot put an event on such a channel at a moment of
/// its choosing; the test-only variant stands in for it, so the requery
/// switch-over ([`BrowseState::drain`]) can be driven event by event.
enum EventSource {
    /// The receiver `ServiceDaemon::browse` returned.
    Daemon(Receiver<ServiceEvent>),
    /// Test-only scripted channel. `Some(event)` is an event the daemon sent;
    /// `None` is a gap: the read that takes it sees an empty channel, as a
    /// drain that ran just before the daemon sent the next event would.
    #[cfg(test)]
    Scripted(std::sync::mpsc::Receiver<Option<ServiceEvent>>),
}

/// What one read of an [`EventSource`] found.
enum Next {
    /// An event the daemon had queued.
    Event(ServiceEvent),
    /// Nothing queued right now; more may arrive.
    Empty,
    /// Nothing queued, and nothing ever will be: the daemon dropped its
    /// sender. It does that when a requery replaces this receiver (the
    /// expected end of a retiring receiver), when it closes the browse
    /// because this receiver's channel filled, and when it shuts down.
    Closed,
}

impl EventSource {
    /// Read the next queued event without waiting. A closed channel still
    /// yields everything queued before it closed (flume and `std::sync::mpsc`
    /// both drain the queue before reporting the disconnect), so
    /// [`Next::Closed`] only comes once the queue is empty.
    fn try_next(&self) -> Next {
        match self {
            Self::Daemon(receiver) => match receiver.try_recv() {
                Ok(event) => Next::Event(event),
                Err(mdns_sd::TryRecvError::Empty) => Next::Empty,
                Err(mdns_sd::TryRecvError::Disconnected) => Next::Closed,
            },
            #[cfg(test)]
            Self::Scripted(receiver) => match receiver.try_recv() {
                Ok(Some(event)) => Next::Event(event),
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Empty) => Next::Empty,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Next::Closed,
            },
        }
    }

    /// The next queued event, treating "empty" and "closed" alike: for a
    /// retiring receiver both mean nothing more to apply.
    fn try_event(&self) -> Option<ServiceEvent> {
        match self.try_next() {
            Next::Event(event) => Some(event),
            Next::Empty | Next::Closed => None,
        }
    }
}

/// Whether [`BrowseState::drain`] left the browse able to receive more
/// events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
enum DrainOutcome {
    /// The current receiver is open.
    Open,
    /// The current receiver is closed: the daemon dropped this browse (its
    /// channel filled, or the daemon shut down). Nothing more will arrive
    /// until the browse is opened again.
    Closed,
}

/// The single browse we run for one DNS-SD service type, plus the handles
/// sharing it.
///
/// `mdns-sd` gives us exactly one querier (and one `Receiver`) per service
/// type — see the module docs — so this is the unit of ownership, not the
/// [`QueryHandle`]. `pending` doubles as the refcount: the browse lives while
/// it is non-empty.
struct BrowseState {
    /// Where this browse's events come from: the `Receiver` mdns-sd handed
    /// back for this type. Drained by whichever handle polls; the records go
    /// to every handle in `pending`. A [`Discovery::requery`] installs the
    /// receiver of a re-issued browse here and moves this one to `retiring`.
    receiver: EventSource,
    /// The receiver a [`Discovery::requery`] replaced, while the daemon may
    /// still send to it: `daemon.browse` only queues a command, and until the
    /// daemon thread runs it the querier's listener is still this one.
    /// Dropping it then would lose whatever the daemon sends it in between:
    /// mdns-sd removes a querier whose listener is gone, and the queued browse
    /// then installs the new listener on its own. Drained ahead of the new
    /// receiver and dropped at the switch point, its first event
    /// ([`Self::drain`]). The daemon drops its own end in that same step, so
    /// this receiver reads as closed from then on. `Some` also means a
    /// requery is in progress, and another one is then a no-op.
    retiring: Option<EventSource>,
    /// Handles attached to this browse, each with the records surfaced since
    /// that handle last called [`Discovery::poll_results`].
    ///
    /// A buffer grows until its handle polls or is stopped — the same
    /// accumulation the per-handle `Receiver` used to provide, so a caller
    /// that polls in a loop (every caller in this workspace) sees no
    /// difference. What accumulates is *fan-out events*, and mdns-sd emits a
    /// `ServiceResolved` only when a record actually changes (`DnsCache::
    /// add_or_update` reports `updated == false` for an unchanged record and
    /// never pushes it into `changes`), so a never-polled handle grows with
    /// record churn on the link, not with elapsed time.
    ///
    /// A [`Discovery::requery`] adds to that: mdns-sd replays every cached
    /// resolved record of the type to the re-issued browse, so a never-polled
    /// handle also grows by up to one record per instance per requery.
    pending: HashMap<QueryHandle, Vec<MatterService>>,
    /// The last record surfaced for each instance name, so a handle attached
    /// *after* a record was surfaced can be seeded with it.
    ///
    /// It mirrors the daemon's **live** cache rather than everything ever
    /// seen: an instance is removed when mdns-sd reports `ServiceRemoved` for
    /// it (its PTR/SRV expired, or it sent a goodbye) and re-added by its next
    /// `ServiceResolved`. Without the removal, a browse held open for a long
    /// time would keep replaying an offline device's last record as if it were
    /// current. A removal never touches `pending`: what a handle was already
    /// handed stays handed.
    ///
    /// This is our own replay cache and it exists because mdns-sd gives us
    /// none: skipping `daemon.browse` for a type we already browse (which the
    /// refcount requires — a second `browse` would orphan the first receiver)
    /// also skips the cache replay a fresh `browse` performs, and the daemon
    /// will not re-announce a record that has not changed. Without this map a
    /// late-attaching handle sees **nothing** for a stable, already-resolved
    /// service — a starvation the refcount would otherwise have introduced.
    ///
    /// Keyed by instance name, so it holds at most one entry per distinct
    /// Matter instance on the link no matter how often that instance
    /// re-announces: the replay handed to a new handle is bounded by the size
    /// of the network, not by how long the browse has been open.
    surfaced: HashMap<String, MatterService>,
    /// Handles attached to this browse, each with the instance names of the
    /// `ServiceFound` events (a NEW PTR record) since that handle last called
    /// [`Discovery::poll_found`].
    ///
    /// Inserted and removed together with `pending`, but deliberately NOT
    /// seeded on attach: a found event is an event, not state, so a handle
    /// attached after it never sees it. Each buffer is capped at
    /// [`FOUND_BUFFER_CAP`], oldest dropped.
    found: HashMap<QueryHandle, Vec<String>>,
    /// The instances this browse has reported as found (`ServiceFound`) and
    /// not seen removed (`ServiceRemoved`) since: the PTRs it believes the
    /// daemon holds. A `ServiceFound` for a name already here re-delivers a
    /// PTR the daemon already held — mdns-sd replays every cached PTR to a
    /// re-issued browse — so it is not buffered for
    /// [`Discovery::poll_found`].
    ///
    /// Keyed exactly like `surfaced` ([`instance_name_from_fullname`]) and
    /// **verbatim, not lowercased**: mdns-sd compares PTR aliases
    /// case-sensitively (`dns_parser.rs` 876-878) and its `ServiceFound` /
    /// `ServiceRemoved` carry the cached alias verbatim, so two spellings are
    /// two daemon cache entries and must be two entries here.
    ///
    /// A genuine return is never suppressed: every way a PTR leaves the
    /// daemon's cache (TTL expiry or goodbye, an expired incoming record, an
    /// interface going away) emits `ServiceRemoved`, which removes the name;
    /// stopping the browse drops this state with it; and stopping a base-type
    /// browse wipes only base PTRs, never a subtype browse's.
    present: HashSet<String>,
    /// When a browse the daemon closed is re-opened next, so one that cannot
    /// stay open is not re-opened, and logged, on every poll.
    reopen_backoff: ReopenBackoff,
    /// Test-only queue of events "sent by the daemon" but not yet drained, so a
    /// test can check that `poll_results` and `poll_found` share one drain. The
    /// real `Receiver`'s sender lives inside the daemon and cannot be written
    /// to from a test.
    #[cfg(test)]
    injected: Vec<ServiceEvent>,
}

impl BrowseState {
    /// A browse just opened on `receiver`, with no handles attached yet.
    fn new(receiver: EventSource) -> Self {
        Self {
            receiver,
            retiring: None,
            pending: HashMap::new(),
            surfaced: HashMap::new(),
            found: HashMap::new(),
            present: HashSet::new(),
            reopen_backoff: ReopenBackoff::default(),
            #[cfg(test)]
            injected: Vec::new(),
        }
    }

    /// Start a requery's switch-over: `receiver`, from a fresh `daemon.browse`
    /// of this browse's string, becomes the current receiver, and the old one
    /// retires until [`Self::drain`] sees the switch point. The per-handle
    /// buffers, `surfaced` and `present` are untouched.
    fn begin_requery(&mut self, receiver: EventSource) {
        let old = std::mem::replace(&mut self.receiver, receiver);
        self.retiring = Some(old);
    }

    /// Drain every event queued for this browse and fan each out to every
    /// attached handle ([`fan_out_event`]): resolved records into `pending`
    /// (and `surfaced`), found events into `found`.
    ///
    /// While a requery is in progress (`retiring` is `Some`), the order is
    /// what makes the swap lossless.
    /// mdns-sd sends `SearchStarted` to a new listener first, in the same
    /// daemon step that cancels the old query chain and installs the new
    /// listener in place of the old one (`exec_command_browse`, mdns-sd 0.21.6
    /// `service_daemon.rs` 4086, then 4093 and 4097), so every event the
    /// daemon sends to the old receiver precedes the new receiver's first
    /// event, and nothing reaches the old receiver after it: the daemon has
    /// dropped every sender to it. The steps:
    ///
    /// 1. drain `retiring`;
    /// 2. if the new receiver is still empty, the daemon has not run the
    ///    browse command and the old receiver is still the live listener:
    ///    keep it and stop;
    /// 3. otherwise the new receiver's first event — of any kind — is the
    ///    switch point: drain `retiring` once more (for what the daemon sent
    ///    after step 1 read it), drop it, then apply that event and the rest
    ///    of the new receiver. A first event other than `SearchStarted` is
    ///    traced, so a future mdns-sd reordering is visible.
    ///
    /// So a stale `ServiceRemoved` on the old receiver is always applied
    /// before a genuine `ServiceFound` on the new one and can never erase it.
    ///
    /// Returns [`DrainOutcome::Closed`] when the current receiver turns out to
    /// be closed, once everything queued on it has been applied. A retiring
    /// receiver closing is not reported: the daemon drops the old sender when
    /// it installs the new one, so that is the switch-over itself.
    fn drain(&mut self, service_type: &str) -> DrainOutcome {
        #[cfg(test)]
        let injected = std::mem::take(&mut self.injected);
        let Self {
            receiver,
            retiring,
            pending,
            surfaced,
            found,
            present,
            reopen_backoff,
            ..
        } = self;
        let mut apply = |event: ServiceEvent| {
            fan_out_event(service_type, pending, surfaced, found, present, event);
        };
        // Whether `receiver` is (now) the daemon's live listener for the type.
        let mut switched = true;
        let mut outcome = DrainOutcome::Open;
        // Whether the current receiver delivered anything in this drain.
        let mut delivered = false;
        if let Some(old) = retiring.as_ref() {
            // 1. Everything the daemon has sent the old listener so far.
            while let Some(event) = old.try_event() {
                apply(event);
            }
            match receiver.try_next() {
                Next::Event(first) => {
                    // 3. The switch point. Nothing reaches the old receiver
                    //    after this one was sent, but events may have since
                    //    step 1.
                    while let Some(event) = old.try_event() {
                        apply(event);
                    }
                    if !matches!(first, ServiceEvent::SearchStarted(_)) {
                        tracing::trace!(
                            target: LOG_TARGET,
                            browse = service_type,
                            first = ?first,
                            "mDNS requery: the new receiver's first event is not SearchStarted; \
                             switching over on it anyway",
                        );
                    }
                    apply(first);
                    delivered = true;
                }
                // 2. The daemon has not run the browse command yet.
                Next::Empty => switched = false,
                // The new receiver closed before it delivered anything: the
                // daemon is gone, or dropped the browse. Take what the old
                // receiver still holds; the read below reports the closure.
                Next::Closed => {
                    while let Some(event) = old.try_event() {
                        apply(event);
                    }
                }
            }
        }
        if switched {
            *retiring = None;
            loop {
                match receiver.try_next() {
                    Next::Event(event) => {
                        apply(event);
                        delivered = true;
                    }
                    Next::Empty => break,
                    Next::Closed => {
                        outcome = DrainOutcome::Closed;
                        break;
                    }
                }
            }
        }
        #[cfg(test)]
        for event in injected {
            apply(event);
        }
        if outcome == DrainOutcome::Open {
            reopen_backoff.open_drain(delivered);
        }
        outcome
    }

    /// Install `receiver`, from a fresh `daemon.browse` of this browse's
    /// string, in place of a receiver the daemon closed
    /// ([`DrainOutcome::Closed`]).
    ///
    /// The closure dropped events, so `present` and `surfaced` may no longer
    /// match the daemon's cache: an instance may have left, or left and come
    /// back, unseen. Both are cleared and rebuilt by the fresh browse's
    /// replay, which mdns-sd sends from the cache it keeps across the
    /// closure: every cached instance is reported found once more (a spurious
    /// return costs the controller at most one advert pull, the safe
    /// direction) and every cached record is re-delivered. Per-handle buffers
    /// are untouched: what a handle was handed stays handed.
    fn reopen(&mut self, receiver: EventSource) {
        self.receiver = receiver;
        self.retiring = None;
        self.present.clear();
        self.surfaced.clear();
    }
}

/// Default mDNS discovery adapter for Matter, backed by `mdns-sd`.
///
/// Construct via [`Self::new`] (the 90% path — owns its own daemon)
/// or [`Self::with_daemon`] (advanced: inject a pre-configured daemon).
///
/// Handles of the same [`ServiceKind`] share one browse and each receive
/// every record it surfaces; see the module docs for why that is forced on us
/// by mdns-sd's per-type querier.
pub struct MdnsSdDiscovery {
    daemon: ServiceDaemon,
    owns_daemon: bool,
    /// One entry per *service type* with at least one live handle. Keyed by the
    /// browse string rather than [`ServiceKind`] because a Matter
    /// compressed-fabric subtype
    /// (`_I<id>._sub._matter._tcp.local.`, [`Discovery::query_operational_fabric`])
    /// is a distinct browse key from its base type — which is exactly what lets
    /// the two coexist under the refcount.
    browses: HashMap<String, BrowseState>,
    /// Which browse each live handle belongs to. Absent ⇒ stopped/unknown.
    handle_types: HashMap<QueryHandle, String>,
    next_handle: u64,
    /// Test-only observation of the daemon calls this adapter makes, so a test
    /// can assert "browsed once per type" and "stopped only on the last
    /// release" without a public API or a live network.
    #[cfg(test)]
    browse_calls: HashMap<String, u32>,
    #[cfg(test)]
    stop_browse_calls: HashMap<String, u32>,
}

impl MdnsSdDiscovery {
    /// Spawn a fresh internal [`ServiceDaemon`]. Simplest path.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Mdns`] if daemon creation fails (typically when
    /// the host has no mDNS support or port 5353 is unavailable).
    pub fn new() -> Result<Self> {
        let daemon = ServiceDaemon::new()?;
        Ok(Self::from_daemon(daemon, true))
    }

    /// Reuse an externally-managed [`ServiceDaemon`] — e.g. one configured
    /// with specific interfaces enabled.
    ///
    /// # Sharing one daemon across adapters is unsafe for browsing
    ///
    /// The refcounting described in the module docs is **per
    /// `MdnsSdDiscovery` instance**, while mdns-sd's querier is per *daemon*
    /// and keyed by service type. Two `MdnsSdDiscovery` values built over the
    /// same daemon that browse the same [`ServiceKind`] therefore still
    /// clobber each other exactly as two raw `browse` calls would: the second
    /// instance's `browse` orphans the first's receiver, and its `stop_query`
    /// tears down the browse the first still depends on.
    ///
    /// So: share a daemon freely for *publishing*, but give each component
    /// that browses its own adapter over its own daemon — or, better, share
    /// the one `MdnsSdDiscovery` instance itself (its handles are designed to
    /// coexist). Sharing a daemon between one browsing adapter and any number
    /// of publish-only adapters is also fine.
    #[must_use]
    pub fn with_daemon(daemon: ServiceDaemon) -> Self {
        Self::from_daemon(daemon, false)
    }

    fn from_daemon(daemon: ServiceDaemon, owns_daemon: bool) -> Self {
        Self {
            daemon,
            owns_daemon,
            browses: HashMap::new(),
            handle_types: HashMap::new(),
            next_handle: 1,
            #[cfg(test)]
            browse_calls: HashMap::new(),
            #[cfg(test)]
            stop_browse_calls: HashMap::new(),
        }
    }

    fn allocate_handle(&mut self) -> QueryHandle {
        let h = QueryHandle(self.next_handle);
        self.next_handle = self.next_handle.wrapping_add(1);
        if self.next_handle == 0 {
            self.next_handle = 1;
        }
        h
    }

    /// DNS-SD instance fullname for a `(instance_name, kind)` pair.
    fn fullname(instance_name: &str, kind: ServiceKind) -> String {
        format!("{}.{}", instance_name, kind.service_type())
    }

    /// Attach a handle to `service_type`'s browse, opening the browse only if
    /// this is the first handle for that exact string. Shared by
    /// [`Discovery::query`] (base types) and
    /// [`Discovery::query_operational_fabric`] (compressed-fabric subtypes) —
    /// the two differ only in the string, so refcounting, fan-out and replay are
    /// identical for both.
    ///
    /// A second `daemon.browse` of a string we are already browsing would
    /// *replace* the daemon's querier for it and orphan the receiver every
    /// existing handle of that string reads from (module docs, issue #113), so
    /// the reuse here is a correctness requirement, not an optimisation. Note
    /// that mdns-sd keys its querier by the browsed string too, so a subtype and
    /// its base type are separate queriers and neither disturbs the other.
    ///
    /// Reusing the browse costs the new handle mdns-sd's cache replay — that
    /// only happens on a real `browse` call — and the daemon will not re-emit a
    /// record that has not changed, so a handle attaching to a stable,
    /// already-resolved service would otherwise wait out its whole budget and
    /// see nothing. It is therefore seeded from this adapter's own record of
    /// the last service surfaced per instance name (`BrowseState::surfaced`),
    /// which restores the replay without a second browse.
    fn query_service_type(&mut self, service_type: String) -> Result<QueryHandle> {
        // Open the browse BEFORE allocating a handle so a daemon failure
        // leaves no state behind.
        let receiver = if self.browses.contains_key(&service_type) {
            None
        } else {
            let receiver = self.daemon.browse(&service_type)?;
            #[cfg(test)]
            self.browse_calls
                .entry(service_type.clone())
                .and_modify(|n| *n += 1)
                .or_insert(1);
            tracing::debug!(
                target: LOG_TARGET,
                service_type = %service_type,
                "mDNS browse started",
            );
            Some(receiver)
        };
        if let Some(receiver) = receiver {
            self.browses.insert(
                service_type.clone(),
                BrowseState::new(EventSource::Daemon(receiver)),
            );
        }
        let handle = self.allocate_handle();
        let Some(browse) = self.browses.get_mut(&service_type) else {
            // Unreachable: the entry was either already present or inserted
            // just above. Reported as an error rather than asserted, because a
            // handle with no browse behind it could only ever return nothing.
            return Err(Error::Mdns(format!(
                "internal: no browse state for {service_type} after opening it"
            )));
        };
        // Seed with what this browse has already surfaced. Empty for a browse
        // opened just above; for a reused one this is the replay mdns-sd will
        // not give us (see the method docs). Each record is handed to this
        // handle exactly once, as if it had been attached all along.
        let replayed: Vec<MatterService> = browse.surfaced.values().cloned().collect();
        let replayed_count = replayed.len();
        browse.pending.insert(handle, replayed);
        // No replay for found events: a new handle starts with an empty buffer.
        browse.found.insert(handle, Vec::new());
        let handles = browse.pending.len();
        tracing::debug!(
            target: LOG_TARGET,
            service_type = %service_type,
            handle = handle.0,
            handles,
            replayed = replayed_count,
            "mDNS browse handle attached",
        );
        self.handle_types.insert(handle, service_type);
        Ok(handle)
    }

    /// Test seam: hand a browse event to the `kind` browse exactly as a
    /// [`Discovery::poll_results`] drain would.
    ///
    /// The browse `Receiver` is a re-exported `flume::Receiver` whose `Sender`
    /// lives inside the daemon, so a test cannot put an event on the real
    /// channel, and CI hosts do not deliver loopback mDNS (see the `#[ignore]`
    /// on `self_publish_self_discover`). This bypasses **only** the delivery
    /// of the event: translation, fan-out to every attached handle, buffering
    /// and per-handle consumption all run the production code.
    ///
    /// No-op when no browse is open for `kind` — which is itself the assertion
    /// some tests make.
    #[cfg(test)]
    fn deliver_for_test(&mut self, kind: ServiceKind, event: ServiceEvent) {
        self.deliver_for_type_for_test(kind.service_type(), event);
    }

    /// As [`Self::deliver_for_test`], but naming the browse by its exact string
    /// so a test can deliver to a compressed-fabric subtype browse (which has no
    /// [`ServiceKind`] of its own).
    #[cfg(test)]
    fn deliver_for_type_for_test(&mut self, service_type: &str, event: ServiceEvent) {
        if let Some(browse) = self.browses.get_mut(service_type) {
            fan_out_event(
                service_type,
                &mut browse.pending,
                &mut browse.surfaced,
                &mut browse.found,
                &mut browse.present,
                event,
            );
        }
    }

    /// Test seam: queue `event` on the `service_type` browse as if the daemon
    /// had sent it, WITHOUT fanning it out. Unlike
    /// [`Self::deliver_for_type_for_test`], the event waits for the next drain,
    /// so a test can check that `poll_results` and `poll_found` share one drain.
    /// No-op when no browse is open for `service_type`.
    #[cfg(test)]
    fn queue_for_test(&mut self, service_type: &str, event: ServiceEvent) {
        if let Some(browse) = self.browses.get_mut(service_type) {
            browse.injected.push(event);
        }
    }

    /// Drain every event the daemon has queued for `service_type`'s browse and
    /// fan each out to every attached handle: resolved records into `pending`
    /// (and `surfaced`), found events into `found` — including a requery's
    /// switch-over from its retiring receiver (`BrowseState::drain`).
    ///
    /// Both [`Discovery::poll_results`] and [`Discovery::poll_found`] call this
    /// first. Whichever runs first drains for both, so the order of the two
    /// calls never matters and neither can starve the other of events. Calling
    /// it from both in one pass is safe mid-requery too: the switch-over state
    /// lives in the browse, not in the call.
    ///
    /// # A browse the daemon closed
    ///
    /// mdns-sd closes a browse whose event channel fills: it stops the
    /// querier and drops its sender, and from then on the receiver only
    /// disconnects. Left alone, the browse would look like a quiet network
    /// forever. So when the drain finds the current receiver closed, the
    /// browse is opened again here, with the same string, and the fresh
    /// browse's cache replay rebuilds its state (`BrowseState::reopen`). That
    /// is logged at `warn`: it means events were lost, because this browse
    /// was not polled often enough to keep up.
    ///
    /// A browse that closes again right after it is re-opened is retried with
    /// a backoff counted in drains ([`ReopenBackoff`]), logged at `warn` only
    /// for the first attempt of each episode. If the daemon refuses (it has
    /// shut down), the closed receiver stays in place and a later drain tries
    /// again, on the same backoff.
    ///
    /// Returns [`DrainOutcome::Closed`] when the drain found the browse
    /// closed, whether or not the re-open then succeeded.
    fn drain_events(&mut self, service_type: &str) -> DrainOutcome {
        let Some(browse) = self.browses.get_mut(service_type) else {
            return DrainOutcome::Open;
        };
        if browse.drain(service_type) == DrainOutcome::Open {
            return DrainOutcome::Open;
        }
        browse.reopen_backoff.closed();
        if !browse.reopen_backoff.ready() {
            tracing::debug!(
                target: LOG_TARGET,
                service_type = %service_type,
                "mDNS browse is still closed; waiting before re-opening it again",
            );
            return DrainOutcome::Closed;
        }
        let first = browse.reopen_backoff.attempted();
        match self.daemon.browse(service_type) {
            Ok(receiver) => {
                browse.reopen(EventSource::Daemon(receiver));
                #[cfg(test)]
                self.browse_calls
                    .entry(service_type.to_string())
                    .and_modify(|n| *n += 1)
                    .or_insert(1);
                if first {
                    tracing::warn!(
                        target: LOG_TARGET,
                        service_type = %service_type,
                        handles = browse.pending.len(),
                        "mDNS browse closed by the daemon (events were lost: it was not polled \
                         often enough to keep up); re-opened",
                    );
                } else {
                    tracing::debug!(
                        target: LOG_TARGET,
                        service_type = %service_type,
                        "mDNS browse closed again; re-opened",
                    );
                }
            }
            Err(e) if first => {
                tracing::warn!(
                    target: LOG_TARGET,
                    service_type = %service_type,
                    error = %e,
                    "mDNS browse closed by the daemon, and re-opening it failed; \
                     retrying with a backoff",
                );
            }
            Err(e) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    service_type = %service_type,
                    error = %e,
                    "mDNS browse is still closed; re-opening it failed again",
                );
            }
        }
        DrainOutcome::Closed
    }
}

impl Drop for MdnsSdDiscovery {
    fn drop(&mut self) {
        if self.owns_daemon {
            let _ = self.daemon.shutdown();
        }
    }
}

impl Discovery for MdnsSdDiscovery {
    fn publish(&mut self, service: &MatterService) -> Result<()> {
        // ServiceInfo::new requires host_name. Synthesize from
        // instance_name (mdns-sd appends .local. automatically).
        let host = format!("{}.local.", service.instance_name);
        let addrs: Vec<IpAddr> = service.addresses.clone();
        // TXT values are raw bytes; build `TxtProperty` entries that carry
        // them verbatim (mdns-sd's `From<(K, V: AsRef<[u8]>)>`) rather than
        // the `HashMap<String, String>` path, which would force a UTF-8
        // value and drop binary-valued keys.
        let props: Vec<TxtProperty> = service
            .txt_records
            .iter()
            .map(|(k, v)| TxtProperty::from((k.clone(), v.clone())))
            .collect();
        let info = ServiceInfo::new(
            service.kind.service_type(),
            &service.instance_name,
            &host,
            &addrs[..],
            service.port,
            props,
        )?;
        self.daemon.register(info)?;
        Ok(())
    }

    fn unpublish(&mut self, instance_name: &str, kind: ServiceKind) -> Result<()> {
        let fullname = Self::fullname(instance_name, kind);
        // unregister returns a Receiver we drop — the daemon completes
        // the unregister asynchronously.
        let _ = self.daemon.unregister(&fullname)?;
        Ok(())
    }

    /// Attach a handle to this service type's browse, opening the browse only
    /// if this is the first handle for the type. The refcount/replay rules
    /// (one browse per service-type *string*, fan-out to every attached handle,
    /// replay for a late-attaching one) live in the private
    /// `query_service_type` and are shared with the subtype browse.
    fn query(&mut self, kind: ServiceKind) -> Result<QueryHandle> {
        self.query_service_type(kind.service_type().to_string())
    }

    /// Browse the fabric's DNS-SD compressed-fabric subtype
    /// (`_I<compressed-fabric-id>._sub._matter._tcp.local.`) instead of every
    /// operational node on the link.
    ///
    /// mdns-sd accepts a subtype string in `browse` (it validates only the
    /// `._tcp.local.` suffix) and keys its querier, PTR cache and `stop_browse`
    /// by that exact string, so this browse is fully independent of a
    /// concurrent base-type browse: both can be open at once, each gets its own
    /// records, and stopping one leaves the other alone. Records arrive with
    /// `ty_domain` set to the *subtype*; translation strips the
    /// `._sub.` prefix so they surface as ordinary
    /// [`ServiceKind::Operational`] records, indistinguishable to the caller
    /// from base-type ones.
    ///
    /// Why prefer it: mdns-sd resolves discovered instances one at a time with
    /// an exponential backoff, so on a link with many operational nodes a
    /// base-type browse can spend a caller's entire budget without ever
    /// resolving the wanted node (issue #113). The subtype narrows the browse to
    /// the controller's own fabric.
    ///
    /// # Errors
    ///
    /// - [`Error::Mdns`] on daemon failure.
    fn query_operational_fabric(&mut self, compressed_fabric_id: [u8; 8]) -> Result<QueryHandle> {
        self.query_service_type(operational_fabric_subtype(compressed_fabric_id))
    }

    /// Release one handle. The underlying browse is stopped only when its
    /// **last** handle goes away.
    ///
    /// Note that `mdns_sd::ServiceDaemon::stop_browse` also drops the daemon's
    /// cached records for the service type, so releasing the last handle
    /// usually discards what the browse had learned; the next `query` for that
    /// type then starts from an empty cache and must wait for fresh
    /// announcements. That is why stopping is deferred to the last release,
    /// and why a caller that reconnects repeatedly is better off holding one
    /// handle open.
    ///
    /// "Usually": the receiver is dropped here, and `stop_browse` only queues
    /// a command. If the daemon pushes an event for the type in between, or
    /// had already closed the browse because its channel filled, mdns-sd
    /// removes the querier without dropping the cache, and the stop then
    /// finds nothing to stop. The next `query` for the type is then replayed
    /// the records still within their TTL: ordinary mDNS caching, reported as
    /// found once more (the safe direction).
    fn stop_query(&mut self, handle: QueryHandle) {
        // Unknown or already-stopped handle: no-op.
        let Some(service_type) = self.handle_types.remove(&handle) else {
            return;
        };
        let remaining = match self.browses.get_mut(&service_type) {
            Some(browse) => {
                browse.pending.remove(&handle);
                browse.found.remove(&handle);
                browse.pending.len()
            }
            None => 0,
        };
        tracing::debug!(
            target: LOG_TARGET,
            service_type = %service_type,
            handle = handle.0,
            remaining,
            "mDNS browse handle released",
        );
        if remaining == 0 {
            self.browses.remove(&service_type);
            // Per-string, so releasing a fabric-subtype browse never touches a
            // base-type browse (or another fabric's subtype) still in use.
            let _ = self.daemon.stop_browse(&service_type);
            #[cfg(test)]
            self.stop_browse_calls
                .entry(service_type.clone())
                .and_modify(|n| *n += 1)
                .or_insert(1);
            tracing::debug!(
                target: LOG_TARGET,
                service_type = %service_type,
                "mDNS browse stopped (last handle released; mdns-sd also drops its cached records for this type)",
            );
        }
    }

    /// Drain the shared browse once and return **this** handle's share.
    ///
    /// Every record drained here is appended to the buffer of *every* handle
    /// attached to the same service type, then this handle's buffer is taken
    /// and returned. So the documented contract still holds — a record is
    /// returned to a given handle exactly once — while a second handle on the
    /// same type no longer steals records the first is waiting for.
    fn poll_results(&mut self, handle: QueryHandle) -> Vec<MatterService> {
        let Some(service_type) = self.handle_types.get(&handle).cloned() else {
            return Vec::new();
        };
        let _ = self.drain_events(&service_type);
        // `handle_types` and `pending` are inserted and removed together, so a
        // missing buffer is unreachable; empty is the safe answer either way.
        self.browses
            .get_mut(&service_type)
            .and_then(|browse| browse.pending.get_mut(&handle))
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Drain the shared browse once and return **this** handle's found events:
    /// the instance names mdns-sd reported as `ServiceFound` (a new PTR record
    /// entering its cache) since this handle's last call — except an instance
    /// this browse already reported and has not seen `ServiceRemoved` for
    /// since: that `ServiceFound` re-delivers a PTR the daemon already held.
    /// Instance names are compared verbatim, as mdns-sd compares them.
    ///
    /// Shares one drain with [`Discovery::poll_results`], so the order of the
    /// two calls never matters. A handle attached after a `ServiceFound` does
    /// not receive it (found events are not replayed, unlike resolved records).
    /// Each handle buffers at most `FOUND_BUFFER_CAP` (256) names, oldest
    /// dropped, so a handle whose owner never calls this costs bounded memory.
    ///
    /// One imprecision is accepted: mdns-sd also sends `ServiceRemoved` when an
    /// instance's SRV or address records expire while its PTR is still cached,
    /// so the next [`Discovery::requery`] replays that PTR and it is reported
    /// once more. For the controller that is at most one spurious advert pull
    /// per outage, bounded by its 30 s cooldown, in the safe direction.
    fn poll_found(&mut self, handle: QueryHandle) -> Vec<String> {
        let Some(service_type) = self.handle_types.get(&handle).cloned() else {
            return Vec::new();
        };
        let _ = self.drain_events(&service_type);
        self.browses
            .get_mut(&service_type)
            .and_then(|browse| browse.found.get_mut(&handle))
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Re-issue this handle's browse with `daemon.browse`, so mdns-sd sends a
    /// fresh PTR query now instead of at the next step of its own doubling
    /// schedule (which reaches an hour). The browse is shared by every handle
    /// of its service type, so this re-queries for all of them; their buffers
    /// are untouched.
    ///
    /// # The swap loses nothing
    ///
    /// `daemon.browse` only queues a command: until the daemon thread runs it,
    /// the querier still sends to the current receiver. That receiver is
    /// therefore kept (retiring) and drained ahead of anything the new one
    /// delivers, until the new one's first event (see the module docs). A
    /// requery while such a switch-over is still pending, or for an unknown or
    /// stopped handle, does nothing; if the daemon refuses the browse, nothing
    /// changes either (logged at `debug`). The daemon cancels the old query
    /// chain when it runs the new browse. A requery first drains the browse,
    /// and if the daemon has closed it, re-opens it instead (see
    /// `drain_events`): that is a fresh browse already.
    ///
    /// # What it costs
    ///
    /// mdns-sd replays every cached PTR of the type as `ServiceFound` plus
    /// `ServiceResolved`. Found replays of instances already present are not
    /// reported ([`Discovery::poll_found`]); resolved replays are re-delivered
    /// to every handle by [`Discovery::poll_results`] (latest wins). The
    /// replay is up to `1 + 2N` events, queued until the caller drains them,
    /// on a channel that holds 1000: one that fills is closed, and the next
    /// drain re-opens it (see the module docs), so on a very
    /// large fabric poll briskly for a while after a requery. The restarted
    /// query chain sends about five small multicast PTR queries in the first
    /// 15 s and about nine in the first 5 minutes, each carrying known answers
    /// (the cached PTRs not past half their TTL, so devices whose fresh PTR
    /// the daemon holds stay silent), and with each one resolve queries (SRV
    /// and TXT, or A and AAAA) for every instance cached but not yet resolved.
    fn requery(&mut self, handle: QueryHandle) {
        let Some(service_type) = self.handle_types.get(&handle).cloned() else {
            return;
        };
        // Drain first, so a browse the daemon has closed is seen as closed.
        // Retired, its receiver would read as the old listener of an ordinary
        // switch-over, and the closure would go unnoticed. A re-open issues a
        // fresh browse, which is what a requery does, so it is not repeated.
        if self.drain_events(&service_type) == DrainOutcome::Closed {
            return;
        }
        let service_type = &service_type;
        let Some(browse) = self.browses.get_mut(service_type) else {
            return;
        };
        if browse.retiring.is_some() {
            tracing::debug!(
                target: LOG_TARGET,
                service_type = %service_type,
                "mDNS requery skipped: the previous one has not switched over yet",
            );
            return;
        }
        match self.daemon.browse(service_type) {
            Ok(receiver) => {
                browse.begin_requery(EventSource::Daemon(receiver));
                #[cfg(test)]
                self.browse_calls
                    .entry(service_type.clone())
                    .and_modify(|n| *n += 1)
                    .or_insert(1);
                tracing::debug!(
                    target: LOG_TARGET,
                    service_type = %service_type,
                    handles = browse.pending.len(),
                    "mDNS requery: browse re-issued",
                );
            }
            Err(e) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    service_type = %service_type,
                    error = %e,
                    "mDNS requery failed; the browse continues unchanged",
                );
            }
        }
    }
}

/// Translate one browse event and append the resulting record to the buffer of
/// **every** handle attached to that browse, recording it in `surfaced` so a
/// handle attached later can be seeded with it ([`Discovery::query`]).
///
/// Split out of [`Discovery::poll_results`] so the fan-out has one
/// implementation, shared with the test seam that delivers synthetic events
/// (mdns-sd's browse `Sender` lives inside the daemon, so an event cannot
/// otherwise be injected without a live multicast network).
///
/// `browse` is the exact string this browse was opened with, logged with every
/// surfaced record so a trace shows **which** browse produced it — base type or
/// compressed-fabric subtype (issue #113). A `ServiceFound` is also appended,
/// as an instance name, to every handle's found buffer unless the instance is
/// already in `present` ([`buffer_found`]), and a `ServiceRemoved` drops its
/// instance from `surfaced` and `present` (never from a handle's buffer).
fn fan_out_event(
    browse: &str,
    pending: &mut HashMap<QueryHandle, Vec<MatterService>>,
    surfaced: &mut HashMap<String, MatterService>,
    found: &mut HashMap<QueryHandle, Vec<String>>,
    present: &mut HashSet<String>,
    event: ServiceEvent,
) {
    if let ServiceEvent::ServiceFound(_, fullname) = &event {
        buffer_found(browse, present, found, fullname);
    }
    // The daemon no longer holds this instance, so the replay cache must stop
    // offering it to late handles, and its next `ServiceFound` is a genuine
    // return. Same split as `buffer_found` and the resolved-record path, so the
    // key matches the one `surfaced` and `present` were filled under. Still
    // traced below with every other non-resolved event.
    if let ServiceEvent::ServiceRemoved(_, fullname) = &event {
        if let Some(instance) = instance_name_from_fullname(fullname) {
            surfaced.remove(&instance);
            present.remove(&instance);
        }
    }
    match event {
        ServiceEvent::ServiceResolved(info) => {
            // `service_info_to_matter` logs its own drop reason, so a
            // `None` here is already accounted for in the trace.
            if let Some(svc) = service_info_to_matter(&info) {
                tracing::debug!(
                    target: LOG_TARGET,
                    browse,
                    subtype_browse = is_subtype(browse),
                    instance = %svc.instance_name,
                    kind = ?svc.kind,
                    addresses = ?svc.addresses,
                    port = svc.port,
                    handles = pending.len(),
                    "mDNS record surfaced",
                );
                for buffer in pending.values_mut() {
                    buffer.push(svc.clone());
                }
                // Latest wins: a re-announce with new addresses replaces the
                // entry rather than adding one, which is what keeps this map
                // bounded by the number of instances on the link.
                surfaced.insert(svc.instance_name.clone(), svc);
            }
        }
        // Other events (ServiceFound, ServiceRemoved, SearchStarted,
        // SearchStopped, etc.) produce no record — poll_results only emits
        // usable (address-resolved) services. ServiceFound was buffered above
        // for poll_found; every one of them is still traced, because a
        // `ServiceFound` never followed by a `ServiceResolved` for the same
        // instance is the signature of the resolver failing to complete
        // SRV/address resolution, and is otherwise invisible.
        other => trace_unused_event(&other),
    }
}

/// Append the instance named by a `ServiceFound` fullname to every attached
/// handle's found buffer, dropping a buffer's oldest name first once it holds
/// [`FOUND_BUFFER_CAP`] — unless the instance is already in `present`.
///
/// mdns-sd reports the PTR's alias, `<instance>.<service-type>.local.`; for a
/// subtype browse that is the base-type instance fullname, so the same
/// [`instance_name_from_fullname`] split applies to both kinds of browse. A
/// fullname with no instance label is dropped (logged at `debug`), never
/// buffered as an empty name. An instance already in `present` was reported
/// and has not been removed since, so this `ServiceFound` re-delivers a PTR
/// the daemon already held: it is suppressed (logged at `trace`).
fn buffer_found(
    browse: &str,
    present: &mut HashSet<String>,
    found: &mut HashMap<QueryHandle, Vec<String>>,
    fullname: &str,
) {
    let Some(instance) = instance_name_from_fullname(fullname) else {
        tracing::debug!(
            target: LOG_TARGET,
            browse,
            fullname = %fullname,
            "mDNS ServiceFound ignored: malformed fullname (no leading instance label)",
        );
        return;
    };
    if !present.insert(instance.clone()) {
        tracing::trace!(
            target: LOG_TARGET,
            browse,
            instance = %instance,
            "mDNS ServiceFound suppressed: already present (a replay of a cached PTR)",
        );
        return;
    }
    for (handle, buffer) in found {
        if buffer.len() >= FOUND_BUFFER_CAP {
            let dropped = buffer.remove(0);
            tracing::trace!(
                target: LOG_TARGET,
                browse,
                handle = handle.0,
                dropped = %dropped,
                cap = FOUND_BUFFER_CAP,
                "mDNS found-event buffer full; dropped the oldest",
            );
        }
        buffer.push(instance.clone());
    }
}

/// Trace a browse event `poll_results` does not turn into a [`MatterService`],
/// naming the variant and (where the variant carries one) the instance it
/// concerns.
///
/// `ServiceEvent` is `#[non_exhaustive]`, so the catch-all arm is required and
/// keeps a future variant visible as its `Debug` form rather than silent.
fn trace_unused_event(event: &ServiceEvent) {
    match event {
        ServiceEvent::SearchStarted(ty) => {
            tracing::trace!(target: LOG_TARGET, service_type = %ty, "mDNS event: SearchStarted");
        }
        ServiceEvent::ServiceFound(ty, fullname) => {
            tracing::trace!(
                target: LOG_TARGET,
                service_type = %ty,
                fullname = %fullname,
                "mDNS event: ServiceFound (awaiting ServiceResolved)",
            );
        }
        ServiceEvent::ServiceRemoved(ty, fullname) => {
            tracing::trace!(
                target: LOG_TARGET,
                service_type = %ty,
                fullname = %fullname,
                "mDNS event: ServiceRemoved",
            );
        }
        ServiceEvent::SearchStopped(ty) => {
            tracing::trace!(target: LOG_TARGET, service_type = %ty, "mDNS event: SearchStopped");
        }
        other => {
            tracing::trace!(target: LOG_TARGET, event = ?other, "mDNS event: unhandled variant");
        }
    }
}

/// Translate an mdns-sd `ResolvedService` into our `MatterService`.
/// Returns `None` (skipping the record) if:
/// - the service has no resolved addresses (the spec guarantees
///   `ServiceResolved` only fires once at least one address is known, but
///   we double-check defensively);
/// - the service type is not one we recognise;
/// - the `fullname` has no dot, so the leading instance label cannot be
///   split off. A well-formed DNS-SD fullname is
///   `<instance>.<service-type>.local.`, so a dotless name is malformed —
///   we skip it rather than fabricate a service with an empty
///   `instance_name` (which would later fail to commission/resolve).
///
/// Every one of those drops is logged (see the module's Diagnostics section)
/// with the value that caused it, because a dropped record is indistinguishable
/// from an absent one at the caller.
///
/// `ResolvedService` replaced `ServiceInfo` here in mdns-sd 0.20: the
/// resolver now emits a plain data struct rather than the
/// builder-style cert. Field access shifts from `info.get_*()` to
/// public fields (`info.fullname`, `info.port`, `info.addresses`,
/// `info.ty_domain`, `info.txt_properties`). The `addresses` field
/// also changed to `HashSet<ScopedIp>`; we flatten each entry into
/// `IpAddr` via `ScopedIp::to_ip_addr` since matter-transport doesn't
/// (yet) thread interface scope through.
fn service_info_to_matter(info: &ResolvedService) -> Option<MatterService> {
    let addresses: Vec<IpAddr> = info
        .addresses
        .iter()
        .map(mdns_sd::ScopedIp::to_ip_addr)
        .collect();
    if addresses.is_empty() {
        tracing::debug!(
            target: LOG_TARGET,
            fullname = %info.fullname,
            "mDNS record dropped: resolved with no addresses",
        );
        return None;
    }
    let Some(kind) = kind_from_service_type(&info.ty_domain) else {
        // A record surfaced under a Matter subtype (`_I<compressed-fabric>.
        // _sub._matter._tcp.local.`) is accepted — the subtype is stripped
        // first — so what dies here is a genuinely foreign service type.
        // Logging the actual value is what makes that distinguishable from
        // "no record arrived at all".
        tracing::debug!(
            target: LOG_TARGET,
            fullname = %info.fullname,
            ty_domain = %info.ty_domain,
            "mDNS record dropped: service type is not a recognised Matter type",
        );
        return None;
    };
    // Skip malformed records whose fullname has no instance label (see the
    // helper). Surfacing as `None` is the adapter's "skip this record" path.
    let Some(instance_name) = instance_name_from_fullname(&info.fullname) else {
        tracing::warn!(
            target: LOG_TARGET,
            fullname = %info.fullname,
            ty_domain = %info.ty_domain,
            "mDNS record dropped: malformed fullname (no leading instance label)",
        );
        return None;
    };
    let txt = txt_properties_to_records(&info.txt_properties);
    Some(MatterService {
        instance_name,
        kind,
        addresses,
        port: info.port,
        txt_records: txt,
    })
}

/// Split the leading DNS-SD instance label off a service fullname.
///
/// A well-formed fullname is `<instance>.<service-type>.local.`, so the
/// instance label is everything before the first dot. Returns `None` when
/// the fullname has no dot (cannot be split) or the leading label is empty
/// — both malformed — so the caller skips the record rather than
/// fabricating a service with an empty `instance_name`.
fn instance_name_from_fullname(fullname: &str) -> Option<String> {
    match fullname.split_once('.') {
        Some((instance, _rest)) if !instance.is_empty() => Some(instance.to_string()),
        _ => None,
    }
}

/// Translate mdns-sd `TxtProperties` into our raw-byte TXT map.
///
/// DNS-SD TXT values are arbitrary octet strings (RFC 6763 §6.5); we keep
/// them as `Vec<u8>` rather than lossily UTF-8-decoding, so a binary-valued
/// or vendor-specific Matter key is preserved byte-for-byte. A value-less
/// boolean key maps to an empty value.
fn txt_properties_to_records(props: &mdns_sd::TxtProperties) -> HashMap<String, Vec<u8>> {
    let mut txt = HashMap::new();
    for prop in props.iter() {
        txt.insert(
            prop.key().to_string(),
            prop.val().map(<[u8]>::to_vec).unwrap_or_default(),
        );
    }
    txt
}

/// Whether a browse string is a DNS-SD subtype (`<sub>._sub.<base>`).
///
/// Used for diagnostics only — the records a subtype browse yields are ordinary
/// records of the base type.
fn is_subtype(service_type: &str) -> bool {
    service_type.contains("._sub.")
}

/// Map a browse/record service type onto a [`ServiceKind`], **stripping a
/// DNS-SD subtype prefix first**.
///
/// mdns-sd reports the string the querier was opened with as a resolved
/// record's `ty_domain`, so a record found through the Matter compressed-fabric
/// subtype arrives as `_I<id>._sub._matter._tcp.local.` rather than
/// `_matter._tcp.local.`. RFC 6763 §7.1 defines that form as a *subtype of* the
/// base service, and Matter uses it for exactly that (§4.3.1), so such a record
/// is an operational record and must map to [`ServiceKind::Operational`] — not
/// be dropped as unrecognised, which is what would silently defeat the subtype
/// browse (issue #113).
fn kind_from_service_type(service_type: &str) -> Option<ServiceKind> {
    // `rsplit_once` so the BASE type is what remains, whatever the subtype
    // label contains.
    let base = match service_type.rsplit_once("._sub.") {
        Some((_subtype_label, base)) => base,
        None => service_type,
    };
    match base {
        "_matterc._udp.local." => Some(ServiceKind::Commissionable),
        "_matterd._udp.local." => Some(ServiceKind::Commissioner),
        "_matter._tcp.local." => Some(ServiceKind::Operational),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::net::Ipv6Addr;
    use std::time::{Duration, Instant};

    fn sample_service(instance: &str, kind: ServiceKind) -> MatterService {
        let mut txt = HashMap::new();
        txt.insert("D".to_string(), b"3840".to_vec());
        MatterService {
            instance_name: instance.to_string(),
            kind,
            addresses: vec![std::net::IpAddr::V6(Ipv6Addr::LOCALHOST)],
            port: 5540,
            txt_records: txt,
        }
    }

    #[test]
    fn dotless_fullname_is_skipped() {
        // A fullname with no dot cannot be split into instance + service —
        // it must be skipped (None), not yield an empty instance_name.
        assert_eq!(instance_name_from_fullname("nodothere"), None);
        // An empty leading label is also malformed.
        assert_eq!(instance_name_from_fullname("._matterc._udp.local."), None);
        // A well-formed fullname yields just the instance label.
        assert_eq!(
            instance_name_from_fullname("INSTANCE._matterc._udp.local."),
            Some("INSTANCE".to_string()),
        );
    }

    #[test]
    fn binary_txt_value_preserved_and_ascii_parses() {
        use mdns_sd::IntoTxtProperties;
        // A binary TXT value must survive the parse verbatim; an ASCII
        // discriminator key must still parse.
        let binary = vec![0x00u8, 0xFF, 0x80, 0x01];
        assert!(std::str::from_utf8(&binary).is_err());
        let props: mdns_sd::TxtProperties = vec![
            mdns_sd::TxtProperty::from(("BIN".to_string(), binary.clone())),
            mdns_sd::TxtProperty::from(("D".to_string(), b"3840".to_vec())),
        ]
        .into_txt_properties();

        let records = txt_properties_to_records(&props);
        assert_eq!(records.get("BIN"), Some(&binary), "binary value verbatim");

        // Wrap in a service to exercise the ASCII accessor.
        let svc = MatterService::new(
            "INSTANCE".to_string(),
            ServiceKind::Commissionable,
            vec![std::net::IpAddr::V6(Ipv6Addr::LOCALHOST)],
            5540,
            records,
        );
        assert_eq!(svc.txt_str("BIN"), None, "non-UTF-8 value is not a str");
        assert_eq!(
            svc.txt_str("D").and_then(|d| d.parse::<u16>().ok()),
            Some(3840),
        );
    }

    #[test]
    fn new_spawns_daemon_without_error() {
        let _ = MdnsSdDiscovery::new().unwrap();
    }

    #[test]
    fn publish_unpublish_roundtrip() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let svc = sample_service("matter-rust-test-publish", ServiceKind::Commissionable);
        d.publish(&svc).unwrap();
        d.unpublish("matter-rust-test-publish", ServiceKind::Commissionable)
            .unwrap();
    }

    #[test]
    fn with_daemon_uses_provided_daemon() {
        let daemon = mdns_sd::ServiceDaemon::new().unwrap();
        let mut d = MdnsSdDiscovery::with_daemon(daemon);
        let svc = sample_service("matter-rust-test-with-daemon", ServiceKind::Commissionable);
        d.publish(&svc).unwrap();
        d.unpublish("matter-rust-test-with-daemon", ServiceKind::Commissionable)
            .unwrap();
    }

    #[test]
    fn query_returns_fresh_handles() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h1 = d.query(ServiceKind::Commissionable).unwrap();
        let h2 = d.query(ServiceKind::Operational).unwrap();
        assert_ne!(h1, h2);
        d.stop_query(h1);
        d.stop_query(h2);
    }

    #[test]
    fn stop_query_idempotent() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query(ServiceKind::Commissionable).unwrap();
        d.stop_query(h);
        d.stop_query(h); // second call is a no-op, must not panic
                         // poll_results on a stopped handle returns empty.
        let results = d.poll_results(h);
        assert_eq!(results, Vec::<crate::discovery::MatterService>::new());
    }

    // ---------------------------------------------------------------------
    // Shared-browse fan-out and refcounting (issue #113).
    //
    // mdns-sd keeps ONE querier per service type; these tests pin that this
    // adapter models that rather than pretending each handle owns a browse.
    // Events are delivered through `deliver_for_test` (see its docs) because
    // the browse `Sender` is inside the daemon and CI hosts don't deliver
    // loopback mDNS; everything downstream of delivery is production code.
    // ---------------------------------------------------------------------

    /// A `ServiceResolved` event for `instance`, shaped exactly like the one
    /// mdns-sd emits for a Matter record of `kind`.
    fn resolved_event(instance: &str, kind: ServiceKind) -> ServiceEvent {
        let info = ServiceInfo::new(
            kind.service_type(),
            instance,
            &format!("{instance}.local."),
            &[std::net::IpAddr::V6(Ipv6Addr::LOCALHOST)][..],
            5540,
            Vec::<TxtProperty>::new(),
        )
        .unwrap();
        ServiceEvent::ServiceResolved(Box::new(info.as_resolved_service()))
    }

    /// Whether a poll result carries `instance`. Used instead of comparing
    /// whole vectors: a developer machine may have real Matter records on the
    /// network, and they land in these browses too.
    fn contains(results: &[MatterService], instance: &str) -> bool {
        results.iter().any(|s| s.instance_name == instance)
    }

    /// A compressed fabric id and its subtype string, used by the subtype tests.
    const TEST_CFID: [u8; 8] = [0xF5, 0x2A, 0xC1, 0x07, 0xC9, 0x54, 0xE3, 0x8E];
    const TEST_SUBTYPE: &str = "_IF52AC107C954E38E._sub._matter._tcp.local.";

    /// A `ServiceResolved` event exactly as the daemon emits it for a **subtype**
    /// browse: `ty_domain` is the string the querier was opened with (mdns-sd
    /// keys its PTR cache and queriers by that string and passes it straight
    /// into `resolve_service_from_cache`), i.e. the subtype, NOT the base type.
    /// `ResolvedService` is `#[non_exhaustive]`, so it is built through
    /// `ServiceInfo` and then re-pointed at the subtype.
    fn resolved_event_under_subtype(instance: &str, subtype: &str) -> ServiceEvent {
        let info = ServiceInfo::new(
            ServiceKind::Operational.service_type(),
            instance,
            &format!("{instance}.local."),
            &[std::net::IpAddr::V6(Ipv6Addr::LOCALHOST)][..],
            5540,
            Vec::<TxtProperty>::new(),
        )
        .unwrap();
        let mut resolved = info.as_resolved_service();
        resolved.ty_domain = subtype.to_string();
        resolved.sub_ty_domain = Some(subtype.to_string());
        ServiceEvent::ServiceResolved(Box::new(resolved))
    }

    #[test]
    fn subtype_service_type_maps_to_operational_kind() {
        // A record arriving under the fabric subtype IS an operational record;
        // dropping it as an unrecognised type is what would defeat the whole
        // subtype browse (issue #113).
        assert_eq!(
            kind_from_service_type(TEST_SUBTYPE),
            Some(ServiceKind::Operational),
        );
        assert_eq!(
            kind_from_service_type("_S15._sub._matterc._udp.local."),
            Some(ServiceKind::Commissionable),
        );
        // The base types are unaffected...
        assert_eq!(
            kind_from_service_type("_matter._tcp.local."),
            Some(ServiceKind::Operational),
        );
        // ... and a genuinely foreign type is still rejected, subtype or not.
        assert_eq!(kind_from_service_type("_http._tcp.local."), None);
        assert_eq!(kind_from_service_type("_x._sub._http._tcp.local."), None);
        assert!(is_subtype(TEST_SUBTYPE));
        assert!(!is_subtype("_matter._tcp.local."));
    }

    #[test]
    fn subtype_browse_is_a_distinct_key_from_the_base_type() {
        // THE #113 FIX: the fabric subtype and the base type are two separate
        // browses (mdns-sd keys its querier by the browsed string), so a
        // controller can run both at once — narrow-and-fast plus never-worse
        // fallback — and neither steals the other's records or tears the other
        // down.
        let base = ServiceKind::Operational.service_type();
        let mut d = MdnsSdDiscovery::new().unwrap();
        let sub_handle = d.query_operational_fabric(TEST_CFID).unwrap();
        let base_handle = d.query(ServiceKind::Operational).unwrap();
        assert_ne!(sub_handle, base_handle);
        assert_eq!(
            d.browse_calls.get(TEST_SUBTYPE),
            Some(&1),
            "the subtype must be browsed in its own right",
        );
        assert_eq!(
            d.browse_calls.get(base),
            Some(&1),
            "and the base type separately",
        );

        // A record delivered on the SUBTYPE browse reaches only that browse's
        // handle, and arrives as a normal operational record.
        d.deliver_for_type_for_test(
            TEST_SUBTYPE,
            resolved_event_under_subtype("113-subtype-only", TEST_SUBTYPE),
        );
        let from_sub = d.poll_results(sub_handle);
        assert!(contains(&from_sub, "113-subtype-only"));
        assert_eq!(
            from_sub
                .iter()
                .find(|s| s.instance_name == "113-subtype-only")
                .map(|s| s.kind),
            Some(ServiceKind::Operational),
            "a subtype record must surface as an operational record",
        );
        assert!(!contains(&d.poll_results(base_handle), "113-subtype-only"));

        // ... and vice versa: the base browse still works on its own.
        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-base-only", ServiceKind::Operational),
        );
        assert!(contains(&d.poll_results(base_handle), "113-base-only"));
        assert!(!contains(&d.poll_results(sub_handle), "113-base-only"));

        // Releasing the subtype browse must not stop the base-type browse.
        d.stop_query(sub_handle);
        assert_eq!(d.stop_browse_calls.get(TEST_SUBTYPE), Some(&1));
        assert_eq!(d.stop_browse_calls.get(base), None);
        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-base-survives", ServiceKind::Operational),
        );
        assert!(contains(&d.poll_results(base_handle), "113-base-survives"));
        d.stop_query(base_handle);
        assert_eq!(d.stop_browse_calls.get(base), Some(&1));
    }

    #[test]
    fn subtype_browse_refcounts_and_replays_like_any_other() {
        // Everything the base-type browse gained in `ca6e093d`/`a1670eeb` —
        // one browse per string, fan-out to every handle, replay for a
        // late-attaching handle — must hold for a subtype browse too, since the
        // controller and the commissioning driver can both be resolving on the
        // same fabric at once.
        let mut d = MdnsSdDiscovery::new().unwrap();
        let a = d.query_operational_fabric(TEST_CFID).unwrap();
        let b = d.query_operational_fabric(TEST_CFID).unwrap();
        assert_eq!(
            d.browse_calls.get(TEST_SUBTYPE),
            Some(&1),
            "two handles on one fabric share a single subtype browse",
        );

        d.deliver_for_type_for_test(
            TEST_SUBTYPE,
            resolved_event_under_subtype("113-fanout", TEST_SUBTYPE),
        );
        assert!(contains(&d.poll_results(a), "113-fanout"));
        assert!(contains(&d.poll_results(b), "113-fanout"), "fan-out");

        // A handle attaching after the record was surfaced is seeded from the
        // replay cache (no further event will ever arrive for a stable record).
        let late = d.query_operational_fabric(TEST_CFID).unwrap();
        assert!(contains(&d.poll_results(late), "113-fanout"), "replay");

        // Refcount: only the last release stops the browse.
        d.stop_query(a);
        d.stop_query(b);
        assert_eq!(d.stop_browse_calls.get(TEST_SUBTYPE), None);
        d.stop_query(late);
        assert_eq!(d.stop_browse_calls.get(TEST_SUBTYPE), Some(&1));

        // A different fabric is a different browse string entirely.
        let other = d.query_operational_fabric([0; 8]).unwrap();
        assert_eq!(
            d.browse_calls
                .get("_I0000000000000000._sub._matter._tcp.local."),
            Some(&1),
        );
        d.stop_query(other);
    }

    #[test]
    fn two_handles_of_one_type_share_one_browse_and_both_see_each_record() {
        // THE #113 FAILURE: the second `query` used to open a second
        // `daemon.browse` of the same type, which replaces the daemon's
        // querier and orphans the first handle's receiver — so the first
        // handle never saw another record.
        let mut d = MdnsSdDiscovery::new().unwrap();
        let a = d.query(ServiceKind::Operational).unwrap();
        let b = d.query(ServiceKind::Operational).unwrap();
        assert_ne!(a, b);
        assert_eq!(
            d.browse_calls.get(ServiceKind::Operational.service_type()),
            Some(&1),
            "the second handle must reuse the first handle's browse",
        );

        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-shared", ServiceKind::Operational),
        );

        let first = d.poll_results(a);
        let second = d.poll_results(b);
        assert!(contains(&first, "113-shared"), "handle A must see it");
        assert!(
            contains(&second, "113-shared"),
            "handle B must see it too — one handle polling must not consume \
             another handle's copy",
        );

        // Per-handle consumption still holds: a record is handed to a given
        // handle exactly once.
        assert!(!contains(&d.poll_results(a), "113-shared"));
        assert!(!contains(&d.poll_results(b), "113-shared"));

        d.stop_query(a);
        d.stop_query(b);
    }

    #[test]
    fn a_handle_attached_after_a_record_was_surfaced_still_receives_it() {
        // THE STARVATION THE REFCOUNT WOULD HAVE INTRODUCED: the second handle
        // deliberately skips `daemon.browse`, so it gets no cache replay — and
        // mdns-sd re-emits a record only when it CHANGES. A stable service that
        // resolved before this handle attached would therefore never be
        // surfaced to it again, and a resolve for it would burn its whole
        // budget and fail with "not found via mDNS" while the record sits in
        // the daemon's cache. The `surfaced` replay cache is what prevents it.
        let mut d = MdnsSdDiscovery::new().unwrap();
        let early = d.query(ServiceKind::Operational).unwrap();

        // A record arrives and is consumed by the only handle open at the time.
        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-already-resolved", ServiceKind::Operational),
        );
        assert!(contains(&d.poll_results(early), "113-already-resolved"));

        // A late handle attaches to the SAME browse. No further event will ever
        // arrive for this stable record, so everything it can learn must come
        // from the adapter's own memory.
        let late = d.query(ServiceKind::Operational).unwrap();
        assert_eq!(
            d.browse_calls.get(ServiceKind::Operational.service_type()),
            Some(&1),
            "the late handle must still reuse the shared browse",
        );
        let replayed = d.poll_results(late);
        assert!(
            contains(&replayed, "113-already-resolved"),
            "a handle attaching after the record was surfaced must be seeded \
             with it — without the replay it sees nothing for the life of the \
             browse",
        );

        // Exactly once, as for any other handle: the replay is a delivery, not
        // a permanent duplicate source.
        assert!(!contains(&d.poll_results(late), "113-already-resolved"));

        // Replay collapses per instance: a re-announce updates the cached entry
        // rather than appending, so the next handle to attach is seeded with one
        // copy however often the record was seen.
        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-already-resolved", ServiceKind::Operational),
        );
        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-already-resolved", ServiceKind::Operational),
        );
        let _ = d.poll_results(early);
        let later = d.query(ServiceKind::Operational).unwrap();
        let seeded = d.poll_results(later);
        assert_eq!(
            seeded
                .iter()
                .filter(|s| s.instance_name == "113-already-resolved")
                .count(),
            1,
            "the replay holds one entry per instance, not one per announcement",
        );

        d.stop_query(early);
        d.stop_query(late);
        d.stop_query(later);
    }

    #[test]
    fn stopping_one_handle_leaves_the_other_handles_browse_alive() {
        // THE OTHER HALF OF #113: `stop_browse` is per service type, so the
        // short-lived resolve's `stop_query` used to tear down the browse the
        // long-lived handle was still waiting on (and wipe mdns-sd's cache
        // for the type), permanently for the process.
        let ty = ServiceKind::Operational.service_type();
        let mut d = MdnsSdDiscovery::new().unwrap();
        let long_lived = d.query(ServiceKind::Operational).unwrap();
        let short_lived = d.query(ServiceKind::Operational).unwrap();

        d.stop_query(short_lived);
        assert_eq!(
            d.stop_browse_calls.get(ty),
            None,
            "a browse with a handle still attached must not be stopped",
        );

        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-survivor", ServiceKind::Operational),
        );
        assert!(
            contains(&d.poll_results(long_lived), "113-survivor"),
            "the surviving handle must still receive records",
        );
        // The released handle is inert, not merely quiet.
        assert_eq!(
            d.poll_results(short_lived),
            Vec::<crate::discovery::MatterService>::new()
        );

        d.stop_query(long_lived);
        assert_eq!(
            d.stop_browse_calls.get(ty),
            Some(&1),
            "only the LAST release stops the browse",
        );
    }

    #[test]
    fn browse_reopens_after_the_last_handle_is_released() {
        // Refcount must drop to zero and be usable again — a stopped type is
        // not permanently poisoned.
        let ty = ServiceKind::Operational.service_type();
        let mut d = MdnsSdDiscovery::new().unwrap();
        let first = d.query(ServiceKind::Operational).unwrap();
        d.stop_query(first);
        assert_eq!(d.stop_browse_calls.get(ty), Some(&1));

        // A delivery with no browse open goes nowhere (nothing to fan out to).
        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-orphan", ServiceKind::Operational),
        );

        let second = d.query(ServiceKind::Operational).unwrap();
        assert_eq!(
            d.browse_calls.get(ty),
            Some(&2),
            "a type with no handles must be browsed afresh",
        );
        assert!(!contains(&d.poll_results(second), "113-orphan"));
        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-reopened", ServiceKind::Operational),
        );
        assert!(contains(&d.poll_results(second), "113-reopened"));
        d.stop_query(second);
    }

    #[test]
    fn handles_of_different_types_are_independent() {
        // Fan-out is per service type: an operational record must not leak
        // into a commissionable browse, and stopping one type must not touch
        // the other's browse.
        let mut d = MdnsSdDiscovery::new().unwrap();
        let operational = d.query(ServiceKind::Operational).unwrap();
        let commissionable = d.query(ServiceKind::Commissionable).unwrap();

        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-op-only", ServiceKind::Operational),
        );
        assert!(!contains(&d.poll_results(commissionable), "113-op-only"));
        assert!(contains(&d.poll_results(operational), "113-op-only"));

        d.stop_query(commissionable);
        assert_eq!(
            d.stop_browse_calls
                .get(ServiceKind::Operational.service_type()),
            None,
            "stopping one type must not stop another",
        );
        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-still-live", ServiceKind::Operational),
        );
        assert!(contains(&d.poll_results(operational), "113-still-live"));
        d.stop_query(operational);
    }

    #[test]
    fn actor_usage_pattern_survives_a_nested_resolve() {
        // The exact shape of the reported failure, at the adapter level: the
        // controller actor holds ONE long-lived operational browse for its
        // parked resolves, while another operational resolve (the connect
        // fallback, or a commissioning driver sharing the same `Discovery`)
        // runs its own query/poll/stop cycle underneath. Before the fix the
        // nested resolve orphaned the actor's receiver and then stopped the
        // shared browse, and every later parked resolve failed with
        // "not found via mDNS" for the rest of the process.
        let mut d = MdnsSdDiscovery::new().unwrap();
        let actor_browse = d.query(ServiceKind::Operational).unwrap();

        // ... a nested resolve runs to completion, exactly as
        // `resolve_operational` does: query, poll, stop_query.
        let nested = d.query(ServiceKind::Operational).unwrap();
        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-nested-target", ServiceKind::Operational),
        );
        assert!(contains(&d.poll_results(nested), "113-nested-target"));
        d.stop_query(nested);

        // ... and afterwards the actor's browse is still live and still
        // delivering, which is what #113 lost.
        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("113-after-nested", ServiceKind::Operational),
        );
        let late = d.poll_results(actor_browse);
        assert!(
            contains(&late, "113-after-nested"),
            "the long-lived browse must survive a nested resolve",
        );
        // It also still holds the record the nested resolve consumed for
        // itself — the fan-out gave both handles a copy.
        assert!(contains(&late, "113-nested-target"));
        d.stop_query(actor_browse);
    }

    #[test]
    fn service_kind_to_dns_sd_string_exact() {
        // Defensive: the adapter relies on these strings; if
        // `ServiceKind::service_type` changes, this test catches it.
        assert_eq!(
            ServiceKind::Commissionable.service_type(),
            "_matterc._udp.local."
        );
        assert_eq!(
            ServiceKind::Commissioner.service_type(),
            "_matterd._udp.local."
        );
        assert_eq!(
            ServiceKind::Operational.service_type(),
            "_matter._tcp.local."
        );
    }

    #[test]
    #[ignore = "requires host mDNS multicast support; CI containers (GitHub \
                Actions ubuntu-latest, macos-latest) don't deliver loopback \
                mDNS announces between two ServiceDaemon instances even with \
                enable_interface(LoopbackV{4,6}). Run locally with `cargo test \
                --features tokio,mdns-sd -- --ignored self_publish_self_discover`. \
                Tracked in TODO-1.0.md as 'mDNS loopback interop in CI'."]
    fn self_publish_self_discover() {
        // Daemon A publishes; adapter B (with its own daemon) queries
        // and observes the service within 5 seconds via poll_results.
        //
        // The sample_service uses `::1` (IPv6 loopback) — mdns-sd 0.13
        // disables loopback interfaces by default and filters
        // addresses to those that match an enabled interface's subnet.
        // We therefore explicitly enable the loopback interfaces on
        // both daemons (publisher and querier) before use. This keeps
        // the test hermetic — no dependency on a real network
        // interface — at the cost of needing the lower-level
        // `with_daemon` constructor here.
        //
        // (Plan default was 2s; bumped to 5s — mdns-sd's register
        // path performs a probing/announce delay before answers
        // appear on a querier, so 2s is racy locally and on CI.)
        let pub_daemon = mdns_sd::ServiceDaemon::new().unwrap();
        pub_daemon
            .enable_interface(mdns_sd::IfKind::LoopbackV4)
            .unwrap();
        pub_daemon
            .enable_interface(mdns_sd::IfKind::LoopbackV6)
            .unwrap();
        let q_daemon = mdns_sd::ServiceDaemon::new().unwrap();
        q_daemon
            .enable_interface(mdns_sd::IfKind::LoopbackV4)
            .unwrap();
        q_daemon
            .enable_interface(mdns_sd::IfKind::LoopbackV6)
            .unwrap();
        let mut publisher = MdnsSdDiscovery::with_daemon(pub_daemon);
        let mut querier = MdnsSdDiscovery::with_daemon(q_daemon);

        let svc = sample_service("matter-rust-test-discover", ServiceKind::Commissionable);
        publisher.publish(&svc).unwrap();
        let handle = querier.query(ServiceKind::Commissionable).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = false;
        while Instant::now() < deadline {
            let results = querier.poll_results(handle);
            if results
                .iter()
                .any(|s| s.instance_name == "matter-rust-test-discover")
            {
                found = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        querier.stop_query(handle);
        publisher
            .unpublish("matter-rust-test-discover", ServiceKind::Commissionable)
            .unwrap();

        assert!(found, "did not discover self-published service within 5s");
    }

    // ---------------------------------------------------------------------
    // Found events (`poll_found`): a NEW PTR record, mdns-sd `ServiceFound`.
    // ---------------------------------------------------------------------

    /// A `ServiceFound` event exactly as mdns-sd emits it: `(ty_domain, fullname)`.
    fn found_event(service_type: &str, fullname: &str) -> ServiceEvent {
        ServiceEvent::ServiceFound(service_type.to_string(), fullname.to_string())
    }

    /// Whether a `poll_found` result names `instance`. Used instead of
    /// comparing whole vectors: a developer machine may have real Matter nodes
    /// announcing on the network, and their found events land here too.
    fn names(found: &[String], instance: &str) -> bool {
        found.iter().any(|n| n == instance)
    }

    /// A found event is fanned out to every handle of the browse, and each
    /// handle gets it exactly once.
    #[test]
    fn a_found_event_reaches_every_attached_handle_once() {
        let ty = ServiceKind::Operational.service_type();
        let mut d = MdnsSdDiscovery::new().unwrap();
        let a = d.query(ServiceKind::Operational).unwrap();
        let b = d.query(ServiceKind::Operational).unwrap();
        d.deliver_for_test(
            ServiceKind::Operational,
            found_event(ty, "t1-found._matter._tcp.local."),
        );
        assert!(names(&d.poll_found(a), "t1-found"));
        assert!(
            names(&d.poll_found(b), "t1-found"),
            "one handle's poll must not consume another handle's copy",
        );
        assert!(!names(&d.poll_found(a), "t1-found"), "returned once");
        assert!(!names(&d.poll_found(b), "t1-found"), "returned once");
        d.stop_query(a);
        // A stopped handle is inert: its buffer went with it.
        assert_eq!(d.poll_found(a), Vec::<String>::new());
        d.stop_query(b);
    }

    /// Found events are events, not state — a handle attached after one
    /// does not get it — while the same late handle still gets the `surfaced`
    /// replay of resolved records.
    #[test]
    fn found_events_are_not_replayed_to_a_handle_attached_later() {
        let ty = ServiceKind::Operational.service_type();
        let mut d = MdnsSdDiscovery::new().unwrap();
        let early = d.query(ServiceKind::Operational).unwrap();
        d.deliver_for_test(
            ServiceKind::Operational,
            resolved_event("t2-node", ServiceKind::Operational),
        );
        d.deliver_for_test(
            ServiceKind::Operational,
            found_event(ty, "t2-node._matter._tcp.local."),
        );
        let late = d.query(ServiceKind::Operational).unwrap();
        assert!(
            !names(&d.poll_found(late), "t2-node"),
            "a found event must never be replayed to a later handle",
        );
        assert!(
            contains(&d.poll_results(late), "t2-node"),
            "the resolved record IS replayed from `surfaced`",
        );
        assert!(names(&d.poll_found(early), "t2-node"));
        d.stop_query(early);
        d.stop_query(late);
    }

    /// A `ServiceRemoved` event exactly as mdns-sd emits it on PTR/SRV expiry
    /// or a goodbye: `(ty_domain, fullname)`.
    fn removed_event(service_type: &str, fullname: &str) -> ServiceEvent {
        ServiceEvent::ServiceRemoved(service_type.to_string(), fullname.to_string())
    }

    /// `surfaced` mirrors the daemon's live cache: a record the daemon has
    /// removed (expiry or goodbye) is no longer replayed to a late-attached
    /// handle, and a later resolve puts it back. What a handle was already
    /// handed stays handed — a removal never reaches into `pending`.
    #[test]
    fn a_removed_instance_is_not_replayed_until_it_resolves_again() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let watch = d.query_operational_fabric(TEST_CFID).unwrap();
        d.deliver_for_type_for_test(
            TEST_SUBTYPE,
            resolved_event_under_subtype("rm-node", TEST_SUBTYPE),
        );
        assert!(contains(&d.poll_results(watch), "rm-node"));
        // Attached while the record was live, not yet polled.
        let unpolled = d.query_operational_fabric(TEST_CFID).unwrap();

        d.deliver_for_type_for_test(
            TEST_SUBTYPE,
            removed_event(TEST_SUBTYPE, "rm-node._matter._tcp.local."),
        );
        let late = d.query_operational_fabric(TEST_CFID).unwrap();
        assert!(
            !contains(&d.poll_results(late), "rm-node"),
            "a removed instance must not be replayed to a handle attached after \
             the removal",
        );
        assert!(
            contains(&d.poll_results(unpolled), "rm-node"),
            "a removal must not reach into a handle's already-delivered buffer",
        );

        d.deliver_for_type_for_test(
            TEST_SUBTYPE,
            resolved_event_under_subtype("rm-node", TEST_SUBTYPE),
        );
        let later = d.query_operational_fabric(TEST_CFID).unwrap();
        assert!(
            contains(&d.poll_results(later), "rm-node"),
            "resolved again: replayed again",
        );
        for h in [watch, unpolled, late, later] {
            d.stop_query(h);
        }
    }

    /// On a subtype browse mdns-sd reports the PTR's alias, which is the
    /// BASE-type instance fullname (mdns-sd 0.21.6 `service_daemon.rs`
    /// L3316-3334); the instance label is everything before the first dot. A
    /// fullname with no instance label is ignored, not surfaced as "".
    #[test]
    fn a_subtype_found_event_yields_the_base_type_instance_label() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        d.deliver_for_type_for_test(
            TEST_SUBTYPE,
            found_event(
                TEST_SUBTYPE,
                "F52AC107C954E38E-0000000000000042._matter._tcp.local.",
            ),
        );
        d.deliver_for_type_for_test(TEST_SUBTYPE, found_event(TEST_SUBTYPE, "nolabel"));
        let found = d.poll_found(h);
        assert!(names(&found, "F52AC107C954E38E-0000000000000042"));
        assert!(!names(&found, "nolabel"), "a dotless fullname is malformed");
        assert!(found.iter().all(|n| !n.is_empty()));
        d.stop_query(h);
    }

    /// `poll_results` and `poll_found` share one drain, so whichever runs
    /// first fans every pending event out to both kinds of buffer and neither
    /// call order loses anything. Events go through `queue_for_test`, which
    /// leaves them for the next drain instead of fanning them out at once.
    #[test]
    fn poll_results_and_poll_found_share_one_drain_in_either_order() {
        let ty = ServiceKind::Operational.service_type();
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query(ServiceKind::Operational).unwrap();

        d.queue_for_test(ty, resolved_event("t4-first", ServiceKind::Operational));
        d.queue_for_test(ty, found_event(ty, "t4-first._matter._tcp.local."));
        assert!(contains(&d.poll_results(h), "t4-first"));
        assert!(
            names(&d.poll_found(h), "t4-first"),
            "the drain `poll_results` did must have buffered the found event",
        );

        d.queue_for_test(ty, resolved_event("t4-second", ServiceKind::Operational));
        d.queue_for_test(ty, found_event(ty, "t4-second._matter._tcp.local."));
        assert!(
            names(&d.poll_found(h), "t4-second"),
            "`poll_found` must drain on its own",
        );
        assert!(
            contains(&d.poll_results(h), "t4-second"),
            "the drain `poll_found` did must have buffered the resolved record",
        );
        d.stop_query(h);
    }

    /// A handle nobody calls `poll_found` on (the controller's base handle)
    /// costs bounded memory: at the cap the OLDEST name is dropped. Driven
    /// through `fan_out_event` directly so no live daemon event can perturb the
    /// count.
    #[test]
    fn a_found_buffer_drops_its_oldest_name_at_the_cap() {
        let ty = ServiceKind::Operational.service_type();
        let h = QueryHandle(1);
        let mut pending: HashMap<QueryHandle, Vec<MatterService>> = HashMap::new();
        let mut surfaced: HashMap<String, MatterService> = HashMap::new();
        let mut found: HashMap<QueryHandle, Vec<String>> = HashMap::new();
        let mut present: HashSet<String> = HashSet::new();
        pending.insert(h, Vec::new());
        found.insert(h, Vec::new());
        for i in 0..=FOUND_BUFFER_CAP {
            fan_out_event(
                ty,
                &mut pending,
                &mut surfaced,
                &mut found,
                &mut present,
                found_event(ty, &format!("cap-{i:03}._matter._tcp.local.")),
            );
        }
        let buffer = &found[&h];
        assert_eq!(buffer.len(), FOUND_BUFFER_CAP);
        assert_eq!(
            buffer.first().map(String::as_str),
            Some("cap-001"),
            "cap-000 was dropped"
        );
        assert_eq!(
            buffer.last().map(String::as_str),
            Some(format!("cap-{FOUND_BUFFER_CAP:03}").as_str()),
        );
        assert!(pending[&h].is_empty(), "a found event is not a record");
    }

    // ---------------------------------------------------------------------
    // Replay suppression (`present`).
    // ---------------------------------------------------------------------

    /// A found instance is reported once; a second `ServiceFound` with no
    /// `ServiceRemoved` between is a re-delivery of a PTR the daemon already
    /// held, not a new one; after a removal, a found event is a genuine return
    /// and is reported again.
    #[test]
    fn a_found_instance_is_reported_again_only_after_it_was_removed() {
        let fullname = "present-a._matter._tcp.local.";
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        d.deliver_for_type_for_test(TEST_SUBTYPE, found_event(TEST_SUBTYPE, fullname));
        assert!(names(&d.poll_found(h), "present-a"), "first appearance");
        d.deliver_for_type_for_test(TEST_SUBTYPE, found_event(TEST_SUBTYPE, fullname));
        assert!(
            !names(&d.poll_found(h), "present-a"),
            "a second Found with no Removed between is a replay"
        );
        d.deliver_for_type_for_test(TEST_SUBTYPE, removed_event(TEST_SUBTYPE, fullname));
        d.deliver_for_type_for_test(TEST_SUBTYPE, found_event(TEST_SUBTYPE, fullname));
        assert!(
            names(&d.poll_found(h), "present-a"),
            "removed, then found again: a genuine return"
        );
        d.stop_query(h);
    }

    /// A replayed PTR (a requery replays every cached one) reaches no handle as
    /// a found event, while its resolved record is still re-delivered by
    /// `poll_results` (latest-wins consumers are unaffected).
    #[test]
    fn a_replayed_found_event_is_reported_to_no_handle() {
        let fullname = "replay-a._matter._tcp.local.";
        let mut d = MdnsSdDiscovery::new().unwrap();
        let first = d.query_operational_fabric(TEST_CFID).unwrap();
        let second = d.query_operational_fabric(TEST_CFID).unwrap();
        d.deliver_for_type_for_test(TEST_SUBTYPE, found_event(TEST_SUBTYPE, fullname));
        d.deliver_for_type_for_test(
            TEST_SUBTYPE,
            resolved_event_under_subtype("replay-a", TEST_SUBTYPE),
        );
        for h in [first, second] {
            assert!(names(&d.poll_found(h), "replay-a"));
            assert!(contains(&d.poll_results(h), "replay-a"));
        }
        // The replay: the same PTR and record again, no removal between.
        d.deliver_for_type_for_test(TEST_SUBTYPE, found_event(TEST_SUBTYPE, fullname));
        d.deliver_for_type_for_test(
            TEST_SUBTYPE,
            resolved_event_under_subtype("replay-a", TEST_SUBTYPE),
        );
        for h in [first, second] {
            assert!(
                !names(&d.poll_found(h), "replay-a"),
                "a replayed PTR is not a new one"
            );
            assert!(
                contains(&d.poll_results(h), "replay-a"),
                "the resolved record itself is re-delivered"
            );
        }
        d.stop_query(first);
        d.stop_query(second);
    }

    /// `present` is keyed verbatim, matching mdns-sd's case-sensitive PTR alias
    /// comparison (`dns_parser.rs` 876-878): two spellings are two cache
    /// entries there and two entries here.
    #[test]
    fn present_instance_keys_are_case_sensitive_like_mdns_sd() {
        let upper = "CASE-TEST._matter._tcp.local.";
        let lower = "case-test._matter._tcp.local.";
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        d.deliver_for_type_for_test(TEST_SUBTYPE, found_event(TEST_SUBTYPE, upper));
        assert!(names(&d.poll_found(h), "CASE-TEST"));
        d.deliver_for_type_for_test(TEST_SUBTYPE, removed_event(TEST_SUBTYPE, lower));
        d.deliver_for_type_for_test(TEST_SUBTYPE, found_event(TEST_SUBTYPE, upper));
        assert!(
            !names(&d.poll_found(h), "CASE-TEST"),
            "removing the lower-case spelling must not remove the upper-case one"
        );
        d.deliver_for_type_for_test(TEST_SUBTYPE, found_event(TEST_SUBTYPE, lower));
        assert!(
            names(&d.poll_found(h), "case-test"),
            "the lower-case spelling is its own entry"
        );
        d.stop_query(h);
    }

    // ---------------------------------------------------------------------
    // Requery: the lossless receiver swap.
    //
    // The switch-over is driven on a `BrowseState` fed by scripted channels
    // (`EventSource::Scripted`), so a test decides exactly what the "daemon"
    // has sent to which receiver before each drain. `requery` itself, which
    // calls the real daemon, is tested on `MdnsSdDiscovery` below.
    // ---------------------------------------------------------------------

    /// The sending end of a scripted receiver: `Some(event)` is an event the
    /// daemon sent, `None` a gap (the read that takes it sees an empty
    /// channel).
    type Script = std::sync::mpsc::Sender<Option<ServiceEvent>>;

    /// A subtype browse fed by a scripted receiver instead of the daemon, with
    /// one attached handle. Returns the browse, its script, and the handle.
    fn scripted_browse() -> (BrowseState, Script, QueryHandle) {
        let (script, receiver) = std::sync::mpsc::channel();
        let mut browse = BrowseState::new(EventSource::Scripted(receiver));
        let h = QueryHandle(1);
        browse.pending.insert(h, Vec::new());
        browse.found.insert(h, Vec::new());
        (browse, script, h)
    }

    /// Start a requery's switch-over on `browse` with a fresh scripted
    /// receiver, exactly as `requery` does with the daemon's; returns its
    /// script.
    fn begin_scripted_requery(browse: &mut BrowseState) -> Script {
        let (script, receiver) = std::sync::mpsc::channel();
        browse.begin_requery(EventSource::Scripted(receiver));
        script
    }

    /// `h`'s found events since the last call, as `poll_found` takes them.
    fn take_found(browse: &mut BrowseState, h: QueryHandle) -> Vec<String> {
        browse
            .found
            .get_mut(&h)
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// The first event mdns-sd sends on a new browse listener.
    fn search_started() -> ServiceEvent {
        ServiceEvent::SearchStarted(TEST_SUBTYPE.to_string())
    }

    /// What the old receiver holds is applied before anything after the new
    /// receiver's `SearchStarted`. The device left (its PTR expired:
    /// `ServiceRemoved` to the old listener) and was back by the time the
    /// daemon ran the browse (a genuine `ServiceFound` on the new one): the
    /// return must be reported, not suppressed as a replay.
    #[test]
    fn requery_applies_the_retiring_receiver_before_the_new_one() {
        let fullname = "requery-a._matter._tcp.local.";
        let (mut browse, to_old, h) = scripted_browse();
        to_old
            .send(Some(found_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert_eq!(take_found(&mut browse, h), vec!["requery-a".to_string()]);

        let to_new = begin_scripted_requery(&mut browse);
        to_old
            .send(Some(removed_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        to_new.send(Some(search_started())).unwrap();
        to_new
            .send(Some(found_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert_eq!(
            take_found(&mut browse, h),
            vec!["requery-a".to_string()],
            "the removal on the old receiver is applied first, so the return is reported"
        );
        assert!(browse.retiring.is_none(), "switched over at SearchStarted");
        assert!(
            to_old.send(None).is_err(),
            "and the old receiver is dropped"
        );
    }

    /// The daemon sends a removal to the old listener AFTER step 1 of a drain
    /// read it empty, then runs the browse command before step 2 reads the new
    /// receiver. Step 3 drains the old receiver once more at the switch point;
    /// without it the removal is dropped with the old receiver and the
    /// instance's genuine return is suppressed for good.
    #[test]
    fn requery_keeps_a_removal_sent_just_before_the_switch() {
        let fullname = "requery-race._matter._tcp.local.";
        let (mut browse, to_old, h) = scripted_browse();
        to_old
            .send(Some(found_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert_eq!(take_found(&mut browse, h), vec!["requery-race".to_string()]);

        let to_new = begin_scripted_requery(&mut browse);
        // Step 1 of the next drain reads the old receiver empty (the gap)...
        to_old.send(None).unwrap();
        // ... then the daemon sends the removal there and runs the browse.
        to_old
            .send(Some(removed_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        to_new.send(Some(search_started())).unwrap();
        to_new
            .send(Some(found_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert_eq!(
            take_found(&mut browse, h),
            vec!["requery-race".to_string()],
            "the old receiver is drained once more at the switch point"
        );
    }

    /// While the new receiver is empty the daemon has not run the browse
    /// command, so the old receiver is still the live listener: it is kept and
    /// drained, and a removal it carries then is applied, not lost. It is
    /// dropped once the new receiver's `SearchStarted` arrives, and the
    /// instance's later return is reported.
    #[test]
    fn requery_keeps_the_old_receiver_until_the_new_one_speaks() {
        let fullname = "requery-kept._matter._tcp.local.";
        let (mut browse, to_old, h) = scripted_browse();
        to_old
            .send(Some(found_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert_eq!(take_found(&mut browse, h), vec!["requery-kept".to_string()]);

        let to_new = begin_scripted_requery(&mut browse);
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert!(
            browse.retiring.is_some(),
            "the new receiver is empty: the old one is still the live listener"
        );

        // Sent to the old listener after that drain, before the daemon runs
        // the browse command.
        to_old
            .send(Some(removed_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert!(
            !browse.present.contains("requery-kept"),
            "the removal is applied, not lost"
        );
        assert!(browse.retiring.is_some());

        to_new.send(Some(search_started())).unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert!(browse.retiring.is_none(), "dropped at SearchStarted");
        assert!(to_old.send(None).is_err());

        to_new
            .send(Some(found_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert_eq!(
            take_found(&mut browse, h),
            vec!["requery-kept".to_string()],
            "its later genuine return is reported"
        );
    }

    /// The switch point is the new receiver's first event of ANY kind (a
    /// future mdns-sd that sent something before `SearchStarted` is traced,
    /// not waited on forever), and that event is applied, not swallowed as a
    /// marker.
    #[test]
    fn requery_switches_on_a_first_event_other_than_search_started() {
        let (mut browse, to_old, h) = scripted_browse();
        let to_new = begin_scripted_requery(&mut browse);
        to_new
            .send(Some(found_event(
                TEST_SUBTYPE,
                "requery-first._matter._tcp.local.",
            )))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert!(browse.retiring.is_none());
        assert!(to_old.send(None).is_err());
        assert_eq!(
            take_found(&mut browse, h),
            vec!["requery-first".to_string()],
            "the first event is applied"
        );
    }

    /// An accepted imprecision, pinned: mdns-sd also reports `ServiceRemoved`
    /// when only an instance's SRV/addresses expire while its PTR stays cached.
    /// The next requery replays that PTR, and it is reported as found once —
    /// and only once: the requery after that replays it again, suppressed.
    #[test]
    fn requery_after_an_srv_expiry_reports_the_instance_once() {
        let fullname = "requery-s1._matter._tcp.local.";
        let (mut browse, to_old, h) = scripted_browse();
        to_old
            .send(Some(found_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert_eq!(take_found(&mut browse, h), vec!["requery-s1".to_string()]);
        to_old
            .send(Some(removed_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert_eq!(take_found(&mut browse, h), Vec::<String>::new());

        let to_first = begin_scripted_requery(&mut browse);
        to_first.send(Some(search_started())).unwrap();
        to_first
            .send(Some(found_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert_eq!(
            take_found(&mut browse, h),
            vec!["requery-s1".to_string()],
            "the accepted imprecision: reported once"
        );

        let to_second = begin_scripted_requery(&mut browse);
        to_second.send(Some(search_started())).unwrap();
        to_second
            .send(Some(found_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert_eq!(browse.drain(TEST_SUBTYPE), DrainOutcome::Open);
        assert_eq!(
            take_found(&mut browse, h),
            Vec::<String>::new(),
            "and only once"
        );
    }

    /// A requery while a swap is still pending (no drain has seen the new
    /// receiver's first event) is a no-op — a second `browse` would retire the
    /// not-yet-live receiver and drop the live one. Per-handle buffers survive
    /// the swap untouched.
    #[test]
    fn requery_while_a_switch_is_pending_is_a_no_op() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        d.deliver_for_type_for_test(
            TEST_SUBTYPE,
            found_event(TEST_SUBTYPE, "requery-kept-buffer._matter._tcp.local."),
        );
        d.requery(h);
        assert_eq!(d.browse_calls.get(TEST_SUBTYPE), Some(&2), "re-browsed");
        assert!(d.browses[TEST_SUBTYPE].retiring.is_some());
        // `requery` drains first, so if the daemon thread had already run the
        // browse command, its `SearchStarted` would complete the switch and
        // the second requery would rightly browse again. Stand an empty
        // scripted receiver in for the new one, so the switch is still
        // pending however far the daemon has got.
        let _pending = script_current_receiver(&mut d, TEST_SUBTYPE);
        d.requery(h);
        assert_eq!(
            d.browse_calls.get(TEST_SUBTYPE),
            Some(&2),
            "a second requery before the switch-over must not browse again"
        );
        assert!(
            names(&d.poll_found(h), "requery-kept-buffer"),
            "the handle's buffer is untouched by the swap"
        );
        d.stop_query(h);
        assert_eq!(d.stop_browse_calls.get(TEST_SUBTYPE), Some(&1));
        assert_eq!(d.browses.len(), 0);
    }

    /// `requery` on an unknown or stopped handle does nothing (no browse is
    /// opened or re-opened).
    #[test]
    fn requery_on_an_unknown_or_stopped_handle_is_a_no_op() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        d.requery(QueryHandle(999));
        assert_eq!(d.browse_calls.get(TEST_SUBTYPE), None);
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        d.stop_query(h);
        d.requery(h);
        assert_eq!(
            d.browse_calls.get(TEST_SUBTYPE),
            Some(&1),
            "only the original browse"
        );
        assert_eq!(d.browses.len(), 0);
    }

    // ---------------------------------------------------------------------
    // A browse the daemon closed.
    //
    // mdns-sd closes a browse whose event channel fills (1000 events): it
    // removes the querier, and the receiver disconnects once its queue is
    // drained. These tests stand a scripted receiver in for the daemon's and
    // drop its sending end to play that closure.
    // ---------------------------------------------------------------------

    /// Replace `service_type`'s current receiver on `d` with a scripted one;
    /// returns its script. Dropping the script closes the receiver, as the
    /// daemon does when it drops a subscription.
    fn script_current_receiver(d: &mut MdnsSdDiscovery, service_type: &str) -> Script {
        let (script, receiver) = std::sync::mpsc::channel();
        d.browses.get_mut(service_type).unwrap().receiver = EventSource::Scripted(receiver);
        script
    }

    /// The daemon closed the browse: what was queued is still delivered, and
    /// the browse is opened again with the daemon, so the handle keeps
    /// receiving records instead of going quiet for good.
    #[test]
    fn a_browse_the_daemon_closed_is_reopened() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        let script = script_current_receiver(&mut d, TEST_SUBTYPE);
        script
            .send(Some(found_event(
                TEST_SUBTYPE,
                "closed-queued._matter._tcp.local.",
            )))
            .unwrap();
        drop(script);

        assert!(
            names(&d.poll_found(h), "closed-queued"),
            "the events queued before the closure are delivered"
        );
        assert_eq!(
            d.browse_calls.get(TEST_SUBTYPE),
            Some(&2),
            "the closed browse is opened again"
        );
        assert!(
            matches!(d.browses[TEST_SUBTYPE].receiver, EventSource::Daemon(_)),
            "on a receiver from the daemon"
        );
        d.stop_query(h);
    }

    /// The re-opened browse's cache replay rebuilds which instances are
    /// present. The closure dropped events, so an instance this browse saw
    /// before it may have left and come back unseen; its replayed
    /// `ServiceFound` is reported rather than suppressed as a re-delivery.
    #[test]
    fn a_reopened_browse_reports_the_instances_its_replay_finds() {
        let fullname = "closed-return._matter._tcp.local.";
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        let script = script_current_receiver(&mut d, TEST_SUBTYPE);
        script
            .send(Some(found_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert!(names(&d.poll_found(h), "closed-return"));

        drop(script);
        assert_eq!(d.poll_found(h), Vec::<String>::new());
        assert_eq!(d.browse_calls.get(TEST_SUBTYPE), Some(&2), "re-opened");

        d.queue_for_test(TEST_SUBTYPE, found_event(TEST_SUBTYPE, fullname));
        assert!(
            names(&d.poll_found(h), "closed-return"),
            "the replay's ServiceFound is reported"
        );
        d.stop_query(h);
    }

    /// A handle attached after the re-open is not seeded with a record from
    /// before the closure: that instance may have gone while events were
    /// being dropped. What the daemon still caches reaches it in the replay.
    #[test]
    fn a_reopened_browse_does_not_seed_new_handles_with_stale_records() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        let script = script_current_receiver(&mut d, TEST_SUBTYPE);
        script
            .send(Some(resolved_event_under_subtype(
                "closed-stale",
                TEST_SUBTYPE,
            )))
            .unwrap();
        assert!(contains(&d.poll_results(h), "closed-stale"));

        drop(script);
        let _ = d.poll_results(h);
        assert_eq!(d.browse_calls.get(TEST_SUBTYPE), Some(&2), "re-opened");

        let late = d.query_operational_fabric(TEST_CFID).unwrap();
        assert!(
            !contains(&d.poll_results(late), "closed-stale"),
            "a record from before the closure is not replayed to a new handle"
        );
        d.stop_query(late);
        d.stop_query(h);
    }

    /// A requery that runs after the daemon closed the browse, before any poll
    /// saw it, still notices the closure. Retiring the closed receiver would
    /// otherwise pass for an ordinary switch-over: no `warn`, and the
    /// instances a lost `ServiceRemoved` should have cleared stay present, so
    /// the replay's report of their return is suppressed.
    #[test]
    fn a_requery_after_the_daemon_closed_the_browse_reopens_it() {
        let fullname = "closed-requery._matter._tcp.local.";
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        let script = script_current_receiver(&mut d, TEST_SUBTYPE);
        script
            .send(Some(found_event(TEST_SUBTYPE, fullname)))
            .unwrap();
        assert!(names(&d.poll_found(h), "closed-requery"));

        drop(script);
        d.requery(h);
        assert_eq!(
            d.browse_calls.get(TEST_SUBTYPE),
            Some(&2),
            "re-opened once; the requery does not browse again on top"
        );
        assert!(
            d.browses[TEST_SUBTYPE].retiring.is_none(),
            "the closed receiver is not retired as if it were live"
        );

        d.queue_for_test(TEST_SUBTYPE, found_event(TEST_SUBTYPE, fullname));
        assert!(
            names(&d.poll_found(h), "closed-requery"),
            "the replay's ServiceFound is reported"
        );
        d.stop_query(h);
    }

    /// Close `d`'s current receiver for `service_type` the way a daemon whose
    /// replay overflowed the channel does: it delivers `SearchStarted` and is
    /// closed by the time the next drain reads it.
    fn close_after_search_started(d: &mut MdnsSdDiscovery, service_type: &str) {
        let script = script_current_receiver(d, service_type);
        script.send(Some(search_started())).unwrap();
    }

    /// `d`'s browse calls for the test subtype so far.
    fn subtype_browses(d: &MdnsSdDiscovery) -> u32 {
        d.browse_calls.get(TEST_SUBTYPE).copied().unwrap_or(0)
    }

    /// A browse that closes again as soon as it is re-opened (its cache
    /// replay alone overflows the channel) is re-opened with a backoff:
    /// after one skipped drain, then two, then four, rather than on every
    /// poll.
    #[test]
    fn a_browse_that_keeps_closing_is_reopened_with_backoff() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        close_after_search_started(&mut d, TEST_SUBTYPE);
        let mut seen = Vec::new();
        for _ in 0..10 {
            let _ = d.poll_found(h);
            seen.push(subtype_browses(&d));
            if matches!(d.browses[TEST_SUBTYPE].receiver, EventSource::Daemon(_)) {
                // Just re-opened: that browse overflows too.
                close_after_search_started(&mut d, TEST_SUBTYPE);
            }
        }
        assert_eq!(
            seen,
            vec![2, 2, 3, 3, 3, 4, 4, 4, 4, 4],
            "re-opened at once, then after 1, 2 and 4 skipped drains"
        );
        d.stop_query(h);
    }

    /// Once a re-opened browse has delivered events and stayed open for
    /// `REOPEN_HEALTHY_DRAINS` drains in a row, it works, and a later closure
    /// is a new episode: re-opened at once.
    #[test]
    fn a_reopened_browse_that_stays_open_resets_the_backoff() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        close_after_search_started(&mut d, TEST_SUBTYPE);
        let _ = d.poll_found(h);
        assert_eq!(subtype_browses(&d), 2, "re-opened");

        let healthy = script_current_receiver(&mut d, TEST_SUBTYPE);
        healthy.send(Some(search_started())).unwrap();
        for _ in 0..REOPEN_HEALTHY_DRAINS {
            let _ = d.poll_found(h);
        }
        drop(healthy);
        let _ = d.poll_found(h);
        assert_eq!(
            subtype_browses(&d),
            3,
            "a closure after a long healthy run is re-opened at once"
        );
        d.stop_query(h);
    }

    /// A re-opened browse that delivers its replay but closes again a few
    /// drains later (the replay nearly filled the channel, and live answers
    /// filled the rest) has not proved itself: the backoff carries on rather
    /// than restarting, so it is not re-opened at once, with a `warn`, every
    /// couple of drains.
    #[test]
    fn a_reopened_browse_that_closes_soon_after_keeps_the_backoff() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        close_after_search_started(&mut d, TEST_SUBTYPE);
        let _ = d.poll_found(h);
        assert_eq!(subtype_browses(&d), 2, "re-opened");

        let briefly = script_current_receiver(&mut d, TEST_SUBTYPE);
        briefly.send(Some(search_started())).unwrap();
        for _ in 0..3 {
            let _ = d.poll_found(h);
        }
        drop(briefly);
        let _ = d.poll_found(h);
        assert_eq!(
            subtype_browses(&d),
            2,
            "still backing off: one drain skipped"
        );
        let _ = d.poll_found(h);
        assert_eq!(subtype_browses(&d), 3, "then re-opened");
        d.stop_query(h);
    }

    /// A closure ends a healthy run: the next re-open starts counting from
    /// zero, so a long run before the closure, topped up by drains of a
    /// receiver that has delivered nothing yet, does not reset the backoff.
    #[test]
    fn a_closure_ends_the_healthy_run() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        close_after_search_started(&mut d, TEST_SUBTYPE);
        let _ = d.poll_found(h);
        assert_eq!(subtype_browses(&d), 2, "re-opened");

        let run = script_current_receiver(&mut d, TEST_SUBTYPE);
        run.send(Some(search_started())).unwrap();
        for _ in 0..REOPEN_HEALTHY_DRAINS - 6 {
            let _ = d.poll_found(h);
        }
        drop(run);
        let _ = d.poll_found(h);
        assert_eq!(subtype_browses(&d), 2, "one drain skipped");
        let _ = d.poll_found(h);
        assert_eq!(subtype_browses(&d), 3, "re-opened again");

        let silent = script_current_receiver(&mut d, TEST_SUBTYPE);
        for _ in 0..6 {
            let _ = d.poll_found(h);
        }
        drop(silent);
        let _ = d.poll_found(h);
        assert_eq!(
            subtype_browses(&d),
            3,
            "still backing off: the earlier run did not carry over"
        );
        d.stop_query(h);
    }

    /// A drain of a re-opened browse that has delivered nothing yet (the
    /// daemon has not run the browse command) proves nothing, so it does not
    /// reset the backoff.
    #[test]
    fn an_open_drain_that_delivered_nothing_keeps_the_backoff() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        close_after_search_started(&mut d, TEST_SUBTYPE);
        let _ = d.poll_found(h);
        assert_eq!(subtype_browses(&d), 2, "re-opened");

        let silent = script_current_receiver(&mut d, TEST_SUBTYPE);
        let _ = d.poll_found(h);
        drop(silent);
        let _ = d.poll_found(h);
        assert_eq!(
            subtype_browses(&d),
            2,
            "still backing off: one drain skipped"
        );
        let _ = d.poll_found(h);
        assert_eq!(subtype_browses(&d), 3, "then re-opened");
        d.stop_query(h);
    }

    /// The old receiver of a requery disconnecting is the switch-over itself
    /// (the daemon replaced its sender), not a closed browse: nothing is
    /// re-opened.
    #[test]
    fn a_retiring_receiver_closing_is_not_a_closed_browse() {
        let mut d = MdnsSdDiscovery::new().unwrap();
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        let to_old = script_current_receiver(&mut d, TEST_SUBTYPE);
        let (to_new, new_receiver) = std::sync::mpsc::channel();
        d.browses
            .get_mut(TEST_SUBTYPE)
            .unwrap()
            .begin_requery(EventSource::Scripted(new_receiver));
        drop(to_old);
        to_new.send(Some(search_started())).unwrap();

        let _ = d.poll_found(h);
        assert!(d.browses[TEST_SUBTYPE].retiring.is_none(), "switched over");
        assert_eq!(
            d.browse_calls.get(TEST_SUBTYPE),
            Some(&1),
            "only the original browse: the new receiver is live"
        );
        d.stop_query(h);
    }

    /// If the daemon refuses to re-open the browse (it has shut down), the
    /// handle stays usable and polling does not panic; a later poll tries
    /// again.
    #[test]
    fn a_browse_that_cannot_be_reopened_keeps_its_handle_usable() {
        let daemon = ServiceDaemon::new().unwrap();
        let mut d = MdnsSdDiscovery::with_daemon(daemon.clone());
        let h = d.query_operational_fabric(TEST_CFID).unwrap();
        let script = script_current_receiver(&mut d, TEST_SUBTYPE);
        let _ = daemon.shutdown().unwrap().recv();
        drop(script);

        assert_eq!(d.poll_found(h), Vec::<String>::new());
        assert_eq!(d.poll_results(h), Vec::<MatterService>::new());
        assert_eq!(
            d.browse_calls.get(TEST_SUBTYPE),
            Some(&1),
            "the re-open was refused"
        );
        assert!(d.browses.contains_key(TEST_SUBTYPE), "the browse is kept");
        d.stop_query(h);
    }
}
