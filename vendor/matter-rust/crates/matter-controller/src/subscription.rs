//! A live attribute subscription: reports arrive via [`Subscription::next`].

use matter_codec::Value;
use matter_interaction::{AttributePath, EventReport};
use tokio::sync::mpsc;

use crate::actor::Command;
use crate::error::Error;

/// One reported attribute change from a subscription.
///
/// `#[non_exhaustive]`: this is a read-only report produced by the library, so
/// future fields (e.g. a data version) can be added without a breaking change.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AttributeReport {
    /// The concrete attribute path the device reported.
    pub path: AttributePath,
    /// The new value.
    pub value: Value,
}

/// An event from a live [`Subscription`].
///
/// This enum is `#[non_exhaustive]`: matching on it must include a wildcard arm,
/// because future protocol work may add variants without a breaking change.
#[derive(Debug)]
#[non_exhaustive]
pub enum SubscriptionEvent {
    /// A reported attribute value (a priming value or a steady-state change).
    Report(AttributeReport),
    /// A reported event (a priming/historical event or a steady-state emission).
    /// Delivered on the same bounded report channel as [`Self::Report`]; under
    /// backpressure it is dropped and surfaced via [`Self::Lagged`] like any
    /// report. Events have no list-merge semantics, so they are delivered as they
    /// arrive (they bypass the chunked-attribute reassembler).
    Event(EventReport),
    /// The subscription was (re-)established by the device; carries the
    /// device-assigned subscription id and the negotiated max interval. Fired
    /// after each successful `SubscribeResponse`, including after an
    /// auto-resubscribe. Priming [`Self::Report`]s, if any, precede it (they
    /// arrive before the `SubscribeResponse` on the wire).
    ///
    /// Delivered reliably even under report backpressure (see [`Subscription`]).
    ///
    /// The variant is `#[non_exhaustive]`, so a pattern must end in `..`
    /// (`Established { subscription_id, .. }`); later fields can then be added
    /// without a breaking change.
    #[non_exhaustive]
    Established {
        /// The device-assigned subscription id.
        subscription_id: u32,
        /// The max interval, in seconds, the device agreed to in its
        /// `SubscribeResponse`: it will send a report, empty if nothing changed,
        /// at least this often.
        ///
        /// The device chooses it, and it need not equal the ceiling the caller
        /// requested. It may be lower, or higher: the spec lets a publisher go up
        /// to the larger of the ceiling and 60 minutes, and an ICD may go up to
        /// its idle-mode duration (chip `ReadHandler::GetPublisherSelectedIntervalLimit`).
        /// The controller treats the subscription as stale, and resubscribes, if
        /// nothing arrives within this plus a short grace, so this is also the
        /// worst-case delay before a dead device is noticed. It can change
        /// across resubscribes.
        max_interval: u16,
    },
    /// The subscription went stale (liveness timeout or session loss) and is
    /// being transparently re-established; `cause` is why. Reports resume after
    /// the next [`Self::Established`]. Emitted by the auto-resubscribe engine.
    ///
    /// Delivered reliably even under report backpressure (see [`Subscription`]).
    Resubscribing {
        /// Why the subscription is being re-established.
        cause: Error,
    },
    /// One or more [`Self::Report`]s were dropped because the consumer did not
    /// drain [`Subscription::next`] fast enough to keep up with the device's
    /// reporting cadence, and the bounded report buffer filled. `dropped` is the
    /// number of reports discarded since the previous `Lagged` (a coalesced
    /// count, not one event per drop). Subsequent reports continue to arrive;
    /// only the buffer-overflow ones were lost. A re-read or the next
    /// [`Self::Established`] re-prime can be used to recover authoritative state.
    Lagged {
        /// Number of reports dropped since the last `Lagged` event.
        dropped: usize,
    },
}

/// Capacity of the bounded report channel feeding a [`Subscription`].
///
/// Steady-state attribute reports are buffered here. The cap bounds controller
/// memory: a malicious or compromised device controls how many attribute items
/// each `ReportData` carries and how often it sends them (`min_interval` is only
/// a value we *request* — the device need not honour it), so an unbounded buffer
/// would let such a device drive controller memory growth without limit
/// (memory-DoS). When the buffer is full, further reports are dropped and a
/// [`SubscriptionEvent::Lagged`] event signals how many were lost; control
/// events ([`SubscriptionEvent::Established`] / [`SubscriptionEvent::Resubscribing`])
/// are never dropped — they travel on a separate, low-volume channel.
pub(crate) const SUBSCRIPTION_CHANNEL_CAP: usize = 256;

/// A live attribute subscription. Await events with [`Self::next`]; dropping
/// the handle cancels the subscription (best-effort).
///
/// Steady-state [`SubscriptionEvent::Report`]s are buffered in a **bounded**
/// channel (capacity `SUBSCRIPTION_CHANNEL_CAP`, 256) so a device — whose reporting
/// cadence and per-report size are attacker-controlled — cannot drive unbounded
/// controller memory growth. If the consumer does not call [`Self::next`]
/// promptly and the buffer fills, excess reports are dropped and a
/// [`SubscriptionEvent::Lagged`] event reports how many were lost.
///
/// Control events ([`SubscriptionEvent::Established`] and
/// [`SubscriptionEvent::Resubscribing`]) travel on a separate, low-volume channel
/// and are delivered **reliably** even while reports are being dropped; they are
/// also prioritised by [`Self::next`].
pub struct Subscription {
    /// Bounded channel of steady-state reports (and coalesced `Lagged` signals).
    pub(crate) rx: mpsc::Receiver<SubscriptionEvent>,
    /// Reliable, low-volume channel of control events (`Established` /
    /// `Resubscribing`). Kept separate so a saturated report buffer can never
    /// drop a control event.
    pub(crate) ctrl_rx: mpsc::UnboundedReceiver<SubscriptionEvent>,
    pub(crate) tx: mpsc::Sender<Command>,
    pub(crate) key: crate::actor::SubId,
    pub(crate) cancelled: bool,
}

impl Subscription {
    /// Await the next subscription event, or `None` once the subscription has
    /// ended (cancelled, or the controller task stopped).
    ///
    /// Control events ([`SubscriptionEvent::Established`] /
    /// [`SubscriptionEvent::Resubscribing`]) are prioritised over buffered
    /// reports, so a re-establishment is observed promptly even behind a backlog.
    pub async fn next(&mut self) -> Option<SubscriptionEvent> {
        tokio::select! {
            biased;
            // Prefer control events: they are rare, reliable, and ordering them
            // ahead of buffered reports lets the consumer react to a
            // (re-)establishment without first draining a report backlog.
            ctrl = self.ctrl_rx.recv() => {
                match ctrl {
                    Some(ev) => Some(ev),
                    // Control channel closed (actor gone): drain any reports that
                    // are still buffered, then end.
                    None => self.rx.recv().await,
                }
            }
            report = self.rx.recv() => {
                match report {
                    Some(ev) => Some(ev),
                    // Report channel closed: drain any control events still queued
                    // (e.g. a final Resubscribing) before ending.
                    None => self.ctrl_rx.recv().await,
                }
            }
        }
    }

    /// Cancel the subscription explicitly and stop receiving reports.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ControllerStopped`] if the owning task has already
    /// stopped (the subscription is effectively cancelled either way).
    pub async fn cancel(mut self) -> Result<(), Error> {
        self.cancelled = true;
        self.tx
            .send(Command::CancelSubscription { key: self.key })
            .await
            .map_err(|_| Error::ControllerStopped)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if !self.cancelled {
            // Best-effort cancel on drop; ignore a full/closed channel.
            let _ = self
                .tx
                .try_send(Command::CancelSubscription { key: self.key });
        }
    }
}
